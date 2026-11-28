//! heimdall-trust-set.rs — twin of skills/heimdall/heimdall-trust-set.mjs (db WRITE lane,
//! cross-db, async-over-sync). NAMES ARE CONTRACT: `heimdallTrustSet` (default). COMMITS a
//! trust override (the human half of AI-3 self-calibration, same contract as ceiling.set: the
//! report RECOMMENDS, the operator COMMITS) — null/'' clears the override back to the seeded
//! PROVENANCE_TRUST default that ledger-rebuild falls back to.
// QUALITY: 82/100 — enum-gated upsert/delete over the (provenance, category) override table

use rusqlite::Connection;
use serde_json::{json, Map, Value};

// Standard helper functions (playbook §Standard helper functions) — copied verbatim (subset used).
fn str_qq(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(),
        Some(o) => o.to_string(),
    }
}
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
fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}
fn js_number(v: Option<&Value>) -> f64 {
    match v {
        None => f64::NAN,
        Some(Value::Null) => 0.0,
        Some(Value::Bool(b)) => if *b { 1.0 } else { 0.0 },
        Some(Value::Number(n)) => n.as_f64().unwrap_or(f64::NAN),
        Some(Value::String(s)) => { let t = s.trim(); if t.is_empty() { 0.0 } else { t.parse().unwrap_or(f64::NAN) } }
        Some(_) => f64::NAN,
    }
}
// JS `${count}`/JSON output for a genuinely fractional-or-whole computed number (playbook gotcha 2).
fn js_num_value(f: f64) -> Value { if f.is_finite() && f == f.trunc() && f.abs() < 1e15 { json!(f as i64) } else { json!(f) } }

// Plan §5.2 evidence-trust provenance keys — mirrors casting.mjs PROVENANCE_TRUST's key SET
// (heimdall-trust-set only validates membership; the weight values are ledger-rebuild's concern).
const PROVENANCE_KEYS: [&str; 10] = [
    "human", "prod_gate", "prod_terminal", "prod_court", "ai_judge",
    "ai_panel", "bench_det", "bench_rubric", "playground", "lineage_prior",
];

pub fn heimdallTrustSet(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS: `const { db } = context ?? {}; if (!db) throw err.contextMissing('heimdall-trust-set','db')`
    // — the db lane always injects a live connection, so this guard is unreachable here.

    // `String(input?.provenance ?? '')` = str_qq.
    let provenance = str_qq(input.get("provenance"));
    // JS throws err.enumVal(...) when provenance isn't a PROVENANCE_TRUST key — a guard-throw,
    // not exercised by fixtures (only success paths are, gotcha 9).
    if !PROVENANCE_KEYS.contains(&provenance.as_str()) {
        return Err(format!("heimdall-trust-set: provenance must be one of: {} (got '{}')", PROVENANCE_KEYS.join(", "), provenance));
    }
    // `input?.category ? String(input.category) : ''` — truthy-gated coercion ('' = provenance-global).
    let category = if truthy(input.get("category")) { js_string(input.get("category").unwrap()) } else { String::new() };
    let category_out = if category.is_empty() { Value::Null } else { json!(category.clone()) };

    let v = input.get("trust");
    // `v == null || v === ''` — JS loose-null covers both an absent key and an explicit null.
    let is_clear = match v {
        None | Some(Value::Null) => true,
        Some(Value::String(s)) => s.is_empty(),
        _ => false,
    };
    if is_clear {
        db.execute(
            "DELETE FROM brain.heimdall_trust_overrides WHERE provenance = ? AND category = ?",
            rusqlite::params![provenance, category],
        ).map_err(|e| e.to_string())?;
        let mut out = Map::new();
        out.insert("ok".into(), json!(true));
        out.insert("provenance".into(), json!(provenance));
        out.insert("category".into(), category_out);
        out.insert("trust".into(), Value::Null);
        out.insert("cleared".into(), json!(true));
        return Ok(Value::Object(out));
    }

    let trust = js_number(v);
    // JS throws err.range(...) when trust isn't finite in [0,1] — a guard-throw, not exercised
    // by fixtures (gotcha 9).
    if !trust.is_finite() || trust < 0.0 || trust > 1.0 {
        return Err("heimdall-trust-set: trust must be between 0 and 1".to_string());
    }
    // `input?.reason_md ? String(input.reason_md).slice(0, 500) : null`
    let reason_md: Option<String> = if truthy(input.get("reason_md")) {
        Some(js_string(input.get("reason_md").unwrap()).chars().take(500).collect())
    } else {
        None
    };
    // FROZEN-CLOCK LANE: `datetime('now')` → the pinned instant (playbook datetime-skills rule).
    db.execute(
        &format!("INSERT INTO brain.heimdall_trust_overrides (provenance, category, trust, reason_md, updated_at) \
         VALUES (?, ?, ?, ?, {}) \
         ON CONFLICT(provenance, category) DO UPDATE SET trust = excluded.trust, reason_md = excluded.reason_md, updated_at = {}", crate::clock::now_sql(), crate::clock::now_sql()),
        rusqlite::params![provenance, category, trust, reason_md],
    ).map_err(|e| e.to_string())?;

    let mut out = Map::new();
    out.insert("ok".into(), json!(true));
    out.insert("provenance".into(), json!(provenance));
    out.insert("category".into(), category_out);
    out.insert("trust".into(), js_num_value(trust));
    out.insert("cleared".into(), json!(false));
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db_with_brain() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "ATTACH ':memory:' AS brain;
             CREATE TABLE brain.heimdall_trust_overrides (
               provenance TEXT NOT NULL,
               category TEXT NOT NULL DEFAULT '',
               trust REAL NOT NULL CHECK (trust >= 0 AND trust <= 1),
               reason_md TEXT,
               updated_at TEXT NOT NULL DEFAULT (datetime('now')),
               PRIMARY KEY (provenance, category)
             );",
        )
        .unwrap();
        c
    }

    fn row_count(c: &Connection, provenance: &str, category: &str) -> i64 {
        c.query_row(
            "SELECT COUNT(*) FROM brain.heimdall_trust_overrides WHERE provenance = ? AND category = ?",
            rusqlite::params![provenance, category],
            |r| r.get(0),
        )
        .unwrap()
    }

    #[test]
    fn invalid_provenance_errs() {
        let c = db_with_brain();
        let r = heimdallTrustSet(&json!({ "provenance": "not-a-real-tier" }), &c);
        assert!(r.is_err());
    }

    #[test]
    fn missing_trust_clears_provenance_global_override() {
        let c = db_with_brain();
        c.execute(
            "INSERT INTO brain.heimdall_trust_overrides (provenance, category, trust, updated_at) VALUES ('ai_judge','',0.4,'2025-12-01 00:00:00')",
            [],
        )
        .unwrap();
        let r = heimdallTrustSet(&json!({ "provenance": "ai_judge" }), &c).unwrap();
        assert_eq!(r, json!({ "ok": true, "provenance": "ai_judge", "category": null, "trust": null, "cleared": true }));
        assert_eq!(row_count(&c, "ai_judge", ""), 0);
    }

    #[test]
    fn empty_string_trust_clears_categorized_override() {
        let c = db_with_brain();
        c.execute(
            "INSERT INTO brain.heimdall_trust_overrides (provenance, category, trust, reason_md, updated_at) VALUES ('ai_panel','architecture',0.6,'old','2025-12-01 00:00:00')",
            [],
        )
        .unwrap();
        let r = heimdallTrustSet(&json!({ "provenance": "ai_panel", "category": "architecture", "trust": "" }), &c).unwrap();
        assert_eq!(r, json!({ "ok": true, "provenance": "ai_panel", "category": "architecture", "trust": null, "cleared": true }));
        assert_eq!(row_count(&c, "ai_panel", "architecture"), 0);
    }

    #[test]
    fn write_inserts_provenance_global_row_with_null_category_output() {
        let c = db_with_brain();
        let r = heimdallTrustSet(&json!({ "provenance": "prod_court", "trust": 0.4 }), &c).unwrap();
        assert_eq!(r, json!({ "ok": true, "provenance": "prod_court", "category": null, "trust": 0.4, "cleared": false }));
        let (trust, reason_md): (f64, Option<String>) = c
            .query_row(
                "SELECT trust, reason_md FROM brain.heimdall_trust_overrides WHERE provenance='prod_court' AND category=''",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(trust, 0.4);
        assert_eq!(reason_md, None);
    }

    #[test]
    fn conflict_updates_existing_row_whole_number_trust_and_long_reason_md_is_sliced_to_500() {
        let c = db_with_brain();
        c.execute(
            "INSERT INTO brain.heimdall_trust_overrides (provenance, category, trust, reason_md, updated_at) VALUES ('human','planning',0.6,'prior note','2025-11-01 00:00:00')",
            [],
        )
        .unwrap();
        let long_reason = "a".repeat(510);
        let r = heimdallTrustSet(&json!({ "provenance": "human", "category": "planning", "trust": 1, "reason_md": long_reason }), &c).unwrap();
        assert_eq!(r, json!({ "ok": true, "provenance": "human", "category": "planning", "trust": 1, "cleared": false }));
        let (trust, reason_md, updated_at): (f64, String, String) = c
            .query_row(
                "SELECT trust, reason_md, updated_at FROM brain.heimdall_trust_overrides WHERE provenance='human' AND category='planning'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(trust, 1.0);
        assert_eq!(reason_md.len(), 500);
        assert!(reason_md.chars().all(|ch| ch == 'a'));
        assert_eq!(updated_at, "2026-01-01 00:00:00");
        assert_eq!(row_count(&c, "human", "planning"), 1);
    }

    #[test]
    fn string_numeric_trust_is_coerced_like_js_number() {
        let c = db_with_brain();
        let r = heimdallTrustSet(&json!({ "provenance": "lineage_prior", "trust": "0.15" }), &c).unwrap();
        assert_eq!(r, json!({ "ok": true, "provenance": "lineage_prior", "category": null, "trust": 0.15, "cleared": false }));
    }

    #[test]
    fn zero_trust_is_a_valid_value_not_a_clear_signal() {
        // JS clear check is `v == null || v === ''` (strict-equal ''), so 0 falls through to the
        // insert branch — unlike heimdall-ceiling-set, where 0 IS a clear signal.
        let c = db_with_brain();
        let r = heimdallTrustSet(&json!({ "provenance": "bench_det", "trust": 0 }), &c).unwrap();
        assert_eq!(r, json!({ "ok": true, "provenance": "bench_det", "category": null, "trust": 0, "cleared": false }));
        assert_eq!(row_count(&c, "bench_det", ""), 1);
    }
}
