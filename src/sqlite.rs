//! 极薄的只读 SQLite 封装 —— 直接调 Windows 自带的 `winsqlite3.dll`。
//!
//! 为什么不用 `rusqlite`：
//!   * `bundled` 要把整个 SQLite C 源码编进来（每次构建多几秒、exe 多 1 MB）；
//!   * `libsqlite3-sys` 0.30 没有 `winsqlite3` feature，系统库路线反而要自己写绑定。
//!
//! 这里只暴露「打开 → 跑一条查询 → 按列取」三件事，够读 OpenCode 的会话库。
//! 链接走 `raw-dylib`：编译期生成导入表，不依赖 winsqlite3.lib 是否存在。
//!
//! ⚠️ 只读打开。OpenCode 自己正在写这个库；我们要保证绝不改动它。

use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::marker::PhantomData;
use std::path::Path;
use std::ptr;

#[repr(C)]
pub struct sqlite3 {
    _private: [u8; 0],
}

#[repr(C)]
pub struct sqlite3_stmt {
    _private: [u8; 0],
}

const SQLITE_OK: c_int = 0;
const SQLITE_ROW: c_int = 100;
const SQLITE_DONE: c_int = 101;

const SQLITE_OPEN_READONLY: c_int = 0x0000_0001;
const SQLITE_OPEN_NOMUTEX: c_int = 0x0000_8000;

#[link(name = "winsqlite3", kind = "raw-dylib")]
extern "C" {
    fn sqlite3_open_v2(
        filename: *const c_char,
        pp_db: *mut *mut sqlite3,
        flags: c_int,
        z_vfs: *const c_char,
    ) -> c_int;
    fn sqlite3_close_v2(db: *mut sqlite3) -> c_int;
    fn sqlite3_errmsg(db: *mut sqlite3) -> *const c_char;
    fn sqlite3_busy_timeout(db: *mut sqlite3, ms: c_int) -> c_int;
    fn sqlite3_prepare_v2(
        db: *mut sqlite3,
        sql: *const c_char,
        n_byte: c_int,
        pp_stmt: *mut *mut sqlite3_stmt,
        pz_tail: *mut *const c_char,
    ) -> c_int;
    fn sqlite3_step(stmt: *mut sqlite3_stmt) -> c_int;
    fn sqlite3_finalize(stmt: *mut sqlite3_stmt) -> c_int;
    fn sqlite3_column_text(stmt: *mut sqlite3_stmt, i: c_int) -> *const u8;
    fn sqlite3_column_bytes(stmt: *mut sqlite3_stmt, i: c_int) -> c_int;
    fn sqlite3_column_int64(stmt: *mut sqlite3_stmt, i: c_int) -> i64;
    fn sqlite3_bind_text(
        stmt: *mut sqlite3_stmt,
        i: c_int,
        value: *const c_char,
        n: c_int,
        destructor: *const c_void,
    ) -> c_int;
}

/// 打开的只读库。`Drop` 里关闭。
pub struct Db {
    raw: *mut sqlite3,
}

unsafe impl Send for Db {}

impl Db {
    pub fn open_readonly(path: &Path) -> Result<Db, String> {
        let text = path.to_string_lossy().to_string();
        let c = CString::new(text).map_err(|_| "数据库路径里有 NUL")?;
        let mut raw: *mut sqlite3 = ptr::null_mut();
        let rc = unsafe {
            sqlite3_open_v2(
                c.as_ptr(),
                &mut raw,
                SQLITE_OPEN_READONLY | SQLITE_OPEN_NOMUTEX,
                ptr::null(),
            )
        };
        if rc != SQLITE_OK {
            let msg = if raw.is_null() {
                "打开 SQLite 失败".to_string()
            } else {
                let m = unsafe { errmsg(raw) };
                unsafe { sqlite3_close_v2(raw) };
                m
            };
            return Err(msg);
        }
        unsafe {
            // OpenCode 可能正在写；等一小会儿再报 BUSY，别把面板卡住。
            sqlite3_busy_timeout(raw, 2000);
        }
        Ok(Db { raw })
    }

    /// 跑一条查询，逐行回调。SQL 里可用 `?1`。
    pub fn query(
        &self,
        sql: &str,
        binds: &[&str],
        mut row: impl FnMut(&Row<'_>) -> Result<(), String>,
    ) -> Result<(), String> {
        let csql = CString::new(sql).map_err(|_| "SQL 里有 NUL")?;
        let mut stmt: *mut sqlite3_stmt = ptr::null_mut();
        let rc = unsafe {
            sqlite3_prepare_v2(
                self.raw,
                csql.as_ptr(),
                -1,
                &mut stmt,
                ptr::null_mut(),
            )
        };
        if rc != SQLITE_OK || stmt.is_null() {
            return Err(unsafe { errmsg(self.raw) });
        }

        // bind 的字符串必须活到 finalize；`cstring_binds` 在这里兜住。
        let mut cstring_binds: Vec<CString> = Vec::with_capacity(binds.len());
        for (i, text) in binds.iter().enumerate() {
            let c = CString::new(*text).map_err(|_| "参数里有 NUL")?;
            let brc = unsafe {
                sqlite3_bind_text(
                    stmt,
                    (i + 1) as c_int,
                    c.as_ptr(),
                    -1,
                    ptr::null(),
                )
            };
            if brc != SQLITE_OK {
                let msg = unsafe { errmsg(self.raw) };
                unsafe { sqlite3_finalize(stmt) };
                return Err(msg);
            }
            cstring_binds.push(c);
        }

        let mut result = Ok(());
        loop {
            let step = unsafe { sqlite3_step(stmt) };
            if step == SQLITE_ROW {
                let r = Row {
                    stmt,
                    _p: PhantomData,
                };
                if let Err(e) = row(&r) {
                    result = Err(e);
                    break;
                }
            } else if step == SQLITE_DONE {
                break;
            } else {
                result = Err(unsafe { errmsg(self.raw) });
                break;
            }
        }

        unsafe { sqlite3_finalize(stmt) };
        drop(cstring_binds);
        result
    }
}

impl Drop for Db {
    fn drop(&mut self) {
        if !self.raw.is_null() {
            unsafe { sqlite3_close_v2(self.raw) };
            self.raw = ptr::null_mut();
        }
    }
}

/// 一行结果。列号从 0 开始。
pub struct Row<'a> {
    stmt: *mut sqlite3_stmt,
    _p: PhantomData<&'a ()>,
}

impl Row<'_> {
    pub fn text(&self, i: usize) -> String {
        unsafe {
            let p = sqlite3_column_text(self.stmt, i as c_int);
            if p.is_null() {
                return String::new();
            }
            let n = sqlite3_column_bytes(self.stmt, i as c_int);
            let bytes = std::slice::from_raw_parts(p, n.max(0) as usize);
            String::from_utf8_lossy(bytes).to_string()
        }
    }

    pub fn int(&self, i: usize) -> i64 {
        unsafe { sqlite3_column_int64(self.stmt, i as c_int) }
    }
}

unsafe fn errmsg(db: *mut sqlite3) -> String {
    if db.is_null() {
        return "SQLite 错误（无句柄）".to_string();
    }
    let p = unsafe { sqlite3_errmsg(db) };
    if p.is_null() {
        return "SQLite 错误".to_string();
    }
    unsafe { CStr::from_ptr(p) }
        .to_string_lossy()
        .to_string()
}