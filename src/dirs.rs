//! 目录浏览 / 最近项目 / 原生文件夹框。
//!
//! 移植自 dsh-agent-manager 的 `lib/features/workspace/dirs.js`。
//! 合并前有两套：pi-web 的 `list_dir`（列文件供预览抽屉）和 dsh 的
//! `browseDirs`（只列目录供设置页选工作目录）。这里一个函数同时给三样，
//! 不再各写一遍排序与截断。

use std::collections::HashSet;
use std::path::Path;

use serde_json::{json, Value};

use crate::atomic::{read_json, write_json_atomic_pretty};
use crate::paths;

/// 列目录的最大条目数。
const MAX_ENTRIES: usize = 2000;

/// 这些目录不出现在「可选目录」列表里。
fn is_skipped(name: &str) -> bool {
    matches!(name, "." | "node_modules" | "__pycache__")
}

/// 列目录。空路径 = 列盘符根。
pub fn browse(raw: &str) -> Result<Value, String> {
    let text = raw.trim();
    if text.is_empty() {
        let mut roots = Vec::new();
        for byte in b'A'..=b'Z' {
            let drive = format!("{}:\\", byte as char);
            if std::path::Path::new(&drive).is_dir() {
                roots.push(json!({ "path": drive, "name": drive }));
            }
        }
        return Ok(json!({
            "ok": true, "path": "", "parent": null, "roots": true,
            "dirs": roots, "entries": [], "truncated": 0,
        }));
    }

    let target = paths::dunce_resolve(Path::new(text));
    let meta = std::fs::metadata(&target).map_err(|_| format!("目录不存在: {}", target.display()))?;
    if !meta.is_dir() {
        return Err(format!("不是目录: {}", target.display()));
    }

    let read = std::fs::read_dir(&target).map_err(|e| format!("无法读取目录: {e}"))?;

    let mut entries: Vec<Value> = Vec::new();
    for item in read.flatten() {
        let name = item.file_name().to_string_lossy().to_string();
        let path = item.path();
        // 断链 / 无权限的条目直接跳过，不让它把整个列表搞崩。
        let meta = match std::fs::metadata(&path) {
            Ok(m) => m,
            Err(_) => continue,
        };
        let is_dir = meta.is_dir();
        let size = if is_dir { 0 } else { meta.len() };
        entries.push(json!({
            "name": name,
            "path": path.to_string_lossy(),
            "dir": is_dir,
            "size": size,
        }));
    }

    // 目录在前，名字按中文排序（Windows 上 Foo/foo 不该被 ASCII 劈成两段）。
    entries.sort_by(|a, b| {
        let da = a["dir"].as_bool().unwrap_or(false);
        let db = b["dir"].as_bool().unwrap_or(false);
        if da != db {
            return db.cmp(&da);
        }
        let na = a["name"].as_str().unwrap_or("");
        let nb = b["name"].as_str().unwrap_or("");
        na.cmp(nb)
    });

    let parent = target.parent().map(|p| p.to_string_lossy().to_string());
    let parent = match parent {
        Some(p) if paths::norm(Path::new(&p)) != paths::norm(&target) => Some(p),
        _ => None,
    };

    let dirs: Vec<Value> = entries
        .iter()
        .filter(|e| e["dir"].as_bool().unwrap_or(false))
        .filter(|e| {
            let n = e["name"].as_str().unwrap_or("");
            !is_skipped(n) && !n.starts_with('.')
        })
        .map(|e| json!({ "path": e["path"].as_str().unwrap_or(""), "name": e["name"].as_str().unwrap_or("") }))
        .collect();

    let truncated = entries.len().saturating_sub(MAX_ENTRIES);
    entries.truncate(MAX_ENTRIES);

    Ok(json!({
        "ok": true,
        "path": target.to_string_lossy(),
        "parent": parent,
        "roots": false,
        "entries": entries,
        "dirs": dirs,
        "truncated": truncated,
    }))
}

/// 设置页「工作目录」下拉：最近项目 + 宿主当前项目去重。
pub fn list_dirs() -> Value {
    let mut seen: HashSet<String> = HashSet::new();
    let mut dirs: Vec<Value> = Vec::new();

    let push = |p: &str, dirs: &mut Vec<Value>, seen: &mut HashSet<String>| {
        let key = paths::norm(Path::new(p));
        if key.is_empty() || seen.contains(&key) {
            return;
        }
        seen.insert(key);
        let name = Path::new(p)
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| p.to_string());
        dirs.push(json!({ "path": p, "name": name }));
    };

    for p in known_workspaces() {
        push(&p, &mut dirs, &mut seen);
    }
    for (p, _) in opened_at() {
        push(&p, &mut dirs, &mut seen);
    }

    let current = read_json::<Value>(&paths::last_workspace_file(), None)
        .and_then(|v| v.get("path").and_then(|p| p.as_str()).map(|s| s.to_string()));

    json!({ "ok": true, "current": current, "dirs": dirs })
}

/// 「开过的目录」名单。空项目也永久留着（用户明确要留占位），但目录必须还在。
pub fn opened_at() -> Vec<(String, u64)> {
    let raw = read_json::<Value>(&paths::opened_file(), None).unwrap_or_else(|| json!({}));
    let Some(obj) = raw.as_object() else {
        return Vec::new();
    };
    let mut out: Vec<(String, u64)> = obj
        .iter()
        .filter_map(|(k, v)| v.as_u64().map(|n| (k.clone(), n)))
        .collect();
    out.sort_by_key(|x| std::cmp::Reverse(x.1));
    out
}

/// 记住一个「开过的目录」。
pub fn remember(dir: &str) -> std::io::Result<()> {
    let key = paths::norm(Path::new(dir));
    let mut raw = read_json::<Value>(&paths::opened_file(), None).unwrap_or_else(|| json!({}));
    if !raw.is_object() {
        raw = json!({});
    }
    raw[key] = json!(now_millis());
    write_json_atomic_pretty(&paths::opened_file(), &raw, 1)?;
    write_json_atomic_pretty(&paths::last_workspace_file(), &json!({ "path": dir }), 1)?;
    Ok(())
}

/// 上次的工作目录。
pub fn last_workspace() -> Option<String> {
    read_json::<Value>(&paths::last_workspace_file(), None)
        .and_then(|v| v.get("path").and_then(|p| p.as_str()).map(|s| s.to_string()))
        .filter(|s| !s.is_empty())
}

/// 新建一个项目目录。名字只允许常规字符，位置必须已存在。
pub fn create_project(parent: &str, name: &str) -> Result<String, String> {
    let clean = name.trim();
    if clean.is_empty()
        || clean.contains(['\\', '/', ':', '*', '?', '"', '<', '>', '|'])
        || clean == "."
        || clean == ".."
    {
        return Err("项目名不合法".to_string());
    }
    let base = paths::dunce_resolve(Path::new(parent));
    if !base.is_dir() {
        return Err("位置不存在".to_string());
    }
    let target = base.join(clean);
    std::fs::create_dir(&target).map_err(|e| format!("新建目录失败: {e}"))?;
    let _ = remember(&target.to_string_lossy());
    Ok(target.to_string_lossy().to_string())
}

/// 重命名项目目录。
pub fn rename_project(old_path: &str, new_name: &str) -> Result<String, String> {
    let clean = new_name.trim();
    if clean.is_empty() || clean.contains(['\\', '/', ':', '*', '?', '"', '<', '>', '|']) {
        return Err("项目名不合法".to_string());
    }
    let from = paths::dunce_resolve(Path::new(old_path));
    let target = from
        .parent()
        .map(|p| p.join(clean))
        .ok_or_else(|| "路径不合法".to_string())?;
    if paths::norm(&from) == paths::norm(&target) {
        return Ok(target.to_string_lossy().to_string());
    }
    std::fs::rename(&from, &target).map_err(|e| format!("重命名失败: {e}"))?;
    let _ = remember(&target.to_string_lossy());
    Ok(target.to_string_lossy().to_string())
}

/// 在资源管理器里定位（选中文件）。
pub fn reveal(target: &str) {
    let p = paths::dunce_resolve(Path::new(target));
    let _ = std::process::Command::new("explorer.exe")
        .arg(format!("/select,{}", p.display()))
        .spawn();
}

/// 已知的「最近项目」：从会话转录里反查 cwd，去重后按最近活动排序。
pub fn known_workspaces() -> Vec<String> {
    crate::transcripts::known_workspaces()
}

fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// 原生文件夹选择框。走 PowerShell 的 FolderBrowserDialog（系统自带）。
pub async fn pick_folder(initial: &str) -> Option<String> {
    let script = format!(
        "Add-Type -AssemblyName System.Windows.Forms; \
         $d = New-Object System.Windows.Forms.FolderBrowserDialog; \
         $d.Description = '选择工作目录'; \
         $init = '{}'; \
         if ($init) {{ $d.SelectedPath = $init }}; \
         if ($d.ShowDialog() -eq 'OK') {{ [Console]::Out.Write($d.SelectedPath) }}",
        initial.replace('\'', "''")
    );
    let out = crate::proc::CmdSpec::owned(
        "powershell.exe",
        &[
            "-NoProfile".to_string(),
            "-STA".to_string(),
            "-NonInteractive".to_string(),
            "-Command".to_string(),
            script.clone(),
        ],
    )
    .timeout(120_000)
    .run()
    .await
    .ok()?;
    let trimmed = out.stdout.trim().to_string();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    }
}
