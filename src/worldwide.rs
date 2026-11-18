//! 「全球热门」页的两个榜 —— OpenRouter 最近一天的模型与 App。
//!
//! ## 数据源
//!
//! OpenRouter 前端接口（都返回 JSON）：
//! - 模型：`/api/frontend/v1/rankings/models` —— 每行一天一个模型
//!   （`date` + `model_permaslug` + prompt/completion tokens）。按天聚合成
//!   「最近 `DAYS` 天」的榜，`DAYS = 1` 就是「最近一天」。
//! - App：`/api/frontend/v1/rankings/apps` —— `data.{day,week,month}` 各一组，
//!   每行带 `rank`、`total_tokens`（**字符串**）、`total_requests` 和
//!   `app.title`。这就是 OpenRouter「Top Apps」页的数据（只统计自愿开启
//!   用量统计的应用）。
//!
//! ## 「一天一次」的缓存
//!
//! 两个源共用 `CACHE_TTL_MS`（24h）。缓存不止在内存里，还原子写到
//! `<state_dir>/worldwide-cache.json`，所以**重启之后**进入该页仍然直接吃
//! 上次的榜。面板「刷新」走 `POST /api/worldwide/refresh` → `clear_cache()`，
//! 内存和磁盘一起清，才是真重抓。
//!
//! ## 失败语义
//!
//! 两个源逐张独立成败：一家挂了只让那张卡显示 `数据较旧 / 取不到`，
//! 不清掉已经拿到的旧值。

use std::collections::HashMap;
use std::future::Future;
use std::path::PathBuf;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

const MODELS_URL: &str = "https://openrouter.ai/api/frontend/v1/rankings/models";
const APPS_URL: &str = "https://openrouter.ai/api/frontend/v1/rankings/apps";

/// 一张卡取几条。
pub const TOP_N: usize = 5;

/// 「最近 N 天」：用户要的是最近一天。
pub const DAYS: usize = 1;

/// 两个网络源的缓存时长：**一天一次**。面板切到该页时命中就不发请求，
/// 只有显式「刷新」或过期才真抓。
const CACHE_TTL_MS: u64 = 24 * 60 * 60 * 1000;

/// 抓外网的超时。
const FETCH_TIMEOUT_MS: u64 = 20_000;

const USER_AGENT: &str = "Mozilla/5.0 (compatible; agent-manager/1.0)";

/// 持久化文件放在 app state dir（`PI_STATE_DIR` 可覆盖，便于测试隔离）。
/// 形状：`{ "<key>": { "at": <ms>, "items": [...] } }`。
fn cache_file() -> PathBuf {
    crate::paths::state_dir().join("worldwide-cache.json")
}

// ============================================================ 模型榜

/// 把 OpenRouter 的**按天**排行聚合成「最近 `DAYS` 天」的模型榜，取前 `limit`。
///
/// 锚点是**载荷里最新的一天**，不是墙上时间的今天：上游数据常滞后一天，
/// 按今天过滤会得到空榜。
pub fn openrouter_models(payload: &Value, limit: usize) -> Result<Vec<Value>, String> {
    let Some(rows) = payload.get("data").and_then(|v| v.as_array()) else {
        return Err("OpenRouter 模型排行载荷里没有 data 数组（接口变了？）".to_string());
    };

    let mut latest: Option<String> = None;
    for row in rows {
        let Some(day) = row.get("date").and_then(|v| v.as_str()).and_then(day_of) else {
            continue;
        };
        if latest.as_deref().map(|d| day.as_str() > d).unwrap_or(true) {
            latest = Some(day);
        }
    }
    let Some(latest) = latest else {
        return Ok(Vec::new());
    };
    let cutoff = day_key(day_ms(&latest) - (DAYS as i64 - 1) * 86_400_000);

    let mut totals: HashMap<String, u64> = HashMap::new();
    for row in rows {
        let Some(row) = row.as_object() else { continue };
        let Some(day) = row.get("date").and_then(|v| v.as_str()).and_then(day_of) else {
            continue;
        };
        if day < cutoff || day > latest {
            continue;
        }
        let slug = row
            .get("model_permaslug")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim();
        if slug.is_empty() {
            continue;
        }
        let total =
            as_int(row.get("total_prompt_tokens")) + as_int(row.get("total_completion_tokens"));
        *totals.entry(slug.to_string()).or_insert(0) += total;
    }

    let mut ranked: Vec<(String, u64)> = totals.into_iter().collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    ranked.truncate(limit);

    Ok(ranked
        .into_iter()
        .map(|(model, tokens)| json!({ "model": model, "name": model, "tokens": tokens }))
        .collect())
}

// ============================================================ App 榜

/// OpenRouter「Top Apps」：`data.day`（缺了就 week、再 month）里按 `rank`
/// 排序取前 `limit`。`total_tokens` 是字符串，要自己转。
pub fn openrouter_apps(payload: &Value, limit: usize) -> Result<Vec<Value>, String> {
    let Some(data) = payload.get("data") else {
        return Err("OpenRouter App 排行载荷里没有 data（接口变了？）".to_string());
    };
    let rows = ["day", "week", "month"]
        .iter()
        .find_map(|k| data.get(*k).and_then(|v| v.as_array()))
        .ok_or_else(|| "OpenRouter App 排行里没有 day/week/month 数组".to_string())?;

    let mut ranked: Vec<(i64, String, u64, u64)> = Vec::new();
    for row in rows {
        let rank = row.get("rank").and_then(|v| v.as_i64()).unwrap_or(i64::MAX);
        let name = row
            .get("app")
            .and_then(|a| a.get("title"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim();
        if name.is_empty() {
            continue;
        }
        let tokens = row
            .get("total_tokens")
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse::<u64>().ok())
            .or_else(|| row.get("total_tokens").and_then(|v| v.as_u64()))
            .unwrap_or(0);
        let requests = row
            .get("total_requests")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        ranked.push((rank, name.to_string(), tokens, requests));
    }

    ranked.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| b.2.cmp(&a.2)));
    ranked.truncate(limit);

    Ok(ranked
        .into_iter()
        .map(|(_, name, tokens, requests)| {
            json!({ "name": name, "tokens": tokens, "requests": requests })
        })
        .collect())
}

// ============================================================ 日期

/// `"2026-10-09 00:00:00"` → `"2026-10-09"`；认不出是 `None`。
fn day_of(value: &str) -> Option<String> {
    if value.len() < 10 {
        return None;
    }
    let head = &value[..10];
    let bytes = head.as_bytes();
    let shape = bytes.len() == 10
        && bytes[4] == b'-'
        && bytes[7] == b'-'
        && bytes[0..4].iter().all(|b| b.is_ascii_digit())
        && bytes[5..7].iter().all(|b| b.is_ascii_digit())
        && bytes[8..10].iter().all(|b| b.is_ascii_digit());
    shape.then(|| head.to_string())
}

/// Unix 毫秒 → `"YYYY-MM-DD"`（UTC）。
fn day_key(ms: i64) -> String {
    let days = ms.div_euclid(86_400_000);
    let (y, m, d) = civil_from_days(days);
    format!("{y:04}-{m:02}-{d:02}")
}

/// `"YYYY-MM-DD"` → Unix 毫秒（UTC 零点）。
fn day_ms(day: &str) -> i64 {
    let n = |s: &str| s.parse::<i64>().ok();
    let (Some(y), Some(m), Some(d)) = (n(&day[0..4]), n(&day[5..7]), n(&day[8..10])) else {
        return 0;
    };
    days_from_civil(y, m as u32, d as u32) * 86_400_000
}

/// Howard Hinnant：公历 → 1970-01-01 起的天数。
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m as i64 + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn as_int(v: Option<&Value>) -> u64 {
    match v {
        Some(Value::Number(n)) => n.as_u64().unwrap_or(0),
        Some(Value::String(s)) => s.parse::<u64>().unwrap_or(0),
        _ => 0,
    }
}

/// Howard Hinnant 的公历换算（1970-01-01 起的天数）。
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

// ============================================================ 缓存

#[derive(Clone, Serialize, Deserialize)]
struct CacheEntry {
    at: u64,
    items: Vec<Value>,
}

/// 进程内缓存：启动时读一次盘，之后读写都在内存里；落盘由
/// `write_json_atomic` 负责（临时文件 + rename，不写半截）。
static CACHE: LazyLock<Mutex<HashMap<String, CacheEntry>>> =
    LazyLock::new(|| Mutex::new(load_persisted(&cache_file())));

fn cache() -> &'static Mutex<HashMap<String, CacheEntry>> {
    &CACHE
}

/// 这张卡最近一次真正抓到数据的时刻（Unix 毫秒），给面板显示「上次更新」。
fn cache_at(key: &str) -> Option<u64> {
    cache().lock().unwrap().get(key).map(|e| e.at)
}

fn load_persisted(file: &std::path::Path) -> HashMap<String, CacheEntry> {
    let Some(doc) = crate::atomic::read_value(file) else {
        return HashMap::new();
    };
    let mut out = HashMap::new();
    // 只收形状正确的 key；坏的那条直接丢，不影响另一张卡。
    if let Some(obj) = doc.as_object() {
        for (key, val) in obj {
            if let Ok(entry) = serde_json::from_value::<CacheEntry>(val.clone()) {
                out.insert(key.clone(), entry);
            }
        }
    }
    out
}

/// 把整份内存缓存原子写回磁盘。写失败不抛 —— 缓存只是优化，不该让请求挂掉。
fn persist(file: &std::path::Path, map: &HashMap<String, CacheEntry>) {
    let Ok(doc) = serde_json::to_value(map) else {
        return;
    };
    let _ = crate::atomic::write_json_atomic(file, &doc, 0);
}

/// 清掉两个网络源的缓存 —— **内存和磁盘都清**。
pub fn clear_cache() {
    let file = cache_file();
    let mut map = cache().lock().unwrap();
    clear_in(&file, &mut map);
}

/// 清空的实质动作，单独拆出来便于用临时文件测试（不碰全局环境）。
fn clear_in(file: &std::path::Path, map: &mut HashMap<String, CacheEntry>) {
    map.clear();
    persist(file, map);
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

async fn fetch_text(url: &str) -> Result<String, String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(FETCH_TIMEOUT_MS))
        .user_agent(USER_AGENT)
        .build()
        .map_err(|e| e.to_string())?;
    let res = client.get(url).send().await.map_err(|e| e.to_string())?;
    if !res.status().is_success() {
        return Err(format!("HTTP {} {url}", res.status().as_u16()));
    }
    res.text().await.map_err(|e| e.to_string())
}

/// 命中新鲜缓存就不发请求。失败时**回退到上一次成功的结果**并把原因带出去。
async fn cached<F, Fut>(key: &str, produce: F) -> (Vec<Value>, Option<String>, bool)
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<Vec<Value>, String>>,
{
    let now = now_ms();
    {
        let map = cache().lock().unwrap();
        if let Some(hit) = map.get(key) {
            if now.saturating_sub(hit.at) < CACHE_TTL_MS {
                return (hit.items.clone(), None, false);
            }
        }
    }
    match produce().await {
        Ok(items) => {
            let mut map = cache().lock().unwrap();
            map.insert(
                key.to_string(),
                CacheEntry { at: now, items: items.clone() },
            );
            persist(&cache_file(), &map);
            (items, None, false)
        }
        Err(error) => {
            let stale = cache().lock().unwrap().get(key).map(|e| e.items.clone());
            (stale.unwrap_or_default(), Some(error), true)
        }
    }
}

/// 一张网络卡。
async fn network_card(
    key: &str,
    url: &str,
    days: u64,
    parse: fn(&str) -> Result<Vec<Value>, String>,
) -> Value {
    let (items, error, stale) = cached(key, || async {
        let text = fetch_text(url).await?;
        parse(&text)
    })
    .await;
    json!({
        "ok": error.is_none(),
        "items": items,
        "stale": stale,
        "error": error,
        "days": days,
        "updatedAt": cache_at(key),
    })
}

/// 三张卡的载荷（OpenRouter 模型 / opencode 模型 / OpenRouter App）。
/// **逐张独立成败** —— 一家挂了不该让另一张也变空。
pub async fn worldwide_report() -> Value {
    let (models, opencode, apps) = tokio::join!(
        network_card("models", MODELS_URL, DAYS as u64, |text| {
            let payload: Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
            openrouter_models(&payload, TOP_N)
        }),
        network_card("opencode", crate::worldwide_opencode::STATS_URL, crate::worldwide_opencode::DAYS as u64, |html| {
            crate::worldwide_opencode::parse_opencode_weekly(html, TOP_N)
        }),
        network_card("apps", APPS_URL, DAYS as u64, |text| {
            let payload: Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
            openrouter_apps(&payload, TOP_N)
        }),
    );
    json!({ "ok": true, "days": DAYS, "models": models, "opencode": opencode, "apps": apps })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("agent-manager-ww-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn cache_file_lives_in_state_dir() {
        assert!(cache_file().ends_with("worldwide-cache.json"));
        assert!(cache_file().starts_with(crate::paths::state_dir()));
    }

    #[test]
    fn persisted_round_trip_and_corrupt_silent() {
        let dir = temp_dir("roundtrip");
        let file = dir.join("worldwide-cache.json");
        let mut map = HashMap::new();
        map.insert(
            "models".to_string(),
            CacheEntry { at: now_ms(), items: vec![json!({ "model": "x", "tokens": 1 })] },
        );
        map.insert(
            "apps".to_string(),
            CacheEntry { at: now_ms(), items: vec![json!({ "name": "y", "tokens": 2 })] },
        );
        persist(&file, &map);

        let back = load_persisted(&file);
        assert_eq!(back["models"].items[0]["model"], "x");
        assert_eq!(back["apps"].items[0]["name"], "y");
        assert!(now_ms().saturating_sub(back["models"].at) < CACHE_TTL_MS);

        // 一条坏了不能拖垮另一条。
        std::fs::write(&file, r#"{"models":{"at":"oops","items":[]},"apps":{"at":1,"items":[]}}"#)
            .unwrap();
        let back = load_persisted(&file);
        assert!(!back.contains_key("models"));
        assert!(back.contains_key("apps"));
    }

    #[test]
    fn clear_leaves_empty_persisted_file() {
        let dir = temp_dir("clear");
        let file = dir.join("worldwide-cache.json");
        let mut map = HashMap::new();
        map.insert("models".to_string(), CacheEntry { at: now_ms(), items: vec![] });
        persist(&file, &map);
        clear_in(&file, &mut map);
        assert!(map.is_empty());
        assert!(load_persisted(&file).is_empty());
    }

    #[test]
    fn ttl_is_24_hours() {
        assert_eq!(CACHE_TTL_MS, 24 * 60 * 60 * 1000);
        assert_eq!(DAYS, 1);
    }

    #[test]
    fn models_use_only_the_requested_day_and_sort_by_tokens() {
        let payload = json!({
            "data": [
                // 今天：a 5+5=10，b 3+1=4
                {"date": "2026-11-18 00:00:00", "model_permaslug": "a", "total_prompt_tokens": 5, "total_completion_tokens": 5},
                {"date": "2026-11-18 00:00:00", "model_permaslug": "b", "total_prompt_tokens": 3, "total_completion_tokens": 1},
                // 昨天的不算
                {"date": "2026-11-17 00:00:00", "model_permaslug": "a", "total_prompt_tokens": 100, "total_completion_tokens": 100},
                {"date": "2026-11-18 00:00:00", "model_permaslug": "a", "variant": "free", "total_prompt_tokens": 0, "total_completion_tokens": 0}
            ]
        });
        // 锚点是载荷里最新的 2026-11-18；11-17 的行被窗口过滤。
        let rows = openrouter_models(&payload, TOP_N).unwrap();
        // 窗口外的行全部被过滤，只剩今天的两条（a=10, b=4）。
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["model"], "a");
        assert_eq!(rows[0]["tokens"], 10);
        assert_eq!(rows[1]["model"], "b");
    }

    #[test]
    fn apps_prefer_day_and_sort_by_rank() {
        let payload = json!({
            "data": {
                "day": [
                    {"rank": 9, "total_tokens": "900", "total_requests": 9, "app": {"title": "nine"}},
                    {"rank": 1, "total_tokens": "100", "total_requests": 1, "app": {"title": "one"}},
                    {"rank": 3, "total_tokens": "300", "total_requests": 3, "app": {"title": ""}}
                ],
                "week": [
                    {"rank": 1, "total_tokens": "1", "total_requests": 1, "app": {"title": "week"}}
                ]
            }
        });
        let rows = openrouter_apps(&payload, TOP_N).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["name"], "one");
        assert_eq!(rows[0]["tokens"], 100);
        assert_eq!(rows[1]["name"], "nine");
    }
}
