//! brain-fire-record.rs — twin of skills/entities/brain-fire-record.mjs (db WRITE lane, attached
//! `brain` db). NAMES ARE CONTRACT: `brainFireRecord` (default). Completes an in-flight fire:
//! UPDATE the brain_fires row with terminal values (last write wins — a re-record overwrites,
//! and fields you don't pass become NULL), then SELECT * the row back.
use rusqlite::Connection;
use serde_json::Value;

// JS `const ALLOWED_OUTCOMES = [...]` (exported data const; also feeds the enumVal message).
const ALLOWED_OUTCOMES: [&str; 5] = ["success", "failed", "timeout", "rejected", "truncated"];

// JS truthiness (the `!outcome` guard).
fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}
// JS `${v}` interpolation (the enumVal/notFound messages interpolate got/value).
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
// JS `typeof v` for the err.type message ("got undefined" / "got object" / "got string" / …).
fn js_typeof(v: Option<&Value>) -> &'static str {
    match v {
        None => "undefined",
        Some(Value::Null) => "object",
        Some(Value::Bool(_)) => "boolean",
        Some(Value::Number(_)) => "number",
        Some(Value::String(_)) => "string",
        Some(_) => "object",
    }
}

pub fn brainFireRecord(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS throws err.contextMissing('brain-fire-record','db') when no db — the db lane always
    // injects one, so that guard is unreachable here.
    let obj = input.as_object();
    let get = |k: &str| obj.and_then(|o| o.get(k));
    let fire_id = get("fire_id").cloned().unwrap_or(Value::Null);
    let outcome = get("outcome").cloned().unwrap_or(Value::Null);
    // JS destructure defaults (`= null`) fire only on absent keys; explicit null stays null —
    // both land on Value::Null, exactly JS's result.
    let duration_ms = get("duration_ms").cloned().unwrap_or(Value::Null);
    let tokens_in = get("tokens_in").cloned().unwrap_or(Value::Null);
    let tokens_out = get("tokens_out").cloned().unwrap_or(Value::Null);
    let cost_usd = get("cost_usd").cloned().unwrap_or(Value::Null);
    let result_summary = get("result_summary").cloned().unwrap_or(Value::Null);
    let ended_at = get("ended_at").cloned().unwrap_or(Value::Null);

    // Number.isInteger(fire_id): a JS number with no fractional part ("5" and 5.5 both fail).
    let is_integer = matches!(&fire_id, Value::Number(n)
        if n.as_i64().is_some() || n.as_u64().is_some()
            || n.as_f64().map_or(false, |f| f.is_finite() && f.fract() == 0.0));
    if !is_integer {
        return Err(format!(
            "brain-fire-record: fire_id must be an integer (got {})",
            js_typeof(get("fire_id"))
        ));
    }
    if !truthy(Some(&outcome)) {
        return Err("brain-fire-record: outcome is required".into());
    }
    if !outcome.as_str().map_or(false, |s| ALLOWED_OUTCOMES.contains(&s)) {
        return Err(format!(
            "brain-fire-record: outcome must be one of: {} (got '{}')",
            ALLOWED_OUTCOMES.join(", "),
            tmpl(Some(&outcome))
        ));
    }

    let existing = crate::db_json::query_rows(db, "SELECT id FROM brain.brain_fires WHERE id = ?", &[fire_id.clone()])?;
    if existing.as_array().map_or(true, |a| a.is_empty()) {
        return Err(format!("brain-fire-record: fire id '{}' not found", tmpl(Some(&fire_id))));
    }

    // JS SQL says datetime('now') — the frozen-clock lane pins the executed instant on both sides.
    crate::db_json::run_write(
        db,
        &format!("UPDATE brain.brain_fires\n       SET ended_at       = COALESCE(?, {}),\n           duration_ms    = ?,\n           tokens_in      = ?,\n           tokens_out     = ?,\n           cost_usd       = ?,\n           outcome        = ?,\n           result_summary = ?\n     WHERE id = ?", crate::clock::now_sql()),
        &[ended_at, duration_ms, tokens_in, tokens_out, cost_usd, outcome, result_summary, fire_id.clone()],
    )?;

    let rows = crate::db_json::query_rows(db, "SELECT * FROM brain.brain_fires WHERE id = ?", &[fire_id])?;
    Ok(rows.as_array().and_then(|a| a.first()).cloned().unwrap_or(Value::Null))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // Real brain_fires shape with the fired_at DEFAULT frozen the same way the fixture does.
    const SCHEMA: &str = "CREATE TABLE brain.brain_fires ( id INTEGER PRIMARY KEY AUTOINCREMENT, session_id TEXT NOT NULL, fired_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')), ended_at TEXT, card_slug TEXT, agent TEXT NOT NULL, action TEXT NOT NULL, inferred_state TEXT, duration_ms INTEGER, tokens_in INTEGER, tokens_out INTEGER, cost_usd REAL, outcome TEXT, result_summary TEXT, CHECK (outcome IN ('success','failed','timeout','rejected','truncated') OR outcome IS NULL) )";

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS brain").unwrap();
        c.execute_batch(SCHEMA).unwrap();
        c
    }
    fn seed_in_flight(c: &Connection, sid: &str) -> i64 {
        c.execute(
            "INSERT INTO brain.brain_fires (session_id, fired_at, agent, action) VALUES (?, '2025-12-31 10:00:00', 'pl', 'pl.plan')",
            rusqlite::params![sid],
        )
        .unwrap();
        c.last_insert_rowid()
    }

    #[test]
    fn records_terminal_values_with_explicit_ended_at() {
        let c = db();
        let id = seed_in_flight(&c, "S-1");
        let r = brainFireRecord(
            &json!({ "fire_id": id, "outcome": "success", "duration_ms": 1234, "tokens_in": 100,
                     "tokens_out": 50, "cost_usd": 0.0042, "result_summary": "planned 3 cards",
                     "ended_at": "2026-01-01 00:05:00" }),
            &c,
        )
        .unwrap();
        assert_eq!(
            r,
            json!({ "id": 1, "session_id": "S-1", "fired_at": "2025-12-31 10:00:00",
                    "ended_at": "2026-01-01 00:05:00", "card_slug": null, "agent": "pl",
                    "action": "pl.plan", "inferred_state": null, "duration_ms": 1234,
                    "tokens_in": 100, "tokens_out": 50, "cost_usd": 0.0042,
                    "outcome": "success", "result_summary": "planned 3 cards" })
        );
    }

    #[test]
    fn null_ended_at_coalesces_to_the_frozen_now() {
        let c = db();
        let id = seed_in_flight(&c, "S-2");
        let r = brainFireRecord(&json!({ "fire_id": id, "outcome": "failed" }), &c).unwrap();
        assert_eq!(r["ended_at"], json!("2026-01-01 00:00:00"));
        assert_eq!(r["outcome"], json!("failed"));
        assert_eq!(r["duration_ms"], Value::Null);
        assert_eq!(r["cost_usd"], Value::Null);
    }

    #[test]
    fn explicit_null_optionals_behave_like_absent() {
        let c = db();
        let id = seed_in_flight(&c, "S-3");
        let r = brainFireRecord(
            &json!({ "fire_id": id, "outcome": "timeout", "duration_ms": null, "tokens_in": null,
                     "tokens_out": null, "cost_usd": null, "result_summary": null, "ended_at": null }),
            &c,
        )
        .unwrap();
        assert_eq!(r["ended_at"], json!("2026-01-01 00:00:00"));
        assert_eq!(r["tokens_in"], Value::Null);
        assert_eq!(r["result_summary"], Value::Null);
    }

    #[test]
    fn re_record_overwrites_last_write_wins_and_nulls_unpassed_fields() {
        let c = db();
        let id = seed_in_flight(&c, "S-4");
        brainFireRecord(
            &json!({ "fire_id": id, "outcome": "success", "duration_ms": 900, "cost_usd": 0.25,
                     "result_summary": "first", "ended_at": "2025-12-31 11:00:00" }),
            &c,
        )
        .unwrap();
        let r = brainFireRecord(&json!({ "fire_id": id, "outcome": "rejected", "tokens_in": 7 }), &c).unwrap();
        assert_eq!(r["outcome"], json!("rejected"));
        assert_eq!(r["tokens_in"], json!(7));
        assert_eq!(r["duration_ms"], Value::Null); // not passed the second time → NULL
        assert_eq!(r["result_summary"], Value::Null);
        assert_eq!(r["ended_at"], json!("2026-01-01 00:00:00")); // COALESCE refires on the frozen now
    }

    #[test]
    fn targets_only_the_given_fire_id() {
        let c = db();
        let a = seed_in_flight(&c, "S-5a");
        let b = seed_in_flight(&c, "S-5b");
        let r = brainFireRecord(&json!({ "fire_id": b, "outcome": "truncated" }), &c).unwrap();
        assert_eq!(r["id"], json!(b));
        let untouched: Option<String> =
            c.query_row("SELECT outcome FROM brain.brain_fires WHERE id = ?", [a], |row| row.get(0)).unwrap();
        assert_eq!(untouched, None);
    }

    #[test]
    fn rejects_bad_inputs() {
        let c = db();
        let id = seed_in_flight(&c, "S-6");
        assert!(brainFireRecord(&json!({ "outcome": "success" }), &c).is_err()); // fire_id absent
        assert!(brainFireRecord(&json!({ "fire_id": "1", "outcome": "success" }), &c).is_err()); // string id
        assert!(brainFireRecord(&json!({ "fire_id": id }), &c).is_err()); // outcome missing
        assert!(brainFireRecord(&json!({ "fire_id": id, "outcome": "exploded" }), &c).is_err()); // bad enum
        assert!(brainFireRecord(&json!({ "fire_id": 999, "outcome": "success" }), &c).is_err()); // not found
        let still_open: Option<String> =
            c.query_row("SELECT ended_at FROM brain.brain_fires WHERE id = ?", [id], |row| row.get(0)).unwrap();
        assert_eq!(still_open, None); // no guard path wrote anything
    }
}
