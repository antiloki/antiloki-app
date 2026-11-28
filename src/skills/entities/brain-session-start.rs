//! brain-session-start.rs — twin of skills/entities/brain-session-start.mjs (db WRITE lane,
//! attached `brain` db). NAMES ARE CONTRACT: `brainSessionStart` (default). Opens a driver session:
//! INSERTs a brain_decisions row (decision='session_start'), returns it with session_id spread on top.
use rusqlite::Connection;
use serde_json::{json, Map, Value};

// JS truthiness (the `!started_by` guard and `session_id ||` fallback).
fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}
// JS `${goal}` interpolation for the "Session started: <goal>" rationale.
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
// Fallback for `session_id || randomUUID()` when session_id is absent/falsy. NOT byte-identical
// to node:crypto's RNG — never exercised by conform fixtures (those always pass an explicit
// session_id, mirroring the interviewer-confirm-gate / atlas-smell-to-card precedent).
fn random_uuid() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos();
    let mut x = (nanos as u64) ^ 0x9E3779B97F4A7C15u64;
    let mut hex = String::new();
    for _ in 0..32 {
        x ^= x << 13; x ^= x >> 7; x ^= x << 17;
        hex.push(std::char::from_digit((x % 16) as u32, 16).unwrap());
    }
    format!("{}-{}-{}-{}-{}", &hex[0..8], &hex[8..12], &hex[12..16], &hex[16..20], &hex[20..32])
}

pub fn brainSessionStart(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS throws err.contextMissing('brain-session-start','db') when no db — the db lane always
    // injects one, so that guard is unreachable here.
    let obj = input.as_object();
    let get = |k: &str| obj.and_then(|o| o.get(k));
    let started_by = get("started_by").cloned().unwrap_or(Value::Null);
    let session_id = get("session_id").cloned().unwrap_or(Value::Null);
    let goal = get("goal").cloned().unwrap_or(Value::Null);
    let card_slug = get("card_slug").cloned().unwrap_or(Value::Null);

    if !truthy(Some(&started_by)) {
        return Err("brain-session-start: started_by is required".into());
    }

    let sid = if truthy(Some(&session_id)) { session_id.as_str().map(|s| s.to_string()).unwrap_or_else(random_uuid) } else { random_uuid() };

    // Same insertion order as the JS `{ goal, started_by }` object literal.
    let mut inputs = Map::new();
    inputs.insert("goal".into(), goal.clone());
    inputs.insert("started_by".into(), started_by.clone());
    let inputs_json = serde_json::to_string(&Value::Object(inputs)).map_err(|e| e.to_string())?;

    let rationale = if truthy(Some(&goal)) { format!("Session started: {}", tmpl(Some(&goal))) } else { "Session started".to_string() };

    let (_changes, rowid) = crate::db_json::run_write(
        db,
        "INSERT INTO brain.brain_decisions\n       (session_id, card_slug, decision, rationale, inputs_json)\n     VALUES (?, ?, ?, ?, ?)",
        &[json!(sid.clone()), card_slug, json!("session_start"), json!(rationale), json!(inputs_json)],
    )?;
    let rows = crate::db_json::query_rows(db, "SELECT * FROM brain.brain_decisions WHERE id = ?", &[json!(rowid)])?;
    let row = rows.as_array().and_then(|a| a.first()).cloned().unwrap_or(Value::Null);

    // JS `{ ...row, session_id: sid }` — spread the SELECTed row then overwrite session_id
    // (existing key position kept, new keys appended — row already has session_id so position stays).
    let mut out = row.as_object().cloned().unwrap_or_default();
    out.insert("session_id".into(), json!(sid));
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCHEMA_DECISIONS: &str = "CREATE TABLE brain.brain_decisions ( id INTEGER PRIMARY KEY AUTOINCREMENT, session_id TEXT NOT NULL, decided_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')), card_slug TEXT, decision TEXT NOT NULL, rationale TEXT NOT NULL, inputs_json TEXT, fire_id INTEGER, CHECK (decision IN ('fire','wait','rollback','escalate','done','pause','session_start','session_end','goal_set','classify','gate_verdict','rule_match','sandbox_create','review_pass','schedule_and_dispatch','agent_step')) )";

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS brain").unwrap();
        c.execute_batch(SCHEMA_DECISIONS).unwrap();
        c
    }

    #[test]
    fn inserts_decision_row_with_explicit_session_id() {
        let c = db();
        let r = brainSessionStart(&json!({ "started_by": "human:dani", "session_id": "custom-sid-001" }), &c).unwrap();
        assert_eq!(
            r,
            json!({ "id": 1, "session_id": "custom-sid-001", "decided_at": "2026-01-01 00:00:00", "card_slug": null,
                    "decision": "session_start", "rationale": "Session started",
                    "inputs_json": "{\"goal\":null,\"started_by\":\"human:dani\"}", "fire_id": null })
        );
    }

    #[test]
    fn stores_goal_and_started_by_in_inputs_json_and_rationale() {
        let c = db();
        let r = brainSessionStart(&json!({ "started_by": "human:dani", "session_id": "S-2", "goal": "drive PROJ-1 to done" }), &c).unwrap();
        assert_eq!(r["rationale"], json!("Session started: drive PROJ-1 to done"));
        assert_eq!(r["inputs_json"], json!("{\"goal\":\"drive PROJ-1 to done\",\"started_by\":\"human:dani\"}"));
    }

    #[test]
    fn accepts_card_slug() {
        let c = db();
        let r = brainSessionStart(&json!({ "started_by": "t", "session_id": "S-3", "card_slug": "PROJ-1" }), &c).unwrap();
        assert_eq!(r["card_slug"], json!("PROJ-1"));
    }

    #[test]
    fn rejects_missing_started_by() {
        let c = db();
        assert!(brainSessionStart(&json!({ "session_id": "S-4" }), &c).is_err());
        let n: i64 = c.query_row("SELECT COUNT(*) FROM brain.brain_decisions", [], |row| row.get(0)).unwrap();
        assert_eq!(n, 0);
    }
}
