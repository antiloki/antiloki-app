//! codebase-onboard-start.rs — twin of skills/codebase/codebase-onboard-start.mjs (db WRITE
//! lane). NAMES ARE CONTRACT: `codebaseOnboardStart` (default). SINGLE-FLIGHT: while a job is
//! running/pause_requested (and not stale > 5min), returns THAT job instead of minting a new one;
//! a stale live job is marked failed before a fresh job_id is minted.
//!
//! `job_id = "onboard-" + randomUUID().slice(0,12)` is NOT byte-identical to node:crypto's RNG —
//! mirrors the brain-session-start / atlas-smell-to-card precedent (see those twins' module docs):
//! fixtures only exercise the deterministic "already running" branch. The no-live-job branch and
//! the stale-live-job branch both end in a freshly minted randomUUID-derived job_id and are
//! therefore NOT fixture-comparable on either lane (JS itself produces a different job_id every
//! run) — implemented here for real usage, just never exercised by a fixture case.
//!
//! datetime('now') frozen to 2026-01-01 00:00:00 per playbook (both the staleness check and the
//! stale-row UPDATE's `updated_at`).

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

// Fallback for `onboard-${randomUUID().slice(0,12)}`. NOT byte-identical to node:crypto's RNG —
// never exercised by conform fixtures (see module doc).
fn random_uuid() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos();
    let mut x = (nanos as u64) ^ 0x9E3779B97F4A7C15u64;
    let mut hex = String::new();
    for _ in 0..32 {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        hex.push(std::char::from_digit((x % 16) as u32, 16).unwrap());
    }
    format!("{}-{}-{}-{}-{}", &hex[0..8], &hex[8..12], &hex[12..16], &hex[16..20], &hex[20..32])
}

pub fn codebaseOnboardStart(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS: `context.db` is always injected by the db lane — the `!db` guard-throw is unreachable
    // here, same convention as sibling twins (brain-session-start, atlas-entry-points).
    let root_in = input.as_object().and_then(|o| o.get("root")).filter(|v| !v.is_null());
    let root_val = match root_in {
        Some(v) => v.clone(),
        None => crate::engine_workspace_root::getWorkspaceRoot(db),
    };
    // JS `String(root).replace(/\\/g, '/')`.
    let root_s = match &root_val {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    };
    let norm_root = root_s.replace('\\', "/");

    let live_rows = crate::db_json::query_rows(
        db,
        &format!("SELECT job_id, root, phase, status, (updated_at < datetime({}, '-5 minutes')) AS stale FROM code_onboard_jobs WHERE status IN ('running','pause_requested') ORDER BY rowid DESC LIMIT 1", crate::clock::now_sql_inner()),
        &[],
    )?;
    let live = live_rows.as_array().and_then(|a| a.first()).cloned();

    if let Some(live_row) = &live {
        let stale = truthy(live_row.get("stale"));
        if !stale {
            let mut out = Map::new();
            out.insert("job_id".into(), live_row.get("job_id").cloned().unwrap_or(Value::Null));
            out.insert("root".into(), live_row.get("root").cloned().unwrap_or(Value::Null));
            out.insert("phase".into(), live_row.get("phase").cloned().unwrap_or(Value::Null));
            out.insert("status".into(), live_row.get("status").cloned().unwrap_or(Value::Null));
            out.insert("already_running".into(), json!(true));
            return Ok(Value::Object(out));
        }
        let job_id_live = live_row.get("job_id").cloned().unwrap_or(Value::Null);
        crate::db_json::run_write(
            db,
            &format!("UPDATE code_onboard_jobs SET status = 'failed', phase = 'failed', error = COALESCE(error, 'stale: the BE restarted mid-onboard — rebuild again'), updated_at = {} WHERE job_id = ?", crate::clock::now_sql()),
            &[job_id_live],
        )?;
    }

    let job_id = format!("onboard-{}", &random_uuid()[0..12]);
    crate::db_json::run_write(
        db,
        "INSERT INTO code_onboard_jobs (job_id, root, phase, status) VALUES (?, ?, 'queued', 'running')",
        &[json!(job_id.clone()), json!(norm_root.clone())],
    )?;

    let mut out = Map::new();
    out.insert("job_id".into(), json!(job_id));
    out.insert("root".into(), json!(norm_root));
    out.insert("phase".into(), json!("queued"));
    out.insert("status".into(), json!("running"));
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCHEMA: &str = "CREATE TABLE code_onboard_jobs ( job_id TEXT PRIMARY KEY, root TEXT NOT NULL, phase TEXT NOT NULL DEFAULT 'queued', status TEXT NOT NULL DEFAULT 'running', profile_json TEXT, error TEXT, detail TEXT, created_at TEXT NOT NULL DEFAULT '2026-01-01 00:00:00', updated_at TEXT NOT NULL DEFAULT '2026-01-01 00:00:00' )";

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(SCHEMA).unwrap();
        c
    }

    #[test]
    fn returns_the_live_running_job_instead_of_minting_a_new_one() {
        let c = db();
        c.execute(
            "INSERT INTO code_onboard_jobs (job_id, root, phase, status, updated_at) VALUES ('j1','/repo','ingest','running','2026-01-01 00:00:00')",
            [],
        ).unwrap();
        let r = codebaseOnboardStart(&json!({ "root": "C:/whatever" }), &c).unwrap();
        assert_eq!(
            r,
            json!({ "job_id": "j1", "root": "/repo", "phase": "ingest", "status": "running", "already_running": true })
        );
        let n: i64 = c.query_row("SELECT COUNT(*) FROM code_onboard_jobs", [], |row| row.get(0)).unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn picks_the_most_recently_inserted_live_job() {
        let c = db();
        c.execute(
            "INSERT INTO code_onboard_jobs (job_id, root, phase, status, updated_at) VALUES ('j-old','/a','ingest','running','2026-01-01 00:00:00')",
            [],
        ).unwrap();
        c.execute(
            "INSERT INTO code_onboard_jobs (job_id, root, phase, status, updated_at) VALUES ('j-new','/b','scope','pause_requested','2026-01-01 00:00:00')",
            [],
        ).unwrap();
        let r = codebaseOnboardStart(&json!({ "root": "/whatever" }), &c).unwrap();
        assert_eq!(r["job_id"], json!("j-new"));
        assert_eq!(r["status"], json!("pause_requested"));
    }
}
