//! 代理管理的面板入口。
//!
//! 只有这一个窗口：无 daemon、无托盘。面板直接调用库函数，
//! 数据写在 `%LOCALAPPDATA%\AgentManager\`。

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() -> iced::Result {
    agent_manager::panel::run()
}
