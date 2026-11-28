//! stuck-red-notifs.rs — twin of skills/reconcilers/stuck-red-notifs.mjs (reconciler-object lane).
//! NAMES ARE CONTRACT: the units are the default object's METHODS `detect(db)` and `fix(finding,
//! db)`. Dismisses severity='error' notifications open >=STALE_AGE_MIN(30)min — the drift this
//! guards against is a substrate bug that regenerates the same red every tick; once the bug is
//! fixed the stale reds linger and keep halting BE dispatch (FEAT-DAEMON-PAUSES-ON-RED-
//! NOTIFICATIONS) until dismissed. Safety: safe (dismissal only sets dismissed_at — append-only,
//! reversible).
//!
//! FIX-RECONCILER-FIX-IS-REAL-NOT-JUST-DISMISS: fix() first pauses the notification's originating
//! card via the already-converted `agents_god_god_card_driver_intent_set::godCardDriverIntentSet`
//! twin IF that card has 2+ failed brain.driver_ticks in the last 30min and isn't already
//! paused/terminal — only THEN dismisses. The JS wraps the setIntent call in try/catch; the guard
//! right above it (card found there + not paused/terminal) means setIntent cannot actually fail on
//! this call path (same card_slug, same synchronous db, no intervening mutation) — the Err arm
//! below exists for parity with the JS control flow but is not fixture-exercised (playbook gotcha
//! 9: fixture only the success path).
//!
//! cross-db lane: brain.driver_ticks is qualified SQL against the attached `brain` db; `cards` is
//! unqualified (main/state db), transcribed verbatim.
//! datetime-injection lane: both `datetime('now', ...)` call sites use the frozen literal
//! '2026-01-01 00:00:00' in place of 'now'.

use rusqlite::Connection;
use serde_json::{json, Map, Value};

fn truthy(v: Option<&Value>) -> bool { match v { None|Some(Value::Null)=>false, Some(Value::Bool(b))=>*b, Some(Value::Number(n))=>n.as_f64().map_or(false,|f|f!=0.0&&!f.is_nan()), Some(Value::String(s))=>!s.is_empty(), Some(_)=>true } }
fn str_qq(v: Option<&Value>) -> String { match v { None|Some(Value::Null)=>String::new(), Some(Value::String(s))=>s.clone(), Some(Value::Bool(b))=>b.to_string(), Some(n@Value::Number(_))=>n.to_string(), Some(o)=>o.to_string() } } // = JS String(v ?? '')
fn tmpl(v: Option<&Value>) -> String { match v { None=>"undefined".into(), Some(Value::Null)=>"null".into(), Some(x)=>str_qq(Some(x)) } } // = JS `${v}`
fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) } // JS `??` right operand: nz(a).or(nz(b))

/// JS: `async detect(db)` — severity='error', undismissed, ts older than STALE_AGE_MIN (30min),
/// ASC by id. The try/catch around the query maps ANY prepare error (notifications table absent)
/// to rows = [] exactly like the JS `catch { /* table absent */ }`.
pub fn detect(db: &Connection) -> Value {
    let rows = crate::db_json::query_rows(
        db,
        &format!("SELECT id, ts, kind, source, message FROM notifications WHERE severity = 'error' AND dismissed_at IS NULL AND ts < datetime({}, '-30 minutes') ORDER BY id ASC", crate::clock::now_sql_inner()),
        &[],
    )
    .ok()
    .and_then(|v| v.as_array().cloned())
    .unwrap_or_default();

    let findings: Vec<Value> = rows
        .iter()
        .map(|r| {
            let id = r.get("id").cloned().unwrap_or(Value::Null);
            let ts = r.get("ts").cloned().unwrap_or(Value::Null);
            let kind = r.get("kind").cloned().unwrap_or(Value::Null);
            let source = r.get("source").cloned().unwrap_or(Value::Null);
            // (r.message ?? '').slice(0, 80)
            let msg80: String = str_qq(r.get("message")).chars().take(80).collect();

            let mut detail = Map::new();
            detail.insert("notification_id".into(), id.clone());
            detail.insert("ts".into(), ts.clone());
            detail.insert("kind".into(), kind);
            detail.insert("source".into(), source.clone());

            let mut f = Map::new();
            f.insert("kind".into(), json!("stuck-red-notif"));
            f.insert("severity".into(), json!("human_blocking"));
            f.insert("message".into(), json!(format!(
                "red notification id={} stuck since {} ({}): \"{}\"",
                tmpl(Some(&id)), tmpl(Some(&ts)), tmpl(Some(&source)), msg80
            )));
            f.insert("detail".into(), Value::Object(detail));
            f.insert("suggested_action".into(), json!("dismiss"));
            Value::Object(f)
        })
        .collect();
    json!(findings)
}

/// JS: `async fix(finding, db)` — FIX-RECONCILER-FIX-IS-REAL-NOT-JUST-DISMISS: pause the
/// originating card first (if 2+ recent failures and not already paused/terminal), then dismiss.
pub fn fix(finding: &Value, db: &Connection) -> Value {
    let id_v = finding.get("detail").and_then(|d| d.get("notification_id")).cloned();
    if !truthy(id_v.as_ref()) {
        return json!({ "applied": false, "reason": "missing notification_id" });
    }
    let id = id_v.unwrap();

    let notif = crate::db_json::query_rows(
        db,
        "SELECT related_card_slug, payload_json FROM notifications WHERE id = ?",
        &[id.clone()],
    )
    .ok()
    .and_then(|v| v.as_array().and_then(|a| a.first().cloned()));

    // card_slug = notif?.related_card_slug ?? null
    let mut card_slug: Value = notif
        .as_ref()
        .and_then(|n| nz(n.get("related_card_slug")))
        .cloned()
        .unwrap_or(Value::Null);

    // if (!card_slug && notif?.payload_json) { try { card_slug = JSON.parse(...)?.card_slug ?? null } catch {} }
    if !truthy(Some(&card_slug)) {
        let payload_json = notif.as_ref().and_then(|n| n.get("payload_json"));
        if truthy(payload_json) {
            if let Some(s) = payload_json.and_then(|v| v.as_str()) {
                if let Ok(parsed) = serde_json::from_str::<Value>(s) {
                    card_slug = nz(parsed.get("card_slug")).cloned().unwrap_or(Value::Null);
                }
                // JSON.parse throw -> caught -> card_slug stays as-is (already null here)
            }
        }
    }

    let mut actions_taken: Vec<Value> = Vec::new();

    if truthy(Some(&card_slug)) {
        let recent_fails: i64 = crate::db_json::query_rows(
            db,
            &format!("SELECT COUNT(*) AS c FROM brain.driver_ticks WHERE card_slug = ? AND outcome = 'failed' AND started_at >= datetime({}, '-30 minutes')", crate::clock::now_sql_inner()),
            &[card_slug.clone()],
        )
        .ok()
        .and_then(|v| v.as_array().and_then(|a| a.first().cloned()))
        .and_then(|r| r.get("c").cloned())
        .and_then(|c| c.as_i64())
        .unwrap_or(0);

        if recent_fails >= 2 {
            let card = crate::db_json::query_rows(
                db,
                "SELECT status, driver_intent FROM cards WHERE slug = ?",
                &[card_slug.clone()],
            )
            .ok()
            .and_then(|v| v.as_array().and_then(|a| a.first().cloned()));

            if let Some(card_row) = card {
                let driver_intent = card_row.get("driver_intent").and_then(|v| v.as_str()).unwrap_or("");
                let status = card_row.get("status").and_then(|v| v.as_str()).unwrap_or("");
                if driver_intent != "paused" && status != "done" && status != "archived" {
                    let set_input = json!({ "slug": card_slug.clone(), "intent": "paused" });
                    match crate::agents_god_god_card_driver_intent_set::godCardDriverIntentSet(&set_input, db) {
                        Ok(_) => {
                            let mut a = Map::new();
                            a.insert("card_slug".into(), card_slug.clone());
                            a.insert("action".into(), json!("paused"));
                            a.insert("reason".into(), json!(format!("{} failures in 30min", recent_fails)));
                            actions_taken.push(Value::Object(a));
                        }
                        Err(e) => {
                            // JS: actions_taken.push({ card_slug, action: 'pause-failed', reason: e?.message?.slice(0, 100) })
                            let mut a = Map::new();
                            a.insert("card_slug".into(), card_slug.clone());
                            a.insert("action".into(), json!("pause-failed"));
                            let reason: String = e.chars().take(100).collect();
                            a.insert("reason".into(), json!(reason));
                            actions_taken.push(Value::Object(a));
                        }
                    }
                }
            }
        }
    }

    let changes = crate::db_json::run_write(
        db,
        &format!("UPDATE notifications SET dismissed_at = {} WHERE id = ? AND dismissed_at IS NULL", crate::clock::now_sql()),
        &[id.clone()],
    )
    .map(|(c, _)| c)
    .unwrap_or(0);

    let mut dismissed = Map::new();
    dismissed.insert("notification_id".into(), id);
    dismissed.insert("action".into(), json!("dismissed"));
    dismissed.insert("applied".into(), json!(changes > 0));
    actions_taken.push(Value::Object(dismissed));

    let applied = changes > 0 || actions_taken.len() > 1;

    let mut out = Map::new();
    out.insert("applied".into(), json!(applied));
    let mut changes_obj = Map::new();
    changes_obj.insert("actions_taken".into(), json!(actions_taken));
    out.insert("changes".into(), Value::Object(changes_obj));
    if changes == 0 {
        // JS: reason: r.changes === 0 ? '...' : undefined — undefined keys are omitted from output.
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
            "ATTACH ':memory:' AS brain; \
             CREATE TABLE notifications (id INTEGER PRIMARY KEY, ts TEXT, kind TEXT, source TEXT, message TEXT, severity TEXT, dismissed_at TEXT, related_card_slug TEXT, payload_json TEXT); \
             CREATE TABLE cards (slug TEXT PRIMARY KEY, status TEXT, driver_intent TEXT, updated_at TEXT); \
             CREATE TABLE brain.driver_ticks (id INTEGER PRIMARY KEY, card_slug TEXT, outcome TEXT, started_at TEXT);",
        )
        .unwrap();
        c
    }

    fn insert_notif(
        c: &Connection,
        id: i64,
        ts: &str,
        severity: &str,
        dismissed_at: Option<&str>,
        related_card_slug: Option<&str>,
        payload_json: Option<&str>,
    ) {
        c.execute(
            "INSERT INTO notifications (id, ts, kind, source, message, severity, dismissed_at, related_card_slug, payload_json) VALUES (?, ?, 'daemon-error', 'tl.contract.amend', 'msg', ?, ?, ?, ?)",
            rusqlite::params![id, ts, severity, dismissed_at, related_card_slug, payload_json],
        )
        .unwrap();
    }

    fn insert_card(c: &Connection, slug: &str, status: &str, driver_intent: &str) {
        c.execute(
            "INSERT INTO cards (slug, status, driver_intent, updated_at) VALUES (?, ?, ?, '2025-12-01 00:00:00')",
            rusqlite::params![slug, status, driver_intent],
        )
        .unwrap();
    }

    fn insert_tick(c: &Connection, card_slug: &str, outcome: &str, started_at: &str) {
        c.execute(
            "INSERT INTO brain.driver_ticks (card_slug, outcome, started_at) VALUES (?, ?, ?)",
            rusqlite::params![card_slug, outcome, started_at],
        )
        .unwrap();
    }

    #[test]
    fn absent_table_and_empty_table_yield_empty_array() {
        let empty = Connection::open_in_memory().unwrap();
        assert_eq!(detect(&empty), json!([])); // table absent -> JS catch -> []
        assert_eq!(detect(&db()), json!([]));
    }

    #[test]
    fn filters_severity_dismissed_age_then_sorts_asc_and_truncates_message() {
        let c = db();
        insert_notif(&c, 1, "2025-12-31 23:00:00", "error", None, None, None); // stuck -> surfaces
        insert_notif(&c, 2, "2025-12-31 23:45:00", "error", None, None, None); // < 30min old -> excluded
        insert_notif(&c, 3, "2025-12-31 22:00:00", "error", Some("2025-12-31 23:00:00"), None, None); // dismissed -> excluded
        insert_notif(&c, 4, "2025-12-31 22:00:00", "warn", None, None, None); // not error -> excluded
        let long = "y".repeat(120);
        c.execute(
            "INSERT INTO notifications (id, ts, kind, source, message, severity) VALUES (5, '2025-12-31 22:00:00', 'daemon-error', 'watchdog', ?, 'error')",
            rusqlite::params![long],
        )
        .unwrap();
        let out = detect(&c);
        let arr = out.as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0]["detail"]["notification_id"], json!(1));
        assert_eq!(arr[0]["kind"], json!("stuck-red-notif"));
        assert_eq!(arr[0]["severity"], json!("human_blocking"));
        assert_eq!(arr[0]["message"], json!("red notification id=1 stuck since 2025-12-31 23:00:00 (tl.contract.amend): \"msg\""));
        assert_eq!(arr[0]["suggested_action"], json!("dismiss"));
        assert_eq!(arr[1]["detail"]["notification_id"], json!(5));
        assert_eq!(arr[1]["message"], json!(format!("red notification id=5 stuck since 2025-12-31 22:00:00 (watchdog): \"{}\"", "y".repeat(80))));
    }

    #[test]
    fn null_fields_render_like_js_template_literals() {
        let c = db();
        c.execute(
            "INSERT INTO notifications (id, ts, kind, source, message, severity) VALUES (6, '2025-12-31 22:00:00', NULL, NULL, NULL, 'error')",
            [],
        )
        .unwrap();
        let out = detect(&c);
        assert_eq!(out[0]["message"], json!("red notification id=6 stuck since 2025-12-31 22:00:00 (null): \"\""));
        assert_eq!(out[0]["detail"], json!({ "notification_id": 6, "ts": "2025-12-31 22:00:00", "kind": null, "source": null }));
    }

    #[test]
    fn fix_missing_notification_id_returns_early_untouched() {
        let c = db();
        let r = fix(&json!({}), &c);
        assert_eq!(r, json!({ "applied": false, "reason": "missing notification_id" }));
    }

    #[test]
    fn fix_dismisses_when_no_card_slug() {
        let c = db();
        insert_notif(&c, 10, "2025-12-31 23:00:00", "error", None, None, None);
        let r = fix(&json!({ "detail": { "notification_id": 10 } }), &c);
        assert_eq!(
            r,
            json!({ "applied": true, "changes": { "actions_taken": [{ "notification_id": 10, "action": "dismissed", "applied": true }] } })
        );
        let dismissed_at: Option<String> = c.query_row("SELECT dismissed_at FROM notifications WHERE id=10", [], |row| row.get(0)).unwrap();
        assert!(dismissed_at.is_some());
    }

    #[test]
    fn fix_card_slug_from_payload_json_below_fail_threshold_dismisses_only() {
        let c = db();
        insert_notif(&c, 11, "2025-12-31 23:00:00", "error", None, None, Some(r#"{"card_slug":"CARD-A"}"#));
        insert_card(&c, "CARD-A", "doing", "auto");
        insert_tick(&c, "CARD-A", "failed", "2025-12-31 23:50:00"); // only 1 fail
        let r = fix(&json!({ "detail": { "notification_id": 11 } }), &c);
        assert_eq!(r["changes"]["actions_taken"], json!([{ "notification_id": 11, "action": "dismissed", "applied": true }]));
        let driver_intent: String = c.query_row("SELECT driver_intent FROM cards WHERE slug='CARD-A'", [], |row| row.get(0)).unwrap();
        assert_eq!(driver_intent, "auto");
    }

    #[test]
    fn fix_pauses_card_with_2plus_recent_failures_then_dismisses() {
        let c = db();
        insert_notif(&c, 12, "2025-12-31 23:00:00", "error", None, Some("CARD-B"), None);
        insert_card(&c, "CARD-B", "doing", "auto");
        insert_tick(&c, "CARD-B", "failed", "2025-12-31 23:50:00");
        insert_tick(&c, "CARD-B", "failed", "2025-12-31 23:55:00");
        let r = fix(&json!({ "detail": { "notification_id": 12 } }), &c);
        assert_eq!(
            r["changes"]["actions_taken"],
            json!([
                { "card_slug": "CARD-B", "action": "paused", "reason": "2 failures in 30min" },
                { "notification_id": 12, "action": "dismissed", "applied": true },
            ])
        );
        assert_eq!(r["applied"], json!(true));
        let driver_intent: String = c.query_row("SELECT driver_intent FROM cards WHERE slug='CARD-B'", [], |row| row.get(0)).unwrap();
        assert_eq!(driver_intent, "paused");
    }

    #[test]
    fn fix_never_pauses_a_terminal_card() {
        let c = db();
        insert_notif(&c, 13, "2025-12-31 23:00:00", "error", None, Some("CARD-C"), None);
        insert_card(&c, "CARD-C", "done", "auto");
        insert_tick(&c, "CARD-C", "failed", "2025-12-31 23:50:00");
        insert_tick(&c, "CARD-C", "failed", "2025-12-31 23:55:00");
        let r = fix(&json!({ "detail": { "notification_id": 13 } }), &c);
        assert_eq!(r["changes"]["actions_taken"], json!([{ "notification_id": 13, "action": "dismissed", "applied": true }]));
        let driver_intent: String = c.query_row("SELECT driver_intent FROM cards WHERE slug='CARD-C'", [], |row| row.get(0)).unwrap();
        assert_eq!(driver_intent, "auto");
    }

    #[test]
    fn fix_skips_pause_when_already_paused() {
        let c = db();
        insert_notif(&c, 14, "2025-12-31 23:00:00", "error", None, Some("CARD-D"), None);
        insert_card(&c, "CARD-D", "doing", "paused");
        insert_tick(&c, "CARD-D", "failed", "2025-12-31 23:50:00");
        insert_tick(&c, "CARD-D", "failed", "2025-12-31 23:55:00");
        let r = fix(&json!({ "detail": { "notification_id": 14 } }), &c);
        assert_eq!(r["changes"]["actions_taken"], json!([{ "notification_id": 14, "action": "dismissed", "applied": true }]));
    }

    #[test]
    fn fix_already_dismissed_reports_applied_false_with_reason() {
        let c = db();
        insert_notif(&c, 15, "2025-12-31 23:00:00", "error", Some("2025-12-31 12:00:00"), None, None);
        let r = fix(&json!({ "detail": { "notification_id": 15 } }), &c);
        assert_eq!(
            r,
            json!({
                "applied": false,
                "changes": { "actions_taken": [{ "notification_id": 15, "action": "dismissed", "applied": false }] },
                "reason": "notification was already dismissed; checked for root cause",
            })
        );
    }

    #[test]
    fn fix_malformed_payload_json_is_swallowed_dismisses_only() {
        let c = db();
        insert_notif(&c, 16, "2025-12-31 23:00:00", "error", None, None, Some("not-json{"));
        let r = fix(&json!({ "detail": { "notification_id": 16 } }), &c);
        assert_eq!(r["changes"]["actions_taken"], json!([{ "notification_id": 16, "action": "dismissed", "applied": true }]));
    }
}
