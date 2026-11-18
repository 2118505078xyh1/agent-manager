//! 窗口登记表 —— pi 与 opencode 共用。
//!
//! 为什么不用 pid 记账：Windows Terminal 是**一个进程托管所有窗口/标签页**。
//! `wt.exe` 只是启动器，真正的进程是 WindowsTerminal.exe；按 pid 杀会把用户
//! 自己开的标签页一起带走。所以记账的键是 **mark**。
//!
//! 这份表只用于界面展示与「关闭全部」，不参与进程管理 —— 表丢了也只是少个
//! 列表项，不会造成孤儿进程。

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::launch::{close_by_mark, live_marks};

/// 超过这个时间没被动过就当陈货清掉（不影响进程，只影响列表）。
const ENTRY_TTL_MS: u64 = 12 * 60 * 60 * 1000;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WindowRecord {
    pub mark: String,
    pub kind: String,
    pub title: Option<String>,
    pub session_id: Option<String>,
    pub cwd: Option<String>,
    pub hwnd: Option<i64>,
    pub focused: bool,
    pub at: u64,
}

static LIVE: OnceLock<Mutex<HashMap<String, WindowRecord>>> = OnceLock::new();

fn live() -> &'static Mutex<HashMap<String, WindowRecord>> {
    LIVE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// 记一个刚打开的窗口。
pub fn remember_window(entry: WindowRecord) -> Option<WindowRecord> {
    if entry.mark.is_empty() {
        return None;
    }
    let mut record = entry;
    record.at = now_ms();
    let mut guard = live().lock().unwrap();
    guard.insert(record.mark.clone(), record.clone());
    Some(record)
}

fn prune(guard: &mut HashMap<String, WindowRecord>) {
    let now = now_ms();
    guard.retain(|_, rec| now.saturating_sub(rec.at) <= ENTRY_TTL_MS);
}

/// 列出由本插件打开、仍在表里的窗口。
///
/// 只返回**真的还有进程**的那些：用户可以自己关掉终端标签页，插件无从得知。
/// 核查失败时保守地全部保留 —— 宁可多列一项，也不能把正在用的窗口报成已关闭。
pub async fn list_windows(kind: Option<&str>) -> Vec<WindowRecord> {
    let candidates: Vec<WindowRecord> = {
        let mut guard = live().lock().unwrap();
        prune(&mut guard);
        guard
            .values()
            .filter(|w| kind.map_or(true, |k| w.kind == k))
            .cloned()
            .collect()
    };
    if candidates.is_empty() {
        return Vec::new();
    }

    let marks: Vec<String> = candidates.iter().map(|w| w.mark.clone()).collect();
    let alive = match live_marks(&marks).await {
        Some(list) => list,
        // 查不了就当全部活着：宁可多列一项，也不能把正在用的窗口报成已关闭。
        None => marks.clone(),
    };

    let mut guard = live().lock().unwrap();
    for w in &candidates {
        if !alive.contains(&w.mark) {
            guard.remove(&w.mark);
        }
    }
    candidates
        .into_iter()
        .filter(|w| alive.contains(&w.mark))
        .collect()
}

/// 关闭一个（或全部）由本插件打开的窗口。
pub async fn close_windows(mark: Option<&str>, all: bool, kind: Option<&str>) -> serde_json::Value {
    let targets: Vec<String> = if all {
        list_windows(kind).await.into_iter().map(|w| w.mark).collect()
    } else {
        match mark {
            Some(m) if !m.is_empty() => vec![m.to_string()],
            _ => Vec::new(),
        }
    };

    let mut closed = Vec::new();
    let mut failed = Vec::new();
    for m in targets {
        let (ok, _) = close_by_mark(&m).await;
        if ok {
            closed.push(m.clone());
            live().lock().unwrap().remove(&m);
        } else {
            failed.push(m);
        }
    }
    serde_json::json!({
        "ok": failed.is_empty(),
        "closed": closed,
        "failed": failed,
    })
}
