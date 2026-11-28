//! interviewer-session-stale-end.rs — twin of skills/agents/in/interviewer-session-stale-end.mjs
//! (db WRITE lane). NAMES ARE CONTRACT: `interviewerSessionStaleEnd` (default). Auto-closes active
//! interviewer sessions idle past the threshold; datetime('now') frozen to 2026-01-01 00:00:00.
use rusqlite::Connection;
use serde_json::{json, Value};

const DEFAULT_IDLE_MINUTES: f64 = 60.0;

// = JS Number(v)
fn js_number(v: Option<&Value>) -> f64 {
    match v {
        None => f64::NAN,
        Some(Value::Null) => 0.0,
        Some(Value::Bool(b)) => if *b { 1.0 } else { 0.0 },
        Some(Value::Number(n)) => n.as_f64().unwrap_or(f64::NAN),
        Some(Value::String(s)) => { let t = s.trim(); if t.is_empty() { 0.0 } else { t.parse().unwrap_or(f64::NAN) } },
        Some(_) => f64::NAN,
    }
}

// JS `${n}` for a finite number in the minutes domain: integers render without a decimal point.
fn js_num_str(f: f64) -> String {
    if f == 0.0 { return "0".into(); }
    if f == f.trunc() && f.abs() < 1e15 { return format!("{}", f as i64); }
    format!("{}", f)
}

pub fn interviewerSessionStaleEnd(input: &Value, db: &Connection) -> Result<Value, String> {
    let n = js_number(input.get("idle_minutes"));
    let idle_minutes = if n.is_finite() { n } else { DEFAULT_IDLE_MINUTES };
    let modifier = format!("-{} minutes", js_num_str(idle_minutes));

    // JS: datetime('now', ?) — the conform JS lane freezes 'now' to the literal below; the twin
    // writes that same frozen instant so both lanes run identical SQL. The modifier stays a bind param.
    let stale = crate::db_json::query_rows(
        db,
        &format!("SELECT id, last_active_at FROM interviewer_sessions \
         WHERE status = 'active' AND COALESCE(last_active_at, started_at) < datetime({}, ?)", crate::clock::now_sql_inner()),
        &[json!(modifier)],
    )?;
    let rows = stale.as_array().cloned().unwrap_or_default();

    for s in &rows {
        crate::db_json::run_write(
            db,
            &format!("UPDATE interviewer_sessions SET status = 'closed', ended_at = {} WHERE id = ?", crate::clock::now_sql()),
            &[s.get("id").cloned().unwrap_or(Value::Null)],
        )?;
    }

    Ok(json!({ "ended": stale, "count": rows.len() as i64 }))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn seeded() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE interviewer_sessions (id TEXT PRIMARY KEY, status TEXT, last_active_at TEXT, started_at TEXT, ended_at TEXT); \
             INSERT INTO interviewer_sessions (id,status,last_active_at,started_at) VALUES \
               ('s-old','active','2025-12-20 00:00:00','2025-12-19 00:00:00'), \
               ('s-recent','active','2025-12-31 23:59:00','2025-12-31 00:00:00'), \
               ('s-nullactive','active',NULL,'2025-12-15 00:00:00'), \
               ('s-closed','closed','2025-12-01 00:00:00','2025-11-30 00:00:00');",
        ).unwrap();
        c
    }

    #[test]
    fn ends_stale_default_idle() {
        let c = seeded();
        let r = interviewerSessionStaleEnd(&json!({}), &c).unwrap();
        assert_eq!(r["count"], json!(2));
        let ended = r["ended"].as_array().unwrap();
        let ids: Vec<&str> = ended.iter().map(|e| e["id"].as_str().unwrap()).collect();
        assert_eq!(ids, vec!["s-old", "s-nullactive"]); // s-recent stays open, s-closed excluded by status
        assert_eq!(ended[1]["last_active_at"], Value::Null); // null-active row snapshots last_active_at: null
        // db_after: stamped ended_at on a closed row; the pre-existing s-closed keeps its untouched ended_at (NULL)
        let stamp = crate::db_json::query_rows(&c, "SELECT ended_at FROM interviewer_sessions WHERE id='s-old'", &[]).unwrap();
        assert_eq!(stamp[0]["ended_at"], json!("2026-01-01 00:00:00"));
        let recent = crate::db_json::query_rows(&c, "SELECT status FROM interviewer_sessions WHERE id='s-recent'", &[]).unwrap();
        assert_eq!(recent[0]["status"], json!("active"));
    }

    #[test]
    fn zero_idle_widens_window_to_now() {
        let c = seeded();
        // idle=0 → threshold is exactly the frozen 'now', so s-recent (23:59) is now < now and closes too.
        let r = interviewerSessionStaleEnd(&json!({ "idle_minutes": 0 }), &c).unwrap();
        assert_eq!(r["count"], json!(3));
        let ids: Vec<&str> = r["ended"].as_array().unwrap().iter().map(|e| e["id"].as_str().unwrap()).collect();
        assert_eq!(ids, vec!["s-old", "s-recent", "s-nullactive"]);
    }

    #[test]
    fn huge_window_ends_nothing() {
        let c = seeded();
        // 1e9 minutes ago sits centuries before any seeded row → nothing qualifies.
        let r = interviewerSessionStaleEnd(&json!({ "idle_minutes": 1000000000 }), &c).unwrap();
        assert_eq!(r["count"], json!(0));
        assert_eq!(r["ended"], json!([]));
    }
}
