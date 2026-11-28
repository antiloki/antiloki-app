//! brain-session-end.rs — twin of skills/entities/brain-session-end.mjs (db WRITE lane, attached
//! `brain` db). NAMES ARE CONTRACT: `brainSessionEnd` (default). Closes a brain session: aggregates
//! fire totals from brain_fires, INSERTs a brain_decisions row (decision='session_end') whose
//! inputs_json carries the totals, then SELECT * the new row back by rowid.
use rusqlite::Connection;
use serde_json::{json, Map, Value};

// JS truthiness (the `!session_id` guard).
fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}
// JS `${v}` interpolation (status may be an explicit null; fire_count is a number).
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
// JS Number(v) for the aggregate cell (COUNT/SUM come back as integer or real).
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
// JS Number.prototype.toFixed(4) for the finite values that occur here. NOTE: Rust rounds
// exact 4-decimal ties half-to-even where JS picks the larger — fixtures use tie-free sums.
fn to_fixed4(x: f64) -> String { format!("{:.4}", x) }

pub fn brainSessionEnd(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS throws err.contextMissing('brain-session-end','db') when no db — the db lane always
    // injects one, so that guard is unreachable here.
    let obj = input.as_object();
    let get = |k: &str| obj.and_then(|o| o.get(k));
    let session_id = get("session_id").cloned().unwrap_or(Value::Null);
    // JS destructure defaults fire only on ABSENT keys: an explicit null status STAYS null
    // (rationale reads "Session null: ..."); notes lands on null either way.
    let status = get("status").cloned().unwrap_or(json!("completed"));
    let notes = get("notes").cloned().unwrap_or(Value::Null);

    if !truthy(Some(&session_id)) {
        return Err("brain-session-end: session_id is required".into());
    }

    let totals_rows = crate::db_json::query_rows(
        db,
        "SELECT COUNT(*) AS fire_count,\n            COALESCE(SUM(cost_usd), 0) AS total_cost_usd,\n            COALESCE(SUM(tokens_in), 0) AS total_tokens_in,\n            COALESCE(SUM(tokens_out), 0) AS total_tokens_out\n       FROM brain.brain_fires\n      WHERE session_id = ?",
        &[session_id.clone()],
    )?;
    let totals = totals_rows.as_array().and_then(|a| a.first()).cloned().unwrap_or(Value::Null);

    // Same insertion order as the JS object literal — key order is part of inputs_json.
    let mut inputs = Map::new();
    inputs.insert("status".into(), status.clone());
    inputs.insert("total_fires".into(), totals.get("fire_count").cloned().unwrap_or(Value::Null));
    inputs.insert("total_cost_usd".into(), totals.get("total_cost_usd").cloned().unwrap_or(Value::Null));
    inputs.insert("total_tokens_in".into(), totals.get("total_tokens_in").cloned().unwrap_or(Value::Null));
    inputs.insert("total_tokens_out".into(), totals.get("total_tokens_out").cloned().unwrap_or(Value::Null));
    inputs.insert("notes".into(), notes);
    let inputs_json = serde_json::to_string(&Value::Object(inputs)).map_err(|e| e.to_string())?;

    let rationale = format!(
        "Session {}: {} fires, ${}",
        tmpl(Some(&status)),
        tmpl(totals.get("fire_count")),
        to_fixed4(js_number(totals.get("total_cost_usd")))
    );

    let (_changes, rowid) = crate::db_json::run_write(
        db,
        "INSERT INTO brain.brain_decisions\n       (session_id, decision, rationale, inputs_json)\n     VALUES (?, ?, ?, ?)",
        &[session_id, json!("session_end"), json!(rationale), json!(inputs_json)],
    )?;
    let rows = crate::db_json::query_rows(db, "SELECT * FROM brain.brain_decisions WHERE id = ?", &[json!(rowid)])?;
    Ok(rows.as_array().and_then(|a| a.first()).cloned().unwrap_or(Value::Null))
}

#[cfg(test)]
mod tests {
    use super::*;

    // Real brain shapes with the timestamp DEFAULTs frozen the same way the fixture does.
    const SCHEMA_FIRES: &str = "CREATE TABLE brain.brain_fires ( id INTEGER PRIMARY KEY AUTOINCREMENT, session_id TEXT NOT NULL, fired_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')), ended_at TEXT, card_slug TEXT, agent TEXT NOT NULL, action TEXT NOT NULL, inferred_state TEXT, duration_ms INTEGER, tokens_in INTEGER, tokens_out INTEGER, cost_usd REAL, outcome TEXT, result_summary TEXT, CHECK (outcome IN ('success','failed','timeout','rejected','truncated') OR outcome IS NULL) )";
    const SCHEMA_DECISIONS: &str = "CREATE TABLE brain.brain_decisions ( id INTEGER PRIMARY KEY AUTOINCREMENT, session_id TEXT NOT NULL, decided_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')), card_slug TEXT, decision TEXT NOT NULL, rationale TEXT NOT NULL, inputs_json TEXT, fire_id INTEGER, CHECK (decision IN ('fire','wait','rollback','escalate','done','pause','session_start','session_end','goal_set','classify','gate_verdict','rule_match','sandbox_create','review_pass','schedule_and_dispatch','agent_step')) )";

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS brain").unwrap();
        c.execute_batch(SCHEMA_FIRES).unwrap();
        c.execute_batch(SCHEMA_DECISIONS).unwrap();
        c
    }
    fn seed_fire(c: &Connection, sid: &str, cost: Option<f64>, tin: Option<i64>, tout: Option<i64>) {
        c.execute(
            "INSERT INTO brain.brain_fires (session_id, fired_at, agent, action, tokens_in, tokens_out, cost_usd, outcome) VALUES (?, '2025-12-31 10:00:00', 'pl', 'pl.plan', ?, ?, ?, 'success')",
            rusqlite::params![sid, tin, tout, cost],
        )
        .unwrap();
    }

    #[test]
    fn closes_with_zero_fire_totals() {
        let c = db();
        let r = brainSessionEnd(&json!({ "session_id": "S-1" }), &c).unwrap();
        assert_eq!(
            r,
            json!({ "id": 1, "session_id": "S-1", "decided_at": "2026-01-01 00:00:00", "card_slug": null,
                    "decision": "session_end",
                    "rationale": "Session completed: 0 fires, $0.0000",
                    "inputs_json": "{\"status\":\"completed\",\"total_fires\":0,\"total_cost_usd\":0,\"total_tokens_in\":0,\"total_tokens_out\":0,\"notes\":null}",
                    "fire_id": null })
        );
    }

    #[test]
    fn aggregates_fires_skipping_null_cells_and_other_sessions() {
        let c = db();
        seed_fire(&c, "S-2", Some(0.25), Some(100), Some(50));
        seed_fire(&c, "S-2", Some(0.5), Some(80), Some(20));
        seed_fire(&c, "S-2", None, None, None); // COUNT counts it, SUM skips the NULLs
        seed_fire(&c, "S-other", Some(9.0), Some(999), Some(999)); // filtered out by WHERE
        let r = brainSessionEnd(&json!({ "session_id": "S-2" }), &c).unwrap();
        assert_eq!(r["rationale"], json!("Session completed: 3 fires, $0.7500"));
        assert_eq!(
            r["inputs_json"],
            json!("{\"status\":\"completed\",\"total_fires\":3,\"total_cost_usd\":0.75,\"total_tokens_in\":180,\"total_tokens_out\":70,\"notes\":null}")
        );
    }

    #[test]
    fn accepts_status_override_and_notes() {
        let c = db();
        let r = brainSessionEnd(&json!({ "session_id": "S-3", "status": "aborted", "notes": "user cancelled" }), &c).unwrap();
        assert_eq!(r["rationale"], json!("Session aborted: 0 fires, $0.0000"));
        assert_eq!(
            r["inputs_json"],
            json!("{\"status\":\"aborted\",\"total_fires\":0,\"total_cost_usd\":0,\"total_tokens_in\":0,\"total_tokens_out\":0,\"notes\":\"user cancelled\"}")
        );
    }

    #[test]
    fn explicit_null_status_stays_null() {
        let c = db();
        let r = brainSessionEnd(&json!({ "session_id": "S-4", "status": null }), &c).unwrap();
        assert_eq!(r["rationale"], json!("Session null: 0 fires, $0.0000"));
        assert!(r["inputs_json"].as_str().unwrap().starts_with("{\"status\":null,"));
    }

    #[test]
    fn returns_the_new_row_not_a_preexisting_one() {
        let c = db();
        c.execute(
            "INSERT INTO brain.brain_decisions (session_id, decided_at, decision, rationale, inputs_json) VALUES ('S-5', '2025-12-31 09:00:00', 'session_start', 'Session started', '{}')",
            [],
        )
        .unwrap();
        let r = brainSessionEnd(&json!({ "session_id": "S-5" }), &c).unwrap();
        assert_eq!(r["id"], json!(2));
        assert_eq!(r["decision"], json!("session_end"));
        let n: i64 = c.query_row("SELECT COUNT(*) FROM brain.brain_decisions", [], |row| row.get(0)).unwrap();
        assert_eq!(n, 2);
    }

    #[test]
    fn rejects_missing_session_id() {
        let c = db();
        assert!(brainSessionEnd(&json!({}), &c).is_err());
        assert!(brainSessionEnd(&json!({ "session_id": null }), &c).is_err());
        assert!(brainSessionEnd(&json!({ "session_id": "" }), &c).is_err());
        let n: i64 = c.query_row("SELECT COUNT(*) FROM brain.brain_decisions", [], |row| row.get(0)).unwrap();
        assert_eq!(n, 0);
    }
}
