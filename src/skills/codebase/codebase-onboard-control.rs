//! codebase-onboard-control.rs — twin of skills/codebase/codebase-onboard-control.mjs (db WRITE
//! lane). NAMES ARE CONTRACT: `codebaseOnboardControl` (default). REBUILD-WIZARD job control:
//! pause a running rebuild, mark a paused/failed one running again, or flag a crashed runner.
//! Frozen-clock lane: JS `datetime('now')` → literal 2026-01-01 00:00:00.
use rusqlite::Connection;
use serde_json::{json, Map, Value};

fn truthy(v: Option<&Value>) -> bool { match v { None|Some(Value::Null)=>false, Some(Value::Bool(b))=>*b, Some(Value::Number(n))=>n.as_f64().map_or(false,|f|f!=0.0&&!f.is_nan()), Some(Value::String(s))=>!s.is_empty(), Some(_)=>true } }
fn str_qq(v: Option<&Value>) -> String { match v { None|Some(Value::Null)=>String::new(), Some(Value::String(s))=>s.clone(), Some(Value::Bool(b))=>b.to_string(), Some(n@Value::Number(_))=>n.to_string(), Some(o)=>o.to_string() } } // = JS String(v ?? '')
fn js_string(v:&Value)->String{ match v{Value::Null=>"null".into(),Value::String(s)=>s.clone(),Value::Number(n)=>n.to_string(),Value::Bool(b)=>b.to_string(),Value::Array(a)=>a.iter().map(js_string).collect::<Vec<_>>().join(","),Value::Object(_)=>"[object Object]".into()} } // = JS String(v)
fn tmpl(v: Option<&Value>) -> String { match v { None=>"undefined".into(), Some(Value::Null)=>"null".into(), Some(x)=>str_qq(Some(x)) } } // = JS `${v}`

const DEFAULT_ERR_MSG: &str = "the rebuild process exited unexpectedly \u{2014} resume from the last checkpoint";

// { job_id, status, phase } — the row after (or unchanged by) the transition. Key order is contract.
fn shape(job_id: Value, status: Value, phase: Value) -> Value {
    let mut out = Map::new();
    out.insert("job_id".into(), job_id);
    out.insert("status".into(), status);
    out.insert("phase".into(), phase);
    Value::Object(out)
}

pub fn codebaseOnboardControl(input: &Value, db: &Connection) -> Result<Value, String> {
    let job_id = input.get("job_id");
    if !truthy(job_id) { return Err("codebase-onboard-control: job_id is required".into()); }
    let op = input.get("op").and_then(|v| v.as_str());
    if op != Some("pause") && op != Some("resume-mark") && op != Some("mark-error") {
        return Err("codebase-onboard-control: op must be one of: pause, resume-mark, mark-error".into());
    }
    let op = op.unwrap();

    let job_id_str = str_qq(job_id); // String(job_id)
    let rows = crate::db_json::query_rows(db, "SELECT job_id, status, phase FROM code_onboard_jobs WHERE job_id = ?", &[json!(job_id_str)])?;
    let row = match rows.as_array().and_then(|a| a.first()).cloned() {
        Some(r) => r,
        None => return Err(format!("codebase-onboard-control: onboard job '{}' not found", job_id_str)),
    };
    let row_job_id = row.get("job_id").cloned().unwrap_or(Value::Null);
    let row_status = row.get("status").cloned().unwrap_or(Value::Null);
    let row_phase = row.get("phase").cloned().unwrap_or(Value::Null);
    let status_s = row_status.as_str().unwrap_or("");
    let rjob_s = row_job_id.as_str().unwrap_or("").to_string();

    // mark-error: only a still-live-looking job flips; a terminal-state job is returned unchanged.
    if op == "mark-error" {
        if status_s != "running" && status_s != "pause_requested" {
            return Ok(shape(row_job_id, row_status, row_phase));
        }
        let msg_val: Value = match input.get("message").filter(|v| !v.is_null()) { Some(m) => m.clone(), None => json!(DEFAULT_ERR_MSG) };
        let msg: String = js_string(&msg_val).chars().take(500).collect();
        let err_val = format!("[{}] {}", tmpl(Some(&row_phase)), msg);
        db.execute(
            &format!("UPDATE code_onboard_jobs SET status = 'error', error = ?, updated_at = {} WHERE job_id = ?", crate::clock::now_sql()),
            rusqlite::params![err_val, rjob_s],
        ).map_err(|e| e.to_string())?;
        return Ok(shape(row_job_id, json!("error"), row_phase));
    }

    if op == "pause" {
        if status_s != "running" {
            return Err("codebase-onboard-control: job.status must be 'running' (pause only applies to a live run)".into());
        }
        db.execute(
            &format!("UPDATE code_onboard_jobs SET status = 'pause_requested', updated_at = {} WHERE job_id = ?", crate::clock::now_sql()),
            rusqlite::params![rjob_s],
        ).map_err(|e| e.to_string())?;
        return Ok(shape(row_job_id, json!("pause_requested"), row_phase));
    }

    // resume-mark
    if status_s != "paused" && status_s != "error" && status_s != "failed" {
        return Err("codebase-onboard-control: job.status must be 'paused' or 'error' (resume only applies to an interrupted run)".into());
    }
    db.execute(
        &format!("UPDATE code_onboard_jobs SET status = 'running', error = NULL, updated_at = {} WHERE job_id = ?", crate::clock::now_sql()),
        rusqlite::params![rjob_s],
    ).map_err(|e| e.to_string())?;
    Ok(shape(row_job_id, json!("running"), row_phase))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn setup() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE code_onboard_jobs (job_id TEXT PRIMARY KEY, root TEXT NOT NULL, phase TEXT NOT NULL DEFAULT 'queued', status TEXT NOT NULL DEFAULT 'running', profile_json TEXT, error TEXT, detail TEXT, created_at TEXT NOT NULL, updated_at TEXT NOT NULL)").unwrap();
        c
    }
    fn ins(c: &Connection, job_id: &str, phase: &str, status: &str, error: Option<&str>) {
        c.execute(
            "INSERT INTO code_onboard_jobs (job_id, root, phase, status, error, created_at, updated_at) VALUES (?,?,?,?,?,?,?)",
            rusqlite::params![job_id, "/repo", phase, status, error, "2025-12-20 00:00:00", "2025-12-20 00:00:00"],
        ).unwrap();
    }
    fn after(c: &Connection, job_id: &str) -> Value {
        crate::db_json::query_rows(c, "SELECT status, error, updated_at FROM code_onboard_jobs WHERE job_id = ?", &[json!(job_id)]).unwrap()[0].clone()
    }

    #[test]
    fn pause_flips_running_to_pause_requested() {
        let c = setup();
        ins(&c, "j1", "ingest", "running", None);
        let r = codebaseOnboardControl(&json!({"job_id":"j1","op":"pause"}), &c).unwrap();
        assert_eq!(r, json!({"job_id":"j1","status":"pause_requested","phase":"ingest"}));
        let a = after(&c, "j1");
        assert_eq!(a["status"], json!("pause_requested"));
        assert_eq!(a["updated_at"], json!("2026-01-01 00:00:00"));
        // key order is contract
        assert_eq!(r.as_object().unwrap().keys().collect::<Vec<_>>(), vec!["job_id","status","phase"]);
    }

    #[test]
    fn resume_mark_from_paused_error_failed() {
        for st in ["paused", "error", "failed"] {
            let c = setup();
            ins(&c, "j", "scope", st, Some("boom"));
            let r = codebaseOnboardControl(&json!({"job_id":"j","op":"resume-mark"}), &c).unwrap();
            assert_eq!(r, json!({"job_id":"j","status":"running","phase":"scope"}));
            let a = after(&c, "j");
            assert_eq!(a["status"], json!("running"));
            assert_eq!(a["error"], json!(null)); // error cleared to NULL
        }
    }

    #[test]
    fn mark_error_flips_live_job_with_default_message() {
        let c = setup();
        ins(&c, "j2", "metrics", "running", None);
        let r = codebaseOnboardControl(&json!({"job_id":"j2","op":"mark-error"}), &c).unwrap();
        assert_eq!(r, json!({"job_id":"j2","status":"error","phase":"metrics"}));
        let a = after(&c, "j2");
        assert_eq!(a["error"], json!(format!("[metrics] {}", DEFAULT_ERR_MSG)));
        assert_eq!(a["updated_at"], json!("2026-01-01 00:00:00"));
    }

    #[test]
    fn mark_error_custom_message_and_pause_requested_source() {
        let c = setup();
        ins(&c, "j3", "ingest", "pause_requested", None);
        let r = codebaseOnboardControl(&json!({"job_id":"j3","op":"mark-error","message":"OOM killed"}), &c).unwrap();
        assert_eq!(r, json!({"job_id":"j3","status":"error","phase":"ingest"}));
        assert_eq!(after(&c, "j3")["error"], json!("[ingest] OOM killed"));
    }

    #[test]
    fn mark_error_noop_on_terminal_job() {
        let c = setup();
        ins(&c, "j4", "done", "done", None);
        let r = codebaseOnboardControl(&json!({"job_id":"j4","op":"mark-error"}), &c).unwrap();
        assert_eq!(r, json!({"job_id":"j4","status":"done","phase":"done"}));
        assert_eq!(after(&c, "j4")["updated_at"], json!("2025-12-20 00:00:00")); // untouched
    }

    #[test]
    fn guards_error_out() {
        let c = setup();
        ins(&c, "j5", "ingest", "paused", None);
        assert!(codebaseOnboardControl(&json!({"op":"pause"}), &c).is_err()); // missing job_id
        assert!(codebaseOnboardControl(&json!({"job_id":"j5","op":"bogus"}), &c).is_err()); // bad op
        assert!(codebaseOnboardControl(&json!({"job_id":"ghost","op":"pause"}), &c).is_err()); // not found
        assert!(codebaseOnboardControl(&json!({"job_id":"j5","op":"pause"}), &c).is_err()); // pause on non-running
    }
}
