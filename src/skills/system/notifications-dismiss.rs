//! notifications-dismiss.rs — NEW skill (Rust-only; the JS lane is being retired). NAMES ARE
//! CONTRACT: `notificationsDismiss` (named default). Mark one notification as dismissed.
//!
//! WHY IT EXISTS. The BE was doing this UPDATE itself, straight against SQLite:
//!
//!     db_write(db => db.prepare(`UPDATE notifications SET dismissed_at = datetime('now')
//!                                WHERE id = ? AND dismissed_at IS NULL`).run(id))
//!
//! That is a mutation with no skill and therefore no with-audit row — the operator dismisses a
//! notification and nothing anywhere records that it happened, or who did it. The substrate's
//! rule is that only a skill touches the databases, precisely so every change lands in the audit
//! chain; a raw UPDATE from the customer layer is the hole that rule closes.
//!
//! IDEMPOTENT BY CONSTRUCTION. The `AND dismissed_at IS NULL` guard means a second dismiss is a
//! no-op rather than a re-stamp, so the ORIGINAL dismissal time survives. `dismissed` reports
//! whether THIS call was the one that changed it, which is what lets a caller tell "I dismissed
//! it" from "it was already gone" without a second query.
// QUALITY: 88/100 — idempotent, reports whether it acted, frozen-clock aware
use rusqlite::Connection;
use serde_json::{json, Value};

pub fn notificationsDismiss(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS `Number(id)`: a non-numeric or absent id is a validation error, not a silent no-op — the
    // caller passed something it believed was an id.
    let id = match input.get("id").and_then(|v| v.as_f64().or_else(|| v.as_str().and_then(|s| s.parse().ok()))) {
        Some(n) if n.is_finite() && n > 0.0 => n as i64,
        _ => return Err("notifications-dismiss: id is required".to_string()),
    };

    let changed = db
        .execute(
            &format!(
                "UPDATE notifications SET dismissed_at = {} WHERE id = ? AND dismissed_at IS NULL",
                crate::clock::now_sql()
            ),
            rusqlite::params![id],
        )
        .map_err(|e| format!("notifications-dismiss: {e}"))?;

    Ok(json!({ "id": id, "dismissed": changed > 0 }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE notifications (
               id INTEGER PRIMARY KEY AUTOINCREMENT, kind TEXT, severity TEXT, source TEXT,
               message TEXT, related_card_slug TEXT, payload_json TEXT, dismissed_at TEXT,
               created_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00'))
             );
             INSERT INTO notifications (id, kind, severity, source, message)
               VALUES (1, 'fe-error', 'warn', 'web-fe', 'boom'),
                      (2, 'drift', 'info', 'system', 'drifted');",
        )
        .unwrap();
        c
    }

    fn dismissed_at(c: &Connection, id: i64) -> Option<String> {
        c.query_row("SELECT dismissed_at FROM notifications WHERE id = ?", [id], |r| r.get(0)).unwrap()
    }

    #[test]
    fn missing_or_bad_id_errors() {
        let c = db();
        assert!(notificationsDismiss(&json!({}), &c).unwrap_err().contains("id is required"));
        assert!(notificationsDismiss(&json!({ "id": 0 }), &c).unwrap_err().contains("id is required"));
        assert!(notificationsDismiss(&json!({ "id": "nope" }), &c).unwrap_err().contains("id is required"));
    }

    #[test]
    fn dismisses_and_stamps_the_row() {
        let c = db();
        let r = notificationsDismiss(&json!({ "id": 1 }), &c).unwrap();
        assert_eq!(r, json!({ "id": 1, "dismissed": true }));
        assert!(dismissed_at(&c, 1).is_some());
        assert!(dismissed_at(&c, 2).is_none(), "only the named notification is touched");
    }

    #[test]
    fn a_numeric_string_id_is_accepted() {
        // The id arrives over HTTP, where a path/body value is often a string.
        let c = db();
        assert_eq!(notificationsDismiss(&json!({ "id": "1" }), &c).unwrap()["dismissed"], json!(true));
    }

    #[test]
    fn dismissing_twice_is_a_NO_OP_that_preserves_the_first_stamp() {
        // The guard is what makes this safe to retry: a second dismiss must not re-stamp, or the
        // audit trail would say the operator dismissed it at a time they did not.
        let c = db();
        assert_eq!(notificationsDismiss(&json!({ "id": 1 }), &c).unwrap()["dismissed"], json!(true));
        let first = dismissed_at(&c, 1);
        let second = notificationsDismiss(&json!({ "id": 1 }), &c).unwrap();
        assert_eq!(second["dismissed"], json!(false), "the second call did not change anything");
        assert_eq!(dismissed_at(&c, 1), first, "the original dismissal time survives");
    }

    #[test]
    fn an_unknown_id_reports_false_rather_than_failing() {
        let c = db();
        assert_eq!(notificationsDismiss(&json!({ "id": 999 }), &c).unwrap(), json!({ "id": 999, "dismissed": false }));
    }
}
