//! Pi 管理能力：状态 / 版本 / 更新 / 压缩设置 / 开终端窗口 / 一次性任务。

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::atomic::read_value;
use crate::launch::{build_cmdline, launch_in_terminal, make_mark};
use crate::paths;
use crate::proc::{run_process, run_process_full};
use crate::windows::{remember_window, WindowRecord};

const PI_PACKAGE: &str = "@earendil-works/pi-coding-agent";
const LATEST_TTL_MS: u64 = 60 * 60 * 1000;

fn current_version_path() -> PathBuf {
    paths::pi_home().join("agent").join("install").join("current-version")
}

fn install_root() -> PathBuf {
    paths::pi_home().join("agent").join("install")
}

fn read_text(f: &Path) -> Option<String> {
    std::fs::read_to_string(f)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// pi 可执行文件的候选路径（与 agents/pi.js 同顺序）。
pub fn pi_bin_candidates() -> Vec<PathBuf> {
    let bin_dir = paths::pi_home().join("agent").join("bin");
    vec![bin_dir.join("pi.cmd"), bin_dir.join("pi.ps1"), bin_dir.join("pi")]
}

pub fn find_pi_binary() -> Option<PathBuf> {
    pi_bin_candidates().into_iter().find(|f| f.is_file())
}

/// 把 pi.cmd 拆成 [node.exe, cli.js] 直接 spawn。
///
/// `shell:true` 时提示词里的换行 / 引号会被 cmd 拆碎（模型收到残缺片段），
/// `shell:false` 直接跑 .cmd 又是 EINVAL。解析失败退回 .cmd。
pub fn resolve_pi_invocation(bin: &Path) -> Option<(String, Vec<String>, HashMap<String, String>)> {
    if !cfg!(windows) {
        return Some((bin.to_string_lossy().to_string(), vec![], HashMap::new()));
    }
    if !bin.to_string_lossy().to_lowercase().ends_with(".cmd") {
        return Some((bin.to_string_lossy().to_string(), vec![], HashMap::new()));
    }
    let version = read_text(&current_version_path())?;
    if !version
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '+' || c == '-')
    {
        return None;
    }
    let pkg_dir = install_root()
        .join("releases")
        .join(&version)
        .join("node_modules")
        .join("@earendil-works")
        .join("pi-coding-agent");
    let pkg: Value = serde_json::from_str(&std::fs::read_to_string(pkg_dir.join("package.json")).ok()?).ok()?;
    let bin_path = match pkg.get("bin") {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Object(o)) => o.get("pi").and_then(|v| v.as_str())?.to_string(),
        _ => return None,
    };
    let cli = pkg_dir.join(&bin_path);
    let rel = cli.strip_prefix(&pkg_dir).ok()?;
    if rel.components().any(|c| matches!(c, std::path::Component::ParentDir)) {
        return None;
    }
    if !cli.exists() {
        return None;
    }
    let mut env = HashMap::new();
    env.insert(
        "PI_MANAGED_INSTALL_ROOT".to_string(),
        install_root().to_string_lossy().to_string(),
    );
    Some((
        std::env::current_exe()
            .ok()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|| "node".to_string()),
        vec![cli.to_string_lossy().to_string()],
        env,
    ))
}

/// node.exe 的位置。pi 装在 pi 的 releases 里，node 通常随 Electron 一起，
/// 但不一定在 PATH —— 找不到就退回 "node"。
fn node_exe() -> String {
    if let Ok(exe) = std::env::current_exe() {
        // pi 的可执行文件在 ~/.pi/agent/bin/pi.cmd，node 往往在同一台机器的
        // Electron 发行版里；这里先按 PATH 找，再退回 "node"。
        let _ = exe;
    }
    "node".to_string()
}

/// 一次性命令统一走这里：能拆成 node 直跑就不穿 shell。
async fn pi_run(args: &[String], timeout_ms: u64, cwd: Option<&Path>) -> crate::proc::ProcResult {
    let Some(bin) = find_pi_binary() else {
        return crate::proc::ProcResult::failed_with("未找到 pi 可执行文件");
    };
    if let Some((file, prefix, env)) = resolve_pi_invocation(&bin) {
        let file = if file == std::env::current_exe().ok().map(|p| p.to_string_lossy().to_string()).unwrap_or_default() {
            node_exe()
        } else {
            file
        };
        let mut all = prefix;
        all.extend_from_slice(args);
        return run_process(&file, &all, cwd, timeout_ms, Some(&env)).await;
    }
    let mut all: Vec<String> = args.to_vec();
    all.insert(0, bin.to_string_lossy().to_string());
    let bin_str = bin.to_string_lossy().to_string();
    run_process_full(&bin_str, args, cwd, timeout_ms, None, true).await
}

// ------------------------------------------------------------------ 版本三态

use std::sync::Mutex;

struct LatestCache {
    at: u64,
    version: Option<String>,
    error: Option<String>,
}

static LATEST: Mutex<Option<LatestCache>> = Mutex::new(None);

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn read_latest_cache() -> (Option<String>, Option<String>, bool) {
    let guard = LATEST.lock().unwrap();
    match guard.as_ref() {
        None => (None, None, false),
        Some(c) => {
            let fresh = now_ms().saturating_sub(c.at) < LATEST_TTL_MS;
            let known = c.version.is_some() && fresh;
            (c.version.clone(), c.error.clone(), known)
        }
    }
}

async fn fetch_latest_version() -> (Option<String>, Option<String>) {
    {
        let guard = LATEST.lock().unwrap();
        if let Some(c) = guard.as_ref() {
            if now_ms().saturating_sub(c.at) < LATEST_TTL_MS {
                return (c.version.clone(), c.error.clone());
            }
        }
    }
    let url = format!(
        "https://registry.npmjs.org/{}/latest",
        PI_PACKAGE.replace('/', "%2f")
    );
    let mut version: Option<String> = None;
    let mut error: Option<String> = None;

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(8))
        .build();
    if let Ok(client) = client {
        match client.get(&url).header("accept", "application/json").send().await {
            Ok(res) if res.status().is_success() => match res.json::<Value>().await {
                Ok(body) => {
                    let v = body
                        .get("version")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .trim()
                        .to_string();
                    if v.is_empty() {
                        error = Some("registry 响应里没有 version".into());
                    } else {
                        version = Some(v);
                    }
                }
                Err(e) => error = Some(e.to_string()),
            },
            Ok(res) => error = Some(format!("registry HTTP {}", res.status().as_u16())),
            Err(e) => error = Some(e.to_string()),
        }
    }

    // 兜底：curl（有些机器 Rust 的 TLS 走不通但 curl 能走）
    if version.is_none() {
        let args = vec![
            "-sS".to_string(),
            "--max-time".to_string(),
            "20".to_string(),
            "-H".to_string(),
            "accept: application/json".to_string(),
            url.clone(),
        ];
        let r = run_process("curl.exe", &args, None, 25_000, None).await;
        if !r.failed {
            if let Ok(body) = serde_json::from_str::<Value>(&r.stdout) {
                if let Some(v) = body.get("version").and_then(|v| v.as_str()) {
                    let v = v.trim().to_string();
                    if !v.is_empty() {
                        version = Some(v);
                        error = None;
                    }
                }
            }
        }
    }

    // 再兜底：本机 npm 自己配的 registry
    if version.is_none() {
        let npm = if cfg!(windows) { "npm.cmd" } else { "npm" };
        let r = run_process_full(
            npm,
            &[
                "view".to_string(),
                PI_PACKAGE.to_string(),
                "version".to_string(),
            ],
            None,
            30_000,
            None,
            cfg!(windows),
        )
        .await;
        if !r.failed {
            if let Some(v) = r.stdout.split_whitespace().last() {
                if v.starts_with(|c: char| c.is_ascii_digit())
                    && v.split('.').count() >= 2
                    && v.chars().all(|c| c.is_ascii_digit() || c == '.')
                {
                    version = Some(v.to_string());
                    error = None;
                }
            }
        }
    }

    *LATEST.lock().unwrap() = Some(LatestCache {
        at: now_ms(),
        version: version.clone(),
        error: error.clone(),
    });
    (version, error)
}

// ------------------------------------------------------------------ 状态

pub async fn get_status() -> Value {
    let bin = find_pi_binary();
    let models = read_value(&paths::models_json()).unwrap_or_else(|| json!({ "providers": {} }));
    let settings = read_value(&paths::settings_json()).unwrap_or_else(|| json!({}));
    let installed_version = read_text(&current_version_path());

    let (latest_version, latest_error, latest_known) = read_latest_cache();
    if !latest_known {
        tokio::spawn(async {
            let _ = fetch_latest_version().await;
        });
    }

    let mut version = Value::Null;
    let mut version_error = Value::Null;
    if bin.is_some() {
        let r = pi_run(&["--version".to_string()], 20_000, None).await;
        if !r.failed {
            let t = r.stdout.trim();
            let t = if t.is_empty() { r.stderr.trim() } else { t };
            version = json!(t);
        } else {
            let t = format!("{}{}", r.stdout, r.stderr).trim().to_string();
            version_error = json!(t.chars().take(400).collect::<String>());
        }
    }

    let provider_list: Vec<Value> = models
        .get("providers")
        .and_then(|v| v.as_object())
        .map(|m| {
            m.iter()
                .map(|(id, cfg)| {
                    let ms = cfg.get("models").and_then(|v| v.as_array()).cloned().unwrap_or_default();
                    json!({
                        "id": id,
                        "baseUrl": cfg.get("baseUrl").cloned().unwrap_or(Value::Null),
                        "api": cfg.get("api").cloned().unwrap_or(Value::Null),
                        "hasKey": cfg.get("apiKey").and_then(|v| v.as_str()).map(|s| !s.is_empty()).unwrap_or(false),
                        "modelCount": ms.len(),
                        "models": ms.iter().filter_map(|m| m.get("id").cloned()).collect::<Vec<_>>(),
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    let model_count: usize = provider_list
        .iter()
        .map(|p| p.get("modelCount").and_then(|v| v.as_u64()).unwrap_or(0) as usize)
        .sum();

    json!({
        "installed": bin.is_some(),
        "binary": bin.map(|b| b.to_string_lossy().to_string()),
        "version": version,
        "installedVersion": installed_version,
        "versionError": version_error,
        "latestVersion": latest_version,
        "latestError": latest_error,
        "latestKnown": latest_known,
        "updateAvailable": match (&latest_version, &installed_version) {
            (Some(l), Some(i)) => l != i,
            _ => false,
        },
        "home": paths::pi_home().to_string_lossy(),
        "modelsPath": paths::models_json().to_string_lossy(),
        "settingsPath": paths::settings_json().to_string_lossy(),
        "providerCount": provider_list.len(),
        "modelCount": model_count,
        "providers": provider_list,
        "shellPath": settings.get("shellPath").cloned().unwrap_or(Value::Null),
        "defaultThinkingLevel": settings.get("defaultThinkingLevel").cloned().unwrap_or(Value::Null),
        "projectDir": resolve_project_dir().await.map(|p| p.get("path").cloned().unwrap_or(Value::Null)),
    })
}

pub async fn latest() -> Value {
    let (version, error) = fetch_latest_version().await;
    json!({
        "ok": true,
        "latestVersion": version,
        "latestError": error,
        "installedVersion": read_text(&current_version_path()),
    })
}

/// 当前安装的 pi 版本（跑 `pi --version`）。没装或跑不出来返回 `None`。
///
/// 给「更新与接入」页用：`/api/agents` 要每家的 `currentVersion`，
/// 这一条只取版本，不做 `get_status()` 那一堆（读 models.json / settings.json、
/// 触发 registry 缓存）。
pub async fn version() -> Option<String> {
    if find_pi_binary().is_none() {
        return None;
    }
    let r = pi_run(&["--version".to_string()], 20_000, None).await;
    if r.failed {
        return None;
    }
    let t = r.stdout.trim();
    let t = if t.is_empty() { r.stderr.trim() } else { t };
    if t.is_empty() {
        None
    } else {
        Some(t.to_string())
    }
}

/// 最新版本（npm registry / curl / 本机 npm，带 1 小时缓存）。
pub async fn latest_version() -> Option<String> {
    fetch_latest_version().await.0
}

// ------------------------------------------------------------------ 更新 / 模型清单 / 一次性任务

pub async fn update_pi() -> Value {
    if find_pi_binary().is_none() {
        return json!({ "ok": false, "error": "未找到 pi 可执行文件" });
    }
    let before = read_text(&current_version_path());
    let r = pi_run(&["update".to_string()], 600_000, None).await;
    let after = read_text(&current_version_path());
    // update 有时超时但装完了
    let changed = match (&before, &after) {
        (Some(b), Some(a)) => b != a,
        _ => false,
    };
    let output: String = format!("{}{}", r.stdout, r.stderr)
        .trim()
        .chars()
        .rev()
        .take(4000)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    json!({
        "ok": changed || !r.failed,
        "code": r.code,
        "from": before,
        "to": after,
        "changed": changed,
        "output": output,
    })
}

pub async fn list_pi_models() -> Value {
    let r = pi_run(&["--list-models".to_string()], 60_000, None).await;
    let text = format!("{}{}", r.stdout, r.stderr).trim().to_string();
    json!({
        "ok": !r.failed,
        "output": text.chars().rev().take(20_000).collect::<Vec<_>>().into_iter().rev().collect::<String>(),
    })
}

pub async fn run_pi_task(body: &Value) -> Value {
    let prompt = body.get("prompt").and_then(|v| v.as_str()).unwrap_or("");
    if prompt.is_empty() {
        return json!({ "ok": false, "error": "prompt 必填" });
    }
    let mut args = vec![
        "-p".to_string(),
        prompt.to_string(),
        "--mode".to_string(),
        if body.get("mode").and_then(|v| v.as_str()) == Some("json") {
            "json".to_string()
        } else {
            "text".to_string()
        },
        "--no-session".to_string(),
    ];
    if let Some(p) = body.get("provider").and_then(|v| v.as_str()).filter(|s| !s.is_empty()) {
        args.push("--provider".to_string());
        args.push(p.to_string());
    }
    if let Some(m) = body.get("model").and_then(|v| v.as_str()).filter(|s| !s.is_empty()) {
        args.push("--model".to_string());
        args.push(m.to_string());
    }
    let cwd = body
        .get("cwd")
        .and_then(|v| v.as_str())
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
    let r = pi_run(&args, 600_000, Some(&cwd)).await;
    json!({
        "ok": !r.failed,
        "code": r.code,
        "output": r.stdout.chars().rev().take(40_000).collect::<Vec<_>>().into_iter().rev().collect::<String>(),
        "stderr": r.stderr.chars().rev().take(4_000).collect::<Vec<_>>().into_iter().rev().collect::<String>(),
    })
}

// ------------------------------------------------------------------ DSH 宿主工作区解析

/// 从宿主自己的 workspace.json 反查当前窗口属于哪个项目。
pub async fn resolve_project_dir() -> Option<Value> {
    let session_id = std::env::var("DSH_SESSION_ID").unwrap_or_default();
    let doc: Value = serde_json::from_str(
        &std::fs::read_to_string(paths::dsh_workspace_path()).ok()?,
    )
    .ok()?;
    let workspaces = doc.get("tables")?.get("workspaces")?.as_object()?;
    let entries: Vec<&Value> = workspaces
        .values()
        .filter(|w| w.get("path").and_then(|v| v.as_str()).is_some_and(|p| !p.is_empty()))
        .collect();

    if !session_id.trim().is_empty() {
        let hit = entries.iter().find(|w| {
            w.get("sessionIds")
                .and_then(|v| v.as_array())
                .map(|a| a.iter().any(|x| x.as_str() == Some(session_id.as_str())))
                .unwrap_or(false)
        });
        // 带着会话 id 却对不上任何工作区，说明会话被清掉了 —— 宁可返回 null，
        // 也别拿 cwd 冒充。
        return hit.map(|w| {
            json!({
                "path": w.get("path").cloned().unwrap_or(Value::Null),
                "title": w.get("title").cloned().unwrap_or(Value::Null),
                "source": "session",
            })
        });
    }

    if let Some(def) = doc.get("global").and_then(|g| g.get("defaultWorkspaceId")).and_then(|v| v.as_str()) {
        if let Some(w) = workspaces.get(def) {
            if w.get("path").is_some() {
                return Some(json!({
                    "path": w.get("path").cloned().unwrap_or(Value::Null),
                    "title": w.get("title").cloned().unwrap_or(Value::Null),
                    "source": "default",
                }));
            }
        }
    }

    let mut recent: Vec<&Value> = entries
        .iter()
        .copied()
        .filter(|w| w.get("updatedAt").and_then(|v| v.as_str()).is_some())
        .collect();
    recent.sort_by(|a, b| {
        let ka = a.get("updatedAt").and_then(|v| v.as_str()).unwrap_or("");
        let kb = b.get("updatedAt").and_then(|v| v.as_str()).unwrap_or("");
        kb.cmp(ka)
    });
    recent.first().map(|w| {
        json!({
            "path": w.get("path").cloned().unwrap_or(Value::Null),
            "title": w.get("title").cloned().unwrap_or(Value::Null),
            "source": "recent",
        })
    })
}

// ------------------------------------------------------------------ 开真实终端窗口

pub async fn open_pi_session(body: &Value) -> Value {
    let Some(bin) = find_pi_binary() else {
        return json!({ "ok": false, "error": "未找到 pi 可执行文件" });
    };
    let mut args: Vec<String> = Vec::new();
    if let Some(p) = body.get("provider").and_then(|v| v.as_str()).filter(|s| !s.is_empty()) {
        args.push("--provider".to_string());
        args.push(p.to_string());
    }
    if let Some(m) = body.get("model").and_then(|v| v.as_str()).filter(|s| !s.is_empty()) {
        args.push("--model".to_string());
        args.push(m.to_string());
    }
    if let Some(p) = body.get("prompt").and_then(|v| v.as_str()).filter(|s| !s.is_empty()) {
        args.push(p.to_string());
    }

    let resolved = if body.get("cwd").and_then(|v| v.as_str()).is_some() {
        None
    } else {
        resolve_project_dir().await
    };
    let workdir = body
        .get("cwd")
        .and_then(|v| v.as_str())
        .map(PathBuf::from)
        .or_else(|| {
            resolved
                .as_ref()
                .and_then(|r| r.get("path").and_then(|v| v.as_str()).map(PathBuf::from))
        })
        .or_else(|| crate::dirs::last_workspace().map(PathBuf::from))
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
    let cwd_source = resolved
        .as_ref()
        .and_then(|r| r.get("source").and_then(|v| v.as_str()))
        .unwrap_or("explicit")
        .to_string();

    let mut parts = vec![bin.to_string_lossy().to_string()];
    parts.extend(args.iter().cloned());
    let cmdline = build_cmdline(&parts);

    if !cfg!(windows) {
        let r = run_process(
            "sh",
            &["-c".to_string(), format!("cd '{}' && exec {}", workdir.to_string_lossy(), cmdline)],
            Some(&workdir),
            15_000,
            None,
        )
        .await;
        return json!({
            "ok": !r.failed,
            "launcher": "shell",
            "command": cmdline,
            "cwd": workdir.to_string_lossy(),
            "cwdSource": cwd_source,
        });
    }

    let mark = make_mark("PI");
    let r = launch_in_terminal(&workdir, &cmdline, "pi", Some(mark.clone()), None).await;
    if !r.ok {
        return json!({
            "ok": false,
            "launcher": "wt",
            "command": cmdline,
            "cwd": workdir.to_string_lossy(),
            "cwdSource": cwd_source,
            "error": r.error,
        });
    }
    remember_window(WindowRecord {
        mark: mark.clone(),
        kind: "pi".to_string(),
        title: Some("pi".to_string()),
        session_id: None,
        cwd: Some(r.cwd.to_string_lossy().to_string()),
        hwnd: r.hwnd,
        focused: r.focused,
        at: 0,
    });
    json!({
        "ok": true,
        "launcher": "wt",
        "command": cmdline,
        "cwd": r.cwd.to_string_lossy(),
        "cwdSource": cwd_source,
        "warning": r.cwd_warning,
        "mark": mark,
        "hwnd": r.hwnd,
        "focused": r.focused,
        "ms": r.ms,
    })
}
