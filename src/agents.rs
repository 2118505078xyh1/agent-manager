//! Agent 接入：检测本机的 agent，读写它们的模型配置。
//!
//! # 为什么这一页最重要
//!
//! magpie 自己的定位是「one place to pick every agent's model」——
//! 网关只是中间层，真正的价值是**把模型塞进 agent 已经在用的配置里**。
//! agent 不用改 baseURL、不用装插件，用自己熟悉的那份配置就能用上
//! mymagpie 的模型。
//!
//! # 检测方式
//!
//! 每个 agent 一个 `Agent` 描述：配置文件在哪、模型写在文件的什么形状里。
//! 检测 = 配置文件在不在；读 = 解析出当前模型；写 = 改回去。
//!
//! 清单目前只保留四个 agent（其余渠道账号见「渠道」页）：
//!
//! ```text
//!   Claude Code        ~\.claude\settings.json
//!   Codex              ~\.codex\config.toml
//!   OpenCode           ~\.config\opencode\opencode.json
//!   Pi                 ~\.pi\agent\settings.json
//! ```

use serde_json::{json, Value};

use crate::paths;

/// 一个 agent 的描述。
pub struct Agent {
    /// 稳定 id（前端用）。
    pub id: &'static str,
    /// 显示名。
    pub name: &'static str,
    /// 配置文件相对主目录的路径。
    pub file: &'static str,
    /// 配置里模型字段的形状。
    pub shape: Shape,
}

/// 模型字段的形状 —— 各家 agent 的配置格式不一样。
#[derive(Clone, Copy, PartialEq)]
pub enum Shape {
    /// `{"model": "xxx"}`（顶层字符串）
    TopLevelString,
    /// `{"provider": {"models": {...}}}` 之类，模型是嵌套的
    Nested {
        /// 取模型值的路径（点分隔）
        path: &'static str,
    },
    /// TOML：`model = "xxx"`
    Toml,
    /// YAML：`model: xxx`
    Yaml,
    /// YAML 一层嵌套：`parent:` 块里的 `child:`（点分隔，如 `modelRoles.default`）
    YamlNested {
        path: &'static str,
    },
}

/// 全部已知的 agent。
pub const AGENTS: &[Agent] = &[
    Agent {
        id: "pi",
        name: "Pi",
        file: ".pi\\agent\\settings.json",
        shape: Shape::Nested { path: "models.default" },
    },
    Agent {
        id: "opencode",
        name: "OpenCode",
        file: ".config\\opencode\\opencode.json",
        shape: Shape::TopLevelString,
    },
    Agent {
        id: "claude-code",
        name: "Claude Code",
        file: ".claude\\settings.json",
        shape: Shape::TopLevelString,
    },
    Agent {
        id: "codex",
        name: "Codex",
        file: ".codex\\config.toml",
        shape: Shape::Toml,
    },
    Agent {
        id: "omp",
        name: "omp",
        file: ".omp\\agent\\config.yml",
        shape: Shape::YamlNested { path: "modelRoles.default" },
    },
];

/// 一个 agent 的当前状态。
#[derive(serde::Serialize, Clone)]
pub struct AgentStatus {
    pub id: String,
    pub name: String,
    /// 配置文件是否存在
    pub installed: bool,
    /// 配置文件绝对路径
    pub path: String,
    /// 当前模型（读不出来就是空串）
    pub model: String,
    /// 当前 effort / reasoning 档位
    pub effort: String,
    /// 读的时候出的问题（文件坏了、JSON 解析失败等）
    pub error: String,
}

/// 检测全部 agent。
pub fn detect_all() -> Vec<AgentStatus> {
    AGENTS.iter().map(|a| detect(a)).collect()
}

fn detect(a: &Agent) -> AgentStatus {
    let path = paths::home().join(a.file);
    let mut st = AgentStatus {
        id: a.id.to_string(),
        name: a.name.to_string(),
        installed: path.is_file(),
        path: path.to_string_lossy().to_string(),
        model: String::new(),
        effort: String::new(),
        error: String::new(),
    };
    if !st.installed {
        return st;
    }
    let Ok(text) = std::fs::read_to_string(&path) else {
        st.error = "读不了文件".to_string();
        return st;
    };
    match a.shape {
        Shape::TopLevelString => {
            match serde_json::from_str::<Value>(&text) {
                Ok(v) => {
                    st.model = v
                        .get("model")
                        .and_then(|x| x.as_str())
                        .unwrap_or_default()
                        .to_string();
                    st.effort = v
                        .get("effort")
                        .and_then(|x| x.as_str())
                        .unwrap_or_default()
                        .to_string();
                }
                Err(e) => st.error = format!("JSON 解析失败：{e}"),
            }
        }
        Shape::Nested { path: p } => {
            match serde_json::from_str::<Value>(&text) {
                Ok(v) => {
                    st.model = path_str(&v, p);
                    st.effort = path_str(&v, "models.effort");
                }
                Err(e) => st.error = format!("JSON 解析失败：{e}"),
            }
        }
        Shape::Toml => {
            let (m, e) = toml_get(&text, "model");
            st.model = m;
            st.effort = toml_get(&text, "model_reasoning_effort").0;
            if let Some(err) = e {
                st.error = err;
            }
        }
        Shape::Yaml => {
            let (m, _) = yaml_get(&text, "model");
            st.model = m;
        }
        Shape::YamlNested { path: p } => {
            let (parent, child) = p.split_once('.').unwrap_or((p, ""));
            st.model = yaml_child_get(&text, parent, child);
        }
    }
    st
}

/// 读 YAML 里 `parent:` 块下一层的 `child:` 标量（只处理一层缩进）。
/// omp 的 `modelRoles:\n  default: xxx` 就是这种形状。
fn yaml_child_get(text: &str, parent: &str, child: &str) -> String {
    let mut in_block = false;
    for line in text.lines() {
        let t = line.trim_end();
        if t.is_empty() || t.starts_with('#') || t.starts_with('-') {
            continue;
        }
        let indented = t.starts_with(' ') || t.starts_with('\t');
        if !indented {
            in_block = t.len() == parent.len() + 1
                && t.starts_with(parent)
                && t.as_bytes()[parent.len()] == b':';
            continue;
        }
        if !in_block {
            continue;
        }
        if let Some(rest) = t.trim_start().strip_prefix(child) {
            if let Some(v) = rest.trim_start().strip_prefix(':') {
                return v.trim().trim_matches('"').trim_matches('\'').to_string();
            }
        }
    }
    String::new()
}

/// 取嵌套路径的字符串值。
///
/// 不用 `and_then(|x| x.as_str())` —— 那会返回一个借用闭包参数的 `&str`，
/// Rust 直接拒绝（E0515）。先取出 `Value` 再转字符串。
fn path_str(v: &Value, path: &str) -> String {
    let mut cur = v;
    for seg in path.split('.') {
        match cur.get(seg) {
            Some(x) => cur = x,
            None => return String::new(),
        }
    }
    cur.as_str().unwrap_or_default().to_string()
}

/// 取嵌套路径的值。`"models.default"` → `v["models"]["default"]`。


/// 从 TOML 文本里取一个顶层键的字符串值。
///
/// **不写完整的 TOML 解析器** —— 只认 `key = "value"` 这一种形状。
/// Codex 的 `config.toml` 里模型就是这么写的；别的形状读不到就返回空，
/// 不会猜。
fn toml_get(text: &str, key: &str) -> (String, Option<String>) {
    for line in text.lines() {
        let t = line.trim();
        if t.starts_with('#') {
            continue;
        }
        if let Some(rest) = t.strip_prefix(key) {
            let rest = rest.trim_start();
            if let Some(v) = rest.strip_prefix('=') {
                let v = v.trim();
                let v = v.trim_matches('"').trim_matches('\'');
                return (v.to_string(), None);
            }
        }
    }
    (String::new(), None)
}

/// 从 YAML 文本里取一个顶层键。
fn yaml_get(text: &str, key: &str) -> (String, Option<String>) {
    for line in text.lines() {
        let t = line.trim_end();
        if t.starts_with('#') || t.starts_with('-') {
            continue;
        }
        if let Some(rest) = t.strip_prefix(key) {
            let rest = rest.trim_start();
            if let Some(v) = rest.strip_prefix(':') {
                return (v.trim().trim_matches('"').trim_matches('\'').to_string(), None);
            }
        }
    }
    (String::new(), None)
}

/// 把模型写进 agent 的配置。
///
/// 只改模型那一处，其余原样保留 —— 这些文件里还有用户的其它设置。
/// 写之前先备份一份（`.bak-<时间戳>`）。
pub fn set_model(id: &str, model: &str, effort: Option<&str>) -> Result<String, String> {
    let a = AGENTS
        .iter()
        .find(|x| x.id == id)
        .ok_or_else(|| format!("没有这个 agent：{id}"))?;
    let path = paths::home().join(a.file);
    if !path.is_file() {
        return Err(format!("{} 没有安装（找不到 {}）", a.name, path.display()));
    }
    let text = std::fs::read_to_string(&path).map_err(|e| format!("读文件失败：{e}"))?;

    let new_text = match a.shape {
        Shape::TopLevelString => set_json_top(&text, model, effort)?,
        Shape::Nested { path: p } => set_json_nested(&text, p, model, effort)?,
        Shape::Toml => set_toml(&text, model, effort),
        Shape::Yaml => set_yaml(&text, model, effort),
        Shape::YamlNested { path: p } => {
            let (parent, child) = p.split_once('.').unwrap_or((p, ""));
            set_yaml_child(&text, parent, child, model)
        }
    };

    // 备份
    let stamp = now_stamp();
    let bak = path.with_extension(format!(
        "{}bak-mymagpie-{}",
        path.extension().map(|_| ".").unwrap_or(""),
        stamp
    ));
    let _ = std::fs::copy(&path, &bak);

    std::fs::write(&path, new_text).map_err(|e| format!("写文件失败：{e}"))?;
    Ok(bak.to_string_lossy().to_string())
}

/// 改 JSON 里的顶层字符串字段。
fn set_json_top(text: &str, model: &str, effort: Option<&str>) -> Result<String, String> {
    let mut v: Value =
        serde_json::from_str(text).map_err(|e| format!("JSON 解析失败：{e}"))?;
    if !v.is_object() {
        v = json!({});
    }
    v["model"] = json!(model);
    if let Some(e) = effort {
        if !e.is_empty() {
            v["effort"] = json!(e);
        }
    }
    serde_json::to_string_pretty(&v).map_err(|e| format!("序列化失败：{e}"))
}

/// 改 JSON 里的嵌套字段。
fn set_json_nested(text: &str, path: &str, model: &str, effort: Option<&str>) -> Result<String, String> {
    let mut v: Value =
        serde_json::from_str(text).map_err(|e| format!("JSON 解析失败：{e}"))?;
    if !v.is_object() {
        v = json!({});
    }
    let segs: Vec<&str> = path.split('.').collect();
    {
        let mut cur = &mut v;
        for seg in &segs[..segs.len() - 1] {
            if !cur.get(*seg).map(|x| x.is_object()).unwrap_or(false) {
                cur[*seg] = json!({});
            }
            cur = cur.get_mut(*seg).unwrap();
        }
        cur[segs[segs.len() - 1]] = json!(model);
    }
    if let Some(e) = effort {
        if !e.is_empty() {
            let models = v
                .get_mut("models")
                .and_then(|m| m.as_object_mut());
            if let Some(m) = models {
                m.insert("effort".into(), json!(e));
            }
        }
    }
    serde_json::to_string_pretty(&v).map_err(|e| format!("序列化失败：{e}"))
}

/// 改 TOML 里的顶层键。保留原有格式（只替换那一行）。
fn set_toml(text: &str, model: &str, effort: Option<&str>) -> String {
    let mut out = String::new();
    let mut seen_model = false;
    let mut seen_effort = false;
    for line in text.lines() {
        let t = line.trim_start();
        let indent = &line[..line.len() - t.len()];
        if t.starts_with("model") && !t.starts_with("model_") {
            out.push_str(&format!("{indent}model = \"{model}\"\n"));
            seen_model = true;
            continue;
        }
        if let Some(e) = effort {
            if !e.is_empty() && t.starts_with("model_reasoning_effort") {
                out.push_str(&format!("{indent}model_reasoning_effort = \"{e}\"\n"));
                seen_effort = true;
                continue;
            }
        }
        out.push_str(line);
        out.push('\n');
    }
    if !seen_model {
        out.push_str(&format!("model = \"{model}\"\n"));
    }
    if let Some(e) = effort {
        if !e.is_empty() && !seen_effort {
            out.push_str(&format!("model_reasoning_effort = \"{e}\"\n"));
        }
    }
    out
}

/// 改 YAML 里的顶层键。
fn set_yaml(text: &str, model: &str, _effort: Option<&str>) -> String {
    let mut out = String::new();
    let mut seen = false;
    for line in text.lines() {
        let t = line.trim_start();
        let indent = &line[..line.len() - t.len()];
        if t.starts_with("model:") {
            out.push_str(&format!("{indent}model: {model}\n"));
            seen = true;
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    if !seen {
        out.push_str(&format!("model: {model}\n"));
    }
    out
}

/// 写 YAML 里 `parent:` 块下一层的 `child:`；块或键不存在时补出来。
fn set_yaml_child(text: &str, parent: &str, child: &str, model: &str) -> String {
    let mut lines: Vec<String> = text.lines().map(str::to_string).collect();

    let parent_at = lines
        .iter()
        .position(|l| !is_indented(l) && is_yaml_key(l, parent));

    let Some(pi) = parent_at else {
        lines.push(format!("{parent}:"));
        lines.push(format!("  {child}: {model}"));
        return join_yaml(lines);
    };

    // 父块到下一个顶层行为止。
    let end = lines[pi + 1..]
        .iter()
        .position(|l| !l.trim().is_empty() && !is_indented(l))
        .map(|i| pi + 1 + i)
        .unwrap_or(lines.len());

    let child_at = lines[pi + 1..end]
        .iter()
        .position(|l| is_yaml_key(l.trim_start(), child))
        .map(|i| pi + 1 + i);

    match child_at {
        Some(ci) => {
            let lead = lines[ci].len() - lines[ci].trim_start().len();
            let indent = lines[ci][..lead].to_string();
            lines[ci] = format!("{indent}{child}: {model}");
        }
        None => lines.insert(pi + 1, format!("  {child}: {model}")),
    }

    join_yaml(lines)
}

fn is_indented(line: &str) -> bool {
    line.starts_with(' ') || line.starts_with('\t')
}

/// `key:` 开头的行（允许行尾空白）。
fn is_yaml_key(line: &str, key: &str) -> bool {
    let t = line.trim_end();
    t.len() == key.len() + 1 && t.starts_with(key) && t.as_bytes()[key.len()] == b':'
}

fn join_yaml(lines: Vec<String>) -> String {
    let mut out = lines.join("\n");
    out.push('\n');
    out
}

/// 当前时间戳（用于备份文件名）。
pub fn now_stamp() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // YYYYMMDD-HHMMSS
    let days = secs / 86400;
    let rem = secs % 86400;
    let (y, mo, d) = civil_from_days(days as i64);
    format!(
        "{:04}{:02}{:02}-{:02}{:02}{:02}",
        y,
        mo,
        d,
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

// ============================================================ 更新 / 一键接入网关
//
// 四个 agent（Pi / Claude Code / Codex / OpenCode）各有自己的自更新子命令与
// 配置形状。这一节把它们统一成 API 层要的三件事：
//   * `version_info` —— 当前版本、最新版本、可不可更新；
//   * `update_agent` —— 跑那个 agent 自己的更新子命令；
//   * `onboard_agent` —— 把本地网关的 baseURL + key 写进它自己的配置。
//
// 只碰 agent 自己的配置文件，写前一律备份（`atomic`）。

use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

use crate::proc::{run_process, CmdSpec, ProcResult};

/// 一个可更新 / 可接入的 agent 的元数据。
pub struct AgentOp {
    /// 对外 id（API 用）。
    pub api_id: &'static str,
    /// `AGENTS` 里的 id（`AGENTS` 用 `claude-code`）。
    pub agent_id: &'static str,
    /// npm 包名（查最新版本）。
    pub pkg: &'static str,
    /// 自更新子命令。
    pub update: &'static [&'static str],
}

pub const AGENT_OPS: &[AgentOp] = &[
    AgentOp {
        api_id: "pi",
        agent_id: "pi",
        pkg: "@earendil-works/pi-coding-agent",
        update: &["update"],
    },
    AgentOp {
        api_id: "claude",
        agent_id: "claude-code",
        pkg: "@anthropic-ai/claude-code",
        update: &["update"],
    },
    AgentOp {
        api_id: "codex",
        agent_id: "codex",
        pkg: "@openai/codex",
        update: &["update"],
    },
    AgentOp {
        api_id: "opencode",
        agent_id: "opencode",
        pkg: "@opencode/cli",
        update: &["upgrade"],
    },
    AgentOp {
        api_id: "omp",
        agent_id: "omp",
        pkg: "@oh-my-pi/pi-coding-agent",
        update: &["update"],
    },
];

/// 兼容 `claude` / `claude-code` 两种写法。
fn find_op(id: &str) -> Option<&'static AgentOp> {
    AGENT_OPS.iter().find(|o| o.api_id == id || o.agent_id == id)
}

fn npm_global_bin(name: &str) -> Option<PathBuf> {
    let p = paths::home()
        .join("AppData")
        .join("Roaming")
        .join("npm")
        .join(format!("{name}.cmd"));
    if p.is_file() {
        Some(p)
    } else {
        None
    }
}

/// 在 PATH 上按 PATHEXT 找一个可执行文件（比交给 `cmd` 解析裸名可靠）。
fn find_on_path(name: &str) -> Option<String> {
    let path = std::env::var_os("PATH")?;
    let exts: Vec<String> = std::env::var("PATHEXT")
        .unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".to_string())
        .split(';')
        .map(|s| s.to_ascii_lowercase())
        .collect();
    for dir in std::env::split_paths(&path) {
        if dir.as_os_str().is_empty() {
            continue;
        }
        for ext in std::iter::once(String::new()).chain(exts.iter().cloned()) {
            let cand = dir.join(format!("{name}{ext}"));
            if cand.is_file() {
                return Some(cand.to_string_lossy().to_string());
            }
        }
    }
    None
}

/// 这个 agent 的可执行文件。先看标准安装位置，再退回 PATH。
fn agent_bin(op: &AgentOp) -> Option<String> {
    let resolved = |name: &str| -> String {
        npm_global_bin(name)
            .map(|p| p.to_string_lossy().to_string())
            .or_else(|| find_on_path(name))
            .unwrap_or_else(|| name.to_string())
    };
    match op.api_id {
        "pi" => crate::pi::find_pi_binary().map(|p| p.to_string_lossy().to_string()),
        "claude" => {
            let native = paths::home().join(".local").join("bin").join("claude.exe");
            if native.is_file() {
                Some(native.to_string_lossy().to_string())
            } else {
                Some(resolved("claude"))
            }
        }
        "codex" => Some(resolved("codex")),
        "opencode" => Some(resolved("opencode")),
        "omp" => {
            let bun = paths::home().join(".bun").join("bin").join("omp.exe");
            if bun.is_file() {
                Some(bun.to_string_lossy().to_string())
            } else {
                Some(resolved("omp"))
            }
        }
        _ => None,
    }
}

/// 跑一条命令。`.cmd` / `.bat` / `.ps1` 以及 PATH 上的裸名要走 `cmd /C`，
/// 否则 Windows 上 `.cmd` 直接 spawn 会 EINVAL。
async fn run_bin(bin: &str, args: &[String], timeout_ms: u64) -> ProcResult {
    let lower = bin.to_lowercase();
    let needs_shell = cfg!(windows)
        && (lower.ends_with(".cmd")
            || lower.ends_with(".bat")
            || lower.ends_with(".ps1")
            || (!lower.contains('/') && !lower.contains('\\')));
    match CmdSpec::owned(bin, args)
        .timeout(timeout_ms)
        .shell(needs_shell)
        .run()
        .await
    {
        Ok(r) => r,
        Err(e) => ProcResult::failed_with(e),
    }
}

/// 从任意版本输出里抠出第一个版本号（`\d+(\.\d+)+`）。
fn first_version(text: &str) -> Option<String> {
    for tok in text.split(|c: char| !(c.is_ascii_digit() || c == '.')) {
        let t = tok.trim_matches('.');
        if t.split('.').count() >= 2
            && t.chars().next().is_some_and(|c| c.is_ascii_digit())
            && t.chars().all(|c| c.is_ascii_digit() || c == '.')
        {
            return Some(t.to_string());
        }
    }
    None
}

async fn bin_version(bin: &str) -> Option<String> {
    let args = vec!["--version".to_string()];
    let r = run_bin(bin, &args, 20_000).await;
    let text = if r.stdout.trim().is_empty() {
        r.stderr.clone()
    } else {
        r.stdout.clone()
    };
    first_version(&text)
}

/// npm registry 最新版本缓存（1 小时）。
static LATEST_CACHE: Mutex<Vec<(String, u64, Option<String>)>> = Mutex::new(Vec::new());
const LATEST_TTL_MS: u64 = 60 * 60 * 1000;

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// 一个 npm 包的最新版本。registry → curl → 本机 npm，任一成功即用。
async fn npm_latest(pkg: &str) -> Option<String> {
    {
        let g = LATEST_CACHE.lock().unwrap();
        for (p, at, v) in g.iter() {
            if p == pkg && now_ms().saturating_sub(*at) < LATEST_TTL_MS {
                return v.clone();
            }
        }
    }
    let url = format!(
        "https://registry.npmjs.org/{}/latest",
        pkg.replace('/', "%2f")
    );
    let mut version: Option<String> = None;

    if let Ok(client) = reqwest::Client::builder().timeout(Duration::from_secs(8)).build() {
        if let Ok(res) = client
            .get(&url)
            .header("accept", "application/json")
            .send()
            .await
        {
            if res.status().is_success() {
                if let Ok(body) = res.json::<Value>().await {
                    version = body
                        .get("version")
                        .and_then(|v| v.as_str())
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty());
                }
            }
        }
    }

    // 兜底：curl（有些机器 Rust 的 TLS 走不通但 curl 能走）
    if version.is_none() {
        let args = vec![
            "-sS".to_string(),
            "--max-time".to_string(),
            "20".to_string(),
            url.clone(),
        ];
        let r = run_process("curl.exe", &args, None, 25_000, None).await;
        if !r.failed {
            if let Ok(body) = serde_json::from_str::<Value>(&r.stdout) {
                version = body
                    .get("version")
                    .and_then(|v| v.as_str())
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty());
            }
        }
    }

    let mut g = LATEST_CACHE.lock().unwrap();
    g.retain(|(p, _, _)| p != pkg);
    g.push((pkg.to_string(), now_ms(), version.clone()));
    version
}

/// 一个 agent 的版本三态。
///
/// 空串 / 只有空白一律当成「未知」，避免把 `""` 塞进 `latestVersion`。
fn known_version(v: Option<String>) -> Option<String> {
    v.map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

/// 面板要的是 `string | null`：拿不到的版本一律 `null`（不是 `""`），
/// `updatable` 只有在两边都有值时才可能为真。
async fn version_info(op: &AgentOp) -> Value {
    let current = known_version(match op.api_id {
        "pi" => crate::pi::version().await,
        _ => match agent_bin(op) {
            Some(b) => bin_version(&b).await,
            None => None,
        },
    });
    let latest = known_version(match op.api_id {
        "pi" => crate::pi::latest_version().await,
        _ => npm_latest(op.pkg).await,
    });
    let updatable = match (&current, &latest) {
        (Some(c), Some(l)) => c != l,
        _ => false,
    };
    json!({
        "currentVersion": current,
        "latestVersion": latest,
        "updatable": updatable,
    })
}

// ------------------------------------------------------------------ DSH（DeepSeek Harness）

/// DSH 的 nightly 更新源。和 `resources/app-update.yml` 里的 feed 同源。
const DSH_FEED_URL: &str = "https://download.deepseek.com/dsh-desk/feeds/win-x64/nightly.yml";

#[derive(Clone, Default)]
struct DshFeed {
    version: Option<String>,
    url: Option<String>,
}

static DSH_FEED: Mutex<Option<(u64, DshFeed)>> = Mutex::new(None);

/// 从 `dsh://` 协议注册项里找安装目录。便携版没注册协议时退回常见路径。
#[cfg(windows)]
fn dsh_exe_from_registry() -> Option<PathBuf> {
    use windows::core::{w, PCWSTR};
    use windows::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_SZ};

    let mut buf = [0u16; 1024];
    let mut size = std::mem::size_of_val(&buf) as u32;
    let r = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!("Software\\Classes\\dsh\\shell\\open\\command"),
            PCWSTR::null(),
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr() as *mut core::ffi::c_void),
            Some(&mut size),
        )
    };
    if r.0 != 0 {
        return None;
    }
    let n = (size as usize / 2).saturating_sub(1).min(buf.len());
    let s = String::from_utf16_lossy(&buf[..n]);
    // `"D:\deepseek_harness\DeepSeek Harness.exe" "%1"`
    let rest = s.trim().strip_prefix('"')?;
    let end = rest.find('"')?;
    let exe = PathBuf::from(&rest[..end]);
    exe.is_file().then_some(exe)
}

#[cfg(not(windows))]
fn dsh_exe_from_registry() -> Option<PathBuf> {
    None
}

fn dsh_exe() -> Option<PathBuf> {
    if let Ok(v) = std::env::var("DSH_DESKTOP_EXE") {
        let p = PathBuf::from(v.trim());
        if p.is_file() {
            return Some(p);
        }
    }
    if let Some(p) = dsh_exe_from_registry() {
        return Some(p);
    }
    let candidates = [
        PathBuf::from(r"D:\deepseek_harness\DeepSeek Harness.exe"),
        crate::paths::home()
            .join("AppData")
            .join("Local")
            .join("Programs")
            .join("DeepSeek Harness")
            .join("DeepSeek Harness.exe"),
        PathBuf::from(r"C:\Program Files\DeepSeek Harness\DeepSeek Harness.exe"),
    ];
    candidates.into_iter().find(|p| p.is_file())
}

/// DSH 的 `package.json` 在 app.asar 里；直接跑它自己的 CLI 取版本
/// （`ELECTRON_RUN_AS_NODE`，约 40ms，不会弹窗）。
fn dsh_cli(exe: &std::path::Path) -> Option<PathBuf> {
    let root = exe.parent()?;
    Some(
        root.join("resources")
            .join("app.asar")
            .join("dsh")
            .join("node_modules")
            .join("@deepseek-ai")
            .join("dsh-desktop-host")
            .join("lib")
            .join("cli.js"),
    )
}

/// 版本号要保留 `-rc.3` 这种预发布后缀 —— DSH 的版本就是
/// `0.2.2-rc.3`，只取数字部分会误判成「可更新」。
fn first_full_version(text: &str) -> Option<String> {
    for tok in text.split_whitespace() {
        let t = tok
            .trim()
            .trim_start_matches('v')
            .trim_matches(|c: char| c == ',' || c == ';' || c == '(' || c == ')');
        let core = t.split(['-', '+']).next().unwrap_or("");
        if core.split('.').count() >= 2
            && core.chars().next().is_some_and(|c| c.is_ascii_digit())
            && core.chars().all(|c| c.is_ascii_digit() || c == '.')
        {
            return Some(t.to_string());
        }
    }
    None
}

async fn dsh_version(exe: &std::path::Path) -> Option<String> {
    let cli = dsh_cli(exe)?;
    let args = vec![
        "--expose-internals".to_string(),
        cli.to_string_lossy().to_string(),
        "--version".to_string(),
    ];
    let r = CmdSpec::owned(&exe.to_string_lossy(), &args)
        .env("ELECTRON_RUN_AS_NODE", "1")
        .timeout(20_000)
        .run()
        .await
        .ok()?;
    if r.failed {
        return None;
    }
    let text = if r.stdout.trim().is_empty() {
        r.stderr
    } else {
        r.stdout
    };
    first_full_version(&text)
}

/// 从 `key: value` / `key: >-` + 下一行的 YAML 片段里取标量。
fn yaml_scalar(doc: &str, key: &str) -> Option<String> {
    let mut lines = doc.lines();
    while let Some(line) = lines.next() {
        let t = line.trim_start();
        let Some(rest) = t.strip_prefix(key) else {
            continue;
        };
        let Some(rest) = rest.strip_prefix(':') else {
            continue;
        };
        let v = rest.trim();
        if v.is_empty() || matches!(v, ">" | ">-" | "|" | "|-") {
            return lines
                .by_ref()
                .map(str::trim)
                .find(|x| !x.is_empty())
                .map(|x| x.trim_matches('"').trim_matches('\'').to_string());
        }
        return Some(v.trim_matches('"').trim_matches('\'').to_string());
    }
    None
}

/// nightly.yml 的版本与安装包地址。只在拿到版本时缓存 1 小时，断网不锁死。
async fn dsh_feed_cached() -> DshFeed {
    {
        let g = DSH_FEED.lock().unwrap();
        if let Some((at, f)) = g.as_ref() {
            if now_ms().saturating_sub(*at) < LATEST_TTL_MS {
                return f.clone();
            }
        }
    }

    let mut doc: Option<String> = None;
    if let Ok(client) = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
    {
        if let Ok(res) = client.get(DSH_FEED_URL).send().await {
            if res.status().is_success() {
                if let Ok(text) = res.text().await {
                    doc = Some(text);
                }
            }
        }
    }
    if doc.is_none() {
        let args = vec![
            "-sS".to_string(),
            "--max-time".to_string(),
            "20".to_string(),
            DSH_FEED_URL.to_string(),
        ];
        let r = run_process("curl.exe", &args, None, 25_000, None).await;
        if !r.failed {
            doc = Some(r.stdout);
        }
    }

    let feed = match doc {
        Some(text) => DshFeed {
            version: known_version(yaml_scalar(&text, "version")),
            url: yaml_scalar(&text, "path"),
        },
        None => DshFeed::default(),
    };
    if feed.version.is_some() {
        *DSH_FEED.lock().unwrap() = Some((now_ms(), feed.clone()));
    }
    feed
}

async fn dsh_info() -> Value {
    let exe = dsh_exe();
    let installed = exe.as_ref().is_some_and(|p| p.is_file());
    let current = match (installed, exe.as_deref()) {
        (true, Some(p)) => known_version(dsh_version(p).await),
        _ => None,
    };
    let feed = dsh_feed_cached().await;
    let latest = known_version(feed.version);
    let updatable = match (&current, &latest) {
        (Some(c), Some(l)) => c != l,
        _ => false,
    };
    json!({
        "id": "dsh",
        "name": "DSH",
        "installed": installed,
        "path": exe.map(|p| p.to_string_lossy().to_string()).unwrap_or_default(),
        "model": "",
        "effort": "",
        "error": "",
        "currentVersion": current,
        "latestVersion": latest,
        "updatable": updatable,
    })
}

/// DSH 是 300 MB 的 NSIS 安装包，不走 npm 自更新：打开官方下载页，
/// 下完由用户运行安装程序（装完需要重启 DSH）。
async fn update_dsh() -> Value {
    let Some(exe) = dsh_exe() else {
        return json!({
            "ok": false,
            "id": "dsh",
            "error": "未找到 DeepSeek Harness（dsh:// 协议未注册，也不在常见安装路径）",
        });
    };
    let current = known_version(dsh_version(&exe).await);
    let feed = dsh_feed_cached().await;
    let Some(latest) = known_version(feed.version) else {
        return json!({
            "ok": false,
            "id": "dsh",
            "error": "读取不到 DSH 最新版本（nightly feed 不通）",
        });
    };
    if current.as_deref() == Some(latest.as_str()) {
        return json!({
            "ok": true,
            "id": "dsh",
            "version": latest,
            "message": format!("DSH 已是最新 {latest}"),
        });
    }
    let Some(url) = feed.url else {
        return json!({
            "ok": false,
            "id": "dsh",
            "error": format!("nightly feed 里没有 {latest} 的安装包地址"),
        });
    };
    match open::that_detached(&url) {
        Ok(()) => json!({
            "ok": true,
            "id": "dsh",
            "version": latest,
            "opened": url,
            "message": format!("DSH {latest} 需要安装包：已打开官方下载页（约 300 MB），装完请重启 DSH"),
        }),
        Err(e) => json!({
            "ok": false,
            "id": "dsh",
            "error": format!("打开下载页失败：{e}"),
        }),
    }
}

/// ZCode 桌面端版本：从卸载注册表项的 DisplayName（形如 `ZCode 3.18.2`）取。
/// 它的更新源是 localhost:8081 代理，拿不到公网最新版，所以 latest 恒为 null。
async fn zcode_info() -> Value {
    let exe = PathBuf::from(r"D:\zcode\ZCode.exe");
    let installed = exe.is_file();
    let current = if installed {
        let args = vec![
            "query".to_string(),
            r"HKCU\Software\Microsoft\Windows\CurrentVersion\Uninstall".to_string(),
            "/s".to_string(),
            "/f".to_string(),
            "ZCode".to_string(),
        ];
        CmdSpec::owned("reg.exe", &args)
            .timeout(10_000)
            .run()
            .await
            .ok()
            .and_then(|r| {
                let text = format!("{}{}", r.stdout, r.stderr);
                text.lines().find_map(|line| {
                    let rest = line.trim().strip_prefix("DisplayName")?.trim_start();
                    let name = rest.strip_prefix("REG_SZ")?.trim();
                    name.strip_prefix("ZCode ").map(|v| v.trim().to_string())
                })
            })
    } else {
        None
    };
    json!({
        "id": "zcode", "name": "ZCode", "installed": installed,
        "path": exe.to_string_lossy(), "model": "", "effort": "", "error": "",
        "currentVersion": current, "latestVersion": Value::Null, "updatable": false,
    })
}

/// Kimi CLI：版本取 `kimi --version`，最新版取 npm `@moonshot-ai/kimi-code`。
async fn kimicli_info() -> Value {
    let exe = std::env::var("KIMI_BIN")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            let p = crate::paths::kimi_home().join("bin").join("kimi.exe");
            p.is_file().then_some(p)
        })
        .or_else(|| npm_global_bin("kimi"))
        .unwrap_or_else(|| crate::paths::kimi_home().join("bin").join("kimi.exe"));
    let installed = exe.is_file();
    let current = if installed {
        let r = run_bin(&exe.to_string_lossy(), &["--version".to_string()], 10_000).await;
        first_full_version(&r.stdout)
    } else {
        None
    };
    let latest = npm_latest("@moonshot-ai/kimi-code").await;
    let updatable = match (&current, &latest) {
        (Some(c), Some(l)) => c != l,
        _ => false,
    };
    json!({
        "id": "kimicli", "name": "Kimi CLI", "installed": installed,
        "path": exe.to_string_lossy(), "model": "", "effort": "", "error": "",
        "currentVersion": current, "latestVersion": latest, "updatable": updatable,
    })
}

/// 检测全部 agent，并给可更新的补上版本信息。
///
/// 保留 `detect_all` 的所有键，额外加 `currentVersion` / `latestVersion` /
/// `updatable`（其它 agent 这三个是 null / null / false）。DSH 不在 npm 里，
/// 但同样要出现在更新页，所以最后追加一行。
pub async fn detect_all_extended() -> Vec<Value> {
    let statuses = detect_all();

    // 五个 op + DSH 并发取版本（网络 + 子进程），别串行拖到面板超时。
    let (a, b, c, d, e, dsh, zcode, kimi) = tokio::join!(
        version_info(&AGENT_OPS[0]),
        version_info(&AGENT_OPS[1]),
        version_info(&AGENT_OPS[2]),
        version_info(&AGENT_OPS[3]),
        version_info(&AGENT_OPS[4]),
        dsh_info(),
        zcode_info(),
        kimicli_info(),
    );
    let infos: [(&str, Value); 5] = [
        (AGENT_OPS[0].api_id, a),
        (AGENT_OPS[1].api_id, b),
        (AGENT_OPS[2].api_id, c),
        (AGENT_OPS[3].api_id, d),
        (AGENT_OPS[4].api_id, e),
    ];

    let mut rows: Vec<Value> = statuses
        .into_iter()
        .map(|st| {
            let mut v = serde_json::to_value(&st).unwrap_or_else(|_| json!({}));
            let info = AGENT_OPS
                .iter()
                .find(|o| o.agent_id == st.id)
                .and_then(|o| infos.iter().find(|(api, _)| *api == o.api_id))
                .map(|(_, i)| i);
            match info {
                Some(i) => {
                    v["currentVersion"] = i["currentVersion"].clone();
                    v["latestVersion"] = i["latestVersion"].clone();
                    v["updatable"] = i["updatable"].clone();
                }
                None => {
                    v["currentVersion"] = Value::Null;
                    v["latestVersion"] = Value::Null;
                    v["updatable"] = Value::Bool(false);
                }
            }
            v
        })
        .collect();
    rows.push(dsh);
    rows.push(zcode);
    rows.push(kimi);
    rows
}

/// 取末尾若干字符（命令输出可能很长）。
fn tail_text(s: &str, n: usize) -> String {
    let t = s.trim();
    let chars: Vec<char> = t.chars().collect();
    if chars.len() <= n {
        t.to_string()
    } else {
        chars[chars.len() - n..].iter().collect()
    }
}

/// 给「复制命令」按钮用：拼出用户可以直接粘到终端执行的更新命令。
///
/// 本机 `claude update` / `claude install` 在某些网络下会一直卡住且没有进度，
/// 所以更新页改成只给命令，由用户在自己的终端里跑。
pub fn update_command_text(id: &str) -> String {
    if id == "kimicli" {
        let exe = std::env::var("KIMI_BIN")
            .ok()
            .filter(|v| !v.trim().is_empty())
            .map(PathBuf::from)
            .or_else(|| {
                let p = crate::paths::kimi_home().join("bin").join("kimi.exe");
                p.is_file().then_some(p)
            })
            .or_else(|| npm_global_bin("kimi"))
            .unwrap_or_else(|| PathBuf::from("kimi"));
        return format!("\"{}\" update -y", exe.to_string_lossy());
    }
    if id == "zcode" {
        // ZCode 桌面端由 app 内 updater 更新，这里不给命令。
        return String::new();
    }
    let Some(op) = find_op(id) else {
        return String::new();
    };
    let bin = agent_bin(op).unwrap_or_else(|| op.api_id.to_string());
    let mut parts = vec![crate::launch::quote_cmd_arg(&bin)];
    parts.extend(op.update.iter().map(|s| s.to_string()));
    parts.join(" ")
}

/// 跑某个 agent 的自更新子命令。
pub async fn update_agent(id: &str) -> Value {
    if id == "dsh" {
        return update_dsh().await;
    }
    let Some(op) = find_op(id) else {
        return json!({ "ok": false, "id": id, "error": format!("没有这个 agent：{id}") });
    };
    let Some(bin) = agent_bin(op) else {
        return json!({ "ok": false, "id": id, "error": format!("未找到 {} 的可执行文件", op.api_id) });
    };
    let args: Vec<String> = op.update.iter().map(|s| s.to_string()).collect();
    // 面板给这条请求 600s，但 claude 自更新在部分网络下会卡在
    // "Updating to x.y.z..." 不动。3 分钟就放弃并明确报错，别让用户干等。
    let r = run_bin(&bin, &args, 180_000).await;
    let output = tail_text(&format!("{}{}", r.stdout, r.stderr), 4000);
    if r.timed_out {
        let msg = if output.is_empty() {
            format!("{} 更新超时（3 分钟），没有更多输出", op.api_id)
        } else {
            format!("{} 更新超时（3 分钟）：{}", op.api_id, output)
        };
        return json!({
            "ok": false,
            "id": id,
            "error": msg,
            "output": output,
            "timedOut": true,
        });
    }
    if r.failed {
        let msg = if output.is_empty() {
            format!("{} 更新命令失败（退出码 {:?}）", op.api_id, r.code)
        } else {
            output.clone()
        };
        json!({ "ok": false, "id": id, "error": msg, "output": output, "code": r.code })
    } else {
        json!({ "ok": true, "id": id, "output": output })
    }
}

/// 从网关拿模型清单（`{id,name}`）。网关没起来 / 卡住就给空表。


/// 这个 agent 是否支持「一键接入本机网关」。
/// omp 的 provider 由它自己的 `models.yml` 管理，这里不代写。


/// 把本地网关写进某个 agent 的配置。
///
/// `base_url` 是网关的地址（带 `/v1`），`key` 是网关密钥。


/// Pi：写 `~/.pi/agent/models.json` 的 `mymagpie` provider（走唯一写入口 `models::add_provider`）。


/// OpenCode：写 `~/.config/opencode/opencode.json` 的一个 OpenAI 兼容 provider。


/// 纯函数：在 Claude settings.json 的 `env` 里合并网关三个环境变量。
/// 其他字段原样保留；`env` 不是对象时报错（绝不覆盖用户数据）。


/// Claude Code：写 `~/.claude/settings.json` 的 `env`，把 Anthropic Messages
/// 指向本机网关的 `/v1/messages` 翻译桥。
///
/// 只改三个环境变量键，其他字段（权限、hooks、模型偏好）原样保留；
/// 已有旧值会被替换，可重复执行。写前备份。


/// Codex：写 `~/.codex/config.toml` 的 `mymagpie` provider，并把 `model_provider`
/// 指过去（wire_api=chat 对 OpenAI 兼容的 /v1/chat/completions）。


/// 把 `mymagpie` provider 合并进 Codex 的 config.toml 文本。
///
/// 只做两件小事：顶层 `model_provider` 改成 `mymagpie`；追加（或整块替换）
/// `[model_providers.mymagpie]`。其余原样保留。可重复执行。


/// 天数 → 公历日期（Howard Hinnant 的算法）。
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn omp_model_role_roundtrip() {
        let src = "modelRoles:\n  default: old/model\nstartup:\n  setupWizard: false\n";
        assert_eq!(yaml_child_get(src, "modelRoles", "default"), "old/model");

        let out = set_yaml_child(src, "modelRoles", "default", "magpie/auto/x");
        assert!(out.contains("  default: magpie/auto/x"));
        assert!(out.contains("startup:\n  setupWizard: false"));
        assert_eq!(yaml_child_get(&out, "modelRoles", "default"), "magpie/auto/x");

        // 块存在但没有该键：插在块首；块整体缺失：补出来。
        let missing_key = set_yaml_child("modelRoles:\n  other: 1\n", "modelRoles", "default", "m");
        assert_eq!(
            yaml_child_get(&missing_key, "modelRoles", "default"),
            "m"
        );
        let missing_block = set_yaml_child("startup: {}\n", "modelRoles", "default", "m");
        assert_eq!(
            yaml_child_get(&missing_block, "modelRoles", "default"),
            "m"
        );
    }

    #[test]
    fn first_version_extracts_leading_semver() {
        assert_eq!(first_version("1.5.2").as_deref(), Some("1.5.2"));
        assert_eq!(first_version("2.1.325 (Claude Code)").as_deref(), Some("2.1.325"));
        assert_eq!(first_version("codex-cli 0.169.0").as_deref(), Some("0.169.0"));
        assert_eq!(first_version("opencode v2.3.0").as_deref(), Some("2.3.0"));
        assert_eq!(first_version("no version here"), None);
    }

    #[test]
    fn yaml_scalar_reads_nightly_feed() {
        let doc = "version: 0.2.2-rc.3\nfiles:\n  - url: >-\n      https://x/deepseek-harness-0.2.2-rc.3-win-x64.exe\n    size: 1\npath: >-\n  https://x/deepseek-harness-0.2.2-rc.3-win-x64.exe\nsha512: abc\n";
        assert_eq!(yaml_scalar(doc, "version").as_deref(), Some("0.2.2-rc.3"));
        assert_eq!(
            yaml_scalar(doc, "path").as_deref(),
            Some("https://x/deepseek-harness-0.2.2-rc.3-win-x64.exe")
        );
        assert_eq!(yaml_scalar(doc, "missing"), None);
    }

    #[test]
    fn dsh_version_keeps_prerelease_suffix() {
        assert_eq!(
            first_full_version("0.2.2-rc.3\n").as_deref(),
            Some("0.2.2-rc.3")
        );
        assert_eq!(
            first_full_version("dsh 0.2.2-rc.3 (nightly)").as_deref(),
            Some("0.2.2-rc.3")
        );
        assert_eq!(first_full_version("no version").as_deref(), None);
        // 和 old parser 的差异：old 会丢掉 -rc.3。
        assert_eq!(first_version("0.2.2-rc.3").as_deref(), Some("0.2.2"));
    }

    #[test]
    fn tail_text_keeps_last_chars() {
        assert_eq!(tail_text("abcdef", 3), "def");
        assert_eq!(tail_text("abc", 10), "abc");
        assert_eq!(tail_text("  x  ", 5), "x");
    }

    #[test]
    fn registry_has_exactly_five_agents() {
        let ids: Vec<&str> = AGENTS.iter().map(|a| a.id).collect();
        assert_eq!(ids, ["pi", "opencode", "claude-code", "codex", "omp"]);
        for removed in [
            "vscode", "cline", "qoder", "zcode", "workbuddy", "workbuddy-ai", "codebuddy",
        ] {
            assert!(!ids.contains(&removed), "still listed: {removed}");
        }
    }

    #[test]
    fn every_registry_agent_has_an_op() {
        // 每条注册表项都要能被更新 / 接入逻辑识别（版本信息靠 agent_id 对齐）。
        for a in AGENTS {
            assert!(
                AGENT_OPS.iter().any(|o| o.agent_id == a.id),
                "no AgentOp for {}",
                a.id
            );
        }
        assert_eq!(AGENT_OPS.len(), 5);
    }

    #[test]
    fn unknown_versions_are_null_not_empty_string() {
        assert_eq!(known_version(None), None);
        assert_eq!(known_version(Some("".into())), None);
        assert_eq!(known_version(Some("   \n".into())), None);
        assert_eq!(known_version(Some(" 1.2.3 ".into())).as_deref(), Some("1.2.3"));
        // 少任意一边都不能算可更新。
        let shape = |c: Option<String>, l: Option<String>| {
            let c = known_version(c);
            let l = known_version(l);
            let updatable = matches!((&c, &l), (Some(a), Some(b)) if a != b);
            json!({ "currentVersion": c, "latestVersion": l, "updatable": updatable })
        };
        for (c, l) in [
            (None, Some("2.0.0".into())),
            (Some("1.0.0".into()), None),
            (Some("".into()), Some("2.0.0".into())),
            (Some("1.0.0".into()), Some("".into())),
        ] {
            let v = shape(c, l);
            assert!(!v["updatable"].as_bool().unwrap(), "v: {v}");
        }
        assert!(shape(Some("1.0.0".into()), Some("2.0.0".into()))["updatable"]
            .as_bool()
            .unwrap());
    }

    #[test]
    fn find_op_accepts_both_id_spellings() {
        assert_eq!(find_op("claude").unwrap().agent_id, "claude-code");
        assert_eq!(find_op("claude-code").unwrap().api_id, "claude");
        assert!(find_op("nope").is_none());
    }
}
