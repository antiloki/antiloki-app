//! brain-decision-record.rs — twin of skills/entities/brain-decision-record.mjs (db WRITE lane,
//! attached `brain` db). NAMES ARE CONTRACT: `brainDecisionRecord` (default). Records a single
//! driver/agent decision: validates decision against the ALLOWED kinds, INSERTs a
//! brain_decisions row (inputs stringified for deterministic replay), SELECT * the row back.
use rusqlite::Connection;
use serde_json::{json, Value};

// All decision kinds this skill may record — session_start/session_end go through their own skills.
const ALLOWED: [&str; 14] = [
    // Driver loop kinds (original)
    "fire", "wait", "rollback", "escalate", "done", "pause", "goal_set",
    // FEAT-AGENT-REASONING-LOG: agent reasoning kinds
    "classify", "gate_verdict", "rule_match", "sandbox_create",
    "review_pass", "schedule_and_dispatch", "agent_step",
];

// JS truthiness (the `!session_id` / `!decision` / `!rationale` guards).
fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}
// JS `${v}` interpolation for the enum-error message.
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
// JS `typeof v` for the values a JSON input can carry (the type-error message).
fn js_typeof(v: &Value) -> &'static str {
    match v {
        Value::Null | Value::Array(_) | Value::Object(_) => "object",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
    }
}
// JS Number.isInteger(v): only Number values with no fractional part qualify.
fn is_js_integer(v: &Value) -> bool {
    match v {
        Value::Number(n) => n.as_f64().map_or(false, |f| f.is_finite() && f.fract() == 0.0),
        _ => false,
    }
}

pub fn brainDecisionRecord(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS throws err.contextMissing('brain-decision-record','db') when no db — the db lane always
    // injects one, so that guard is unreachable here.
    let obj = input.as_object();
    let get = |k: &str| obj.and_then(|o| o.get(k));
    let session_id = get("session_id").cloned().unwrap_or(Value::Null);
    let decision = get("decision").cloned().unwrap_or(Value::Null);
    let rationale = get("rationale").cloned().unwrap_or(Value::Null);
    // JS destructure defaults are null anyway — absent and explicit null land the same.
    let card_slug = get("card_slug").cloned().unwrap_or(Value::Null);
    let inputs = get("inputs").cloned().unwrap_or(Value::Null);
    let fire_id = get("fire_id").cloned().unwrap_or(Value::Null);

    if !truthy(Some(&session_id)) {
        return Err("brain-decision-record: session_id is required".into());
    }
    if !truthy(Some(&decision)) {
        return Err("brain-decision-record: decision is required".into());
    }
    if !decision.as_str().map_or(false, |s| ALLOWED.contains(&s)) {
        return Err(format!(
            "brain-decision-record: decision must be one of: {} (got '{}')",
            ALLOWED.join(", "),
            tmpl(Some(&decision))
        ));
    }
    if !truthy(Some(&rationale)) {
        return Err("brain-decision-record: rationale is required".into());
    }
    if !fire_id.is_null() && !is_js_integer(&fire_id) {
        return Err(format!(
            "brain-decision-record: fire_id must be an integer (got {})",
            js_typeof(&fire_id)
        ));
    }

    // inputs == null ? null : JSON.stringify(inputs) — serde compact form matches JSON.stringify
    // (fixture key order is preserved end-to-end via serde preserve_order).
    let inputs_json = if inputs.is_null() {
        Value::Null
    } else {
        json!(serde_json::to_string(&inputs).map_err(|e| e.to_string())?)
    };

    let (_changes, rowid) = crate::db_json::run_write(
        db,
        "INSERT INTO brain.brain_decisions\n       (session_id, card_slug, decision, rationale, inputs_json, fire_id)\n     VALUES (?, ?, ?, ?, ?, ?)",
        &[session_id, card_slug, decision, rationale, inputs_json, fire_id],
    )?;
    let rows = crate::db_json::query_rows(db, "SELECT * FROM brain.brain_decisions WHERE id = ?", &[json!(rowid)])?;
    Ok(rows.as_array().and_then(|a| a.first()).cloned().unwrap_or(Value::Null))
}

#[cfg(test)]
mod tests {
    use super::*;

    // Real brain_decisions shape with the timestamp DEFAULT frozen the same way the fixture does.
    const SCHEMA_DECISIONS: &str = "CREATE TABLE brain.brain_decisions ( id INTEGER PRIMARY KEY AUTOINCREMENT, session_id TEXT NOT NULL, decided_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')), card_slug TEXT, decision TEXT NOT NULL, rationale TEXT NOT NULL, inputs_json TEXT, fire_id INTEGER, CHECK (decision IN ('fire','wait','rollback','escalate','done','pause','session_start','session_end','goal_set','classify','gate_verdict','rule_match','sandbox_create','review_pass','schedule_and_dispatch','agent_step')) )";

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS brain").unwrap();
        c.execute_batch(SCHEMA_DECISIONS).unwrap();
        c
    }

    #[test]
    fn inserts_a_basic_decision_with_null_defaults() {
        let c = db();
        let r = brainDecisionRecord(
            &json!({ "session_id": "S-1", "decision": "wait", "rationale": "card locked by another session" }),
            &c,
        )
        .unwrap();
        assert_eq!(
            r,
            json!({ "id": 1, "session_id": "S-1", "decided_at": "2026-01-01 00:00:00", "card_slug": null,
                    "decision": "wait", "rationale": "card locked by another session",
                    "inputs_json": null, "fire_id": null })
        );
    }

    #[test]
    fn accepts_every_allowed_decision() {
        for d in ALLOWED {
            let c = db();
            let r = brainDecisionRecord(
                &json!({ "session_id": "S-A", "decision": d, "rationale": format!("recording {d}") }),
                &c,
            )
            .unwrap();
            assert_eq!(r["decision"], json!(d));
        }
    }

    #[test]
    fn stores_inputs_as_json_preserving_key_order() {
        let c = db();
        let r = brainDecisionRecord(
            &json!({ "session_id": "S-2", "decision": "fire", "rationale": "TL gate", "card_slug": "CARD-9",
                     "inputs": { "current_phase": "tl_review", "iter_count": 2, "retrieved_memories": [42, 17], "remaining_budget": 4.5 } }),
            &c,
        )
        .unwrap();
        assert_eq!(
            r["inputs_json"],
            json!("{\"current_phase\":\"tl_review\",\"iter_count\":2,\"retrieved_memories\":[42,17],\"remaining_budget\":4.5}")
        );
        assert_eq!(r["card_slug"], json!("CARD-9"));
    }

    #[test]
    fn links_to_fire_id_and_counts_rowid_past_seeded_rows() {
        let c = db();
        c.execute(
            "INSERT INTO brain.brain_decisions (session_id, decided_at, decision, rationale, inputs_json) VALUES ('S-3', '2025-12-31 09:00:00', 'fire', 'earlier', '{}')",
            [],
        )
        .unwrap();
        let r = brainDecisionRecord(
            &json!({ "session_id": "S-3", "decision": "fire", "rationale": "fire DEV", "fire_id": 42 }),
            &c,
        )
        .unwrap();
        assert_eq!(r["id"], json!(2));
        assert_eq!(r["fire_id"], json!(42));
        let n: i64 = c.query_row("SELECT COUNT(*) FROM brain.brain_decisions", [], |row| row.get(0)).unwrap();
        assert_eq!(n, 2);
    }

    #[test]
    fn rejects_missing_required_fields() {
        let c = db();
        assert!(brainDecisionRecord(&json!({ "decision": "wait", "rationale": "x" }), &c)
            .unwrap_err()
            .contains("session_id"));
        assert!(brainDecisionRecord(&json!({ "session_id": "s", "rationale": "x" }), &c)
            .unwrap_err()
            .contains("decision"));
        assert!(brainDecisionRecord(&json!({ "session_id": "s", "decision": "wait" }), &c)
            .unwrap_err()
            .contains("rationale"));
        let n: i64 = c.query_row("SELECT COUNT(*) FROM brain.brain_decisions", [], |row| row.get(0)).unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn rejects_unknown_and_session_lifecycle_decisions() {
        let c = db();
        let e = brainDecisionRecord(&json!({ "session_id": "s", "decision": "whoops", "rationale": "x" }), &c).unwrap_err();
        assert!(e.contains("decision must be one of"));
        assert!(e.contains("(got 'whoops')"));
        // session_start is NOT in ALLOWED for direct insertion via this skill
        let e2 = brainDecisionRecord(&json!({ "session_id": "s", "decision": "session_start", "rationale": "x" }), &c).unwrap_err();
        assert!(e2.contains("decision must be one of"));
    }

    #[test]
    fn rejects_non_integer_fire_id() {
        let c = db();
        let e = brainDecisionRecord(
            &json!({ "session_id": "s", "decision": "wait", "rationale": "x", "fire_id": 4.5 }),
            &c,
        )
        .unwrap_err();
        assert_eq!(e, "brain-decision-record: fire_id must be an integer (got number)");
        let e2 = brainDecisionRecord(
            &json!({ "session_id": "s", "decision": "wait", "rationale": "x", "fire_id": "42" }),
            &c,
        )
        .unwrap_err();
        assert_eq!(e2, "brain-decision-record: fire_id must be an integer (got string)");
    }
}
