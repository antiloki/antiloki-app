//! inbox-message-update.rs — twin of skills/entities/inbox-message-update.mjs (db WRITE lane).
//! NAMES ARE CONTRACT: `inboxMessageUpdate` (default). Operator action on one inbox message:
//! mark_read / mark_unread / archive / unarchive; returns { id, action, applied }.
// QUALITY: enum guard, existence check, per-action frozen-clock UPDATE, applied = changes>0
use rusqlite::Connection;
use serde_json::{json, Value};

fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}
// = JS `${v}` — used only in the (un-fixtured) guard error messages.
fn tmpl(v: &Value) -> String {
    match v {
        Value::Null => "null".into(),
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        o => o.to_string(),
    }
}

pub fn inboxMessageUpdate(input: &Value, db: &Connection) -> Result<Value, String> {
    let id = input.get("id").cloned().unwrap_or(Value::Null);
    if !truthy(Some(&id)) { return Err("inbox-message-update: id is required".into()); }
    let action = input.get("action").cloned().unwrap_or(Value::Null);
    if !truthy(Some(&action)) { return Err("inbox-message-update: action is required".into()); }

    // ALLOWED = Set('mark_read','mark_unread','archive','unarchive'); non-member (incl. non-string) → enum throw.
    let action_str = action.as_str().unwrap_or("");
    if !matches!(action_str, "mark_read" | "mark_unread" | "archive" | "unarchive") {
        return Err(format!(
            "inbox-message-update: action must be one of mark_read/mark_unread/archive/unarchive (got '{}')",
            tmpl(&action)));
    }

    // Existence check — NOT_FOUND throw (guard path, not fixtured).
    let row = crate::db_json::query_rows(db, "SELECT id FROM inbox_messages WHERE id = ?", &[id.clone()])?;
    if row.as_array().map_or(true, |a| a.is_empty()) {
        return Err(format!("inbox-message-update: inbox_message id={} not found", tmpl(&id)));
    }

    // Frozen-clock lane: JS `datetime('now')` → the frozen literal; both lanes run identical SQL.
    let now = crate::clock::now_sql();
    let sql = match action_str {
        "mark_read"   => format!("UPDATE inbox_messages SET read_at = {now} WHERE id = ?"),
        "mark_unread" => "UPDATE inbox_messages SET read_at = NULL WHERE id = ?".to_string(),
        "archive"     => format!("UPDATE inbox_messages SET archived_at = {now}, read_at = COALESCE(read_at, {now}) WHERE id = ?"),
        "unarchive"   => "UPDATE inbox_messages SET archived_at = NULL WHERE id = ?".to_string(),
        _ => unreachable!(),
    };
    let (changes, _) = crate::db_json::run_write(db, &sql, &[id.clone()])?;
    Ok(json!({ "id": id, "action": action, "applied": changes > 0 }))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE inbox_messages (id INTEGER PRIMARY KEY AUTOINCREMENT, read_at TEXT, archived_at TEXT); \
            INSERT INTO inbox_messages (id, read_at, archived_at) VALUES (1, NULL, NULL);").unwrap();
        c
    }
    #[test]
    fn mark_read_and_unread() {
        let c = db();
        let r = inboxMessageUpdate(&json!({ "id": 1, "action": "mark_read" }), &c).unwrap();
        assert_eq!(r, json!({ "id": 1, "action": "mark_read", "applied": true }));
        let ra: Option<String> = c.query_row("SELECT read_at FROM inbox_messages WHERE id=1", [], |r| r.get(0)).unwrap();
        assert_eq!(ra.as_deref(), Some("2026-01-01 00:00:00"));
        let r = inboxMessageUpdate(&json!({ "id": 1, "action": "mark_unread" }), &c).unwrap();
        assert_eq!(r["applied"], json!(true));
        let ra: Option<String> = c.query_row("SELECT read_at FROM inbox_messages WHERE id=1", [], |r| r.get(0)).unwrap();
        assert_eq!(ra, None);
    }
    #[test]
    fn archive_coalesces_read_at() {
        let c = db();
        // read_at NULL → archive sets both to frozen instant
        let r = inboxMessageUpdate(&json!({ "id": 1, "action": "archive" }), &c).unwrap();
        assert_eq!(r, json!({ "id": 1, "action": "archive", "applied": true }));
        let (ra, aa): (Option<String>, Option<String>) = c.query_row("SELECT read_at, archived_at FROM inbox_messages WHERE id=1", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert_eq!(ra.as_deref(), Some("2026-01-01 00:00:00"));
        assert_eq!(aa.as_deref(), Some("2026-01-01 00:00:00"));
        // unarchive clears archived_at, preserves read_at
        inboxMessageUpdate(&json!({ "id": 1, "action": "unarchive" }), &c).unwrap();
        let (ra, aa): (Option<String>, Option<String>) = c.query_row("SELECT read_at, archived_at FROM inbox_messages WHERE id=1", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert_eq!(ra.as_deref(), Some("2026-01-01 00:00:00"));
        assert_eq!(aa, None);
    }
    #[test]
    fn archive_preserves_existing_read_at() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE inbox_messages (id INTEGER PRIMARY KEY, read_at TEXT, archived_at TEXT); \
            INSERT INTO inbox_messages (id, read_at, archived_at) VALUES (7, '2025-06-01 00:00:00', NULL);").unwrap();
        inboxMessageUpdate(&json!({ "id": 7, "action": "archive" }), &c).unwrap();
        let ra: String = c.query_row("SELECT read_at FROM inbox_messages WHERE id=7", [], |r| r.get(0)).unwrap();
        assert_eq!(ra, "2025-06-01 00:00:00"); // COALESCE keeps the existing timestamp
    }
    #[test]
    fn guards() {
        let c = db();
        assert!(inboxMessageUpdate(&json!({ "action": "archive" }), &c).unwrap_err().contains("id is required"));
        assert!(inboxMessageUpdate(&json!({ "id": 1 }), &c).unwrap_err().contains("action is required"));
        assert!(inboxMessageUpdate(&json!({ "id": 1, "action": "banana" }), &c).unwrap_err().contains("action must be one of"));
        assert!(inboxMessageUpdate(&json!({ "id": 999, "action": "archive" }), &c).unwrap_err().contains("not found"));
    }
}
