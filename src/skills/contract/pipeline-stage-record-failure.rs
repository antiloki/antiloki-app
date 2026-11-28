//! pipeline-stage-record-failure.rs — twin of skills/contract/pipeline-stage-record-failure.mjs (db WRITE lane).
//! NAMES ARE CONTRACT: `pipelineStageRecordFailure` (default). Stamps a failed driver pipeline stage
//! onto its contract (last_failure_kind/md/at) so the failure lands on the audit chain, not vanishes.
// QUALITY: NOT_FOUND when contract absent · kind capped 120 · md capped 2000 · datetime('now') frozen
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
// String(v ?? '')
fn str_qq(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(),
        Some(o) => o.to_string(),
    }
}
// JS `${v}`
fn tmpl(v: Option<&Value>) -> String {
    match v {
        None => "undefined".into(),
        Some(Value::Null) => "null".into(),
        Some(x) => str_qq(Some(x)),
    }
}
fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) }
fn take_chars(s: &str, n: usize) -> String { s.chars().take(n).collect() } // JS .slice(0,n) (BMP-equivalent)

pub fn pipelineStageRecordFailure(input: &Value, db: &Connection) -> Result<Value, String> {
    let contract_slug = input.get("contract_slug").cloned().unwrap_or(Value::Null);
    if !truthy(Some(&contract_slug)) {
        return Err("pipeline-stage-record-failure: contract_slug is required".into());
    }

    let stage = nz(input.get("stage")).cloned().unwrap_or(json!("unknown"));
    let failure_kind = nz(input.get("failure_kind")).cloned().unwrap_or(json!("stage_failed"));
    let failure_md = take_chars(&str_qq(input.get("failure_md")), 2000);

    let rows = crate::db_json::query_rows(db, "SELECT slug, card_slug FROM contracts WHERE slug = ?", &[contract_slug.clone()])?;
    let contract = match rows.as_array().and_then(|a| a.first()).cloned() {
        Some(c) => c,
        None => return Err(format!("pipeline-stage-record-failure: contract '{}' not found", str_qq(Some(&contract_slug)))),
    };

    // `${stage}:${failure_kind}`.slice(0, 120) ; datetime('now') pinned to the frozen conform-db instant.
    let kind_param = take_chars(&format!("{}:{}", tmpl(Some(&stage)), tmpl(Some(&failure_kind))), 120);
    crate::db_json::run_write(
        db,
        &format!("UPDATE contracts SET last_failure_kind = ?, last_failure_md = ?, last_failure_at = {}, updated_at = {} WHERE slug = ?", crate::clock::now_sql(), crate::clock::now_sql()),
        &[json!(kind_param), json!(failure_md), contract_slug.clone()],
    )?;

    let mut out = Map::new();
    out.insert("contract_slug".into(), contract_slug);
    out.insert("card_slug".into(), contract.get("card_slug").cloned().unwrap_or(Value::Null));
    out.insert("stage".into(), stage);
    out.insert("failure_kind".into(), failure_kind);
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE contracts (slug TEXT PRIMARY KEY, card_slug TEXT, last_failure_kind TEXT, last_failure_md TEXT, last_failure_at TEXT, updated_at TEXT); \
             INSERT INTO contracts (slug, card_slug) VALUES ('C1','CARD-1'),('C4',NULL);",
        )
        .unwrap();
        c
    }

    #[test]
    fn stamps_failure() {
        let c = setup();
        let r = pipelineStageRecordFailure(
            &json!({ "contract_slug": "C1", "stage": "qa.acceptance.run", "failure_kind": "qa_fail", "failure_md": "acceptance 2/5" }),
            &c,
        )
        .unwrap();
        assert_eq!(r, json!({ "contract_slug": "C1", "card_slug": "CARD-1", "stage": "qa.acceptance.run", "failure_kind": "qa_fail" }));
        let (kind, md, at): (String, String, String) = c
            .query_row("SELECT last_failure_kind, last_failure_md, last_failure_at FROM contracts WHERE slug='C1'", [], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })
            .unwrap();
        assert_eq!(kind, "qa.acceptance.run:qa_fail");
        assert_eq!(md, "acceptance 2/5");
        assert_eq!(at, "2026-01-01 00:00:00");
    }

    #[test]
    fn defaults_and_null_card_and_not_found() {
        let c = setup();
        // defaults when stage/failure_kind/failure_md absent
        let d = pipelineStageRecordFailure(&json!({ "contract_slug": "C1" }), &c).unwrap();
        assert_eq!(d["stage"], json!("unknown"));
        assert_eq!(d["failure_kind"], json!("stage_failed"));
        // null card_slug is passed through
        let n = pipelineStageRecordFailure(&json!({ "contract_slug": "C4", "stage": "x", "failure_kind": "y" }), &c).unwrap();
        assert_eq!(n["card_slug"], Value::Null);
        // missing contract → Err
        assert!(pipelineStageRecordFailure(&json!({ "contract_slug": "NOPE" }), &c).is_err());
    }
}
