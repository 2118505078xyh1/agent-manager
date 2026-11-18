//! 代理管理的身份常量：数据目录与显示名。
//!
//! 数据目录固定为 `%LOCALAPPDATA%\AgentManager\`。

use std::path::PathBuf;

/// `%LOCALAPPDATA%` 下的数据目录名。
pub const DATA_DIR: &str = "AgentManager";

/// 应用显示名。
pub const DISPLAY_NAME: &str = "代理管理";

/// `%LOCALAPPDATA%\AgentManager`。
pub fn local_appdata() -> PathBuf {
    crate::paths::home()
        .join("AppData")
        .join("Local")
        .join(DATA_DIR)
}
