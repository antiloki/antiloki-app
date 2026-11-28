//! driver-build-tests.rs — twin of skills/driver/driver-build-tests.mjs (db lane). NAMES ARE
//! CONTRACT: `driverBuildTests` (named default). Two defensive read-only queries.
// QUALITY: 87/100 — two SELECTs + limit default; table-absent → []
use rusqlite::Connection;
use serde_json::{json, Value};

pub fn driverBuildTests(input: &Value, db: &Connection) -> Result<Value, String> {
    let slug = match input.get("contract_slug").and_then(|v| v.as_str()).filter(|s| !s.is_empty()) {
        Some(s) => s,
        None => return Err("driver-build-tests: contract_slug is required".into()),
    };
    // JS: Number.isInteger(limit) && limit > 0 ? limit : 10
    let limit = input.get("limit").and_then(|v| v.as_i64()).filter(|&n| n > 0).unwrap_or(10);
    let build = crate::db_json::query_rows(db,
        "SELECT id, passed, skipped, build_cmd, evidence_md, applied_at, ts FROM build_gate_runs WHERE contract_slug = ? ORDER BY id DESC LIMIT ?",
        &[json!(slug), json!(limit)]).unwrap_or_else(|_| json!([]));
    let qa = crate::db_json::query_rows(db,
        "SELECT id, ts, verifier, scope_passed, acceptance_pass, acceptance_fail, all_passed, notes_md, feedback_md, escalation_reason FROM qa_runs WHERE contract_slug = ? ORDER BY id DESC LIMIT ?",
        &[json!(slug), json!(limit)]).unwrap_or_else(|_| json!([]));
    Ok(json!({ "contract_slug": slug, "build_gate_runs": build, "qa_runs": qa }))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn two_gates_and_required() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE build_gate_runs (id INTEGER, contract_slug TEXT, passed INTEGER, skipped INTEGER, build_cmd TEXT, evidence_md TEXT, applied_at TEXT, ts TEXT);
            CREATE TABLE qa_runs (id INTEGER, contract_slug TEXT, ts TEXT, verifier TEXT, scope_passed INTEGER, acceptance_pass INTEGER, acceptance_fail INTEGER, all_passed INTEGER, notes_md TEXT, feedback_md TEXT, escalation_reason TEXT);
            INSERT INTO build_gate_runs (id,contract_slug,passed) VALUES (1,'K',1);").unwrap();
        let r = driverBuildTests(&json!({ "contract_slug": "K" }), &c).unwrap();
        assert_eq!(r["build_gate_runs"].as_array().unwrap().len(), 1);
        assert_eq!(r["qa_runs"].as_array().unwrap().len(), 0);
        assert!(driverBuildTests(&json!({}), &c).is_err());
    }
}
