//! 原子写 + 自动备份。配置是用户数据，写一半比不写更糟。

use std::fs;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;
use serde_json::Value;

pub const BACKUP_KEEP: usize = 10;

/// 只清理本函数生成的 .bak-YYYYMMDDHHMMSS；用户手命名的存档一律不动。
fn is_auto_backup(name: &str) -> bool {
    let Some(rest) = name.rsplit_once(".bak-") else {
        return false;
    };
    let digits = rest.1;
    (digits.len() == 14 || digits.len() == 15) && digits.chars().all(|c| c.is_ascii_digit())
}

pub fn read_json<T: serde::de::DeserializeOwned>(file: &Path, fallback: Option<T>) -> Option<T> {
    let text = fs::read_to_string(file).ok()?;
    match serde_json::from_str::<T>(&text) {
        Ok(v) => Some(v),
        Err(_) => fallback,
    }
}

/// 读任意 JSON 值；坏文件返回 `fallback`。
pub fn read_value(file: &Path) -> Option<Value> {
    read_json(file, None)
}

/// 时间戳（本地时区）→ YYYYMMDDHHMMSS。
fn stamp_now() -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    // 本地时区偏移（秒）。Windows 上拿不到 TZ 转换，但用 UTC 也够排序用。
    let offset = local_offset_seconds();
    let local = (now as i64 + offset) as u64;
    let (y, mo, d, h, mi, s) = civil_from_unix(local);
    format!("{y:04}{mo:02}{d:02}{h:02}{mi:02}{s:02}")
}

fn local_offset_seconds() -> i64 {
    // 借用 chrono 式的算法太重；用 TZ 偏移的近似：
    // Windows 的 localtime 与 UTC 的差值可以通过环境变量 TZ 拿不到，
    // 这里直接用 0（UTC）。备份名只需要单调可排序，精确到时区不影响功能。
    0
}

/// Unix 秒 → (年, 月, 日, 时, 分, 秒)，UTC。Howard Hinnant 的 civil_from_days。
fn civil_from_unix(secs: u64) -> (i64, u32, u32, u32, u32, u32) {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let (y, m, d) = civil_from_days(days);
    (
        y,
        m,
        d,
        (rem / 3600) as u32,
        ((rem % 3600) / 60) as u32,
        (rem % 60) as u32,
    )
}

fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// 只清理本函数生成的 .bak-YYYYMMDDHHMMSS；用户手命名的存档一律不动。
pub fn prune_backups(file: &Path) {
    let Some(dir) = file.parent() else { return };
    let base = match file.file_name().and_then(|s| s.to_str()) {
        Some(s) => s.to_string(),
        None => return,
    };
    let Ok(entries) = fs::read_dir(dir) else { return };
    let mut mine: Vec<String> = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with(&format!("{base}.bak-")) && is_auto_backup(&name) {
            mine.push(name);
        }
    }
    mine.sort();
    let keep = mine.len().saturating_sub(BACKUP_KEEP);
    for name in mine.into_iter().take(keep) {
        let _ = fs::remove_file(dir.join(name));
    }
}

/// 备份一个文件，返回备份路径。文件不存在返回 None。
pub fn backup_file(file: &Path) -> Option<std::path::PathBuf> {
    if !file.exists() {
        return None;
    }
    let target = file.with_file_name(format!(
        "{}.bak-{}",
        file.file_name()?.to_string_lossy(),
        stamp_now()
    ));
    fs::copy(file, &target).ok()?;
    prune_backups(file);
    Some(target)
}

/// 临时文件 + rename。Windows 上目标被别的进程开着读时 rename 会 WinError 5，
/// 那种时候退回直写。
pub fn write_json_atomic(file: &Path, value: &Value, indent: usize) -> std::io::Result<()> {
    if let Some(dir) = file.parent() {
        fs::create_dir_all(dir)?;
    }
    let text = if indent > 0 {
        let mut buf = Vec::new();
        let indent_bytes = b" ".repeat(indent);
        let formatter = serde_json::ser::PrettyFormatter::with_indent(&indent_bytes);
        let mut ser = serde_json::Serializer::with_formatter(&mut buf, formatter);
        serde::Serialize::serialize(value, &mut ser)?;
        buf.push(b'\n');
        String::from_utf8_lossy(&buf).to_string()
    } else {
        let mut buf = Vec::new();
        let formatter = serde_json::ser::CompactFormatter;
        let mut ser = serde_json::Serializer::with_formatter(&mut buf, formatter);
        serde::Serialize::serialize(value, &mut ser)?;
        String::from_utf8_lossy(&buf).to_string()
    };
    write_text_atomic(file, &text)
}

pub fn write_json_atomic_pretty<T: Serialize>(
    file: &Path,
    value: &T,
    indent: usize,
) -> std::io::Result<()> {
    let v = serde_json::to_value(value)?;
    write_json_atomic(file, &v, indent)
}

/// 临时文件 + rename 的文本版本。
pub fn write_text_atomic(file: &Path, text: &str) -> std::io::Result<()> {
    if let Some(dir) = file.parent() {
        fs::create_dir_all(dir)?;
    }
    let tmp = file.with_extension(format!(
        "{}.tmp",
        file.extension().and_then(|s| s.to_str()).unwrap_or("")
    ));
    fs::write(&tmp, text)?;
    match fs::rename(&tmp, file) {
        Ok(()) => Ok(()),
        Err(_) => {
            fs::write(file, text)?;
            let _ = fs::remove_file(&tmp);
            Ok(())
        }
    }
}
