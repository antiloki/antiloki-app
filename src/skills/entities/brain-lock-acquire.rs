//! brain-lock-acquire.rs — twin of skills/entities/brain-lock-acquire.mjs (db WRITE lane, attached
//! `brain` db). NAMES ARE CONTRACT: `brainLockAcquire` (default). Validates type/state enums,
//! throws LOCK_CONTENTION when a lock already exists on (resource_type, resource_id), else
//! INSERTs and returns the fresh row. expires_at absent → NULL ("no auto-expiry").
use rusqlite::Connection;
use serde_json::Value;

const ALLOWED_TYPES: [&str; 2] = ["card", "agent"];
const ALLOWED_STATES: [&str; 4] = ["driving", "thinking", "executing", "waiting"];

// JS truthiness (the `!resource_type` / `!resource_id` / `!state` / `!holder_session` guards).
fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}
// = JS `${v}` (template-literal interpolation; only feeds error messages).
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

pub fn brainLockAcquire(input: &Value, db: &Connection) -> Result<Value, String> {
    let obj = input.as_object();
    let get = |k: &str| obj.and_then(|o| o.get(k));
    let resource_type = get("resource_type").cloned().unwrap_or(Value::Null);
    let resource_id = get("resource_id").cloned().unwrap_or(Value::Null);
    let state = get("state").cloned().unwrap_or(Value::Null);
    let holder_session = get("holder_session").cloned().unwrap_or(Value::Null);
    // JS destructure defaults `expires_at = null` / `reason = null` fire only when the key is
    // ABSENT; an explicit null stays null — both land on Value::Null here, exactly JS's result.
    let expires_at = get("expires_at").cloned().unwrap_or(Value::Null);
    let reason = get("reason").cloned().unwrap_or(Value::Null);

    if !truthy(Some(&resource_type)) {
        return Err("brain-lock-acquire: resource_type is required".into());
    }
    // JS Array.includes is strict-equality: only a string can match the allowed list.
    if !resource_type.as_str().map_or(false, |s| ALLOWED_TYPES.contains(&s)) {
        return Err(format!(
            "brain-lock-acquire: resource_type must be one of: {} (got '{}')",
            ALLOWED_TYPES.join(", "),
            tmpl(Some(&resource_type))
        ));
    }
    if !truthy(Some(&resource_id)) {
        return Err("brain-lock-acquire: resource_id is required".into());
    }
    if !truthy(Some(&state)) {
        return Err("brain-lock-acquire: state is required".into());
    }
    if !state.as_str().map_or(false, |s| ALLOWED_STATES.contains(&s)) {
        return Err(format!(
            "brain-lock-acquire: state must be one of: {} (got '{}')",
            ALLOWED_STATES.join(", "),
            tmpl(Some(&state))
        ));
    }
    if !truthy(Some(&holder_session)) {
        return Err("brain-lock-acquire: holder_session is required".into());
    }

    let rows = crate::db_json::query_rows(
        db,
        "SELECT holder_session FROM brain.brain_locks WHERE resource_type=? AND resource_id=?",
        &[resource_type.clone(), resource_id.clone()],
    )?;
    if let Some(existing) = rows.as_array().and_then(|a| a.first()) {
        let held = existing.get("holder_session").cloned().unwrap_or(Value::Null);
        return Err(format!(
            "brain-lock-acquire: lock on {}/{} already held by session '{}'",
            tmpl(Some(&resource_type)),
            tmpl(Some(&resource_id)),
            tmpl(Some(&held))
        ));
    }

    crate::db_json::run_write(
        db,
        "INSERT INTO brain.brain_locks (resource_type, resource_id, state, holder_session, expires_at, reason) VALUES (?, ?, ?, ?, ?, ?)",
        &[
            resource_type.clone(),
            resource_id.clone(),
            state,
            holder_session,
            expires_at,
            reason,
        ],
    )?;

    let rows = crate::db_json::query_rows(
        db,
        "SELECT * FROM brain.brain_locks WHERE resource_type=? AND resource_id=?",
        &[resource_type, resource_id],
    )?;
    Ok(rows.as_array().and_then(|a| a.first()).cloned().unwrap_or(Value::Null))
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
    fn count(c: &Connection) -> i64 {
        c.query_row("SELECT COUNT(*) FROM brain.brain_locks", [], |r| r.get(0)).unwrap()
    }

    #[test]
    fn acquires_a_basic_lock_and_returns_the_row() {
        let c = db();
        let r = brainLockAcquire(
            &json!({ "resource_type": "card", "resource_id": "PROJ-1", "state": "driving", "holder_session": "sess-1" }),
            &c,
        )
        .unwrap();
        assert_eq!(
            r,
            json!({ "resource_type": "card", "resource_id": "PROJ-1", "state": "driving", "holder_session": "sess-1", "acquired_at": "2026-01-01 00:00:00", "expires_at": null, "reason": null })
        );
        assert_eq!(count(&c), 1);
    }

    #[test]
    fn stores_expires_at_and_reason() {
        let c = db();
        let r = brainLockAcquire(
            &json!({ "resource_type": "card", "resource_id": "X", "state": "driving", "holder_session": "s", "expires_at": "2099-01-01 00:00:00", "reason": "driving card to done" }),
            &c,
        )
        .unwrap();
        assert_eq!(r["expires_at"], json!("2099-01-01 00:00:00"));
        assert_eq!(r["reason"], json!("driving card to done"));
    }

    #[test]
    fn rejects_double_acquire_on_same_resource() {
        let c = db();
        brainLockAcquire(
            &json!({ "resource_type": "card", "resource_id": "X", "state": "driving", "holder_session": "s1" }),
            &c,
        )
        .unwrap();
        let e = brainLockAcquire(
            &json!({ "resource_type": "card", "resource_id": "X", "state": "driving", "holder_session": "s2" }),
            &c,
        )
        .unwrap_err();
        assert!(e.contains("already held by session 's1'"));
        assert_eq!(count(&c), 1);
    }

    #[test]
    fn same_session_can_lock_different_resources() {
        let c = db();
        brainLockAcquire(
            &json!({ "resource_type": "card", "resource_id": "A", "state": "driving", "holder_session": "s1" }),
            &c,
        )
        .unwrap();
        brainLockAcquire(
            &json!({ "resource_type": "agent", "resource_id": "dev", "state": "thinking", "holder_session": "s1" }),
            &c,
        )
        .unwrap();
        assert_eq!(count(&c), 2);
    }

    #[test]
    fn rejects_unknown_resource_type_and_state() {
        let c = db();
        let e = brainLockAcquire(
            &json!({ "resource_type": "martian", "resource_id": "X", "state": "driving", "holder_session": "s" }),
            &c,
        )
        .unwrap_err();
        assert_eq!(e, "brain-lock-acquire: resource_type must be one of: card, agent (got 'martian')");
        let e = brainLockAcquire(
            &json!({ "resource_type": "card", "resource_id": "X", "state": "pondering", "holder_session": "s" }),
            &c,
        )
        .unwrap_err();
        assert_eq!(e, "brain-lock-acquire: state must be one of: driving, thinking, executing, waiting (got 'pondering')");
    }

    #[test]
    fn rejects_missing_required_fields() {
        let c = db();
        let base = json!({ "resource_type": "card", "resource_id": "X", "state": "driving", "holder_session": "s" });
        for field in ["resource_type", "resource_id", "state", "holder_session"] {
            let mut partial = base.clone();
            partial.as_object_mut().unwrap().remove(field);
            let e = brainLockAcquire(&partial, &c).unwrap_err();
            assert!(e.contains(field), "error `{e}` names `{field}`");
        }
        assert_eq!(count(&c), 0);
    }
}
