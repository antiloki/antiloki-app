//! card-causes-repeat-failures.rs — twin of skills/reconcilers/card-causes-repeat-failures.mjs
//! (reconciler-object lane: the METHODS are the units, both db-last). NAMES ARE CONTRACT: `detect`
//! (detect(db)) and `fix` (fix(finding, db)). Cards with >=2 failed brain.driver_ticks in the last
//! 10 minutes (COALESCE(driver_intent,'auto') != 'paused', status not terminal) surface as findings;
//! `fix` really pauses the card via the already-converted twin
//! agents_god_god_card_driver_intent_set::godCardDriverIntentSet — safety=safe (unlike most other
//! reconcilers in this folder, which are manual-only refusals, this one actually writes
//! cards.driver_intent).
//!
//! MIN_FAILURES=2 / WINDOW_MIN=10 are plain-data consts on the JS default object (not units) —
//! transcribed here only as the literals the two methods' output actually uses.
//!
//! cross-db lane: brain.driver_ticks is qualified SQL against the attached `brain` db; `cards` is
//! unqualified (main/state db) — transcribed verbatim from the JS SQL.
//!
//! datetime-injection lane: `datetime('now', '-10 minutes')` → frozen literal
//! `datetime('2026-01-01 00:00:00', '-10 minutes')`.
//!
//! fix()'s cross-call: JS `setIntent({ slug, intent: 'paused' }, { db })` is SYNC despite the outer
//! `async fix` — its guard-throw paths (missing slug/intent, bad intent enum, card not found, play-
//! on-terminal) are never reached here (slug is checked truthy first, intent is always the literal
//! 'paused', and any finding.card_slug this reconciler's own detect() produces already JOINed
//! against an existing cards row) — per playbook gotcha #9, fixture ONLY the success path. `.expect`
//! stands in for JS's unhandled-throw-propagates-through-async-fix semantics; it is never hit by a
//! valid fixture case.

use rusqlite::Connection;
use serde_json::{json, Map, Value};

fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}
fn str_qq(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(),
        Some(o) => o.to_string(),
    }
}
fn tmpl(v: Option<&Value>) -> String {
    match v {
        None => "undefined".into(),
        Some(Value::Null) => "null".into(),
        Some(x) => str_qq(Some(x)),
    }
}

/// JS: `async detect(db)` — the try/catch around the query maps ANY prepare error
/// (brain.driver_ticks or cards absent) to rows = [], exactly like the JS
/// `catch { /* tables absent */ }`.
pub fn detect(db: &Connection) -> Value {
    let rows = match crate::db_json::query_rows(
        db,
        &format!("\n        SELECT t.card_slug, COUNT(*) AS failures,\n               MAX(t.error_message) AS last_error,\n               MAX(t.decided_action) AS last_action\n        FROM brain.driver_ticks t\n        JOIN cards c ON c.slug = t.card_slug\n        WHERE t.outcome = 'failed'\n          AND t.started_at >= datetime({}, '-10 minutes')\n          AND COALESCE(c.driver_intent, 'auto') != 'paused'\n          AND c.status NOT IN ('done', 'archived')\n        GROUP BY t.card_slug\n        HAVING failures >= 2\n      ", crate::clock::now_sql_inner()),
        &[],
    ) {
        Ok(v) => v.as_array().cloned().unwrap_or_default(),
        Err(_) => Vec::new(), // tables absent
    };

    let findings: Vec<Value> = rows
        .iter()
        .map(|r| {
            let tail: String = str_qq(r.get("last_error")).chars().take(80).collect();
            json!({
                "kind": "card-causes-repeat-failures",
                "severity": "human_blocking",
                "message": format!(
                    "{}: {} failed tick(s) in 10min on {} ({})",
                    tmpl(r.get("card_slug")), tmpl(r.get("failures")), tmpl(r.get("last_action")), tail
                ),
                "card_slug": r.get("card_slug").cloned().unwrap_or(Value::Null),
                "detail": {
                    "failure_count": r.get("failures").cloned().unwrap_or(Value::Null),
                    "last_action": r.get("last_action").cloned().unwrap_or(Value::Null),
                    "last_error": r.get("last_error").cloned().unwrap_or(Value::Null),
                    "window_minutes": 10,
                },
                "suggested_action": "pause the card so it stops regenerating reds; investigate the failure_kind separately",
            })
        })
        .collect();
    json!(findings)
}

/// JS: `async fix(finding, db)` — pauses the card by calling the god-card-driver-intent-set twin.
pub fn fix(finding: &Value, db: &Connection) -> Value {
    let slug = finding.get("card_slug").cloned();
    if !truthy(slug.as_ref()) {
        return json!({ "applied": false, "reason": "missing card_slug" });
    }
    let slug = slug.unwrap();
    let set_input = json!({ "slug": slug, "intent": "paused" });
    let r = crate::agents_god_god_card_driver_intent_set::godCardDriverIntentSet(&set_input, db)
        .expect("card-causes-repeat-failures.fix: setIntent guard-throw not exercised by conform fixtures");
    let changed = r.get("changed").and_then(Value::as_bool).unwrap_or(false);
    let mut out = Map::new();
    out.insert("applied".into(), json!(changed));
    out.insert("changes".into(), json!({ "table": "cards", "intent_changed": changed }));
    if !changed {
        out.insert("reason".into(), json!("already paused"));
    }
    Value::Object(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "ATTACH ':memory:' AS brain; \
             CREATE TABLE cards (slug TEXT, status TEXT, driver_intent TEXT, updated_at TEXT); \
             CREATE TABLE brain.driver_ticks (id INTEGER PRIMARY KEY, card_slug TEXT, outcome TEXT, started_at TEXT, error_message TEXT, decided_action TEXT);",
        )
        .unwrap();
        c
    }

    fn insert_card(c: &Connection, slug: &str, status: &str, driver_intent: Option<&str>) {
        c.execute(
            "INSERT INTO cards (slug, status, driver_intent, updated_at) VALUES (?1, ?2, ?3, '2025-12-01 00:00:00')",
            rusqlite::params![slug, status, driver_intent],
        )
        .unwrap();
    }

    fn insert_tick(c: &Connection, id: i64, card_slug: &str, outcome: &str, started_at: &str, error_message: Option<&str>, decided_action: Option<&str>) {
        c.execute(
            "INSERT INTO brain.driver_ticks (id, card_slug, outcome, started_at, error_message, decided_action) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![id, card_slug, outcome, started_at, error_message, decided_action],
        )
        .unwrap();
    }

    #[test]
    fn absent_table_and_empty_table_return_empty() {
        let bare = Connection::open_in_memory().unwrap();
        assert_eq!(detect(&bare), json!([])); // brain.driver_ticks absent → JS catch → []
        assert_eq!(detect(&db()), json!([]));
    }

    #[test]
    fn filters_paused_terminal_threshold_join_and_window_then_aggregates() {
        let c = db();
        insert_card(&c, "CARD-A", "doing", None); // COALESCE(NULL,'auto') != 'paused' → eligible
        insert_card(&c, "CARD-B", "doing", Some("paused")); // excluded: driver_intent='paused'
        insert_card(&c, "CARD-C", "done", None); // excluded: terminal status
        insert_card(&c, "CARD-E", "doing", Some("auto")); // excluded: only 1 recent failure < MIN_FAILURES
        insert_card(&c, "CARD-G", "archived", None); // excluded: terminal status
        // CARD-F ticks reference a card that has no `cards` row at all — INNER JOIN drops it.

        let long_error = "E".repeat(100);
        insert_tick(&c, 1, "CARD-A", "failed", "2025-12-31 23:55:00", None, None);
        insert_tick(&c, 2, "CARD-A", "failed", "2025-12-31 23:58:00", Some(&long_error), Some("qa.run"));
        insert_tick(&c, 3, "CARD-A", "succeeded", "2025-12-31 23:59:00", None, None); // excluded: outcome != 'failed'
        insert_tick(&c, 4, "CARD-A", "failed", "2025-12-31 23:40:00", Some("old-error"), Some("old.action")); // excluded: outside 10min window
        insert_tick(&c, 5, "CARD-B", "failed", "2025-12-31 23:55:00", None, None);
        insert_tick(&c, 6, "CARD-B", "failed", "2025-12-31 23:58:00", None, None);
        insert_tick(&c, 7, "CARD-C", "failed", "2025-12-31 23:55:00", None, None);
        insert_tick(&c, 8, "CARD-C", "failed", "2025-12-31 23:58:00", None, None);
        insert_tick(&c, 9, "CARD-E", "failed", "2025-12-31 23:58:00", None, None);
        insert_tick(&c, 10, "CARD-G", "failed", "2025-12-31 23:55:00", None, None);
        insert_tick(&c, 11, "CARD-G", "failed", "2025-12-31 23:58:00", None, None);
        insert_tick(&c, 12, "CARD-F", "failed", "2025-12-31 23:55:00", None, None);
        insert_tick(&c, 13, "CARD-F", "failed", "2025-12-31 23:58:00", None, None);

        let f = detect(&c);
        let arr = f.as_array().unwrap();
        assert_eq!(arr.len(), 1); // only CARD-A survives every filter
        let tail80 = "E".repeat(80);
        assert_eq!(
            arr[0],
            json!({
                "kind": "card-causes-repeat-failures",
                "severity": "human_blocking",
                "message": format!("CARD-A: 2 failed tick(s) in 10min on qa.run ({tail80})"),
                "card_slug": "CARD-A",
                "detail": { "failure_count": 2, "last_action": "qa.run", "last_error": long_error, "window_minutes": 10 },
                "suggested_action": "pause the card so it stops regenerating reds; investigate the failure_kind separately",
            })
        );
    }

    #[test]
    fn window_boundary_is_inclusive_at_exactly_ten_minutes_ago() {
        // Frozen now 2026-01-01 00:00:00 → cutoff 2025-12-31 23:50:00 (>= is inclusive).
        let c = db();
        insert_card(&c, "CARD-BOUND", "backlog", Some("auto"));
        insert_tick(&c, 1, "CARD-BOUND", "failed", "2025-12-31 23:50:00", None, None); // exactly at cutoff → included
        insert_tick(&c, 2, "CARD-BOUND", "failed", "2025-12-31 23:59:00", None, None);
        insert_tick(&c, 3, "CARD-BOUND", "failed", "2025-12-31 23:49:59", None, None); // 1s before cutoff → excluded

        let f = detect(&c);
        let arr = f.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["card_slug"], json!("CARD-BOUND"));
        assert_eq!(arr[0]["detail"], json!({ "failure_count": 2, "last_action": Value::Null, "last_error": Value::Null, "window_minutes": 10 }));
        assert_eq!(arr[0]["message"], json!("CARD-BOUND: 2 failed tick(s) in 10min on null ()"));
    }

    #[test]
    fn fix_missing_or_empty_card_slug_never_touches_the_db() {
        let c = db();
        assert_eq!(fix(&json!({ "kind": "card-causes-repeat-failures" }), &c), json!({ "applied": false, "reason": "missing card_slug" }));
        assert_eq!(fix(&json!({ "card_slug": "" }), &c), json!({ "applied": false, "reason": "missing card_slug" }));
    }

    #[test]
    fn fix_pauses_an_active_card() {
        let c = db();
        insert_card(&c, "CARD-A", "doing", Some("auto"));
        let r = fix(&json!({ "card_slug": "CARD-A" }), &c);
        assert_eq!(r, json!({ "applied": true, "changes": { "table": "cards", "intent_changed": true } }));
        let intent: String = c.query_row("SELECT driver_intent FROM cards WHERE slug='CARD-A'", [], |row| row.get(0)).unwrap();
        assert_eq!(intent, "paused");
    }

    #[test]
    fn fix_on_already_paused_card_reports_unapplied_with_reason() {
        let c = db();
        insert_card(&c, "CARD-B", "doing", Some("paused"));
        let r = fix(&json!({ "card_slug": "CARD-B" }), &c);
        assert_eq!(r, json!({ "applied": false, "changes": { "table": "cards", "intent_changed": false }, "reason": "already paused" }));
    }
}
