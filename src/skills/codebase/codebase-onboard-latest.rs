//! codebase-onboard-latest.rs — twin of skills/codebase/codebase-onboard-latest.mjs (db lane).
//! NAMES ARE CONTRACT: `codebaseOnboardLatest` (named default). Latest row or {job_id:null}.
// QUALITY: 86/100 — single-row defensive read + detail JSON-column parse
use rusqlite::Connection;
use serde_json::{json, Value};

pub fn codebaseOnboardLatest(_input: &Value, db: &Connection) -> Result<Value, String> {
    let rows = crate::db_json::query_rows(db,
        "SELECT job_id, root, phase, status, detail, created_at, updated_at FROM code_onboard_jobs ORDER BY created_at DESC, rowid DESC LIMIT 1",
        &[]).unwrap_or_else(|_| json!([]));
    let arr = rows.as_array().unwrap();
    if arr.is_empty() {
        return Ok(json!({ "job_id": null }));
    }
    let o = arr[0].as_object().unwrap();
    // JS: if (row.detail) { try detail = JSON.parse(row.detail) catch null }  (else null)
    let detail = o.get("detail").and_then(|v| v.as_str()).filter(|s| !s.is_empty())
        .and_then(|s| serde_json::from_str::<Value>(s).ok())
        .unwrap_or(Value::Null);
    Ok(json!({
        "job_id": o["job_id"].clone(), "root": o["root"].clone(), "phase": o["phase"].clone(),
        "status": o["status"].clone(), "detail": detail,
        "created_at": o["created_at"].clone(), "updated_at": o["updated_at"].clone(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn latest_or_null() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE code_onboard_jobs (job_id TEXT, root TEXT, phase TEXT, status TEXT, detail TEXT, created_at TEXT, updated_at TEXT)").unwrap();
        assert_eq!(codebaseOnboardLatest(&Value::Null, &c).unwrap(), json!({ "job_id": null }));
        c.execute_batch("INSERT INTO code_onboard_jobs VALUES ('j1','/r','scan','running','{\"pct\":50}','t1','t2')").unwrap();
        let r = codebaseOnboardLatest(&Value::Null, &c).unwrap();
        assert_eq!(r["job_id"], json!("j1"));
        assert_eq!(r["detail"], json!({ "pct": 50 }));
    }
}
