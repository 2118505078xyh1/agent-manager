//! 进程日志：只往 `%LOCALAPPDATA%\AgentManager\agentmanager.log` 追加。
//!
//! 代理管理没有常驻服务，日志只用于记录启动期的自动清理与异常。
//! GUI 程序不能 `eprintln!`（会拉起控制台窗口），所以统一走这里。

use std::io::Write;

use crate::flavor;

/// 日志文件路径。
pub fn log_path() -> std::path::PathBuf {
    flavor::local_appdata().join("agentmanager.log")
}

/// 追加一行带时间戳的日志。写失败静默 —— 日志不该影响主流程。
pub fn log_line(msg: &str) {
    let dir = flavor::local_appdata();
    let _ = std::fs::create_dir_all(&dir);
    let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path())
    else {
        return;
    };
    let _ = writeln!(f, "{} {msg}", timestamp());
}

/// `YYYY-MM-DD HH:MM:SS`（本地时间，粗粒度够用）。
fn timestamp() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02}",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

/// Howard Hinnant：1970-01-01 起的天数 → 公历。
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}
