//! `models.json` 的**唯一**写入口（Pi 侧）。
//!
//! 规矩照抄原 Node 版：
//!   - 13 个渠道装进**同一个 `ofm` provider**；
//!   - `id` 保留渠道前缀（它是路由数据，剥掉前缀 Pi 发给上游就 404）；
//!   - 读一次现有文件再改，**绝不整份重造**；
//!   - 写前跑 assert_pi_schema：contextWindow/maxTokens 写成 0 会让**整份文件**
//!     被 Pi 拒绝，而症状是一句无关的 "No API key found"。

use std::collections::HashSet;
use std::path::PathBuf;

use serde_json::{json, Map, Value};

use crate::atomic::{backup_file, read_value, write_json_atomic};
use crate::paths;

pub const PROVIDER_ID: &str = "ofm";
const LEVELS: [&str; 6] = ["minimal", "low", "medium", "high", "xhigh", "max"];

/// 不认的档位写 null（Pi 的约定 = 不可选），六个档必须写满，少写一档全局会卡在 medium。
pub fn thinking_map(efforts: &[String], fallback: Option<&str>) -> Value {
    let mut table = Map::new();
    for l in LEVELS {
        table.insert(l.to_string(), Value::Null);
    }
    for l in efforts {
        if let Some(v) = table.get_mut(l.as_str()) {
            *v = Value::String(l.clone());
        }
    }
    if let Some(fb) = fallback {
        if let Some(v) = table.get_mut(fb) {
            if v.is_null() {
                *v = Value::String(fb.to_string());
            }
        }
    }
    if table.get("medium").map(|v| v.is_null()).unwrap_or(false) {
        if let Some(fb) = fallback {
            table.insert("medium".to_string(), Value::String(fb.to_string()));
        }
    }
    Value::Object(table)
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

/// 一个目录模型 → Pi 的 model 条目。
pub fn model_entry(m: &Value) -> Value {
    let id = m.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let mut name = m
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or(&id)
        .to_string();

    if let Some(mult) = m.get("multiplier") {
        if !mult.is_null() {
            let price = if mult.as_f64().unwrap_or(0.0) == 0.0 {
                "免费".to_string()
            } else {
                format!("x{}", trim_float(mult.as_f64().unwrap_or(0.0)))
            };
            let upper = name.to_uppercase();
            if !upper.contains("免费") && !upper.contains("FREE") {
                name = format!("{name} · {price}");
            }
        }
    }

    let supports_images = m
        .get("supportsImages")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let efforts: Vec<String> = m
        .get("efforts")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();

    let mut entry = Map::new();
    entry.insert("id".into(), json!(id));
    entry.insert("name".into(), json!(name));
    entry.insert(
        "input".into(),
        json!(if supports_images {
            vec!["text", "image"]
        } else {
            vec!["text"]
        }),
    );
    entry.insert("reasoning".into(), json!(!efforts.is_empty()));
    entry.insert(
        "cost".into(),
        json!({ "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0 }),
    );

    // ⚠️ 只在有正数时写。0 会毁掉整份文件。
    let ctx = positive(m.get("contextWindow"));
    if ctx > 0 {
        entry.insert("contextWindow".into(), json!(ctx));
    }
    let max = positive(m.get("maxTokens"));
    if max > 0 {
        entry.insert("maxTokens".into(), json!(max));
    }
    if !efforts.is_empty() {
        entry.insert("thinkingLevels".into(), json!(efforts));
        let fb = m.get("defaultEffort").and_then(|v| v.as_str());
        entry.insert("thinkingLevelMap".into(), thinking_map(&efforts, fb));
    }
    Value::Object(entry)
}

fn trim_float(v: f64) -> String {
    let s = format!("{v}");
    s.trim_end_matches(".0").to_string()
}

/// 写前 schema 断言。返回 Err 时带上可读的字段路径。
pub fn assert_pi_schema(doc: &Value) -> Result<(), String> {
    let Some(providers) = doc.get("providers").and_then(|v| v.as_object()) else {
        return Ok(());
    };
    for (pid, block) in providers {
        let Some(models) = block.get("models").and_then(|v| v.as_array()) else {
            continue;
        };
        for (i, m) in models.iter().enumerate() {
            for f in ["contextWindow", "maxTokens"] {
                let Some(v) = m.get(f) else { continue };
                let ok = v
                    .as_u64()
                    .map(|n| n > 0)
                    .unwrap_or(false);
                if !ok {
                    return Err(format!(
                        "providers.{pid}.models[{i}].{f} = {v}；必须是正整数，0 会让整份 models.json 被 Pi 拒绝"
                    ));
                }
            }
        }
    }
    Ok(())
}

fn sidecar_path() -> PathBuf {
    paths::sidecar_path()
}

fn read_sidecar() -> Value {
    read_value(&sidecar_path()).unwrap_or_else(|| json!({}))
}

/// 读 → 改 → 验 → 原子写。返回写给 Pi 的 baseUrl。
pub fn sync_models(models: &[Value], base_url: &str, api_key: &str, provider_id: &str) -> Value {
    let target = paths::models_json();
    let mut doc = read_value(&target).unwrap_or_else(|| json!({ "providers": {} }));
    if !doc.is_object() {
        doc = json!({ "providers": {} });
    }
    if !doc.get("providers").map(|v| v.is_object()).unwrap_or(false) {
        doc["providers"] = json!({});
    }

    let side = read_sidecar();
    let prev: Vec<String> = side
        .get(provider_id)
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();
    let mine: HashSet<String> = prev.into_iter().collect();

    let mut existing = doc["providers"]
        .get(provider_id)
        .and_then(|v| v.as_object())
        .cloned()
        .unwrap_or_default();

    // 陈旧条目清理：三条**同时**成立才摘。第三条缺了会把用户手加的
    // my-local-ollama（不在 sidecar、也不在目录里）当成陈旧条目删掉。
    let ours: HashSet<String> = models
        .iter()
        .filter_map(|m| m.get("id").and_then(|v| v.as_str()).map(|s| s.to_string()))
        .collect();
    let bare: HashSet<String> = ours
        .iter()
        .filter(|id| id.contains('/'))
        .map(|id| id.split('/').skip(1).collect::<Vec<_>>().join("/"))
        .collect();

    let kept: Vec<Value> = existing
        .get("models")
        .and_then(|v| v.as_array())
        .map(|a| a.to_vec())
        .unwrap_or_default()
        .into_iter()
        .filter(|e| {
            let id = e.get("id").and_then(|v| v.as_str()).unwrap_or("");
            !(id.is_empty() || mine.contains(id) || ours.contains(id) || bare.contains(id))
        })
        .collect();

    // 我们自己写的那批按目录整体替换，用户手加的留下。
    let user_kept: Vec<Value> = kept
        .into_iter()
        .filter(|e| {
            let id = e.get("id").and_then(|v| v.as_str()).unwrap_or("");
            !mine.contains(id)
        })
        .collect();

    let mut models_out = user_kept;
    models_out.extend(models.iter().map(model_entry));

    existing.insert("models".into(), Value::Array(models_out));
    existing.insert("baseUrl".into(), json!(base_url));
    existing.insert("api".into(), json!("openai-completions"));
    existing.insert("apiKey".into(), json!(api_key));
    doc["providers"][provider_id] = Value::Object(existing);

    if let Err(msg) = assert_pi_schema(&doc) {
        return json!({ "ok": false, "error": msg });
    }
    let _ = backup_file(&target);
    if let Err(e) = write_json_atomic(&target, &doc, 2) {
        return json!({ "ok": false, "error": format!("写 models.json 失败：{e}") });
    }

    let mut side2 = read_sidecar();
    if !side2.is_object() {
        side2 = json!({});
    }
    let ids: Vec<Value> = ours.iter().cloned().map(Value::String).collect();
    side2[provider_id] = Value::Array(ids);
    let _ = write_json_atomic(&sidecar_path(), &side2, 1);

    json!({
        "ok": true,
        "path": target.to_string_lossy(),
        "count": models.len(),
        "baseUrl": base_url,
        "provider": provider_id,
        "backup": true,
    })
}

/// 给 Pi 用的 baseUrl：目录里的 /v1 要去掉，Pi 的 openai-completions 自己会拼。
pub fn base_for_pi(base: &str) -> String {
    let trimmed = base.trim_end_matches('/');
    if trimmed.ends_with("/v1") {
        trimmed.strip_suffix("/v1").unwrap_or(trimmed).to_string()
    } else {
        trimmed.to_string()
    }
}

/// 通用「接入渠道」：用户手填 id / baseUrl / apiKey / models。
pub fn add_provider(body: &Value) -> Value {
    let id = body.get("id").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
    if id.is_empty() {
        return json!({ "ok": false, "error": "id 必填" });
    }
    let base_url = body
        .get("baseUrl")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    if base_url.is_empty() {
        return json!({ "ok": false, "error": "baseUrl 必填" });
    }
    let api_key = body
        .get("apiKey")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let compat = body.get("compat").cloned();

    let target = paths::models_json();
    let mut doc = read_value(&target).unwrap_or_else(|| json!({ "providers": {} }));
    if !doc.get("providers").map(|v| v.is_object()).unwrap_or(false) {
        doc["providers"] = json!({});
    }

    let raw_models = body
        .get("models")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let entries: Vec<Value> = raw_models
        .iter()
        .filter(|m| m.get("id").and_then(|v| v.as_str()).is_some_and(|s| !s.is_empty()))
        .map(|m| {
            let mid = m.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let name = m
                .get("name")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .unwrap_or(&mid)
                .to_string();
            let input = m
                .get("input")
                .and_then(|v| v.as_array())
                .filter(|a| !a.is_empty())
                .cloned()
                .unwrap_or_else(|| vec![json!("text")]);
            let efforts: Vec<String> = m
                .get("efforts")
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str().map(|s| s.to_string()))
                        .collect()
                })
                .unwrap_or_default();
            let reasoning = m
                .get("reasoning")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
                || !efforts.is_empty();

            let mut e = Map::new();
            e.insert("id".into(), json!(mid));
            e.insert("name".into(), json!(name));
            e.insert("input".into(), Value::Array(input));
            e.insert("reasoning".into(), json!(reasoning));
            e.insert(
                "cost".into(),
                json!({ "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0 }),
            );
            let ctx = positive(m.get("contextWindow"));
            if ctx > 0 {
                e.insert("contextWindow".into(), json!(ctx));
            }
            let max = positive(m.get("maxTokens"));
            if max > 0 {
                e.insert("maxTokens".into(), json!(max));
            }
            if !efforts.is_empty() {
                e.insert("thinkingLevels".into(), json!(efforts));
                let fb = m.get("defaultEffort").and_then(|v| v.as_str());
                e.insert("thinkingLevelMap".into(), thinking_map(&efforts, fb));
            }
            Value::Object(e)
        })
        .collect();

    let mut block = Map::new();
    block.insert("baseUrl".into(), json!(base_url));
    block.insert("api".into(), json!("openai-completions"));
    block.insert("apiKey".into(), json!(api_key));
    block.insert("models".into(), Value::Array(entries.clone()));
    if let Some(c) = compat {
        block.insert("compat".into(), c);
    }
    doc["providers"][id.as_str()] = Value::Object(block);

    if let Err(msg) = assert_pi_schema(&doc) {
        return json!({ "ok": false, "error": msg });
    }
    let _ = backup_file(&target);
    if let Err(e) = write_json_atomic(&target, &doc, 2) {
        return json!({ "ok": false, "error": format!("写 models.json 失败：{e}") });
    }
    let remaining: Vec<String> = doc["providers"]
        .as_object()
        .map(|m| m.keys().cloned().collect())
        .unwrap_or_default();
    json!({
        "ok": true,
        "provider": id,
        "modelCount": entries.len(),
        "backup": true,
        "remaining": remaining,
    })
}

/// 删一个 provider（默认 ofm）。带备份，原子写。
pub fn remove_provider(id: &str) -> Value {
    let target = paths::models_json();
    let Some(mut doc) = read_value(&target) else {
        return json!({ "ok": false, "error": format!("没有这个 provider: {id}") });
    };
    let exists = doc
        .get("providers")
        .and_then(|v| v.get(id))
        .is_some();
    if !exists {
        return json!({ "ok": false, "error": format!("没有这个 provider: {id}") });
    }
    if let Some(p) = doc["providers"].as_object_mut() {
        p.remove(id);
    }
    let _ = backup_file(&target);
    if let Err(e) = write_json_atomic(&target, &doc, 2) {
        return json!({ "ok": false, "error": format!("写 models.json 失败：{e}") });
    }
    let _ = write_json_atomic(&sidecar_path(), &json!({}), 1);
    let remaining: Vec<String> = doc["providers"]
        .as_object()
        .map(|m| m.keys().cloned().collect())
        .unwrap_or_default();
    json!({ "ok": true, "id": id, "remaining": remaining })
}

/// 模型清单（给 /models 与前端下拉共用）。
pub fn list_models() -> Value {
    let doc = read_value(&paths::models_json()).unwrap_or_else(|| json!({}));
    let mut groups = Vec::new();
    if let Some(providers) = doc.get("providers").and_then(|v| v.as_object()) {
        for (provider, block) in providers {
            let models: Vec<Value> = block
                .get("models")
                .and_then(|v| v.as_array())
                .map(|a| a.to_vec())
                .unwrap_or_default()
                .into_iter()
                .filter(|m| m.get("id").and_then(|v| v.as_str()).is_some_and(|s| !s.is_empty()))
                .map(|m| {
                    let id = m.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
                    let name = m
                        .get("name")
                        .and_then(|v| v.as_str())
                        .filter(|s| !s.is_empty())
                        .unwrap_or(&id)
                        .to_string();
                    json!({ "id": id, "name": name })
                })
                .collect();
            if !models.is_empty() {
                groups.push(json!({ "provider": provider, "models": models }));
            }
        }
    }
    json!({ "ok": true, "groups": groups })
}
