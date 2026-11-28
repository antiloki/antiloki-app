//! pl-unanswered.rs — twin of skills/system/focus-detectors/pl-unanswered.mjs (db-last lane).
//! NAMES ARE CONTRACT: the export is `detect` (the JS named default), signature detect(card, db).
//! Unlike blocked-status/stopped-intent this detector actually READS the db: latest PL session
//! for the card in 'clarifying' with unanswered questions (pl_turns first, pl_questions fallback).

use rusqlite::Connection;
use serde_json::{json, Value};

/// JS `try { db.prepare(..).get(..) } catch { .. }` for absent tables maps to query_rows `Err`
/// here: the session query errs → null; a count query errs → that count stays 0.
pub fn detect(card: &Value, db: &Connection) -> Value {
    let slug = card.get("slug").cloned().unwrap_or(Value::Null);
    let rows = match crate::db_json::query_rows(
        db,
        "SELECT id, status FROM pl_sessions WHERE approved_card_slug = ? OR card_slug = ? ORDER BY id DESC LIMIT 1",
        &[slug.clone(), slug],
    ) {
        Ok(r) => r,
        Err(_) => return Value::Null, // pl_sessions table absent
    };
    let Some(session) = rows.as_array().and_then(|a| a.first()).cloned() else { return Value::Null };
    if session.get("status").and_then(|v| v.as_str()) != Some("clarifying") {
        return Value::Null;
    }
    let session_id = session.get("id").cloned().unwrap_or(Value::Null);

    // pl_turns / pl_questions naming varies by migration version — try both (JS comment kept).
    let count = |sql: &str| -> i64 {
        crate::db_json::query_rows(db, sql, &[session_id.clone()])
            .ok()
            .and_then(|r| {
                r.as_array()
                    .and_then(|a| a.first())
                    .and_then(|row| row.get("c"))
                    .and_then(|c| c.as_i64())
            })
            .unwrap_or(0)
    };
    let mut pending =
        count("SELECT COUNT(*) AS c FROM pl_turns WHERE session_id = ? AND answered_at IS NULL AND question_md IS NOT NULL");
    if pending == 0 {
        pending = count(
            "SELECT COUNT(*) AS c FROM pl_questions q LEFT JOIN pl_answers a ON a.question_id = q.id WHERE q.session_id = ? AND a.id IS NULL",
        );
    }
    if pending == 0 {
        return Value::Null;
    }

    json!({
        "kind": "pl_unanswered",
        "detail": { "pl_session_id": session_id, "pending_count": pending },
        "severity": "yellow",
        "reason_md": format!(
            "PL session has {} unanswered question{}. Open Inbox (i) or Chat to answer. The card cannot advance until you respond.",
            pending,
            if pending == 1 { "" } else { "s" }
        ),
        "surfaces_in": ["board:yellow-badge", "pipeline:blocked-pane", "fixit:finding"],
        "fix_options": [
            { "id": "A", "label": "Open Inbox to answer", "action": "nav_inbox" },
            { "id": "B", "label": "Open Chat", "action": "nav_chat" },
        ],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE pl_sessions (id INTEGER PRIMARY KEY, card_slug TEXT, approved_card_slug TEXT, status TEXT);
             CREATE TABLE pl_turns (id INTEGER PRIMARY KEY, session_id INTEGER, question_md TEXT, answered_at TEXT);
             CREATE TABLE pl_questions (id INTEGER PRIMARY KEY, session_id INTEGER, question_md TEXT);
             CREATE TABLE pl_answers (id INTEGER PRIMARY KEY, question_id INTEGER);",
        )
        .unwrap();
        c
    }

    #[test]
    fn null_paths() {
        // tables absent entirely → catch path → null, not an error
        let empty = Connection::open_in_memory().unwrap();
        assert_eq!(detect(&json!({"slug":"CARD-1"}), &empty), Value::Null);
        let c = db();
        // no session at all
        assert_eq!(detect(&json!({"slug":"CARD-NONE"}), &c), Value::Null);
        // latest session not clarifying (ORDER BY id DESC picks the 'tuning' one)
        c.execute_batch("INSERT INTO pl_sessions (id, card_slug, status) VALUES (1,'CARD-A','clarifying'),(2,'CARD-A','tuning');").unwrap();
        assert_eq!(detect(&json!({"slug":"CARD-A"}), &c), Value::Null);
        // clarifying but zero pending in both tables
        c.execute_batch("INSERT INTO pl_sessions (id, card_slug, status) VALUES (9,'CARD-C','clarifying');").unwrap();
        assert_eq!(detect(&json!({"slug":"CARD-C"}), &c), Value::Null);
    }

    #[test]
    fn fires_from_pl_turns_plural() {
        let c = db();
        c.execute_batch(
            "INSERT INTO pl_sessions (id, card_slug, approved_card_slug, status) VALUES (6,'OLD-SLUG','FEAT-X','done'),(7,'OLD-SLUG','FEAT-X','clarifying');
             INSERT INTO pl_turns (id, session_id, question_md, answered_at) VALUES (1,7,'q1',NULL),(2,7,'q2',NULL),(3,7,'q3','2025-12-30 10:00:00'),(4,7,NULL,NULL),(5,6,'other session',NULL);
             INSERT INTO pl_questions (id, session_id, question_md) VALUES (90,7,'stray');",
        )
        .unwrap();
        let f = detect(&json!({"slug":"FEAT-X"}), &c);
        assert_eq!(f["kind"], json!("pl_unanswered"));
        // pl_turns wins: 2 unanswered (answered + null-question_md + other-session rows excluded,
        // and the stray pl_questions row is never consulted)
        assert_eq!(f["detail"], json!({"pl_session_id":7,"pending_count":2}));
        assert_eq!(f["severity"], json!("yellow"));
        assert!(f["reason_md"].as_str().unwrap().starts_with("PL session has 2 unanswered questions. "));
        assert_eq!(f["surfaces_in"], json!(["board:yellow-badge", "pipeline:blocked-pane", "fixit:finding"]));
        assert_eq!(f["fix_options"][0]["action"], json!("nav_inbox"));
        assert_eq!(f["fix_options"][1]["action"], json!("nav_chat"));
    }

    #[test]
    fn falls_back_to_pl_questions_singular() {
        let c = db();
        c.execute_batch(
            "INSERT INTO pl_sessions (id, card_slug, status) VALUES (5,'CARD-B','clarifying');
             INSERT INTO pl_turns (id, session_id, question_md, answered_at) VALUES (11,5,'done q','2025-12-31 09:00:00');
             INSERT INTO pl_questions (id, session_id, question_md) VALUES (10,5,'q-a'),(11,5,'q-b');
             INSERT INTO pl_answers (id, question_id) VALUES (100,10);",
        )
        .unwrap();
        let f = detect(&json!({"slug":"CARD-B"}), &c);
        assert_eq!(f["detail"], json!({"pl_session_id":5,"pending_count":1}));
        assert!(f["reason_md"].as_str().unwrap().starts_with("PL session has 1 unanswered question. "));
    }
}
