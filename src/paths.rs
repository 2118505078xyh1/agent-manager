//! 全项目唯一的路径定义。**每次现算**，不在导入期冻结 ——
//! 环境变量可以把整份状态指到临时目录，导入期常量会让「写一个文件、读另一个」重现。
//!
//! 环境变量沿用两个老项目的名字，用户已有的脚本不用改：
//!   PI_HOME / PI_AGENT_DIR / PI_STATE_DIR / PI_CODING_AGENT_SESSION_DIR / OFM_*

use std::path::{Path, PathBuf};

/// 用户主目录。**每次现算** —— 环境变量可能变。
pub fn home() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("C:\\"))
}

pub fn pi_home() -> PathBuf {
    match std::env::var("PI_HOME") {
        Ok(v) if !v.trim().is_empty() => PathBuf::from(v),
        _ => home().join(".pi"),
    }
}

pub fn agent_dir() -> PathBuf {
    match std::env::var("PI_AGENT_DIR") {
        Ok(v) if !v.trim().is_empty() => PathBuf::from(v),
        _ => pi_home().join("agent"),
    }
}

pub fn state_dir() -> PathBuf {
    match std::env::var("PI_STATE_DIR") {
        Ok(v) if !v.trim().is_empty() => PathBuf::from(v),
        _ => agent_dir().join("state"),
    }
}

pub fn models_json() -> PathBuf {
    agent_dir().join("models.json")
}

pub fn settings_json() -> PathBuf {
    agent_dir().join("settings.json")
}

pub fn sessions_root() -> PathBuf {
    match std::env::var("PI_CODING_AGENT_SESSION_DIR") {
        Ok(v) if !v.trim().is_empty() => PathBuf::from(v),
        _ => agent_dir().join("sessions"),
    }
}

pub fn sidecar_path() -> PathBuf {
    state_dir().join("dsh-agent-manager-models.json")
}

pub fn order_file() -> PathBuf {
    state_dir().join("session-order.json")
}

pub fn opened_file() -> PathBuf {
    state_dir().join("opened-workspaces.json")
}

pub fn last_workspace_file() -> PathBuf {
    state_dir().join("last-workspace.txt")
}

pub fn trash_dir() -> PathBuf {
    state_dir().join("webui-trash")
}

/// Kimi CLI / Kimi Code 状态根目录。
pub fn kimi_home() -> PathBuf {
    match std::env::var("KIMI_HOME") {
        Ok(v) if !v.trim().is_empty() => PathBuf::from(v),
        _ => home().join(".kimi-code"),
    }
}

/// Kimi Desktop 内嵌 runtime 的会话索引（`session_index.jsonl`）。
pub fn kimi_desktop_session_index() -> PathBuf {
    match std::env::var("APPDATA") {
        Ok(v) if !v.trim().is_empty() => PathBuf::from(v)
            .join("kimi-desktop")
            .join("daimon-share")
            .join("daimon")
            .join("runtime")
            .join("kimi-code")
            .join("home")
            .join("session_index.jsonl"),
        _ => PathBuf::new(),
    }
}

/// ZCode 桌面端的会话数据目录（minzcode 变体）。
pub fn minzcode_sessions_dir() -> PathBuf {
    home().join(".minzcode").join("sessions")
}

/// Argent（Rust coding agent）状态根目录。
pub fn argent_home() -> PathBuf {
    match std::env::var("ARGENT_HOME") {
        Ok(v) if !v.trim().is_empty() => PathBuf::from(v),
        _ => home().join(".argent"),
    }
}

/// omp（oh-my-pi）状态根目录。会话在 `<omp_home>\agent\sessions`。
pub fn omp_home() -> PathBuf {
    match std::env::var("OMP_HOME") {
        Ok(v) if !v.trim().is_empty() => PathBuf::from(v),
        _ => home().join(".omp"),
    }
}

/// DSH 自己的状态根目录。DSH 的 CLI 用 `$DSH_HOME`，这里跟着走。
pub fn dsh_home() -> PathBuf {
    match std::env::var("DSH_HOME") {
        Ok(v) if !v.trim().is_empty() => PathBuf::from(v),
        _ => home().join(".dsh"),
    }
}

/// DSH 宿主工作区文件（与 Pi 转录里的「最近项目」是两件事）。
pub fn dsh_workspace_path() -> PathBuf {
    dsh_home().join("workspace.json")
}

/// DSH 的会话索引缓存：一个会话一份 `<id>.json`（`record.rows` 里存标题、
/// 模型、轮数等摘要）。真正的对话事件不在这里，所以**只能读、不能删**。
pub fn dsh_session_cache_dir() -> PathBuf {
    dsh_home()
        .join("storages")
        .join("session_projcache")
        .join("sessions")
}

/// 路径比较的统一形式。Windows 大小写不敏感、分隔符两种写法。
pub fn norm(p: &Path) -> String {
    if p.as_os_str().is_empty() {
        return String::new();
    }
    let abs = dunce_resolve(p);
    let s = abs.to_string_lossy().replace('/', "\\").to_lowercase();
    s
}

/// `std::fs::canonicalize` 在 Windows 上会把 `\\?\` 前缀与短名展开，
/// 这里只要一个稳定可比较的绝对路径，不需要真的解析符号链接。
pub fn dunce_resolve(p: &Path) -> PathBuf {
    if p.is_absolute() {
        normalize_win(p)
    } else {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("C:\\"));
        normalize_win(&cwd.join(p))
    }
}

/// 去掉 `.` / `..` 与重复分隔符，但**不**碰盘符与 UNC 前缀。
fn normalize_win(p: &Path) -> PathBuf {
    let raw = p.to_string_lossy().replace('/', "\\");
    let mut prefix = String::new();
    let mut rest = raw.as_str();
    if let Some(stripped) = rest.strip_prefix("\\\\") {
        // UNC: \\server\share\...
        prefix = "\\\\".to_string();
        rest = stripped;
    } else if rest.len() >= 2 && rest.as_bytes()[1] == b':' {
        prefix = rest[..2].to_string();
        rest = &rest[2..];
    }
    let mut parts: Vec<&str> = Vec::new();
    for seg in rest.split('\\') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            s => parts.push(s),
        }
    }
    let mut out = prefix;
    for seg in parts {
        out.push('\\');
        out.push_str(seg);
    }
    if out.is_empty() {
        out.push('\\');
    }
    PathBuf::from(out)
}

/// cwd → Pi 的 --xxx-- 目录名。只用于初筛，精确 cwd 一律以转录首行为准。
pub fn slug_for(cwd: &Path) -> String {
    let text = dunce_resolve(cwd)
        .to_string_lossy()
        .trim_start_matches(['\\', '/'])
        .to_string();
    let text = text.replace(['/', '\\', ':'], "-");
    format!("--{text}--")
}

/// opencode 的配置根目录。允许 OPENCODE_HOME 覆盖（测试隔离用）。
pub fn opencode_home() -> PathBuf {
    match std::env::var("OPENCODE_HOME") {
        Ok(v) if !v.trim().is_empty() => PathBuf::from(v),
        _ => home(),
    }
}

pub fn opencode_config_dir() -> PathBuf {
    opencode_home().join(".config").join("opencode")
}

pub fn opencode_config_path() -> PathBuf {
    opencode_config_dir().join("opencode.json")
}

pub fn opencode_data_dir() -> PathBuf {
    opencode_home().join(".local").join("share").join("opencode")
}

pub fn opencode_db_path() -> PathBuf {
    opencode_data_dir().join("opencode.db")
}

/// 临时目录（启动脚本、导出等）。
pub fn temp_dir() -> PathBuf {
    std::env::temp_dir()
}
