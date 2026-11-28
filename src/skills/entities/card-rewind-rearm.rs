//! card-rewind-rearm.rs — twin of skills/entities/card-rewind-rearm.mjs (db WRITE lane).
//! NAMES ARE CONTRACT: `cardRewindRearm` (default). The card-side re-arm half of a process
//! rewind: sets driver_intent='play', bounces blocked/archived status back to backlog, clears
//! the card's OpenRouter session (via the already-converted `ai_run_session::clearCardSession`),
//! and dismisses this card's stale severity='error' notifications so the daemon resumes.
//! Unlike god-card-driver-intent-set this does NOT refuse a terminal ('done') card.
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

pub fn cardRewindRearm(input: &Value, db: &Connection) -> Result<Value, String> {
    let slug = input.get("slug").cloned().unwrap_or(Value::Null);
    if !truthy(Some(&slug)) {
        return Err("card-rewind-rearm: slug is required".into());
    }

    crate::db_json::run_write(
        db,
        &format!("UPDATE cards \
         SET driver_intent = 'play', \
             status        = CASE WHEN status IN ('blocked', 'archived') THEN 'backlog' ELSE status END, \
             updated_at    = {} \
         WHERE slug = ?", crate::clock::now_sql()),
        &[slug.clone()],
    )?;

    // FEAT-OPENROUTER-SESSION-TRACKING: a rewind begins a NEW run — clear the session so the
    // next driver tick mints a fresh OpenRouter one. Guarded internally by the twin (degrades to
    // a no-op on persisted DBs that have not yet run migration 137).
    crate::ai_run_session::clearCardSession(db, &slug);

    // notifications is not an audited entity — direct UPDATE is the canonical dismissal.
    // Guarded: notifications table absent on legacy DBs degrades to 0 (matches the JS try/catch).
    let cleared_notifications = match crate::db_json::run_write(
        db,
        &format!("UPDATE notifications SET dismissed_at = {} \
         WHERE severity = 'error' AND dismissed_at IS NULL AND related_card_slug = ?", crate::clock::now_sql()),
        &[slug.clone()],
    ) {
        Ok((changes, _)) => changes as i64,
        Err(_) => 0,
    };

    Ok(json!({ "slug": slug, "cleared_notifications": cleared_notifications }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> Connection {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(
            "CREATE TABLE cards (slug TEXT PRIMARY KEY, driver_intent TEXT, status TEXT, updated_at TEXT, openrouter_session_id TEXT); \
             CREATE TABLE notifications (id INTEGER PRIMARY KEY AUTOINCREMENT, severity TEXT, dismissed_at TEXT, related_card_slug TEXT);",
        ).unwrap();
        db
    }

    #[test]
    fn bounces_blocked_status_to_backlog_and_clears_session() {
        let db = setup();
        db.execute(
            "INSERT INTO cards (slug, driver_intent, status, updated_at, openrouter_session_id) VALUES ('CARD-A', 'pause', 'blocked', '2025-01-01 00:00:00', 'card-CARD-A-old')",
            [],
        ).unwrap();
        let out = cardRewindRearm(&json!({ "slug": "CARD-A" }), &db).unwrap();
        assert_eq!(out, json!({ "slug": "CARD-A", "cleared_notifications": 0 }));
        let (intent, status, updated_at, session): (String, String, String, Option<String>) = db
            .query_row(
                "SELECT driver_intent, status, updated_at, openrouter_session_id FROM cards WHERE slug='CARD-A'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        assert_eq!(intent, "play");
        assert_eq!(status, "backlog");
        assert_eq!(updated_at, "2026-01-01 00:00:00");
        assert_eq!(session, None);
    }

    #[test]
    fn leaves_non_blocked_archived_status_untouched_and_dismisses_error_notifications() {
        let db = setup();
        db.execute(
            "INSERT INTO cards (slug, driver_intent, status) VALUES ('CARD-B', 'pause', 'done')",
            [],
        ).unwrap();
        db.execute(
            "INSERT INTO notifications (severity, dismissed_at, related_card_slug) VALUES ('error', NULL, 'CARD-B')",
            [],
        ).unwrap();
        db.execute(
            "INSERT INTO notifications (severity, dismissed_at, related_card_slug) VALUES ('warning', NULL, 'CARD-B')",
            [],
        ).unwrap();
        let out = cardRewindRearm(&json!({ "slug": "CARD-B" }), &db).unwrap();
        assert_eq!(out, json!({ "slug": "CARD-B", "cleared_notifications": 1 }));
        let status: String = db.query_row("SELECT status FROM cards WHERE slug='CARD-B'", [], |r| r.get(0)).unwrap();
        assert_eq!(status, "done"); // terminal card re-armed, status stays (not blocked/archived)
    }

    #[test]
    fn missing_notifications_table_degrades_to_zero() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE cards (slug TEXT PRIMARY KEY, driver_intent TEXT, status TEXT, updated_at TEXT)").unwrap();
        db.execute("INSERT INTO cards (slug, driver_intent, status) VALUES ('CARD-C', 'pause', 'archived')", []).unwrap();
        let out = cardRewindRearm(&json!({ "slug": "CARD-C" }), &db).unwrap();
        assert_eq!(out, json!({ "slug": "CARD-C", "cleared_notifications": 0 }));
        let status: String = db.query_row("SELECT status FROM cards WHERE slug='CARD-C'", [], |r| r.get(0)).unwrap();
        assert_eq!(status, "backlog");
    }

    #[test]
    fn slug_required() {
        let db = setup();
        assert!(cardRewindRearm(&json!({}), &db).unwrap_err().contains("slug is required"));
    }
}
