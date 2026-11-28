//! playing-card-no-driver-progress.rs — twin of skills/reconcilers/playing-card-no-driver-progress.mjs
//! (reconciler-object lane: the METHODS are the units, both db-last). NAMES ARE CONTRACT:
//! `detect` (detect(db)) and `fix` (fix(finding, db)). Cards showing status='doing' AND
//! driver_intent='play' (board says "playing") but with no non-idle brain.driver_ticks
//! progress in the last 60s (pipeline says idle) surface as a desync finding; safety=manual,
//! so fix never applies — the operator picks A/B/C from the Fixit fix_options.
//!
//! The correlated subquery filters brain.driver_ticks on a column named `ts` — transcribed
//! verbatim from the JS SQL (READ, never edit logic). production brain.driver_ticks only has
//! `started_at` (see migrations/20260520-001-driver-ticks.mjs), so in real deployments
//! db.prepare() throws "no such column: ts" and the try/catch always yields [] — this reconciler
//! is dormant in production today. The fixture creates brain.driver_ticks WITH a `ts` column so
//! both lanes can still exercise the found/not-found filter logic identically.
//!
//! datetime-injection lane: the JS `datetime('now', ?)` (bound to `-60 seconds`, the
//! STALE_WINDOW_SECONDS constant) is written frozen as the literal
//! `datetime('2026-01-01 00:00:00', '-60 seconds')`.

use rusqlite::Connection;
use serde_json::{json, Value};

fn str_qq(v: Option<&Value>) -> String { match v { None|Some(Value::Null)=>String::new(), Some(Value::String(s))=>s.clone(), Some(Value::Bool(b))=>b.to_string(), Some(n@Value::Number(_))=>n.to_string(), Some(o)=>o.to_string() } } // = JS String(v ?? '')
fn tmpl(v: Option<&Value>) -> String { match v { None=>"undefined".into(), Some(Value::Null)=>"null".into(), Some(x)=>str_qq(Some(x)) } } // = JS `${v}`

/// JS: `async detect(db)` — cards in status='doing' AND driver_intent='play' where the
/// correlated subquery's MAX(id) (non-idle driver_ticks in the last 60s) is null. The try/catch
/// around the query maps ANY prepare error (cards table absent, or brain.driver_ticks missing
/// the `ts` column as in production) to rows = [], exactly like the JS `catch { /* ... */ }`.
pub fn detect(db: &Connection) -> Value {
    let rows = match crate::db_json::query_rows(
        db,
        &format!("\n        SELECT c.slug, c.status, c.driver_intent, c.updated_at,\n               (SELECT MAX(id) FROM brain.driver_ticks\n                  WHERE card_slug = c.slug AND outcome != 'idle'\n                    AND ts >= datetime({}, '-60 seconds')) AS recent_progress_id\n        FROM cards c\n        WHERE c.status = 'doing' AND c.driver_intent = 'play'\n      ", crate::clock::now_sql_inner()),
        &[],
    ) {
        Ok(v) => v.as_array().cloned().unwrap_or_default(),
        Err(_) => Vec::new(), // cards absent / driver_ticks.ts absent (fresh DB / bootstrap / production reality)
    };

    let findings: Vec<Value> = rows
        .iter()
        .filter(|r| r.get("recent_progress_id").map_or(true, Value::is_null))
        .map(|r| {
            let slug = r.get("slug").cloned().unwrap_or(Value::Null);
            json!({
                "kind": "playing-card-no-driver-progress",
                "severity": "warn",
                "message": format!(
                    "{}: board says playing (status=doing, driver_intent=play) but pipeline shows no progress in the last 60s. Board and pipeline are desynced.",
                    tmpl(r.get("slug"))
                ),
                "card_slug": slug.clone(),
                "detail": {
                    "status": r.get("status").cloned().unwrap_or(Value::Null),
                    "driver_intent": r.get("driver_intent").cloned().unwrap_or(Value::Null),
                    "updated_at": r.get("updated_at").cloned().unwrap_or(Value::Null),
                    "stale_window_seconds": 60,
                },
                "suggested_action": "Investigate why the driver is not picking up this card (check Fixit for red notifs, inflight AI calls, or stuck contracts). Alternatively pause the card via Card view → s.",
                "fix_options": [
                    { "id": "A", "label": "Pause card (driver_intent=pause)", "action": "pause_card", "card_slug": slug.clone() },
                    { "id": "B", "label": "Stop card (driver_intent=stop)", "action": "stop_card", "card_slug": slug.clone() },
                    { "id": "C", "label": "Force a tick now (kick the driver)", "action": "tick_now" },
                ],
                "confidence": 0.6,
            })
        })
        .collect();
    json!(findings)
}

/// JS: `async fix(_finding, _db)` — manual-only; ignores both params and always refuses.
pub fn fix(_finding: &Value, _db: &Connection) -> Value {
    json!({ "applied": false, "reason": "manual-only — operator picks A/B/C from Fixit options" })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "ATTACH ':memory:' AS brain; \
             CREATE TABLE cards (slug TEXT, status TEXT, driver_intent TEXT, updated_at TEXT); \
             CREATE TABLE brain.driver_ticks (id INTEGER PRIMARY KEY, card_slug TEXT, outcome TEXT, ts TEXT);",
        )
        .unwrap();
        c
    }

    fn insert_card(c: &Connection, slug: &str, status: &str, driver_intent: &str, updated_at: &str) {
        c.execute(
            "INSERT INTO cards (slug, status, driver_intent, updated_at) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![slug, status, driver_intent, updated_at],
        )
        .unwrap();
    }

    fn insert_tick(c: &Connection, id: i64, card_slug: &str, outcome: &str, ts: &str) {
        c.execute(
            "INSERT INTO brain.driver_ticks (id, card_slug, outcome, ts) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![id, card_slug, outcome, ts],
        )
        .unwrap();
    }

    #[test]
    fn absent_table_and_empty_table_return_empty() {
        let bare = Connection::open_in_memory().unwrap();
        assert_eq!(detect(&bare), json!([])); // cards absent → JS catch → []
        assert_eq!(detect(&db()), json!([]));
    }

    #[test]
    fn no_ticks_at_all_raises_finding_with_full_shape() {
        let c = db();
        insert_card(&c, "CARD-A", "doing", "play", "2025-12-31 20:00:00");
        let f = detect(&c);
        assert_eq!(f, json!([{
            "kind": "playing-card-no-driver-progress",
            "severity": "warn",
            "message": "CARD-A: board says playing (status=doing, driver_intent=play) but pipeline shows no progress in the last 60s. Board and pipeline are desynced.",
            "card_slug": "CARD-A",
            "detail": { "status": "doing", "driver_intent": "play", "updated_at": "2025-12-31 20:00:00", "stale_window_seconds": 60 },
            "suggested_action": "Investigate why the driver is not picking up this card (check Fixit for red notifs, inflight AI calls, or stuck contracts). Alternatively pause the card via Card view \u{2192} s.",
            "fix_options": [
                { "id": "A", "label": "Pause card (driver_intent=pause)", "action": "pause_card", "card_slug": "CARD-A" },
                { "id": "B", "label": "Stop card (driver_intent=stop)", "action": "stop_card", "card_slug": "CARD-A" },
                { "id": "C", "label": "Force a tick now (kick the driver)", "action": "tick_now" },
            ],
            "confidence": 0.6,
        }]));
    }

    #[test]
    fn recent_non_idle_progress_excludes_but_idle_and_stale_progress_still_finding() {
        // Frozen now 2026-01-01 00:00:00 → cutoff 2025-12-31 23:59:00 (>= is inclusive).
        let c = db();
        insert_card(&c, "CARD-RECENT-PROGRESS", "doing", "play", "2025-12-31 23:00:00");
        insert_card(&c, "CARD-IDLE-ONLY", "doing", "play", "2025-12-31 22:00:00");
        insert_card(&c, "CARD-OLD-PROGRESS", "doing", "play", "2025-12-31 21:00:00");
        insert_card(&c, "CARD-BOUNDARY", "doing", "play", "2025-12-31 23:30:00");
        insert_card(&c, "CARD-NOT-DOING", "backlog", "play", "2025-12-31 20:00:00");
        insert_card(&c, "CARD-NOT-PLAYING", "doing", "pause", "2025-12-31 20:00:00");
        insert_tick(&c, 10, "CARD-RECENT-PROGRESS", "fired", "2025-12-31 23:59:30");
        insert_tick(&c, 11, "CARD-IDLE-ONLY", "idle", "2025-12-31 23:59:45");
        insert_tick(&c, 12, "CARD-OLD-PROGRESS", "fired", "2025-12-31 23:00:00");
        insert_tick(&c, 13, "CARD-BOUNDARY", "fired", "2025-12-31 23:59:00"); // exactly at cutoff, >= includes it

        let f = detect(&c);
        let slugs: Vec<Value> = f.as_array().unwrap().iter().map(|x| x["card_slug"].clone()).collect();
        assert_eq!(json!(slugs), json!(["CARD-IDLE-ONLY", "CARD-OLD-PROGRESS"]));
    }

    #[test]
    fn fix_never_applies_and_ignores_both_args() {
        let c = Connection::open_in_memory().unwrap();
        assert_eq!(
            fix(&json!({ "kind": "playing-card-no-driver-progress", "card_slug": "CARD-A" }), &c),
            json!({ "applied": false, "reason": "manual-only — operator picks A/B/C from Fixit options" })
        );
        assert_eq!(
            fix(&Value::Null, &c),
            json!({ "applied": false, "reason": "manual-only — operator picks A/B/C from Fixit options" })
        );
    }
}
