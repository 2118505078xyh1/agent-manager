//! 会话转录 —— 全项目唯一的一份实现。
//!
//! 移植自 dsh-agent-manager 的 `lib/features/sessions/transcripts.js`。
//!
//! 转录是 Pi 自己写的 JSONL，只追加：
//!   `~/.pi/agent/sessions/--<path>--/<ts>_<id>.jsonl`
//! 目录名只用来**初筛**（slug 会把 `D:\a-b` 和 `D:\a\b` 算成同一个），
//! 精确 cwd 一律以首行 `{"type":"session",cwd:...}` 为准。

use std::collections::{HashMap, HashSet};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use serde_json::{json, Value};
use sha2::{Digest, Sha512};

use crate::atomic::read_json;
use crate::paths;

const NO_MESSAGES: &str = "(没有消息)";
const CACHE_MAX: usize = 2048;
const BLOCK: usize = 256 * 1024;
const FP_BYTES: u64 = 4096;
const TAIL_BYTES: u64 = 256 * 1024;

/// 一份转录的元信息。
#[derive(Clone, Default)]
pub struct Meta {
    pub name: String,
    pub first_message: String,
    pub message_count: u32,
    pub activity: f64,
    /// 最近一次切模型后的 `modelId`。总览页要展示，不用再单独扫一遍。
    pub model: String,
}

/// 缓存项：文件没变就零 I/O。
#[derive(Clone)]
struct CacheEntry {
    size: u64,
    mtime_ns: u64,
    meta: Meta,
    offset: u64,
    fp: [u8; 16],
    header: Option<Value>,
}

use std::sync::Mutex;
use std::sync::OnceLock;

fn cache() -> &'static Mutex<HashMap<String, CacheEntry>> {
    static CACHE: OnceLock<Mutex<HashMap<String, CacheEntry>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn remember(key: String, entry: CacheEntry) -> Meta {
    let meta = entry.meta.clone();
    let mut map = cache().lock().unwrap();
    if map.len() >= CACHE_MAX {
        map.clear();
    }
    map.insert(key, entry);
    meta
}

/// 忘掉一份转录的缓存（改过名/删过之后要调）。
pub fn forget(file: &str) {
    let key = paths::norm(Path::new(file));
    cache().lock().unwrap().remove(&key);
}

/// 清空缓存。
pub fn clear_cache() {
    cache().lock().unwrap().clear();
}

/// ISO 时间串 → Unix 秒。
fn iso(t: &str) -> f64 {
    if t.is_empty() {
        return 0.0;
    }
    // 不引 chrono：只认 `YYYY-MM-DDTHH:MM:SS(.sss)?(Z|±HH:MM)` 这一种形状。
    let bytes = t.as_bytes();
    if bytes.len() < 19 {
        return 0.0;
    }
    let num = |s: &str| -> Option<i64> { s.parse::<i64>().ok() };
    let (Some(y), Some(mo), Some(d)) = (
        num(&t[0..4]),
        num(&t[5..7]),
        num(&t[8..10]),
    ) else {
        return 0.0;
    };
    let (Some(h), Some(mi), Some(se)) = (num(&t[11..13]), num(&t[14..16]), num(&t[17..19])) else {
        return 0.0;
    };
    // 天数 → Unix 秒（Howard Hinnant 的 days_from_civil）
    let days = days_from_civil(y, mo as u32, d as u32);
    let mut secs = days * 86_400 + h * 3600 + mi * 60 + se;
    // 时区偏移
    let rest = &t[19..];
    if let Some(stripped) = rest.strip_prefix('Z') {
        let _ = stripped;
    } else if rest.len() >= 6 {
        let sign = if rest.starts_with('-') { -1 } else { 1 };
        if let (Some(oh), Some(om)) = (
            rest.get(1..3).and_then(|s| s.parse::<i64>().ok()),
            rest.get(4..6).and_then(|s| s.parse::<i64>().ok()),
        ) {
            secs -= sign * (oh * 3600 + om * 60);
        }
    }
    secs as f64
}

/// 民用日期 → 天数（自 1970-01-01）。
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400; // [0, 399]
    let mp = if m > 2 { m - 3 } else { m + 9 } as i64; // [0, 11]
    let doy = (153 * mp + 2) / 5 + d as i64 - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    era * 146_097 + doe - 719_468
}

/// 从一条消息记录里抽纯文本。
fn text_of(message: &Value) -> String {
    let Some(content) = message.get("content") else {
        return String::new();
    };
    let mut s = String::new();
    if let Some(text) = content.as_str() {
        s = text.to_string();
    } else if let Some(blocks) = content.as_array() {
        for b in blocks {
            if b.get("type").and_then(|v| v.as_str()) == Some("text") {
                if let Some(t) = b.get("text").and_then(|v| v.as_str()) {
                    s.push_str(t);
                    s.push(' ');
                }
            }
        }
    }
    // 折叠所有空白
    let mut out = String::with_capacity(s.len());
    let mut prev_space = false;
    for ch in s.chars() {
        if ch.is_whitespace() {
            if !prev_space {
                out.push(' ');
                prev_space = true;
            }
        } else {
            out.push(ch);
            prev_space = false;
        }
    }
    out.trim().to_string()
}

/// 把一条记录累加进统计。
fn absorb(tally: &mut Meta, rec: &Value) {
    let kind = rec.get("type").and_then(|v| v.as_str()).unwrap_or("");
    if kind == "session_info" {
        // 最后一条说了算（空串 = 清掉名字）。
        let n = rec.get("name").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
        tally.name = n;
        return;
    }
    if kind == "model_change" {
        if let Some(m) = rec
            .get("modelId")
            .or_else(|| rec.get("model"))
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
        {
            tally.model = m.to_string();
        }
        return;
    }
    if kind != "message" {
        return;
    }
    tally.message_count += 1;
    let m = rec.get("message").cloned().unwrap_or_else(|| json!({}));
    if let Some(model) = m.get("model").and_then(|v| v.as_str()).filter(|s| !s.is_empty()) {
        tally.model = model.to_string();
    }
    let at = match m.get("timestamp").and_then(|v| v.as_f64()) {
        Some(n) if n.is_finite() => n / 1000.0,
        _ => rec
            .get("timestamp")
            .and_then(|v| v.as_str())
            .map(iso)
            .unwrap_or(0.0),
    };
    if at > tally.activity {
        tally.activity = at;
    }
    if tally.first_message.is_empty() && m.get("role").and_then(|v| v.as_str()) == Some("user") {
        let t = text_of(&m);
        if !t.is_empty() {
            tally.first_message = t;
        }
    }
}

/// 头尾各 4KB 的指纹。不能盖到文件末尾 —— 小文件每次追加都会挪走末尾。
fn fingerprint(file: &Path, upto: u64) -> Option<[u8; 16]> {
    let mut f = std::fs::File::open(file).ok()?;
    let mut head = vec![0u8; std::cmp::min(upto, FP_BYTES) as usize];
    f.read_exact(&mut head).ok()?;
    let mut tail = Vec::new();
    if upto > FP_BYTES {
        let mut t = vec![0u8; FP_BYTES as usize];
        f.seek(SeekFrom::Start(upto - FP_BYTES)).ok()?;
        f.read_exact(&mut t).ok()?;
        tail = t;
    }
    let mut hasher = Sha512::new();
    hasher.update(&head);
    hasher.update(&tail);
    let digest = hasher.finalize();
    let mut out = [0u8; 16];
    out.copy_from_slice(&digest[..16]);
    Some(out)
}

/// 从 start 字节续扫。offset 一定落在换行之后，末尾那半截 JSON 不推进断点
/// （下次从这儿重读，绝不漏掉）。
fn scan_from(file: &Path, start: u64, tally: &mut Meta) -> Option<(Meta, u64)> {
    let mut f = std::fs::File::open(file).ok()?;
    let mut offset = start;
    let mut pos = start;
    let mut rest: Vec<u8> = Vec::new();
    let mut rest_at = start;

    loop {
        let mut buf = vec![0u8; BLOCK];
        // ⚠️ 必须传**显式 position**。传 None 会从文件当前位置（也就是 0）读，
        // 续扫就把上一次扫过的那一段又累加一遍 —— 症状是「追加一条消息，条数涨两条」。
        let n = match read_at(&mut f, &mut buf, pos) {
            Ok(n) => n,
            Err(_) => return None,
        };
        if n == 0 {
            break;
        }
        pos += n as u64;

        let mut merged = std::mem::take(&mut rest);
        merged.extend_from_slice(&buf[..n]);

        let mut from = 0usize;
        while let Some(rel) = merged[from..].iter().position(|&b| b == b'\n') {
            let nl = from + rel;
            let line = String::from_utf8_lossy(&merged[from..nl]).trim().to_string();
            if !line.is_empty() {
                if let Ok(rec) = serde_json::from_str::<Value>(&line) {
                    absorb(tally, &rec);
                }
            }
            from = nl + 1;
        }
        rest = merged.split_off(from);
        offset = rest_at + rest.len() as u64;
        rest_at = offset;
    }

    // 末尾那半截：正在写的，不推进断点。
    let tail = String::from_utf8_lossy(&rest).trim().to_string();
    if !tail.is_empty() {
        if let Ok(rec) = serde_json::from_str::<Value>(&tail) {
            absorb(tally, &rec);
            offset = rest_at + rest.len() as u64;
        }
    }

    Some((tally.clone(), offset))
}

/// 文件身份：同一份文件（junction / 大小写变体）拿到同一个 key。
///
/// Windows 上用「卷序列号 + 文件索引」，但 `MetadataExt` 的这两个方法还在
/// unstable（`windows_by_handle`）。所以退而求其次：用 **规范化路径 + 大小**。
/// 这在 junction 场景下仍会重复，但 junction 在本机不是常态，可以接受。
fn file_identity(file: &Path) -> String {
    let size = std::fs::metadata(file).map(|m| m.len()).unwrap_or(0);
    format!("{}|{}", paths::norm(file), size)
}

/// 在指定位置读，不移动文件游标。
///
/// `std::fs::File` 没有 `read_at`（那是 `std::os::unix::fs::FileExt`），
/// 这里用 `seek + read` 实现，并且**读之前不重置游标** —— 续扫依赖这一点。
fn read_at(f: &mut std::fs::File, buf: &mut [u8], pos: u64) -> std::io::Result<usize> {
    let saved = f.stream_position()?;
    f.seek(SeekFrom::Start(pos))?;
    let n = f.read(buf);
    let _ = f.seek(SeekFrom::Start(saved));
    n
}

/// 读首行（session 头）。
fn header_of(file: &Path) -> Option<Value> {
    let mut f = std::fs::File::open(file).ok()?;
    let mut out = String::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = f.read(&mut buf).ok()?;
        if n == 0 {
            break;
        }
        out.push_str(&String::from_utf8_lossy(&buf[..n]));
        if let Some(nl) = out.find('\n') {
            out.truncate(nl);
            break;
        }
    }
    let rec: Value = serde_json::from_str(out.trim()).ok()?;
    if rec.get("type").and_then(|v| v.as_str()) == Some("session") {
        Some(rec)
    } else {
        None
    }
}

/// 扫描一份转录（带缓存增量）。
fn scan(file: &Path, size: u64, mtime_ns: u64) -> Meta {
    let key = paths::norm(file);
    let prev = cache().lock().unwrap().get(&key).cloned();

    if let Some(p) = &prev {
        if p.size == size && p.mtime_ns == mtime_ns {
            return p.meta.clone();
        }
        // 指纹一致 = 文件只可能在尾部追加了内容，可以续扫。
        if let Some(fp) = fingerprint(file, p.offset) {
            if fp == p.fp {
                let mut tally = p.meta.clone();
                if let Some((meta, offset)) = scan_from(file, p.offset, &mut tally) {
                    let new_fp = fingerprint(file, offset).unwrap_or(p.fp);
                    return remember(
                        key,
                        CacheEntry {
                            size,
                            mtime_ns,
                            meta,
                            offset,
                            fp: new_fp,
                            header: p.header.clone(),
                        },
                    );
                }
            }
        }
    }

    let mut tally = Meta::default();
    let Some((meta, offset)) = scan_from(file, 0, &mut tally) else {
        return Meta::default();
    };
    let fp = fingerprint(file, offset).unwrap_or([0u8; 16]);
    let header = header_of(file);
    remember(
        key,
        CacheEntry { size, mtime_ns, meta, offset, fp, header },
    )
}

/// 一份转录的对外形状。前端只渲染，不再自己拼标题。
fn session_json(s: &Value) -> Value {
    json!({
        "path": s["path"],
        "id": s["id"],
        "cwd": s["cwd"],
        "created": s["created"],
        "title": s["title"],
        "name": s["name"],
        "firstMessage": s["firstMessage"],
        "messageCount": s["messageCount"],
        "modified": s["modified"],
        "size": s["size"],
        "model": s["model"],
    })
}

/// 描述一份转录。
fn describe(file: &Path) -> Option<Value> {
    let st = std::fs::metadata(file).ok()?;
    let key = paths::norm(file);
    let cached_header = cache().lock().unwrap().get(&key).and_then(|c| c.header.clone());
    let header = match cached_header {
        Some(h) => Some(h),
        None => header_of(file),
    }?;
    let mtime_ns = st
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let meta = scan(file, st.len(), mtime_ns);
    if let Some(cur) = cache().lock().unwrap().get_mut(&key) {
        cur.header = Some(header.clone());
    }

    let cwd = header.get("cwd").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let created = header.get("timestamp").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let id = header
        .get("id")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| {
            file.file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .and_then(|s| s.rsplit('_').next().map(|x| x.to_string()))
                .unwrap_or_default()
        });

    // 「名字 → 首条用户消息 → 占位」只在这里算一次，前端只渲染不重拼。
    let title = if !meta.name.is_empty() {
        meta.name.clone()
    } else if !meta.first_message.is_empty() {
        meta.first_message.clone()
    } else {
        NO_MESSAGES.to_string()
    };

    // 「上次活动」用消息时间戳，不是 mtime —— 改个名也会动 mtime，
    // 但那不该把这份对话顶到「最近」最前面。
    let modified = if meta.activity > 0.0 {
        meta.activity
    } else {
        let by_header = if created.is_empty() { 0.0 } else { iso(&created) };
        if by_header > 0.0 {
            by_header
        } else {
            st.modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs_f64())
                .unwrap_or(0.0)
        }
    };

    Some(json!({
        "path": file.to_string_lossy(),
        "id": id,
        "cwd": cwd,
        "created": created,
        "title": title,
        "name": meta.name,
        "firstMessage": meta.first_message,
        "messageCount": meta.message_count,
        "modified": modified,
        "size": st.len(),
        "model": meta.model,
    }))
}

/// 某个 cwd 的转录可能躺在哪几个目录里。大小写变体要**并集**。
fn folders_for(cwd: &str) -> Vec<PathBuf> {
    let root = paths::sessions_root();
    let slug = paths::slug_for(Path::new(cwd));
    let mut out: Vec<PathBuf> = Vec::new();

    let exact = root.join(&slug);
    if exact.is_dir() {
        out.push(exact);
    }
    if let Ok(entries) = std::fs::read_dir(&root) {
        let mut has_flat_jsonl = false;
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if !entry.path().is_dir() {
                // --session-dir 指向的平铺布局：Pi 直接把 jsonl 写在根下。
                if name.ends_with(".jsonl") {
                    has_flat_jsonl = true;
                }
                continue;
            }
            if name.to_lowercase() != slug.to_lowercase() || name == slug {
                continue;
            }
            out.push(entry.path());
        }
        if has_flat_jsonl {
            out.push(root.clone());
        }
    }

    let mut seen = HashSet::new();
    out.retain(|p| {
        let k = paths::norm(p);
        seen.insert(k)
    });
    out
}

fn read_order(cwd: &str) -> Vec<String> {
    let all = read_json::<Value>(&paths::order_file(), None).unwrap_or_else(|| json!({}));
    all.get(paths::norm(Path::new(cwd)))
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default()
}

/// 写手动排序。
pub fn write_order(cwd: &str, list: &[String]) -> std::io::Result<()> {
    let mut all = read_json::<Value>(&paths::order_file(), None).unwrap_or_else(|| json!({}));
    if !all.is_object() {
        all = json!({});
    }
    all[paths::norm(Path::new(cwd))] = json!(list);
    crate::atomic::write_json_atomic_pretty(&paths::order_file(), &all, 1)
}

/// 套上手动排序。
fn apply_order(found: Vec<Value>, order: Vec<String>) -> Vec<Value> {
    if order.is_empty() {
        return found;
    }
    let mut rank: HashMap<String, usize> = HashMap::new();
    for (i, p) in order.iter().enumerate() {
        let k = paths::norm(Path::new(p));
        rank.entry(k).or_insert(i);
    }
    let mut pinned: Vec<Value> = Vec::new();
    let mut rest: Vec<Value> = Vec::new();
    for s in found {
        let k = paths::norm(Path::new(s["path"].as_str().unwrap_or("")));
        if rank.contains_key(&k) {
            pinned.push(s);
        } else {
            rest.push(s);
        }
    }
    pinned.sort_by_key(|s| {
        let k = paths::norm(Path::new(s["path"].as_str().unwrap_or("")));
        *rank.get(&k).unwrap_or(&usize::MAX)
    });
    pinned.extend(rest);
    pinned
}

/// 某个工作目录下的会话，最近活动在前，再套手动排序。
pub fn list_sessions(cwd: &str) -> Vec<Value> {
    let wanted = paths::norm(Path::new(cwd));
    let mut found: Vec<Value> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();

    for folder in folders_for(cwd) {
        let Ok(names) = std::fs::read_dir(&folder) else {
            continue;
        };
        for entry in names.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if !name.ends_with(".jsonl") {
                continue;
            }
            let file = folder.join(&name);
            let Some(s) = describe(&file) else {
                continue;
            };
            if paths::norm(Path::new(s["cwd"].as_str().unwrap_or(""))) != wanted {
                continue;
            }
            // 同一份转录可能从两个目录各读到一次（junction / 大小写变体）：
            // 用文件标识认「这份文件」，不是认路径字符串。
            let key = file_identity(&file);
            if !seen.insert(key) {
                continue;
            }
            found.push(s);
        }
    }

    found.sort_by(|a, b| {
        let ma = a["modified"].as_f64().unwrap_or(0.0);
        let mb = b["modified"].as_f64().unwrap_or(0.0);
        mb.partial_cmp(&ma).unwrap_or(std::cmp::Ordering::Equal)
    });
    apply_order(found, read_order(cwd))
}

/// 把目录 slug 解回路径。`--D--code--foo--` → `D:\code\foo`。
fn decode_slug(name: &str) -> String {
    let inner = name.trim_start_matches("--").trim_end_matches("--");
    if let Some(rest) = inner.strip_prefix("--") {
        if let Some(drive) = rest.chars().next() {
            if drive.is_ascii_alphabetic() {
                let tail = rest[1..].strip_prefix("--").unwrap_or(&rest[1..]);
                return format!("{}:\\{}", drive.to_ascii_uppercase(), tail.replace("--", "\\"));
            }
        }
    }
    inner.replace("--", "\\")
}

/// 全局列表，给设置页「历史会话」用。
pub fn list_all_sessions(project: Option<&str>, limit: usize) -> Value {
    let root = paths::sessions_root();
    let Ok(entries) = std::fs::read_dir(&root) else {
        return json!({ "ok": true, "sessions": [], "projects": [], "root": root.to_string_lossy(), "total": 0 });
    };

    let dirs: Vec<String> = entries
        .flatten()
        .filter(|e| e.path().is_dir())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();

    let pick: Vec<Option<String>> = match project {
        Some(p) if !p.is_empty() => dirs
            .iter()
            .filter(|d| d.as_str() == p)
            .map(|d| Some(d.clone()))
            .collect(),
        _ => dirs.iter().map(|d| Some(d.clone())).chain(std::iter::once(None)).collect(),
    };

    let mut rows: Vec<Value> = Vec::new();
    for dir in pick {
        let folder = match &dir {
            Some(d) => root.join(d),
            None => root.clone(),
        };
        let Ok(names) = std::fs::read_dir(&folder) else {
            continue;
        };
        for entry in names.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if !name.ends_with(".jsonl") {
                continue;
            }
            let file = folder.join(&name);
            let Some(s) = describe(&file) else {
                continue;
            };
            let mut row = session_json(&s);
            row["project"] = json!(dir.clone().unwrap_or_default());
            row["projectPath"] = json!(match &dir {
                Some(d) => decode_slug(d),
                None => s["cwd"].as_str().unwrap_or("").to_string(),
            });
            row["file"] = json!(name);
            row["mtime"] = json!(s["modified"].as_f64().unwrap_or(0.0) * 1000.0);
            row["createdAt"] = s["created"].clone();
            row["model"] = s["model"].clone();
            rows.push(row);
        }
    }

    rows.sort_by(|a, b| {
        let ma = a["mtime"].as_f64().unwrap_or(0.0);
        let mb = b["mtime"].as_f64().unwrap_or(0.0);
        mb.partial_cmp(&ma).unwrap_or(std::cmp::Ordering::Equal)
    });

    let total = rows.len();
    let capped = limit.clamp(1, 500).min(total);
    let sessions: Vec<Value> = rows.into_iter().take(capped).collect();

    json!({
        "ok": true,
        "sessions": sessions,
        "projects": dirs.iter().map(|d| json!({ "name": d, "path": decode_slug(d) })).collect::<Vec<_>>(),
        "root": root.to_string_lossy(),
        "total": total,
    })
}

/// 左栏「最近项目」：从转录里收集。
pub fn known_workspaces() -> Vec<String> {
    let root = paths::sessions_root();
    let mut folders = vec![root.clone()];
    if let Ok(entries) = std::fs::read_dir(&root) {
        for e in entries.flatten() {
            if e.path().is_dir() {
                folders.push(e.path());
            }
        }
    }

    let mut seen: HashMap<String, (String, u64)> = HashMap::new();
    let mut reachable: HashMap<String, HashSet<String>> = HashMap::new();

    for folder in &folders {
        let Ok(names) = std::fs::read_dir(folder) else {
            continue;
        };
        let mut with_time: Vec<(String, u64)> = Vec::new();
        for entry in names.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if !name.ends_with(".jsonl") {
                continue;
            }
            let path = folder.join(&name);
            if let Ok(m) = std::fs::metadata(&path) {
                let mt = m
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_millis() as u64)
                    .unwrap_or(0);
                with_time.push((name, mt));
            }
        }
        with_time.sort_by_key(|x| std::cmp::Reverse(x.1));

        for (name, mtime) in with_time {
            let file = folder.join(&name);
            let Some(header) = header_of(&file) else {
                continue;
            };
            let cwd = header.get("cwd").and_then(|v| v.as_str()).unwrap_or("").to_string();
            if cwd.is_empty() {
                continue;
            }
            let key = paths::norm(Path::new(&cwd));
            let set = reachable.entry(key.clone()).or_insert_with(|| {
                folders_for(&cwd)
                    .iter()
                    .map(|p| paths::norm(p))
                    .collect()
            });
            if !set.contains(&paths::norm(folder)) {
                continue;
            }
            // 目录没了：点开必然报错，不列。
            if !Path::new(&cwd).is_dir() {
                continue;
            }
            let slot = seen.entry(key).or_insert_with(|| (cwd.clone(), 0));
            if mtime > slot.1 {
                *slot = (cwd.clone(), mtime);
            }
            break; // 每个目录只看最新那份
        }
    }

    let mut out: Vec<(String, u64)> = seen.into_values().collect();
    out.sort_by_key(|x| std::cmp::Reverse(x.1));
    out.into_iter().map(|(cwd, _)| cwd).collect()
}

/// 路径是不是会话转录（在会话目录内的 .jsonl）。
pub fn is_session_file(file: &str) -> bool {
    let candidate = paths::dunce_resolve(Path::new(file));
    let root = paths::dunce_resolve(&paths::sessions_root());
    candidate.extension().and_then(|e| e.to_str()) == Some("jsonl")
        && candidate.starts_with(&root)
}

/// 改名 = 往转录尾部追加一条 session_info（与 Pi 的 /name 同形）。
pub fn rename_session(file: &str, name: &str) -> Result<(), String> {
    let target = paths::dunce_resolve(Path::new(file));
    if !is_session_file(&target.to_string_lossy()) {
        return Err("拒绝操作：路径不是会话转录".to_string());
    }
    let st = std::fs::metadata(&target).map_err(|e| format!("读不到文件: {e}"))?;
    let size = st.len();

    // 从尾部找 parentId（最后一条带 id 的记录）。
    let tail: String;
    {
        use std::io::Read;
        let mut f = std::fs::File::open(&target).map_err(|e| format!("打开失败: {e}"))?;
        let want = std::cmp::min(size, TAIL_BYTES) as usize;
        let mut buf = vec![0u8; want];
        f.seek(SeekFrom::Start(size.saturating_sub(want as u64)))
            .map_err(|e| format!("seek 失败: {e}"))?;
        let n = f.read(&mut buf).unwrap_or(0);
        tail = String::from_utf8_lossy(&buf[..n]).to_string();
    }

    let mut parent_id: Option<String> = None;
    for line in tail.lines().rev() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Ok(r) = serde_json::from_str::<Value>(line) {
            if let Some(id) = r.get("id").and_then(|v| v.as_str()) {
                parent_id = Some(id.to_string());
                break;
            }
        }
    }

    let rec = json!({
        "type": "session_info",
        "id": random_hex(4),
        "parentId": parent_id,
        "timestamp": now_iso(),
        "name": name.split_whitespace().collect::<Vec<_>>().join(" "),
    });

    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .open(&target)
        .map_err(|e| format!("追加失败: {e}"))?;
    writeln!(f, "{}", serde_json::to_string(&rec).unwrap_or_default())
        .map_err(|e| format!("写入失败: {e}"))?;

    forget(&target.to_string_lossy());
    Ok(())
}

/// 删会话 / 删项目目录。返回去向。
pub async fn remove(target: &str, recycle: bool) -> Result<Value, String> {
    let file = paths::dunce_resolve(Path::new(target));
    let is_dir = file.is_dir();
    if !is_dir && !is_session_file(&file.to_string_lossy()) {
        return Err("拒绝删除：路径不在会话目录内".to_string());
    }
    remove_path(&file, recycle).await
}

/// 已由调用方校验过路径的删除。Pi 之外的 Agent（Claude / Codex）也走这里，
/// 回收站与跨卷兜底只维护一份实现。
pub async fn remove_path(file: &Path, recycle: bool) -> Result<Value, String> {
    let file = paths::dunce_resolve(file);
    if recycle && send_to_recycle_bin(&file).await {
        forget(&file.to_string_lossy());
        return Ok(json!({ "method": "recycle", "where": null }));
    }

    let trash = paths::trash_dir();
    let _ = std::fs::create_dir_all(&trash);
    let stamp = now_iso().replace(['-', ':', 'T'], "").chars().take(14).collect::<String>();
    let base = file
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "item".to_string());
    let mut dest = trash.join(format!("{stamp}__{base}"));
    let mut n = 1;
    while dest.exists() {
        dest = trash.join(format!("{stamp}-{}__{}", n, base));
        n += 1;
    }

    if std::fs::rename(&file, &dest).is_err() {
        // 跨卷时 rename 失败，退化成复制 + 删除。
        copy_recursive(&file, &dest).map_err(|e| format!("移动失败: {e}"))?;
        let _ = std::fs::remove_dir_all(&file);
        let _ = std::fs::remove_file(&file);
    }
    forget(&file.to_string_lossy());
    Ok(json!({ "method": "trash", "where": dest.to_string_lossy() }))
}

fn copy_recursive(from: &Path, to: &Path) -> std::io::Result<()> {
    let meta = std::fs::metadata(from)?;
    if meta.is_dir() {
        std::fs::create_dir_all(to)?;
        for entry in std::fs::read_dir(from)? {
            let entry = entry?;
            copy_recursive(&entry.path(), &to.join(entry.file_name()))?;
        }
    } else {
        std::fs::copy(from, to)?;
    }
    Ok(())
}

/// 系统回收站。走 PowerShell 的 VisualBasic FileSystem（等价于 SHFileOperationW + FOF_ALLOWUNDO）。
async fn send_to_recycle_bin(target: &Path) -> bool {
    let script = format!(
        "Add-Type -AssemblyName Microsoft.VisualBasic; \
         [Microsoft.VisualBasic.FileIO.FileSystem]::DeleteFile('{}', 'OnlyErrorDialogs', 'SendToRecycleBin')",
        target.to_string_lossy().replace('\'', "''")
    );
    let out = crate::proc::CmdSpec::owned(
        "powershell.exe",
        &[
            "-NoProfile".to_string(),
            "-NonInteractive".to_string(),
            "-Command".to_string(),
            script.clone(),
        ],
    )
    .timeout(15_000)
    .run()
    .await;
    if out.is_err() {
        return false;
    }
    !target.exists()
}

/// 导出：原样复制一份转录到用户给的路径。
pub fn export_session(file: &str, out: &str) -> Result<Value, String> {
    let src = paths::dunce_resolve(Path::new(file));
    if !is_session_file(&src.to_string_lossy()) {
        return Err("拒绝导出：路径不是会话转录".to_string());
    }
    let dest = if out.trim().is_empty() {
        paths::state_dir()
            .join("exports")
            .join(src.file_name().unwrap_or_default())
    } else {
        paths::dunce_resolve(Path::new(out))
    };
    if let Some(parent) = dest.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    std::fs::copy(&src, &dest).map_err(|e| format!("复制失败: {e}"))?;
    Ok(json!({ "ok": true, "path": dest.to_string_lossy() }))
}

/// 在**真实终端窗口**里打开某份转录。对话只走终端，这是唯一的「打开会话」入口。
pub async fn open_in_pi(session_file: &str, cwd: Option<&str>) -> Result<Value, String> {
    let file = paths::dunce_resolve(Path::new(session_file));
    if !is_session_file(&file.to_string_lossy()) {
        return Err("拒绝打开：路径不是会话转录".to_string());
    }
    let workdir = cwd
        .map(|s| s.to_string())
        .unwrap_or_else(|| {
            std::env::current_dir()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_default()
        });
    let bin = find_pi_binary();
    let cmdline = crate::launch::build_cmdline(&[
        bin.to_string_lossy().to_string(),
        "--session".to_string(),
        file.to_string_lossy().to_string(),
    ]);
    let mark = crate::launch::make_mark("DSP");
    let r = crate::launch::launch_in_terminal(
        std::path::Path::new(&workdir),
        &cmdline,
        "Pi · 会话",
        Some(mark.clone()),
        None,
    )
    .await;
    crate::windows::remember_window(crate::windows::WindowRecord {
        mark: mark.clone(),
        kind: "pi".to_string(),
        title: Some("Pi · 会话".to_string()),
        session_id: None,
        cwd: Some(workdir.clone()),
        hwnd: r.hwnd,
        focused: r.focused,
        at: now_millis(),
    });
    Ok(json!({
        "ok": r.ok,
        "launcher": "wt",
        "command": cmdline,
        "cwd": workdir,
        "mark": mark,
        "error": r.error,
    }))
}

/// 与 `pi.rs` 同一套候选顺序，但这里不引它（避免成环）。
fn find_pi_binary() -> PathBuf {
    let candidates = [
        paths::pi_home().join("agent").join("bin").join("pi.cmd"),
        paths::pi_home().join("agent").join("bin").join("pi.ps1"),
    ];
    for f in candidates {
        if f.is_file() {
            return f;
        }
    }
    PathBuf::from("pi")
}

fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn random_hex(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    let _ = getrandom(&mut buf);
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

/// 极简随机：读系统时间 + pid 混合。够用来做 session_info 的 id（不用于安全）。
fn getrandom(buf: &mut [u8]) -> std::io::Result<()> {
    use std::time::{SystemTime, UNIX_EPOCH};
    let now = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    let mut state = now as u64 ^ ((std::process::id() as u64) << 17);
    for slot in buf.iter_mut() {
        // xorshift64*
        state ^= state >> 12;
        state ^= state << 25;
        state ^= state >> 27;
        let v = state.wrapping_mul(0x2545F4914F6CDD1D);
        *slot = (v >> 33) as u8;
    }
    Ok(())
}

/// 当前时间的 ISO 串（带 Z）。
fn now_iso() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.000Z",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

/// 天数 → 民用日期。
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
