//! law-surface-for-context.rs — twin of skills/entities/law-surface-for-context.mjs (db READ
//! lane via catalog-find's attached `laws` db). NAMES ARE CONTRACT: `lawSurfaceForContext`
//! (default, async in JS — awaited to its resolved value per the async-default-export lane) and
//! `buildContextQuery` (named export, pure no-db helper). Builds a text query from work context
//! (card/action/agent/domain/recent_prompts), then delegates to catalog-find's deterministic
//! substring-fallback path (entity_type='law', fallback:true) and post-filters to
//! active + non-hard laws. catalog-find's vector-search branch is PARITY-DEFERRED (unconvertible
//! embed() ML call) — this twin never triggers it because its own fixtures never seed
//! memory.catalog_embeddings, matching the real call site's `fallback: true`.
use rusqlite::Connection;
use serde_json::{json, Map, Value};

fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}
fn tmpl(v: Option<&Value>) -> String {
    match v {
        None => "undefined".into(),
        Some(Value::Null) => "null".into(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(),
        Some(o) => o.to_string(),
    }
}
// = JS String(v) — used for Array.prototype.join element coercion (p.content ?? p).
fn js_string(v: &Value) -> String {
    match v {
        Value::Null => "null".into(),
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Array(a) => a.iter().map(js_string).collect::<Vec<_>>().join(","),
        Value::Object(_) => "[object Object]".into(),
    }
}
// JS `p.content ?? p`: only reads .content off a plain object; any other type (string/number/etc)
// has no .content, so the whole element falls back to itself, unchanged.
fn recent_val(p: &Value) -> Value {
    let content = p.as_object().and_then(|o| o.get("content")).cloned();
    match content {
        Some(v) if !v.is_null() => v,
        _ => p.clone(),
    }
}

pub fn buildContextQuery(a: &Value) -> Value {
    let obj = a.as_object();
    let get = |k: &str| obj.and_then(|o| o.get(k));
    let mut parts: Vec<String> = Vec::new();

    if truthy(get("action")) {
        parts.push(format!("action: {}", tmpl(get("action"))));
    }
    if truthy(get("agent")) {
        parts.push(format!("agent: {}", tmpl(get("agent"))));
    }
    if truthy(get("domain")) {
        parts.push(format!("domain: {}", tmpl(get("domain"))));
    }

    let card_obj = get("card").and_then(|c| c.as_object());
    if let Some(title) = card_obj.and_then(|c| c.get("title")).filter(|v| truthy(Some(v))) {
        parts.push(format!("card: {}", tmpl(Some(title))));
    }
    if let Some(body_json) = card_obj.and_then(|c| c.get("body_json")).filter(|v| truthy(Some(v))) {
        if let Some(s) = body_json.as_str() {
            if let Ok(parsed) = serde_json::from_str::<Value>(s) {
                if let Ok(stringified) = serde_json::to_string(&parsed) {
                    let sliced: String = stringified.chars().take(200).collect();
                    parts.push(format!("card_body: {}", sliced));
                }
            }
            // JSON.parse throw -> caught -> skip (malformed body)
        }
    }

    if let Some(arr) = get("recent_prompts").and_then(|v| v.as_array()) {
        let len = arr.len();
        let start = len.saturating_sub(3);
        let tail = arr[start..]
            .iter()
            .map(|p| js_string(&recent_val(p)))
            .collect::<Vec<_>>()
            .join(" | ");
        if !tail.is_empty() {
            let sliced: String = tail.chars().take(300).collect();
            parts.push(format!("recent: {}", sliced));
        }
    }

    Value::String(parts.join("\n"))
}

pub fn lawSurfaceForContext(input: &Value, db: &Connection) -> Result<Value, String> {
    let obj = input.as_object();
    let get = |k: &str| obj.and_then(|o| o.get(k));

    let mut ctx = Map::new();
    ctx.insert("card".into(), get("card").cloned().unwrap_or(Value::Null));
    ctx.insert("action".into(), get("action").cloned().unwrap_or(Value::Null));
    ctx.insert("agent".into(), get("agent").cloned().unwrap_or(Value::Null));
    ctx.insert("domain".into(), get("domain").cloned().unwrap_or(Value::Null));
    ctx.insert(
        "recent_prompts".into(),
        get("recent_prompts").cloned().unwrap_or(json!([])),
    );

    let query = match buildContextQuery(&Value::Object(ctx)) {
        Value::String(s) => s,
        _ => String::new(),
    };
    if query.is_empty() {
        return Ok(json!([]));
    }

    let limit = get("limit").cloned().unwrap_or(json!(5));
    let min_score = get("minScore").cloned().unwrap_or(json!(0.0));

    let mut cf_input = Map::new();
    cf_input.insert("query".into(), json!(query));
    cf_input.insert("entity_type".into(), json!("law"));
    cf_input.insert("limit".into(), limit);
    cf_input.insert("minScore".into(), min_score);
    cf_input.insert("fallback".into(), json!(true));

    let results = crate::entities_catalog_find::catalogFind(&Value::Object(cf_input), db)?;

    let filtered: Vec<Value> = results
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter(|r| match r.get("entity") {
            Some(Value::Object(e)) => {
                let superseded_ok = e.get("superseded_by").map_or(true, |v| v.is_null());
                let enforcement_ok = e.get("enforcement").and_then(|v| v.as_str()) != Some("hard");
                superseded_ok && enforcement_ok
            }
            _ => false,
        })
        .collect();

    Ok(json!(filtered))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "ATTACH ':memory:' AS memory; ATTACH ':memory:' AS laws;
             CREATE TABLE memory.catalog_embeddings (entity_type TEXT NOT NULL, entity_name TEXT NOT NULL, model TEXT NOT NULL, dims INTEGER NOT NULL, vector BLOB NOT NULL);
             CREATE TABLE memory.skills (name TEXT, description TEXT);
             CREATE TABLE actions (name TEXT, description TEXT);
             CREATE TABLE agents (name TEXT, description TEXT);
             CREATE TABLE laws.laws (slug TEXT, title TEXT, statement TEXT, rationale TEXT, enforcement TEXT, superseded_by INTEGER);
             CREATE TABLE memory.memories (id INTEGER PRIMARY KEY, title TEXT);",
        )
        .unwrap();
        c
    }

    #[test]
    fn build_context_query_joins_present_parts_with_newlines() {
        let q = buildContextQuery(&json!({
            "action": "card.update", "agent": "dev", "domain": "security",
            "card": { "title": "Fix login bug", "body_json": "{\"kind\":\"bug\",\"size\":\"s\"}" },
            "recent_prompts": ["please look at bug", { "content": "still broken" }]
        }));
        assert_eq!(
            q,
            json!("action: card.update\nagent: dev\ndomain: security\ncard: Fix login bug\ncard_body: {\"kind\":\"bug\",\"size\":\"s\"}\nrecent: please look at bug | still broken")
        );
    }

    #[test]
    fn build_context_query_skips_malformed_body_and_empty_recent() {
        let q = buildContextQuery(&json!({
            "action": "x", "card": { "body_json": "not json" }, "recent_prompts": []
        }));
        assert_eq!(q, json!("action: x"));
    }

    #[test]
    fn empty_context_returns_empty_query_and_short_circuits() {
        let c = db();
        let r = lawSurfaceForContext(&json!({}), &c).unwrap();
        assert_eq!(r, json!([]));
    }

    #[test]
    fn matched_law_filters_out_hard_enforcement() {
        let c = db();
        let q = "domain: audit-topic";
        c.execute(
            "INSERT INTO laws.laws (slug, title, statement, enforcement, superseded_by) VALUES (?, ?, ?, 'soft', NULL)",
            rusqlite::params!["SOFT_LAW", format!("law about {q}"), "stmt"],
        )
        .unwrap();
        c.execute(
            "INSERT INTO laws.laws (slug, title, statement, enforcement, superseded_by) VALUES (?, ?, ?, 'hard', NULL)",
            rusqlite::params!["HARD_LAW", format!("law about {q}"), "stmt"],
        )
        .unwrap();
        let r = lawSurfaceForContext(&json!({ "domain": "audit-topic" }), &c).unwrap();
        let arr = r.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["entity"]["slug"], json!("SOFT_LAW"));
    }
}
