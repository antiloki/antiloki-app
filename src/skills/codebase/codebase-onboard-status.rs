//! codebase-onboard-status.rs — twin of skills/codebase/codebase-onboard-status.mjs (db read+heal
//! lane). NAMES ARE CONTRACT: `codebaseOnboardStatus` (default). Reads a live onboard job's state;
//! ZOMBIE HEAL flips a `running` job dead when its heartbeat is > 5 min stale (frozen-clock lane).
use rusqlite::Connection;
use serde_json::{json, Map, Value};

fn truthy(v: Option<&Value>) -> bool { match v { None|Some(Value::Null)=>false, Some(Value::Bool(b))=>*b, Some(Value::Number(n))=>n.as_f64().map_or(false,|f|f!=0.0&&!f.is_nan()), Some(Value::String(s))=>!s.is_empty(), Some(_)=>true } }
fn str_qq(v: Option<&Value>) -> String { match v { None|Some(Value::Null)=>String::new(), Some(Value::String(s))=>s.clone(), Some(Value::Bool(b))=>b.to_string(), Some(n@Value::Number(_))=>n.to_string(), Some(o)=>o.to_string() } } // = JS String(v ?? '')
fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) } // JS `??` right operand

// The old wording asserted a cause it could not know ("the BE restarted"), and was wrong in the
// case that actually fired: the BE was up the whole time and the rebuild succeeded. Say what was
// observed — no heartbeat — and point at where the real reason lives.
const STALE_MSG: &str = "no heartbeat for 20 minutes \u{2014} the rebuild process is gone or wedged. Check the runner log (antiloki-BE/.onboard-logs) and rebuild again.";

pub fn codebaseOnboardStatus(input: &Value, db: &Connection) -> Result<Value, String> {
    let job_id = input.get("job_id");
    if !truthy(job_id) { return Err("codebase-onboard-status: job_id is required".into()); }
    let job_id_str = str_qq(job_id); // String(job_id)

    let rows = crate::db_json::query_rows(
        db,
        "SELECT job_id, root, phase, status, profile_json, error, detail, created_at, updated_at FROM code_onboard_jobs WHERE job_id = ?",
        &[json!(job_id_str)],
    )?;
    let mut row = match rows.as_array().and_then(|a| a.first()).cloned() {
        Some(r) => r,
        None => return Err(format!("codebase-onboard-status: onboard job '{}' not found", job_id_str)),
    };

    // ZOMBIE HEAL: a running job whose heartbeat has stopped is dead — flip it so the poller moves
    // on. Frozen-clock lane: JS `datetime('now', ...)` → literal 2026-01-01 00:00:00.
    //
    // THE WINDOW IS 20 MINUTES, NOT 5, AND THAT MATTERS. This check runs on a READ, so a poll is
    // what kills the job — and at 5 minutes it killed HEALTHY rebuilds: ingest is a single phase,
    // codebase-onboard only wrote the row at phase boundaries, and any repo big enough to spend
    // five minutes parsing tripped it. Observed on a 46-file Rust project: the runner exited 0
    // having written 1064 nodes, while the row read "failed — the BE restarted mid-onboard".
    // atlas-ingest now heartbeats per family, so a live job refreshes constantly; the wider window
    // is the seatbelt for the case a heartbeat cannot cover — one pathological file (a 400KB+
    // source) parsed inside a single family tick.
    if row.get("status").and_then(|v| v.as_str()) == Some("running") {
        let stale = crate::db_json::query_rows(
            db,
            &format!("SELECT 1 AS s FROM code_onboard_jobs WHERE job_id = ? AND updated_at < datetime({}, '-20 minutes')", crate::clock::now_sql_inner()),
            &[json!(job_id_str)],
        )?;
        if stale.as_array().map_or(false, |a| !a.is_empty()) {
            db.execute(
                // Bind STALE_MSG rather than repeat it inline — the two copies had already drifted
                // apart once, so the healed row and the returned row could disagree.
                &format!("UPDATE code_onboard_jobs SET status = 'failed', phase = 'failed', error = COALESCE(error, ?2), updated_at = {} WHERE job_id = ?1", crate::clock::now_sql()),
                rusqlite::params![job_id_str, STALE_MSG],
            ).map_err(|e| e.to_string())?;
            let obj = row.as_object_mut().unwrap();
            obj.insert("status".into(), json!("failed"));
            obj.insert("phase".into(), json!("failed"));
            let healed_err = nz(obj.get("error")).cloned().unwrap_or_else(|| json!(STALE_MSG)); // row.error ?? STALE_MSG
            obj.insert("error".into(), healed_err);
        }
    }

    let mut profile = Value::Null;
    if truthy(row.get("profile_json")) {
        if let Some(s) = row.get("profile_json").and_then(|v| v.as_str()) {
            profile = serde_json::from_str::<Value>(s).unwrap_or(Value::Null); // JSON.parse w/ catch → null
        }
    }
    let mut detail = Value::Null;
    if truthy(row.get("detail")) {
        if let Some(s) = row.get("detail").and_then(|v| v.as_str()) {
            detail = serde_json::from_str::<Value>(s).unwrap_or(Value::Null);
        }
    }

    let mut out = Map::new();
    out.insert("job_id".into(), row.get("job_id").cloned().unwrap_or(Value::Null));
    out.insert("root".into(), row.get("root").cloned().unwrap_or(Value::Null));
    out.insert("phase".into(), row.get("phase").cloned().unwrap_or(Value::Null));
    out.insert("status".into(), row.get("status").cloned().unwrap_or(Value::Null));
    out.insert("profile".into(), profile);
    out.insert("error".into(), nz(row.get("error")).cloned().unwrap_or(Value::Null)); // row.error ?? null
    out.insert("detail".into(), detail);
    out.insert("created_at".into(), row.get("created_at").cloned().unwrap_or(Value::Null));
    out.insert("updated_at".into(), row.get("updated_at").cloned().unwrap_or(Value::Null)); // ORIGINAL value (row read pre-UPDATE)
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn setup() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE code_onboard_jobs (job_id TEXT PRIMARY KEY, root TEXT NOT NULL, phase TEXT NOT NULL DEFAULT 'queued', status TEXT NOT NULL DEFAULT 'running', profile_json TEXT, error TEXT, detail TEXT, created_at TEXT NOT NULL, updated_at TEXT NOT NULL)").unwrap();
        c
    }
    fn ins(c: &Connection, job_id: &str, phase: &str, status: &str, profile: Option<&str>, error: Option<&str>, detail: Option<&str>, updated: &str) {
        c.execute(
            "INSERT INTO code_onboard_jobs (job_id, root, phase, status, profile_json, error, detail, created_at, updated_at) VALUES (?,?,?,?,?,?,?,?,?)",
            rusqlite::params![job_id, "/repo", phase, status, profile, error, detail, "2025-12-25 00:00:00", updated],
        ).unwrap();
    }

    #[test]
    fn done_job_parses_profile_and_detail() {
        let c = setup();
        ins(&c, "j1", "done", "done", Some("{\"lang\":\"js\",\"files\":10}"), None, Some("{\"folder\":\"src\"}"), "2025-12-25 10:05:00");
        let r = codebaseOnboardStatus(&json!({"job_id":"j1"}), &c).unwrap();
        assert_eq!(r["status"], json!("done"));
        assert_eq!(r["profile"], json!({"lang":"js","files":10}));
        assert_eq!(r["detail"], json!({"folder":"src"}));
        assert_eq!(r["error"], json!(null));
        // key order is contract
        let keys: Vec<&String> = r.as_object().unwrap().keys().collect();
        assert_eq!(keys, vec!["job_id","root","phase","status","profile","error","detail","created_at","updated_at"]);
    }

    #[test]
    fn stale_running_job_is_healed() {
        let c = setup();
        ins(&c, "z1", "ingest", "running", None, None, Some("{\"family_index\":1}"), "2025-12-20 08:30:00");
        let r = codebaseOnboardStatus(&json!({"job_id":"z1"}), &c).unwrap();
        assert_eq!(r["status"], json!("failed"));
        assert_eq!(r["phase"], json!("failed"));
        assert_eq!(r["error"], json!(STALE_MSG));
        assert_eq!(r["profile"], json!(null));
        assert_eq!(r["updated_at"], json!("2025-12-20 08:30:00")); // output keeps the pre-heal timestamp
        // db was actually flipped + timestamped to the frozen instant
        let after = crate::db_json::query_rows(&c, "SELECT status, error, updated_at FROM code_onboard_jobs WHERE job_id = ?", &[json!("z1")]).unwrap();
        assert_eq!(after[0]["status"], json!("failed"));
        assert_eq!(after[0]["error"], json!(STALE_MSG));
        assert_eq!(after[0]["updated_at"], json!("2026-01-01 00:00:00"));
    }

    #[test]
    fn stale_running_job_keeps_existing_error() {
        let c = setup();
        ins(&c, "z2", "scope", "running", None, Some("disk full"), None, "2025-12-01 00:00:00");
        let r = codebaseOnboardStatus(&json!({"job_id":"z2"}), &c).unwrap();
        assert_eq!(r["status"], json!("failed"));
        assert_eq!(r["error"], json!("disk full")); // COALESCE keeps the real error
    }

    #[test]
    fn fresh_running_job_not_healed() {
        let c = setup();
        ins(&c, "live", "metrics", "running", None, None, None, "2025-12-31 23:59:00");
        let r = codebaseOnboardStatus(&json!({"job_id":"live"}), &c).unwrap();
        assert_eq!(r["status"], json!("running")); // heartbeat is < 5 min old → untouched
        assert_eq!(r["detail"], json!(null));
    }

    #[test]
    fn bad_json_falls_back_to_null() {
        let c = setup();
        ins(&c, "broke", "failed", "error", Some("{not valid"), Some("boom"), Some("also not json"), "2025-12-30 00:00:00");
        let r = codebaseOnboardStatus(&json!({"job_id":"broke"}), &c).unwrap();
        assert_eq!(r["profile"], json!(null));
        assert_eq!(r["detail"], json!(null));
        assert_eq!(r["error"], json!("boom"));
    }
}
