//! contracts-rejected-no-redraft.rs — twin of skills/reconcilers/contracts-rejected-no-redraft.mjs
//! (reconciler-object lane: the METHODS are the units, both db-last). NAMES ARE CONTRACT: `detect`
//! (detect(db)) and `fix` (fix(finding, db)). Drift pattern: a contract went to status='rejected'
//! (e.g. after pressing Restart) but no successor contract ever appeared because the redraft
//! pipeline never got a chance to run — card.driver_intent isn't 'play' (scheduler skipped) or the
//! card is 'blocked' (terminal). detect surfaces only the LATEST contract per card (no ct2.id >
//! ct.id for the same card_slug — i.e. no successor landed) among those where the scheduler
//! actually looks stuck. fix nudges the scheduler: sets driver_intent=play via
//! god-card-driver-intent-set (COMPOSED THROUGH with-audit so the mutation is sealed), then — only
//! if that changed something AND the finding's own snapshot said the card was 'blocked' AND the
//! card is STILL 'blocked' right now (stale-detection re-check) — also bounces card.status back to
//! 'backlog' via card-status-change (also through with-audit).
//!
//! Cross-module: dispatches to `crate::engine_with_audit::withAuditAs` (actor-parameterized — this
//! reconciler's JS passes `actor: 'system:reconciler'`), which in turn dispatches to
//! `crate::agents_god_god_card_driver_intent_set::godCardDriverIntentSet` and
//! `crate::entities_card_status_change::cardStatusChange` — both already-converted twins. Neither
//! wrappedInput sets `halt_path`, matching the JS exactly (no override) — god-card-driver-intent-
//! set's own DEFAULT_HALT_PATH fallback is a real filesystem check but resolves to a path that does
//! not exist on the conform machine in either lane, so `cleared_halt` is deterministically false.

use rusqlite::Connection;
use serde_json::{json, Value};

fn truthy(v: Option<&Value>) -> bool { match v { None|Some(Value::Null)=>false, Some(Value::Bool(b))=>*b, Some(Value::Number(n))=>n.as_f64().map_or(false,|f|f!=0.0&&!f.is_nan()), Some(Value::String(s))=>!s.is_empty(), Some(_)=>true } }
fn tmpl(v: Option<&Value>) -> String { match v { None=>"undefined".into(), Some(Value::Null)=>"null".into(), Some(Value::String(s))=>s.clone(), Some(Value::Bool(b))=>b.to_string(), Some(n@Value::Number(_))=>n.to_string(), Some(o)=>o.to_string() } } // = JS `${v}`

/// JS: `async detect(db)` — rejected contracts (with a rejection_reason_md, not yet superseded)
/// whose card is not terminal (done/archived) and which is the LATEST contract for its card (no
/// ct2.id > ct.id for the same card_slug). Of those, only the ones where the scheduler actually
/// looks stuck: card_intent isn't 'play', OR the card is 'blocked' (even with intent=play a
/// blocked card can't progress). The try/catch around the query maps a missing contracts/cards
/// table to rows = [] (`catch { /* tables absent */ }`).
pub fn detect(db: &Connection) -> Value {
    let rows = match crate::db_json::query_rows(
        db,
        "\n        SELECT ct.slug AS contract_slug, ct.card_slug, ct.rejection_reason_md, ct.failure_kind,\n               c.status AS card_status, c.driver_intent AS card_intent\n        FROM contracts ct\n        JOIN cards c ON c.slug = ct.card_slug\n        WHERE ct.status = 'rejected'\n          AND COALESCE(ct.superseded_by, '') = ''\n          AND ct.rejection_reason_md IS NOT NULL\n          AND c.status NOT IN ('done', 'archived')\n          AND NOT EXISTS (\n            SELECT 1 FROM contracts ct2\n            WHERE ct2.card_slug = ct.card_slug AND ct2.id > ct.id\n          )\n      ",
        &[],
    ) {
        Ok(v) => v.as_array().cloned().unwrap_or_default(),
        Err(_) => Vec::new(), // tables absent
    };

    let findings: Vec<Value> = rows
        .iter()
        .filter(|r| {
            let card_intent = r.get("card_intent").and_then(|v| v.as_str());
            let card_status = r.get("card_status").and_then(|v| v.as_str());
            card_intent != Some("play") || card_status == Some("blocked")
        })
        .map(|r| {
            let failure_kind = r.get("failure_kind").cloned().unwrap_or(Value::Null);
            let failure_kind_disp = if failure_kind.is_null() { "no failure_kind".to_string() } else { tmpl(Some(&failure_kind)) };
            json!({
                "kind": "contract-rejected-no-redraft",
                "severity": "human_blocking",
                "message": format!(
                    "{}: contract {} rejected ({}), no successor — scheduler is blocked (card.status={}, intent={})",
                    tmpl(r.get("card_slug")),
                    tmpl(r.get("contract_slug")),
                    failure_kind_disp,
                    tmpl(r.get("card_status")),
                    tmpl(r.get("card_intent")),
                ),
                "card_slug": r.get("card_slug").cloned().unwrap_or(Value::Null),
                "contract_slug": r.get("contract_slug").cloned().unwrap_or(Value::Null),
                "detail": {
                    "card_status": r.get("card_status").cloned().unwrap_or(Value::Null),
                    "card_intent": r.get("card_intent").cloned().unwrap_or(Value::Null),
                    "failure_kind": failure_kind,
                },
                "suggested_action": "set driver_intent=play; unblock card.status if blocked",
            })
        })
        .collect();
    json!(findings)
}

/// JS: `async fix(finding, db)` — nudges the scheduler. Sets driver_intent=play via
/// god-card-driver-intent-set (composed through with-audit, actor='system:reconciler'). If that
/// changed the intent AND the finding recorded the card as 'blocked' AND the card is STILL
/// 'blocked' right now (re-read — stale-detection guard), also bounces status to 'backlog' via
/// card-status-change (also through with-audit). Neither with-audit call's error path is fixtured
/// (playbook gotcha 9 — the JS has no try/catch around either await either).
pub fn fix(finding: &Value, db: &Connection) -> Value {
    let slug = finding.get("card_slug");
    if !truthy(slug) {
        return json!({ "applied": false, "reason": "missing card_slug" });
    }
    let slug_v = slug.unwrap().clone();

    let payload1 = json!({
        "skill": "god-card-driver-intent-set",
        "wrappedInput": { "slug": slug_v.clone(), "intent": "play" },
        "operation": "card.driver_intent.set",
        "entity_type": "card",
        "entity_id": slug_v.clone(),
    });
    let r = match crate::engine_with_audit::withAuditAs(&payload1, db, "system:reconciler") {
        Ok(v) => v,
        Err(e) => panic!("contracts-rejected-no-redraft fix: with-audit (god-card-driver-intent-set) failed on the fixtured success path: {e}"),
    };
    let changed = r.get("changed").cloned().unwrap_or(json!(false));

    let detail_status = finding.get("detail").and_then(|d| d.get("card_status")).and_then(|v| v.as_str());
    if changed.as_bool() == Some(true) && detail_status == Some("blocked") {
        let cur_status = crate::db_json::query_rows(db, "SELECT status FROM cards WHERE slug = ?", &[slug_v.clone()])
            .unwrap()
            .as_array()
            .and_then(|a| a.first())
            .and_then(|row| row.get("status"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        if cur_status.as_deref() == Some("blocked") {
            let payload2 = json!({
                "skill": "card-status-change",
                "wrappedInput": { "slug": slug_v.clone(), "status": "backlog" },
                "operation": "card.status.change",
                "entity_type": "card",
                "entity_id": slug_v.clone(),
            });
            if let Err(e) = crate::engine_with_audit::withAuditAs(&payload2, db, "system:reconciler") {
                panic!("contracts-rejected-no-redraft fix: with-audit (card-status-change) failed on the fixtured success path: {e}");
            }
        }
    }

    let demoted_others = r.get("demoted_count").filter(|v| !v.is_null()).cloned().unwrap_or(json!(0));
    json!({
        "applied": changed,
        "changes": { "table": "cards", "intent_changed": changed, "demoted_others": demoted_others },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE contracts (id INTEGER PRIMARY KEY AUTOINCREMENT, slug TEXT, card_slug TEXT, status TEXT, rejection_reason_md TEXT, failure_kind TEXT, superseded_by TEXT);
             CREATE TABLE cards (id INTEGER PRIMARY KEY AUTOINCREMENT, slug TEXT UNIQUE, status TEXT, driver_intent TEXT, updated_at TEXT);
             CREATE TABLE sacred_timeline (id INTEGER PRIMARY KEY AUTOINCREMENT, ts TEXT, actor TEXT, operation TEXT, entity_type TEXT, entity_id TEXT, before_json TEXT, after_json TEXT, payload TEXT, prev_print TEXT, digital_print TEXT, user_goal_id TEXT, exec_env TEXT, tick_id TEXT);",
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
    fn insert_contract(c: &Connection, slug: &str, card_slug: &str, status: &str, rejection_reason_md: Option<&str>, failure_kind: Option<&str>, superseded_by: Option<&str>) {
        c.execute(
            "INSERT INTO contracts (slug, card_slug, status, rejection_reason_md, failure_kind, superseded_by) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![slug, card_slug, status, rejection_reason_md, failure_kind, superseded_by],
        )
        .unwrap();
    }

    #[test]
    fn absent_tables_and_empty_tables_yield_empty_array() {
        let bare = Connection::open_in_memory().unwrap();
        assert_eq!(detect(&bare), json!([])); // contracts/cards absent → JS catch → []
        assert_eq!(detect(&db()), json!([]));
    }

    #[test]
    fn filters_every_branch_and_maps_full_finding_shape() {
        let c = db();
        insert_card(&c, "CARD-A", "doing", Some("stopped"));
        insert_card(&c, "CARD-B", "blocked", Some("play"));
        insert_card(&c, "CARD-C", "doing", Some("play"));
        insert_card(&c, "CARD-D", "done", Some("stopped"));
        insert_card(&c, "CARD-E", "doing", None);
        insert_card(&c, "CARD-F", "doing", Some("stopped"));
        insert_card(&c, "CARD-G", "doing", Some("stopped"));
        insert_card(&c, "CARD-H", "doing", Some("stopped"));
        insert_card(&c, "CARD-I", "doing", Some("stopped"));

        // CT-A1: included — intent 'stopped' != 'play'
        insert_contract(&c, "CT-A1", "CARD-A", "rejected", Some("timeout during microtask 3"), Some("timeout"), Some(""));
        // CT-B1: included — card status 'blocked' (even though intent='play')
        insert_contract(&c, "CT-B1", "CARD-B", "rejected", Some("qa failed"), None, Some(""));
        // CT-C1: excluded — intent='play' AND status != 'blocked'
        insert_contract(&c, "CT-C1", "CARD-C", "rejected", Some("minor issue"), Some("lint"), Some(""));
        // CT-D1: excluded — card status is terminal (done)
        insert_contract(&c, "CT-D1", "CARD-D", "rejected", Some("terminal card"), None, Some(""));
        // CT-E1: included — driver_intent is NULL (!= 'play')
        insert_contract(&c, "CT-E1", "CARD-E", "rejected", Some("oom crash"), Some("oom"), Some(""));
        // CT-F1/CT-F2: excluded — CT-F1 has a newer successor contract for the same card
        insert_contract(&c, "CT-F1", "CARD-F", "rejected", Some("has successor"), None, Some(""));
        insert_contract(&c, "CT-F2", "CARD-F", "proposed", None, None, Some(""));
        // CT-G1: excluded — status is not 'rejected'
        insert_contract(&c, "CT-G1", "CARD-G", "proposed", Some("not rejected status"), None, Some(""));
        // CT-H1: excluded — superseded_by is non-empty
        insert_contract(&c, "CT-H1", "CARD-H", "rejected", Some("superseded"), None, Some("CT-NEWER"));
        // CT-I1: excluded — rejection_reason_md IS NULL
        insert_contract(&c, "CT-I1", "CARD-I", "rejected", None, None, Some(""));

        let out = detect(&c);
        assert_eq!(
            out,
            json!([
                {
                    "kind": "contract-rejected-no-redraft",
                    "severity": "human_blocking",
                    "message": "CARD-A: contract CT-A1 rejected (timeout), no successor — scheduler is blocked (card.status=doing, intent=stopped)",
                    "card_slug": "CARD-A",
                    "contract_slug": "CT-A1",
                    "detail": { "card_status": "doing", "card_intent": "stopped", "failure_kind": "timeout" },
                    "suggested_action": "set driver_intent=play; unblock card.status if blocked",
                },
                {
                    "kind": "contract-rejected-no-redraft",
                    "severity": "human_blocking",
                    "message": "CARD-B: contract CT-B1 rejected (no failure_kind), no successor — scheduler is blocked (card.status=blocked, intent=play)",
                    "card_slug": "CARD-B",
                    "contract_slug": "CT-B1",
                    "detail": { "card_status": "blocked", "card_intent": "play", "failure_kind": Value::Null },
                    "suggested_action": "set driver_intent=play; unblock card.status if blocked",
                },
                {
                    "kind": "contract-rejected-no-redraft",
                    "severity": "human_blocking",
                    "message": "CARD-E: contract CT-E1 rejected (oom), no successor — scheduler is blocked (card.status=doing, intent=null)",
                    "card_slug": "CARD-E",
                    "contract_slug": "CT-E1",
                    "detail": { "card_status": "doing", "card_intent": Value::Null, "failure_kind": "oom" },
                    "suggested_action": "set driver_intent=play; unblock card.status if blocked",
                },
            ])
        );
    }

    #[test]
    fn fix_guard_reasons() {
        let c = db();
        assert_eq!(fix(&json!({}), &c), json!({ "applied": false, "reason": "missing card_slug" }));
        assert_eq!(fix(&json!({ "card_slug": "" }), &c), json!({ "applied": false, "reason": "missing card_slug" }));
        assert_eq!(fix(&json!({ "card_slug": null }), &c), json!({ "applied": false, "reason": "missing card_slug" }));
    }

    #[test]
    fn fix_sets_play_without_second_call_when_finding_was_not_blocked() {
        let c = db();
        insert_card(&c, "CARD-1", "doing", Some("stopped"));
        let r = fix(&json!({ "card_slug": "CARD-1", "detail": { "card_status": "doing" } }), &c);
        assert_eq!(r, json!({ "applied": true, "changes": { "table": "cards", "intent_changed": true, "demoted_others": 0 } }));
        let (status, intent): (String, String) = c.query_row("SELECT status, driver_intent FROM cards WHERE slug='CARD-1'", [], |row| Ok((row.get(0)?, row.get(1)?))).unwrap();
        assert_eq!(status, "doing"); // untouched — only one with-audit call fires
        assert_eq!(intent, "play");
        let n: i64 = c.query_row("SELECT COUNT(*) FROM sacred_timeline", [], |row| row.get(0)).unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn fix_blocked_card_gets_both_intent_play_and_status_backlog_and_demotes_others() {
        let c = db();
        insert_card(&c, "CARD-2", "blocked", Some("stopped"));
        insert_card(&c, "CARD-2B", "doing", Some("play")); // will be demoted
        let r = fix(&json!({ "card_slug": "CARD-2", "detail": { "card_status": "blocked" } }), &c);
        assert_eq!(r, json!({ "applied": true, "changes": { "table": "cards", "intent_changed": true, "demoted_others": 1 } }));

        let (status, intent): (String, String) = c.query_row("SELECT status, driver_intent FROM cards WHERE slug='CARD-2'", [], |row| Ok((row.get(0)?, row.get(1)?))).unwrap();
        assert_eq!(status, "backlog"); // second with-audit call unblocked it
        assert_eq!(intent, "play");
        let other_intent: String = c.query_row("SELECT driver_intent FROM cards WHERE slug='CARD-2B'", [], |row| row.get(0)).unwrap();
        assert_eq!(other_intent, "paused");

        let n: i64 = c.query_row("SELECT COUNT(*) FROM sacred_timeline", [], |row| row.get(0)).unwrap();
        assert_eq!(n, 2); // both mutations sealed
        let ops: Vec<String> = {
            let mut stmt = c.prepare("SELECT operation FROM sacred_timeline ORDER BY id").unwrap();
            stmt.query_map([], |row| row.get(0)).unwrap().map(|r| r.unwrap()).collect()
        };
        assert_eq!(ops, vec!["card.driver_intent.set", "card.status.change"]);
    }

    #[test]
    fn fix_skips_second_call_when_card_is_no_longer_blocked() {
        let c = db();
        insert_card(&c, "CARD-3", "doing", Some("stopped")); // already recovered since detection
        let r = fix(&json!({ "card_slug": "CARD-3", "detail": { "card_status": "blocked" } }), &c);
        assert_eq!(r, json!({ "applied": true, "changes": { "table": "cards", "intent_changed": true, "demoted_others": 0 } }));
        let status: String = c.query_row("SELECT status FROM cards WHERE slug='CARD-3'", [], |row| row.get(0)).unwrap();
        assert_eq!(status, "doing"); // never touched — stale-detection re-check refused the flip
        let n: i64 = c.query_row("SELECT COUNT(*) FROM sacred_timeline", [], |row| row.get(0)).unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn fix_unchanged_when_already_play_still_demotes_others_but_skips_second_call() {
        let c = db();
        insert_card(&c, "CARD-4", "blocked", Some("play")); // already play → changed=false
        insert_card(&c, "CARD-4B", "doing", Some("play")); // still demoted regardless of `changed`
        let r = fix(&json!({ "card_slug": "CARD-4", "detail": { "card_status": "blocked" } }), &c);
        assert_eq!(r, json!({ "applied": false, "changes": { "table": "cards", "intent_changed": false, "demoted_others": 1 } }));
        let status: String = c.query_row("SELECT status FROM cards WHERE slug='CARD-4'", [], |row| row.get(0)).unwrap();
        assert_eq!(status, "blocked"); // second call never fires — changed was false
        let other_intent: String = c.query_row("SELECT driver_intent FROM cards WHERE slug='CARD-4B'", [], |row| row.get(0)).unwrap();
        assert_eq!(other_intent, "paused");
        let n: i64 = c.query_row("SELECT COUNT(*) FROM sacred_timeline", [], |row| row.get(0)).unwrap();
        assert_eq!(n, 1);
    }
}
