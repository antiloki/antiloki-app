//! stuck-contracts.rs — twin of skills/reconcilers/stuck-contracts.mjs (reconciler-object lane).
//! NAMES ARE CONTRACT: the units are the default object's METHODS `detect(db)` and `fix(finding,
//! db)`. Contracts stuck 5+ minutes on proposed/impl/tl_approved surfaced as Fixit findings;
//! safety is manual — fix() only tells the operator to pick option A/B/C/D.
//!
//! datetime-injection lane: the JS SQL's `julianday('now')` is written here as the frozen literal
//! `julianday('2026-01-01 00:00:00')` (both occurrences) — the conform-db harness's JS-side db
//! proxy rewrites 'now' to the same instant, so both lanes compute the identical stuck-duration.

use rusqlite::Connection;
use serde_json::{json, Map, Value};

fn str_qq(v: Option<&Value>) -> String { match v { None|Some(Value::Null)=>String::new(), Some(Value::String(s))=>s.clone(), Some(Value::Bool(b))=>b.to_string(), Some(n@Value::Number(_))=>n.to_string(), Some(o)=>o.to_string() } } // = JS String(v ?? '')
fn tmpl(v: Option<&Value>) -> String { match v { None=>"undefined".into(), Some(Value::Null)=>"null".into(), Some(x)=>str_qq(Some(x)) } } // = JS `${v}`

// The JS findings carry fix_options whose `fix` values are FUNCTIONS. The conform JS lane's
// normalizeResult stringifies a function via String(v) — i.e. Function.prototype.toString(),
// the exact source slice from the .mjs file. antiloki-core sets eol=lf in .gitattributes, so that
// slice uses \n. These literals were transcribed while the .mjs files were still CRLF on disk and
// silently diverged once the repo normalized — conform-db caught it. They must stay byte-identical
// to the JS source (12-space body indent, 10-space closing brace, \n endings).
const FIX_A_SRC: &str = "async fix(finding, db) {\n            const { default: setIntent } = await import('../agents/god/god-card-driver-intent-set.mjs');\n            const res = setIntent({ slug: finding.card_slug, intent: 'paused' }, { db });\n            return { applied: res.changed, changes: { intent: 'paused', card_slug: finding.card_slug } };\n          }";
const FIX_B_SRC: &str = "async fix(finding, db) {\n            const { default: restart } = await import('../entities/card-restart-from-failure.mjs');\n            const res = await restart({ slug: finding.card_slug }, { db });\n            return { applied: !!res.changed, changes: res };\n          }";
const FIX_C_SRC: &str = "async fix(finding, db) {\n            const r = db.prepare(`UPDATE contracts SET updated_at=datetime('now') WHERE slug=?`).run(finding.contract_slug);\n            return { applied: r.changes > 0, changes: { contract_slug: finding.contract_slug, bumped: r.changes > 0 } };\n          }";

/// JS: `detect(db)` — contracts in status proposed/impl/tl_approved, not superseded, stuck >=5min
/// (frozen-now minus updated_at), newest-stuck-first, max 20. The try/catch around the query maps
/// ANY prepare error (contracts table absent) to [] exactly like the JS `catch { /* table absent */ }`
/// leaving rows = [].
pub fn detect(db: &Connection) -> Value {
    let rows = crate::db_json::query_rows(
        db,
        &format!("SELECT ct.slug AS contract_slug, ct.card_slug, ct.status,
               CAST(({} - julianday(ct.updated_at)) * 24 * 60 AS INTEGER) AS minutes_stuck
         FROM contracts ct
         WHERE ct.status IN ('proposed', 'impl', 'tl_approved')
           AND COALESCE(ct.superseded_by, '') = ''
           AND ({} - julianday(ct.updated_at)) * 24 * 60 >= 5
         ORDER BY minutes_stuck DESC
         LIMIT 20", crate::clock::julianday_now_sql(), crate::clock::julianday_now_sql()),
        &[],
    )
    .ok()
    .and_then(|v| v.as_array().cloned())
    .unwrap_or_default();

    let findings: Vec<Value> = rows
        .iter()
        .map(|r| {
            let mut detail = Map::new();
            detail.insert("status".into(), r.get("status").cloned().unwrap_or(Value::Null));
            detail.insert("minutes_stuck".into(), r.get("minutes_stuck").cloned().unwrap_or(Value::Null));

            let mut f = Map::new();
            f.insert("kind".into(), json!("stuck-contract"));
            f.insert("severity".into(), json!("human_blocking"));
            f.insert("message".into(), json!(format!(
                "{}: contract {} stuck at status='{}' for {}min",
                tmpl(r.get("card_slug")), tmpl(r.get("contract_slug")), tmpl(r.get("status")), tmpl(r.get("minutes_stuck")),
            )));
            f.insert("card_slug".into(), r.get("card_slug").cloned().unwrap_or(Value::Null));
            f.insert("contract_slug".into(), r.get("contract_slug").cloned().unwrap_or(Value::Null));
            f.insert("detail".into(), Value::Object(detail));
            f.insert("suggested_action".into(), json!("pick A / B / C below, or D to type custom guidance"));
            f.insert("confidence".into(), json!(0.4));
            f.insert("fix_options".into(), json!([
                { "key": "A", "label": "Pause the card (operator inspects later)", "fix": FIX_A_SRC },
                { "key": "B", "label": "Restart the card (reject contract + auto-redraft)", "fix": FIX_B_SRC },
                { "key": "C", "label": "Nudge the contract (bump updated_at; clear failure streak)", "fix": FIX_C_SRC },
            ]));
            Value::Object(f)
        })
        .collect();
    json!(findings)
}

/// JS: `async fix() { ... }` — manual reconciler: ignores its (finding, db) args and returns the
/// static "pick A/B/C/D" refusal. Never touches the db.
pub fn fix(_finding: &Value, _db: &Connection) -> Value {
    json!({ "applied": false, "reason": "manual — operator must pick option A/B/C/D" })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE contracts (slug TEXT, card_slug TEXT, status TEXT, superseded_by TEXT, updated_at TEXT)").unwrap();
        c
    }

    #[test]
    fn absent_table_and_empty_table_yield_empty_array() {
        let empty = Connection::open_in_memory().unwrap();
        assert_eq!(detect(&empty), json!([])); // table absent → JS catch → []
        assert_eq!(detect(&db()), json!([]));
    }

    #[test]
    fn filters_status_superseded_and_threshold_then_sorts_stuck_desc() {
        let c = db();
        c.execute_batch("\
            INSERT INTO contracts (slug, card_slug, status, superseded_by, updated_at) VALUES ('CT-OLD', 'CARD-OLD', 'proposed', '', '2025-12-20 00:00:00'); \
            INSERT INTO contracts (slug, card_slug, status, superseded_by, updated_at) VALUES ('CT-NULLSUP', 'CARD-NULLSUP', 'impl', NULL, '2025-12-25 00:00:00'); \
            INSERT INTO contracts (slug, card_slug, status, superseded_by, updated_at) VALUES ('CT-MID', 'CARD-MID', 'tl_approved', '', '2025-12-31 23:50:00'); \
            INSERT INTO contracts (slug, card_slug, status, superseded_by, updated_at) VALUES ('CT-EDGE-IN', 'CARD-EDGE-IN', 'proposed', '', '2025-12-31 23:54:00'); \
            INSERT INTO contracts (slug, card_slug, status, superseded_by, updated_at) VALUES ('CT-EDGE-OUT', 'CARD-EDGE-OUT', 'impl', '', '2025-12-31 23:56:00'); \
            INSERT INTO contracts (slug, card_slug, status, superseded_by, updated_at) VALUES ('CT-DONE', 'CARD-DONE', 'done', '', '2025-12-20 00:00:00'); \
            INSERT INTO contracts (slug, card_slug, status, superseded_by, updated_at) VALUES ('CT-SUPER', 'CARD-SUPER', 'proposed', 'CT-NEWER', '2025-12-20 00:00:00');\
        ").unwrap();
        let out = detect(&c);
        let arr = out.as_array().unwrap();
        // status not in set (CT-DONE), superseded (CT-SUPER), and below-threshold (CT-EDGE-OUT) excluded
        assert_eq!(arr.len(), 4);
        assert_eq!(arr[0]["contract_slug"], json!("CT-OLD"));
        assert_eq!(arr[0]["detail"]["minutes_stuck"], json!(17280));
        assert_eq!(arr[1]["contract_slug"], json!("CT-NULLSUP"));
        assert_eq!(arr[1]["detail"]["minutes_stuck"], json!(10080));
        assert_eq!(arr[2]["contract_slug"], json!("CT-MID"));
        assert_eq!(arr[2]["detail"]["minutes_stuck"], json!(10));
        assert_eq!(arr[3]["contract_slug"], json!("CT-EDGE-IN"));
        assert_eq!(arr[3]["detail"]["minutes_stuck"], json!(6));

        assert_eq!(arr[0]["kind"], json!("stuck-contract"));
        assert_eq!(arr[0]["severity"], json!("human_blocking"));
        assert_eq!(arr[0]["message"], json!("CARD-OLD: contract CT-OLD stuck at status='proposed' for 17280min"));
        assert_eq!(arr[0]["card_slug"], json!("CARD-OLD"));
        assert_eq!(arr[0]["detail"], json!({ "status": "proposed", "minutes_stuck": 17280 }));
        assert_eq!(arr[0]["suggested_action"], json!("pick A / B / C below, or D to type custom guidance"));
        assert_eq!(arr[0]["confidence"], json!(0.4));
        assert_eq!(arr[0]["fix_options"][0]["key"], json!("A"));
        assert_eq!(arr[0]["fix_options"][1]["key"], json!("B"));
        assert_eq!(arr[0]["fix_options"][2]["key"], json!("C"));
        // the stringified JS method sources ride along byte-exactly (LF, matching the .mjs on disk)
        assert!(arr[0]["fix_options"][0]["fix"].as_str().unwrap().starts_with("async fix(finding, db) {\n"));
        assert!(arr[0]["fix_options"][0]["fix"].as_str().unwrap().contains("god-card-driver-intent-set.mjs"));
        assert!(arr[0]["fix_options"][1]["fix"].as_str().unwrap().contains("card-restart-from-failure.mjs"));
        assert!(arr[0]["fix_options"][2]["fix"].as_str().unwrap().contains("SET updated_at=datetime('now')"));
    }

    #[test]
    fn fix_is_manual_refusal_and_touches_nothing() {
        let c = db();
        c.execute("INSERT INTO contracts (slug, card_slug, status, superseded_by, updated_at) VALUES ('CT-1', 'CARD-A', 'proposed', '', '2025-12-20 00:00:00')", []).unwrap();
        let r = fix(&json!({ "card_slug": "CARD-A", "contract_slug": "CT-1", "detail": { "status": "proposed", "minutes_stuck": 10 } }), &c);
        assert_eq!(r, json!({ "applied": false, "reason": "manual — operator must pick option A/B/C/D" }));
        let updated_at: String = c.query_row("SELECT updated_at FROM contracts WHERE slug = 'CT-1'", [], |row| row.get(0)).unwrap();
        assert_eq!(updated_at, "2025-12-20 00:00:00");
    }
}
