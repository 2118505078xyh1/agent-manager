//! 子进程：统一超时与「连子树一起收」。
//!
//! 关键点（都是原 Node 版踩出来的）：
//!   * `stdio: ["ignore","pipe","pipe"]` —— 子进程会看 fd 0 是不是终端，
//!     继承管道会让 TUI 静默挂死。
//!   * 超时用 `taskkill /F /T` 连子树收 —— 只杀直接子进程会漏孤儿。
//!   * 去掉代理变量 —— 上游只打回环，继承来的死代理会让每条请求 Connection error。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use tokio::io::{AsyncReadExt, BufReader};
use tokio::process::Command;

#[derive(Debug, Clone)]
pub struct ProcResult {
    pub failed: bool,
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub timed_out: bool,
}

impl ProcResult {
    pub fn failed_with(stderr: impl Into<String>) -> Self {
        Self {
            failed: true,
            code: None,
            stdout: String::new(),
            stderr: stderr.into(),
            timed_out: false,
        }
    }
}

/// 去掉代理变量。
pub fn clean_env(extra: &HashMap<String, String>) -> Vec<(String, String)> {
    let drop: [&str; 8] = [
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "ALL_PROXY",
        "NO_PROXY",
        "http_proxy",
        "https_proxy",
        "all_proxy",
        "no_proxy",
    ];
    let mut out: Vec<(String, String)> = std::env::vars().collect();
    out.retain(|(k, _)| !drop.contains(&k.as_str()));
    for (k, v) in extra {
        out.retain(|(ek, _)| ek != k);
        out.push((k.clone(), v.clone()));
    }
    out
}

/// Windows 上 child.kill() 只杀直接子进程；用 taskkill /T 连子树收。
pub fn kill_tree(pid: u32) {
    let mut c = std::process::Command::new("taskkill");
    c.args(["/F", "/T", "/PID", &pid.to_string()])
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    no_window_std(&mut c);
    let _ = c.status();
}

/// 服务是 GUI 子系统进程：它 spawn 的任何控制台程序（powershell / git / taskkill…）
/// 不指定 `CREATE_NO_WINDOW` 就会**弹出一个黑框再消失**。这里统一压掉。
#[cfg(windows)]
fn no_window(cmd: &mut Command) {
    // CREATE_NO_WINDOW
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    cmd.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
fn no_window(_cmd: &mut Command) {}

#[cfg(windows)]
fn no_window_std(cmd: &mut std::process::Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    cmd.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
fn no_window_std(_cmd: &mut std::process::Command) {}

/// 一次性命令：拿输出就走。超时连子树一起收。
pub async fn run_process(
    file: &str,
    args: &[String],
    cwd: Option<&Path>,
    timeout_ms: u64,
    env_extra: Option<&HashMap<String, String>>,
) -> ProcResult {
    run_process_full(file, args, cwd, timeout_ms, env_extra, false).await
}

/// 命令构造器：链式指定 cwd / 超时 / 环境变量，最后 `run()`。
///
/// 比 `run_process` 的一长串位置参数好读 —— 调用点往往只关心其中一两个。
///
/// 自己拥有 file / args 的所有权：调用点常常传 `bin.to_string_lossy()` 这种
/// 临时值，借用会当场报 "temporary value dropped while borrowed"。
pub struct CmdSpec {
    file: String,
    args: Vec<String>,
    cwd: Option<PathBuf>,
    timeout_ms: u64,
    env_extra: HashMap<String, String>,
    shell: bool,
}

impl CmdSpec {
    pub fn new(file: &str, args: &[&str]) -> CmdSpec {
        CmdSpec {
            file: file.to_string(),
            args: args.iter().map(|s| s.to_string()).collect(),
            cwd: None,
            timeout_ms: 60_000,
            env_extra: HashMap::new(),
            shell: false,
        }
    }

    /// 传 `String` / `&String` 版本。
    pub fn owned(file: &str, args: &[String]) -> CmdSpec {
        CmdSpec {
            file: file.to_string(),
            args: args.to_vec(),
            cwd: None,
            timeout_ms: 60_000,
            env_extra: HashMap::new(),
            shell: false,
        }
    }

    pub fn cwd(mut self, dir: &str) -> CmdSpec {
        self.cwd = Some(PathBuf::from(dir));
        self
    }

    pub fn timeout(mut self, ms: u64) -> CmdSpec {
        self.timeout_ms = ms;
        self
    }

    pub fn env(mut self, k: &str, v: &str) -> CmdSpec {
        self.env_extra.insert(k.to_string(), v.to_string());
        self
    }

    pub fn shell(mut self, on: bool) -> CmdSpec {
        self.shell = on;
        self
    }

    pub async fn run(self) -> Result<ProcResult, String> {
        let env = if self.env_extra.is_empty() {
            None
        } else {
            Some(&self.env_extra)
        };
        Ok(run_process_full(
            &self.file,
            &self.args,
            self.cwd.as_deref(),
            self.timeout_ms,
            env,
            self.shell,
        )
        .await)
    }
}

/// 完整版：`shell` 走 `cmd /C`。
pub async fn run_process_full(
    file: &str,
    args: &[String],
    cwd: Option<&Path>,
    timeout_ms: u64,
    env_extra: Option<&HashMap<String, String>>,
    shell: bool,
) -> ProcResult {
    let mut cmd = if shell {
        // `.cmd` / `.bat` 必须经 `cmd /C`，但**不能**自己拼带引号的一整行：
        // Rust 的 Command 在 Windows 上会把含空格的单个参数再包一层引号并把
        // 内部 `"` 转义成 `\"`，cmd.exe 不认 `\"`，于是把 `"C:\...\opencode.cmd"`
        // 当成文件名（引号算进去了）报「不是内部或外部命令」。
        // 正确做法：把 file 和每个参数作为独立 arg 传给 `cmd /C`，由 Rust 负责引号。
        let mut c = Command::new("cmd");
        c.arg("/C").arg(file).args(args);
        c
    } else {
        let mut c = Command::new(file);
        c.args(args);
        c
    };

    cmd.kill_on_drop(true);
    cmd.stdin(Stdio::null());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    cmd.envs(clean_env(env_extra.unwrap_or(&HashMap::new())));
    no_window(&mut cmd);

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return ProcResult::failed_with(e.to_string()),
    };

    let stdout_handle = child.stdout.take();
    let stderr_handle = child.stderr.take();
    let out_task = tokio::spawn(async move {
        let mut buf = String::new();
        if let Some(mut s) = stdout_handle {
            let mut bytes = Vec::new();
            let _ = s.read_to_end(&mut bytes).await;
            buf = String::from_utf8_lossy(&bytes).to_string();
        }
        buf
    });
    let err_task = tokio::spawn(async move {
        let mut buf = String::new();
        if let Some(mut s) = stderr_handle {
            let mut bytes = Vec::new();
            let _ = s.read_to_end(&mut bytes).await;
            buf = String::from_utf8_lossy(&bytes).to_string();
        }
        buf
    });

    let timeout = Duration::from_millis(timeout_ms);
    let status = tokio::time::timeout(timeout, child.wait()).await;

    match status {
        Err(_) => {
            if let Some(pid) = child.id() {
                kill_tree(pid);
            }
            let _ = child.start_kill();
            let stdout = out_task.await.unwrap_or_default();
            let stderr = err_task.await.unwrap_or_default();
            ProcResult {
                failed: true,
                code: None,
                stdout,
                stderr: format!("{stderr}\n超时"),
                timed_out: true,
            }
        }
        Ok(Ok(s)) => {
            let stdout = out_task.await.unwrap_or_default();
            let stderr = err_task.await.unwrap_or_default();
            ProcResult {
                failed: !s.success(),
                code: s.code(),
                stdout,
                stderr,
                timed_out: false,
            }
        }
        Ok(Err(e)) => {
            let stdout = out_task.await.unwrap_or_default();
            let stderr = err_task.await.unwrap_or_default();
            ProcResult {
                failed: true,
                code: None,
                stdout,
                stderr: format!("{stderr}{e}"),
                timed_out: false,
            }
        }
    }
}

/// 读一个子进程的 stdout（用于需要流式输出的场景）。
pub async fn read_all(mut reader: BufReader<tokio::process::ChildStdout>) -> String {
    let mut out = String::new();
    let mut buf = Vec::new();
    let _ = reader.read_to_end(&mut buf).await;
    out.push_str(&String::from_utf8_lossy(&buf));
    out
}
