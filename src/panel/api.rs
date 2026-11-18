//! 面板用到的后台任务。
//!
//! 代理管理没有 daemon：这些任务由面板进程直接调用库函数（`run_local`），
//! 返回与旧 HTTP 版本相同的 JSON 形状，`handle_loaded` 不用区分来源。

use serde_json::{json, Value};

#[derive(Clone, Debug)]
pub enum Job {
    Agents,
    AgentsUpdate { id: String },
    SetAgent {
        id: String,
        model: String,
        effort: Option<String>,
    },
    Conversations,
    Worldwide(bool),
    ConversationOpen {
        agent: String,
        path: String,
        id: String,
    },
    ConversationDelete {
        agent: String,
        path: String,
        id: String,
    },
}

impl Job {
    /// 进程内直接调用底层函数。
    pub async fn run_local(self) -> Result<Value, String> {
        match self {
            Job::Agents => {
                let list = crate::agents::detect_all_extended().await;
                Ok(json!({ "agents": list }))
            }
            Job::AgentsUpdate { id } => Ok(crate::agents::update_agent(&id).await),
            Job::SetAgent { id, model, effort } => {
                match crate::agents::set_model(&id, &model, effort.as_deref()) {
                    Ok(bak) => Ok(json!({ "ok": true, "id": id, "backup": bak })),
                    Err(e) => Ok(json!({ "ok": false, "id": id, "error": e })),
                }
            }
            Job::Conversations => {
                // 服务器侧不做筛选：面板自己按 conv_agent / conv_q 在本地过滤。
                let v = tokio::task::spawn_blocking(|| crate::conversations::list(None, "", 500))
                    .await
                    .map_err(|e| format!("读取会话失败：{e}"))?;
                // Argent 短会话在扫描时已排队：异步移入回收站，不阻塞这次返回。
                let pending = crate::conversations::take_pending_deletes();
                if !pending.is_empty() {
                    tokio::spawn(async move {
                        for file in pending {
                            let _ = crate::transcripts::remove_path(&file, true).await;
                        }
                    });
                }
                Ok(v)
            }
            Job::ConversationOpen { agent, path, id } => {
                crate::conversations::open(&agent, &path, &id).await
            }
            Job::ConversationDelete { agent, path, id } => {
                crate::conversations::delete(&agent, &path, &id, true).await
            }
            Job::Worldwide(refresh) => {
                if refresh {
                    crate::worldwide::clear_cache();
                }
                Ok(crate::worldwide::worldwide_report().await)
            }
        }
    }
}

/// 从错误信封里取可读文案；没有就给出 HTTP 状态。
pub fn error_text(v: &Value, status: u16) -> String {
    let msg = v
        .get("error")
        .and_then(|e| e.get("message").or_else(|| Some(e)))
        .and_then(Value::as_str)
        .or_else(|| v.get("message").and_then(Value::as_str))
        .unwrap_or("")
        .trim();
    if msg.is_empty() {
        format!("请求失败（HTTP {status}）")
    } else {
        msg.to_string()
    }
}
