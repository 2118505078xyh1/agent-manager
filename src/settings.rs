//! 本程序的设置（`%LOCALAPPDATA%\<身份目录>\settings.json`）。
//!
//! 目前只有一个键：`port`。0 = 每次随机（默认，最省事、不会撞端口），
//! 设成固定值之后 agent 那边写死的 baseURL 才不会每次重启都失效。
//! **改完要重启程序才生效**（窗口的 origin 跟着服务端口走）。

use serde_json::{json, Value};

fn path() -> std::path::PathBuf {
    crate::flavor::local_appdata().join("settings.json")
}

fn read() -> Value {
    std::fs::read_to_string(path())
        .ok()
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .filter(|v| v.is_object())
        .unwrap_or_else(|| json!({}))
}

fn write(v: &Value) -> Result<(), String> {
    let p = path();
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("建目录失败：{e}"))?;
    }
    let text = serde_json::to_string_pretty(v).map_err(|e| format!("序列化失败：{e}"))?;
    std::fs::write(&p, text).map_err(|e| format!("写设置失败：{e}"))
}

/// 端口设置。0 = 随机。
pub fn port() -> u16 {
    read()
        .get("port")
        .and_then(|v| v.as_u64())
        .filter(|p| *p > 1024 && *p < 65535)
        .map(|p| p as u16)
        .unwrap_or(0)
}

/// 引擎线程数。默认 **1**：个人网关一次一个对话足够，多一个引擎就多一份
/// QuickJS 运行时（几十 MB + 一个线程）。要并发流式再调到 2~4。
pub fn workers() -> usize {
    read()
        .get("workers")
        .and_then(|v| v.as_u64())
        .map(|n| (n as usize).clamp(1, 6))
        .unwrap_or(1)
}

/// 读全部设置（界面用）。
pub fn all() -> Value {
    let mut v = read();
    let obj = v.as_object_mut().expect("设置应当是对象");
    obj.entry("port").or_insert(json!(0));
    obj.entry("workers").or_insert(json!(workers()));
    obj.entry("path").or_insert(json!(path().to_string_lossy()));
    v
}

/// 写设置。只认 `port`（0 或 1025..65535）和 `workers`（1..6）。
pub fn set(patch: &Value) -> Value {
    let mut v = read();
    if let Some(p) = patch.get("port").and_then(|x| x.as_u64()) {
        if p == 0 || (1024..65536).contains(&p) {
            v["port"] = json!(p);
            let _ = write(&v);
        }
    }
    if let Some(n) = patch.get("workers").and_then(|x| x.as_u64()) {
        if (1..=6).contains(&n) {
            v["workers"] = json!(n);
            let _ = write(&v);
        }
    }
    all()
}
