//! substrate-stream.rs — twin of skills/system/substrate-stream.mjs (db READ lane, cross-db
//! brain.ai_calls / brain.driver_ticks). NAMES ARE CONTRACT: `substrateStream` (default). Merges
//! sacred_timeline (audit) + brain.ai_calls (ai) + brain.driver_ticks (tick) + notifications into
//! one chronologically-descending firehose, capped at `limit` (clamped 1..1000, default 200).
// QUALITY: 82/100 — 4-source merge, per-source LIMIT before the overall ts-desc stable sort/slice
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
// JS `${v}` interpolation.
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
// JS String(v) (no ??).
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
// JS Number(v).
fn js_number(v: Option<&Value>) -> f64 {
    match v {
        None => f64::NAN,
        Some(Value::Null) => 0.0,
        Some(Value::Bool(b)) => if *b { 1.0 } else { 0.0 },
        Some(Value::Number(n)) => n.as_f64().unwrap_or(f64::NAN),
        Some(Value::String(s)) => { let t = s.trim(); if t.is_empty() { 0.0 } else { t.parse::<f64>().unwrap_or(f64::NAN) } },
        Some(_) => f64::NAN,
    }
}
fn g(row: &Value, key: &str) -> Value { row.get(key).cloned().unwrap_or(Value::Null) }

pub fn substrateStream(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS: if (!db) throw err.contextMissing('substrate-stream','db') — the db lane always injects one.
    let limit_n = js_number(input.get("limit"));
    // Number(limit) || 200 — falsy (0 or NaN) falls back to 200; the destructured default (also
    // 200) converges with this fallback so absent-vs-null-vs-present all resolve the same way.
    let base = if limit_n == 0.0 || limit_n.is_nan() { 200.0 } else { limit_n };
    let cap = base.max(1.0).min(1000.0) as i64;

    let since_ts_v = input.get("since_ts").cloned().unwrap_or(Value::Null);
    let since_filter = if truthy(Some(&since_ts_v)) {
        format!(" AND ts > '{}'", js_string(&since_ts_v).replace('\'', "''"))
    } else {
        String::new()
    };
    // sinceFilter.replace(/ts/g, 'started_at') — literal substring replace, every occurrence.
    let since_filter_started_at = since_filter.replace("ts", "started_at");

    let kinds_v = input.get("kinds").cloned().unwrap_or(Value::Null);
    let want = |k: &str| -> bool {
        if !truthy(Some(&kinds_v)) { return true; }
        kinds_v.as_array().map_or(false, |arr| arr.iter().any(|x| x.as_str() == Some(k)))
    };

    let mut events: Vec<Value> = Vec::new();

    if want("audit") {
        let sql = format!(
            "SELECT id, ts, actor, operation, entity_type, entity_id, \
             substr(payload, 1, 500) AS payload_short, \
             substr(digital_print, 1, 12) AS dp_short \
             FROM sacred_timeline WHERE 1=1 {since_filter} ORDER BY id DESC LIMIT ?"
        );
        if let Ok(rows) = crate::db_json::query_rows(db, &sql, &[json!(cap)]) {
            for r in rows.as_array().cloned().unwrap_or_default() {
                let mut o = Map::new();
                o.insert("source".into(), json!("audit"));
                o.insert("id".into(), json!(format!("audit-{}", tmpl(Some(&g(&r, "id"))))));
                o.insert("ts".into(), g(&r, "ts"));
                o.insert("actor".into(), g(&r, "actor"));
                o.insert("operation".into(), g(&r, "operation"));
                o.insert("entity_type".into(), g(&r, "entity_type"));
                o.insert("entity_id".into(), g(&r, "entity_id"));
                o.insert("digital_print_short".into(), g(&r, "dp_short"));
                o.insert("payload_short".into(), g(&r, "payload_short"));
                events.push(Value::Object(o));
            }
        }
    }

    if want("ai") {
        let sql = format!(
            "SELECT id, started_at AS ts, prompt_name, prompt_version, \
             tier, temperature, provider, model_id, outcome, \
             cost_usd_actual, tokens_in, tokens_out, duration_ms, cached \
             FROM brain.ai_calls WHERE 1=1 {since_filter_started_at} ORDER BY id DESC LIMIT ?"
        );
        if let Ok(rows) = crate::db_json::query_rows(db, &sql, &[json!(cap)]) {
            for r in rows.as_array().cloned().unwrap_or_default() {
                let mut o = Map::new();
                o.insert("source".into(), json!("ai"));
                o.insert("id".into(), json!(format!("ai-{}", tmpl(Some(&g(&r, "id"))))));
                o.insert("ts".into(), g(&r, "ts"));
                o.insert("prompt_name".into(), g(&r, "prompt_name"));
                o.insert("prompt_version".into(), g(&r, "prompt_version"));
                o.insert("tier".into(), g(&r, "tier"));
                o.insert("temperature".into(), g(&r, "temperature"));
                o.insert("provider".into(), g(&r, "provider"));
                o.insert("model_id".into(), g(&r, "model_id"));
                o.insert("outcome".into(), g(&r, "outcome"));
                o.insert("cost_usd".into(), g(&r, "cost_usd_actual"));
                o.insert("tokens_in".into(), g(&r, "tokens_in"));
                o.insert("tokens_out".into(), g(&r, "tokens_out"));
                o.insert("duration_ms".into(), g(&r, "duration_ms"));
                o.insert("cached".into(), json!(truthy(Some(&g(&r, "cached")))));
                events.push(Value::Object(o));
            }
        }
    }

    if want("tick") {
        let sql = format!(
            "SELECT id, started_at AS ts, ended_at, card_slug, \
             decided_agent, decided_action, decided_reason, \
             outcome, error_message, cost_usd, duration_ms \
             FROM brain.driver_ticks WHERE 1=1 {since_filter_started_at} ORDER BY id DESC LIMIT ?"
        );
        if let Ok(rows) = crate::db_json::query_rows(db, &sql, &[json!(cap)]) {
            for r in rows.as_array().cloned().unwrap_or_default() {
                let mut o = Map::new();
                o.insert("source".into(), json!("tick"));
                o.insert("id".into(), json!(format!("tick-{}", tmpl(Some(&g(&r, "id"))))));
                o.insert("ts".into(), g(&r, "ts"));
                o.insert("ended_at".into(), g(&r, "ended_at"));
                o.insert("card_slug".into(), g(&r, "card_slug"));
                o.insert("decided_agent".into(), g(&r, "decided_agent"));
                o.insert("decided_action".into(), g(&r, "decided_action"));
                o.insert("decided_reason".into(), g(&r, "decided_reason"));
                o.insert("outcome".into(), g(&r, "outcome"));
                o.insert("error_message".into(), g(&r, "error_message"));
                o.insert("cost_usd".into(), g(&r, "cost_usd"));
                o.insert("duration_ms".into(), g(&r, "duration_ms"));
                events.push(Value::Object(o));
            }
        }
    }

    if want("notification") {
        let sql = format!(
            "SELECT id, ts, kind, severity, source, related_card_slug, \
             substr(message, 1, 300) AS message, \
             dismissed_at \
             FROM notifications WHERE 1=1 {since_filter} ORDER BY id DESC LIMIT ?"
        );
        if let Ok(rows) = crate::db_json::query_rows(db, &sql, &[json!(cap)]) {
            for r in rows.as_array().cloned().unwrap_or_default() {
                let mut o = Map::new();
                o.insert("source".into(), json!("notification"));
                o.insert("id".into(), json!(format!("notif-{}", tmpl(Some(&g(&r, "id"))))));
                o.insert("ts".into(), g(&r, "ts"));
                o.insert("kind".into(), g(&r, "kind"));
                o.insert("severity".into(), g(&r, "severity"));
                o.insert("notif_source".into(), g(&r, "source"));
                o.insert("card_slug".into(), g(&r, "related_card_slug"));
                o.insert("message".into(), g(&r, "message"));
                o.insert("dismissed".into(), json!(truthy(Some(&g(&r, "dismissed_at")))));
                events.push(Value::Object(o));
            }
        }
    }

    // Chronological merge, newest first — stable sort keeps per-source push order (audit, ai,
    // tick, notification) as the tie-break, matching JS Array.prototype.sort's guaranteed stability.
    events.sort_by(|a, b| {
        let a_ts = a.get("ts").and_then(|v| v.as_str()).unwrap_or("");
        let b_ts = b.get("ts").and_then(|v| v.as_str()).unwrap_or("");
        b_ts.cmp(a_ts)
    });
    events.truncate(cap as usize);
    Ok(json!(events))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seed() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS brain;").unwrap();
        c.execute_batch(
            "CREATE TABLE sacred_timeline (id INTEGER PRIMARY KEY, ts TEXT, actor TEXT, operation TEXT, entity_type TEXT, entity_id TEXT, payload TEXT, digital_print TEXT); \
             CREATE TABLE brain.ai_calls (id INTEGER PRIMARY KEY, started_at TEXT, prompt_name TEXT, prompt_version INTEGER, tier TEXT, temperature TEXT, provider TEXT, model_id TEXT, outcome TEXT, cost_usd_actual REAL, tokens_in INTEGER, tokens_out INTEGER, duration_ms INTEGER, cached INTEGER); \
             CREATE TABLE brain.driver_ticks (id INTEGER PRIMARY KEY, started_at TEXT, ended_at TEXT, card_slug TEXT, decided_agent TEXT, decided_action TEXT, decided_reason TEXT, outcome TEXT, error_message TEXT, cost_usd REAL, duration_ms INTEGER); \
             CREATE TABLE notifications (id INTEGER PRIMARY KEY, ts TEXT, kind TEXT, severity TEXT, source TEXT, related_card_slug TEXT, message TEXT, dismissed_at TEXT);"
        ).unwrap();
        c
    }

    #[test]
    fn empty_db_returns_empty_array() {
        let c = seed();
        let r = substrateStream(&json!({}), &c).unwrap();
        assert_eq!(r, json!([]));
    }

    #[test]
    fn merges_all_sources_sorted_desc_with_field_mapping() {
        let c = seed();
        c.execute("INSERT INTO sacred_timeline (id, ts, actor, operation, entity_type, entity_id, payload, digital_print) VALUES (1, '2026-01-01 00:00:00', 'human:dani', 'card.add', 'card', 'CARD-1', 'p1', 'deadbeefcafe0000')", []).unwrap();
        c.execute("INSERT INTO brain.ai_calls (id, started_at, prompt_name, prompt_version, tier, temperature, provider, model_id, outcome, cost_usd_actual, tokens_in, tokens_out, duration_ms, cached) VALUES (1, '2026-01-01 00:01:00', 'plan', 3, 'mid', 'cold', 'anthropic', 'sonnet', 'success', 0.5, 100, 50, 900, 1)", []).unwrap();
        c.execute("INSERT INTO brain.driver_ticks (id, started_at, ended_at, card_slug, decided_agent, decided_action, decided_reason, outcome, error_message, cost_usd, duration_ms) VALUES (1, '2026-01-01 00:02:00', '2026-01-01 00:02:05', 'CARD-1', 'dev', 'card.update', 'next-step', 'fired', NULL, 0.1, 5000)", []).unwrap();
        c.execute("INSERT INTO notifications (id, ts, kind, severity, source, related_card_slug, message, dismissed_at) VALUES (1, '2026-01-01 00:03:00', 'generic', 'warn', 'driver', 'CARD-1', 'watch out', NULL)", []).unwrap();

        let r = substrateStream(&json!({}), &c).unwrap();
        let arr = r.as_array().unwrap();
        assert_eq!(arr.len(), 4);
        // newest first: notification(00:03) > tick(00:02) > ai(00:01) > audit(00:00)
        assert_eq!(arr[0]["source"], json!("notification"));
        assert_eq!(arr[0]["id"], json!("notif-1"));
        assert_eq!(arr[0]["notif_source"], json!("driver"));
        assert_eq!(arr[0]["dismissed"], json!(false));
        assert_eq!(arr[1]["source"], json!("tick"));
        assert_eq!(arr[1]["id"], json!("tick-1"));
        assert_eq!(arr[2]["source"], json!("ai"));
        assert_eq!(arr[2]["id"], json!("ai-1"));
        assert_eq!(arr[2]["cached"], json!(true));
        assert_eq!(arr[2]["cost_usd"], json!(0.5));
        assert_eq!(arr[3]["source"], json!("audit"));
        assert_eq!(arr[3]["id"], json!("audit-1"));
        assert_eq!(arr[3]["digital_print_short"], json!("deadbeefcafe"));
    }

    #[test]
    fn since_ts_filters_and_kinds_restricts_sources() {
        let c = seed();
        c.execute("INSERT INTO sacred_timeline (id, ts, actor, operation, entity_type, entity_id, payload, digital_print) VALUES (1, '2025-12-01 00:00:00', 'human:dani', 'card.add', 'card', 'OLD', 'p', 'dp'), (2, '2026-01-05 00:00:00', 'human:dani', 'card.add', 'card', 'NEW', 'p', 'dp2')", []).unwrap();
        c.execute("INSERT INTO brain.driver_ticks (id, started_at, ended_at, card_slug, decided_agent, decided_action, decided_reason, outcome, error_message, cost_usd, duration_ms) VALUES (1, '2026-01-05 00:00:00', NULL, 'NEW', 'dev', 'card.add', 'r', 'fired', NULL, 0, 10)", []).unwrap();

        let r = substrateStream(&json!({ "since_ts": "2026-01-01 00:00:00", "kinds": ["audit"] }), &c).unwrap();
        let arr = r.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["entity_id"], json!("NEW"));
    }

    #[test]
    fn kinds_non_array_or_empty_excludes_everything() {
        let c = seed();
        c.execute("INSERT INTO sacred_timeline (id, ts, actor, operation, entity_type, entity_id, payload, digital_print) VALUES (1, '2026-01-01 00:00:00', 'human:dani', 'card.add', 'card', 'X', 'p', 'dp')", []).unwrap();

        let non_array = substrateStream(&json!({ "kinds": "audit" }), &c).unwrap();
        assert_eq!(non_array, json!([]));
        let empty_array = substrateStream(&json!({ "kinds": [] }), &c).unwrap();
        assert_eq!(empty_array, json!([]));
    }

    #[test]
    fn limit_clamps_and_per_source_limit_applies_before_overall_sort() {
        let c = seed();
        // id DESC LIMIT 2 picks ids 3,2 (ts d2,d3) — id 1 (ts d1) is dropped BEFORE the ts sort.
        c.execute("INSERT INTO sacred_timeline (id, ts, actor, operation, entity_type, entity_id, payload, digital_print) VALUES \
            (1, '2026-01-01 00:00:01', 'a', 'op', 'card', 'A', NULL, 'd1'), \
            (2, '2026-01-01 00:00:03', 'a', 'op', 'card', 'B', NULL, 'd2'), \
            (3, '2026-01-01 00:00:02', 'a', 'op', 'card', 'C', NULL, 'd3')", []).unwrap();

        let r = substrateStream(&json!({ "limit": 2 }), &c).unwrap();
        let arr = r.as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0]["entity_id"], json!("B")); // ts 00:00:03, highest among ids {2,3}
        assert_eq!(arr[1]["entity_id"], json!("C")); // ts 00:00:02
    }

    #[test]
    fn limit_zero_and_over_1000_and_negative_clamp() {
        let c = seed();
        c.execute("INSERT INTO sacred_timeline (id, ts, actor, operation, entity_type, entity_id, payload, digital_print) VALUES (1, '2026-01-01 00:00:00', 'a', 'op', 'card', 'X', NULL, 'd1')", []).unwrap();

        assert_eq!(substrateStream(&json!({ "limit": 0 }), &c).unwrap().as_array().unwrap().len(), 1);
        assert_eq!(substrateStream(&json!({ "limit": 5000 }), &c).unwrap().as_array().unwrap().len(), 1);
        assert_eq!(substrateStream(&json!({ "limit": -5 }), &c).unwrap().as_array().unwrap().len(), 1);
    }
}
