//! 各 Agent 的对话统一视图。
//!
//! 每个 Agent 的会话存储格式都不一样：
//!
//! | Agent    | 位置                                                  | 格式 |
//! |----------|-------------------------------------------------------|------|
//! | Pi       | `~/.pi/agent/sessions/--<cwd>--/*.jsonl`              | Pi 自己的 JSONL（`type: session/message/model_change`） |
//! | Claude   | `~/.claude/projects/<slug>/*.jsonl`                   | Claude Code JSONL（`type: user/assistant`） |
//! | Codex    | `~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl`        | Codex rollout JSONL（`session_meta/response_item/turn_context`） |
//! | OpenCode | `~/.local/share/opencode/opencode.db`                 | SQLite（`session_v2` + `session_message`） |
//! | DSH      | `~/.dsh/storages/session_projcache/sessions/*.json`    | DSH 会话索引（`record.rows.*.val`，只读） |
//! | Argent   | `~/.argent/sessions/*.jsonl`                           | Argent 会话（`header` + `message` 行） |
//! | omp      | `~/.omp/agent/sessions/--<cwd>--/*.jsonl`              | omp JSONL（`title/session/model_change/message`） |
//!
//! 设计上每个来源是一个 `SessionSource`：**扫描**与**生成启动/删除计划**各自负责，
//! HTTP 层只做参数校验和异步执行。加第七个 Agent 时不需要改前面的分支。
//!
//! ## 资源占用
//!
//! JSONL 会话动辄几十 MB（实测 Codex 有一份 31 MB）。列表只扫一次并缓存
//! `(文件大小, 已扫到的字节偏移, 摘要)`；文件只追加，所以下次从断点续扫。
//! 文件没变时零 I/O —— 面板每分钟刷新也不会重复读盘。
//!
//! ## 安全边界
//!
//! 打开/删除的路径必须落在各自 Agent 的会话根目录内，且是 `.jsonl`。
//! 前端传来的路径一律重新规范化 + 重新校验，不信任列表里的旧值。

use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use serde_json::{json, Value};

use crate::paths;

const BLOCK: usize = 256 * 1024;
const TITLE_MAX: usize = 180;
const LIST_MAX: usize = 500;

// ---------------------------------------------------------------- Agent

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Agent {
    Pi,
    Claude,
    Codex,
    OpenCode,
    Dsh,
    Argent,
    Zcode,
    Kimicli,
    Omp,
}

impl Agent {
    pub const ALL: [Agent; 9] = [
        Agent::Pi,
        Agent::Claude,
        Agent::Codex,
        Agent::OpenCode,
        Agent::Dsh,
        Agent::Argent,
        Agent::Zcode,
        Agent::Kimicli,
        Agent::Omp,
    ];

    pub fn id(self) -> &'static str {
        match self {
            Agent::Pi => "pi",
            Agent::Claude => "claude",
            Agent::Codex => "codex",
            Agent::OpenCode => "opencode",
            Agent::Dsh => "dsh",
            Agent::Argent => "argent",
            Agent::Zcode => "zcode",
            Agent::Kimicli => "kimicli",
            Agent::Omp => "omp",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Agent::Pi => "Pi",
            Agent::Claude => "Claude",
            Agent::Codex => "Codex",
            Agent::OpenCode => "OpenCode",
            Agent::Dsh => "DSH",
            Agent::Argent => "Argent",
            Agent::Zcode => "ZCode",
            Agent::Kimicli => "Kimi CLI",
            Agent::Omp => "omp",
        }
    }

    pub fn parse(s: &str) -> Option<Agent> {
        Agent::ALL.into_iter().find(|a| a.id() == s)
    }
}

// ---------------------------------------------------------------- 会话

#[derive(Debug, Clone)]
pub struct Session {
    pub agent: Agent,
    /// Agent 自己的会话 ID（Claude UUID / Codex UUID / OpenCode `ses_…` / Pi header id）。
    pub id: String,
    pub title: String,
    pub cwd: String,
    pub model: String,
    pub messages: u32,
    /// Unix 秒。界面显示「3 分钟前 / 昨天 / 11-14」。
    pub modified: f64,
    pub size: u64,
    /// JSONL 文件的绝对路径；OpenCode 为空（它只有数据库里的 ID）。
    pub path: String,
}

impl Session {
    fn empty(agent: Agent) -> Session {
        Session {
            agent,
            id: String::new(),
            title: String::new(),
            cwd: String::new(),
            model: String::new(),
            messages: 0,
            modified: 0.0,
            size: 0,
            path: String::new(),
        }
    }

    pub fn to_json(&self) -> Value {
        json!({
            "key": format!("{}:{}", self.agent.id(), if self.path.is_empty() { &self.id } else { &self.path }),
            "agent": self.agent.id(),
            "agentName": self.agent.label(),
            "id": self.id,
            "title": self.title,
            "cwd": self.cwd,
            "model": self.model,
            "messages": self.messages,
            "modified": self.modified,
            "size": self.size,
            "path": self.path,
            "when": when_text(self.modified),
        })
    }
}

// ---------------------------------------------------------------- 启动 / 删除计划

pub struct OpenPlan {
    pub cwd: PathBuf,
    pub argv: Vec<String>,
    pub title: String,
    /// 非终端类入口：桌面应用的协议 URI（DSH 只能聚焦窗口，不能按会话直达）。
    pub uri: Option<String>,
}

impl OpenPlan {
    fn cmdline(&self) -> String {
        crate::launch::build_cmdline(&self.argv)
    }
}

pub enum DeletePlan {
    /// 会话文件（走回收站）。
    File(PathBuf),
    /// Agent 自己的 CLI（OpenCode 的 `session delete`）。
    Command { argv: Vec<String>, cwd: PathBuf },
    /// DSH：真正的会话数据目录 + 索引缓存 + workspace.json 三处一起处理。
    Dsh {
        id: String,
        cache_file: PathBuf,
        data_dir: Option<PathBuf>,
        workspace: PathBuf,
    },
}

// ---------------------------------------------------------------- 来源接口

pub trait SessionSource: Send + Sync {
    fn agent(&self) -> Agent;
    /// 列出全部会话。失败（目录不存在不算失败）要返回人话。
    fn scan(&self) -> Result<Vec<Session>, String>;
    /// 校验之后给出「怎么打开」。cwd 必须存在。
    fn open_plan(&self, s: &Session) -> Result<OpenPlan, String>;
    /// 校验之后给出「怎么删除」。
    fn delete_plan(&self, s: &Session) -> Result<DeletePlan, String>;
    /// 删除是否安全。DSH 的 projcache 只是索引，真正的会话在别处。
    fn deletable(&self) -> bool {
        true
    }
}

pub fn source(agent: Agent) -> Box<dyn SessionSource> {
    match agent {
        Agent::Pi => Box::new(PiSource),
        Agent::Claude => Box::new(ClaudeSource),
        Agent::Codex => Box::new(CodexSource),
        Agent::OpenCode => Box::new(OpenCodeSource),
        Agent::Dsh => Box::new(DshSource),
        Agent::Argent => Box::new(ArgentSource),
        Agent::Zcode => Box::new(ZcodeSource),
        Agent::Kimicli => Box::new(KimicliSource),
        Agent::Omp => Box::new(OmpSource),
    }
}

// ---------------------------------------------------------------- 公共：JSONL 摘要缓存

#[derive(Clone, Default)]
struct Summary {
    id: String,
    title: String,
    cwd: String,
    model: String,
    messages: u32,
    activity: f64,
    created: f64,
}

#[derive(Clone)]
struct CacheEntry {
    size: u64,
    offset: u64,
    summary: Summary,
}

fn cache() -> &'static Mutex<HashMap<String, CacheEntry>> {
    static CACHE: OnceLock<Mutex<HashMap<String, CacheEntry>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn forget(file: &Path) {
    cache().lock().unwrap().remove(&paths::norm(file));
}

/// 扫一份只追加的 JSONL，带增量缓存。
fn scan_jsonl(file: &Path, mut absorb: impl FnMut(&mut Summary, &Value)) -> Summary {
    let Ok(st) = std::fs::metadata(file) else {
        return Summary::default();
    };
    let size = st.len();
    let key = paths::norm(file);

    let prev = cache().lock().unwrap().get(&key).cloned();
    let (mut summary, mut offset, append_only) = match prev {
        Some(p) if p.size == size => return p.summary,
        Some(p) if p.size < size && p.offset <= size => (p.summary, p.offset, true),
        _ => (Summary::default(), 0, false),
    };
    if !append_only {
        summary = Summary::default();
    }

    // `offset` 一定落在换行之后。文件被重写（变小）时从头来。
    if let Ok(new_offset) = scan_lines(file, offset, &mut summary, &mut absorb) {
        offset = new_offset;
    }

    let mut map = cache().lock().unwrap();
    if map.len() >= 4096 {
        map.clear();
    }
    map.insert(
        key,
        CacheEntry {
            size,
            offset,
            summary: summary.clone(),
        },
    );
    summary
}

/// 从 start 字节开始，逐行解析。返回值停在**最后一个完整行的末尾**，
/// 末尾尚未写完的半截 JSON 不推进断点。
fn scan_lines(
    file: &Path,
    start: u64,
    summary: &mut Summary,
    absorb: &mut impl FnMut(&mut Summary, &Value),
) -> std::io::Result<u64> {
    let mut f = std::fs::File::open(file)?;
    f.seek(SeekFrom::Start(start))?;
    let mut rest: Vec<u8> = Vec::new();
    let mut offset = start;

    loop {
        // `rest` 的绝对起点。读块之前记下来，处理换行时用它算绝对偏移。
        let base = offset;
        let mut buf = vec![0u8; BLOCK];
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        let mut merged = std::mem::take(&mut rest);
        merged.extend_from_slice(&buf[..n]);

        let mut from = 0usize;
        while let Some(rel) = merged[from..].iter().position(|&b| b == b'\n') {
            let nl = from + rel;
            let line = String::from_utf8_lossy(&merged[from..nl]);
            let line = line.trim();
            if !line.is_empty() {
                if let Ok(rec) = serde_json::from_str::<Value>(line) {
                    absorb(summary, &rec);
                }
            }
            from = nl + 1;
        }
        rest = merged.split_off(from);
        // 末尾半截不推进断点：没有换行时 from == 0，offset 原地不动。
        offset = base + from as u64;
    }
    Ok(offset)
}

fn touch(summary: &mut Summary, secs: f64) {
    if secs <= 0.0 {
        return;
    }
    if summary.activity < secs {
        summary.activity = secs;
    }
    if summary.created <= 0.0 || secs < summary.created {
        summary.created = secs;
    }
}

/// ISO 时间 → Unix 秒。只认 `YYYY-MM-DDTHH:MM:SS(.sss)?(Z|±HH:MM)`。
fn iso(t: &str) -> f64 {
    let b = t.as_bytes();
    if b.len() < 19 {
        return 0.0;
    }
    let n = |s: &str| s.parse::<i64>().ok();
    let (Some(y), Some(mo), Some(d)) = (n(&t[0..4]), n(&t[5..7]), n(&t[8..10])) else {
        return 0.0;
    };
    let (Some(h), Some(mi), Some(se)) = (n(&t[11..13]), n(&t[14..16]), n(&t[17..19])) else {
        return 0.0;
    };
    let days = days_from_civil(y, mo as u32, d as u32);
    let mut secs = days * 86_400 + h * 3600 + mi * 60 + se;
    let rest = &t[19..];
    if !rest.starts_with('Z') && rest.len() >= 6 {
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

fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = if m > 2 { m - 3 } else { m + 9 } as i64;
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

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

fn when_text(secs: f64) -> String {
    if secs <= 0.0 {
        return "未知".to_string();
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0);
    let age = (now - secs).max(0.0);
    if age < 60.0 {
        return "刚刚".to_string();
    }
    if age < 3600.0 {
        return format!("{} 分钟前", (age / 60.0) as i64);
    }
    if age < 86_400.0 {
        return format!("{} 小时前", (age / 3600.0) as i64);
    }
    if age < 2.0 * 86_400.0 {
        return "昨天".to_string();
    }
    let days = (secs as i64).div_euclid(86_400);
    let (_, m, d) = civil_from_days(days);
    format!("{m:02}-{d:02}")
}

/// 折叠空白并截断。标题只用于列表，不塞整段对话。
fn tidy(text: &str) -> String {
    let mut out = String::with_capacity(text.len().min(TITLE_MAX * 2));
    let mut space = false;
    for ch in text.chars() {
        if ch.is_whitespace() {
            if !space {
                out.push(' ');
                space = true;
            }
        } else {
            out.push(ch);
            space = false;
            if out.chars().count() >= TITLE_MAX {
                out.push('…');
                break;
            }
        }
    }
    out.trim().to_string()
}

/// Claude / Codex 的注入消息不是用户写的，不能拿来做标题。
fn is_meta_text(text: &str) -> bool {
    let t = text.trim_start().to_lowercase();
    t.starts_with("<environment_context>")
        || t.starts_with("<skills_instructions>")
        || t.starts_with("<user_instructions>")
        || t.starts_with("<local-command-caveat>")
        || t.starts_with("<command-name>")
        || t.starts_with("<command-message>")
        || t.starts_with("<system-reminder>")
        || t.starts_with("caveat:")
        || t.starts_with("[request interrupted")
        || t.starts_with("## skills")
        || t.starts_with("# agents.md")
        || t.starts_with("<turn_aborted>")
}

/// 从 `message.content` 里抽文本。Claude 用 `text`，Codex 用 `input_text/output_text`。
fn content_text(message: &Value) -> String {
    let Some(content) = message.get("content") else {
        return String::new();
    };
    if let Some(s) = content.as_str() {
        return tidy(s);
    }
    let Some(blocks) = content.as_array() else {
        return String::new();
    };
    let mut out = String::new();
    for b in blocks {
        let kind = b.get("type").and_then(Value::as_str).unwrap_or("");
        if !matches!(kind, "text" | "input_text" | "output_text") {
            continue;
        }
        if let Some(t) = b.get("text").and_then(Value::as_str) {
            if !out.is_empty() {
                out.push(' ');
            }
            out.push_str(t);
        }
    }
    tidy(&out)
}

/// 路径必须落在 Agent 的会话根目录内，且是 `.jsonl`。
fn valid_session_file(root: &Path, path: &str) -> Result<PathBuf, String> {
    let file = paths::dunce_resolve(Path::new(path));
    let root = paths::dunce_resolve(root);
    if file.extension().and_then(|e| e.to_str()) != Some("jsonl") {
        return Err("拒绝操作：不是 .jsonl 会话文件".to_string());
    }
    if !file.starts_with(&root) || file == root {
        return Err("拒绝操作：路径不在该 Agent 的会话目录内".to_string());
    }
    if !file.is_file() {
        return Err(format!("会话文件不存在：{}", file.display()));
    }
    Ok(file)
}

/// DSH 的会话索引是 `.json`（不是 `.jsonl`），同样要求路径落在 DSH 缓存目录内。
fn valid_dsh_cache_file(root: &Path, path: &str) -> Result<PathBuf, String> {
    let file = paths::dunce_resolve(Path::new(path));
    let root = paths::dunce_resolve(root);
    if file.extension().and_then(|e| e.to_str()) != Some("json") {
        return Err("拒绝操作：不是 DSH .json 会话索引文件".to_string());
    }
    if !file.starts_with(&root) || file == root {
        return Err("拒绝操作：路径不在 DSH 会话索引目录内".to_string());
    }
    if !file.is_file() {
        return Err(format!("DSH 会话索引不存在：{}", file.display()));
    }
    Ok(file)
}

fn cwd_or_home(cwd: &str) -> PathBuf {
    let c = PathBuf::from(cwd);
    if c.is_dir() {
        c
    } else {
        paths::home()
    }
}

fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// ID 前缀，只用于占位标题。按字符截，避免 UUID 以外的内容切在 UTF-8 中间。
fn short_id(id: &str) -> String {
    id.chars().take(8).collect()
}

fn npm_bin(name: &str) -> String {
    let candidate = paths::home()
        .join("AppData")
        .join("Roaming")
        .join("npm")
        .join(format!("{name}.cmd"));
    if candidate.is_file() {
        candidate.to_string_lossy().to_string()
    } else {
        name.to_string()
    }
}

fn claude_bin() -> String {
    let candidate = paths::home().join(".local").join("bin").join("claude.exe");
    if candidate.is_file() {
        candidate.to_string_lossy().to_string()
    } else {
        "claude".to_string()
    }
}

fn pi_bin() -> String {
    let bin = paths::agent_dir().join("bin");
    for name in ["pi.cmd", "pi.ps1"] {
        let candidate = bin.join(name);
        if candidate.is_file() {
            return candidate.to_string_lossy().to_string();
        }
    }
    "pi".to_string()
}

fn dirs_jsonl(root: &Path, max_depth: usize) -> Vec<PathBuf> {
    let mut out = Vec::new();
    fn walk(dir: &Path, depth: usize, max: usize, out: &mut Vec<PathBuf>) {
        if depth > max {
            return;
        }
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, depth + 1, max, out);
            } else if p.extension().and_then(|x| x.to_str()) == Some("jsonl") {
                out.push(p);
            }
        }
    }
    walk(root, 0, max_depth, &mut out);
    out
}

// ---------------------------------------------------------------- Pi

struct PiSource;

impl SessionSource for PiSource {
    fn agent(&self) -> Agent {
        Agent::Pi
    }

    fn scan(&self) -> Result<Vec<Session>, String> {
        let v = crate::transcripts::list_all_sessions(None, LIST_MAX);
        let rows = v
            .get("sessions")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let mut out = Vec::with_capacity(rows.len());
        for r in rows {
            let mut s = Session::empty(Agent::Pi);
            s.id = r.get("id").and_then(Value::as_str).unwrap_or("").to_string();
            s.title = r.get("title").and_then(Value::as_str).unwrap_or("").to_string();
            s.cwd = r.get("cwd").and_then(Value::as_str).unwrap_or("").to_string();
            s.model = r.get("model").and_then(Value::as_str).unwrap_or("").to_string();
            s.messages = r.get("messageCount").and_then(Value::as_u64).unwrap_or(0) as u32;
            s.modified = r.get("modified").and_then(Value::as_f64).unwrap_or(0.0);
            s.size = r.get("size").and_then(Value::as_u64).unwrap_or(0);
            s.path = r.get("path").and_then(Value::as_str).unwrap_or("").to_string();
            if s.title.is_empty() {
                s.title = if s.id.is_empty() {
                    "Pi 会话".to_string()
                } else {
                    format!("Pi · {}", short_id(&s.id))
                };
            }
            out.push(s);
        }
        Ok(out)
    }

    fn open_plan(&self, s: &Session) -> Result<OpenPlan, String> {
        let file = valid_session_file(&paths::sessions_root(), &s.path)?;
        Ok(OpenPlan {
            cwd: cwd_or_home(&s.cwd),
            argv: vec![
                pi_bin(),
                "--session".to_string(),
                file.to_string_lossy().to_string(),
            ],
            title: "Pi · 会话".to_string(),
            uri: None,
        })
    }

    fn delete_plan(&self, s: &Session) -> Result<DeletePlan, String> {
        Ok(DeletePlan::File(valid_session_file(
            &paths::sessions_root(),
            &s.path,
        )?))
    }
}

// ---------------------------------------------------------------- Claude

struct ClaudeSource;

fn claude_root() -> PathBuf {
    paths::home().join(".claude").join("projects")
}

fn absorb_claude(s: &mut Summary, rec: &Value) {
    if rec.get("isSidechain").and_then(Value::as_bool) == Some(true) {
        return;
    }
    if let Some(ts) = rec.get("timestamp").and_then(Value::as_str) {
        touch(s, iso(ts));
    }
    if s.cwd.is_empty() {
        if let Some(c) = rec.get("cwd").and_then(Value::as_str) {
            s.cwd = c.to_string();
        }
    }
    if s.id.is_empty() {
        if let Some(id) = rec.get("sessionId").and_then(Value::as_str) {
            s.id = id.to_string();
        }
    }
    match rec.get("type").and_then(Value::as_str).unwrap_or("") {
        "user" => {
            if let Some(m) = rec.get("message") {
                s.messages += 1;
                if s.title.is_empty() {
                    let text = content_text(m);
                    if !text.is_empty() && !is_meta_text(&text) {
                        s.title = text;
                    }
                }
            }
        }
        "assistant" => {
            if let Some(m) = rec.get("message") {
                s.messages += 1;
                if let Some(model) = m.get("model").and_then(Value::as_str).filter(|x| !x.is_empty()) {
                    s.model = model.to_string();
                }
            }
        }
        _ => {}
    }
}

impl SessionSource for ClaudeSource {
    fn agent(&self) -> Agent {
        Agent::Claude
    }

    fn scan(&self) -> Result<Vec<Session>, String> {
        let root = claude_root();
        let Ok(projects) = std::fs::read_dir(&root) else {
            return Ok(Vec::new());
        };
        let mut out = Vec::new();
        for project in projects.flatten() {
            let dir = project.path();
            if !dir.is_dir() {
                continue;
            }
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let file = entry.path();
                if !file.is_file()
                    || file.extension().and_then(|e| e.to_str()) != Some("jsonl")
                {
                    continue;
                }
                let summary = scan_jsonl(&file, absorb_claude);
                if summary.messages == 0 && summary.activity <= 0.0 {
                    continue;
                }
                let size = std::fs::metadata(&file).map(|m| m.len()).unwrap_or(0);
                let id = if summary.id.is_empty() {
                    file.file_stem()
                        .map(|s| s.to_string_lossy().to_string())
                        .unwrap_or_default()
                } else {
                    summary.id.clone()
                };
                let mut s = Session::empty(Agent::Claude);
                s.id = id.clone();
                s.title = if summary.title.is_empty() {
                    format!("Claude · {}", short_id(&id))
                } else {
                    summary.title
                };
                s.cwd = summary.cwd;
                s.model = summary.model;
                s.messages = summary.messages;
                s.modified = summary.activity;
                s.size = size;
                s.path = file.to_string_lossy().to_string();
                out.push(s);
            }
        }
        Ok(out)
    }

    fn open_plan(&self, s: &Session) -> Result<OpenPlan, String> {
        let file = valid_session_file(&claude_root(), &s.path)?;
        let id = file
            .file_stem()
            .map(|x| x.to_string_lossy().to_string())
            .ok_or("会话文件名无效")?;
        Ok(OpenPlan {
            cwd: cwd_or_home(&s.cwd),
            argv: vec![claude_bin(), "--resume".to_string(), id],
            title: "Claude · 会话".to_string(),
            uri: None,
        })
    }

    fn delete_plan(&self, s: &Session) -> Result<DeletePlan, String> {
        Ok(DeletePlan::File(valid_session_file(
            &claude_root(),
            &s.path,
        )?))
    }
}

// ---------------------------------------------------------------- Codex

struct CodexSource;

fn codex_root() -> PathBuf {
    paths::home().join(".codex").join("sessions")
}

/// `rollout-2026-11-14T22-45-24-<uuid>.jsonl` → `<uuid>`。
fn codex_id_from_file(file: &Path) -> Option<String> {
    let stem = file.file_stem()?.to_string_lossy();
    let rest = stem.strip_prefix("rollout-")?;
    if rest.len() < 36 {
        return None;
    }
    let cand = &rest[rest.len() - 36..];
    let ok = cand.len() == 36
        && cand
            .chars()
            .enumerate()
            .all(|(i, c)| match i {
                8 | 13 | 18 | 23 => c == '-',
                _ => c.is_ascii_hexdigit(),
            });
    if ok {
        Some(cand.to_string())
    } else {
        None
    }
}

fn absorb_codex(s: &mut Summary, rec: &Value) {
    if let Some(ts) = rec.get("timestamp").and_then(Value::as_str) {
        touch(s, iso(ts));
    }
    let kind = rec.get("type").and_then(Value::as_str).unwrap_or("");
    let payload = rec.get("payload").cloned().unwrap_or(Value::Null);

    match kind {
        "session_meta" => {
            if s.id.is_empty() {
                if let Some(id) = payload.get("id").and_then(Value::as_str) {
                    s.id = id.to_string();
                } else if let Some(id) = payload.get("session_id").and_then(Value::as_str) {
                    s.id = id.to_string();
                }
            }
            if s.cwd.is_empty() {
                if let Some(c) = payload.get("cwd").and_then(Value::as_str) {
                    s.cwd = c.to_string();
                }
            }
        }
        "turn_context" => {
            if let Some(m) = payload.get("model").and_then(Value::as_str).filter(|x| !x.is_empty()) {
                s.model = m.to_string();
            }
        }
        "event_msg" => {
            if payload.get("type").and_then(Value::as_str) == Some("thread_settings_applied") {
                if let Some(m) = payload
                    .get("thread_settings")
                    .and_then(|t| t.get("model"))
                    .and_then(Value::as_str)
                    .filter(|x| !x.is_empty())
                {
                    s.model = m.to_string();
                }
            }
        }
        "response_item" => {
            if payload.get("type").and_then(Value::as_str) != Some("message") {
                return;
            }
            match payload.get("role").and_then(Value::as_str).unwrap_or("") {
                "user" => {
                    s.messages += 1;
                    if s.title.is_empty() {
                        let text = content_text(&payload);
                        if !text.is_empty() && !is_meta_text(&text) {
                            s.title = text;
                        }
                    }
                }
                "assistant" => s.messages += 1,
                _ => {}
            }
        }
        _ => {}
    }
}

impl SessionSource for CodexSource {
    fn agent(&self) -> Agent {
        Agent::Codex
    }

    fn scan(&self) -> Result<Vec<Session>, String> {
        let files = dirs_jsonl(&codex_root(), 5);
        let mut out = Vec::new();
        for file in files {
            let summary = scan_jsonl(&file, absorb_codex);
            if summary.messages == 0 && summary.activity <= 0.0 {
                continue;
            }
            let id = codex_id_from_file(&file)
                .or_else(|| (!summary.id.is_empty()).then(|| summary.id.clone()))
                .unwrap_or_default();
            let size = std::fs::metadata(&file).map(|m| m.len()).unwrap_or(0);
            let mut s = Session::empty(Agent::Codex);
            s.id = id.clone();
            s.title = if summary.title.is_empty() {
                if id.is_empty() {
                    "Codex 会话".to_string()
                } else {
                    format!("Codex · {}", short_id(&id))
                }
            } else {
                summary.title
            };
            s.cwd = summary.cwd;
            s.model = summary.model;
            s.messages = summary.messages;
            s.modified = summary.activity;
            s.size = size;
            s.path = file.to_string_lossy().to_string();
            out.push(s);
        }
        Ok(out)
    }

    fn open_plan(&self, s: &Session) -> Result<OpenPlan, String> {
        let file = valid_session_file(&codex_root(), &s.path)?;
        let id = codex_id_from_file(&file).ok_or("无法从 Codex 文件名解析会话 ID")?;
        Ok(OpenPlan {
            cwd: cwd_or_home(&s.cwd),
            argv: vec![
                npm_bin("codex"),
                "resume".to_string(),
                id,
            ],
            title: "Codex · 会话".to_string(),
            uri: None,
        })
    }

    fn delete_plan(&self, s: &Session) -> Result<DeletePlan, String> {
        Ok(DeletePlan::File(valid_session_file(
            &codex_root(),
            &s.path,
        )?))
    }
}

// ---------------------------------------------------------------- OpenCode

struct OpenCodeSource;

fn opencode_id(id: &str) -> Result<(), String> {
    if !id.starts_with("ses_")
        || !id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Err("OpenCode 会话 ID 形状不对".to_string());
    }
    Ok(())
}

fn opencode_first_user(data: &str) -> String {
    let Ok(v) = serde_json::from_str::<Value>(data) else {
        return String::new();
    };
    if let Some(t) = v.get("text").and_then(Value::as_str) {
        return tidy(t);
    }
    if let Some(t) = v
        .get("metadata")
        .and_then(|m| m.get("displayText"))
        .and_then(Value::as_str)
    {
        return tidy(t);
    }
    String::new()
}

impl SessionSource for OpenCodeSource {
    fn agent(&self) -> Agent {
        Agent::OpenCode
    }

    fn scan(&self) -> Result<Vec<Session>, String> {
        let db_path = paths::opencode_db_path();
        if !db_path.is_file() {
            return Ok(Vec::new());
        }
        let db = crate::sqlite::Db::open_readonly(&db_path)?;
        let mut out = Vec::new();
        db.query(
            "SELECT s.id, s.title, s.directory, s.model, s.time_updated, \
             (SELECT COUNT(*) FROM session_message m \
              WHERE m.session_id = s.id AND m.type IN ('user','assistant')) AS n, \
             (SELECT m.data FROM session_message m \
              WHERE m.session_id = s.id AND m.type = 'user' \
              ORDER BY m.seq LIMIT 1) AS first_user \
             FROM session_v2 s WHERE s.parent_id IS NULL \
             ORDER BY s.time_updated DESC LIMIT 200",
            &[],
            |row| {
                let id = row.text(0);
                if id.is_empty() {
                    return Ok(());
                }
                let mut s = Session::empty(Agent::OpenCode);
                s.id = id.clone();
                let title = row.text(1);
                s.title = if title.trim().is_empty() {
                    let first = opencode_first_user(&row.text(6));
                    if first.is_empty() {
                        format!("OpenCode · {}", short_id(&id))
                    } else {
                        first
                    }
                } else {
                    tidy(&title)
                };
                s.cwd = row.text(2);
                let model_json = row.text(3);
                s.model = serde_json::from_str::<Value>(&model_json)
                    .ok()
                    .and_then(|m| m.get("id").and_then(Value::as_str).map(str::to_string))
                    .unwrap_or_default();
                s.messages = row.int(5).max(0) as u32;
                s.modified = row.int(4).max(0) as f64 / 1000.0;
                s.path = String::new();
                out.push(s);
                Ok(())
            },
        )?;
        Ok(out)
    }

    fn open_plan(&self, s: &Session) -> Result<OpenPlan, String> {
        opencode_id(&s.id)?;
        Ok(OpenPlan {
            cwd: cwd_or_home(&s.cwd),
            argv: vec![
                npm_bin("opencode"),
                "--session".to_string(),
                s.id.clone(),
            ],
            title: "OpenCode · 会话".to_string(),
            uri: None,
        })
    }

    fn delete_plan(&self, s: &Session) -> Result<DeletePlan, String> {
        opencode_id(&s.id)?;
        let db_path = paths::opencode_db_path();
        if !db_path.is_file() {
            return Err("找不到 OpenCode 数据库".to_string());
        }
        // 先确认这条会话真的存在，避免把任意 ID 交给 CLI。
        let db = crate::sqlite::Db::open_readonly(&db_path)?;
        let mut found = false;
        db.query(
            "SELECT 1 FROM session_v2 WHERE id = ?1 LIMIT 1",
            &[&s.id],
            |_| {
                found = true;
                Ok(())
            },
        )?;
        if !found {
            return Err("OpenCode 里找不到这条会话".to_string());
        }
        Ok(DeletePlan::Command {
            argv: vec![
                npm_bin("opencode"),
                "session".to_string(),
                "delete".to_string(),
                s.id.clone(),
            ],
            cwd: cwd_or_home(&s.cwd),
        })
    }
}

// ---------------------------------------------------------------- DSH

/// DSH 的会话索引缓存（`~/.dsh/storages/session_projcache/sessions`）。
///
/// 每份 JSON 是 DSH 自己的 UI 快照，字段在 `record.rows.<key>.val`：
/// `title` / `modelSelection.lastUsed` / `sessionStats.turns` /
/// `sessionListMetadata.{lastPromptAt,blank}`，cwd 在 `record.identity.cwd`。
/// 真正的对话事件不在这里，所以这里**只读**。
struct DshSource;

fn dsh_val<'a>(rows: &'a Value, key: &str) -> Option<&'a Value> {
    rows.get(key)?.get("val")
}

/// 标题优先用 DSH 自己生成的；没有就退到第一轮用户输入。
fn dsh_title(rows: &Value) -> String {
    if let Some(t) = dsh_val(rows, "title").and_then(Value::as_str) {
        if !t.trim().is_empty() {
            return tidy(t);
        }
    }
    dsh_val(rows, "turnOutline")
        .and_then(|v| v.get("turns"))
        .and_then(Value::as_array)
        .and_then(|turns| {
            turns.iter().find_map(|t| {
                t.get("prompt")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|p| !p.is_empty())
            })
        })
        .map(tidy)
        .unwrap_or_default()
}

fn dsh_model(rows: &Value) -> String {
    let Some(last) = dsh_val(rows, "modelSelection").and_then(|v| v.get("lastUsed")) else {
        return String::new();
    };
    let provider = last
        .get("provider")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    let model = last.get("model").and_then(Value::as_str).unwrap_or("").trim();
    match (provider.is_empty(), model.is_empty()) {
        (true, true) => String::new(),
        (true, false) => model.to_string(),
        (false, true) => provider.to_string(),
        (false, false) => format!("{provider}/{model}"),
    }
}

fn mtime_secs(md: &std::fs::Metadata) -> f64 {
    md.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

impl SessionSource for DshSource {
    fn agent(&self) -> Agent {
        Agent::Dsh
    }

    fn scan(&self) -> Result<Vec<Session>, String> {
        let root = paths::dsh_session_cache_dir();
        let Ok(entries) = std::fs::read_dir(&root) else {
            return Ok(Vec::new());
        };
        let mut out = Vec::new();
        for entry in entries.flatten() {
            let file = entry.path();
            if !file.is_file() || file.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&file) else {
                continue;
            };
            let Ok(doc) = serde_json::from_str::<Value>(&text) else {
                continue;
            };
            let Some(rows) = doc.pointer("/record/rows") else {
                continue;
            };
            // DSH 的 subagent 也各有一份 projcache JSON：它们是一次对话里并行跑的
            // 不同任务（文件名没有 session- 前缀，subagent.identity.mode 非空），
            // 不是顶层会话。之前把 4 个 subagent 误当成 4 条对话列出来了。
            let is_subagent = doc
                .pointer("/record/rows/subagent/val/identity/mode")
                .and_then(Value::as_str)
                .is_some_and(|m| !m.trim().is_empty());
            if is_subagent {
                continue;
            }
            // 空白会话（新建但没提问）DSH 自己也不展示。
            let blank = dsh_val(rows, "sessionListMetadata")
                .and_then(|v| v.get("blank"))
                .and_then(Value::as_bool)
                .unwrap_or(false);
            if blank {
                continue;
            }
            let Ok(md) = std::fs::metadata(&file) else {
                continue;
            };
            let id = file
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default();
            if id.is_empty() {
                continue;
            }

            let mut s = Session::empty(Agent::Dsh);
            s.id = id.clone();
            let title = dsh_title(rows);
            s.title = if title.is_empty() {
                format!("DSH · {}", short_id(&id))
            } else {
                title
            };
            s.cwd = doc
                .pointer("/record/identity/cwd")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            s.model = dsh_model(rows);
            s.messages = dsh_val(rows, "sessionStats")
                .and_then(|v| v.get("turns"))
                .and_then(Value::as_u64)
                .unwrap_or(0) as u32;
            let last = dsh_val(rows, "sessionListMetadata")
                .and_then(|v| v.get("lastPromptAt"))
                .and_then(Value::as_f64)
                .unwrap_or(0.0);
            let created = doc
                .pointer("/record/identity/createdAt")
                .and_then(Value::as_f64)
                .unwrap_or(0.0);
            s.modified = if last > 0.0 {
                last / 1000.0
            } else if created > 0.0 {
                created / 1000.0
            } else {
                mtime_secs(&md)
            };
            s.size = md.len();
            s.path = file.to_string_lossy().to_string();
            out.push(s);
        }
        Ok(out)
    }

    fn open_plan(&self, s: &Session) -> Result<OpenPlan, String> {
        Ok(OpenPlan {
            cwd: cwd_or_home(&s.cwd),
            argv: Vec::new(),
            title: "DSH · 会话".to_string(),
            // `dsh://open` 只会聚焦主窗口，没有按会话深链；先这样，别假装能直达。
            uri: Some("dsh://open".to_string()),
        })
    }

    fn delete_plan(&self, s: &Session) -> Result<DeletePlan, String> {
        let cache_file = valid_dsh_cache_file(&paths::dsh_session_cache_dir(), &s.path)?;
        // 真正的会话数据在 ~/.dsh/sessions/<cwd-slug>/<id>/session.v4.jsonl.zstd。
        // 按 id 找子目录，不依赖 slug 规则。
        let mut data_dir = None;
        if let Ok(dirs) = std::fs::read_dir(paths::dsh_home().join("sessions")) {
            for d in dirs.flatten() {
                let candidate = d.path().join(&s.id);
                if candidate.is_dir() {
                    data_dir = Some(candidate);
                    break;
                }
            }
        }
        Ok(DeletePlan::Dsh {
            id: s.id.clone(),
            cache_file,
            data_dir,
            workspace: paths::dsh_home().join("storages").join("workspace.json"),
        })
    }

    fn deletable(&self) -> bool {
        true
    }
}

// ---------------------------------------------------------------- Argent

/// Argent 里不足 3 轮的会话：扫描时排队，等 list 返回后异步移入回收站。
static ARGENT_PENDING: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

/// 取走待删的短会话（由面板在会话列表读完后异步处理）。
pub fn take_pending_deletes() -> Vec<PathBuf> {
    std::mem::take(&mut ARGENT_PENDING.lock().unwrap())
}

/// Argent（Rust coding agent）的会话存储：`~/.argent/sessions/*.jsonl`。
///
/// 首行是 `{"kind":"header","data":{"cwd","title","created","version"}}`，
/// 之后每行是 `{"kind":"message","message":{"role","content",...},"created_at":ms}`。
struct ArgentSource;

fn argent_bin() -> String {
    if let Ok(v) = std::env::var("ARGENT_BIN") {
        if !v.trim().is_empty() {
            return v;
        }
    }
    "argent".to_string()
}

impl SessionSource for ArgentSource {
    fn agent(&self) -> Agent {
        Agent::Argent
    }

    fn scan(&self) -> Result<Vec<Session>, String> {
        let root = paths::argent_home().join("sessions");
        let Ok(entries) = std::fs::read_dir(&root) else {
            return Ok(Vec::new());
        };
        let mut out = Vec::new();
        for entry in entries.flatten() {
            let file = entry.path();
            if !file.is_file() || file.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&file) else {
                continue;
            };
            let mut lines = text.lines();
            let first = lines.next().unwrap_or("");
            let Ok(header) = serde_json::from_str::<Value>(first) else {
                continue;
            };
            if header.get("kind").and_then(Value::as_str) != Some("header") {
                continue;
            }
            let data = header.get("data");
            let mut s = Session::empty(Agent::Argent);
            s.id = file
                .file_stem()
                .map(|v| v.to_string_lossy().to_string())
                .unwrap_or_default();
            if s.id.is_empty() {
                continue;
            }
            s.title = data
                .and_then(|d| d.get("title"))
                .and_then(Value::as_str)
                .filter(|t| !t.trim().is_empty())
                .map(tidy)
                .unwrap_or_else(|| format!("Argent · {}", short_id(&s.id)));
            s.cwd = data
                .and_then(|d| d.get("cwd"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            s.modified = header
                .get("created_at")
                .and_then(Value::as_f64)
                .map(|ms| ms / 1000.0)
                .unwrap_or(0.0);
            let mut users = 0u32;
            for line in lines {
                let Ok(rec) = serde_json::from_str::<Value>(line) else {
                    continue;
                };
                if rec.get("kind").and_then(Value::as_str) == Some("message") {
                    if rec
                        .pointer("/message/role")
                        .and_then(Value::as_str)
                        == Some("user")
                    {
                        users += 1;
                    }
                    if let Some(ms) = rec.get("created_at").and_then(Value::as_f64) {
                        s.modified = ms / 1000.0;
                    }
                }
            }
            // 用户要求：不足 3 轮的 Argent 会话直接删（移入回收站），不区分时间。
            if users < 3 {
                let mut pending = ARGENT_PENDING.lock().unwrap();
                if !pending.contains(&file) {
                    pending.push(file);
                }
                continue;
            }
            s.messages = users;
            if let Ok(md) = std::fs::metadata(&file) {
                s.size = md.len();
                if s.modified <= 0.0 {
                    s.modified = mtime_secs(&md);
                }
            }
            s.path = file.to_string_lossy().to_string();
            out.push(s);
        }
        Ok(out)
    }

    fn open_plan(&self, s: &Session) -> Result<OpenPlan, String> {
        let file = valid_session_file(&paths::argent_home().join("sessions"), &s.path)?;
        Ok(OpenPlan {
            cwd: cwd_or_home(&s.cwd),
            argv: vec![argent_bin(), "-r".to_string(), file.to_string_lossy().to_string()],
            title: "Argent · 会话".to_string(),
            uri: None,
        })
    }

    fn delete_plan(&self, s: &Session) -> Result<DeletePlan, String> {
        Ok(DeletePlan::File(valid_session_file(
            &paths::argent_home().join("sessions"),
            &s.path,
        )?))
    }
}

// ---------------------------------------------------------------- ZCode / Kimi CLI

/// ZCode 真正的会话在 `~/.zcode/cli/db/db.sqlite` 的 `session` 表（不是 minzcode）。
struct ZcodeSource;

fn zcode_exe() -> String {
    for p in [r"D:\zcode\ZCode.exe"] {
        if Path::new(p).is_file() {
            return p.to_string();
        }
    }
    "ZCode.exe".to_string()
}

impl SessionSource for ZcodeSource {
    fn agent(&self) -> Agent {
        Agent::Zcode
    }

    fn scan(&self) -> Result<Vec<Session>, String> {
        let db = paths::home()
            .join(".zcode")
            .join("cli")
            .join("db")
            .join("db.sqlite");
        if !db.is_file() {
            return Ok(Vec::new());
        }
        let conn = crate::sqlite::Db::open_readonly(&db)
            .map_err(|e| format!("打开 ZCode 会话库失败：{e}"))?;
        let mut out = Vec::new();
        conn.query(
            "select s.id, s.directory, s.title, s.time_created, s.time_updated, \
             (select count(*) from message m where m.session_id = s.id) \
             from session s where s.time_deleted is null \
             order by s.time_updated desc limit 300",
            &[],
            |r| {
                let mut s = Session::empty(Agent::Zcode);
                s.id = r.text(0);
                let title = r.text(2);
                s.title = if title.trim().is_empty() {
                    format!("ZCode · {}", short_id(&s.id))
                } else {
                    tidy(&title)
                };
                s.cwd = r.text(1);
                s.messages = r.int(5).max(0) as u32;
                s.modified = (r.int(4).max(r.int(3)) as f64) / 1000.0;
                s.path = s.id.clone();
                out.push(s);
                Ok(())
            },
        )
        .map_err(|e| format!("读取 ZCode 会话失败：{e}"))?;
        Ok(out)
    }

    fn open_plan(&self, s: &Session) -> Result<OpenPlan, String> {
        // ZCode 桌面端没有按会话深链，只能启动应用。
        Ok(OpenPlan {
            cwd: cwd_or_home(&s.cwd),
            argv: vec![zcode_exe()],
            title: "ZCode · 会话".to_string(),
            uri: None,
        })
    }

    fn deletable(&self) -> bool {
        // 会话在 sqlite 里，删行会破坏 ZCode 自己的状态，只读不删。
        false
    }

    fn delete_plan(&self, _s: &Session) -> Result<DeletePlan, String> {
        Err("ZCode 会话在它自己的数据库里，请在 ZCode 内删除".to_string())
    }
}

/// Kimi CLI 自己的会话在 `~/.kimi-code/sessions/**/state.json`
/// （Kimi Desktop 的那套索引不是 CLI 的，不在这里读）。
struct KimicliSource;

fn kimi_exe() -> String {
    if let Ok(v) = std::env::var("KIMI_BIN") {
        if !v.trim().is_empty() {
            return v;
        }
    }
    let native = paths::kimi_home().join("bin").join("kimi.exe");
    if native.is_file() {
        return native.to_string_lossy().to_string();
    }
    let npm = paths::home().join("AppData").join("Roaming").join("npm").join("kimi.cmd");
    if npm.is_file() {
        return npm.to_string_lossy().to_string();
    }
    "kimi".to_string()
}

impl SessionSource for KimicliSource {
    fn agent(&self) -> Agent {
        Agent::Kimicli
    }

    fn scan(&self) -> Result<Vec<Session>, String> {
        let root = paths::kimi_home().join("sessions");
        let Ok(entries) = std::fs::read_dir(&root) else {
            return Ok(Vec::new());
        };
        let mut out = Vec::new();
        for ws in entries.flatten() {
            let Ok(sessions) = std::fs::read_dir(ws.path()) else {
                continue;
            };
            for dir in sessions.flatten() {
                if !dir.path().is_dir() {
                    continue;
                }
                let state_file = dir.path().join("state.json");
                let Ok(text) = std::fs::read_to_string(&state_file) else {
                    continue;
                };
                let Ok(state) = serde_json::from_str::<Value>(&text) else {
                    continue;
                };
                let mut s = Session::empty(Agent::Kimicli);
                s.id = dir.file_name().to_string_lossy().to_string();
                s.path = dir.path().to_string_lossy().to_string();
                s.title = state
                    .get("title")
                    .and_then(Value::as_str)
                    .filter(|t| !t.trim().is_empty())
                    .map(tidy)
                    .unwrap_or_else(|| format!("Kimi · {}", short_id(&s.id)));
                s.cwd = ws.file_name().to_string_lossy().to_string();
                let wire = dir.path().join("agents").join("main").join("wire.jsonl");
                if let Ok(w) = std::fs::read_to_string(&wire) {
                    s.messages = w
                        .lines()
                        .filter(|line| line.contains("\"role\":\"user\""))
                        .count() as u32;
                }
                if let Ok(md) = std::fs::metadata(&dir.path()) {
                    s.size = md.len();
                    s.modified = mtime_secs(&md);
                }
                out.push(s);
            }
        }
        Ok(out)
    }

    fn open_plan(&self, s: &Session) -> Result<OpenPlan, String> {
        Ok(OpenPlan {
            cwd: cwd_or_home(&s.cwd),
            argv: vec![kimi_exe(), "-S".to_string(), s.id.clone()],
            title: "Kimi CLI · 会话".to_string(),
            uri: None,
        })
    }

    fn delete_plan(&self, s: &Session) -> Result<DeletePlan, String> {
        let dir = paths::dunce_resolve(Path::new(&s.path));
        let root = paths::dunce_resolve(&paths::kimi_home().join("sessions"));
        if dir == root || !dir.starts_with(&root) {
            return Err("拒绝操作：路径不在 Kimi CLI 会话目录内".to_string());
        }
        Ok(DeletePlan::File(dir))
    }
}

// ---------------------------------------------------------------- omp（oh-my-pi）

struct OmpSource;

fn omp_exe() -> String {
    if let Ok(v) = std::env::var("OMP_BIN") {
        if !v.trim().is_empty() {
            return v;
        }
    }
    let native = paths::home().join(".bun").join("bin").join("omp.exe");
    if native.is_file() {
        return native.to_string_lossy().to_string();
    }
    "omp".to_string()
}

fn omp_sessions_root() -> PathBuf {
    paths::omp_home().join("agent").join("sessions")
}

/// omp 的会话 JSONL：`title` / `session` / `model_change` / `message` 行。
/// 同名的 `*.jsonl/` 目录（里面是 `changes.jsonl`）不算会话文件。
fn absorb_omp(s: &mut Summary, rec: &Value) {
    match rec.get("type").and_then(Value::as_str) {
        Some("session") => {
            if s.id.is_empty() {
                s.id = rec
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
            }
            if s.cwd.is_empty() {
                s.cwd = rec
                    .get("cwd")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
            }
        }
        Some("title") | Some("title_change") => {
            if let Some(t) = rec.get("title").and_then(Value::as_str) {
                if !t.trim().is_empty() {
                    s.title = tidy(t);
                }
            }
            if let Some(ts) = rec.get("updatedAt").and_then(Value::as_str) {
                let secs = iso(ts);
                if secs > s.activity {
                    s.activity = secs;
                }
            }
        }
        Some("model_change") => {
            s.model = rec
                .get("model")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
        }
        Some("message") => {
            if rec.pointer("/message/role").and_then(Value::as_str) == Some("user") {
                s.messages += 1;
            }
            if let Some(ts) = rec.get("timestamp").and_then(Value::as_str) {
                let secs = iso(ts);
                if secs > s.activity {
                    s.activity = secs;
                }
            }
        }
        _ => {}
    }
}

impl SessionSource for OmpSource {
    fn agent(&self) -> Agent {
        Agent::Omp
    }

    fn scan(&self) -> Result<Vec<Session>, String> {
        let root = omp_sessions_root();
        let Ok(workspaces) = std::fs::read_dir(&root) else {
            return Ok(Vec::new());
        };
        let mut out = Vec::new();
        for ws in workspaces.flatten() {
            if !ws.path().is_dir() {
                continue;
            }
            let Ok(entries) = std::fs::read_dir(ws.path()) else {
                continue;
            };
            for entry in entries.flatten() {
                let file = entry.path();
                if !file.is_file() || file.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                    continue;
                }
                let summary = scan_jsonl(&file, absorb_omp);
                if summary.messages == 0 {
                    continue;
                }
                let mut s = Session::empty(Agent::Omp);
                s.id = if summary.id.is_empty() {
                    file.file_stem()
                        .map(|v| v.to_string_lossy().to_string())
                        .unwrap_or_default()
                } else {
                    summary.id.clone()
                };
                if s.id.is_empty() {
                    continue;
                }
                s.cwd = summary.cwd.clone();
                s.model = summary.model.clone();
                s.messages = summary.messages;
                s.title = if summary.title.is_empty() {
                    format!("omp · {}", short_id(&s.id))
                } else {
                    summary.title.clone()
                };
                if let Ok(md) = std::fs::metadata(&file) {
                    s.size = md.len();
                    s.modified = if summary.activity > 0.0 {
                        summary.activity
                    } else {
                        mtime_secs(&md)
                    };
                }
                s.path = file.to_string_lossy().to_string();
                out.push(s);
            }
        }
        Ok(out)
    }

    fn open_plan(&self, s: &Session) -> Result<OpenPlan, String> {
        Ok(OpenPlan {
            cwd: cwd_or_home(&s.cwd),
            argv: vec![omp_exe(), "-r".to_string(), s.id.clone()],
            title: "omp · 会话".to_string(),
            uri: None,
        })
    }

    fn delete_plan(&self, s: &Session) -> Result<DeletePlan, String> {
        Ok(DeletePlan::File(valid_session_file(
            &omp_sessions_root(),
            &s.path,
        )?))
    }
}

// ---------------------------------------------------------------- 对外入口

fn session_from_request(agent: Agent, path: &str, id: &str) -> Session {
    let mut s = Session::empty(agent);
    s.path = path.to_string();
    s.id = id.to_string();
    s
}

/// 列表。`agent=None` 是全部；`q` 在标题/目录/模型/ID 里做不区分大小写的包含。
pub fn list(agent: Option<Agent>, q: &str, limit: usize) -> Value {
    let mut rows: Vec<Session> = Vec::new();
    let mut counts = serde_json::Map::new();
    let mut errors = serde_json::Map::new();

    for a in Agent::ALL {
        let src = source(a);
        match src.scan() {
            Ok(found) => {
                counts.insert(a.id().to_string(), json!(found.len()));
                rows.extend(found);
            }
            Err(e) => {
                counts.insert(a.id().to_string(), json!(0));
                errors.insert(a.id().to_string(), json!(e));
            }
        }
    }

    rows.sort_by(|a, b| {
        b.modified
            .partial_cmp(&a.modified)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let needle = q.trim().to_lowercase();
    let mut shown: Vec<Session> = rows
        .into_iter()
        .filter(|s| agent.map(|a| s.agent == a).unwrap_or(true))
        .filter(|s| {
            if needle.is_empty() {
                return true;
            }
            s.title.to_lowercase().contains(&needle)
                || s.cwd.to_lowercase().contains(&needle)
                || s.model.to_lowercase().contains(&needle)
                || s.id.to_lowercase().contains(&needle)
        })
        .collect();
    let matched = shown.len();
    shown.truncate(limit.clamp(1, LIST_MAX));
    let total: usize = counts
        .values()
        .filter_map(Value::as_u64)
        .map(|n| n as usize)
        .sum();

    json!({
        "ok": true,
        "sessions": shown.iter().map(Session::to_json).collect::<Vec<_>>(),
        "counts": counts,
        "errors": errors,
        "matched": matched,
        "total": total,
        "supported": Agent::ALL.iter().map(|a| json!({
            "id": a.id(),
            "name": a.label(),
        })).collect::<Vec<_>>(),
    })
}

/// 给「复制打开」按钮用：只生成命令文本和工作目录，不真的启动终端 / 应用。
pub fn open_command_text(agent: &str, path: &str, id: &str) -> Result<(String, String), String> {
    let a = Agent::parse(agent).ok_or("未知 Agent")?;
    let src = source(a);
    let sess = session_from_request(a, path, id);
    let plan = src.open_plan(&sess)?;
    let command = match plan.uri.as_deref() {
        Some(uri) => uri.to_string(),
        None => plan.cmdline(),
    };
    Ok((command, plan.cwd.to_string_lossy().to_string()))
}

/// 打开会话：在真实终端里跑对应 Agent 的 resume 命令。
pub async fn open(agent: &str, path: &str, id: &str) -> Result<Value, String> {
    let a = Agent::parse(agent).ok_or("未知 Agent")?;
    let src = source(a);
    let sess = session_from_request(a, path, id);
    let plan = src.open_plan(&sess)?;
    if let Some(uri) = plan.uri.as_deref() {
        open::that_detached(uri).map_err(|e| format!("启动 DSH 失败：{e}"))?;
        return Ok(json!({
            "ok": true,
            "agent": a.id(),
            "launcher": "app",
            "command": uri,
            "note": "DSH 不支持按会话直达，已切到 DSH 窗口；请在左侧会话列表里选择",
        }));
    }
    let mark = crate::launch::make_mark("DSP");
    let cmdline = plan.cmdline();
    let r = crate::launch::launch_in_terminal(
        &plan.cwd,
        &cmdline,
        &plan.title,
        Some(mark.clone()),
        None,
    )
    .await;
    crate::windows::remember_window(crate::windows::WindowRecord {
        mark: mark.clone(),
        kind: a.id().to_string(),
        title: Some(plan.title.clone()),
        session_id: (!sess.id.is_empty()).then(|| sess.id.clone()),
        cwd: Some(plan.cwd.to_string_lossy().to_string()),
        hwnd: r.hwnd,
        focused: r.focused,
        at: now_millis(),
    });
    Ok(json!({
        "ok": r.ok,
        "agent": a.id(),
        "launcher": "wt",
        "command": cmdline,
        "cwd": plan.cwd.to_string_lossy(),
        "mark": mark,
        "error": r.error,
    }))
}

/// 删除一条会话的核心逻辑：按 Session 生成删除计划并执行。
///
/// `File` 计划走 [`crate::transcripts::remove_path`]（可选移入回收站），
/// `Command` 计划跑 Agent 自己的 CLI（OpenCode 的 `session delete`）。
/// `delete` 与 [`auto_purge`] 共用这一份实现。
/// 在 DSH 的 workspace.json 里把一条会话移出 sessionIds、记入 archivedSessionIds。
/// 这是 DSH 自己的「删除」语义（归档），保留数据可恢复性。返回是否真的改动了。
fn dsh_archive_in_workspace(doc: &mut Value, id: &str) -> bool {
    let mut changed = false;
    if let Some(ws) = doc
        .pointer_mut("/tables/workspaces")
        .and_then(Value::as_object_mut)
    {
        for (_, w) in ws.iter_mut() {
            if let Some(ids) = w.get_mut("sessionIds").and_then(Value::as_array_mut) {
                let before = ids.len();
                ids.retain(|v| v.as_str() != Some(id));
                changed |= ids.len() != before;
            }
        }
    }
    if let Some(archived) = doc
        .pointer_mut("/global/archivedSessionIds")
        .and_then(Value::as_array_mut)
    {
        if !archived.iter().any(|v| v.as_str() == Some(id)) {
            archived.push(json!(id));
            changed = true;
        }
    }
    changed
}

async fn delete_session(a: Agent, s: &Session, recycle: bool) -> Result<Value, String> {
    let src = source(a);
    match src.delete_plan(s)? {
        DeletePlan::File(file) => {
            forget(&file);
            let r = crate::transcripts::remove_path(&file, recycle).await?;
            Ok(json!({
                "ok": true,
                "agent": a.id(),
                "method": r.get("method").cloned().unwrap_or(Value::Null),
                "where": r.get("where").cloned().unwrap_or(Value::Null),
            }))
        }
        DeletePlan::Command { argv, cwd } => {
            if argv.is_empty() {
                return Err("删除命令为空".to_string());
            }
            let out = crate::proc::CmdSpec::owned(&argv[0], &argv[1..])
                .cwd(&cwd.to_string_lossy())
                .timeout(60_000)
                .run()
                .await?;
            if out.failed {
                let msg = if !out.stderr.trim().is_empty() {
                    out.stderr.trim().to_string()
                } else {
                    format!("OpenCode 删除失败（退出码 {:?}）", out.code)
                };
                return Err(msg);
            }
            Ok(json!({
                "ok": true,
                "agent": a.id(),
                "method": "cli",
                "output": out.stdout.trim(),
            }))
        }
        DeletePlan::Dsh {
            id,
            cache_file,
            data_dir,
            workspace,
        } => {
            let mut moved = Vec::new();
            // 真正的会话数据目录（zstd 事件流）。
            if let Some(dir) = data_dir {
                forget(&dir);
                let r = crate::transcripts::remove_path(&dir, recycle).await?;
                if let Some(w) = r.get("where").and_then(Value::as_str) {
                    moved.push(format!("数据：{w}"));
                }
            }
            // projcache 索引：删了它 DSH 重启后不会再列出这条。
            forget(&cache_file);
            let r = crate::transcripts::remove_path(&cache_file, recycle).await?;
            if let Some(w) = r.get("where").and_then(Value::as_str) {
                moved.push(format!("索引：{w}"));
            }
            // workspace.json：移出 sessionIds 并记入 archivedSessionIds（DSH 自己
            // 的「删除」也是归档语义，保留可恢复性）。
            if let Ok(text) = std::fs::read_to_string(&workspace) {
                if let Ok(mut doc) = serde_json::from_str::<Value>(&text) {
                    if dsh_archive_in_workspace(&mut doc, &id) {
                        let _ = crate::atomic::write_json_atomic(&workspace, &doc, 2);
                    }
                }
            }
            Ok(json!({
                "ok": true,
                "agent": a.id(),
                "method": "dsh",
                "where": moved.join("; "),
            }))
        }
    }
}

/// 删除会话。JSONL 走回收站；OpenCode 走它自己的 CLI。
pub async fn delete(agent: &str, path: &str, id: &str, recycle: bool) -> Result<Value, String> {
    let a = Agent::parse(agent).ok_or("未知 Agent")?;
    let sess = session_from_request(a, path, id);
    delete_session(a, &sess, recycle).await
}

/// 自动清理：消息数 1..=max_msgs 且最后修改早于 max_age_secs 的会话，移入回收站。
///
/// 只扫 `Agent::ALL`，某个来源扫描失败就跳过它。消息数 0 的会话**一律跳过**
/// —— 那多半是还没解析出来的 transcript，删掉会丢数据。永不硬删，只进回收站。
pub async fn auto_purge(max_msgs: u32, max_age_secs: f64) -> Value {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0);
    let mut purged: u64 = 0;
    let mut failed: u64 = 0;
    let mut items: Vec<Value> = Vec::new();
    for a in Agent::ALL {
        // DSH 可以手动删，但自动清理绝不能碰它：数据目录结构不同于其它 agent。
        if a == Agent::Dsh {
            continue;
        }
        let src = source(a);
        if !src.deletable() {
            continue;
        }
        let sessions = match src.scan() {
            Ok(found) => found,
            Err(_) => continue,
        };
        for s in sessions {
            if s.messages == 0 || s.messages > max_msgs {
                continue;
            }
            if s.modified <= 0.0 {
                continue;
            }
            if now - s.modified < max_age_secs {
                continue;
            }
            match delete_session(a, &s, true).await {
                Ok(_) => {
                    purged += 1;
                    items.push(json!({
                        "agent": a.id(),
                        "path": s.path,
                        "title": s.title,
                        "messages": s.messages,
                    }));
                }
                Err(_) => failed += 1,
            }
        }
    }
    json!({"ok": true, "purged": purged, "failed": failed, "items": items})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codex_filename_gives_uuid() {
        let p = Path::new(
            "C:\\x\\rollout-2026-11-14T22-45-24-01a1da85-2fa8-70f2-9948-09b14b9b897f.jsonl",
        );
        assert_eq!(
            codex_id_from_file(p).as_deref(),
            Some("01a1da85-2fa8-70f2-9948-09b14b9b897f")
        );
    }

    #[test]
    fn meta_text_is_filtered() {
        assert!(is_meta_text("<environment_context>\n<cwd>D:\\x</cwd>"));
        assert!(is_meta_text("Caveat: the messages below were generated"));
        assert!(!is_meta_text("只回复 OK"));
    }

    #[test]
    fn when_text_buckets() {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs_f64();
        assert_eq!(when_text(now - 5.0), "刚刚");
        assert_eq!(when_text(now - 120.0), "2 分钟前");
    }

    #[test]
    fn dsh_session_title_and_model() {
        let rows = json!({
            "title": { "val": "  优化项目  " },
            "modelSelection": { "val": { "lastUsed": { "provider": "workbuddy", "model": "hy4free" } } },
            "sessionStats": { "val": { "turns": 7 } },
        });
        assert_eq!(dsh_title(&rows), "优化项目");
        assert_eq!(dsh_model(&rows), "workbuddy/hy4free");
        assert_eq!(
            rows.pointer("/sessionStats/val/turns")
                .and_then(Value::as_u64),
            Some(7)
        );
    }

    #[test]
    fn dsh_title_falls_back_to_first_prompt() {
        let rows = json!({
            "title": { "val": null },
            "turnOutline": { "val": { "turns": [
                { "prompt": "" },
                { "prompt": "第一句话" }
            ] } },
        });
        assert_eq!(dsh_title(&rows), "第一句话");
        assert_eq!(dsh_model(&rows), "");
    }

    #[test]
    fn dsh_agent_is_registered() {
        assert_eq!(Agent::parse("dsh"), Some(Agent::Dsh));
        assert_eq!(Agent::Dsh.label(), "DSH");
        assert!(source(Agent::Dsh).deletable());
        assert!(source(Agent::OpenCode).deletable());
    }

    #[test]
    fn dsh_cache_file_accepts_json() {
        let dir = std::env::temp_dir().join(format!("am-dsh-cache-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let file = dir.join("session-x.json");
        std::fs::write(&file, "{}").unwrap();
        assert_eq!(
            valid_dsh_cache_file(&dir, &file.to_string_lossy()).unwrap(),
            paths::dunce_resolve(&file)
        );
        // 通用校验只认 .jsonl，DSH 索引必须走专用校验。
        assert!(valid_session_file(&dir, &file.to_string_lossy()).is_err());
        assert!(valid_dsh_cache_file(&dir, "C:\\outside.json").is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn dsh_delete_removes_index_and_data() {
        let root = std::env::temp_dir().join(format!("am-dsh-del-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let cache_dir = root.join("storages/session_projcache/sessions");
        let data_dir = root.join("sessions/--D-code-test--/session-test");
        std::fs::create_dir_all(&cache_dir).unwrap();
        std::fs::create_dir_all(&data_dir).unwrap();
        let cache = cache_dir.join("session-test.json");
        std::fs::write(&cache, "{\"record\":{\"rows\":{}}}").unwrap();
        std::fs::write(data_dir.join("session.v4.jsonl.zstd"), "x").unwrap();
        let workspace = root.join("storages/workspace.json");
        std::fs::write(
            &workspace,
            "{\"global\":{\"archivedSessionIds\":[]},\"tables\":{\"workspaces\":{\"w\":{\"sessionIds\":[\"session-test\"]}}}}",
        )
        .unwrap();

        let previous = std::env::var("DSH_HOME").ok();
        std::env::set_var("DSH_HOME", &root);
        let r = delete("dsh", &cache.to_string_lossy(), "session-test", false)
            .await
            .unwrap();
        match previous {
            Some(v) => std::env::set_var("DSH_HOME", v),
            None => std::env::remove_var("DSH_HOME"),
        }

        assert_eq!(r["method"], json!("dsh"));
        assert!(!cache.exists(), "projcache index should be gone");
        assert!(!data_dir.exists(), "real session data should be gone");
        let doc: Value = serde_json::from_str(&std::fs::read_to_string(&workspace).unwrap()).unwrap();
        assert_eq!(doc["global"]["archivedSessionIds"], json!(["session-test"]));
        assert_eq!(doc["tables"]["workspaces"]["w"]["sessionIds"], json!([]));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn dsh_archive_updates_workspace() {
        let mut doc = json!({
            "global": { "archivedSessionIds": ["old-one"] },
            "tables": { "workspaces": {
                "w1": { "sessionIds": ["session-a", "session-b"] },
                "w2": { "sessionIds": ["session-a"] }
            }}
        });
        assert!(dsh_archive_in_workspace(&mut doc, "session-a"));
        assert_eq!(
            doc["tables"]["workspaces"]["w1"]["sessionIds"],
            json!(["session-b"])
        );
        assert_eq!(
            doc["tables"]["workspaces"]["w2"]["sessionIds"],
            json!([])
        );
        assert_eq!(
            doc["global"]["archivedSessionIds"],
            json!(["old-one", "session-a"])
        );
        // 重复归档不该再追加。
        assert!(!dsh_archive_in_workspace(&mut doc, "session-a"));
    }
}