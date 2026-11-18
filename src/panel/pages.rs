//! 各个页签的视图。顺序与网页版一致。

use std::time::{SystemTime, UNIX_EPOCH};

use iced::widget::{column, container, row, text, text_input, Space};
use iced::{Alignment, Color, Element, Length, Theme};
use serde_json::Value;

use super::theme::*;
use super::*;

// ---------------------------------------------------------------- 通用小部件

fn section_head<'a>(
    title: impl Into<String>,
    sub: impl Into<String>,
    actions: Vec<Element<'a, Msg>>,
) -> Element<'a, Msg> {
    let mut r = row![text(title.into()).size(16.0).font(semibold()).color(TEXT)]
        .align_y(Alignment::Center)
        .spacing(8.0);
    let sub = sub.into();
    if !sub.is_empty() {
        r = r.push(text(sub).size(11.5).font(font()).color(DIM));
    }
    r = r.push(Space::new().width(Length::Fill));
    for a in actions {
        r = r.push(a);
    }
    r.into()
}



fn empty_box(label: impl Into<String>) -> Element<'static, Msg> {
    container(text(label.into()).size(12.0).font(font()).color(DIM))
        .padding(14)
        .width(Length::Fill)
        .style(inset_style)
        .into()
}

fn field<'a>(
    placeholder: &str,
    value: &str,
    width: f32,
    mono_font: bool,
    on_input: impl Fn(String) -> Msg + 'a,
) -> Element<'a, Msg> {
    let mut t = text_input(placeholder, value)
        .on_input(on_input)
        .padding([6.0, 9.0])
        .size(12.0)
        .width(Length::Fixed(width))
        .style(input_style);
    t = if mono_font { t.font(mono()) } else { t.font(font()) };
    t.into()
}



// ---------------------------------------------------------------- 更新与接入



/// 代理管理（软件 A）的「更新」页：只做版本检测 + 一键更新，
/// 不展示网关密钥 / 接入 / 模型写入（那些归鹊桥网关）。
pub fn agent_update_view(app: &App) -> Element<'_, Msg> {
    let agents = app
        .agents
        .as_ref()
        .and_then(|v| v.get("agents"))
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);

    let mut list = column![].spacing(0.0);
    if agents.is_empty() {
        let msg = if !app.agent_error.is_empty() {
            app.agent_error.clone()
        } else if app.agent_loading {
            "读取中…".to_string()
        } else {
            "没有 Agent 数据".to_string()
        };
        list = list.push(empty_box(msg));
    }
    for (i, agent) in agents.iter().enumerate() {
        list = list.push(update_agent_card(app, agent));
        if i + 1 < agents.len() {
            list = list.push(divider());
        }
    }

    let sub = if app.agents_updated_at > 0.0 {
        format!(
            "检测本机 Agent 的现版本与可更新版本 · 上次更新 {}",
            ago_text(app.agents_updated_at)
        )
    } else {
        "检测本机 Agent 的现版本与可更新版本，一键更新".to_string()
    };

    card(column![
        section_head(
            "Agent 更新",
            sub,
            vec![btn(
                "刷新",
                if app.agent_loading {
                    None
                } else {
                    Some(Msg::AgentRefresh)
                },
                BtnKind::Normal
            )],
        ),
        Space::new().height(Length::Fixed(4.0)),
        list,
    ]
    .spacing(8.0))
    .into()
}

fn update_agent_card<'a>(_app: &'a App, agent: &'a Value) -> Element<'a, Msg> {
    let id = jstr(agent, "id");
    let name = jstr(agent, "name");
    let installed = jbool(agent, "installed");
    let has_error = !jstr(agent, "error").is_empty();
    let updatable = jbool(agent, "updatable");
    let current = {
        let c = jstr(agent, "currentVersion");
        if c.is_empty() { jstr(agent, "version") } else { c }
    };
    let latest = jstr(agent, "latestVersion");
    let cur_text = if current.is_empty() { "—".to_string() } else { current.clone() };
    let latest_text = if latest.is_empty() {
        "—".to_string()
    } else if latest == current {
        "已是最新".to_string()
    } else {
        latest.clone()
    };

    // 一行足够：名字 + 状态徽章 + 现版本/最新版 + 更新按钮。
    // 原来拆成三行（标题 / 现版本 / 可更新版本）每张卡太高，更新页放不下几项。
    let mut one = row![
        text(if name.is_empty() { id.clone() } else { name })
            .size(13.0)
            .font(semibold())
            .color(TEXT),
        if installed {
            ok_badge("已安装")
        } else {
            warn_badge("未安装")
        },
    ]
    .spacing(10.0)
    .align_y(Alignment::Center);
    if updatable {
        one = one.push(warn_badge("可更新"));
    }
    if has_error {
        one = one.push(bad_badge("配置有误"));
    }
    one = one.push(
        text(format!("现 {cur_text}"))
            .size(11.5)
            .font(mono())
            .color(DIM),
    );
    one = one.push(
        text(format!("→ {latest_text}"))
            .size(11.5)
            .font(mono())
            .color(if updatable { ACCENT } else { DIM }),
    );
    one = one.push(Space::new().width(Length::Fill));
    // 已是最新就不显示按钮，避免一排按钮看着杂乱。
    // npm 系 agent：复制命令让用户自己到终端跑（后台跑会卡住且没进度）；
    // DSH：已是最新什么都不显示；有新版也只提示「请到 app 更新」，不给下载按钮。
    if id == "dsh" {
        if updatable {
            one = one.push(
                text("请到app更新")
                    .size(11.0)
                    .font(font())
                    .color(WARN),
            );
        }
    } else if updatable && installed {
        one = one.push(btn(
            "复制命令",
            Some(Msg::AgentCopyUpdate(id.clone())),
            BtnKind::Primary,
        ));
    }

    // 不再给每个 agent 套方框：外层只有一个大卡片，行之间用一条细横线分隔，
    // 这样每行的「更新」按钮和顶部的「刷新」能严格右对齐。
    one.padding([10.0, 0.0]).into()
}

fn divider<'a>() -> Element<'a, Msg> {
    container(Space::new().width(Length::Fill).height(Length::Fixed(1.0)))
        .style(|_: &Theme| container::Style {
            background: Some(LINE.into()),
            ..container::Style::default()
        })
        .into()
}







// ---------------------------------------------------------------- 对话总览

fn agent_accent(agent: &str) -> Color {
    match agent {
        "claude" => Color::from_rgb8(214, 129, 92),
        "codex" => Color::from_rgb8(90, 190, 140),
        "opencode" => ACCENT,
        "pi" => Color::from_rgb8(167, 139, 250),
        "dsh" => Color::from_rgb8(86, 156, 240),
        "argent" => Color::from_rgb8(232, 180, 92),
        "zcode" => Color::from_rgb8(120, 160, 255),
        "kimicli" => Color::from_rgb8(210, 120, 200),
        "omp" => Color::from_rgb8(120, 200, 160),
        _ => DIM,
    }
}

const CONV_AGENTS: [(&str, &str); 10] = [
    ("all", "全部"),
    ("pi", "Pi"),
    ("claude", "Claude"),
    ("codex", "Codex"),
    ("opencode", "OpenCode"),
    ("dsh", "DSH"),
    ("argent", "Argent"),
    ("zcode", "ZCode"),
    ("kimicli", "Kimi CLI"),
    ("omp", "omp"),
];

const CONV_SHOW_MAX: usize = 300;
/// 每页显示的会话条数：分页后不用在列表里放上下滚动条。
const CONV_PAGE_SIZE: usize = 8;

pub fn conversations_view(app: &App) -> Element<'_, Msg> {
    let data = app.conv_data.as_ref();
    let counts = data.and_then(|v| v.get("counts"));
    let total = data
        .and_then(|v| v.get("total"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let count_of = |id: &str| -> u64 {
        if id == "all" {
            total
        } else {
            counts
                .and_then(|c| c.get(id))
                .and_then(Value::as_u64)
                .unwrap_or(0)
        }
    };

    let mut filters = row![].spacing(5.0).align_y(Alignment::Center);
    for (id, label) in CONV_AGENTS {
        // 没有会话的 agent 不占按钮（「全部」始终显示）。
        if id != "all" && count_of(id) == 0 {
            continue;
        }
        let selected = app.conv_agent == id;
        filters = filters.push(btn(
            format!("{label} {}", count_of(id)),
            Some(Msg::ConvAgent(id)),
            if selected {
                BtnKind::Primary
            } else {
                BtnKind::Normal
            },
        ));
    }
    filters = filters.push(Space::new().width(Length::Fill));
    filters = filters.push(field(
        "搜索标题 / 目录 / 模型 / ID",
        &app.conv_q,
        330.0,
        false,
        Msg::ConvFilter,
    ));

    let mut head = column![
        section_head(
            "对话总览",
            "有会话的本地 agent（omp / Pi / Claude / Codex / OpenCode / DSH / Argent / ZCode / Kimi）",
            vec![btn(
                "刷新",
                if app.conv_loading {
                    None
                } else {
                    Some(Msg::ConvRefresh)
                },
                BtnKind::Normal,
            )],
        ),
        Space::new().height(Length::Fixed(2.0)),
        filters,
    ]
    .spacing(8.0);

    if !app.conv_error.is_empty() {
        head = head.push(
            text(format!("读取失败：{}", app.conv_error))
                .size(11.0)
                .font(font())
                .color(BAD),
        );
    }
    if app.conv_loading && data.is_some() {
        head = head.push(text("刷新中…").size(10.5).font(font()).color(WARN));
    }
    if app.conv_busy {
        head = head.push(text("操作中…").size(10.5).font(font()).color(WARN));
    } else if let Some(errs) = data
        .and_then(|v| v.get("errors"))
        .and_then(Value::as_object)
    {
        for (agent, msg) in errs {
            if let Some(msg) = msg.as_str().filter(|s| !s.is_empty()) {
                head = head.push(
                    text(format!("{agent}：{msg}"))
                        .size(10.5)
                        .font(font())
                        .color(WARN),
                );
            }
        }
    }

    let sessions = data
        .and_then(|v| v.get("sessions"))
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let needle = app.conv_q.trim().to_lowercase();
    let mut matched: Vec<&Value> = sessions
        .iter()
        .filter(|s| {
            let agent = jstr(s, "agent");
            app.conv_agent == "all" || agent == app.conv_agent
        })
        .filter(|s| {
            if needle.is_empty() {
                return true;
            }
            ["title", "cwd", "model", "id"]
                .iter()
                .any(|k| jstr(s, k).to_lowercase().contains(&needle))
        })
        .collect();
    let matched_count = matched.len();
    matched.truncate(CONV_SHOW_MAX);
    let page_count = matched.len().div_ceil(CONV_PAGE_SIZE).max(1);
    let page = app.conv_page.min(page_count - 1);
    let page_items: Vec<&Value> = matched
        .iter()
        .skip(page * CONV_PAGE_SIZE)
        .take(CONV_PAGE_SIZE)
        .copied()
        .collect();

    let mut list = column![].spacing(3.0);
    if page_items.is_empty() {
        list = list.push(empty_box(if app.conv_loading {
            "加载中…"
        } else if data.is_none() {
            "还没读取"
        } else {
            "没有找到会话"
        }));
    }

    for s in page_items {
        let agent = jstr(s, "agent");
        let path = jstr(s, "path");
        let id = jstr(s, "id");
        let cwd = jstr(s, "cwd");
        let model = jstr(s, "model");
        let when = jstr(s, "when");
        let agent_name = {
            let n = jstr(s, "agentName");
            if n.is_empty() {
                agent.clone()
            } else {
                n
            }
        };
        let title = {
            let t = jstr(s, "title");
            if t.is_empty() {
                id.clone()
            } else {
                t
            }
        };

        let open = (!app.conv_busy).then(|| Msg::ConvCopyOpen {
            agent: agent.clone(),
            path: path.clone(),
            id: id.clone(),
        });
        // ZCode 会话在它自己的 sqlite 里，这边不能删。
        let delete = (!app.conv_busy && agent != "zcode").then(|| Msg::ConvDelete {
            agent: agent.clone(),
            path,
            id,
            title: title.clone(),
        });

        list = list.push(
            row![
                column![
                    row![
                        badge(agent_name, agent_accent(&agent)),
                        text(truncate(&title, 60))
                            .size(12.0)
                            .font(semibold())
                            .color(TEXT)
                            .wrapping(iced::widget::text::Wrapping::None)
                            .width(Length::Fill),
                    ]
                    .spacing(8.0)
                    .align_y(Alignment::Center),
                    row![
                        text(if cwd.is_empty() {
                            "(无目录)".to_string()
                        } else {
                            truncate(&cwd, 90)
                        })
                        .size(9.5)
                        .font(mono())
                        .color(FAINT),
                        text(if model.is_empty() {
                            "—".to_string()
                        } else {
                            model
                        })
                        .size(9.5)
                        .font(font())
                        .color(DIM),
                        text(fmt_size(jnum(s, "size")))
                            .size(9.5)
                            .font(font())
                            .color(DIM),
                        text(when).size(9.5).font(mono()).color(DIM),
                    ]
                    .spacing(10.0),
                ]
                .width(Length::Fill)
                .spacing(2.0),
                btn("复制打开", open, BtnKind::Normal),
                btn("删除", delete, BtnKind::Danger),
            ]
            .spacing(6.0)
            .align_y(Alignment::Center),
        );
    }

    let sub = if matched_count > CONV_SHOW_MAX {
        format!(
            "第 {}/{} 页 · 显示前 {CONV_SHOW_MAX} 条 · 共 {matched_count} 条匹配",
            page + 1,
            page_count
        )
    } else {
        format!(
            "第 {}/{} 页 · {matched_count} 条匹配 · 共 {total} 条会话",
            page + 1,
            page_count
        )
    };
    let pager = row![
        btn(
            "上一页",
            if page > 0 { Some(Msg::ConvPagePrev) } else { None },
            BtnKind::Normal
        ),
        text(format!("{}/{}", page + 1, page_count))
            .size(11.0)
            .font(mono())
            .color(DIM),
        btn(
            "下一页",
            if page + 1 < page_count {
                Some(Msg::ConvPageNext)
            } else {
                None
            },
            BtnKind::Normal
        ),
    ]
    .spacing(8.0)
    .align_y(Alignment::Center);
    let list_card = card(column![
        section_head("会话", sub, vec![pager.into()]),
        list,
    ]
    .spacing(8.0));

    column![card(head), list_card].spacing(12.0).into()
}

// ---------------------------------------------------------------- 账号渠道

/// 缓存时间 → 「刚刚 / N 分钟前 / N 小时前 / N 天前」。
fn ago_text(ms: f64) -> String {
    if ms <= 0.0 {
        return "未知".to_string();
    }
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as f64;
    let age = ((now - ms) / 1000.0).max(0.0);
    if age < 60.0 {
        "刚刚".to_string()
    } else if age < 3600.0 {
        format!("{} 分钟前", (age / 60.0) as i64)
    } else if age < 86_400.0 {
        format!("{} 小时前", (age / 3600.0) as i64)
    } else {
        format!("{} 天前", (age / 86_400.0) as i64)
    }
}

fn board<'a>(app: &'a App, key: &'static str, title: &'static str, accent: Color) -> Element<'a, Msg> {
    let c = app
        .ww_report
        .as_ref()
        .and_then(|r| r.get(key))
        .cloned()
        .unwrap_or(Value::Null);
    let ok = jbool(&c, "ok");
    let stale = jbool(&c, "stale");
    let items = c
        .get("items")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);

    let mut head = row![
        text(title).size(14.0).font(semibold()).color(TEXT),
        if stale {
            Element::from(warn_badge("数据较旧"))
        } else {
            Element::from(Space::new().height(Length::Fixed(0.0)))
        },
        if !c.is_null() && !ok {
            Element::from(bad_badge("取不到"))
        } else {
            Element::from(Space::new().height(Length::Fixed(0.0)))
        },
    ]
    .spacing(6.0)
    .align_y(Alignment::Center);

    if !c.is_null() {
        let days = jnum(&c, "days") as i64;
        head = head.push(Space::new().width(Length::Fill));
        head = head.push(
            text(if days <= 1 {
                "最近一天".to_string()
            } else {
                format!("最近 {days} 天")
            })
            .size(10.0)
            .font(font())
            .color(FAINT),
        );
    }

    let mut list = column![].spacing(3.0);
    if ok && !items.is_empty() {
        for (i, it) in items.iter().take(5).enumerate() {
            list = list.push(
                row![
                    text(format!("{}", i + 1))
                        .size(11.0)
                        .font(mono())
                        .color(accent)
                        .width(Length::Fixed(24.0)),
                    text({
                        let n = jstr(it, "name");
                        if n.is_empty() {
                            jstr(it, "model")
                        } else {
                            n
                        }
                    })
                    .size(12.0)
                    .font(font())
                    .color(TEXT)
                    .width(Length::Fill),
                    text(human_tokens(jnum(it, "tokens")))
                        .size(11.0)
                        .font(mono())
                        .color(DIM),
                ]
                .spacing(6.0)
                .align_y(Alignment::Center),
            );
        }
    } else if !jstr(&c, "error").is_empty() {
        list = list.push(empty_box(jstr(&c, "error")));
    } else {
        list = list.push(empty_box(if app.ww_loading {
            "读取中…"
        } else {
            "没有数据"
        }));
    }

    card(column![head, Space::new().height(Length::Fixed(4.0)), list].spacing(8.0))
}

pub fn worldwide_view(app: &App) -> Element<'_, Msg> {
    let updated = app
        .ww_report
        .as_ref()
        .map(|r| {
            ["models", "opencode", "apps"]
                .iter()
                .map(|k| r.get(*k).map(|c| jnum(c, "updatedAt")).unwrap_or(0.0))
                .fold(0.0, f64::max)
        })
        .unwrap_or(0.0);
    let sub = if updated > 0.0 {
        format!("模型看最近一天 · App 看最近一天 · 每天自动更新一次 · 上次更新 {}", ago_text(updated))
    } else {
        "模型看最近一天 · App 看最近一天 · 每天自动更新一次".to_string()
    };

    let header = card(section_head(
        "全球热门",
        sub,
        vec![btn(
            "刷新",
            if app.ww_busy {
                None
            } else {
                Some(Msg::WwLoad(true))
            },
            BtnKind::Primary
        )],
    ));

    // 左 OpenRouter（最近一天），右 opencode（本周），下面 App Top 5。
    let models = row![
        board(
            app,
            "models",
            "OpenRouter 模型 · Top 5",
            Color::from_rgb8(76, 141, 255),
        ),
        board(
            app,
            "opencode",
            "opencode 模型 · Top 5",
            Color::from_rgb8(98, 202, 144),
        ),
    ]
    .spacing(12.0)
    .width(Length::Fill);
    let apps = board(
        app,
        "apps",
        "OpenRouter App · Top 5",
        Color::from_rgb8(232, 180, 92),
    );

    column![header, models, apps].spacing(12.0).into()
}

