//! contract-gate-human-approve.rs — twin of skills/contract/contract-gate-human-approve.mjs (db WRITE lane).
//! NAMES ARE CONTRACT: `contractGateHumanApprove` (default). HITL gate for high-risk contracts: callable
//! only when status='tl_approved' AND tier='high-risk'; advances status to 'human_approved' and returns
//! the ratification record. Guard-throws (missing slug / not found / wrong tier|status) are NOT exercised
//! by the fixture — only the success path is judged (playbook gotcha #9).
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

pub fn contractGateHumanApprove(input: &Value, db: &Connection) -> Result<Value, String> {
    let empty = json!({});
    let input = if input.is_object() { input } else { &empty };

    let slug = input.get("slug").cloned().unwrap_or(Value::Null);
    // JS destructuring defaults fire only on `undefined` (missing key), NOT on an explicit null.
    let approved_by = match input.get("approved_by") { None => json!("human:cli"), Some(v) => v.clone() };
    let note = input.get("note").cloned().unwrap_or(Value::Null);

    if !truthy(Some(&slug)) {
        return Err("contract-gate-human-approve: slug is required".into());
    }

    let rows = crate::db_json::query_rows(
        db,
        "SELECT status, tier FROM contracts WHERE slug = ?",
        &[slug.clone()],
    )?;
    let row = match rows.as_array().and_then(|a| a.first()) {
        Some(r) => r.clone(),
        None => return Err(format!("contract-gate-human-approve: contract '{}' not found", slug.as_str().unwrap_or(""))),
    };

    let tier = row.get("tier").cloned().unwrap_or(Value::Null);
    if tier.as_str() != Some("high-risk") {
        return Err(format!(
            "contract-gate-human-approve: human approval is only for tier='high-risk' contracts; this one is tier='{}'",
            tier.as_str().unwrap_or("null")
        ));
    }
    let status = row.get("status").cloned().unwrap_or(Value::Null);
    if status.as_str() != Some("tl_approved") {
        return Err(format!(
            "contract-gate-human-approve: contract status is '{}', expected 'tl_approved' for human approval",
            status.as_str().unwrap_or("null")
        ));
    }

    // Advance status; datetime('now') pinned to the frozen conform clock.
    crate::db_json::run_write(
        db,
        &format!("UPDATE contracts SET status = 'human_approved', updated_at = {} WHERE slug = ?", crate::clock::now_sql()),
        &[slug.clone()],
    )?;

    let mut out = Map::new();
    out.insert("approved".into(), json!(true));
    out.insert("slug".into(), slug);
    out.insert("new_status".into(), json!("human_approved"));
    out.insert("approved_by".into(), approved_by);
    out.insert("approved_at".into(), json!(crate::clock::now_iso(db)));
    out.insert("note".into(), note);
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE contracts (slug TEXT PRIMARY KEY, status TEXT, tier TEXT, updated_at TEXT);",
        )
        .unwrap();
        c
    }

    #[test]
    fn happy_path_advances_and_defaults() {
        let c = db();
        c.execute(
            "INSERT INTO contracts (slug, status, tier, updated_at) VALUES ('C1','tl_approved','high-risk','2025-12-20 00:00:00')",
            [],
        )
        .unwrap();

        let r = contractGateHumanApprove(&json!({ "slug": "C1" }), &c).unwrap();
        assert_eq!(
            r,
            json!({
                "approved": true,
                "slug": "C1",
                "new_status": "human_approved",
                "approved_by": "human:cli",
                "approved_at": "2026-01-01T00:00:00.000Z",
                "note": Value::Null,
            })
        );
        // key order is part of the output
        let keys: Vec<&String> = r.as_object().unwrap().keys().collect();
        assert_eq!(keys, vec!["approved", "slug", "new_status", "approved_by", "approved_at", "note"]);

        let (st, at): (String, String) = c
            .query_row("SELECT status, updated_at FROM contracts WHERE slug='C1'", [], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap();
        assert_eq!(st, "human_approved");
        assert_eq!(at, "2026-01-01 00:00:00");
    }

    #[test]
    fn explicit_and_null_values_preserved() {
        let c = db();
        c.execute("INSERT INTO contracts (slug, status, tier) VALUES ('C2','tl_approved','high-risk')", []).unwrap();
        let r = contractGateHumanApprove(&json!({ "slug": "C2", "approved_by": "human:dani", "note": "ship it" }), &c).unwrap();
        assert_eq!(r["approved_by"], json!("human:dani"));
        assert_eq!(r["note"], json!("ship it"));

        // explicit null must NOT be replaced by the 'human:cli' default (default is undefined-only)
        c.execute("INSERT INTO contracts (slug, status, tier) VALUES ('C3','tl_approved','high-risk')", []).unwrap();
        let r2 = contractGateHumanApprove(&json!({ "slug": "C3", "approved_by": Value::Null, "note": Value::Null }), &c).unwrap();
        assert_eq!(r2["approved_by"], Value::Null);
        assert_eq!(r2["note"], Value::Null);
    }

    #[test]
    fn guards_reject() {
        let c = db();
        assert!(contractGateHumanApprove(&json!({}), &c).is_err()); // missing slug
        assert!(contractGateHumanApprove(&json!({ "slug": "NOPE" }), &c).is_err()); // not found
        c.execute("INSERT INTO contracts (slug, status, tier) VALUES ('L','tl_approved','low-risk')", []).unwrap();
        assert!(contractGateHumanApprove(&json!({ "slug": "L" }), &c).is_err()); // wrong tier
        c.execute("INSERT INTO contracts (slug, status, tier) VALUES ('P','proposed','high-risk')", []).unwrap();
        assert!(contractGateHumanApprove(&json!({ "slug": "P" }), &c).is_err()); // wrong status
    }
}
