//! Agent Manager 的原生 iced 面板。
//!
//! 独立进程 `agent-manager-panel.exe`：启动时读 `port.txt`（或 `--url`），
//! 通过本机 HTTP `/api/*` 操作守护进程。关掉面板不影响网关，
//! `agent-manager.exe` 仍然只有二十多 MB。

mod api;
mod pages;
mod theme;

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use iced::widget::{button, column, container, row, scrollable, stack, text, Space};
use iced::window;
use iced::{
    Alignment, Border, Color, Element, Length, Subscription, Task, Theme,
};
use serde_json::{json, Value};

use api::Job;
use theme::*;

// ---------------------------------------------------------------- 页签

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Chat,
    Agents,
    Worldwide,
}

impl Tab {
    /// 代理管理的三个页签。
    fn all() -> Vec<Tab> {
        vec![Tab::Chat, Tab::Agents, Tab::Worldwide]
    }

    fn label(self) -> &'static str {
        match self {
            Tab::Chat => "对话",
            Tab::Agents => "更新",
            Tab::Worldwide => "全球热门",
        }
    }
}

/// 初始页签：默认「对话」；环境变量 `ROOST_TAB` 可指定（截图 / 调试用）。
fn initial_tab() -> Tab {
    match std::env::var("ROOST_TAB").ok().as_deref() {
        Some("agents") | Some("update") => Tab::Agents,
        Some("worldwide") => Tab::Worldwide,
        _ => Tab::Chat,
    }
}

// ---------------------------------------------------------------- 弹窗

#[derive(Debug, Clone)]
pub enum ConfirmAction {
    DeleteConversation {
        agent: String,
        path: String,
        id: String,
        title: String,
    },
}

struct Toast {
    text: String,
    err: bool,
    until: Instant,
}

// ---------------------------------------------------------------- 状态

pub struct App {
    pub tab: Tab,
    toast: Option<Toast>,
    pub confirm: Option<ConfirmAction>,

    // 更新
    pub agents: Option<Value>,
    /// 版本检测结果最近一次抓取时刻（Unix 毫秒），给更新页显示「上次更新」。
    pub agents_updated_at: f64,
    pub agent_loading: bool,
    pub agent_error: String,
    pub agent_drafts: HashMap<String, (String, String)>,
    pub agent_busy: bool,
    pub agent_updating: bool,

    // 对话总览（omp / Pi / Claude / Codex / OpenCode / DSH / Argent / ZCode / Kimi）
    pub conv_data: Option<Value>,
    pub conv_loading: bool,
    pub conv_error: String,
    pub conv_agent: &'static str,
    pub conv_q: String,
    pub conv_busy: bool,
    /// 会话列表分页（每页 CONV_PAGE_SIZE 条），避免整列滚动条。
    pub conv_page: usize,

    // 全球热门
    pub ww_report: Option<Value>,
    pub ww_loading: bool,
    pub ww_busy: bool,
}

impl App {
    fn new() -> App {
        App {
            tab: initial_tab(),
            toast: None,
            confirm: None,
            agents: None,
            agents_updated_at: 0.0,
            agent_loading: false,
            agent_error: String::new(),
            agent_drafts: HashMap::new(),
            agent_busy: false,
            agent_updating: false,
            conv_data: None,
            conv_loading: false,
            conv_error: String::new(),
            conv_agent: "all",
            conv_q: String::new(),
            conv_busy: false,
            conv_page: 0,
            ww_report: None,
            ww_loading: false,
            ww_busy: false,
        }
    }

    fn toast(&self) -> Option<&Toast> {
        self.toast.as_ref().filter(|t| Instant::now() < t.until)
    }

    fn notify(&mut self, text: impl Into<String>, err: bool) {
        self.toast = Some(Toast {
            text: text.into(),
            err,
            until: Instant::now() + Duration::from_secs(3),
        });
    }

}

// ---------------------------------------------------------------- 消息

#[derive(Debug, Clone)]
pub enum Msg {
    Tick,
    Tab(Tab),
    Load(Job),
    Loaded(Job, Result<Value, String>),
    Toast(String, bool),
    DismissToast,
    CopyText(String),
    Noop,
    ConfirmYes,
    ConfirmNo,

    // Agent
    AgentRefresh,
    AgentModel(String, String),
    AgentEffort(String, String),
    AgentWrite(String),
    AgentUpdate(String),
    /// 把自更新命令放进剪贴板，用户自己粘到终端执行（DSH 不走这里）。
    AgentCopyUpdate(String),

    // 对话总览（各 agent）
    ConvRefresh,
    ConvAgent(&'static str),
    ConvFilter(String),
    ConvPagePrev,
    ConvPageNext,
    ConvOpen {
        agent: String,
        path: String,
        id: String,
    },
    /// 复制打开会话的命令，用户自己粘到终端执行。
    ConvCopyOpen {
        agent: String,
        path: String,
        id: String,
    },
    ConvDelete {
        agent: String,
        path: String,
        id: String,
        title: String,
    },

    // 全球热门
    WwLoad(bool),
}

// ---------------------------------------------------------------- 小工具

fn jstr(v: &Value, key: &str) -> String {
    v.get(key).and_then(Value::as_str).unwrap_or("").to_string()
}

fn jbool(v: &Value, key: &str) -> bool {
    v.get(key).and_then(Value::as_bool).unwrap_or(false)
}

fn jnum(v: &Value, key: &str) -> f64 {
    v.get(key).and_then(Value::as_f64).unwrap_or(0.0)
}

fn job_ok(v: &Value) -> bool {
    v.get("ok").and_then(Value::as_bool).unwrap_or(false)
}

fn human_tokens(n: f64) -> String {
    let v = n.max(0.0);
    if v >= 1e12 {
        format!("{:.2}T", v / 1e12)
    } else if v >= 1e9 {
        format!("{:.2}B", v / 1e9)
    } else if v >= 1e6 {
        format!("{:.2}M", v / 1e6)
    } else if v >= 1e3 {
        format!("{:.1}K", v / 1e3)
    } else {
        format!("{}", v.round() as i64)
    }
}

fn fmt_size(bytes: f64) -> String {
    if bytes <= 0.0 {
        return "—".to_string();
    }
    let mb = bytes / 1048576.0;
    if mb >= 1.0 {
        format!("{mb:.1} MB")
    } else {
        format!("{} KB", (bytes / 1024.0).max(1.0).round() as i64)
    }
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        let t: String = s.chars().take(n.saturating_sub(1)).collect();
        format!("{t}…")
    }
}

fn fetch(_app: &App, job: Job) -> Task<Msg> {
    // 没有 daemon：面板直接调底层函数。
    let tag = job.clone();
    Task::perform(async move { job.run_local().await }, move |res| {
        Msg::Loaded(tag, res)
    })
}



// ---------------------------------------------------------------- 加载入口

/// Agent 版本检测结果缓存 24h：和全球模型一样「每天自动刷新一次」。
/// 下载不了新版时也能显示上次的检测结果，不必每次开面板都联网跑 CLI。
const AGENTS_CACHE_TTL_MS: u64 = 24 * 60 * 60 * 1000;

fn agents_cache_file() -> PathBuf {
    crate::paths::state_dir().join("agents-cache.json")
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn agents_cache_load() -> Option<(u64, Value)> {
    let doc = crate::atomic::read_value(&agents_cache_file())?;
    // schema 变了（新增 omp 行）就当缓存失效，避免老缓存缺行。
    if doc.get("schema").and_then(Value::as_u64) != Some(5) {
        return None;
    }
    let at = doc.get("at")?.as_u64()?;
    if now_ms().saturating_sub(at) >= AGENTS_CACHE_TTL_MS {
        return None;
    }
    Some((at, doc.get("agents").cloned()?))
}

fn agents_cache_save(v: &Value) -> u64 {
    let at = now_ms();
    let doc = json!({ "schema": 5, "at": at, "agents": v });
    let _ = crate::atomic::write_json_atomic(&agents_cache_file(), &doc, 0);
    at
}

fn fill_agent_drafts(app: &mut App, v: &Value) {
    if let Some(list) = v.get("agents").and_then(Value::as_array) {
        for a in list {
            let id = jstr(a, "id");
            app.agent_drafts
                .entry(id)
                .or_insert_with(|| (jstr(a, "model"), jstr(a, "effort")));
        }
    }
}

fn load_agents(app: &mut App, force: bool) -> Task<Msg> {
    if app.agent_loading {
        return Task::none();
    }
    if !force {
        if let Some((at, v)) = agents_cache_load() {
            app.agents_updated_at = at as f64;
            fill_agent_drafts(app, &v);
            app.agents = Some(v);
            app.agent_error.clear();
            return Task::none();
        }
    }
    app.agent_loading = true;
    fetch(app, Job::Agents)
}

fn load_conversations(app: &mut App) -> Task<Msg> {
    if app.conv_loading {
        return Task::none();
    }
    app.conv_loading = true;
    app.conv_error.clear();
    fetch(app, Job::Conversations)
}

fn load_worldwide(app: &mut App, refresh: bool) -> Task<Msg> {
    app.ww_loading = true;
    app.ww_busy = refresh;
    fetch(app, Job::Worldwide(refresh))
}

fn tab_tasks(app: &mut App) -> Task<Msg> {
    match app.tab {
        Tab::Chat => {
            if app.conv_data.is_none() {
                load_conversations(app)
            } else {
                Task::none()
            }
        }
        Tab::Agents => {
            if app.agents.is_none() {
                load_agents(app, false)
            } else {
                Task::none()
            }
        }
        Tab::Worldwide => {
            if app.ww_report.is_none() {
                load_worldwide(app, false)
            } else {
                Task::none()
            }
        }
    }
}

// ---------------------------------------------------------------- update

pub fn update(app: &mut App, msg: Msg) -> Task<Msg> {
    match msg {
        Msg::Tick => {
            if let Some(t) = &app.toast {
                if Instant::now() >= t.until {
                    app.toast = None;
                }
            }
            Task::none()
        }
        Msg::Tab(tab) => {
            app.tab = tab;
            tab_tasks(app)
        }
        Msg::Load(job) => fetch(app, job),
        Msg::Toast(text, err) => {
            app.notify(text, err);
            Task::none()
        }
        Msg::DismissToast => {
            app.toast = None;
            Task::none()
        }
        Msg::CopyText(text) => {
            app.notify("已复制", false);
            iced::clipboard::write(text)
        }
        Msg::Noop => Task::none(),
        Msg::ConfirmYes => match app.confirm.take() {
            Some(ConfirmAction::DeleteConversation { agent, path, id, .. }) => {
                app.conv_busy = true;
                fetch(app, Job::ConversationDelete { agent, path, id })
            }
            None => Task::none(),
        },
        Msg::ConfirmNo => {
            app.confirm = None;
            Task::none()
        }

        Msg::AgentRefresh => {
            app.agents = None;
            app.agent_error.clear();
            load_agents(app, true)
        }
        Msg::AgentModel(id, value) => {
            if let Some(draft) = app.agent_drafts.get_mut(&id) {
                draft.0 = value;
            }
            Task::none()
        }
        Msg::AgentEffort(id, value) => {
            if let Some(draft) = app.agent_drafts.get_mut(&id) {
                draft.1 = value;
            }
            Task::none()
        }
        Msg::AgentWrite(id) => {
            let Some((model, effort)) = app.agent_drafts.get(&id).cloned() else {
                return Task::none();
            };
            let model = model.trim().to_string();
            if model.is_empty() {
                app.notify("模型名不能为空", true);
                return Task::none();
            }
            app.agent_busy = true;
            let effort = {
                let e = effort.trim().to_string();
                if e.is_empty() {
                    None
                } else {
                    Some(e)
                }
            };
            fetch(
                app,
                Job::SetAgent {
                    id,
                    model,
                    effort,
                },
            )
        }
        // 只有 DSH 还走这里（打开官方下载页）；其它 agent 的按钮改成复制命令。
        Msg::AgentUpdate(id) => fetch(app, Job::AgentsUpdate { id }),
        Msg::AgentCopyUpdate(id) => {
            let cmd = crate::agents::update_command_text(&id);
            if cmd.is_empty() {
                app.notify(format!("找不到 {id} 的更新命令"), true);
                Task::none()
            } else {
                app.notify(
                    format!("已复制更新命令：{cmd}（粘到终端执行，完事点顶部刷新）"),
                    false,
                );
                iced::clipboard::write(cmd)
            }
        }
        Msg::ConvRefresh => {
            app.conv_data = None;
            load_conversations(app)
        }
        Msg::ConvAgent(agent) => {
            app.conv_agent = agent;
            app.conv_page = 0;
            Task::none()
        }
        Msg::ConvFilter(v) => {
            app.conv_q = v;
            app.conv_page = 0;
            Task::none()
        }
        Msg::ConvPagePrev => {
            app.conv_page = app.conv_page.saturating_sub(1);
            Task::none()
        }
        Msg::ConvPageNext => {
            app.conv_page = app.conv_page.saturating_add(1);
            Task::none()
        }
        Msg::ConvOpen { agent, path, id } => {
            app.conv_busy = true;
            fetch(app, Job::ConversationOpen { agent, path, id })
        }
        Msg::ConvCopyOpen { agent, path, id } => {
            match crate::conversations::open_command_text(&agent, &path, &id) {
                Ok((command, cwd)) => {
                    app.notify(
                        format!("已复制打开命令：{command}（工作目录：{cwd}）"),
                        false,
                    );
                    iced::clipboard::write(command)
                }
                Err(e) => {
                    app.notify(format!("复制打开命令失败：{e}"), true);
                    Task::none()
                }
            }
        }
        Msg::ConvDelete {
            agent,
            path,
            id,
            title,
        } => {
            app.confirm = Some(ConfirmAction::DeleteConversation {
                agent,
                path,
                id,
                title,
            });
            Task::none()
        }

        Msg::WwLoad(refresh) => load_worldwide(app, refresh),

        Msg::Loaded(job, result) => handle_loaded(app, job, result),
    }
}

fn handle_loaded(app: &mut App, job: Job, result: Result<Value, String>) -> Task<Msg> {
    match job {
        Job::Agents => {
            app.agent_loading = false;
            match result {
                Ok(v) => {
                    app.agents_updated_at = agents_cache_save(&v) as f64;
                    fill_agent_drafts(app, &v);
                    app.agents = Some(v);
                    app.agent_error.clear();
                }
                Err(e) => app.agent_error = e,
            }
            Task::none()
        }
        Job::AgentsUpdate { id } => {
            app.agent_updating = false;
            match result {
                Ok(v) if job_ok(&v) => {
                    let msg = jstr(&v, "message");
                    if !msg.is_empty() {
                        app.notify(msg, false);
                    } else {
                        let ver = {
                            let l = jstr(&v, "latestVersion");
                            if l.is_empty() {
                                jstr(&v, "version")
                            } else {
                                l
                            }
                        };
                        app.notify(
                            if ver.is_empty() {
                                format!("{id} 已更新")
                            } else {
                                format!("{id} 已更新到 {ver}")
                            },
                            false,
                        );
                    }
                    app.agents = None;
                    load_agents(app, true)
                }
                Ok(v) => {
                    app.notify(format!("{id} 更新失败：{}", api::error_text(&v, 200)), true);
                    Task::none()
                }
                Err(e) => {
                    app.notify(format!("{id} 更新失败：{e}"), true);
                    Task::none()
                }
            }
        }
        Job::SetAgent { id, .. } => {
            app.agent_busy = false;
            match result {
                Ok(v) if job_ok(&v) => {
                    app.notify(format!("已写入 {id}"), false);
                    app.agents = None;
                    load_agents(app, true)
                }
                Ok(v) => {
                    app.notify(api::error_text(&v, 200), true);
                    Task::none()
                }
                Err(e) => {
                    app.notify(e, true);
                    Task::none()
                }
            }
        }
        Job::Worldwide(_) => {
            app.ww_loading = false;
            app.ww_busy = false;
            match result {
                Ok(v) => app.ww_report = Some(v),
                Err(e) => app.notify(e, true),
            }
            Task::none()
        }
        Job::Conversations => {
            app.conv_loading = false;
            match result {
                Ok(v) => {
                    app.conv_data = Some(v);
                    app.conv_error.clear();
                }
                Err(e) => {
                    app.conv_error = e;
                }
            }
            Task::none()
        }
        Job::ConversationOpen { .. } => {
            app.conv_busy = false;
            match result {
                Ok(v) if job_ok(&v) => {
                    let note = jstr(&v, "note");
                    if note.is_empty() {
                        app.notify(format!("已打开会话：{}", jstr(&v, "command")), false);
                    } else {
                        app.notify(note, false);
                    }
                }
                Ok(v) => {
                    let err = jstr(&v, "error");
                    let msg = if err.is_empty() {
                        api::error_text(&v, 200)
                    } else {
                        err
                    };
                    app.notify(format!("打开失败：{msg}"), true);
                }
                Err(e) => app.notify(format!("打开失败：{e}"), true),
            }
            Task::none()
        }
        Job::ConversationDelete { .. } => {
            app.conv_busy = false;
            match result {
                Ok(v) if job_ok(&v) => {
                    let method = jstr(&v, "method");
                    let where_ = jstr(&v, "where");
                    app.notify(
                        if method == "recycle" {
                            "已移入系统回收站".to_string()
                        } else if !where_.is_empty() {
                            format!("已移到 {where_}")
                        } else {
                            "已删除".to_string()
                        },
                        false,
                    );
                    app.conv_data = None;
                    load_conversations(app)
                }
                Ok(v) => {
                    app.notify(format!("删除失败：{}", api::error_text(&v, 200)), true);
                    Task::none()
                }
                Err(e) => {
                    app.notify(format!("删除失败：{e}"), true);
                    Task::none()
                }
            }
        }
    }
}

// ---------------------------------------------------------------- 视图

pub fn view(app: &App) -> Element<'_, Msg> {
    let (app_name, app_sub) = ("代理管理", "Agent Manager");
    let top = row![
        row![
            // 品牌色竖条：和图标的蓝/青呼应，强化身份辨识
            container(Space::new().width(Length::Fixed(4.0)).height(Length::Fixed(22.0)))
                .style(|_: &Theme| container::Style {
                    background: Some(ACCENT.into()),
                    border: Border {
                        radius: 2.0.into(),
                        ..Border::default()
                    },
                    ..container::Style::default()
                }),
            Space::new().width(Length::Fixed(4.0)),
            text(app_name).size(19.0).font(semibold()).color(TEXT),
            text(app_sub)
                .size(10.5)
                .font(font())
                .color(DIM),
        ]
        .spacing(8.0)
        .align_y(Alignment::Center),
        Space::new().width(Length::Fill),
    ]
    .align_y(Alignment::Center)
    .padding([0.0, 4.0]);

    let tabs = Tab::all().iter().fold(
        row![].spacing(4.0).align_y(Alignment::Center),
        |r, tab| {
            let selected = *tab == app.tab;
            r.push(tab_button(*tab, selected))
        },
    );

    let toast = match app.toast() {
        Some(t) => {
            let c = if t.err { BAD } else { GOOD };
            Element::from(
                container(
                    row![
                        text(t.text.clone()).size(12.0).font(font()).color(c),
                        Space::new().width(Length::Fill),
                        btn("×", Some(Msg::DismissToast), BtnKind::Ghost),
                    ]
                    .align_y(Alignment::Center),
                )
                .padding([6.0, 10.0])
                .width(Length::Fill)
                .style(move |_: &Theme| container::Style {
                    background: Some(CARD2.into()),
                    border: Border {
                        color: c,
                        width: 1.0,
                        radius: 6.0.into(),
                    },
                    ..container::Style::default()
                }),
            )
        }
        None => Element::from(Space::new().height(Length::Fixed(0.0))),
    };

    let body: Element<'_, Msg> = match app.tab {
        Tab::Chat => pages::conversations_view(app),
        Tab::Agents => pages::agent_update_view(app),
        Tab::Worldwide => pages::worldwide_view(app),
    };

    let content = column![
        top,
        tabs,
        Space::new().height(Length::Fixed(8.0)),
        toast,
        Space::new().height(Length::Fixed(10.0)),
        body,
    ]
    .padding(16)
    .spacing(6)
    .width(Length::Fill);

    let scroll = scrollable(container(content).width(Length::Fill))
        .width(Length::Fill)
        .height(Length::Fill);
    // 三页都不需要滚动条：对话已分页，更新/全球热门一屏放得下。
    // 隐藏轨道但保留滚轮能力。
    let scroll = scroll.style(|theme, status| {
        let mut s = scrollable::default(theme, status);
        for rail in [&mut s.vertical_rail, &mut s.horizontal_rail] {
            rail.background = None;
            rail.border = Border::default();
            rail.scroller.background = Color::TRANSPARENT.into();
            rail.scroller.border = Border::default();
        }
        s
    });
    let shell = container(scroll)
        .width(Length::Fill)
    .height(Length::Fill)
    .padding(0)
    .style(|_: &Theme| container::Style {
        background: Some(BG.into()),
        ..container::Style::default()
    });

    if app.confirm.is_none() {
        return shell.into();
    }

    let catcher = iced::widget::mouse_area(
        Space::new()
            .width(Length::Fill)
            .height(Length::Fill),
    )
    .on_press(Msg::Noop);

    stack([
        Element::from(shell),
        Element::from(catcher),
        confirm_overlay(app),
    ])
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}

fn confirm_overlay(app: &App) -> Element<'_, Msg> {
    let (title, detail) = match &app.confirm {
        Some(ConfirmAction::DeleteConversation { title, .. }) => (
            format!("删除对话「{}」？", truncate(title, 40)),
            "会话文件会移进系统回收站；对应的 Agent 将看不到这段对话。".to_string(),
        ),
        None => ("确认".to_string(), String::new()),
    };

    let card = container(
        column![
            text(title).size(16.0).font(semibold()).color(TEXT),
            text(detail).size(12.0).font(font()).color(DIM),
            Space::new().height(Length::Fixed(6.0)),
            row![
                btn("取消", Some(Msg::ConfirmNo), BtnKind::Normal),
                btn("确认", Some(Msg::ConfirmYes), BtnKind::Danger),
            ]
            .spacing(8.0),
        ]
        .spacing(8.0),
    )
    .width(Length::Fixed(420.0))
    .padding(18)
    .style(card_style);

    container(card)
        .width(Length::Fill)
        .height(Length::Fill)
        .center_x(Length::Fill)
        .center_y(Length::Fill)
        .style(|_: &Theme| container::Style {
            background: Some(Color::from_rgba(0.0, 0.0, 0.0, 0.55).into()),
            ..container::Style::default()
        })
        .into()
}

fn tab_button(tab: Tab, selected: bool) -> Element<'static, Msg> {
    button(text(tab.label()).size(12.5).font(font()).color(if selected {
        Color::WHITE
    } else {
        DIM
    }))
    .padding([6.0, 12.0])
    .style(move |_: &Theme, status: button::Status| {
        let hovered = matches!(
            status,
            button::Status::Hovered | button::Status::Pressed
        );
        button::Style {
            background: Some(
                if selected {
                    // 选中：低饱和蓝底 + 白色字，更柔和
                    Color::from_rgba(ACCENT.r, ACCENT.g, ACCENT.b, 0.18)
                } else if hovered {
                    CARD2
                } else {
                    Color::TRANSPARENT
                }
                .into(),
            ),
            text_color: if selected { Color::WHITE } else { DIM },
            border: Border {
                color: if selected {
                    ACCENT
                } else if hovered {
                    LINE
                } else {
                    Color::TRANSPARENT
                },
                // 选中页签底部一条更亮的强调线
                width: if selected { 2.0 } else { 1.0 },
                radius: 6.0.into(),
            },
            shadow: iced::Shadow::default(),
            snap: true,
        }
    })
    .on_press(Msg::Tab(tab))
    .into()
}

// ---------------------------------------------------------------- 订阅 / 启动

fn subscription(_app: &App) -> Subscription<Msg> {
    iced::time::every(Duration::from_secs(1)).map(|_| Msg::Tick)
}

pub fn run() -> iced::Result {
    // 没有 daemon：面板直接调底层函数。启动时用一次性轻量 runtime 跑一遍
    // 短对话自动清理（无常驻服务）。
    std::thread::spawn(|| {
        let rt = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(rt) => rt,
            Err(_) => return,
        };
        rt.block_on(async {
            let r = crate::conversations::auto_purge(5, 86_400.0).await;
            let n = r.get("purged").and_then(serde_json::Value::as_u64).unwrap_or(0);
            if n > 0 {
                crate::app::log_line(&format!("自动清理：移入回收站 {n} 条短对话"));
            }
        });
    });
    // 三页内容保持紧凑：960×600（逻辑像素），最小 800×480。
    let (win_w, win_h, min_w, min_h) = (960.0, 600.0, 800.0, 480.0);
    let icon_bytes: &[u8] = include_bytes!("../../icons/agent-manager.rgba");
    let window = window::Settings {
        size: iced::Size::new(win_w, win_h),
        min_size: Some(iced::Size::new(min_w, min_h)),
        position: window::Position::Centered,
        icon: window::icon::from_rgba(icon_bytes.to_vec(), 32, 32).ok(),
        ..window::Settings::default()
    };

    let title: &'static str = "代理管理";

    iced::application(
        move || {
            let mut app = App::new();
            let task = tab_tasks(&mut app);
            (app, task)
        },
        update,
        view,
    )
    .title(title)
    .window(window)
    .theme(|_: &App| Theme::Dark)
    .default_font(font())
    .subscription(subscription)
    .run()
}
