//! man-audit-compliance.rs — twin of skills/agents/man/man-audit-compliance.mjs (db READ lane). NAMES
//! ARE CONTRACT: `manAuditCompliance` (named default). Declared-vs-actual action drift for an agent.
// QUALITY: 80/100 — insertion-ordered sets, stage grouping, coverage toFixed(3), drift summary
use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::collections::HashSet;

// insertion-ordered dedup push (mirrors JS Set iteration order)
fn push_uniq(vec: &mut Vec<String>, set: &mut HashSet<String>, s: String) {
    if set.insert(s.clone()) { vec.push(s); }
}

pub fn manAuditCompliance(input: &Value, db: &Connection) -> Result<Value, String> {
    let agent_name = match input.get("agent_name").filter(|v| v.as_str().map(|s| !s.is_empty()).unwrap_or(false)) {
        Some(v) => v.as_str().unwrap().to_string(), None => return Err("man-audit-compliance: agent_name is required".into()),
    };
    let since_ts = input.get("since_ts").filter(|v| v.as_str().map(|s| !s.is_empty()).unwrap_or(false)).map(|v| v.as_str().unwrap().to_string());
    let limit_events = input.get("limit_events").and_then(|v| v.as_i64()).unwrap_or(1000);

    let stages = crate::db_json::query_rows(db,
        "SELECT s.id, s.stage_name, s.order_index, sk.action_name, sk.invocation_pattern \
         FROM agent_stages s LEFT JOIN agent_stage_actions sk ON sk.agent_stage_id = s.id \
         WHERE s.agent_name = ? ORDER BY s.order_index, sk.order_index", &[json!(agent_name)])?;
    let stages = stages.as_array().cloned().unwrap_or_default();

    let (mut decl_vec, mut decl_set) = (Vec::new(), HashSet::new());
    let (mut always_vec, mut always_set) = (Vec::new(), HashSet::new());
    for s in &stages {
        if let Some(a) = s.get("action_name").and_then(|v| v.as_str()) {
            push_uniq(&mut decl_vec, &mut decl_set, a.to_string());
            if s.get("invocation_pattern").and_then(|v| v.as_str()) == Some("always") {
                push_uniq(&mut always_vec, &mut always_set, a.to_string());
            }
        }
    }

    let mut wh = vec!["agent = ?".to_string()];
    let mut params = vec![json!(agent_name)];
    if let Some(ts) = &since_ts { wh.push("ts >= ?".into()); params.push(json!(ts)); }
    params.push(json!(limit_events));
    let events = crate::db_json::query_rows(db, &format!("SELECT id, ts, action, outcome, duration_ms FROM runtime_events WHERE {} ORDER BY id DESC LIMIT ?", wh.join(" AND ")), &params)?;
    let events = events.as_array().cloned().unwrap_or_default();

    let (mut run_vec, mut run_set) = (Vec::new(), HashSet::new());
    for e in &events { if let Some(a) = e.get("action").and_then(|v| v.as_str()) { push_uniq(&mut run_vec, &mut run_set, a.to_string()); } }

    let unauthorized: Vec<Value> = run_vec.iter().filter(|s| !decl_set.contains(*s)).map(|s| json!(s)).collect();
    let skipped: Vec<Value> = always_vec.iter().filter(|s| !run_set.contains(*s)).map(|s| json!(s)).collect();
    let declared_count = decl_vec.len();
    let declared_fired = decl_vec.iter().filter(|s| run_set.contains(*s)).count();
    let coverage_pct = if declared_count == 0 { json!(1) } else {
        let x = declared_fired as f64 / declared_count as f64;
        json!((x * 1000.0).round() / 1000.0)
    };

    // declared_stages: group by stage_name preserving order
    let mut ds: Vec<Value> = Vec::new();
    for s in &stages {
        let name = s.get("stage_name").cloned().unwrap_or(Value::Null);
        let idx = ds.iter().position(|x| x.get("stage_name") == Some(&name));
        let i = match idx { Some(i) => i, None => { ds.push(json!({ "stage_name": name, "order_index": s.get("order_index").cloned().unwrap_or(Value::Null), "actions": [] })); ds.len() - 1 } };
        if let Some(a) = s.get("action_name").filter(|v| !v.is_null()) {
            ds[i]["actions"].as_array_mut().unwrap().push(json!({ "action": a, "invocation_pattern": s.get("invocation_pattern").cloned().unwrap_or(Value::Null) }));
        }
    }

    let summary = if unauthorized.is_empty() && skipped.is_empty() { "No drift detected.".to_string() }
        else { format!("Drift: {} unauthorized actions, {} skipped always-actions.", unauthorized.len(), skipped.len()) };
    let mut out = Map::new();
    out.insert("agent_name".into(), json!(agent_name));
    out.insert("window".into(), json!({ "since_ts": since_ts.clone().unwrap_or_else(|| "all".into()), "event_count": events.len() }));
    out.insert("declared_stages".into(), json!(ds));
    out.insert("coverage_pct".into(), coverage_pct);
    out.insert("drift".into(), json!({ "unauthorized_skills": unauthorized, "skipped_always_skills": skipped, "summary_md": summary }));
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn audits_drift() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE agent_stages (id INTEGER, agent_name TEXT, stage_name TEXT, order_index INTEGER); \
          CREATE TABLE agent_stage_actions (agent_stage_id INTEGER, action_name TEXT, invocation_pattern TEXT, order_index INTEGER); \
          CREATE TABLE runtime_events (id INTEGER PRIMARY KEY, ts TEXT, agent TEXT, action TEXT, outcome TEXT, duration_ms INTEGER); \
          INSERT INTO agent_stages VALUES (1,'pl','clarify',0); \
          INSERT INTO agent_stage_actions VALUES (1,'pl.ask','always',0); \
          INSERT INTO runtime_events VALUES (1,'t','pl','pl.rogue','ok',5);").unwrap();
        let r = manAuditCompliance(&json!({ "agent_name": "pl" }), &c).unwrap();
        assert_eq!(r["drift"]["unauthorized_skills"], json!(["pl.rogue"]));
        assert_eq!(r["drift"]["skipped_always_skills"], json!(["pl.ask"]));
        assert_eq!(r["coverage_pct"], json!(0.0));
        assert!(manAuditCompliance(&json!({}), &c).is_err());
    }
}
