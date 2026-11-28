//! session-create.rs — twin of skills/agents/in/session-create.mjs (db WRITE lane). NAMES ARE
//! CONTRACT: `sessionCreate` (named default). Eager INSERT of an empty active session.
// QUALITY: 90/100 — single INSERT + rowid; String()-coerce + 200-char clamp on user_identifier
use rusqlite::Connection;
use serde_json::{json, Value};

pub fn sessionCreate(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS: input?.user_identifier != null ? String(x).slice(0,200) : null
    let user_identifier: Value = match input.get("user_identifier") {
        Some(Value::String(s)) => Value::String(s.chars().take(200).collect()),
        Some(v) if !v.is_null() => Value::String(v.to_string().chars().take(200).collect()),
        _ => Value::Null,
    };
    let (_c, id) = crate::db_json::run_write(
        db,
        "INSERT INTO interviewer_sessions (user_identifier, status) VALUES (?, 'active')",
        &[user_identifier],
    )?;
    Ok(json!({ "ok": true, "session_id": id }))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn inserts_active_session() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE interviewer_sessions (id INTEGER PRIMARY KEY, user_identifier TEXT, status TEXT)").unwrap();
        let r = sessionCreate(&json!({ "user_identifier": "u1" }), &c).unwrap();
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["session_id"], json!(1));
        let r2 = sessionCreate(&json!({}), &c).unwrap();
        assert_eq!(r2["session_id"], json!(2)); // null user_identifier ok
    }
}
