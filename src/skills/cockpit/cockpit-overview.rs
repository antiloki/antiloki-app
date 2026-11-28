//! cockpit-overview.rs — twin of skills/cockpit/cockpit-overview.mjs (db READ lane, async-over-sync).
//! NAMES ARE CONTRACT: `cockpitOverview` (named default). The no-session dashboard: aggregate stats
//! across ALL super-sessions + a per-session summary (panes, spend, AI turns, agents). ONE query.
// QUALITY: 80/100 — self-heals the cockpit schema (twin call), then one correlated-subquery roll-up
use rusqlite::Connection;
use serde_json::{json, Map, Value};

// The landing query: per-session panes/spend/calls/agents roll-up, newest first, capped at 100.
// Whitespace is irrelevant to SQLite and to the output (only the resulting rows are compared), but
// the column aliases ARE contract — they become the row-object keys (id,name,...,panes,cost_usd,...).
const OVERVIEW_SQL: &str = "
    SELECT s.id, s.name, s.status, s.mode, s.created_at,
      (SELECT COUNT(*) FROM cockpit_panes p WHERE p.session_id = s.id)                  AS panes,
      (SELECT COALESCE(SUM(p.cost_usd), 0) FROM cockpit_panes p WHERE p.session_id = s.id) AS cost_usd,
      (SELECT COALESCE(SUM(p.calls), 0) FROM cockpit_panes p WHERE p.session_id = s.id)    AS calls,
      (SELECT GROUP_CONCAT(p.subagent_name, ', ') FROM cockpit_panes p WHERE p.session_id = s.id) AS agents
    FROM cockpit_sessions s ORDER BY s.id DESC LIMIT 100
  ";

pub fn cockpitOverview(_input: &Value, db: &Connection) -> Result<Value, String> {
    // ensureCockpitSchema(db, '') — self-heal the tables on the active workspace (mirrors JS).
    crate::cockpit_cockpit_schema::ensureCockpitSchema(db, &json!(""));

    let sessions = crate::db_json::query_rows(db, OVERVIEW_SQL, &[])?;
    let sessions_arr = sessions.as_array().cloned().unwrap_or_default();

    // totals: filter/reduce over the rows. `x || 0` contributes 0 for a falsy value, so summing the
    // raw value is identical; counts stay i64, cost_usd stays f64 (REAL sums flow through unchanged).
    let active = sessions_arr.iter().filter(|s| s.get("status").and_then(|v| v.as_str()) == Some("active")).count();
    let panes: i64 = sessions_arr.iter().map(|s| s.get("panes").and_then(|v| v.as_i64()).unwrap_or(0)).sum();
    let cost_usd: f64 = sessions_arr.iter().map(|s| s.get("cost_usd").and_then(|v| v.as_f64()).unwrap_or(0.0)).sum();
    let calls: i64 = sessions_arr.iter().map(|s| s.get("calls").and_then(|v| v.as_i64()).unwrap_or(0)).sum();

    let mut totals = Map::new();
    totals.insert("sessions".into(), json!(sessions_arr.len() as i64));
    totals.insert("active".into(), json!(active as i64));
    totals.insert("panes".into(), json!(panes));
    totals.insert("cost_usd".into(), json!(cost_usd));
    totals.insert("calls".into(), json!(calls));

    let mut out = Map::new();
    out.insert("ok".into(), json!(true));
    out.insert("totals".into(), Value::Object(totals));
    out.insert("sessions".into(), Value::Array(sessions_arr));
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn aggregates_totals_and_per_session() {
        let c = Connection::open_in_memory().unwrap();
        // the skill self-heals the schema; seed against those tables, then run it (ensure is idempotent).
        crate::cockpit_cockpit_schema::ensureCockpitSchema(&c, &json!(""));
        c.execute_batch(
            "INSERT INTO cockpit_sessions (id,name,status,mode,created_at) VALUES \
               (1,'alpha','active','steer','2026-01-01 00:00:00'), \
               (2,'beta','merged','monitor','2026-01-02 00:00:00'), \
               (3,'gamma','active','steer','2026-01-03 00:00:00'); \
             INSERT INTO cockpit_panes (session_id,idx,subagent_name,calls,cost_usd) VALUES \
               (3,0,'dev',2,1.5),(3,1,'qa',3,2.5),(2,0,'man',1,0);",
        ).unwrap();
        let r = cockpitOverview(&json!({}), &c).unwrap();
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["totals"]["sessions"], json!(3));
        assert_eq!(r["totals"]["active"], json!(2)); // alpha + gamma
        assert_eq!(r["totals"]["panes"], json!(3));
        assert_eq!(r["totals"]["calls"], json!(6)); // 2+3+1
        assert!((r["totals"]["cost_usd"].as_f64().unwrap() - 4.0).abs() < 1e-9); // 1.5+2.5+0
        let s = r["sessions"].as_array().unwrap();
        // ORDER BY id DESC
        assert_eq!(s.iter().map(|x| x["id"].as_i64().unwrap()).collect::<Vec<_>>(), vec![3, 2, 1]);
        // session 3: two panes, GROUP_CONCAT in rowid order
        assert_eq!(s[0]["panes"], json!(2));
        assert_eq!(s[0]["calls"], json!(5));
        assert_eq!(s[0]["agents"], json!("dev, qa"));
        // session 1: no panes → COUNT 0, COALESCE(SUM,0)→0, GROUP_CONCAT→NULL
        assert_eq!(s[2]["panes"], json!(0));
        assert_eq!(s[2]["agents"], Value::Null);
        // per-session key order is the SELECT column order
        assert_eq!(s[0].as_object().unwrap().keys().collect::<Vec<_>>(),
            vec!["id", "name", "status", "mode", "created_at", "panes", "cost_usd", "calls", "agents"]);
    }

    #[test]
    fn empty_dashboard() {
        let c = Connection::open_in_memory().unwrap();
        let r = cockpitOverview(&json!({}), &c).unwrap();
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["sessions"].as_array().unwrap().len(), 0);
        assert_eq!(r["totals"]["sessions"], json!(0));
        assert_eq!(r["totals"]["active"], json!(0));
        assert_eq!(r["totals"]["panes"], json!(0));
        assert_eq!(r["totals"]["calls"], json!(0));
        assert!((r["totals"]["cost_usd"].as_f64().unwrap()).abs() < 1e-9);
        // totals key order
        assert_eq!(r["totals"].as_object().unwrap().keys().collect::<Vec<_>>(),
            vec!["sessions", "active", "panes", "cost_usd", "calls"]);
    }
}
