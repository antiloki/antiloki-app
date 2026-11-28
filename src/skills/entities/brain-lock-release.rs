//! brain-lock-release.rs — twin of skills/entities/brain-lock-release.mjs (db WRITE lane, attached
//! `brain` db). NAMES ARE CONTRACT: `brainLockRelease` (default). Idempotent release: SELECT the
//! lock row (null when absent), owner-mismatch guard when holder_session given, DELETE + return row.
use rusqlite::Connection;
use serde_json::Value;

// JS truthiness (used for the `!resource_type` / `!resource_id` guards).
fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}
// = JS `${v}` (template-literal interpolation; only feeds the mismatch error message).
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

pub fn brainLockRelease(input: &Value, db: &Connection) -> Result<Value, String> {
    let obj = input.as_object();
    let get = |k: &str| obj.and_then(|o| o.get(k));
    let resource_type = get("resource_type").cloned().unwrap_or(Value::Null);
    let resource_id = get("resource_id").cloned().unwrap_or(Value::Null);
    // JS destructure default `holder_session = null` fires only when the key is ABSENT; an
    // explicit null stays null — both land on Value::Null here, which is exactly JS's result.
    let holder_session = get("holder_session").cloned().unwrap_or(Value::Null);
    if !truthy(Some(&resource_type)) {
        return Err("brain-lock-release: resource_type is required".into());
    }
    if !truthy(Some(&resource_id)) {
        return Err("brain-lock-release: resource_id is required".into());
    }

    let rows = crate::db_json::query_rows(
        db,
        "SELECT * FROM brain.brain_locks WHERE resource_type = ? AND resource_id = ?",
        &[resource_type.clone(), resource_id.clone()],
    )?;
    let existing = match rows.as_array().and_then(|a| a.first()).cloned() {
        None => return Ok(Value::Null), // idempotent: nothing to release
        Some(row) => row,
    };

    // JS: `holder_session != null && existing.holder_session !== holder_session` → throw.
    if !holder_session.is_null() {
        let held = existing.get("holder_session").cloned().unwrap_or(Value::Null);
        if held != holder_session {
            return Err(format!(
                "brain-lock-release: lock on {}/{} is held by '{}', not '{}'",
                tmpl(Some(&resource_type)),
                tmpl(Some(&resource_id)),
                tmpl(Some(&held)),
                tmpl(Some(&holder_session))
            ));
        }
    }

    crate::db_json::run_write(
        db,
        "DELETE FROM brain.brain_locks WHERE resource_type = ? AND resource_id = ?",
        &[resource_type, resource_id],
    )?;
    Ok(existing)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // Real brain_locks shape with the acquired_at DEFAULT frozen the same way the fixture does.
    const SCHEMA: &str = "CREATE TABLE brain.brain_locks ( resource_type TEXT NOT NULL, resource_id TEXT NOT NULL, state TEXT NOT NULL, holder_session TEXT NOT NULL, acquired_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')), expires_at TEXT, reason TEXT, PRIMARY KEY (resource_type, resource_id), CHECK (resource_type IN ('card', 'agent')), CHECK (state IN ('driving','thinking','executing','waiting')) )";

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS brain").unwrap();
        c.execute_batch(SCHEMA).unwrap();
        c
    }
    fn seed(c: &Connection, rt: &str, ri: &str, session: &str) {
        c.execute(
            "INSERT INTO brain.brain_locks (resource_type, resource_id, state, holder_session, acquired_at, expires_at, reason) VALUES (?, ?, 'driving', ?, '2025-12-31 23:00:00', NULL, 'work')",
            rusqlite::params![rt, ri, session],
        ).unwrap();
    }
    fn count(c: &Connection) -> i64 {
        c.query_row("SELECT COUNT(*) FROM brain.brain_locks", [], |r| r.get(0)).unwrap()
    }

    #[test]
    fn releases_and_returns_the_row() {
        let c = db();
        seed(&c, "card", "CARD-1", "session-a");
        let r = brainLockRelease(&json!({ "resource_type": "card", "resource_id": "CARD-1" }), &c).unwrap();
        assert_eq!(r, json!({ "resource_type": "card", "resource_id": "CARD-1", "state": "driving", "holder_session": "session-a", "acquired_at": "2025-12-31 23:00:00", "expires_at": null, "reason": "work" }));
        assert_eq!(count(&c), 0);
    }

    #[test]
    fn missing_lock_returns_null_idempotently() {
        let c = db();
        let r = brainLockRelease(&json!({ "resource_type": "card", "resource_id": "NOPE" }), &c).unwrap();
        assert_eq!(r, Value::Null);
    }

    #[test]
    fn matching_holder_releases_only_that_lock() {
        let c = db();
        seed(&c, "agent", "dev", "session-b");
        seed(&c, "card", "CARD-2", "session-c");
        let r = brainLockRelease(&json!({ "resource_type": "agent", "resource_id": "dev", "holder_session": "session-b" }), &c).unwrap();
        assert_eq!(r["holder_session"], json!("session-b"));
        assert_eq!(count(&c), 1); // CARD-2 untouched
    }

    #[test]
    fn explicit_null_holder_skips_ownership_check() {
        let c = db();
        seed(&c, "card", "CARD-3", "session-d");
        let r = brainLockRelease(&json!({ "resource_type": "card", "resource_id": "CARD-3", "holder_session": null }), &c).unwrap();
        assert_eq!(r["resource_id"], json!("CARD-3"));
        assert_eq!(count(&c), 0);
    }

    #[test]
    fn owner_mismatch_errors_and_keeps_the_lock() {
        let c = db();
        seed(&c, "card", "CARD-4", "session-e");
        let e = brainLockRelease(&json!({ "resource_type": "card", "resource_id": "CARD-4", "holder_session": "session-z" }), &c).unwrap_err();
        assert!(e.contains("held by 'session-e', not 'session-z'"));
        assert_eq!(count(&c), 1);
    }

    #[test]
    fn rejects_missing_required_fields() {
        let c = db();
        assert!(brainLockRelease(&json!({ "resource_id": "X" }), &c).is_err());
        assert!(brainLockRelease(&json!({ "resource_type": "card" }), &c).is_err());
    }
}
