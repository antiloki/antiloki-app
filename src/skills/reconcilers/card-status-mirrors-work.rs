//! card-status-mirrors-work.rs — twin of skills/reconcilers/card-status-mirrors-work.mjs
//! (reconciler-object lane: the METHODS are the units, both db-last). NAMES ARE CONTRACT:
//! `detect` (detect(db)) and `fix` (fix(finding, db)). Cards stuck in status='backlog' despite
//! work being underway — either an open contract past 'proposed' (ACTIVE_STATUSES) or an active
//! PL session with no contract yet — surface as drift findings; fix promotes via a REAL
//! with-audit-wrapped card-status-change dispatch (not a raw UPDATE), matching the JS source's
//! AUDIT-SEAL fix (2026-07-10 code-review P1).
//!
//! `fix` dispatches into the already-converted `engine_with_audit::withAuditAs` twin (the
//! actor-parameterized entry — this reconciler's JS passes `actor: 'system:reconciler'`, which
//! the fixture-only `withAudit` cannot receive), which in turn
//! dispatches to the already-converted `entities_card_status_change::cardStatusChange` twin — both
//! present in lib.rs (playbook cross-module rule). The pre-check (`cur !== 'backlog'`) reproduces
//! card-status-change's own no-op guard so this reconciler's fix() never actually reaches that
//! throw; `withAudit(...).unwrap()` mirrors the JS's un-caught-await-throw semantics (no try/catch
//! around the withAudit call in the source) — success-path only, per playbook gotcha 9.

use rusqlite::Connection;
use serde_json::{json, Value};

fn truthy(v: Option<&Value>) -> bool { match v { None|Some(Value::Null)=>false, Some(Value::Bool(b))=>*b, Some(Value::Number(n))=>n.as_f64().map_or(false,|f|f!=0.0&&!f.is_nan()), Some(Value::String(s))=>!s.is_empty(), Some(_)=>true } }
fn str_qq(v: Option<&Value>) -> String { match v { None|Some(Value::Null)=>String::new(), Some(Value::String(s))=>s.clone(), Some(Value::Bool(b))=>b.to_string(), Some(n@Value::Number(_))=>n.to_string(), Some(o)=>o.to_string() } } // = JS String(v ?? '')
fn tmpl(v: Option<&Value>) -> String { match v { None=>"undefined".into(), Some(Value::Null)=>"null".into(), Some(x)=>str_qq(Some(x)) } } // = JS `${v}`

const ACTIVE_STATUSES: [&str; 6] = ["pl_ratified", "tl_approved", "human_approved", "impl", "qa", "done"];

/// JS: `async detect(db)` — two independent queries, each wrapped in its own try/catch (table
/// absent → that branch contributes no findings, `catch { /* tables absent */ }`). Findings from
/// query 1 (contract branch) are pushed before query 2 (pl_session branch), matching source order.
pub fn detect(db: &Connection) -> Value {
    let mut findings: Vec<Value> = Vec::new();

    // 1. Backlog cards with an open (not superseded) contract past 'proposed'.
    let placeholders = ACTIVE_STATUSES.iter().map(|_| "?").collect::<Vec<_>>().join(",");
    let sql1 = format!(
        "SELECT c.slug, c.status AS card_status, ct.slug AS contract_slug, ct.status AS contract_status
         FROM cards c
         JOIN contracts ct ON ct.card_slug = c.slug
         WHERE c.status = 'backlog'
           AND COALESCE(ct.superseded_by, '') = ''
           AND ct.status IN ({placeholders})"
    );
    let params1: Vec<Value> = ACTIVE_STATUSES.iter().map(|s| json!(*s)).collect();
    if let Ok(rows) = crate::db_json::query_rows(db, &sql1, &params1) {
        for r in rows.as_array().cloned().unwrap_or_default() {
            findings.push(json!({
                "kind": "card-status-mirrors-work",
                "severity": "drift",
                "message": format!(
                    "{}: card.status=backlog but contract {} is at {} — should be in 'doing' lane",
                    tmpl(r.get("slug")), tmpl(r.get("contract_slug")), tmpl(r.get("contract_status")),
                ),
                "card_slug": r.get("slug").cloned().unwrap_or(Value::Null),
                "detail": {
                    "card_status": r.get("card_status").cloned().unwrap_or(Value::Null),
                    "contract_slug": r.get("contract_slug").cloned().unwrap_or(Value::Null),
                    "contract_status": r.get("contract_status").cloned().unwrap_or(Value::Null),
                    "reason": "contract",
                },
                "suggested_action": "promote card.status to doing",
            }));
        }
    }

    // 2. Backlog cards with an active PL session (working) and no open contract yet.
    let sql2 = "SELECT c.slug, c.status AS card_status, pls.id AS session_id, pls.status AS session_status
         FROM cards c
         JOIN pl_sessions pls ON pls.approved_card_slug = c.slug
         WHERE c.status = 'backlog'
           AND pls.status NOT IN ('done', 'abandoned')
           AND NOT EXISTS (
             SELECT 1 FROM contracts ct
             WHERE ct.card_slug = c.slug AND COALESCE(ct.superseded_by, '') = ''
           )";
    if let Ok(rows) = crate::db_json::query_rows(db, sql2, &[]) {
        for r in rows.as_array().cloned().unwrap_or_default() {
            findings.push(json!({
                "kind": "card-status-mirrors-work",
                "severity": "drift",
                "message": format!(
                    "{}: card.status=backlog but PL session #{} is at {} — should be in 'doing' lane (PL is working)",
                    tmpl(r.get("slug")), tmpl(r.get("session_id")), tmpl(r.get("session_status")),
                ),
                "card_slug": r.get("slug").cloned().unwrap_or(Value::Null),
                "detail": {
                    "card_status": r.get("card_status").cloned().unwrap_or(Value::Null),
                    "session_id": r.get("session_id").cloned().unwrap_or(Value::Null),
                    "session_status": r.get("session_status").cloned().unwrap_or(Value::Null),
                    "reason": "pl_session",
                },
                "suggested_action": "promote card.status to doing",
            }));
        }
    }

    json!(findings)
}

/// JS: `async fix(finding, db)` — precheck the card is still 'backlog' (preserves
/// card-status-change's own no-op-throw semantics as a no-op RETURN here instead), then dispatch
/// a REAL audited promotion via with-audit (skill 'card-status-change') rather than a raw UPDATE.
pub fn fix(finding: &Value, db: &Connection) -> Value {
    let slug = finding.get("card_slug");
    if !truthy(slug) {
        return json!({ "applied": false, "reason": "missing card_slug" });
    }
    let slug_v = slug.cloned().unwrap();

    let cur_status = crate::db_json::query_rows(db, "SELECT status FROM cards WHERE slug = ?", &[slug_v.clone()])
        .ok()
        .and_then(|v| v.as_array().and_then(|a| a.first().cloned()))
        .and_then(|r| r.get("status").cloned());
    if cur_status.as_ref().and_then(|v| v.as_str()) != Some("backlog") {
        return json!({ "applied": false, "reason": "card already moved out of backlog" });
    }

    let audit_input = json!({
        "skill": "card-status-change",
        "wrappedInput": { "slug": slug_v.clone(), "status": "doing" },
        "operation": "card.status.change",
        "entity_type": "card",
        "entity_id": slug_v.clone(),
    });
    crate::engine_with_audit::withAuditAs(&audit_input, db, "system:reconciler").unwrap();

    json!({ "applied": true, "changes": { "table": "cards", "rows_affected": 1 } })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE cards (slug TEXT PRIMARY KEY, status TEXT, updated_at TEXT);
             CREATE TABLE contracts (slug TEXT, card_slug TEXT, status TEXT, superseded_by TEXT);
             CREATE TABLE pl_sessions (id INTEGER PRIMARY KEY, approved_card_slug TEXT, status TEXT);
             CREATE TABLE sacred_timeline (
               id INTEGER PRIMARY KEY AUTOINCREMENT, ts TEXT, actor TEXT, operation TEXT,
               entity_type TEXT, entity_id TEXT, before_json TEXT, after_json TEXT, payload TEXT,
               prev_print TEXT, digital_print TEXT, user_goal_id TEXT, exec_env TEXT, tick_id TEXT
             );",
        )
        .unwrap();
        c
    }

    #[test]
    fn absent_tables_and_empty_tables_yield_empty_array() {
        let empty = Connection::open_in_memory().unwrap();
        assert_eq!(detect(&empty), json!([])); // tables absent → JS catch → []
        assert_eq!(detect(&db()), json!([]));
    }

    #[test]
    fn contract_branch_filters_status_superseded_and_card_status() {
        let c = db();
        c.execute_batch(
            "INSERT INTO cards (slug, status) VALUES ('CARD-A', 'backlog');
             INSERT INTO contracts (slug, card_slug, status, superseded_by) VALUES ('CT-A', 'CARD-A', 'impl', '');
             INSERT INTO cards (slug, status) VALUES ('CARD-B', 'backlog');
             INSERT INTO contracts (slug, card_slug, status, superseded_by) VALUES ('CT-B', 'CARD-B', 'proposed', '');
             INSERT INTO cards (slug, status) VALUES ('CARD-C', 'backlog');
             INSERT INTO contracts (slug, card_slug, status, superseded_by) VALUES ('CT-C', 'CARD-C', 'human_approved', 'CT-NEWER');
             INSERT INTO cards (slug, status) VALUES ('CARD-D', 'doing');
             INSERT INTO contracts (slug, card_slug, status, superseded_by) VALUES ('CT-D', 'CARD-D', 'qa', '');
             INSERT INTO cards (slug, status) VALUES ('CARD-E', 'backlog');
             INSERT INTO contracts (slug, card_slug, status, superseded_by) VALUES ('CT-E', 'CARD-E', 'done', NULL);",
        )
        .unwrap();
        let out = detect(&c);
        let arr = out.as_array().unwrap();
        let slugs: Vec<&str> = arr.iter().map(|f| f["detail"]["contract_slug"].as_str().unwrap()).collect();
        // CT-B (not active), CT-C (superseded), CT-D (card not backlog) excluded; CT-E (superseded_by NULL, 'done' active) included
        assert_eq!(slugs, vec!["CT-A", "CT-E"]);

        let f0 = &arr[0];
        assert_eq!(f0["kind"], json!("card-status-mirrors-work"));
        assert_eq!(f0["severity"], json!("drift"));
        assert_eq!(f0["message"], json!("CARD-A: card.status=backlog but contract CT-A is at impl — should be in 'doing' lane"));
        assert_eq!(f0["card_slug"], json!("CARD-A"));
        assert_eq!(f0["detail"], json!({ "card_status": "backlog", "contract_slug": "CT-A", "contract_status": "impl", "reason": "contract" }));
        assert_eq!(f0["suggested_action"], json!("promote card.status to doing"));
    }

    #[test]
    fn pl_session_branch_filters_status_and_open_contract_but_not_superseded() {
        let c = db();
        c.execute_batch(
            "INSERT INTO cards (slug, status) VALUES ('CARD-F', 'backlog');
             INSERT INTO pl_sessions (id, approved_card_slug, status) VALUES (1, 'CARD-F', 'clarifying');
             INSERT INTO cards (slug, status) VALUES ('CARD-G', 'backlog');
             INSERT INTO pl_sessions (id, approved_card_slug, status) VALUES (2, 'CARD-G', 'done');
             INSERT INTO cards (slug, status) VALUES ('CARD-H', 'backlog');
             INSERT INTO pl_sessions (id, approved_card_slug, status) VALUES (3, 'CARD-H', 'abandoned');
             INSERT INTO cards (slug, status) VALUES ('CARD-I', 'backlog');
             INSERT INTO pl_sessions (id, approved_card_slug, status) VALUES (4, 'CARD-I', 'researching');
             INSERT INTO contracts (slug, card_slug, status, superseded_by) VALUES ('CT-I', 'CARD-I', 'proposed', '');
             INSERT INTO cards (slug, status) VALUES ('CARD-J', 'backlog');
             INSERT INTO pl_sessions (id, approved_card_slug, status) VALUES (5, 'CARD-J', 'researching');
             INSERT INTO contracts (slug, card_slug, status, superseded_by) VALUES ('CT-J', 'CARD-J', 'proposed', 'CT-NEWER');",
        )
        .unwrap();
        let out = detect(&c);
        let arr = out.as_array().unwrap();
        let slugs: Vec<&str> = arr.iter().map(|f| f["card_slug"].as_str().unwrap()).collect();
        // CARD-G (done), CARD-H (abandoned), CARD-I (open non-superseded contract blocks) excluded;
        // CARD-J's contract is superseded so NOT EXISTS still passes → included.
        assert_eq!(slugs, vec!["CARD-F", "CARD-J"]);

        let f0 = &arr[0];
        assert_eq!(f0["message"], json!("CARD-F: card.status=backlog but PL session #1 is at clarifying — should be in 'doing' lane (PL is working)"));
        assert_eq!(f0["detail"], json!({ "card_status": "backlog", "session_id": 1, "session_status": "clarifying", "reason": "pl_session" }));
    }

    #[test]
    fn fix_guards_missing_slug_and_non_backlog() {
        let c = db();
        assert_eq!(fix(&json!({}), &c), json!({ "applied": false, "reason": "missing card_slug" }));
        assert_eq!(fix(&json!({ "card_slug": "" }), &c), json!({ "applied": false, "reason": "missing card_slug" }));
        assert_eq!(fix(&json!({ "card_slug": null }), &c), json!({ "applied": false, "reason": "missing card_slug" }));

        c.execute("INSERT INTO cards (slug, status) VALUES ('CARD-K', 'doing')", []).unwrap();
        assert_eq!(fix(&json!({ "card_slug": "CARD-K" }), &c), json!({ "applied": false, "reason": "card already moved out of backlog" }));
        // not found at all → same reason (cur is None, undefined !== 'backlog' in JS)
        assert_eq!(fix(&json!({ "card_slug": "GHOST" }), &c), json!({ "applied": false, "reason": "card already moved out of backlog" }));
    }

    #[test]
    fn fix_promotes_backlog_card_via_real_audited_dispatch() {
        let c = db();
        c.execute(
            "INSERT INTO cards (slug, status, updated_at) VALUES ('CARD-L', 'backlog', '2025-12-01 00:00:00')",
            [],
        )
        .unwrap();
        let r = fix(&json!({ "card_slug": "CARD-L" }), &c);
        assert_eq!(r, json!({ "applied": true, "changes": { "table": "cards", "rows_affected": 1 } }));

        let status: String = c.query_row("SELECT status FROM cards WHERE slug = 'CARD-L'", [], |row| row.get(0)).unwrap();
        assert_eq!(status, "doing");

        let tl = crate::db_json::query_rows(&c, "SELECT * FROM sacred_timeline", &[]).unwrap();
        let rows = tl.as_array().unwrap();
        assert_eq!(rows.len(), 1); // withAudit actually sealed the mutation, not a raw UPDATE
        assert_eq!(rows[0]["operation"], json!("card.status.change"));
        assert_eq!(rows[0]["entity_type"], json!("card"));
        assert_eq!(rows[0]["entity_id"], json!("CARD-L"));
    }
}
