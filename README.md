# 代理管理（Agent Manager）

本机 AI CLI 的原生面板：**对话总览 / 更新 / 全球热门** 三页。
Rust + iced（tiny-skia，CPU 渲染），无 WebView、无 daemon、无托盘；
面板进程直接调用库函数，数据写在 `%LOCALAPPDATA%\AgentManager\`。

## 页面

| 页面 | 能力 |
| --- | --- |
| **对话** | omp / Pi / Claude / Codex / OpenCode / DSH / Argent / ZCode / Kimi CLI 的会话总览：标题、条数、最近活动、搜索、分页；**没有会话的 agent 不显示筛选按钮**。打开 = 起终端跑对应 CLI 的 resume 命令；删除进系统回收站。 |
| **更新** | 检测各 agent 的安装与版本（现版本 / 可更新版本）。npm 系 agent 给「复制命令」，粘到终端自己跑；DSH 是 NSIS 安装包，只提示去 app 更新。omp 的模型读 `~/.omp/agent/config.yml`。 |
| **全球热门** | 最近一天 / 本周的模型榜与 App 榜（OpenRouter + opencode），每天自动更新一次，可手动刷新。 |

## 构建与部署

需要 Rust 1.88+ 与 MSVC（无 C 依赖，不需要 QuickJS）：

```
build.bat
```

`build.bat` 做 `cargo build --release`，然后把 `agent-manager-gui.exe` 复制到
`%LOCALAPPDATA%\Programs\AgentManager\`。桌面快捷方式用
`powershell -ExecutionPolicy Bypass -File make-shortcut.ps1` 创建。

## 结构

- `src/bin/agent-manager-gui.rs` —— 唯一入口。
- `src/panel/` —— iced 面板：`mod.rs`（状态 / 消息 / 启动）、`pages.rs`（三页视图）、`theme.rs`（配色与控件）、`api.rs`（后台任务，进程内直调）。
- `src/agents.rs` —— agent 注册表：配置文件与模型形状（JSON/TOML/YAML/嵌套 YAML）、版本检测、自更新命令。
- `src/conversations.rs` + `src/transcripts.rs` —— 各 agent 的会话扫描（每来源一个 `SessionSource`）。
- `src/worldwide.rs` + `src/worldwide_opencode.rs` —— 模型/App 榜与 24h 缓存。
- `src/paths.rs` / `src/atomic.rs` / `src/app.rs` —— 路径、原子写、日志。

## 安全与约束

- 只读会话文件；删除走系统回收站，路径先规范化再校验前缀。
- 会话读取带增量缓存（文件大小 + 偏移），面板每分钟刷新也不会重复读盘。
- GUI 程序不 `eprintln!`（会拉控制台窗口），日志写 `%LOCALAPPDATA%\AgentManager\agentmanager.log`。
- `#![windows_subsystem = "windows"]` 不能丢。
- 面板不用 wgpu：最小窗口 260 MB；tiny-skia 同窗口约 20 MB。

## 许可

MIT，见 `LICENSE`。
