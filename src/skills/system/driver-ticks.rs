//! driver-ticks.rs — twin of skills/system/driver-ticks.mjs (db READ, attach lane).
//! NAMES ARE CONTRACT: `driverTicks` (default). Recent driver tick history with full detail
//! (the FE Pipeline "Recent ticks" panel), newest first; optional since_id / outcome /
//! card_slug filters; limit defaults 30, clamped 1..=200 via `Number(limit) || 30`.
// QUALITY: 76/100 — dynamic WHERE assembly + JS numeric coercion quirks (0/null/NaN → default 30)
use rusqlite::Connection;
use serde_json::{json, Value};

fn truthy(v: Option<&Value>) -> bool { match v { None|Some(Value::Null)=>false, Some(Value::Bool(b))=>*b, Some(Value::Number(n))=>n.as_f64().map_or(false,|f|f!=0.0&&!f.is_nan()), Some(Value::String(s))=>!s.is_empty(), Some(_)=>true } }
fn js_string(v:&Value)->String{ match v{Value::Null=>"null".into(),Value::String(s)=>s.clone(),Value::Number(n)=>n.to_string(),Value::Bool(b)=>b.to_string(),Value::Array(a)=>a.iter().map(js_string).collect::<Vec<_>>().join(","),Value::Object(_)=>"[object Object]".into()} } // = JS String(v)
fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) } // JS `!= null`
fn js_number(v: Option<&Value>) -> f64 { match v { None=>f64::NAN, Some(Value::Null)=>0.0, Some(Value::Bool(b))=>if *b {1.0} else {0.0}, Some(Value::Number(n))=>n.as_f64().unwrap_or(f64::NAN), Some(Value::String(s))=>{let t=s.trim(); if t.is_empty(){0.0}else{t.parse().unwrap_or(f64::NAN)}}, Some(_)=>f64::NAN } } // = JS Number(v)

// node:sqlite binds integer-valued JS numbers as INTEGER; NaN/Infinity land as NULL (json! → Null)
fn num_param(v: f64) -> Value { if v.is_finite() && v.fract() == 0.0 { json!(v as i64) } else { json!(v) } }

pub fn driverTicks(input: &Value, db: &Connection) -> Result<Value, String> {
    // const { limit = 30, since_id, outcome, card_slug } = input ?? {};
    // cappedLimit = Math.min(Math.max(1, Number(limit) || 30), 200)
    let n = match input.get("limit") { None => 30.0, v => js_number(v) };
    let n = if n == 0.0 || n.is_nan() { 30.0 } else { n }; // JS `Number(limit) || 30`
    let capped_limit = n.max(1.0).min(200.0);

    let mut where_parts: Vec<&str> = Vec::new();
    let mut params: Vec<Value> = Vec::new();
    if let Some(sid) = nz(input.get("since_id")) {
        where_parts.push("id < @since_id");
        params.push(num_param(js_number(Some(sid))));
    }
    if truthy(input.get("outcome")) {
        where_parts.push("outcome = @outcome");
        params.push(json!(js_string(input.get("outcome").unwrap())));
    }
    if truthy(input.get("card_slug")) {
        where_parts.push("card_slug = @card_slug");
        params.push(json!(js_string(input.get("card_slug").unwrap())));
    }
    let where_clause = if where_parts.is_empty() { String::new() } else { format!("WHERE {}", where_parts.join(" AND ")) };

    let sql = format!(
        "\n    SELECT id, started_at, ended_at, card_slug, decided_agent, decided_action,\n           decided_reason, outcome, cost_usd, duration_ms, error_message,\n           fired_event_id\n    FROM brain.driver_ticks\n    {}\n    ORDER BY id DESC\n    LIMIT @limit\n  ",
        where_clause
    );
    // named placeholders bind positionally in first-appearance order — same order as pushed above
    params.push(num_param(capped_limit));
    crate::db_json::query_rows(db, &sql, &params)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS brain; \
          CREATE TABLE brain.driver_ticks ( id INTEGER PRIMARY KEY AUTOINCREMENT, started_at TEXT NOT NULL, card_slug TEXT, decided_agent TEXT, decided_action TEXT, decided_reason TEXT, fired_event_id INTEGER, outcome TEXT NOT NULL DEFAULT 'running', cost_usd REAL DEFAULT 0, duration_ms INTEGER, ended_at TEXT, error_message TEXT, facts_json TEXT ); \
          INSERT INTO brain.driver_ticks (id, started_at, ended_at, card_slug, decided_agent, decided_action, decided_reason, outcome, cost_usd, duration_ms, error_message, fired_event_id) VALUES \
            (1, '2025-12-30 10:00:00', '2025-12-30 10:00:04', 'CARD-A', 'dev', 'dev.card.work',   'rule: doing-card', 'fired',   0.5,  4000, NULL,   7), \
            (2, '2025-12-30 10:01:00', '2025-12-30 10:01:02', 'CARD-B', 'man', NULL,              'rule: failure',    'failed',  NULL, 2000, 'boom', NULL), \
            (3, '2025-12-30 10:02:00', '2025-12-30 10:02:01', 'CARD-A', 'dev', 'dev.card.work',   'rule: doing-card', 'fired',   0.25, 1000, NULL,   9), \
            (4, '2025-12-30 10:03:00', NULL,                  NULL,     NULL,  NULL,              'rule: idle',       'idle',    NULL, NULL, NULL,   NULL);").unwrap();
        c
    }
    fn ids(r: &Value) -> Vec<i64> { r.as_array().unwrap().iter().map(|t| t["id"].as_i64().unwrap()).collect() }

    #[test]
    fn newest_first_with_full_columns() {
        let c = db();
        let r = driverTicks(&json!({}), &c).unwrap();
        assert_eq!(ids(&r), vec![4, 3, 2, 1]);
        // row objects keep SELECT column order
        let keys: Vec<String> = r[3].as_object().unwrap().keys().cloned().collect();
        assert_eq!(keys, vec!["id", "started_at", "ended_at", "card_slug", "decided_agent", "decided_action", "decided_reason", "outcome", "cost_usd", "duration_ms", "error_message", "fired_event_id"]);
        assert_eq!(r[3]["cost_usd"], json!(0.5));
        assert_eq!(r[3]["fired_event_id"], json!(7));
        assert_eq!(r[2]["error_message"], json!("boom"));
    }

    #[test]
    fn filters_since_id_outcome_card_slug() {
        let c = db();
        assert_eq!(ids(&driverTicks(&json!({ "since_id": 3 }), &c).unwrap()), vec![2, 1]);
        // Number() coercion of a string since_id
        assert_eq!(ids(&driverTicks(&json!({ "since_id": "3" }), &c).unwrap()), vec![2, 1]);
        // since_id: null → `!= null` skips the filter
        assert_eq!(ids(&driverTicks(&json!({ "since_id": null }), &c).unwrap()), vec![4, 3, 2, 1]);
        assert_eq!(ids(&driverTicks(&json!({ "outcome": "fired" }), &c).unwrap()), vec![3, 1]);
        // outcome: "" is falsy → no filter
        assert_eq!(ids(&driverTicks(&json!({ "outcome": "" }), &c).unwrap()), vec![4, 3, 2, 1]);
        assert_eq!(ids(&driverTicks(&json!({ "card_slug": "CARD-A" }), &c).unwrap()), vec![3, 1]);
        assert_eq!(ids(&driverTicks(&json!({ "since_id": 3, "outcome": "fired", "card_slug": "CARD-A" }), &c).unwrap()), vec![1]);
    }

    #[test]
    fn limit_clamp_and_defaults() {
        let c = db();
        assert_eq!(ids(&driverTicks(&json!({ "limit": 2 }), &c).unwrap()), vec![4, 3]);
        assert_eq!(ids(&driverTicks(&json!({ "limit": "3" }), &c).unwrap()), vec![4, 3, 2]); // string coercion
        assert_eq!(ids(&driverTicks(&json!({ "limit": 0 }), &c).unwrap()), vec![4, 3, 2, 1]); // 0 || 30 → default
        assert_eq!(ids(&driverTicks(&json!({ "limit": null }), &c).unwrap()), vec![4, 3, 2, 1]); // Number(null)=0 → default
        assert_eq!(ids(&driverTicks(&json!({ "limit": 999 }), &c).unwrap()), vec![4, 3, 2, 1]); // capped 200
        assert_eq!(ids(&driverTicks(&json!({ "limit": -5 }), &c).unwrap()), vec![4]); // Math.max(1, -5) → 1
    }

    #[test]
    fn empty_table() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS brain; \
          CREATE TABLE brain.driver_ticks ( id INTEGER PRIMARY KEY AUTOINCREMENT, started_at TEXT NOT NULL, card_slug TEXT, decided_agent TEXT, decided_action TEXT, decided_reason TEXT, fired_event_id INTEGER, outcome TEXT NOT NULL DEFAULT 'running', cost_usd REAL DEFAULT 0, duration_ms INTEGER, ended_at TEXT, error_message TEXT, facts_json TEXT );").unwrap();
        assert_eq!(driverTicks(&json!({}), &c).unwrap(), json!([]));
    }
}
