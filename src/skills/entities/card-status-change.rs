//! card-status-change.rs — twin of skills/entities/card-status-change.mjs (db WRITE lane). NAMES
//! ARE CONTRACT: `cardStatusChange` (default). Enum guard + UPDATE status/updated_at, return row.
// QUALITY: 85/100 — status enum + not-found + no-op guards; datetime('now') frozen to 2026-01-01
use rusqlite::{params, Connection};
use serde_json::{json, Value};

const ALLOWED_STATUSES: [&str; 5] = ["backlog", "doing", "blocked", "done", "archived"];

pub fn cardStatusChange(input: &Value, db: &Connection) -> Result<Value, String> {
    let slug = match input.get("slug").and_then(|v| v.as_str()).filter(|s| !s.is_empty()) {
        Some(s) => s,
        None => return Err("card-status-change: slug is required".into()),
    };
    let status = match input.get("status").and_then(|v| v.as_str()).filter(|s| !s.is_empty()) {
        Some(s) => s,
        None => return Err("card-status-change: status is required".into()),
    };
    if !ALLOWED_STATUSES.contains(&status) {
        return Err(format!("card-status-change: status must be one of: {} (got '{status}')", ALLOWED_STATUSES.join(", ")));
    }

    let existing = crate::db_json::query_rows(db, "SELECT * FROM cards WHERE slug = ?", &[json!(slug)])?;
    let existing = existing.as_array().unwrap();
    let existing = match existing.first() {
        Some(r) => r,
        None => return Err(format!("card-status-change: card '{slug}' not found")),
    };
    if existing.get("status").and_then(|v| v.as_str()) == Some(status) {
        return Err(format!("card '{slug}' is already in status '{status}' (no-op)"));
    }

    // JS: UPDATE ... updated_at = datetime('now') — frozen-clock lane pins 'now' to the instant below.
    db.execute(
        &format!("UPDATE cards SET status = ?1, updated_at = {} WHERE slug = ?2", crate::clock::now_sql()),
        params![status, slug],
    ).map_err(|e| e.to_string())?;

    let rows = crate::db_json::query_rows(db, "SELECT * FROM cards WHERE slug = ?", &[json!(slug)])?;
    Ok(rows.as_array().unwrap().first().cloned().unwrap_or(Value::Null))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn seeded() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE cards (slug TEXT PRIMARY KEY, title TEXT, status TEXT, updated_at TEXT); \
            INSERT INTO cards VALUES ('S-1','first','backlog','2025-12-01 00:00:00');").unwrap();
        c
    }
    #[test]
    fn moves_status_and_stamps() {
        let c = seeded();
        let r = cardStatusChange(&json!({ "slug": "S-1", "status": "doing" }), &c).unwrap();
        assert_eq!(r["slug"], json!("S-1"));
        assert_eq!(r["status"], json!("doing"));
        assert_eq!(r["updated_at"], json!("2026-01-01 00:00:00"));
        assert_eq!(r["title"], json!("first")); // untouched column rides through SELECT *
    }
    #[test]
    fn guards() {
        let c = seeded();
        assert!(cardStatusChange(&json!({ "slug": "S-1", "status": "doing" }), &c).is_ok());
        assert!(cardStatusChange(&json!({ "slug": "S-1", "status": "doing" }), &c).is_err()); // no-op
        assert!(cardStatusChange(&json!({ "slug": "GHOST", "status": "doing" }), &c).is_err()); // not found
        assert!(cardStatusChange(&json!({ "slug": "S-1", "status": "wibbly" }), &c).is_err()); // bad enum
        assert!(cardStatusChange(&json!({ "status": "doing" }), &c).is_err()); // missing slug
        assert!(cardStatusChange(&json!({ "slug": "S-1" }), &c).is_err()); // missing status
    }
}
