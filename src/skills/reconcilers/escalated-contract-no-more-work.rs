//! escalated-contract-no-more-work.rs — twin of skills/reconcilers/escalated-contract-no-more-work.mjs
//! (reconciler-object lane). NAMES ARE CONTRACT: the units are the default object's METHODS
//! `detect(db)` and `fix(finding, db)`. Contracts stuck in impl with failed QA + no remaining
//! microtasks surfaced as Fixit findings; safety is manual — fix() only tells the operator to
//! pick option A (restart) / B (admin-reject) / C (pause) / D (custom guidance).

use rusqlite::Connection;
use serde_json::{json, Map, Value};

fn tmpl(v: Option<&Value>) -> String { match v { None=>"undefined".into(), Some(Value::Null)=>"null".into(), Some(Value::String(s))=>s.clone(), Some(Value::Bool(b))=>b.to_string(), Some(n@Value::Number(_))=>n.to_string(), Some(o)=>o.to_string() } } // = JS `${v}`

// The JS findings carry fix_options whose `fix` values are FUNCTIONS. The conform JS lane's
// normalizeResult stringifies a function via String(v) — i.e. Function.prototype.toString(),
// the exact source slice from the .mjs file. antiloki-core sets eol=lf in .gitattributes, so that
// slice uses \n. These literals were transcribed while the .mjs files were still CRLF on disk and
// silently diverged once the repo normalized — conform-db caught it. They must stay byte-identical
// to the JS source (12-space body indent, 10-space closing brace, \n endings).
const FIX_A_SRC: &str = "async fix(finding, db2) {\n            const { default: restart } = await import('../entities/card-restart-from-failure.mjs');\n            const res = await restart({ slug: finding.card_slug }, { db: db2 });\n            return { applied: !!res.changed, changes: res };\n          }";
const FIX_B_SRC: &str = "async fix(finding, db2) {\n            const { default: adminReject } = await import('../contract/contract-admin-reject.mjs');\n            try {\n              const res = adminReject({\n                slug: finding.contract_slug,\n                reason: 'Operator admin-reject via Fixit (escalated-contract-no-more-work option B). DEV finished all microtasks, QA failed, no fresh suggestions; operator chose not to redraft.',\n                failure_kind: 'operator_admin_reject_via_fixit',\n              }, { db: db2 });\n              return { applied: true, changes: res };\n            } catch (e) {\n              return { applied: false, reason: e?.message?.slice(0, 200) ?? String(e) };\n            }\n          }";
const FIX_C_SRC: &str = "async fix(finding, db2) {\n            const { default: setIntent } = await import('../agents/god/god-card-driver-intent-set.mjs');\n            const res = setIntent({ slug: finding.card_slug, intent: 'paused' }, { db: db2 });\n            return { applied: res.changed, changes: res };\n          }";

/// JS: `detect(db)` — contracts status='impl', not superseded, whose card is not done/archived,
/// whose LATEST qa_runs row (by id) failed (all_passed = 0), and whose dev_microtask_progress has
/// reached (or passed) the microtasks_json array length (DEV finished, no fresh suggestions). The
/// try/catch around the query maps ANY prepare error (contracts/cards/qa_runs absent) to [] exactly
/// like the JS `catch { /* tables absent */ }` leaving rows = []. No ORDER BY on the outer query —
/// transcribed verbatim from the JS source.
pub fn detect(db: &Connection) -> Value {
    let rows = crate::db_json::query_rows(
        db,
        "SELECT ct.slug AS contract_slug, ct.card_slug,
               c.status AS card_status, c.driver_intent
        FROM contracts ct
        JOIN cards c ON c.slug = ct.card_slug
        WHERE ct.status = 'impl'
          AND COALESCE(ct.superseded_by, '') = ''
          AND c.status NOT IN ('done', 'archived')
          AND (
            SELECT all_passed FROM qa_runs WHERE contract_slug = ct.slug ORDER BY id DESC LIMIT 1
          ) = 0
          AND ct.microtasks_json IS NOT NULL
          AND COALESCE(ct.dev_microtask_progress, 0) >= json_array_length(ct.microtasks_json)",
        &[],
    )
    .ok()
    .and_then(|v| v.as_array().cloned())
    .unwrap_or_default();

    let findings: Vec<Value> = rows
        .iter()
        .map(|r| {
            let mut detail = Map::new();
            detail.insert("contract_slug".into(), r.get("contract_slug").cloned().unwrap_or(Value::Null));
            detail.insert("card_status".into(), r.get("card_status").cloned().unwrap_or(Value::Null));
            detail.insert("intent".into(), r.get("driver_intent").cloned().unwrap_or(Value::Null));

            let mut f = Map::new();
            f.insert("kind".into(), json!("escalated-contract-no-more-work"));
            f.insert("severity".into(), json!("human_blocking"));
            f.insert("message".into(), json!(format!(
                "{}: contract {} escalated — DEV done, QA failed, no fresh suggestions; pipeline stuck",
                tmpl(r.get("card_slug")), tmpl(r.get("contract_slug")),
            )));
            f.insert("card_slug".into(), r.get("card_slug").cloned().unwrap_or(Value::Null));
            f.insert("contract_slug".into(), r.get("contract_slug").cloned().unwrap_or(Value::Null));
            f.insert("detail".into(), Value::Object(detail));
            f.insert("suggested_action".into(), json!("pick A (restart), B (admin-reject), C (pause), or D (custom guidance)"));
            f.insert("confidence".into(), json!(0.3));
            f.insert("fix_options".into(), json!([
                { "key": "A", "label": "Restart: reject contract + auto-redraft v(n+1) (most common — TL gets a fresh try)", "fix": FIX_A_SRC },
                { "key": "B", "label": "Admin-reject the contract without redraft (terminal failure; investigate card)", "fix": FIX_B_SRC },
                { "key": "C", "label": "Pause the card (revisit later; preserves all state)", "fix": FIX_C_SRC },
            ]));
            Value::Object(f)
        })
        .collect();
    json!(findings)
}

/// JS: `async fix() { ... }` — manual reconciler: ignores its (finding, db) args and returns the
/// static "pick A/B/C or D" refusal. Never touches the db.
pub fn fix(_finding: &Value, _db: &Connection) -> Value {
    json!({ "applied": false, "reason": "manual — pick A/B/C or D" })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE contracts (slug TEXT, card_slug TEXT, status TEXT, superseded_by TEXT, microtasks_json TEXT, dev_microtask_progress INTEGER);
             CREATE TABLE cards (slug TEXT, status TEXT, driver_intent TEXT);
             CREATE TABLE qa_runs (id INTEGER PRIMARY KEY AUTOINCREMENT, contract_slug TEXT, all_passed INTEGER);"
        ).unwrap();
        c
    }

    #[test]
    fn absent_tables_and_empty_tables_yield_empty_array() {
        let empty = Connection::open_in_memory().unwrap();
        assert_eq!(detect(&empty), json!([])); // tables absent → JS catch → []
        assert_eq!(detect(&db()), json!([]));
    }

    #[test]
    fn filters_every_branch_and_maps_full_finding_shape() {
        let c = db();
        c.execute_batch(
            "INSERT INTO cards (slug, status, driver_intent) VALUES ('CARD-A', 'impl', 'auto');
             INSERT INTO cards (slug, status, driver_intent) VALUES ('CARD-B', 'impl', 'play');
             INSERT INTO cards (slug, status, driver_intent) VALUES ('CARD-C', 'impl', 'auto');
             INSERT INTO cards (slug, status, driver_intent) VALUES ('CARD-D', 'impl', 'auto');
             INSERT INTO cards (slug, status, driver_intent) VALUES ('CARD-E', 'done', 'auto');
             INSERT INTO cards (slug, status, driver_intent) VALUES ('CARD-F', 'impl', 'auto');
             INSERT INTO cards (slug, status, driver_intent) VALUES ('CARD-G', 'impl', 'auto');
             INSERT INTO cards (slug, status, driver_intent) VALUES ('CARD-H', 'impl', 'auto');
             INSERT INTO cards (slug, status, driver_intent) VALUES ('CARD-I', 'archived', 'auto');
             INSERT INTO cards (slug, status, driver_intent) VALUES ('CARD-J', 'impl', 'auto');

             -- CT-1: included — impl, not superseded, card not done/archived, latest qa_run failed, DEV done
             INSERT INTO contracts (slug, card_slug, status, superseded_by, microtasks_json, dev_microtask_progress) VALUES ('CT-1', 'CARD-A', 'impl', '', '[\"a\",\"b\"]', 2);
             INSERT INTO qa_runs (contract_slug, all_passed) VALUES ('CT-1', 0);

             -- CT-2: excluded — latest qa_run (highest id) PASSED even though an earlier one failed
             INSERT INTO contracts (slug, card_slug, status, superseded_by, microtasks_json, dev_microtask_progress) VALUES ('CT-2', 'CARD-B', 'impl', '', '[\"a\"]', 1);
             INSERT INTO qa_runs (contract_slug, all_passed) VALUES ('CT-2', 0);
             INSERT INTO qa_runs (contract_slug, all_passed) VALUES ('CT-2', 1);

             -- CT-3: excluded — status not impl
             INSERT INTO contracts (slug, card_slug, status, superseded_by, microtasks_json, dev_microtask_progress) VALUES ('CT-3', 'CARD-C', 'proposed', '', '[\"a\"]', 1);
             INSERT INTO qa_runs (contract_slug, all_passed) VALUES ('CT-3', 0);

             -- CT-4: excluded — superseded_by non-empty
             INSERT INTO contracts (slug, card_slug, status, superseded_by, microtasks_json, dev_microtask_progress) VALUES ('CT-4', 'CARD-D', 'impl', 'CT-NEWER', '[\"a\"]', 1);
             INSERT INTO qa_runs (contract_slug, all_passed) VALUES ('CT-4', 0);

             -- CT-5: excluded — card status = done
             INSERT INTO contracts (slug, card_slug, status, superseded_by, microtasks_json, dev_microtask_progress) VALUES ('CT-5', 'CARD-E', 'impl', '', '[\"a\"]', 1);
             INSERT INTO qa_runs (contract_slug, all_passed) VALUES ('CT-5', 0);

             -- CT-6: excluded — microtasks_json IS NULL
             INSERT INTO contracts (slug, card_slug, status, superseded_by, microtasks_json, dev_microtask_progress) VALUES ('CT-6', 'CARD-F', 'impl', '', NULL, 0);
             INSERT INTO qa_runs (contract_slug, all_passed) VALUES ('CT-6', 0);

             -- CT-7: excluded — DEV not finished (progress < array length)
             INSERT INTO contracts (slug, card_slug, status, superseded_by, microtasks_json, dev_microtask_progress) VALUES ('CT-7', 'CARD-G', 'impl', '', '[\"a\",\"b\",\"c\"]', 1);
             INSERT INTO qa_runs (contract_slug, all_passed) VALUES ('CT-7', 0);

             -- CT-8: excluded — no qa_runs row at all (subquery NULL, NULL = 0 is not true)
             INSERT INTO contracts (slug, card_slug, status, superseded_by, microtasks_json, dev_microtask_progress) VALUES ('CT-8', 'CARD-H', 'impl', '', '[\"a\"]', 1);

             -- CT-9: excluded — card status = archived
             INSERT INTO contracts (slug, card_slug, status, superseded_by, microtasks_json, dev_microtask_progress) VALUES ('CT-9', 'CARD-I', 'impl', '', '[\"a\"]', 1);
             INSERT INTO qa_runs (contract_slug, all_passed) VALUES ('CT-9', 0);

             -- CT-10: included — superseded_by NULL (COALESCE), dev_microtask_progress NULL (COALESCE to 0), empty microtasks_json (0 >= 0)
             INSERT INTO contracts (slug, card_slug, status, superseded_by, microtasks_json, dev_microtask_progress) VALUES ('CT-10', 'CARD-J', 'impl', NULL, '[]', NULL);
             INSERT INTO qa_runs (contract_slug, all_passed) VALUES ('CT-10', 0);"
        ).unwrap();

        let out = detect(&c);
        let arr = out.as_array().unwrap();
        let slugs: Vec<&str> = arr.iter().map(|f| f["contract_slug"].as_str().unwrap()).collect();
        assert_eq!(slugs.len(), 2);
        assert!(slugs.contains(&"CT-1"));
        assert!(slugs.contains(&"CT-10"));

        let f1 = arr.iter().find(|f| f["contract_slug"] == json!("CT-1")).unwrap();
        assert_eq!(f1["kind"], json!("escalated-contract-no-more-work"));
        assert_eq!(f1["severity"], json!("human_blocking"));
        assert_eq!(f1["message"], json!("CARD-A: contract CT-1 escalated — DEV done, QA failed, no fresh suggestions; pipeline stuck"));
        assert_eq!(f1["card_slug"], json!("CARD-A"));
        assert_eq!(f1["detail"], json!({ "contract_slug": "CT-1", "card_status": "impl", "intent": "auto" }));
        assert_eq!(f1["suggested_action"], json!("pick A (restart), B (admin-reject), C (pause), or D (custom guidance)"));
        assert_eq!(f1["confidence"], json!(0.3));
        assert_eq!(f1["fix_options"][0]["key"], json!("A"));
        assert_eq!(f1["fix_options"][1]["key"], json!("B"));
        assert_eq!(f1["fix_options"][2]["key"], json!("C"));
        assert_eq!(f1["fix_options"][0]["label"], json!("Restart: reject contract + auto-redraft v(n+1) (most common — TL gets a fresh try)"));
        assert_eq!(f1["fix_options"][1]["label"], json!("Admin-reject the contract without redraft (terminal failure; investigate card)"));
        assert_eq!(f1["fix_options"][2]["label"], json!("Pause the card (revisit later; preserves all state)"));
        // the stringified JS method sources ride along byte-exactly (LF, matching the .mjs on disk)
        assert!(f1["fix_options"][0]["fix"].as_str().unwrap().starts_with("async fix(finding, db2) {\n"));
        assert!(f1["fix_options"][0]["fix"].as_str().unwrap().contains("card-restart-from-failure.mjs"));
        assert!(f1["fix_options"][1]["fix"].as_str().unwrap().contains("contract-admin-reject.mjs"));
        assert!(f1["fix_options"][2]["fix"].as_str().unwrap().contains("god-card-driver-intent-set.mjs"));

        let f10 = arr.iter().find(|f| f["contract_slug"] == json!("CT-10")).unwrap();
        assert_eq!(f10["detail"], json!({ "contract_slug": "CT-10", "card_status": "impl", "intent": "auto" }));
    }

    #[test]
    fn fix_is_manual_refusal_and_touches_nothing() {
        let c = db();
        c.execute("INSERT INTO contracts (slug, card_slug, status, superseded_by, microtasks_json, dev_microtask_progress) VALUES ('CT-1', 'CARD-A', 'impl', '', '[\"a\"]', 1)", []).unwrap();
        let r = fix(&json!({ "card_slug": "CARD-A", "contract_slug": "CT-1" }), &c);
        assert_eq!(r, json!({ "applied": false, "reason": "manual — pick A/B/C or D" }));
        let status: String = c.query_row("SELECT status FROM contracts WHERE slug = 'CT-1'", [], |row| row.get(0)).unwrap();
        assert_eq!(status, "impl");
    }
}
