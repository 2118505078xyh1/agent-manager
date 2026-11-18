//! OFM 目录层：找端口 → 找 key → 解析模型。
//!
//! 端口判据必须是三层，不能只看 TCP：
//!   8326 同时是 DeepSeek Harness 桌面端自己的服务端口，只查「连得上」会把一个
//!   完全不相干的服务判成 OFM（症状：界面说 OFM 在跑但拒绝这个 key，其实它没启动）。

use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::time::Duration;

use serde_json::{json, Value};


pub const STANDALONE_BASE: &str = "http://127.0.0.1:18900/v1";
pub const PLUGIN_BASE: &str = "http://127.0.0.1:8326/v1";

/// OFM 桌面端 CHANNEL_PROVIDERS 的 13 个渠道，顺序照抄 —— 它决定目录里的聚簇顺序。
pub const CHANNELS: [(&str, &str, &str); 13] = [
    ("workbuddy", "WorkBuddy", "WB"),
    ("buddy", "CodeBuddy", "Buddy"),
    ("codearts", "华为云码道", "CodeArts"),
    ("qoder", "Qoder", "Qoder"),
    ("qodercn", "Qoder CN", "QoderCN"),
    ("trae", "TRAE", "Trae"),
    ("cline", "Cline", "Cline"),
    ("loomy", "Loomy", "Loomy"),
    ("raccoon", "Raccoon", "Raccoon"),
    ("minimax", "MiniMax Code", "MiniMax"),
    ("zcode", "ZCode", "ZCode"),
    ("gemini", "Gemini", "Gemini"),
    ("lobsterai", "LobsterAI", "LobsterAI"),
];

pub fn channel_of(id: &str) -> Option<(&'static str, &'static str)> {
    let head = id.split('/').next().unwrap_or("");
    CHANNELS
        .iter()
        .find(|c| c.0 == head)
        .map(|c| (c.1, c.2))
}

pub fn channel_tag(id: &str) -> String {
    if id.starts_with("combo:") {
        return "链路".to_string();
    }
    channel_of(id).map(|c| c.1.to_string()).unwrap_or_default()
}

fn positive(v: Option<&Value>) -> u64 {
    match v {
        Some(Value::Number(n)) => {
            let f = n.as_f64().unwrap_or(0.0);
            if f.is_finite() && f > 0.0 {
                f.floor() as u64
            } else {
                0
            }
        }
        _ => 0,
    }
}

/// 名字里的价格戳、版本戳、渠道词清洗 —— 让列表里那一行读得懂。
pub fn short_model_name(raw: &str, id: &str, keep_tag: bool) -> String {
    let mut name = if raw.trim().is_empty() {
        id.rsplit('/').next().unwrap_or(id).to_string()
    } else {
        raw.trim().to_string()
    };
    if name.is_empty() {
        name = id.to_string();
    }
    // -1017 版本戳
    name = strip_version_stamp(&name);
    // x2.5 倍率
    name = strip_multiplier(&name);
    // FREE / 免费
    name = strip_free(&name);
    name = name.split_whitespace().collect::<Vec<_>>().join(" ");

    let tag = if keep_tag { channel_tag(id) } else { String::new() };
    if !tag.is_empty() && !name.to_lowercase().starts_with(&tag.to_lowercase()) {
        name = format!("{tag} · {name}");
    }
    if name.is_empty() { id.to_string() } else { name }
}

/// `[\s_-]+10\d\d(?!\d)` —— 版本戳。Rust 没有 lookahead，手写扫描。
fn strip_version_stamp(s: &str) -> String {
    let bytes: Vec<char> = s.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < bytes.len() {
        let is_sep = i > 0 && (bytes[i - 1].is_whitespace() || bytes[i - 1] == '_' || bytes[i - 1] == '-');
        if is_sep && bytes[i] == '1' && i + 3 < bytes.len() && bytes[i + 1].is_ascii_digit()
            && bytes[i + 2].is_ascii_digit() && bytes[i + 3].is_ascii_digit()
        {
            // (?!\d)：下一位不能是数字
            let next_is_digit = bytes.get(i + 4).is_some_and(|c| c.is_ascii_digit());
            if !next_is_digit {
                // 连同前面的分隔符一起吃掉
                while out.ends_with([' ', '\t', '_', '-']) {
                    out.pop();
                }
                i += 4;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    out
}

/// `\s*[x×]\s*\d+(?:\.\d+)?\s*` —— 倍率戳。
fn strip_multiplier(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if (c == 'x' || c == 'X' || c == '×') && i > 0 && (chars[i - 1].is_whitespace() || chars[i - 1] == ' ') {
            let mut j = i + 1;
            while j < chars.len() && chars[j].is_whitespace() {
                j += 1;
            }
            let start = j;
            while j < chars.len() && chars[j].is_ascii_digit() {
                j += 1;
            }
            if j > start {
                if j < chars.len() && chars[j] == '.' {
                    j += 1;
                    let frac = j;
                    while j < chars.len() && chars[j].is_ascii_digit() {
                        j += 1;
                    }
                    if j == frac {
                        j = frac - 1;
                    }
                }
                // 连同前面的空白一起吃掉
                while out.ends_with(' ') {
                    out.pop();
                }
                i = j;
                continue;
            }
        }
        out.push(c);
        i += 1;
    }
    out
}

/// `\b(?:FREE|免费)\b` —— 价格词。
fn strip_free(s: &str) -> String {
    let mut out = String::new();
    for word in s.split_inclusive(char::is_whitespace) {
        let trimmed = word.trim();
        let is_free = trimmed.eq_ignore_ascii_case("FREE") || trimmed == "免费";
        if !is_free {
            out.push_str(word);
        }
    }
    out
}

pub fn price_of(raw: &str) -> Option<f64> {
    let lower = raw.to_lowercase();
    let chars: Vec<char> = lower.chars().collect();
    for i in 0..chars.len() {
        if chars[i] != 'x' && chars[i] != '×' {
            continue;
        }
        let mut j = i + 1;
        while j < chars.len() && chars[j].is_whitespace() {
            j += 1;
        }
        let start = j;
        while j < chars.len() && chars[j].is_ascii_digit() {
            j += 1;
        }
        if j == start {
            continue;
        }
        let mut num = lower[start..j].to_string();
        if j < chars.len() && chars[j] == '.' {
            let mut k = j + 1;
            let fs = k;
            while k < chars.len() && chars[k].is_ascii_digit() {
                k += 1;
            }
            if k > fs {
                num.push('.');
                num.push_str(&lower[fs..k]);
            }
        }
        return num.parse::<f64>().ok();
    }
    None
}

fn rank_of(id: &str, meta: &Value) -> i64 {
    if id.starts_with("combo:") {
        return 20000; // 降级链永远排最后
    }
    let order = CHANNELS
        .iter()
        .position(|c| id.split('/').next() == Some(c.0))
        .map(|i| i as i64)
        .unwrap_or(90);
    order * 100 + positive(meta.get("sort_order")) as i64
}

/// GET /v1/models → 只留「写进 Pi 要用」的字段。缺字段留 0，不猜窗口。
pub fn parse_models(doc: &Value, keep_tag: bool) -> Vec<Value> {
    let raw = match doc {
        Value::Array(a) => Some(a.clone()),
        Value::Object(o) => o
            .get("data")
            .and_then(|v| v.as_array())
            .cloned()
            .or_else(|| o.get("models").and_then(|v| v.as_array()).cloned()),
        _ => None,
    };
    let Some(raw) = raw else { return Vec::new() };

    let mut out = Vec::new();
    for entry in raw {
        let id = match &entry {
            Value::String(s) => s.clone(),
            Value::Object(o) => o.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string(),
            _ => String::new(),
        };
        if id.trim().is_empty() {
            continue;
        }
        let meta = if entry.is_object() { entry.clone() } else { json!({}) };

        let efforts: Vec<String> = meta
            .get("openai_efforts")
            .or_else(|| meta.get("x_ofm_efforts"))
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(|s| s.trim().to_string()))
                    .filter(|s| !s.is_empty())
                    .collect()
            })
            .unwrap_or_default();
        let default_effort = meta
            .get("openai_effort_default")
            .or_else(|| meta.get("x_ofm_effort_default"))
            .and_then(|v| v.as_str())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());

        let raw_name = meta.get("name").and_then(|v| v.as_str()).unwrap_or("");
        let trimmed_id = id.trim().to_string();
        let ch = channel_of(&trimmed_id).map(|c| c.0.to_string()).unwrap_or_default();

        out.push(json!({
            "id": trimmed_id,
            "name": short_model_name(raw_name, &trimmed_id, keep_tag),
            "channel": ch,
            "contextWindow": positive(meta.get("context_window")),
            "maxTokens": positive(meta.get("max_tokens")),
            "supportsImages": meta.get("vision").and_then(|v| v.as_bool()).unwrap_or(false),
            "multiplier": price_of(raw_name).unwrap_or(0.0),
            "efforts": efforts,
            "defaultEffort": default_effort,
            "thinkingAlwaysOn": meta.get("canDisableThinking").and_then(|v| v.as_bool()) == Some(false),
            "rank": rank_of(&trimmed_id, &meta),
        }));
    }
    out
}

fn tcp_open(url: &str, ms: u64) -> bool {
    let parsed = match url::parse_base(url) {
        Some(p) => p,
        None => return false,
    };
    let addrs: Vec<SocketAddr> = match (parsed.host.as_str(), parsed.port).to_socket_addrs() {
        Ok(it) => it.collect(),
        Err(_) => return false,
    };
    let timeout = Duration::from_millis(ms);
    for addr in addrs {
        if TcpStream::connect_timeout(&addr, timeout).is_ok() {
            return true;
        }
    }
    false
}

mod url {
    /// 极简 URL 拆解：只要 host / port。
    pub struct Parts {
        pub host: String,
        pub port: u16,
    }
    pub fn parse_base(raw: &str) -> Option<Parts> {
        let rest = raw
            .strip_prefix("http://")
            .or_else(|| raw.strip_prefix("https://"))?;
        let authority = rest.split('/').next()?;
        let (host, port) = if let Some(stripped) = authority.strip_prefix('[') {
            // IPv6
            let (h, p) = stripped.split_once(']')?;
            let port = p
                .strip_prefix(':')
                .and_then(|x| x.parse().ok())
                .unwrap_or(if raw.starts_with("https") { 443 } else { 80 });
            (h.to_string(), port)
        } else {
            match authority.rsplit_once(':') {
                Some((h, p)) if p.chars().all(|c| c.is_ascii_digit()) && !p.is_empty() => {
                    (h.to_string(), p.parse().unwrap_or(80))
                }
                _ => (
                    authority.to_string(),
                    if raw.starts_with("https") { 443 } else { 80 },
                ),
            }
        };
        Some(Parts { host, port })
    }
}

/// 第二、三层：它认得这把 key **并且** 回的是目录形状。
pub async fn probe_identity(base: &str, key: &str, timeout_s: u64) -> Result<Value, String> {
    let url = format!("{}/models", base.trim_end_matches('/'));
    let req = reqwest::Client::builder()
        .timeout(Duration::from_secs(timeout_s))
        .build()
        .map_err(|e| e.to_string())?;
    let mut rb = req.get(&url);
    if !key.is_empty() {
        rb = rb.header("authorization", key);
    }
    let res = rb.send().await.map_err(|e| e.to_string())?;
    if !res.status().is_success() {
        return Err(format!("HTTP {}", res.status().as_u16()));
    }
    let doc: Value = res.json().await.map_err(|e| e.to_string())?;
    let list = match &doc {
        Value::Array(_) => true,
        Value::Object(o) => {
            o.get("data").map(|v| v.is_array()).unwrap_or(false)
                || o.get("models").map(|v| v.is_array()).unwrap_or(false)
        }
        _ => false,
    };
    if list {
        Ok(doc)
    } else {
        Err("回的不是目录形状".to_string())
    }
}

/// key 发现：两份 settings.json 的优先级 + 环境变量。每次现读（OFM 会自己轮换 forwardKey）。
pub fn find_key() -> Option<(String, String)> {
    if let Ok(v) = std::env::var("OFM_FORWARD_KEY") {
        if !v.trim().is_empty() {
            return Some((v.trim().to_string(), "env".to_string()));
        }
    }
    let home = dirs::home_dir()?;
    let candidates = [
        home.join(".our-free-model").join("settings.json"),
        home.join("AppData")
            .join("Roaming")
            .join("our-free-model")
            .join("settings.json"),
        home.join(".dsh").join("our-free-model").join("settings.json"),
    ];
    for file in candidates {
        let Ok(text) = std::fs::read_to_string(&file) else {
            continue;
        };
        let Ok(doc) = serde_json::from_str::<Value>(&text) else {
            continue;
        };
        let candidates_key = [
            doc.get("forwardKey").cloned(),
            doc.get("forward_key").cloned(),
            doc.get("apiKey").cloned(),
            doc.get("gateway")
                .and_then(|g| g.get("forwardKey"))
                .cloned(),
            doc.get("settings")
                .and_then(|g| g.get("forwardKey"))
                .cloned(),
        ];
        for c in candidates_key.into_iter().flatten() {
            if let Some(s) = c.as_str() {
                if !s.trim().is_empty() {
                    return Some((s.trim().to_string(), file.to_string_lossy().to_string()));
                }
            }
        }
    }
    None
}

/// 找网关。设了 OFM_BASE_URL 就只探它一个，且信任它。
///
/// **候选顺序**：环境变量 → **Pi models.json 里的 provider baseUrl** →
/// 标准端口（18900 桌面端 / 8326 插件版）。
///
/// 为什么把 models.json 放在标准端口前面：OFM 桌面端 exe 已经删了，现在跑的是
/// **magpie**（本机实测在 127.0.0.1:3425）。而这个端口**不是**固定值 ——
/// magpie 每次启动可能换端口。标准端口猜不到它，但 Pi 的 models.json 里
/// 记着渠道的 baseUrl，那才是真相。
pub async fn find_base() -> Option<(String, bool)> {
    if let Ok(forced) = std::env::var("OFM_BASE_URL") {
        if !forced.trim().is_empty() {
            return Some((forced.trim_end_matches('/').to_string(), true));
        }
    }
    let Some((key, _)) = find_key() else {
        // 没有 key 就没法验身份，只能盲探标准端口。
        for base in [STANDALONE_BASE, PLUGIN_BASE] {
            if tcp_open(base, 200) {
                return Some((base.to_string(), true));
            }
        }
        return None;
    };

    for base in candidate_bases() {
        if !tcp_open(&base, 200) {
            continue;
        }
        if probe_identity(&base, &key, 3).await.is_ok() {
            return Some((base, true));
        }
    }
    None
}

/// 所有候选网关地址。
///
/// 第一个是 models.json 里各 provider 的 baseUrl（去重、只留回环地址），
/// 后面是标准端口。
pub fn candidate_bases() -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut push = |base: String| {
        let b = base.trim().trim_end_matches('/').to_string();
        if b.is_empty() {
            return;
        }
        if !out.contains(&b) {
            out.push(b);
        }
    };

    // 1) Pi 的 models.json —— 渠道真实地址的来源。
    if let Some(doc) = crate::atomic::read_value(&crate::paths::models_json()) {
        if let Some(providers) = doc.get("providers").and_then(|v| v.as_object()) {
            for block in providers.values() {
                let url = block
                    .get("baseUrl")
                    .and_then(|v| v.as_str())
                    .or_else(|| block.get("baseURL").and_then(|v| v.as_str()))
                    .or_else(|| {
                        block
                            .get("options")
                            .and_then(|o| o.get("baseURL"))
                            .and_then(|v| v.as_str())
                    });
                if let Some(u) = url {
                    push(u.to_string());
                }
            }
        }
    }

    // 2) 标准端口兼底。
    push(STANDALONE_BASE.to_string());
    push(PLUGIN_BASE.to_string());

    out
}

/// 取目录（现取现写，不缓存）。
pub async fn fetch_catalog() -> Value {
    let Some((base, _)) = find_base().await else {
        return json!({
            "ok": false,
            "error": "OFM 客户端没在跑（或 key 读不到 / 端口对不上）",
            "models": []
        });
    };
    let Some((key, key_source)) = find_key() else {
        return json!({ "ok": false, "error": "读不到 forwardKey", "models": [] });
    };
    let probe = match probe_identity(&base, &key, 20).await {
        Ok(doc) => doc,
        Err(e) => {
            return json!({ "ok": false, "error": format!("OFM 目录读不到: {e}"), "models": [] });
        }
    };
    let mut models = parse_models(&probe, true);
    models.sort_by_key(|m| m.get("rank").and_then(|v| v.as_i64()).unwrap_or(0));
    json!({
        "ok": true,
        "base": base,
        "keySource": key_source,
        "models": models,
    })
}

/// 设置页「一键接入ofm」的探测。
pub async fn status() -> Value {
    let found = find_base().await;
    let hit = find_key();
    let catalog = if found.is_some() {
        fetch_catalog().await
    } else {
        json!({ "ok": false, "models": [] })
    };
    let models: Vec<Value> = catalog
        .get("models")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    let mut used: Vec<String> = Vec::new();
    for m in &models {
        if let Some(c) = m.get("channel").and_then(|v| v.as_str()) {
            if !c.is_empty() && !used.contains(&c.to_string()) {
                used.push(c.to_string());
            }
        }
    }

    let base = found.as_ref().map(|(b, _)| b.clone());
    let port = base.as_ref().and_then(|b| url::parse_base(b).map(|p| p.port as i64));

    let model_entries: Vec<Value> = models
        .iter()
        .map(|m| {
            let ctx = m.get("contextWindow").and_then(|v| v.as_u64()).unwrap_or(0);
            let max = m.get("maxTokens").and_then(|v| v.as_u64()).unwrap_or(0);
            json!({
                "id": m.get("id").cloned().unwrap_or(json!("")),
                "name": m.get("name").cloned().unwrap_or(json!("")),
                "contextWindow": if ctx > 0 { json!(ctx) } else { Value::Null },
                "maxTokens": if max > 0 { json!(max) } else { Value::Null },
                "efforts": m.get("efforts").cloned().unwrap_or(json!([])),
                "resolutions": [],
            })
        })
        .collect();

    let efforts_available = models.iter().any(|m| {
        m.get("efforts")
            .and_then(|v| v.as_array())
            .map(|a| !a.is_empty())
            .unwrap_or(false)
    });
    let reasoning_count = models
        .iter()
        .filter(|m| {
            m.get("efforts")
                .and_then(|v| v.as_array())
                .map(|a| !a.is_empty())
                .unwrap_or(false)
        })
        .count();

    let base_url = base.as_ref().map(|b| {
        let trimmed = b.trim_end_matches('/');
        if trimmed.ends_with("/v1") {
            trimmed.to_string()
        } else {
            format!("{trimmed}/v1")
        }
    });

    let error = if found.is_none() {
        Some("OFM 客户端没在跑".to_string())
    } else if hit.is_none() {
        Some("读不到 forwardKey".to_string())
    } else {
        None
    };

    json!({
        "ok": found.is_some() && hit.is_some(),
        "base": base,
        "baseUrl": base_url,
        "apiKey": hit.as_ref().map(|(k, _)| k.clone()).unwrap_or_default(),
        "key": hit.is_some(),
        "keySource": hit.as_ref().map(|(_, s)| s.clone()),
        "port": port,
        "flavor": "desktop",
        "flavorLabel": "OFM 桌面客户端",
        "models": models.iter().filter_map(|m| m.get("id").cloned()).collect::<Vec<_>>(),
        "modelEntries": model_entries,
        "reasoningCount": reasoning_count,
        "effortsAvailable": efforts_available,
        "compat": {
            "maxTokensField": "max_tokens",
            "supportsDeveloperRole": false,
            "supportsReasoningEffort": true,
            "thinkingFormat": "openai",
            "supportsStore": false,
        },
        "channels": used.len(),
        "error": error,
    })
}

/// 同步目录到 Pi 的 models.json（走唯一的写入口）。
pub async fn sync_from_catalog() -> Value {
    let cat = fetch_catalog().await;
    if !cat.get("ok").and_then(|v| v.as_bool()).unwrap_or(false) {
        return json!({
            "ok": false,
            "error": cat.get("error").cloned().unwrap_or(json!("目录读不到"))
        });
    }
    let Some((key, _)) = find_key() else {
        return json!({ "ok": false, "error": "读不到 forwardKey" });
    };
    let models: Vec<Value> = cat
        .get("models")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let base = cat.get("base").and_then(|v| v.as_str()).unwrap_or("");
    let r = crate::models::sync_models(&models, &crate::models::base_for_pi(base), &key, "ofm");
    let count = r.get("count").cloned().unwrap_or(json!(0));
    json!({
        "ok": true,
        "path": r.get("path").cloned(),
        "count": count,
        "baseUrl": r.get("baseUrl").cloned(),
        "provider": "ofm",
        "backup": true,
        "message": format!("已写入 {} 个模型；Pi 只在启动时读一次，重启后生效", count),
    })
}
