//! red-notifs-blocking-dispatch.rs — twin of skills/reconcilers/red-notifs-blocking-dispatch.mjs
//! (reconciler-object lane: the METHODS are the units, both db-last). NAMES ARE CONTRACT:
//! `detect` (detect(db)) and `fix` (fix(finding, db)). The scheduler halts on ANY open red
//! notification (severity='error', dismissed_at IS NULL) regardless of age; detect surfaces
//! every one so the operator can read + dismiss per-item. fix() is a REAL fix (FIX-RECONCILER-
//! FIX-IS-REAL-NOT-JUST-DISMISS), not just a dismiss: it looks up the root-cause card via
//! notifications.related_card_slug (or a payload_json.card_slug fallback), and — only when that
//! card has >=2 failed brain.driver_ticks in the last 30min and isn't already paused/terminal —
//! pauses it via the already-converted god-card-driver-intent-set twin, before dismissing the
//! notification (idempotent: WHERE dismissed_at IS NULL).
//!
//! Cross-db: recentFails counts brain.driver_ticks — fixture declares "attach": ["brain"].
//! datetime('now') / datetime('now', '-30 minutes') → frozen-clock literal '2026-01-01 00:00:00'.
//!
//! The JS wraps BOTH the `cards` SELECT and the setIntent(...) call in ONE try/catch, pushing a
//! 'pause-failed' action on any failure from either. That catch is realistically unreachable via
//! legit input in the conform fixture: our own SELECT re-reads the exact table/slug
//! godCardDriverIntentSet re-reads internally, and the guard below (not paused/done/archived)
//! already excludes every input path that would make it throw — so it is modeled for
//! correctness but exercised only by a Rust unit test (a `cards`-table-absent db), not the
//! JS/Rust fixture, matching the treatment of other honestly-uncovered defensive catches in
//! sibling reconciler twins.

use rusqlite::Connection;
use serde_json::{json, Map, Value};

fn truthy(v: Option<&Value>) -> bool { match v { None|Some(Value::Null)=>false, Some(Value::Bool(b))=>*b, Some(Value::Number(n))=>n.as_f64().map_or(false,|f|f!=0.0&&!f.is_nan()), Some(Value::String(s))=>!s.is_empty(), Some(_)=>true } }
fn str_qq(v: Option<&Value>) -> String { match v { None|Some(Value::Null)=>String::new(), Some(Value::String(s))=>s.clone(), Some(Value::Bool(b))=>b.to_string(), Some(n@Value::Number(_))=>n.to_string(), Some(o)=>o.to_string() } } // = JS String(v ?? '')
fn tmpl(v: Option<&Value>) -> String { match v { None=>"undefined".into(), Some(Value::Null)=>"null".into(), Some(x)=>str_qq(Some(x)) } } // = JS `${v}`
fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) } // JS `??` right operand
fn take_chars(s: &str, n: usize) -> String { s.chars().take(n).collect() } // JS .slice(0,n)

/// JS: `async detect(db)` — open red notifications (severity='error', dismissed_at IS NULL),
/// oldest first. The try/catch around the query maps ANY prepare error (notifications table
/// absent) to [] exactly like the JS `catch { /* table absent */ }` leaving rows = [].
pub fn detect(db: &Connection) -> Value {
    let rows = match crate::db_json::query_rows(
        db,
        "\n        SELECT id, ts, kind, source, message\n        FROM notifications\n        WHERE severity = 'error' AND dismissed_at IS NULL\n        ORDER BY id ASC\n      ",
        &[],
    ) {
        Ok(v) => v.as_array().cloned().unwrap_or_default(),
        Err(_) => Vec::new(), // table absent
    };

    let findings: Vec<Value> = rows
        .iter()
        .map(|r| {
            let msg = take_chars(&str_qq(r.get("message")), 120);
            json!({
                "kind": "red-notif-blocking-dispatch",
                "severity": "human_blocking",
                "message": format!("red #{} ({}) blocking scheduler: {}", tmpl(r.get("id")), tmpl(r.get("kind")), msg),
                "detail": {
                    "notification_id": r.get("id").cloned().unwrap_or(Value::Null),
                    "ts": r.get("ts").cloned().unwrap_or(Value::Null),
                    "kind": r.get("kind").cloned().unwrap_or(Value::Null),
                    "source": r.get("source").cloned().unwrap_or(Value::Null),
                    "full_message": r.get("message").cloned().unwrap_or(Value::Null),
                },
                "suggested_action": "read the message; if root cause is fixed, dismiss",
            })
        })
        .collect();
    json!(findings)
}

/// JS: `async fix(finding, db)` — see module doc. Guard: falsy `finding.detail?.notification_id`
/// → `{applied:false, reason:'missing notification_id'}`. The initial notif SELECT is unguarded
/// in JS (no try/catch) — never fixtured on a table-absent db (schema always creates the table).
pub fn fix(finding: &Value, db: &Connection) -> Value {
    let id = finding.get("detail").and_then(|d| d.get("notification_id")).cloned();
    if !truthy(id.as_ref()) {
        return json!({ "applied": false, "reason": "missing notification_id" });
    }
    let id = id.unwrap();

    let notif = crate::db_json::query_rows(
        db,
        "\n      SELECT related_card_slug, payload_json FROM notifications WHERE id = ?\n    ",
        &[id.clone()],
    )
    .ok()
    .and_then(|v| v.as_array().and_then(|a| a.first().cloned()));

    let mut card_slug: Option<Value> = notif.as_ref().and_then(|n| nz(n.get("related_card_slug")).cloned());
    if !truthy(card_slug.as_ref()) {
        let payload_json = notif.as_ref().and_then(|n| n.get("payload_json"));
        if truthy(payload_json) {
            if let Some(s) = payload_json.and_then(|p| p.as_str()) {
                if let Ok(parsed) = serde_json::from_str::<Value>(s) {
                    card_slug = nz(parsed.get("card_slug")).cloned();
                }
            }
            // JSON.parse throwing (malformed payload_json) is a no-op catch in JS — card_slug
            // simply keeps whatever falsy value it already had, which is output-neutral (the
            // block below only fires on a TRUTHY card_slug either way).
        }
    }

    let mut actions_taken: Vec<Value> = Vec::new();

    if truthy(card_slug.as_ref()) {
        let card_slug_v = card_slug.clone().unwrap();
        let recent_fails: i64 = crate::db_json::query_rows(
            db,
            &format!("\n          SELECT COUNT(*) AS c FROM brain.driver_ticks\n          WHERE card_slug = ? AND outcome = 'failed'\n            AND started_at >= datetime({}, '-30 minutes')\n        ", crate::clock::now_sql_inner()),
            &[card_slug_v.clone()],
        )
        .ok()
        .and_then(|v| v.as_array().and_then(|a| a.first().cloned()))
        .and_then(|r| r.get("c").and_then(|c| c.as_i64()))
        .unwrap_or(0);

        if recent_fails >= 2 {
            let attempt: Result<Option<Value>, String> = (|| {
                let card_rows = crate::db_json::query_rows(
                    db,
                    "SELECT status, driver_intent FROM cards WHERE slug = ?",
                    &[card_slug_v.clone()],
                )?;
                let card = match card_rows.as_array().and_then(|a| a.first().cloned()) {
                    Some(c) => c,
                    None => return Ok(None),
                };
                let status = card.get("status").and_then(|v| v.as_str()).unwrap_or("");
                let driver_intent = card.get("driver_intent").and_then(|v| v.as_str());
                if driver_intent == Some("paused") || status == "done" || status == "archived" {
                    return Ok(None);
                }
                crate::agents_god_god_card_driver_intent_set::godCardDriverIntentSet(
                    &json!({ "slug": card_slug_v.clone(), "intent": "paused" }),
                    db,
                )?;
                Ok(Some(json!({
                    "card_slug": card_slug_v.clone(),
                    "action": "paused",
                    "reason": format!("{} failures in 30min", recent_fails),
                })))
            })();

            match attempt {
                Ok(Some(action)) => actions_taken.push(action),
                Ok(None) => {}
                Err(e) => actions_taken.push(json!({
                    "card_slug": card_slug_v.clone(),
                    "action": "pause-failed",
                    "reason": take_chars(&e, 100),
                })),
            }
        }
    }

    let changes = crate::db_json::run_write(
        db,
        &format!("\n      UPDATE notifications SET dismissed_at = {}\n      WHERE id = ? AND dismissed_at IS NULL\n    ", crate::clock::now_sql()),
        &[id.clone()],
    )
    .map(|(c, _)| c)
    .unwrap_or(0);

    actions_taken.push(json!({
        "notification_id": id.clone(),
        "action": "dismissed",
        "applied": changes > 0,
    }));

    let mut out = Map::new();
    out.insert("applied".into(), json!(changes > 0 || actions_taken.len() > 1));
    out.insert("changes".into(), json!({ "actions_taken": actions_taken }));
    if changes == 0 {
        out.insert("reason".into(), json!("notification was already dismissed; checked for root cause"));
    }
    Value::Object(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE notifications (id INTEGER PRIMARY KEY, ts TEXT, kind TEXT, source TEXT, message TEXT, severity TEXT, dismissed_at TEXT, related_card_slug TEXT, payload_json TEXT);
             CREATE TABLE cards (slug TEXT PRIMARY KEY, status TEXT, driver_intent TEXT, updated_at TEXT);
             ATTACH DATABASE ':memory:' AS brain;
             CREATE TABLE brain.driver_ticks (id INTEGER PRIMARY KEY, card_slug TEXT, outcome TEXT, started_at TEXT);",
        )
        .unwrap();
        c
    }

    #[test]
    fn detect_absent_and_empty_table_yield_empty_array() {
        let empty = Connection::open_in_memory().unwrap();
        assert_eq!(detect(&empty), json!([])); // table absent → JS catch → []
        assert_eq!(detect(&db()), json!([]));
    }

    #[test]
    fn detect_filters_severity_and_dismissed_then_sorts_id_asc_and_slices_message() {
        let c = db();
        c.execute_batch(
            "INSERT INTO notifications (id, ts, kind, source, message, severity, dismissed_at) VALUES (2, '2026-01-01 00:05:00', 'daemon-stop', 'driver', 'second', 'error', NULL);
             INSERT INTO notifications (id, ts, kind, source, message, severity, dismissed_at) VALUES (1, '2026-01-01 00:00:00', 'daemon-error', 'driver', 'first', 'error', NULL);
             INSERT INTO notifications (id, ts, kind, source, message, severity, dismissed_at) VALUES (3, '2026-01-01 00:06:00', 'generic', 'driver', 'already dismissed', 'error', '2026-01-01 00:10:00');
             INSERT INTO notifications (id, ts, kind, source, message, severity, dismissed_at) VALUES (4, '2026-01-01 00:07:00', 'generic', 'driver', 'just a warning', 'warn', NULL);
             INSERT INTO notifications (id, ts, kind, source, message, severity, dismissed_at) VALUES (5, '2026-01-01 00:08:00', 'generic', 'driver', NULL, 'error', NULL);",
        )
        .unwrap();
        let out = detect(&c);
        let arr = out.as_array().unwrap();
        let ids: Vec<i64> = arr.iter().map(|f| f["detail"]["notification_id"].as_i64().unwrap()).collect();
        assert_eq!(ids, vec![1, 2, 5]); // severity=error + undismissed only, ordered by id ASC

        assert_eq!(arr[0]["kind"], json!("red-notif-blocking-dispatch"));
        assert_eq!(arr[0]["severity"], json!("human_blocking"));
        assert_eq!(arr[0]["message"], json!("red #1 (daemon-error) blocking scheduler: first"));
        assert_eq!(arr[0]["detail"], json!({ "notification_id": 1, "ts": "2026-01-01 00:00:00", "kind": "daemon-error", "source": "driver", "full_message": "first" }));
        assert_eq!(arr[0]["suggested_action"], json!("read the message; if root cause is fixed, dismiss"));

        // null message → `(r.message ?? '').slice(0,120)` → ''
        assert_eq!(arr[2]["message"], json!("red #5 (generic) blocking scheduler: "));
        assert_eq!(arr[2]["detail"]["full_message"], Value::Null);
    }

    #[test]
    fn detect_truncates_message_to_120_chars_but_full_message_stays_untruncated() {
        let c = db();
        let long = "z".repeat(150);
        c.execute(
            "INSERT INTO notifications (id, ts, kind, source, message, severity, dismissed_at) VALUES (9, '2026-01-01 00:00:00', 'generic', 'driver', ?1, 'error', NULL)",
            rusqlite::params![long],
        )
        .unwrap();
        let out = detect(&c);
        let f = &out[0];
        assert_eq!(f["message"], json!(format!("red #9 (generic) blocking scheduler: {}", "z".repeat(120))));
        assert_eq!(f["detail"]["full_message"], json!(long));
    }

    #[test]
    fn fix_missing_or_falsy_notification_id() {
        let c = db();
        assert_eq!(fix(&json!({}), &c), json!({ "applied": false, "reason": "missing notification_id" }));
        assert_eq!(fix(&json!({ "detail": {} }), &c), json!({ "applied": false, "reason": "missing notification_id" }));
        assert_eq!(fix(&json!({ "detail": { "notification_id": 0 } }), &c), json!({ "applied": false, "reason": "missing notification_id" }));
        assert_eq!(fix(&json!({ "detail": { "notification_id": null } }), &c), json!({ "applied": false, "reason": "missing notification_id" }));
    }

    #[test]
    fn fix_notification_not_found_dismisses_zero_rows() {
        let c = db();
        let r = fix(&json!({ "detail": { "notification_id": 999 } }), &c);
        assert_eq!(
            r,
            json!({
                "applied": false,
                "changes": { "actions_taken": [{ "notification_id": 999, "action": "dismissed", "applied": false }] },
                "reason": "notification was already dismissed; checked for root cause",
            })
        );
    }

    #[test]
    fn fix_already_dismissed_notification_is_idempotent() {
        let c = db();
        c.execute("INSERT INTO notifications (id, message, severity, dismissed_at) VALUES (10, 'x', 'error', '2026-01-01 00:00:00')", []).unwrap();
        let r = fix(&json!({ "detail": { "notification_id": 10 } }), &c);
        assert_eq!(r["applied"], json!(false));
        assert_eq!(r["reason"], json!("notification was already dismissed; checked for root cause"));
    }

    #[test]
    fn fix_no_related_card_dismisses_only() {
        let c = db();
        c.execute("INSERT INTO notifications (id, message, severity, dismissed_at, related_card_slug, payload_json) VALUES (11, 'x', 'error', NULL, NULL, NULL)", []).unwrap();
        let r = fix(&json!({ "detail": { "notification_id": 11 } }), &c);
        assert_eq!(
            r,
            json!({
                "applied": true,
                "changes": { "actions_taken": [{ "notification_id": 11, "action": "dismissed", "applied": true }] },
            })
        );
        let dismissed_at: String = c.query_row("SELECT dismissed_at FROM notifications WHERE id = 11", [], |row| row.get(0)).unwrap();
        assert_eq!(dismissed_at, "2026-01-01 00:00:00");
    }

    #[test]
    fn fix_falls_back_to_payload_json_card_slug_when_related_card_slug_is_falsy() {
        let c = db();
        c.execute("INSERT INTO notifications (id, message, severity, dismissed_at, related_card_slug, payload_json) VALUES (12, 'x', 'error', NULL, '', '{\"card_slug\":\"CARD-PAYLOAD\"}')", []).unwrap();
        c.execute("INSERT INTO cards (slug, status, driver_intent) VALUES ('CARD-PAYLOAD', 'backlog', 'auto')", []).unwrap();
        c.execute_batch(
            "INSERT INTO brain.driver_ticks (card_slug, outcome, started_at) VALUES ('CARD-PAYLOAD', 'failed', '2025-12-31 23:45:00');
             INSERT INTO brain.driver_ticks (card_slug, outcome, started_at) VALUES ('CARD-PAYLOAD', 'failed', '2025-12-31 23:50:00');",
        )
        .unwrap();
        let r = fix(&json!({ "detail": { "notification_id": 12 } }), &c);
        assert_eq!(r["applied"], json!(true));
        let actions = r["changes"]["actions_taken"].as_array().unwrap();
        assert_eq!(actions[0], json!({ "card_slug": "CARD-PAYLOAD", "action": "paused", "reason": "2 failures in 30min" }));
        assert_eq!(actions[1], json!({ "notification_id": 12, "action": "dismissed", "applied": true }));
        let driver_intent: String = c.query_row("SELECT driver_intent FROM cards WHERE slug = 'CARD-PAYLOAD'", [], |row| row.get(0)).unwrap();
        assert_eq!(driver_intent, "paused");
    }

    #[test]
    fn fix_malformed_payload_json_is_swallowed_like_the_js_catch() {
        let c = db();
        c.execute("INSERT INTO notifications (id, message, severity, dismissed_at, related_card_slug, payload_json) VALUES (13, 'x', 'error', NULL, NULL, '{oops')", []).unwrap();
        let r = fix(&json!({ "detail": { "notification_id": 13 } }), &c);
        assert_eq!(r["applied"], json!(true));
        assert_eq!(r["changes"]["actions_taken"].as_array().unwrap().len(), 1); // no card_slug resolved → no pause attempt
    }

    #[test]
    fn fix_recent_fails_below_threshold_skips_pause() {
        let c = db();
        c.execute("INSERT INTO notifications (id, message, severity, dismissed_at, related_card_slug) VALUES (14, 'x', 'error', NULL, 'CARD-LOW')", []).unwrap();
        c.execute("INSERT INTO cards (slug, status, driver_intent) VALUES ('CARD-LOW', 'backlog', 'auto')", []).unwrap();
        c.execute_batch(
            "INSERT INTO brain.driver_ticks (card_slug, outcome, started_at) VALUES ('CARD-LOW', 'failed', '2025-12-31 23:45:00');
             INSERT INTO brain.driver_ticks (card_slug, outcome, started_at) VALUES ('CARD-LOW', 'failed', '2025-12-31 23:00:00');
             INSERT INTO brain.driver_ticks (card_slug, outcome, started_at) VALUES ('CARD-LOW', 'fired', '2025-12-31 23:50:00');",
        )
        .unwrap(); // only 1 counts: one is outside the 30min window, one has the wrong outcome
        let r = fix(&json!({ "detail": { "notification_id": 14 } }), &c);
        assert_eq!(r["changes"]["actions_taken"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn fix_skips_pause_when_card_already_paused_or_terminal_or_missing() {
        let c = db();
        for (id, slug, status, intent) in [(15, "CARD-PAUSED", "backlog", "paused"), (16, "CARD-DONE", "done", "auto"), (17, "CARD-ARCHIVED", "archived", "auto")] {
            c.execute(&format!("INSERT INTO notifications (id, message, severity, dismissed_at, related_card_slug) VALUES ({}, 'x', 'error', NULL, '{}')", id, slug), []).unwrap();
            c.execute(&format!("INSERT INTO cards (slug, status, driver_intent) VALUES ('{}', '{}', '{}')", slug, status, intent), []).unwrap();
            c.execute_batch(&format!(
                "INSERT INTO brain.driver_ticks (card_slug, outcome, started_at) VALUES ('{0}', 'failed', '2025-12-31 23:45:00');
                 INSERT INTO brain.driver_ticks (card_slug, outcome, started_at) VALUES ('{0}', 'failed', '2025-12-31 23:50:00');",
                slug
            ))
            .unwrap();
            let r = fix(&json!({ "detail": { "notification_id": id } }), &c);
            assert_eq!(r["changes"]["actions_taken"].as_array().unwrap().len(), 1, "slug={}", slug);
        }
        // card row missing entirely (no INSERT into cards) — same skip
        c.execute("INSERT INTO notifications (id, message, severity, dismissed_at, related_card_slug) VALUES (18, 'x', 'error', NULL, 'CARD-MISSING')", []).unwrap();
        c.execute_batch(
            "INSERT INTO brain.driver_ticks (card_slug, outcome, started_at) VALUES ('CARD-MISSING', 'failed', '2025-12-31 23:45:00');
             INSERT INTO brain.driver_ticks (card_slug, outcome, started_at) VALUES ('CARD-MISSING', 'failed', '2025-12-31 23:50:00');",
        )
        .unwrap();
        let r = fix(&json!({ "detail": { "notification_id": 18 } }), &c);
        assert_eq!(r["changes"]["actions_taken"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn fix_pause_failed_is_pushed_when_the_shared_try_block_errors() {
        // No `cards` table at all → the cards SELECT inside the shared try/catch errors,
        // mirroring the JS catch(e) branch (unreachable via legit fixture input — see module doc).
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE notifications (id INTEGER PRIMARY KEY, message TEXT, severity TEXT, dismissed_at TEXT, related_card_slug TEXT, payload_json TEXT);
             ATTACH DATABASE ':memory:' AS brain;
             CREATE TABLE brain.driver_ticks (id INTEGER PRIMARY KEY, card_slug TEXT, outcome TEXT, started_at TEXT);
             INSERT INTO notifications (id, message, severity, dismissed_at, related_card_slug) VALUES (20, 'x', 'error', NULL, 'CARD-NOTBL');
             INSERT INTO brain.driver_ticks (card_slug, outcome, started_at) VALUES ('CARD-NOTBL', 'failed', '2025-12-31 23:45:00');
             INSERT INTO brain.driver_ticks (card_slug, outcome, started_at) VALUES ('CARD-NOTBL', 'failed', '2025-12-31 23:50:00');",
        )
        .unwrap();
        let r = fix(&json!({ "detail": { "notification_id": 20 } }), &c);
        let actions = r["changes"]["actions_taken"].as_array().unwrap();
        assert_eq!(actions.len(), 2);
        assert_eq!(actions[0]["action"], json!("pause-failed"));
        assert_eq!(actions[0]["card_slug"], json!("CARD-NOTBL"));
    }
}
