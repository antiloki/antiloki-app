//! law-violation-record.rs — twin of skills/entities/law-violation-record.mjs (db WRITE lane,
//! cross-db laws via ATTACH). NAMES ARE CONTRACT: `lawViolationRecord` (default),
//! `ALLOWED_OUTCOMES` (data). Logs an attempted law breach into laws.law_violations and returns
//! the inserted row (SELECT * — attempted_at fires from the column DEFAULT, frozen in fixtures).
use rusqlite::Connection;
use serde_json::{json, Value};

pub const ALLOWED_OUTCOMES: [&str; 4] = ["blocked", "warned", "escaped", "allowed_exception"];

fn truthy(v: Option<&Value>) -> bool { match v { None|Some(Value::Null)=>false, Some(Value::Bool(b))=>*b, Some(Value::Number(n))=>n.as_f64().map_or(false,|f|f!=0.0&&!f.is_nan()), Some(Value::String(s))=>!s.is_empty(), Some(_)=>true } }
fn tmpl(v: Option<&Value>) -> String { match v { None=>"undefined".into(), Some(Value::Null)=>"null".into(), Some(Value::String(s))=>s.clone(), Some(Value::Bool(b))=>b.to_string(), Some(n@Value::Number(_))=>n.to_string(), Some(o)=>o.to_string() } }

pub fn lawViolationRecord(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS: `const { db, actor = 'unknown' } = context ?? {}` — the db lane's context is `{ db }`,
    // so `actor` always destructures to its default here.
    let actor = "unknown";

    let law_id = input.get("law_id");
    // Destructure defaults fire on ABSENT (undefined) only — an explicit null stays null.
    let attempted_by = match input.get("attempted_by") { Some(v) => v.clone(), None => json!(actor) };
    let agent_name = input.get("agent_name").cloned().unwrap_or(Value::Null);
    let action_name = input.get("action_name").cloned().unwrap_or(Value::Null);
    let violation_context = input.get("context").cloned().unwrap_or(Value::Null); // JS `context: violationContext = null`
    let outcome = input.get("outcome");

    // JS: Number.isInteger(law_id) — a Number with integral value (JSON `5.0` parses integral in V8).
    let law_id_is_int = match law_id {
        Some(Value::Number(n)) => n.as_i64().is_some() || n.as_f64().map_or(false, |f| f.is_finite() && f.fract() == 0.0),
        _ => false,
    };
    if !law_id_is_int { return Err("law-violation-record: law_id is required".into()); }
    if !truthy(outcome) { return Err("law-violation-record: outcome is required".into()); }
    let outcome_v = outcome.expect("truthy implies present");
    if !outcome_v.as_str().map_or(false, |s| ALLOWED_OUTCOMES.contains(&s)) {
        return Err(format!(
            "law-violation-record: outcome must be one of: {} (got '{}')",
            ALLOWED_OUTCOMES.join(", "), tmpl(outcome)
        ));
    }

    let law_id_v = law_id.expect("integer-checked").clone();
    let law_rows = crate::db_json::query_rows(db, "SELECT id FROM laws.laws WHERE id = ?", &[law_id_v.clone()])?;
    if law_rows.as_array().map_or(true, |a| a.is_empty()) {
        return Err(format!("law-violation-record: law id '{}' not found", tmpl(law_id)));
    }

    let context_json = if violation_context.is_null() { Value::Null } else {
        Value::String(serde_json::to_string(&violation_context).map_err(|e| e.to_string())?)
    };

    let (_changes, last_id) = crate::db_json::run_write(
        db,
        "INSERT INTO laws.law_violations\n       (law_id, attempted_by, agent_name, action_name, context_json, outcome)\n     VALUES (?, ?, ?, ?, ?, ?)",
        &[law_id_v, attempted_by, agent_name, action_name, context_json, outcome_v.clone()],
    )?;

    let rows = crate::db_json::query_rows(db, "SELECT * FROM laws.law_violations WHERE id = ?", &[json!(last_id)])?;
    // JS `.get()` → row object, or undefined (→ null through the lane) if somehow absent.
    Ok(rows.as_array().and_then(|a| a.first()).cloned().unwrap_or(Value::Null))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS laws; \
          CREATE TABLE laws.laws (id INTEGER PRIMARY KEY, slug TEXT, kind TEXT, domain TEXT); \
          CREATE TABLE laws.law_violations (id INTEGER PRIMARY KEY, law_id INTEGER NOT NULL, attempted_at TEXT NOT NULL DEFAULT '2026-01-01 00:00:00', attempted_by TEXT NOT NULL, agent_name TEXT, action_name TEXT, context_json TEXT, outcome TEXT NOT NULL, CHECK (outcome IN ('blocked', 'warned', 'escaped', 'allowed_exception'))); \
          INSERT INTO laws.laws (id,slug,kind,domain) VALUES \
            (1,'NEVER_BYPASS_WITH_AUDIT','constitutional','audit'), \
            (2,'ASK_BEFORE_DESTRUCTIVE','procedural','security');").unwrap();
        c
    }

    #[test]
    fn records_and_returns_inserted_row() {
        let c = db();
        c.execute_batch("INSERT INTO laws.law_violations (id,law_id,attempted_at,attempted_by,agent_name,action_name,context_json,outcome) VALUES \
          (1,2,'2025-12-30 11:00:00','agent:dev','dev','dev.card.delete',NULL,'escaped');").unwrap();
        let row = lawViolationRecord(&json!({
            "law_id": 1, "attempted_by": "agent:dev", "agent_name": "dev",
            "action_name": "dev.file.write", "context": { "path": ".git/HEAD", "hook": "law-hook-no-secrets-in-commit" },
            "outcome": "blocked"
        }), &c).unwrap();
        // full-row key order + values are the contract (SELECT * over the schema column order)
        assert_eq!(serde_json::to_string(&row).unwrap(),
            "{\"id\":2,\"law_id\":1,\"attempted_at\":\"2026-01-01 00:00:00\",\"attempted_by\":\"agent:dev\",\"agent_name\":\"dev\",\"action_name\":\"dev.file.write\",\"context_json\":\"{\\\"path\\\":\\\".git/HEAD\\\",\\\"hook\\\":\\\"law-hook-no-secrets-in-commit\\\"}\",\"outcome\":\"blocked\"}");
    }

    #[test]
    fn defaults_attempted_by_unknown_and_nulls() {
        let c = db();
        let row = lawViolationRecord(&json!({ "law_id": 1, "outcome": "warned" }), &c).unwrap();
        assert_eq!(row["id"], json!(1));
        assert_eq!(row["attempted_by"], json!("unknown")); // context.actor default in the db lane
        assert_eq!(row["agent_name"], Value::Null);
        assert_eq!(row["action_name"], Value::Null);
        assert_eq!(row["context_json"], Value::Null);
        assert_eq!(row["attempted_at"], json!("2026-01-01 00:00:00"));
    }

    #[test]
    fn context_serialization_variants() {
        let c = db();
        let s = lawViolationRecord(&json!({ "law_id": 2, "attempted_by": "human:dani", "context": "raw note", "outcome": "escaped" }), &c).unwrap();
        assert_eq!(s["context_json"], json!("\"raw note\"")); // JSON.stringify of a string
        let a = lawViolationRecord(&json!({ "law_id": 1, "agent_name": "sec", "context": [1, "two", null], "outcome": "allowed_exception" }), &c).unwrap();
        assert_eq!(a["context_json"], json!("[1,\"two\",null]"));
        let n = lawViolationRecord(&json!({ "law_id": 1, "attempted_by": "agent:qa", "context": null, "outcome": "blocked" }), &c).unwrap();
        assert_eq!(n["context_json"], Value::Null); // explicit null context → null column
        assert_eq!(n["id"], json!(3)); // lastInsertRowid advances across inserts
    }

    #[test]
    fn guards_reject_invalid_input() {
        let c = db();
        assert!(lawViolationRecord(&json!({ "outcome": "blocked" }), &c).is_err()); // law_id missing
        assert!(lawViolationRecord(&json!({ "law_id": "1", "outcome": "blocked" }), &c).is_err()); // string law_id fails Number.isInteger
        assert!(lawViolationRecord(&json!({ "law_id": 1 }), &c).is_err()); // outcome missing
        assert!(lawViolationRecord(&json!({ "law_id": 1, "outcome": "denied" }), &c).is_err()); // enum
        assert!(lawViolationRecord(&json!({ "law_id": 99, "outcome": "blocked" }), &c).is_err()); // law not found
        assert!(lawViolationRecord(&Value::Null, &c).is_err()); // input ?? {} → law_id required
        // present-but-null attempted_by is NOT defaulted (destructure default fires on absent only) → NOT NULL rejects
        assert!(lawViolationRecord(&json!({ "law_id": 1, "attempted_by": null, "outcome": "blocked" }), &c).is_err());
    }
}
