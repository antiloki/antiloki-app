//! brain-fire-start.rs — twin of skills/entities/brain-fire-start.mjs (db WRITE lane, attached
//! `brain` db). NAMES ARE CONTRACT: `brainFireStart` (default). Marks an agent fire as in-flight:
//! INSERT a brain_fires row (ended_at/outcome NULL), then SELECT * the new row back by rowid.
use rusqlite::Connection;
use serde_json::{json, Value};

// JS truthiness (the `!session_id` / `!agent` / `!action` guards).
fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}

pub fn brainFireStart(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS throws err.contextMissing('brain-fire-start','db') when no db — the db lane always
    // injects one, so that guard is unreachable here.
    let obj = input.as_object();
    let get = |k: &str| obj.and_then(|o| o.get(k));
    let session_id = get("session_id").cloned().unwrap_or(Value::Null);
    let agent = get("agent").cloned().unwrap_or(Value::Null);
    let action = get("action").cloned().unwrap_or(Value::Null);
    // JS destructure defaults (`= null`) fire only on absent keys; explicit null stays null —
    // both land on Value::Null, exactly JS's result.
    let card_slug = get("card_slug").cloned().unwrap_or(Value::Null);
    let inferred_state = get("inferred_state").cloned().unwrap_or(Value::Null);

    if !truthy(Some(&session_id)) {
        return Err("brain-fire-start: session_id is required".into());
    }
    if !truthy(Some(&agent)) {
        return Err("brain-fire-start: agent is required".into());
    }
    if !truthy(Some(&action)) {
        return Err("brain-fire-start: action is required".into());
    }

    let (_changes, rowid) = crate::db_json::run_write(
        db,
        "INSERT INTO brain.brain_fires\n       (session_id, card_slug, agent, action, inferred_state)\n     VALUES (?, ?, ?, ?, ?)",
        &[session_id, card_slug, agent, action, inferred_state],
    )?;
    let rows = crate::db_json::query_rows(db, "SELECT * FROM brain.brain_fires WHERE id = ?", &[json!(rowid)])?;
    Ok(rows.as_array().and_then(|a| a.first()).cloned().unwrap_or(Value::Null))
}

#[cfg(test)]
mod tests {
    use super::*;

    // Real brain_fires shape with the fired_at DEFAULT frozen the same way the fixture does.
    const SCHEMA: &str = "CREATE TABLE brain.brain_fires ( id INTEGER PRIMARY KEY AUTOINCREMENT, session_id TEXT NOT NULL, fired_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')), ended_at TEXT, card_slug TEXT, agent TEXT NOT NULL, action TEXT NOT NULL, inferred_state TEXT, duration_ms INTEGER, tokens_in INTEGER, tokens_out INTEGER, cost_usd REAL, outcome TEXT, result_summary TEXT, CHECK (outcome IN ('success','failed','timeout','rejected','truncated') OR outcome IS NULL) )";

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS brain").unwrap();
        c.execute_batch(SCHEMA).unwrap();
        c
    }
    fn count(c: &Connection) -> i64 {
        c.query_row("SELECT COUNT(*) FROM brain.brain_fires", [], |r| r.get(0)).unwrap()
    }

    #[test]
    fn inserts_in_flight_row_with_null_ended_at_and_outcome() {
        let c = db();
        let r = brainFireStart(&json!({ "session_id": "S-1", "agent": "pl", "action": "pl.plan" }), &c).unwrap();
        assert_eq!(
            r,
            json!({ "id": 1, "session_id": "S-1", "fired_at": "2026-01-01 00:00:00", "ended_at": null,
                    "card_slug": null, "agent": "pl", "action": "pl.plan", "inferred_state": null,
                    "duration_ms": null, "tokens_in": null, "tokens_out": null, "cost_usd": null,
                    "outcome": null, "result_summary": null })
        );
        assert_eq!(count(&c), 1);
    }

    #[test]
    fn carries_card_slug_and_inferred_state_when_given() {
        let c = db();
        let r = brainFireStart(
            &json!({ "session_id": "S-2", "agent": "dev", "action": "dev.card.add", "card_slug": "CARD-9", "inferred_state": "focused" }),
            &c,
        )
        .unwrap();
        assert_eq!(r["card_slug"], json!("CARD-9"));
        assert_eq!(r["inferred_state"], json!("focused"));
    }

    #[test]
    fn explicit_null_optionals_behave_like_absent() {
        let c = db();
        let r = brainFireStart(
            &json!({ "session_id": "S-3", "agent": "man", "action": "man.card.add", "card_slug": null, "inferred_state": null }),
            &c,
        )
        .unwrap();
        assert_eq!(r["card_slug"], Value::Null);
        assert_eq!(r["inferred_state"], Value::Null);
    }

    #[test]
    fn returns_the_new_row_not_a_preexisting_one() {
        let c = db();
        c.execute(
            "INSERT INTO brain.brain_fires (session_id, fired_at, ended_at, agent, action, outcome) VALUES ('S-0', '2025-12-31 10:00:00', '2025-12-31 10:05:00', 'dev', 'dev.file.write', 'success')",
            [],
        )
        .unwrap();
        let r = brainFireStart(&json!({ "session_id": "S-4", "agent": "qa", "action": "qa.review" }), &c).unwrap();
        assert_eq!(r["id"], json!(2));
        assert_eq!(r["session_id"], json!("S-4"));
        assert_eq!(count(&c), 2);
    }

    #[test]
    fn rejects_missing_required_fields() {
        let c = db();
        assert!(brainFireStart(&json!({ "agent": "pl", "action": "pl.plan" }), &c).is_err());
        assert!(brainFireStart(&json!({ "session_id": "S-5", "action": "pl.plan" }), &c).is_err());
        assert!(brainFireStart(&json!({ "session_id": "S-5", "agent": "pl" }), &c).is_err());
        assert_eq!(count(&c), 0);
    }
}
