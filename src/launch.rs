//! 终端窗口启动器 —— pi 与 opencode 共用。
//!
//! 为什么单独一个文件：
//!   两边都要做同一件事：**在一个真实终端窗口里**跑一个 TUI 程序，并且
//!   ① 用户真的看得见（窗口出现在前台）
//!   ② 我们事后能回收它（不会攒一堆孤儿进程）
//!   ③ 整个过程不往宿主进程里留定时器和子进程引用
//!
//! 踩过的坑（都是实测出来的，不是推测）：
//!   1. 直接 spawn `cmd.exe /k` 而**不分配终端** —— TUI 画不出界面也不会退出。
//!   2. 窗口出来了却**不在前台** —— 必须 ShowWindow + TOPMOST→NOTOPMOST 翻转 +
//!      AttachThreadInput + Alt 键解锁，而且**必须用 GetForegroundWindow 复核**。
//!   3. `title <mark>` 会被 TUI 自己覆盖 —— mark 写进临时 `.cmd` 的**文件名**，
//!      它留在 cmd.exe 的 CommandLine 里，TUI 改不了。
//!   4. 内层命令直接塞进 `cmd /k "..."` 时，引号/`&`/空格路径要经过
//!      PowerShell → cmd → start → wt 五层转义，实测必错。
//!      现在内层命令原样写进一个临时 .cmd 文件，命令行只剩一个路径 —— 零转义。
//!
//! 所以这里把「快照 → 拉起 → 轮询新窗口 → 置前台 → 复核」塞进**一个**
//! PowerShell 进程里一次做完。回收靠 mark：mark 是临时 .cmd 文件名的一部分，
//! 事后按 `Name='cmd.exe' AND CommandLine LIKE '*mark*'` 精确找回，不依赖 pid。

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::json;

use crate::proc::{run_process_full, ProcResult};

/// 轮询新窗口的默认上限。实测窗口 1.5–4 秒内出现，9 秒足够兜住慢机器。
const DEFAULT_WAIT_MS: u64 = 9000;

/// PowerShell 子进程的硬超时：比轮询上限再宽一点，防止它自己卡死。
const PS_TIMEOUT_MS: u64 = DEFAULT_WAIT_MS + 8000;

/// stdout/stderr 保留上限，防止异常输出把内存吃满。
const OUTPUT_CAP: usize = 8_000;

/// 临时启动脚本的文件名前缀。mark 拼在后面，供事后按命令行找回。
const SCRIPT_PREFIX: &str = "dsh-launch-";

/// 临时脚本的保留时长：超过就当作陈货清掉。
const SCRIPT_TTL_MS: u64 = 2 * 60 * 60 * 1000;

static MARK_SEQ: AtomicU64 = AtomicU64::new(0);

/// 生成一个既唯一又可读的标记。
///
/// 唯一性靠**计数器**而不是随机数：实测同一毫秒内连续生成 500 个时，
/// 36² = 1296 个随机盐按生日问题会撞掉 47 个。mark 重复意味着「关闭全部」
/// 会误杀、或关掉别人的窗口 —— 这是安全属性，不能靠概率。
pub fn make_mark(prefix: &str) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let stamp = to_base36(now as u64);
    let seq = to_base36(MARK_SEQ.fetch_add(1, Ordering::Relaxed));
    let pid = to_base36(std::process::id() as u64);
    format!("{prefix}{stamp}{pid}{seq}")
}

fn to_base36(mut n: u64) -> String {
    const DIGITS: &[u8] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ";
    if n == 0 {
        return "0".to_string();
    }
    let mut buf = Vec::new();
    while n > 0 {
        buf.push(DIGITS[(n % 36) as usize]);
        n /= 36;
    }
    buf.reverse();
    String::from_utf8_lossy(&buf).to_string()
}

/// 临时启动脚本的路径（mark 在文件名里，这就是回收锚点）。
pub fn script_path_for(mark: &str) -> PathBuf {
    crate::paths::temp_dir().join(format!("{SCRIPT_PREFIX}{mark}.cmd"))
}

/// 把一个 argv 元素转成**批处理文件里**安全的写法。
///
/// 两个转义各管一段：
///   * `%` → `%%` —— 批处理会展开 `%VAR%`。
///   * `"` → `\"` —— MSVCRT / CommandLineToArgvW 的约定。
///
/// 只有「全是安全字符」才不加引号：`&` `|` `<` `>` `^` 在引号外是命令分隔符。
pub fn quote_cmd_arg(value: &str) -> String {
    let escaped = value.replace('%', "%%").replace('"', "\\\"");
    if !escaped.is_empty()
        && escaped
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "_.:\\/+@=,-[]~".contains(c))
    {
        return escaped;
    }
    format!("\"{escaped}\"")
}

/// 把 argv 数组拼成一条可在批处理文件里执行的命令行。
pub fn build_cmdline(parts: &[String]) -> String {
    parts
        .iter()
        .map(|p| quote_cmd_arg(p))
        .collect::<Vec<_>>()
        .join(" ")
}

/// `title` 命令的文本转义。title 不接受引号包裹，只能把命令分隔符逐字转义。
pub fn escape_title(text: &str) -> String {
    let mut out = String::new();
    for ch in text.chars() {
        match ch {
            '%' => out.push_str("%%"),
            '&' | '|' | '<' | '>' | '^' => {
                out.push('^');
                out.push(ch);
            }
            '\r' | '\n' => out.push(' '),
            _ => out.push(ch),
        }
    }
    out
}

/// 内层命令 + 固定前缀写成 .cmd 文件。
fn build_script(mark: &str, workdir: &Path, title: &str, inner: &str) -> String {
    let lines = [
        "@echo off".to_string(),
        format!("set DSHWIN={mark}"),
        format!("cd /d \"{}\"", workdir.to_string_lossy()),
        format!("title {}", escape_title(if title.is_empty() { mark } else { title })),
        inner.to_string(),
    ];
    lines.join("\r\n") + "\r\n"
}

/// PowerShell 单引号字符串里唯一需要转义的就是单引号本身。
fn ps_quote(text: &str) -> String {
    text.replace('\'', "''")
}

/// 把脚本 + 参数编成 -EncodedCommand 的 base64（UTF-16LE）。
///
/// 不走 `-Command "..."`：脚本里带引号、`&`、空格路径，命令行转义失败过好几次。
fn encode_command(script: &str) -> String {
    let mut bytes = Vec::with_capacity(script.len() * 2);
    for unit in script.encode_utf16() {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }
    base64_encode(&bytes)
}

fn base64_encode(data: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    let mut i = 0;
    while i + 3 <= data.len() {
        let n = ((data[i] as u32) << 16) | ((data[i + 1] as u32) << 8) | data[i + 2] as u32;
        out.push(TABLE[(n >> 18) as usize & 63] as char);
        out.push(TABLE[(n >> 12) as usize & 63] as char);
        out.push(TABLE[(n >> 6) as usize & 63] as char);
        out.push(TABLE[n as usize & 63] as char);
        i += 3;
    }
    let rem = data.len() - i;
    if rem == 1 {
        let n = (data[i] as u32) << 16;
        out.push(TABLE[(n >> 18) as usize & 63] as char);
        out.push(TABLE[(n >> 12) as usize & 63] as char);
        out.push('=');
        out.push('=');
    } else if rem == 2 {
        let n = ((data[i] as u32) << 16) | ((data[i + 1] as u32) << 8);
        out.push(TABLE[(n >> 18) as usize & 63] as char);
        out.push(TABLE[(n >> 12) as usize & 63] as char);
        out.push(TABLE[(n >> 6) as usize & 63] as char);
        out.push('=');
    }
    out
}

const LAUNCH_PS: &str = r#"
$ErrorActionPreference = 'Stop'
$WorkDir = '__WORKDIR__'
$CmdFile = '__CMDFILE__'
$WaitMs = __WAITMS__

Add-Type -TypeDefinition @'
using System;
using System.Text;
using System.Collections.Generic;
using System.Runtime.InteropServices;
using System.Threading;
public class DshLaunchWin {
  public delegate bool EnumProc(IntPtr h, IntPtr l);
  [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc cb, IntPtr l);
  [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
  [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetClassName(IntPtr h, StringBuilder s, int n);
  [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
  [DllImport("user32.dll")] public static extern bool BringWindowToTop(IntPtr h);
  [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h, int c);
  [DllImport("user32.dll")] public static extern bool SetWindowPos(IntPtr h, IntPtr after, int x, int y, int cx, int cy, uint flags);
  [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
  [DllImport("kernel32.dll")] public static extern uint GetCurrentThreadId();
  [DllImport("user32.dll")] public static extern bool AttachThreadInput(uint attach, uint to, bool fAttach);
  [DllImport("user32.dll")] public static extern void keybd_event(byte vk, byte scan, uint flags, UIntPtr extra);

  static readonly IntPtr TOPMOST = new IntPtr(-1);
  static readonly IntPtr NOTOPMOST = new IntPtr(-2);
  const uint NOSIZE = 0x0001, NOMOVE = 0x0002, SHOW = 0x0040;
  const byte VK_MENU = 0x12;
  const uint KEYUP = 0x0002;

  public static List<long> Consoles() {
    var r = new List<long>();
    EnumWindows((h, l) => {
      if (!IsWindowVisible(h)) return true;
      var c = new StringBuilder(64);
      GetClassName(h, c, 64);
      var cls = c.ToString();
      if (cls == "CASCADIA_HOSTING_WINDOW_CLASS" || cls == "ConsoleWindowClass") r.Add(h.ToInt64());
      return true;
    }, IntPtr.Zero);
    return r;
  }

  public static bool Focus(long hwnd) {
    var h = new IntPtr(hwnd);
    for (int i = 0; i < 8; i++) {
      if (GetForegroundWindow() == h) return true;
      ShowWindow(h, 9);
      SetWindowPos(h, TOPMOST, 0, 0, 0, 0, NOSIZE | NOMOVE | SHOW);
      SetWindowPos(h, NOTOPMOST, 0, 0, 0, 0, NOSIZE | NOMOVE | SHOW);

      keybd_event(VK_MENU, 0, 0, UIntPtr.Zero);
      keybd_event(VK_MENU, 0, KEYUP, UIntPtr.Zero);

      uint fgPid;
      uint fgThread = GetWindowThreadProcessId(GetForegroundWindow(), out fgPid);
      uint myThread = GetCurrentThreadId();
      bool attached = fgThread != 0 && fgThread != myThread && AttachThreadInput(myThread, fgThread, true);
      SetForegroundWindow(h);
      BringWindowToTop(h);
      if (attached) AttachThreadInput(myThread, fgThread, false);

      Thread.Sleep(200);
    }
    return GetForegroundWindow() == h;
  }
}
'@

$before = @{}
foreach ($h in [DshLaunchWin]::Consoles()) { $before[$h] = $true }

$innerCmd = 'call "' + $CmdFile + '"'
$hasWt = $null -ne (Get-Command wt.exe -ErrorAction SilentlyContinue)
if ($hasWt) {
  $argLine = '/c start "" wt.exe -w new -d "' + $WorkDir + '" cmd /k ' + $innerCmd
} else {
  $argLine = '/c start "" cmd /k ' + $innerCmd
}
$launcher = Start-Process -FilePath 'cmd.exe' -ArgumentList $argLine -WindowStyle Hidden -PassThru
$null = $launcher.WaitForExit(5000)

$deadline = [Environment]::TickCount64 + $WaitMs
$newHwnd = [int64]0
while ([Environment]::TickCount64 -lt $deadline) {
  Start-Sleep -Milliseconds 250
  foreach ($h in [DshLaunchWin]::Consoles()) {
    if (-not $before.ContainsKey($h)) { $newHwnd = $h; break }
  }
  if ($newHwnd -ne 0) { break }
}

$focused = $false
if ($newHwnd -ne 0) {
  Start-Sleep -Milliseconds 700
  $focused = [DshLaunchWin]::Focus($newHwnd)
}

Write-Output ('hwnd=' + $newHwnd + ';focused=' + $focused)
"#;

/// 清掉过期的临时启动脚本（尽力而为，失败不影响主流程）。
fn prune_stale_scripts() {
    let dir = crate::paths::temp_dir();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.starts_with(SCRIPT_PREFIX) || !name.ends_with(".cmd") {
            continue;
        }
        let file = entry.path();
        let Ok(meta) = entry.metadata() else { continue };
        let mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        if now.saturating_sub(mtime) > SCRIPT_TTL_MS {
            let _ = std::fs::remove_file(file);
        }
    }
}

/// 在真实终端窗口里启动一条命令，并把窗口提到前台。
pub struct LaunchResult {
    pub ok: bool,
    pub mark: String,
    pub script: PathBuf,
    pub cwd: PathBuf,
    pub cwd_warning: Option<String>,
    pub hwnd: Option<i64>,
    pub focused: bool,
    pub ms: u64,
    pub error: Option<String>,
}

impl LaunchResult {
    pub fn to_json(&self) -> serde_json::Value {
        json!({
            "ok": self.ok,
            "mark": self.mark,
            "script": self.script.to_string_lossy(),
            "cwd": self.cwd.to_string_lossy(),
            "cwdWarning": self.cwd_warning,
            "hwnd": self.hwnd,
            "focused": self.focused,
            "ms": self.ms,
            "error": self.error,
        })
    }
}

pub async fn launch_in_terminal(
    workdir: &Path,
    inner: &str,
    title: &str,
    mark: Option<String>,
    wait_ms: Option<u64>,
) -> LaunchResult {
    let started = Instant::now();
    let token = mark.unwrap_or_else(|| make_mark("DSH"));
    let script_file = script_path_for(&token);

    // 目录预检：`cd /d` 失败时 cmd **不报错也不改变 cwd**，TUI 会在别的目录里
    // 静默启动 —— 用户以为打开的是那个项目，实际不是。
    let requested = if workdir.is_absolute() {
        workdir.to_path_buf()
    } else {
        std::env::current_dir().unwrap_or_else(|_| workdir.to_path_buf())
    };
    let mut cwd = requested.clone();
    let mut cwd_warning = None;
    match std::fs::metadata(&requested) {
        Ok(m) if m.is_dir() => {}
        Ok(_) => {
            cwd = std::env::current_dir().unwrap_or_else(|_| requested.clone());
            cwd_warning = Some(format!("目录不可用（不是目录）：{}，已回退到 {}", requested.display(), cwd.display()));
        }
        Err(e) => {
            cwd = std::env::current_dir().unwrap_or_else(|_| requested.clone());
            cwd_warning = Some(format!("目录不可用（{e}）：{}，已回退到 {}", requested.display(), cwd.display()));
        }
    }

    if let Err(e) = std::fs::write(&script_file, build_script(&token, &cwd, title, inner)) {
        return LaunchResult {
            ok: false,
            mark: token,
            script: script_file,
            cwd,
            cwd_warning,
            hwnd: None,
            focused: false,
            ms: started.elapsed().as_millis() as u64,
            error: Some(format!("无法写入启动脚本：{e}").chars().take(300).collect()),
        };
    }
    prune_stale_scripts();

    let script = LAUNCH_PS
        .replace("__WORKDIR__", &ps_quote(&cwd.to_string_lossy()))
        .replace("__CMDFILE__", &ps_quote(&script_file.to_string_lossy()))
        .replace(
            "__WAITMS__",
            &wait_ms.unwrap_or(DEFAULT_WAIT_MS).max(1000).to_string(),
        );

    let args: Vec<String> = vec![
        "-NoLogo".into(),
        "-NoProfile".into(),
        "-NonInteractive".into(),
        "-ExecutionPolicy".into(),
        "Bypass".into(),
        "-EncodedCommand".into(),
        encode_command(&script),
    ];
    let result: ProcResult = run_process_full(
        "powershell.exe",
        &args,
        None,
        PS_TIMEOUT_MS,
        None,
        false,
    )
    .await;

    let ms = started.elapsed().as_millis() as u64;
    let stdout = cap_tail(&result.stdout);
    let stderr = cap_tail(&result.stderr);

    let line = stdout
        .lines()
        .map(|s| s.trim())
        .find(|s| s.starts_with("hwnd="))
        .unwrap_or("");
    let hwnd = line
        .split(";")
        .find_map(|kv| kv.strip_prefix("hwnd="))
        .and_then(|v| v.trim().parse::<i64>().ok());
    let focused = line
        .split(";")
        .find_map(|kv| kv.strip_prefix("focused="))
        .map(|v| v.trim().eq_ignore_ascii_case("true"))
        .unwrap_or(false);

    if result.failed && hwnd.is_none() {
        // PowerShell 的 stderr 在 -EncodedCommand 下是 CLIXML，取可读片段就够了。
        let detail: String = stderr
            .split('<')
            .filter(|s| !s.contains('>'))
            .collect::<Vec<_>>()
            .join(" ")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        let detail: String = detail.chars().take(300).collect();
        return LaunchResult {
            ok: false,
            mark: token,
            script: script_file,
            cwd,
            cwd_warning,
            hwnd: None,
            focused: false,
            ms,
            error: Some(if detail.is_empty() {
                "终端窗口未能打开".to_string()
            } else {
                detail
            }),
        };
    }

    LaunchResult {
        ok: true,
        mark: token,
        script: script_file,
        cwd,
        cwd_warning,
        hwnd: hwnd.filter(|h| *h != 0),
        focused,
        ms,
        error: None,
    }
}

fn cap_tail(s: &str) -> String {
    if s.len() <= OUTPUT_CAP {
        s.to_string()
    } else {
        s[s.len() - OUTPUT_CAP..].to_string()
    }
}

/// 一次查询多个 mark 是否还有存活窗口。
///
/// 用户完全可以**自己关掉终端标签页**，插件无从得知。不核查的话登记表会变成
/// 一串幽灵条目。查询失败时保守地认为全部还活着。
pub async fn live_marks(marks: &[String]) -> Option<Vec<String>> {
    let tokens: Vec<String> = marks
        .iter()
        .filter(|m| !m.trim().is_empty())
        .map(|m| m.trim().to_string())
        .collect::<Vec<_>>();
    let mut uniq: Vec<String> = Vec::new();
    for t in tokens {
        if !uniq.contains(&t) {
            uniq.push(t);
        }
    }
    if uniq.is_empty() {
        return Some(Vec::new());
    }

    let list = uniq
        .iter()
        .map(|t| format!("'{}'", ps_quote(t)))
        .collect::<Vec<_>>()
        .join(",");
    let script = format!(
        r#"
$ErrorActionPreference = 'SilentlyContinue'
$marks = @({list})
$procs = Get-CimInstance Win32_Process -Filter "Name='cmd.exe'"
$alive = @()
foreach ($m in $marks) {{
  foreach ($p in $procs) {{
    if ($p.CommandLine -like ('*' + $m + '*')) {{ $alive += $m; break }}
  }}
}}
Write-Output ($alive -join ',')
"#
    );

    let args: Vec<String> = vec![
        "-NoLogo".into(),
        "-NoProfile".into(),
        "-NonInteractive".into(),
        "-EncodedCommand".into(),
        encode_command(&script),
    ];
    let result = run_process_full("powershell.exe", &args, None, 20_000, None, false).await;
    if result.failed && result.stdout.trim().is_empty() {
        return None;
    }
    Some(
        result
            .stdout
            .trim()
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect(),
    )
}

/// 按 mark 找回并结束由本启动器打开的窗口进程。
///
/// 只认 `Name='cmd.exe'`：mark 同时出现在 PowerShell 自己的命令行里，
/// 不限定进程名会把自己的查询进程也匹配进去。
pub async fn close_by_mark(mark: &str) -> (bool, Vec<u32>) {
    let token = mark.trim();
    if token.is_empty() {
        return (true, Vec::new());
    }

    let script = format!(
        r#"
$ErrorActionPreference = 'SilentlyContinue'
$hits = Get-CimInstance Win32_Process -Filter "Name='cmd.exe'" |
  Where-Object {{ $_.CommandLine -like '*{}*' }} |
  ForEach-Object {{ $_.ProcessId }}
$killed = @()
foreach ($p in $hits) {{
  & taskkill /F /T /PID $p *> $null
  if ($LASTEXITCODE -eq 0) {{ $killed += $p }}
}}
Write-Output ($killed -join ',')
"#,
        ps_quote(token)
    );

    let args: Vec<String> = vec![
        "-NoLogo".into(),
        "-NoProfile".into(),
        "-NonInteractive".into(),
        "-EncodedCommand".into(),
        encode_command(&script),
    ];
    let result = run_process_full("powershell.exe", &args, None, 20_000, None, false).await;

    // 启动脚本用完就删。需要重试：taskkill 返回后 Windows 释放文件句柄有延迟。
    for attempt in 0..5 {
        let p = script_path_for(token);
        match std::fs::remove_file(&p) {
            Ok(()) => break,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => break,
            Err(_) => {
                tokio::time::sleep(Duration::from_millis(200)).await;
                let _ = attempt;
            }
        }
    }

    let killed: Vec<u32> = result
        .stdout
        .trim()
        .split(',')
        .filter_map(|s| s.trim().parse::<u32>().ok())
        .filter(|n| *n > 0)
        .collect();
    (true, killed)
}
