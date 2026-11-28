//! card-phase.rs — twin of skills/system/card-phase.mjs (db lane). NAMES ARE CONTRACT:
//! `cardPhase` (default) + `classifyOperation` (named, no-db helper). Derive a card's phase from ops.
// QUALITY: 80/100 — ordered RegExp-literal → phase table (regex crate), newest-classified-wins walk
use rusqlite::Connection;
use serde_json::{json, Value};
use std::sync::OnceLock;
use regex::Regex;

// OP_TO_PHASE: order matters — earlier patterns win. Mirrors the JS RegExp literals exactly
// (the /i flag becomes an inline (?i)).
fn op_to_phase() -> &'static Vec<(Regex, &'static str)> {
    static CELL: OnceLock<Vec<(Regex, &'static str)>> = OnceLock::new();
    CELL.get_or_init(|| {
        let specs: [(&str, &str); 23] = [
            ("(?i)seal|close", "seal"),
            (r"^keeper\.journey\.append", "keeper"),
            (r"^keeper\.card\.consolidate", "keeper"),
            (r"^man\.card\.review", "man_review"),
            (r"^qa\.gate\.contract\.verify", "qa_pass"),
            (r"^qa\.run", "qa_pass"),
            (r"^qa\.fail", "qa_fail"),
            (r"^dev\.implement\.code", "impl_start"),
            (r"^dev\..*done|impl_done", "impl_done"),
            (r"^dev\.", "dev"),
            (r"^tl\.sandbox", "sandbox"),
            (r"^man\.audit\.contract\.signed", "man_audit"),
            (r"^tl\.contract\.amend", "tl_amend"),
            (r"^tl\.contract\.redraft", "tl_draft"),
            (r"^tl\.refine\.microtasks", "tl_refine"),
            (r"^contract\.draft\.auto", "tl_draft"),
            (r"^contract\.gate\.tl\.sign", "tl_sign"),
            (r"^card\.gate\.pl\.ratify", "pl_ratify"),
            (r"^pl\.review\.contract", "pl_review"),
            (r"(?i)^pl\.session\..*approve", "approve"),
            (r"^pl\.session\.", "pl_session"),
            (r"(?i)^contract\.gate\.human", "approve"),
            (r"^card\.add", "propose"),
        ];
        specs.iter().map(|(p, ph)| (Regex::new(p).unwrap(), *ph)).collect()
    })
}

pub fn classifyOperation(op: &Value) -> Value {
    let s = op.as_str().unwrap_or("");
    for (re, ph) in op_to_phase() {
        if re.is_match(s) { return json!(ph); }
    }
    Value::Null
}

pub fn cardPhase(input: &Value, db: &Connection) -> Result<Value, String> {
    let slug = match input.get("slug").filter(|v| v.as_str().map(|s| !s.is_empty()).unwrap_or(false)) {
        Some(v) => v.clone(), None => return Err("card-phase: slug is required".into()),
    };
    let rows = crate::db_json::query_rows(db,
        "SELECT operation, ts, actor FROM sacred_timeline WHERE entity_type = 'card' AND entity_id = ? ORDER BY id DESC LIMIT 100",
        &[slug.clone()])?;
    let rows = rows.as_array().cloned().unwrap_or_default();
    for r in &rows {
        let phase = classifyOperation(r.get("operation").unwrap_or(&Value::Null));
        if !phase.is_null() {
            return Ok(json!({
                "slug": slug, "current_phase": phase,
                "last_event_ts": r.get("ts").cloned().unwrap_or(Value::Null),
                "last_actor": r.get("actor").cloned().unwrap_or(Value::Null),
                "last_operation": r.get("operation").cloned().unwrap_or(Value::Null),
            }));
        }
    }
    let first = rows.first();
    Ok(json!({
        "slug": slug, "current_phase": Value::Null,
        "last_event_ts": first.and_then(|r| r.get("ts")).cloned().unwrap_or(Value::Null),
        "last_actor": first.and_then(|r| r.get("actor")).cloned().unwrap_or(Value::Null),
        "last_operation": first.and_then(|r| r.get("operation")).cloned().unwrap_or(Value::Null),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn classifies_and_walks() {
        assert_eq!(classifyOperation(&json!("card.add")), json!("propose"));
        assert_eq!(classifyOperation(&json!("qa.gate.contract.verify")), json!("qa_pass"));
        assert_eq!(classifyOperation(&json!("contract.seal.final")), json!("seal")); // /seal|close/i anywhere
        assert_eq!(classifyOperation(&json!("random.op")), Value::Null);
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE sacred_timeline (id INTEGER PRIMARY KEY, entity_type TEXT, entity_id TEXT, operation TEXT, ts TEXT, actor TEXT); \
          INSERT INTO sacred_timeline VALUES (1,'card','C','card.add','t1','human'),(2,'card','C','card.status.change','t2','sys');").unwrap();
        let r = cardPhase(&json!({ "slug": "C" }), &c).unwrap();
        assert_eq!(r["current_phase"], json!("propose")); // status.change unclassified → falls to card.add
        assert!(cardPhase(&json!({}), &c).is_err());
    }
}
