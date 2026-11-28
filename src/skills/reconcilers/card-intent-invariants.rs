//! card-intent-invariants.rs — twin of skills/reconcilers/card-intent-invariants.mjs
//! (reconciler-object lane: the METHODS are the units, both db-last). NAMES ARE CONTRACT:
//! `detect` (detect(db)) and `fix` (fix(finding, db)). Two operator-visible invariants:
//!   1. status IN (done, archived) ⇒ driver_intent != 'play' (a shipped/abandoned card can't be
//!      "playing"). fix resets intent to 'auto'.
//!   2. At most ONE non-terminal card has driver_intent='play' at a time (doing is a queue).
//!      fix keeps the most-recently-updated play card and demotes the rest to 'paused'.
//!
//! Cross-module: both fixes route through `crate::engine_with_audit::withAuditAs` (actor
//! 'system:reconciler'), which dispatches to `crate::agents_god_god_card_driver_intent_set::godCardDriverIntentSet`
//! — both already-converted twins. god-card-driver-intent-set's own play-demotion side effect
//! never fires here (both fixes set intent to 'auto' or 'paused', never 'play').

use rusqlite::Connection;
use serde_json::{json, Value};

fn truthy(v: Option<&Value>) -> bool { match v { None|Some(Value::Null)=>false, Some(Value::Bool(b))=>*b, Some(Value::Number(n))=>n.as_f64().map_or(false,|f|f!=0.0&&!f.is_nan()), Some(Value::String(s))=>!s.is_empty(), Some(_)=>true } }
fn tmpl(v: Option<&Value>) -> String { match v { None=>"undefined".into(), Some(Value::Null)=>"null".into(), Some(Value::String(s))=>s.clone(), Some(Value::Bool(b))=>b.to_string(), Some(n@Value::Number(_))=>n.to_string(), Some(o)=>o.to_string() } } // = JS `${v}`

/// JS: `async detect(db)` — two independent invariant scans, each wrapped in its own
/// try/catch that maps a missing cards table to rows = [] (matches the JS `catch { /* table
/// absent */ }` per-query, not a single try around the whole function).
pub fn detect(db: &Connection) -> Value {
    let mut findings: Vec<Value> = Vec::new();

    let terminals = match crate::db_json::query_rows(
        db,
        "\n        SELECT slug, status, driver_intent\n        FROM cards\n        WHERE status IN ('done', 'archived') AND driver_intent = 'play'\n      ",
        &[],
    ) {
        Ok(v) => v.as_array().cloned().unwrap_or_default(),
        Err(_) => Vec::new(), // table absent
    };
    for c in &terminals {
        findings.push(json!({
            "kind": "card-intent-invariant-terminal-play",
            "severity": "drift",
            "message": format!(
                "{}: status='{}' but intent='play' — terminal cards can't be playing",
                tmpl(c.get("slug")), tmpl(c.get("status"))
            ),
            "card_slug": c.get("slug").cloned().unwrap_or(Value::Null),
            "detail": {
                "kind": "terminal-play",
                "current_status": c.get("status").cloned().unwrap_or(Value::Null),
                "current_intent": c.get("driver_intent").cloned().unwrap_or(Value::Null),
            },
            "suggested_action": "reset intent to auto",
        }));
    }

    let plays = match crate::db_json::query_rows(
        db,
        "\n        SELECT slug, status, driver_intent, updated_at\n        FROM cards\n        WHERE driver_intent = 'play' AND status NOT IN ('done', 'archived')\n        ORDER BY updated_at DESC, id DESC\n      ",
        &[],
    ) {
        Ok(v) => v.as_array().cloned().unwrap_or_default(),
        Err(_) => Vec::new(), // table absent
    };
    if plays.len() > 1 {
        let keep = &plays[0];
        for c in &plays[1..] {
            findings.push(json!({
                "kind": "card-intent-invariant-multiple-play",
                "severity": "drift",
                "message": format!(
                    "{}: extra play-intent card (keeping {} as the active one; demote this to paused)",
                    tmpl(c.get("slug")), tmpl(keep.get("slug"))
                ),
                "card_slug": c.get("slug").cloned().unwrap_or(Value::Null),
                "detail": {
                    "kind": "multiple-play",
                    "keep_slug": keep.get("slug").cloned().unwrap_or(Value::Null),
                    "this_updated_at": c.get("updated_at").cloned().unwrap_or(Value::Null),
                    "keep_updated_at": keep.get("updated_at").cloned().unwrap_or(Value::Null),
                },
                "suggested_action": "demote intent to paused",
            }));
        }
    }

    json!(findings)
}

/// JS: `async fix(finding, db)` — both fixes pre-check the drift still holds (stale-detection
/// guard), then compose god-card-driver-intent-set through with-audit (actor:
/// 'system:reconciler') so the flip is sealed into sacred_timeline.
pub fn fix(finding: &Value, db: &Connection) -> Value {
    let slug_opt = finding.get("card_slug");
    if !truthy(slug_opt) {
        return json!({ "applied": false, "reason": "missing card_slug" });
    }
    let slug_v = slug_opt.unwrap().clone();
    let subkind_v = finding.get("detail").and_then(|d| d.get("kind"));
    let subkind = subkind_v.and_then(|k| k.as_str());

    let set_intent_sealed = |intent: &str| -> Value {
        let payload = json!({
            "skill": "god-card-driver-intent-set",
            "wrappedInput": { "slug": slug_v.clone(), "intent": intent },
            "operation": "card.driver_intent.set",
            "entity_type": "card",
            "entity_id": slug_v.clone(),
        });
        match crate::engine_with_audit::withAuditAs(&payload, db, "system:reconciler") {
            Ok(r) => {
                let changed = r.get("changed").and_then(|v| v.as_bool()).unwrap_or(false);
                json!({ "applied": changed, "changes": { "table": "cards", "rows_affected": if changed { 1 } else { 0 } } })
            }
            // Not fixtured (playbook gotcha 9 — only the success path is fixtured): the JS has
            // no try/catch around this await either, so a with-audit failure here would reject
            // fix()'s promise. Fail loud rather than silently reporting a wrong `applied:false`.
            Err(e) => panic!("card-intent-invariants fix: with-audit failed on the fixtured success path: {e}"),
        }
    };

    if subkind == Some("terminal-play") {
        let cur = crate::db_json::query_rows(db, "SELECT status, driver_intent FROM cards WHERE slug = ?", &[slug_v.clone()])
            .unwrap()
            .as_array()
            .and_then(|a| a.first())
            .cloned();
        let ok = cur.as_ref().map_or(false, |c| {
            let status = c.get("status").and_then(|v| v.as_str()).unwrap_or("");
            let intent = c.get("driver_intent").and_then(|v| v.as_str()).unwrap_or("");
            (status == "done" || status == "archived") && intent == "play"
        });
        if !ok {
            return json!({ "applied": false, "changes": { "table": "cards", "rows_affected": 0 } });
        }
        return set_intent_sealed("auto");
    }
    if subkind == Some("multiple-play") {
        let cur = crate::db_json::query_rows(db, "SELECT driver_intent FROM cards WHERE slug = ?", &[slug_v.clone()])
            .unwrap()
            .as_array()
            .and_then(|a| a.first())
            .cloned();
        let intent = cur.as_ref().and_then(|c| c.get("driver_intent")).and_then(|v| v.as_str());
        if intent != Some("play") {
            return json!({ "applied": false, "changes": { "table": "cards", "rows_affected": 0 } });
        }
        return set_intent_sealed("paused");
    }
    json!({ "applied": false, "reason": format!("unknown subkind '{}'", tmpl(subkind_v)) })
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
        assert_eq!(detect(&bare), json!([]));
        assert_eq!(detect(&db()), json!([]));
    }

    #[test]
    fn detect_invariant1_terminal_plus_play() {
        let c = db();
        insert_card(&c, "CARD-DONE", "done", "play", "2025-12-20 10:00:00");
        insert_card(&c, "CARD-ARCHIVED", "archived", "play", "2025-12-21 10:00:00");
        insert_card(&c, "CARD-DONE-AUTO", "done", "auto", "2025-12-22 10:00:00");
        insert_card(&c, "CARD-DOING-PLAY", "doing", "play", "2025-12-23 10:00:00");
        let findings = detect(&c);
        let arr = findings.as_array().unwrap();
        let terminal: Vec<&Value> = arr.iter().filter(|f| f["kind"] == json!("card-intent-invariant-terminal-play")).collect();
        assert_eq!(terminal.len(), 2);
        assert_eq!(
            terminal[0],
            &json!({
                "kind": "card-intent-invariant-terminal-play",
                "severity": "drift",
                "message": "CARD-DONE: status='done' but intent='play' — terminal cards can't be playing",
                "card_slug": "CARD-DONE",
                "detail": { "kind": "terminal-play", "current_status": "done", "current_intent": "play" },
                "suggested_action": "reset intent to auto",
            })
        );
        assert_eq!(terminal[1]["card_slug"], json!("CARD-ARCHIVED"));
    }

    #[test]
    fn detect_invariant2_multiple_play_keeps_most_recent() {
        let c = db();
        insert_card(&c, "CARD-OLD", "doing", "play", "2025-12-20 10:00:00");
        insert_card(&c, "CARD-NEW", "backlog", "play", "2025-12-23 10:00:00");
        insert_card(&c, "CARD-MID", "doing", "play", "2025-12-22 10:00:00");
        insert_card(&c, "CARD-DONE-PLAY", "done", "play", "2025-12-24 10:00:00");
        insert_card(&c, "CARD-AUTO", "doing", "auto", "2025-12-21 10:00:00");
        let findings = detect(&c);
        let arr = findings.as_array().unwrap();
        let multi: Vec<&Value> = arr.iter().filter(|f| f["kind"] == json!("card-intent-invariant-multiple-play")).collect();
        assert_eq!(
            multi,
            vec![
                &json!({
                    "kind": "card-intent-invariant-multiple-play",
                    "severity": "drift",
                    "message": "CARD-MID: extra play-intent card (keeping CARD-NEW as the active one; demote this to paused)",
                    "card_slug": "CARD-MID",
                    "detail": { "kind": "multiple-play", "keep_slug": "CARD-NEW", "this_updated_at": "2025-12-22 10:00:00", "keep_updated_at": "2025-12-23 10:00:00" },
                    "suggested_action": "demote intent to paused",
                }),
                &json!({
                    "kind": "card-intent-invariant-multiple-play",
                    "severity": "drift",
                    "message": "CARD-OLD: extra play-intent card (keeping CARD-NEW as the active one; demote this to paused)",
                    "card_slug": "CARD-OLD",
                    "detail": { "kind": "multiple-play", "keep_slug": "CARD-NEW", "this_updated_at": "2025-12-20 10:00:00", "keep_updated_at": "2025-12-23 10:00:00" },
                    "suggested_action": "demote intent to paused",
                }),
            ]
        );
    }

    #[test]
    fn detect_single_play_card_no_finding() {
        let c = db();
        insert_card(&c, "CARD-SOLO", "doing", "play", "2025-12-20 10:00:00");
        assert_eq!(detect(&c), json!([]));
    }

    #[test]
    fn fix_missing_slug_and_unknown_subkind() {
        let c = db();
        assert_eq!(fix(&json!({}), &c), json!({ "applied": false, "reason": "missing card_slug" }));
        assert_eq!(fix(&json!({ "card_slug": "X" }), &c), json!({ "applied": false, "reason": "unknown subkind 'undefined'" }));
        assert_eq!(fix(&json!({ "card_slug": "X", "detail": { "kind": "bogus" } }), &c), json!({ "applied": false, "reason": "unknown subkind 'bogus'" }));
    }

    #[test]
    fn fix_terminal_play_stale_guard() {
        let c = db();
        assert_eq!(
            fix(&json!({ "card_slug": "GHOST", "detail": { "kind": "terminal-play" } }), &c),
            json!({ "applied": false, "changes": { "table": "cards", "rows_affected": 0 } })
        );
        insert_card(&c, "CARD-BACKLOG", "backlog", "play", "2025-12-20 10:00:00");
        assert_eq!(
            fix(&json!({ "card_slug": "CARD-BACKLOG", "detail": { "kind": "terminal-play" } }), &c),
            json!({ "applied": false, "changes": { "table": "cards", "rows_affected": 0 } })
        );
        insert_card(&c, "CARD-DONE-AUTO", "done", "auto", "2025-12-20 10:00:00");
        assert_eq!(
            fix(&json!({ "card_slug": "CARD-DONE-AUTO", "detail": { "kind": "terminal-play" } }), &c),
            json!({ "applied": false, "changes": { "table": "cards", "rows_affected": 0 } })
        );
    }

    #[test]
    fn fix_terminal_play_resets_to_auto_via_with_audit() {
        let c = db();
        insert_card(&c, "CARD-DONE", "done", "play", "2025-12-20 10:00:00");
        let r = fix(&json!({ "card_slug": "CARD-DONE", "detail": { "kind": "terminal-play" } }), &c);
        assert_eq!(r, json!({ "applied": true, "changes": { "table": "cards", "rows_affected": 1 } }));

        let (status, driver_intent, updated_at): (String, String, String) = c
            .query_row("SELECT status, driver_intent, updated_at FROM cards WHERE slug = 'CARD-DONE'", [], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })
            .unwrap();
        assert_eq!(status, "done");
        assert_eq!(driver_intent, "auto");
        assert_eq!(updated_at, "2026-01-01 00:00:00"); // frozen clock (via god-card-driver-intent-set)

        let tl = crate::db_json::query_rows(&c, "SELECT * FROM sacred_timeline", &[]).unwrap();
        let rows = tl.as_array().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["actor"], json!("system:reconciler"));
        assert_eq!(rows[0]["operation"], json!("card.driver_intent.set"));
        assert_eq!(rows[0]["entity_id"], json!("CARD-DONE"));
        assert_eq!(rows[0]["prev_print"], json!("GENESIS"));
    }

    #[test]
    fn fix_multiple_play_stale_guard() {
        let c = db();
        assert_eq!(
            fix(&json!({ "card_slug": "GHOST", "detail": { "kind": "multiple-play" } }), &c),
            json!({ "applied": false, "changes": { "table": "cards", "rows_affected": 0 } })
        );
        insert_card(&c, "CARD-PAUSED", "doing", "paused", "2025-12-20 10:00:00");
        assert_eq!(
            fix(&json!({ "card_slug": "CARD-PAUSED", "detail": { "kind": "multiple-play" } }), &c),
            json!({ "applied": false, "changes": { "table": "cards", "rows_affected": 0 } })
        );
    }

    #[test]
    fn fix_multiple_play_demotes_to_paused_via_with_audit() {
        let c = db();
        insert_card(&c, "CARD-KEEP", "doing", "play", "2025-12-23 10:00:00");
        insert_card(&c, "CARD-EXTRA", "doing", "play", "2025-12-20 10:00:00");
        let r = fix(&json!({ "card_slug": "CARD-EXTRA", "detail": { "kind": "multiple-play" } }), &c);
        assert_eq!(r, json!({ "applied": true, "changes": { "table": "cards", "rows_affected": 1 } }));

        let extra: String = c.query_row("SELECT driver_intent FROM cards WHERE slug = 'CARD-EXTRA'", [], |row| row.get(0)).unwrap();
        assert_eq!(extra, "paused");
        let keep: String = c.query_row("SELECT driver_intent FROM cards WHERE slug = 'CARD-KEEP'", [], |row| row.get(0)).unwrap();
        assert_eq!(keep, "play"); // untouched — only the fixed card is demoted

        let tl = crate::db_json::query_rows(&c, "SELECT * FROM sacred_timeline", &[]).unwrap();
        let rows = tl.as_array().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["operation"], json!("card.driver_intent.set"));
        assert_eq!(rows[0]["entity_id"], json!("CARD-EXTRA"));
    }
}
