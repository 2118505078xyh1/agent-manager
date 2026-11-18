//! 代理管理面板的配色、字体和小部件样式。
//!
//! 和任务监控保持同一套暗色观感；渲染器固定 tiny-skia，
//! 面板整体 20–40 MB 工作集，关掉窗口就归零。

use iced::widget::{button, container, text, text_input};
use iced::{
    Border, Color, Element, Font, Length, Shadow, Theme,
};

pub const BG: Color = Color::from_rgb8(22, 22, 26);
pub const CARD: Color = Color::from_rgb8(30, 30, 36);
pub const CARD2: Color = Color::from_rgb8(38, 38, 46);
pub const LINE: Color = Color::from_rgb8(56, 56, 66);
pub const TEXT: Color = Color::from_rgb8(232, 232, 236);
pub const DIM: Color = Color::from_rgb8(148, 148, 160);
pub const FAINT: Color = Color::from_rgb8(96, 96, 108);
pub const ACCENT: Color = Color::from_rgb8(76, 141, 255);
pub const ACCENT_DARK: Color = Color::from_rgb8(38, 70, 130);
pub const GOOD: Color = Color::from_rgb8(98, 202, 144);
pub const WARN: Color = Color::from_rgb8(238, 170, 84);
pub const BAD: Color = Color::from_rgb8(240, 104, 104);
pub const BAD_BG: Color = Color::from_rgb8(74, 34, 38);

pub fn font() -> Font {
    Font::with_name("Microsoft YaHei UI")
}

pub fn semibold() -> Font {
    Font {
        weight: iced::font::Weight::Semibold,
        ..font()
    }
}

pub fn mono() -> Font {
    Font::with_name("Consolas")
}




pub fn card_style(_: &Theme) -> container::Style {
    container::Style {
        background: Some(CARD.into()),
        border: Border {
            color: LINE,
            width: 1.0,
            radius: 10.0.into(),
        },
        ..container::Style::default()
    }
}

pub fn inset_style(_: &Theme) -> container::Style {
    container::Style {
        background: Some(CARD2.into()),
        border: Border {
            color: LINE,
            width: 1.0,
            radius: 6.0.into(),
        },
        ..container::Style::default()
    }
}

pub fn card<'a, M: 'a>(content: impl Into<Element<'a, M>>) -> Element<'a, M> {
    container(content)
        .width(Length::Fill)
        .padding(16)
        .style(card_style)
        .into()
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum BtnKind {
    Normal,
    Primary,
    Danger,
    Ghost,
}

pub fn btn<'a, M: Clone + 'a>(
    label: impl Into<String>,
    on_press: Option<M>,
    kind: BtnKind,
) -> Element<'a, M> {
    let text_color = match kind {
        BtnKind::Primary => Color::WHITE,
        BtnKind::Danger => Color::from_rgb8(255, 188, 188),
        _ => TEXT,
    };

    let style = move |_: &Theme, status: button::Status| {
        let hovered = matches!(
            status,
            button::Status::Hovered | button::Status::Pressed
        );
        let disabled = matches!(status, button::Status::Disabled);
        let (bg, border) = if disabled {
            // 禁用态压暗背景，别让它看着还能按（空分组/ResMon 的「结束」）。
            (Color::from_rgb8(28, 28, 33), Color::from_rgb8(48, 48, 56))
        } else {
            match kind {
                BtnKind::Primary => (
                    if hovered {
                        Color::from_rgb8(96, 158, 255)
                    } else {
                        ACCENT
                    },
                    ACCENT,
                ),
                BtnKind::Danger => (
                    if hovered { Color::from_rgb8(102, 42, 48) } else { BAD_BG },
                    Color::from_rgb8(120, 52, 58),
                ),
                BtnKind::Ghost => (
                    if hovered { CARD2 } else { Color::TRANSPARENT },
                    Color::TRANSPARENT,
                ),
                BtnKind::Normal => (
                    if hovered {
                        Color::from_rgb8(52, 52, 62)
                    } else {
                        CARD2
                    },
                    LINE,
                ),
            }
        };

        button::Style {
            background: Some(bg.into()),
            text_color,
            border: Border {
                color: border,
                width: 1.0,
                radius: 6.0.into(),
            },
            shadow: Shadow::default(),
            snap: true,
        }
    };

    let b = button(
        text(label.into())
            .size(12.0)
            .font(font())
            .color(if on_press.is_some() { text_color } else { FAINT }),
    )
    .padding([5.0, 11.0])
    .style(style);

    match on_press {
        Some(msg) => b.on_press(msg).into(),
        None => b.into(),
    }
}

pub fn input_style(_: &Theme, status: text_input::Status) -> text_input::Style {
    let focused = matches!(status, text_input::Status::Focused { .. });
    text_input::Style {
        background: CARD2.into(),
        border: Border {
            color: if focused { ACCENT } else { LINE },
            width: 1.0,
            radius: 6.0.into(),
        },
        icon: DIM,
        placeholder: FAINT,
        value: TEXT,
        selection: ACCENT_DARK,
    }
}

pub fn badge<'a, M: 'a>(label: impl Into<String>, color: Color) -> Element<'a, M> {
    container(
        iced::widget::text(label.into())
            .size(10.0)
            .font(mono())
            .color(color),
    )
    .padding([2.0, 6.0])
    .style(move |_: &Theme| container::Style {
        background: Some(Color { a: 0.10, ..color }.into()),
        border: Border {
            color,
            width: 1.0,
            radius: 4.0.into(),
        },
        ..container::Style::default()
    })
    .into()
}

pub fn ok_badge<'a, M: 'a>(label: impl Into<String>) -> Element<'a, M> {
    badge(label, GOOD)
}

pub fn warn_badge<'a, M: 'a>(label: impl Into<String>) -> Element<'a, M> {
    badge(label, WARN)
}

pub fn bad_badge<'a, M: 'a>(label: impl Into<String>) -> Element<'a, M> {
    badge(label, BAD)
}
