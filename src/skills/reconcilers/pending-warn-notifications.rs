//! pending-warn-notifications.rs — twin of skills/reconcilers/pending-warn-notifications.mjs
//! (reconciler-object lane: the METHODS are the units, both db-last). NAMES ARE CONTRACT:
//! `detect` (detect(db)) and `fix` (fix(finding, db)). Open warn-level notifications surface
//! for operator awareness; safety=manual, so fix never applies.
// QUALITY: 80/100 — warn-notif surface: open-warn filter, newest-first cap 10, absent-table degrade
use rusqlite::Connection;
use serde_json::{json, Value};

// JS `${v}` for the [kind] interpolation (string/number/null render like JS).
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
// String(v ?? '') — the JS (r.message ?? '') coercion.
fn str_qq(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(),
        Some(o) => o.to_string(),
    }
}

/// JS: `async detect(db)` — the try/catch around the query maps a missing notifications
/// table to rows = [] (so the map yields []), exactly like the JS `catch { /* table absent */ }`.
pub fn detect(db: &Connection) -> Value {
    let rows = match crate::db_json::query_rows(
        db,
        "\n        SELECT id, ts, kind, source, message, related_card_slug\n        FROM notifications\n        WHERE severity = 'warn' AND dismissed_at IS NULL\n        ORDER BY id DESC LIMIT 10\n      ",
        &[],
    ) {
        Ok(v) => v.as_array().cloned().unwrap_or_default(),
        Err(_) => Vec::new(), // table absent
    };
    let findings: Vec<Value> = rows
        .iter()
        .map(|r| {
            let msg: String = str_qq(r.get("message")).chars().take(80).collect();
            json!({
                "kind": "pending-warn-notification",
                "severity": "info",
                "message": format!("[{}] {}", tmpl(r.get("kind")), msg),
                "card_slug": r.get("related_card_slug").cloned().unwrap_or(Value::Null),
                "detail": {
                    "notification_id": r.get("id").cloned().unwrap_or(Value::Null),
                    "ts": r.get("ts").cloned().unwrap_or(Value::Null),
                    "kind": r.get("kind").cloned().unwrap_or(Value::Null),
                    "source": r.get("source").cloned().unwrap_or(Value::Null),
                },
                "suggested_action": "open Notifications (n), read, dismiss (d) if resolved",
            })
        })
        .collect();
    json!(findings)
}

/// JS: `async fix()` — manual-only; the JS method takes no params, so both the finding the
/// harness passes (db-last, args:[finding]) and the db are ignored.
pub fn fix(_finding: &Value, _db: &Connection) -> Value {
    json!({ "applied": false, "reason": "manual — operator decides per-notification" })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE notifications (id INTEGER PRIMARY KEY, ts TEXT, kind TEXT, source TEXT, message TEXT, severity TEXT, dismissed_at TEXT, related_card_slug TEXT)").unwrap();
        c
    }

    #[test]
    fn absent_table_and_empty_table_return_empty() {
        let empty = Connection::open_in_memory().unwrap();
        assert_eq!(detect(&empty), json!([]));
        assert_eq!(detect(&db()), json!([]));
    }

    #[test]
    fn only_open_warn_rows_surface_with_full_shape() {
        let c = db();
        c.execute_batch("INSERT INTO notifications (id, ts, kind, source, message, severity, dismissed_at, related_card_slug) VALUES \
            (1, '2026-01-01 00:00:00', 'qa', 'driver', 'QA flaked once', 'warn', NULL, 'CARD-1'), \
            (2, '2026-01-01 01:00:00', 'qa', 'driver', 'red alarm', 'error', NULL, NULL), \
            (3, '2026-01-01 02:00:00', 'qa', 'driver', 'already handled', 'warn', '2026-01-02 00:00:00', NULL);").unwrap();
        let f = detect(&c);
        assert_eq!(f, json!([{
            "kind": "pending-warn-notification",
            "severity": "info",
            "message": "[qa] QA flaked once",
            "card_slug": "CARD-1",
            "detail": { "notification_id": 1, "ts": "2026-01-01 00:00:00", "kind": "qa", "source": "driver" },
            "suggested_action": "open Notifications (n), read, dismiss (d) if resolved",
        }]));
    }

    #[test]
    fn null_message_kind_slug_and_80_char_slice() {
        let c = db();
        let long = "y".repeat(100);
        c.execute(
            "INSERT INTO notifications (id, ts, kind, source, message, severity, dismissed_at, related_card_slug) VALUES \
             (4, '2026-01-01 03:00:00', NULL, NULL, NULL, 'warn', NULL, NULL), \
             (5, '2026-01-01 04:00:00', 'mem', 'watchdog', ?1, 'warn', NULL, 'CARD-9')",
            rusqlite::params![long],
        ).unwrap();
        let f = detect(&c);
        // newest first: id 5 then id 4
        assert_eq!(f[0]["message"], json!(format!("[mem] {}", "y".repeat(80))));
        assert_eq!(f[1]["message"], json!("[null] ")); // `${null}` + (null ?? '') like JS
        assert_eq!(f[1]["card_slug"], Value::Null);
        assert_eq!(f[1]["detail"], json!({ "notification_id": 4, "ts": "2026-01-01 03:00:00", "kind": Value::Null, "source": Value::Null }));
    }

    #[test]
    fn newest_first_capped_at_10() {
        let c = db();
        for i in 1..=12 {
            c.execute(
                "INSERT INTO notifications (id, ts, kind, source, message, severity, dismissed_at, related_card_slug) VALUES (?1, '2026-01-01 00:00:00', 'qa', 'driver', ?2, 'warn', NULL, NULL)",
                rusqlite::params![i, format!("warn {i}")],
            ).unwrap();
        }
        let f = detect(&c);
        let ids: Vec<Value> = f.as_array().unwrap().iter().map(|x| x["detail"]["notification_id"].clone()).collect();
        assert_eq!(json!(ids), json!([12, 11, 10, 9, 8, 7, 6, 5, 4, 3]));
    }

    #[test]
    fn fix_never_applies() {
        let c = Connection::open_in_memory().unwrap();
        assert_eq!(
            fix(&json!({ "kind": "pending-warn-notification" }), &c),
            json!({ "applied": false, "reason": "manual — operator decides per-notification" })
        );
        assert_eq!(fix(&Value::Null, &c), json!({ "applied": false, "reason": "manual — operator decides per-notification" }));
    }
}
