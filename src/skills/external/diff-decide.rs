//! diff-decide.rs — twin of skills/external/diff-decide.mjs (db WRITE lane). NAMES ARE CONTRACT:
//! `diffDecide` (default). Approve/reject a pending_diffs row: SELECT the row, guard status=pending,
//! UPDATE status/decided_by/decision_reason + stamp decided_at (datetime('now') frozen 2026-01-01),
//! return { id, status, decided_by, decision_reason, card_slug, file_path }.
use rusqlite::Connection;
use serde_json::{json, Map, Value};

// JS truthiness — for the `!diff_id` / `!decision` / `!decided_by` guards.
fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}

pub fn diffDecide(input: &Value, db: &Connection) -> Result<Value, String> {
    let diff_id = input.get("diff_id");
    let decision = input.get("decision");
    let decided_by = input.get("decided_by");
    // JS destructuring default `decision_reason = null` fires on absent (undefined); explicit null is null too.
    let decision_reason = input.get("decision_reason").cloned().unwrap_or(Value::Null);

    if !truthy(diff_id) { return Err("diff-decide: diff_id is required".into()); }
    if !truthy(decision) { return Err("diff-decide: decision is required".into()); }
    if !truthy(decided_by) { return Err("diff-decide: decided_by is required".into()); }

    let decision_str = decision.and_then(|v| v.as_str());
    if decision_str != Some("approved") && decision_str != Some("rejected") {
        return Err(format!("decision must be 'approved' or 'rejected'; got '{}'", decision_str.unwrap_or("")));
    }

    let rows = crate::db_json::query_rows(db, "SELECT * FROM pending_diffs WHERE id = ?", &[diff_id.cloned().unwrap_or(Value::Null)])?;
    let rows = rows.as_array().cloned().unwrap_or_default();
    let row = match rows.first() {
        None => return Err(format!("pending_diff {} not found", diff_id.map(|v| v.to_string()).unwrap_or_default())),
        Some(r) => r,
    };
    let status = row.get("status").and_then(|v| v.as_str()).unwrap_or("");
    if status != "pending" {
        return Err(format!("pending_diff {} is in status='{}'; cannot transition to '{}'",
            diff_id.map(|v| v.to_string()).unwrap_or_default(), status, decision_str.unwrap_or("")));
    }

    // JS: UPDATE ... decided_at = datetime('now') — frozen-clock lane pins 'now' to the instant below.
    crate::db_json::run_write(
        db,
        &format!("UPDATE pending_diffs SET status = ?, decided_at = {}, decided_by = ?, decision_reason = ? WHERE id = ?", crate::clock::now_sql()),
        &[
            decision.cloned().unwrap_or(Value::Null),
            decided_by.cloned().unwrap_or(Value::Null),
            decision_reason.clone(),
            diff_id.cloned().unwrap_or(Value::Null),
        ],
    )?;

    let mut out = Map::new();
    out.insert("id".into(), diff_id.cloned().unwrap_or(Value::Null));
    out.insert("status".into(), decision.cloned().unwrap_or(Value::Null));
    out.insert("decided_by".into(), decided_by.cloned().unwrap_or(Value::Null));
    out.insert("decision_reason".into(), decision_reason);
    out.insert("card_slug".into(), row.get("card_slug").cloned().unwrap_or(Value::Null));
    out.insert("file_path".into(), row.get("file_path").cloned().unwrap_or(Value::Null));
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;
    const SCHEMA: &str = "CREATE TABLE pending_diffs ( id INTEGER PRIMARY KEY, card_slug TEXT NOT NULL, file_path TEXT NOT NULL, diff_text TEXT NOT NULL, proposed_by TEXT NOT NULL, status TEXT NOT NULL DEFAULT 'pending' CHECK (status IN ('pending','approved','rejected','applied','superseded')), proposed_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')), decided_at TEXT, decided_by TEXT, decision_reason TEXT, applied_at TEXT, applied_sha TEXT )";

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(SCHEMA).unwrap();
        c.execute(
            "INSERT INTO pending_diffs (id, card_slug, file_path, diff_text, proposed_by, status, proposed_at) VALUES (1, 'DEC-1', 'x.mjs', '--- a\n+++ b', 'dev', 'pending', '2025-12-01 00:00:00')",
            [],
        ).unwrap();
        c
    }

    #[test]
    fn approves_and_stamps() {
        let c = db();
        let r = diffDecide(&json!({ "diff_id": 1, "decision": "approved", "decided_by": "human:dani", "decision_reason": "looks good" }), &c).unwrap();
        assert_eq!(r, json!({
            "id": 1, "status": "approved", "decided_by": "human:dani",
            "decision_reason": "looks good", "card_slug": "DEC-1", "file_path": "x.mjs"
        }));
        let row = crate::db_json::query_rows(&c, "SELECT status, decided_at, decided_by, decision_reason FROM pending_diffs WHERE id = 1", &[]).unwrap();
        assert_eq!(row[0]["status"], json!("approved"));
        assert_eq!(row[0]["decided_at"], json!("2026-01-01 00:00:00"));
        assert_eq!(row[0]["decided_by"], json!("human:dani"));
    }

    #[test]
    fn rejects_with_null_reason_default() {
        let c = db();
        // no decision_reason → null
        let r = diffDecide(&json!({ "diff_id": 1, "decision": "rejected", "decided_by": "u" }), &c).unwrap();
        assert_eq!(r, json!({
            "id": 1, "status": "rejected", "decided_by": "u",
            "decision_reason": null, "card_slug": "DEC-1", "file_path": "x.mjs"
        }));
    }

    #[test]
    fn guards_and_transitions() {
        let c = db();
        // missing fields
        assert!(diffDecide(&json!({}), &c).is_err());
        assert!(diffDecide(&json!({ "diff_id": 1 }), &c).is_err());
        assert!(diffDecide(&json!({ "diff_id": 1, "decision": "approved" }), &c).is_err());
        // bad decision
        assert!(diffDecide(&json!({ "diff_id": 1, "decision": "maybe", "decided_by": "u" }), &c).is_err());
        // not found
        assert!(diffDecide(&json!({ "diff_id": 9999, "decision": "approved", "decided_by": "u" }), &c).is_err());
        // re-decide a non-pending row fails
        diffDecide(&json!({ "diff_id": 1, "decision": "approved", "decided_by": "u1" }), &c).unwrap();
        assert!(diffDecide(&json!({ "diff_id": 1, "decision": "rejected", "decided_by": "u2" }), &c).is_err());
    }
}
