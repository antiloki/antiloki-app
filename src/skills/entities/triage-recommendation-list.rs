//! triage-recommendation-list.rs — twin of skills/entities/triage-recommendation-list.mjs (db READ
//! lane). NAMES ARE CONTRACT: `triageRecommendationList` (default). Reads active (non-expired) marks:
//! note-state rows whose expires_at is NULL or still in the future, plus latest schedule per rec_id.
// QUALITY: 74/100 — DISTINCT active notes (frozen-clock expiry filter) + MAX(marked_at) per schedule
use rusqlite::Connection;
use serde_json::{json, Value};

pub fn triageRecommendationList(_input: &Value, db: &Connection) -> Result<Value, String> {
    // JS: expires_at > datetime('now') — conform freezes 'now' to 2026-01-01 00:00:00; write it literally.
    let note_rows = crate::db_json::query_rows(
        db,
        &format!("SELECT DISTINCT rec_id FROM triage_recommendation_states \
         WHERE state = 'note' AND (expires_at IS NULL OR expires_at > {})", crate::clock::now_sql()),
        &[],
    )?;
    // JS: .map((r) => r.rec_id)
    let active_notes: Vec<Value> = note_rows
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r.get("rec_id").cloned().unwrap_or(Value::Null))
        .collect();

    let schedules = crate::db_json::query_rows(
        db,
        "SELECT rec_id, MAX(marked_at) AS marked_at \
         FROM triage_recommendation_states \
         WHERE state = 'schedule' \
         GROUP BY rec_id",
        &[],
    )?;

    Ok(json!({ "active_notes": active_notes, "schedules": schedules }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seeded() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE triage_recommendation_states (rec_id TEXT, state TEXT, marked_at TEXT, expires_at TEXT); \
             INSERT INTO triage_recommendation_states (rec_id,state,marked_at,expires_at) VALUES \
               ('r1','note','2025-12-01 00:00:00',NULL), \
               ('r1','note','2025-12-02 00:00:00','2026-06-01 00:00:00'), \
               ('r2','note','2025-11-01 00:00:00','2026-03-01 00:00:00'), \
               ('r3','note','2025-10-01 00:00:00','2025-12-01 00:00:00'), \
               ('s1','schedule','2025-09-01 00:00:00',NULL), \
               ('s1','schedule','2025-09-15 00:00:00',NULL), \
               ('s2','schedule','2025-08-01 00:00:00',NULL);",
        )
        .unwrap();
        c
    }

    #[test]
    fn reads_active_notes_and_schedules() {
        let r = triageRecommendationList(&json!({}), &seeded()).unwrap();
        // r1 (null expiry, deduped) + r2 (future expiry); r3 expired → excluded.
        assert_eq!(r["active_notes"], json!(["r1", "r2"]));
        // MAX(marked_at) per schedule rec_id, key order rec_id then marked_at.
        assert_eq!(
            r["schedules"],
            json!([
                { "rec_id": "s1", "marked_at": "2025-09-15 00:00:00" },
                { "rec_id": "s2", "marked_at": "2025-08-01 00:00:00" }
            ])
        );
    }

    #[test]
    fn empty_when_nothing_active() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE triage_recommendation_states (rec_id TEXT, state TEXT, marked_at TEXT, expires_at TEXT); \
             INSERT INTO triage_recommendation_states (rec_id,state,marked_at,expires_at) VALUES \
               ('x','note','2025-01-01 00:00:00','2025-06-01 00:00:00');",
        )
        .unwrap();
        let r = triageRecommendationList(&json!({}), &c).unwrap();
        assert_eq!(r["active_notes"], json!([]));
        assert_eq!(r["schedules"], json!([]));
    }
}
