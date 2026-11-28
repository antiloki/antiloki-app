//! card-blocked-but-intent-play.rs — twin of skills/reconcilers/card-blocked-but-intent-play.mjs
//! (reconciler-object lane: the METHODS are the units, both db-last). NAMES ARE CONTRACT:
//! `detect` (detect(db)) and `fix` (fix(finding, db)). Drift pattern: a card got status='blocked'
//! (terminal — the scheduler skips it forever) while driver_intent stayed 'play' (the board still
//! renders the play icon). detect surfaces the desync; fix is operator-authorized: it re-checks
//! the card is still 'blocked' (stale-detection guard), then flips status back to 'backlog' via
//! card-status-change COMPOSED THROUGH with-audit (so the flip is sealed into sacred_timeline).
//! driver_intent is left untouched — it's the operator's explicit signal.
//!
//! Cross-module: dispatches to `crate::engine_with_audit::withAuditAs` (actor-parameterized —
//! this reconciler's JS passes `actor: 'system:reconciler'`), which in turn dispatches to
//! `crate::entities_card_status_change::cardStatusChange` — both already-converted twins.

use rusqlite::Connection;
use serde_json::{json, Value};

fn truthy(v: Option<&Value>) -> bool { match v { None|Some(Value::Null)=>false, Some(Value::Bool(b))=>*b, Some(Value::Number(n))=>n.as_f64().map_or(false,|f|f!=0.0&&!f.is_nan()), Some(Value::String(s))=>!s.is_empty(), Some(_)=>true } }
fn str_qq(v: Option<&Value>) -> String { match v { None|Some(Value::Null)=>String::new(), Some(Value::String(s))=>s.clone(), Some(Value::Bool(b))=>b.to_string(), Some(n@Value::Number(_))=>n.to_string(), Some(o)=>o.to_string() } } // = JS String(v ?? '')
fn tmpl(v: Option<&Value>) -> String { match v { None=>"undefined".into(), Some(Value::Null)=>"null".into(), Some(x)=>str_qq(Some(x)) } } // = JS `${v}`

/// JS: `async detect(db)` — cards where status='blocked' AND driver_intent='play'. The try/catch
/// around the query maps a missing cards table to rows = [] (`catch { /* cards table absent */ }`).
pub fn detect(db: &Connection) -> Value {
    let rows = match crate::db_json::query_rows(
        db,
        "\n        SELECT slug, status, driver_intent, updated_at\n        FROM cards\n        WHERE status = 'blocked' AND driver_intent = 'play'\n      ",
        &[],
    ) {
        Ok(v) => v.as_array().cloned().unwrap_or_default(),
        Err(_) => Vec::new(), // cards table absent
    };
    let findings: Vec<Value> = rows
        .iter()
        .map(|r| {
            json!({
                "kind": "card-blocked-but-intent-play",
                "severity": "drift",
                "message": format!(
                    "{}: status=blocked + intent=play (scheduler skips this card while UI shows it as playing)",
                    tmpl(r.get("slug"))
                ),
                "card_slug": r.get("slug").cloned().unwrap_or(Value::Null),
                "detail": {
                    "current_status": r.get("status").cloned().unwrap_or(Value::Null),
                    "current_intent": r.get("driver_intent").cloned().unwrap_or(Value::Null),
                    "updated_at": r.get("updated_at").cloned().unwrap_or(Value::Null),
                },
                "suggested_action": "unblock to backlog (operator decides via Card view R-recover) OR set intent=stopped",
            })
        })
        .collect();
    json!(findings)
}

/// JS: `async fix(finding, db)` — operator-authorized unblock. Pre-check re-reads `status` (JS
/// `.get(slug)?.status`: undefined-on-no-row and a non-'blocked' status both fail the same
/// `cur !== 'blocked'` test) and refuses if the detection has gone stale. On success, composes
/// card-status-change through with-audit (AUDIT-SEAL, 2026-07-10 code-review P1) so the flip is
/// sealed into sacred_timeline; driver_intent is not touched (stays the operator's 'play' signal).
pub fn fix(finding: &Value, db: &Connection) -> Value {
    let slug = finding.get("card_slug");
    if !truthy(slug) {
        return json!({ "applied": false, "reason": "missing card_slug" });
    }
    let slug_v = slug.unwrap().clone();

    let cur = crate::db_json::query_rows(db, "SELECT status FROM cards WHERE slug = ?", &[slug_v.clone()])
        .unwrap()
        .as_array()
        .and_then(|a| a.first())
        .and_then(|r| r.get("status"))
        .cloned();
    if cur.as_ref().and_then(|v| v.as_str()) != Some("blocked") {
        return json!({ "applied": false, "reason": "card status changed since detection" });
    }

    let payload = json!({
        "skill": "card-status-change",
        "wrappedInput": { "slug": slug_v.clone(), "status": "backlog" },
        "operation": "card.status.change",
        "entity_type": "card",
        "entity_id": slug_v.clone(),
    });
    match crate::engine_with_audit::withAuditAs(&payload, db, "system:reconciler") {
        Ok(_) => json!({ "applied": true, "changes": { "table": "cards", "rows_affected": 1 } }),
        // Not fixtured (playbook gotcha 9 — only the success path is fixtured): the JS has no
        // try/catch around this await either, so a with-audit failure here would reject fix()'s
        // promise. This lane's signature can only return Value, so fail loud rather than silently
        // reporting a wrong `applied:false` for what is really a system error.
        Err(e) => panic!("card-blocked-but-intent-play fix: with-audit failed on the fixtured success path: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE cards (id INTEGER PRIMARY KEY AUTOINCREMENT, slug TEXT UNIQUE, status TEXT, driver_intent TEXT, updated_at TEXT);
             CREATE TABLE sacred_timeline (id INTEGER PRIMARY KEY AUTOINCREMENT, ts TEXT, actor TEXT, operation TEXT, entity_type TEXT, entity_id TEXT, before_json TEXT, after_json TEXT, payload TEXT, prev_print TEXT, digital_print TEXT, user_goal_id TEXT, exec_env TEXT, tick_id TEXT);",
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

    #[test]
    fn absent_table_and_empty_table_return_empty() {
        let bare = Connection::open_in_memory().unwrap();
        assert_eq!(detect(&bare), json!([])); // cards absent → JS catch → []
        assert_eq!(detect(&db()), json!([]));
    }

    #[test]
    fn detect_finds_only_blocked_plus_play_rows() {
        let c = db();
        insert_card(&c, "CARD-A", "blocked", "play", "2025-12-20 10:00:00");
        insert_card(&c, "CARD-B", "blocked", "pause", "2025-12-21 10:00:00");
        insert_card(&c, "CARD-C", "doing", "play", "2025-12-22 10:00:00");
        insert_card(&c, "CARD-D", "blocked", "play", "2025-12-23 10:00:00");
        let f = detect(&c);
        assert_eq!(f, json!([
            {
                "kind": "card-blocked-but-intent-play",
                "severity": "drift",
                "message": "CARD-A: status=blocked + intent=play (scheduler skips this card while UI shows it as playing)",
                "card_slug": "CARD-A",
                "detail": { "current_status": "blocked", "current_intent": "play", "updated_at": "2025-12-20 10:00:00" },
                "suggested_action": "unblock to backlog (operator decides via Card view R-recover) OR set intent=stopped",
            },
            {
                "kind": "card-blocked-but-intent-play",
                "severity": "drift",
                "message": "CARD-D: status=blocked + intent=play (scheduler skips this card while UI shows it as playing)",
                "card_slug": "CARD-D",
                "detail": { "current_status": "blocked", "current_intent": "play", "updated_at": "2025-12-23 10:00:00" },
                "suggested_action": "unblock to backlog (operator decides via Card view R-recover) OR set intent=stopped",
            },
        ]));
    }

    #[test]
    fn fix_guard_reasons() {
        let c = db();
        assert_eq!(fix(&json!({}), &c), json!({ "applied": false, "reason": "missing card_slug" }));
        assert_eq!(fix(&json!({ "card_slug": "" }), &c), json!({ "applied": false, "reason": "missing card_slug" }));
        assert_eq!(fix(&json!({ "card_slug": null }), &c), json!({ "applied": false, "reason": "missing card_slug" }));
        assert_eq!(fix(&json!({ "card_slug": "GHOST" }), &c), json!({ "applied": false, "reason": "card status changed since detection" }));
        insert_card(&c, "CARD-MOVED", "backlog", "play", "2025-12-01 00:00:00");
        assert_eq!(fix(&json!({ "card_slug": "CARD-MOVED" }), &c), json!({ "applied": false, "reason": "card status changed since detection" }));
    }

    #[test]
    fn fix_unblocks_via_with_audit_and_seals_one_event() {
        let c = db();
        insert_card(&c, "CARD-A", "blocked", "play", "2025-12-20 10:00:00");
        let r = fix(&json!({ "card_slug": "CARD-A" }), &c);
        assert_eq!(r, json!({ "applied": true, "changes": { "table": "cards", "rows_affected": 1 } }));

        let (status, driver_intent, updated_at): (String, String, String) = c
            .query_row(
                "SELECT status, driver_intent, updated_at FROM cards WHERE slug = 'CARD-A'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(status, "backlog"); // unblocked
        assert_eq!(driver_intent, "play"); // untouched — operator's explicit signal survives
        assert_eq!(updated_at, "2026-01-01 00:00:00"); // frozen clock (via card-status-change)

        let tl = crate::db_json::query_rows(&c, "SELECT * FROM sacred_timeline", &[]).unwrap();
        let rows = tl.as_array().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["operation"], json!("card.status.change"));
        assert_eq!(rows[0]["entity_id"], json!("CARD-A"));
        assert_eq!(rows[0]["prev_print"], json!("GENESIS"));
    }
}
