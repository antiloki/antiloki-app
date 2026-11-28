//! ai-outcome-report.rs — twin of skills/ai/ai-outcome-report.mjs (db WRITE lane, cross-db
//! brain.ai_calls). NAMES ARE CONTRACT: `aiOutcomeReport` (default). HEIMDALL L0: a call site
//! downgrades an ai_call's outcome_class after ai-call already stamped 'success' provisionally.
// QUALITY: single UPDATE guarded by cached=0; { ok, ai_call_id, outcome_class, updated }.
use rusqlite::Connection;
use serde_json::{json, Value};

// JS Number(v) coercion (used for `Number(input?.ai_call_id)`).
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
// = JS String(v ?? '') (used for `String(input?.outcome_class ?? '')`).
fn str_qq(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(),
        Some(o) => o.to_string(),
    }
}
// A JS Number that survived Number.isFinite() renders as an integer in JSON.stringify when it's
// integral (no ".0"), else as a float — mirrors what `{ ai_call_id }` serializes to in JS.
fn num_json(f: f64) -> Value { if f.fract() == 0.0 { json!(f as i64) } else { json!(f) } }

pub fn aiOutcomeReport(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS also guards `if (!db) throw err.contextMissing(...)` — db is always present in this lane.
    let ai_call_id_f = js_number(input.get("ai_call_id"));
    let outcome_class = str_qq(input.get("outcome_class"));
    if !ai_call_id_f.is_finite() || ai_call_id_f <= 0.0 {
        return Err("ai-outcome-report: ai_call_id is required".into());
    }
    if !matches!(outcome_class.as_str(), "degenerate" | "truncated" | "validation_failed") {
        return Err(format!(
            "ai-outcome-report: outcome_class must be one of: degenerate, truncated, validation_failed (got '{}')",
            outcome_class
        ));
    }
    let ai_call_id_json = num_json(ai_call_id_f);
    let (changes, _) = crate::db_json::run_write(
        db,
        "UPDATE brain.ai_calls SET outcome_class = ? WHERE id = ? AND cached = 0",
        &[json!(outcome_class), ai_call_id_json.clone()],
    )?;
    Ok(json!({
        "ok": true,
        "ai_call_id": ai_call_id_json,
        "outcome_class": outcome_class,
        "updated": changes > 0,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "ATTACH ':memory:' AS brain; \
             CREATE TABLE brain.ai_calls (id INTEGER PRIMARY KEY, outcome_class TEXT, cached INTEGER);",
        )
        .unwrap();
        c
    }

    #[test]
    fn updates_uncached_call_and_reports_changed() {
        let c = db();
        c.execute_batch("INSERT INTO brain.ai_calls (id, outcome_class, cached) VALUES (10, NULL, 0);").unwrap();
        let r = aiOutcomeReport(&json!({ "ai_call_id": 10, "outcome_class": "degenerate" }), &c).unwrap();
        assert_eq!(r, json!({ "ok": true, "ai_call_id": 10, "outcome_class": "degenerate", "updated": true }));
        let stored: String = c.query_row("SELECT outcome_class FROM brain.ai_calls WHERE id = 10", [], |row| row.get(0)).unwrap();
        assert_eq!(stored, "degenerate");
    }

    #[test]
    fn cached_call_is_not_updated_but_still_reports_ok() {
        let c = db();
        c.execute_batch("INSERT INTO brain.ai_calls (id, outcome_class, cached) VALUES (11, NULL, 1);").unwrap();
        let r = aiOutcomeReport(&json!({ "ai_call_id": 11, "outcome_class": "truncated" }), &c).unwrap();
        assert_eq!(r, json!({ "ok": true, "ai_call_id": 11, "outcome_class": "truncated", "updated": false }));
        let stored: Option<String> = c.query_row("SELECT outcome_class FROM brain.ai_calls WHERE id = 11", [], |row| row.get(0)).unwrap();
        assert_eq!(stored, None);
    }

    #[test]
    fn missing_row_reports_ok_with_updated_false() {
        let c = db();
        let r = aiOutcomeReport(&json!({ "ai_call_id": 999, "outcome_class": "validation_failed" }), &c).unwrap();
        assert_eq!(r, json!({ "ok": true, "ai_call_id": 999, "outcome_class": "validation_failed", "updated": false }));
    }

    #[test]
    fn string_ai_call_id_is_number_coerced_in_output() {
        let c = db();
        c.execute_batch("INSERT INTO brain.ai_calls (id, outcome_class, cached) VALUES (12, NULL, 0);").unwrap();
        let r = aiOutcomeReport(&json!({ "ai_call_id": "12", "outcome_class": "degenerate" }), &c).unwrap();
        assert_eq!(r["ai_call_id"], json!(12));
    }

    #[test]
    fn rejects_bad_input() {
        let c = db();
        assert!(aiOutcomeReport(&json!({ "ai_call_id": 0, "outcome_class": "degenerate" }), &c).is_err());
        assert!(aiOutcomeReport(&json!({ "ai_call_id": 1, "outcome_class": "not-reportable" }), &c).is_err());
    }
}
