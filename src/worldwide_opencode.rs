//! opencode 统计页（`opencode.ai/stats`）的定向解析。
//!
//! `tokens` 原始值是「十亿」单位：页面把 38162 渲染成 38T，所以这里乘
//! `OPENCODE_TOKEN_SCALE`。页面是 SolidJS SSR，数据以 `$R[n]=` 内嵌在
//! `<script>` 里，没有可用的 JSON 端点。

use serde_json::{json, Value};

/// 统计页地址。
pub const STATS_URL: &str = "https://opencode.ai/stats";

/// opencode `tokens` 的乘数（十亿）。
const OPENCODE_TOKEN_SCALE: u64 = 1_000_000_000;

/// opencode 的榜是「本周」。
pub const DAYS: usize = 7;

// ============================================================ opencode 解析

/// 从 `opencode.ai/stats` 的 HTML 里取 `leaderboard.weekly`。
///
/// **抛错而不是返回空数组** —— 「页面结构变了」和「这一周没人用模型」在界面上
/// 长得一模一样，但前者要人管、后者不用。
pub fn parse_opencode_weekly(html: &str, limit: usize) -> Result<Vec<Value>, String> {
    let body = js_array_after(html, "weekly")
        .ok_or_else(|| "opencode 统计页里没有 leaderboard.weekly（页面结构变了？）".to_string())?;

    let mut rows: Vec<Value> = Vec::new();
    for chunk in js_objects(&body) {
        let Some(model) = js_string(&chunk, "model") else {
            continue;
        };
        let Some(tokens) = js_number(&chunk, "tokens") else {
            continue;
        };
        rows.push(json!({
            "model": model,
            "name": js_string(&chunk, "name").unwrap_or_else(|| model.clone()),
            "provider": js_string(&chunk, "provider").unwrap_or_default(),
            // **必须乘 OPENCODE_TOKEN_SCALE**（它的单位是 10⁹，见文件头）。
            "tokens": (tokens.round() as u64).saturating_mul(OPENCODE_TOKEN_SCALE),
        }));
    }

    rows.sort_by(|a, b| {
        let ta = a["tokens"].as_u64().unwrap_or(0);
        let tb = b["tokens"].as_u64().unwrap_or(0);
        tb.cmp(&ta).then_with(|| {
            let ma = a["model"].as_str().unwrap_or("");
            let mb = b["model"].as_str().unwrap_or("");
            ma.cmp(mb)
        })
    });
    rows.truncate(limit);
    Ok(rows)
}

/// `key:$R[n]=[ ... ]` 里那个数组的**内容**（不含外层方括号）；找不到是 `None`。
fn js_array_after(text: &str, key: &str) -> Option<String> {
    // 正则：`\bkey:\$R\[\d+\]=\[`
    let needle = format!("{key}:$R[");
    let mut search_from = 0usize;
    while let Some(pos) = text[search_from..].find(&needle) {
        let abs = search_from + pos;
        // 确认前面是个词边界（`\b`）
        let before_ok = abs == 0
            || !text[..abs]
                .chars()
                .next_back()
                .map(|c| c.is_alphanumeric() || c == '_')
                .unwrap_or(false);
        if before_ok {
            // 跳过 `$R[` 与数字与 `]=`
            let mut i = abs + needle.len();
            while i < text.len() && text.as_bytes()[i].is_ascii_digit() {
                i += 1;
            }
            if text[i..].starts_with("]=[") {
                let bracket = abs + needle.len() + (i - (abs + needle.len())) + 2;
                return balanced(text, bracket);
            }
        }
        search_from = abs + needle.len();
    }
    None
}

/// 从 `text[start] === '['` 扫到配对的 `]`，返回中间的内容。
///
/// 必须认字符串态：模型名是人写的，里面可能有 `]`、`{`。反斜杠转义也要处理。
fn balanced(text: &str, start: usize) -> Option<String> {
    let bytes = text.as_bytes();
    if start >= bytes.len() || bytes[start] != b'[' {
        return None;
    }
    let mut depth = 0i32;
    let mut quote: Option<u8> = None;
    let mut i = start;
    while i < bytes.len() {
        let b = bytes[i];
        if let Some(q) = quote {
            if b == b'\\' {
                i += 2;
                continue;
            }
            if b == q {
                quote = None;
            }
        } else if b == b'"' || b == b'\'' || b == b'`' {
            quote = Some(b);
        } else if b == b'[' {
            depth += 1;
        } else if b == b']' {
            depth -= 1;
            if depth == 0 {
                return Some(text[start + 1..i].to_string());
            }
        }
        i += 1;
    }
    None
}

/// 把 `$R[1]={...},$R[2]={...}` 切成一个个对象的**内容**。同样靠字符串态。
fn js_objects(text: &str) -> Vec<String> {
    let bytes = text.as_bytes();
    let mut found = Vec::new();
    let mut index = 0usize;
    while index < bytes.len() {
        if bytes[index] != b'{' {
            index += 1;
            continue;
        }
        let mut depth = 0i32;
        let mut quote: Option<u8> = None;
        let mut cursor = index;
        while cursor < bytes.len() {
            let b = bytes[cursor];
            if let Some(q) = quote {
                if b == b'\\' {
                    cursor += 2;
                    continue;
                }
                if b == q {
                    quote = None;
                }
            } else if b == b'"' || b == b'\'' || b == b'`' {
                quote = Some(b);
            } else if b == b'{' {
                depth += 1;
            } else if b == b'}' {
                depth -= 1;
                if depth == 0 {
                    found.push(text[index + 1..cursor].to_string());
                    break;
                }
            }
            cursor += 1;
        }
        index = cursor + 1;
    }
    found
}

/// `key:"..."` 的值（转义按 JSON 解）；没有或不是字符串是 `None`。
fn js_string(text: &str, key: &str) -> Option<String> {
    let needle = format!("{key}:\"");
    let pos = text.find(&needle)?;
    let after = pos + needle.len();
    let bytes = text.as_bytes();
    let mut out = String::new();
    let mut i = after;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'\\' && i + 1 < bytes.len() {
            out.push('\\');
            out.push(bytes[i + 1] as char);
            i += 2;
            continue;
        }
        if b == b'"' {
            // 用 serde_json 解一遍转义
            let raw = format!("\"{out}\"");
            return serde_json::from_str::<String>(&raw).ok();
        }
        out.push(b as char);
        i += 1;
    }
    None
}

/// `key:123` 的值；没有、是 `null` 或不是数字都是 `None`。
///
/// `change` 那类字段实测会是 `null` —— 所以「不是数字」必须和「没有」一样归成
/// `None`，不能当成 0（0 会被当成「没变化」显示出去）。
fn js_number(text: &str, key: &str) -> Option<f64> {
    let needle = format!("{key}:");
    let pos = text.find(&needle)?;
    let rest = &text[pos + needle.len()..];
    let bytes = rest.as_bytes();
    let mut end = 0usize;
    if end < bytes.len() && bytes[end] == b'-' {
        end += 1;
    }
    let start_digits = end;
    while end < bytes.len() && bytes[end].is_ascii_digit() {
        end += 1;
    }
    if end == start_digits {
        return None;
    }
    // 小数部分
    if end < bytes.len() && bytes[end] == b'.' {
        let dot = end;
        end += 1;
        let frac_start = end;
        while end < bytes.len() && bytes[end].is_ascii_digit() {
            end += 1;
        }
        if end == frac_start {
            end = dot;
        }
    }
    rest[..end].parse::<f64>().ok().filter(|v| v.is_finite())
}
