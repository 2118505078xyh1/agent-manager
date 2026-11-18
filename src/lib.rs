//! 代理管理核心库。
//!
//! 面板（`agent-manager-gui`）直接调用这里的函数：Agent 版本检测/更新、会话总览、
//! 全球热门。**对话只在终端进行**：本程序只翻历史会话、开终端窗口，不做 SSE 聊天。

pub mod agents;
pub mod app;
pub mod atomic;
pub mod conversations;
pub mod dirs;
pub mod flavor;
pub mod launch;
pub mod models;
pub mod ofm;
pub mod panel;
pub mod paths;
pub mod pi;
pub mod proc;
pub mod settings;
pub mod sqlite;
pub mod transcripts;
pub mod windows;
pub mod worldwide;
mod worldwide_opencode;

/// 版本号（写进 exe 的元信息与 `/api/info`）。
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// 全局汇总：一眼看清各子系统状态。
pub async fn overview() -> serde_json::Value {
    serde_json::json!({
        "ok": true,
        "version": VERSION,
        "pi": pi::get_status().await,
        "ofm": ofm::status().await,
    })
}
