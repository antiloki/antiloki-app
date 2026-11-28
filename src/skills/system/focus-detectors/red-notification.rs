//! red-notification.rs — twin of skills/system/focus-detectors/red-notification.mjs (db-last lane).
//! NAMES ARE CONTRACT: the export is `detect` (the JS default), signature detect(card, db).
//! Open severity='error' notifications mentioning the card block the driver's dispatch loop.

use rusqlite::Connection;
use serde_json::{json, Map, Value};

// JS `${v}` for the notification-id interpolation (integer/string/null render like JS).
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

/// JS: `detect(card, db)` — the try/catch around the query maps ANY prepare/bind error
/// (notifications table absent) to null, exactly like the JS `catch { return null; }`.
pub fn detect(card: &Value, db: &Connection) -> Value {
    let slug = card.get("slug").cloned().unwrap_or(Value::Null);
    let notifs = match crate::db_json::query_rows(
        db,
        "\n      SELECT id, message_md, severity, created_at\n      FROM notifications\n      WHERE severity = 'error'\n        AND dismissed_at IS NULL\n        AND (\n          json_extract(payload_json, '$.card_slug') = ?\n          OR message_md LIKE '%' || ? || '%'\n        )\n      ORDER BY id DESC LIMIT 5\n    ",
        &[slug.clone(), slug],
    ) {
        Ok(v) => v.as_array().cloned().unwrap_or_default(),
        Err(_) => return Value::Null, // notifications table absent
    };
    if notifs.is_empty() {
        return Value::Null;
    }

    let n = notifs.len();
    let first_id = notifs[0].get("id").cloned().unwrap_or(Value::Null);

    let mut detail = Map::new();
    detail.insert("count".into(), json!(n));
    // notifs[0].message_md?.slice(0, 200) — a NULL message_md is `undefined` after `?.`,
    // and JSON.stringify DROPS the key, so we omit it entirely (never insert null).
    if let Some(Value::String(s)) = notifs[0].get("message_md") {
        detail.insert("first_message".into(), json!(s.chars().take(200).collect::<String>()));
    }
    detail.insert(
        "notification_ids".into(),
        json!(notifs.iter().map(|nf| nf.get("id").cloned().unwrap_or(Value::Null)).collect::<Vec<_>>()),
    );

    let reason_md = format!(
        "{} unresolved red notification{} mentioning this card. Red notifs block the driver's dispatch loop \u{2014} the substrate refuses to fire any action while they exist. Open Fixit to reconcile the root cause (dismissing without fixing is forbidden).",
        n,
        if n == 1 { "" } else { "s" }
    );

    json!({
        "kind": "red_notification",
        "detail": detail,
        "reason_md": reason_md,
        "surfaces_in": ["board:red-badge", "pipeline:blocked-pane", "fixit:finding"],
        "fix_options": [
            { "id": "A", "label": "Open Fixit to reconcile",
              "action": "nav_fixit" },
            { "id": "B", "label": format!("Inspect notification {}", tmpl(Some(&first_id))),
              "action": "nav_notification", "notification_id": first_id },
        ],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE notifications (id INTEGER PRIMARY KEY, message_md TEXT, severity TEXT, created_at TEXT, dismissed_at TEXT, payload_json TEXT)").unwrap();
        c
    }

    #[test]
    fn absent_table_and_non_matching_rows_return_null() {
        let empty = Connection::open_in_memory().unwrap();
        assert_eq!(detect(&json!({ "slug": "C1" }), &empty), Value::Null);
        let c = db();
        c.execute_batch("INSERT INTO notifications (id, message_md, severity) VALUES (1, 'warn about CARD-2', 'warn'); \
            INSERT INTO notifications (id, message_md, severity, dismissed_at) VALUES (2, 'error on CARD-2', 'error', '2026-01-01 01:00:00'); \
            INSERT INTO notifications (id, message_md, severity, payload_json) VALUES (3, 'error elsewhere', 'error', '{\"card_slug\":\"OTHER\"}');").unwrap();
        assert_eq!(detect(&json!({ "slug": "CARD-2" }), &c), Value::Null);
    }

    #[test]
    fn fires_on_payload_slug_match_with_full_shape() {
        let c = db();
        c.execute_batch("INSERT INTO notifications (id, message_md, severity, payload_json) VALUES (5, 'Driver refused to fire', 'error', '{\"card_slug\":\"FEAT-X\"}');").unwrap();
        let f = detect(&json!({ "slug": "FEAT-X" }), &c);
        assert_eq!(f["kind"], json!("red_notification"));
        assert_eq!(f["detail"], json!({ "count": 1, "first_message": "Driver refused to fire", "notification_ids": [5] }));
        assert!(f["reason_md"].as_str().unwrap().starts_with("1 unresolved red notification mentioning"));
        assert_eq!(f["surfaces_in"], json!(["board:red-badge", "pipeline:blocked-pane", "fixit:finding"]));
        assert_eq!(f["fix_options"][0]["action"], json!("nav_fixit"));
        assert_eq!(f["fix_options"][1]["label"], json!("Inspect notification 5"));
        assert_eq!(f["fix_options"][1]["notification_id"], json!(5));
    }

    #[test]
    fn newest_first_capped_at_5_plural_and_like_match() {
        let c = db();
        for i in 10..=15 {
            c.execute(
                "INSERT INTO notifications (id, message_md, severity) VALUES (?1, ?2, 'error')",
                rusqlite::params![i, format!("error {i} touching CARD-M")],
            ).unwrap();
        }
        let f = detect(&json!({ "slug": "CARD-M" }), &c);
        assert_eq!(f["detail"]["notification_ids"], json!([15, 14, 13, 12, 11]));
        assert_eq!(f["detail"]["count"], json!(5));
        assert_eq!(f["detail"]["first_message"], json!("error 15 touching CARD-M"));
        assert!(f["reason_md"].as_str().unwrap().starts_with("5 unresolved red notifications mentioning"));
    }

    #[test]
    fn null_message_md_omits_first_message_and_long_slices_to_200() {
        let c = db();
        c.execute_batch("INSERT INTO notifications (id, severity, payload_json) VALUES (20, 'error', '{\"card_slug\":\"NM\"}');").unwrap();
        let f = detect(&json!({ "slug": "NM" }), &c);
        assert!(f["detail"].get("first_message").is_none(), "stringify-drop semantics");
        let c2 = db();
        let long = format!("LONG-1 {}", "x".repeat(300));
        c2.execute("INSERT INTO notifications (id, message_md, severity) VALUES (30, ?1, 'error')", rusqlite::params![long]).unwrap();
        let f2 = detect(&json!({ "slug": "LONG-1" }), &c2);
        assert_eq!(f2["detail"]["first_message"].as_str().unwrap().chars().count(), 200);
    }
}
