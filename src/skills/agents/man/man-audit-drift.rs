//! man-audit-drift.rs — twin of skills/agents/man/man-audit-drift.mjs (db READ lane). NAMES ARE
//! CONTRACT: `manAuditDrift` (default). Rolls up per-agent man-audit-compliance into one health report.
// QUALITY: aggregate over the compliance twin — stoplight, worst-offender, mean coverage toFixed(3)
use rusqlite::Connection;
use serde_json::{json, Map, Value};

// JS `+(n).toFixed(3)`: whole numbers render as "1" (integer), fractionals shortest ("0.5"/"0.333").
fn js_num(n: f64) -> Value {
    if n.is_finite() && n.fract() == 0.0 && n.abs() < 9e15 { json!(n as i64) } else { json!(n) }
}

pub fn manAuditDrift(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS destructure default null applies to undefined; explicit null stays null — both map to Null.
    let since_ts: Value = input.get("since_ts").cloned().unwrap_or(Value::Null);
    let agent_filter = input.get("agents");

    let rows = crate::db_json::query_rows(db,
        "SELECT DISTINCT agent_name FROM agent_stages ORDER BY agent_name", &[])?;
    let all_agents: Vec<Value> = rows.as_array()
        .map(|a| a.iter().map(|r| r.get("agent_name").cloned().unwrap_or(Value::Null)).collect())
        .unwrap_or_default();

    // Array.isArray(agents) && agents.length > 0 ? allAgents.filter(includes) : allAgents
    let filter_active = agent_filter.and_then(|v| v.as_array()).map(|a| !a.is_empty()).unwrap_or(false);
    let targets: Vec<Value> = if filter_active {
        let filt = agent_filter.unwrap().as_array().unwrap();
        all_agents.iter().filter(|a| filt.iter().any(|f| f == *a)).cloned().collect()
    } else {
        all_agents
    };

    let mut per_agent: Vec<Value> = Vec::new();
    for a in &targets {
        let r = crate::agents_man_man_audit_compliance::manAuditCompliance(
            &json!({ "agent_name": a, "since_ts": since_ts.clone() }), db)?;
        per_agent.push(r);
    }

    let mut total_unauthorized: i64 = 0;
    let mut total_skipped: i64 = 0;
    let mut worst_dc: i64 = -1;
    let mut worst = {
        let mut m = Map::new();
        m.insert("agent_name".into(), Value::Null);
        m.insert("drift_count".into(), json!(-1));
        m
    };
    for r in &per_agent {
        let u = r["drift"]["unauthorized_skills"].as_array().map(|x| x.len()).unwrap_or(0) as i64;
        let s = r["drift"]["skipped_always_skills"].as_array().map(|x| x.len()).unwrap_or(0) as i64;
        total_unauthorized += u;
        total_skipped += s;
        let drift_count = u + s;
        if drift_count > worst_dc {
            worst_dc = drift_count;
            let mut m = Map::new();
            m.insert("agent_name".into(), r["agent_name"].clone());
            m.insert("drift_count".into(), json!(drift_count));
            m.insert("coverage_pct".into(), r["coverage_pct"].clone());
            worst = m;
        }
    }

    let mean_coverage = if per_agent.is_empty() {
        json!(1)
    } else {
        let sum: f64 = per_agent.iter().map(|r| r["coverage_pct"].as_f64().unwrap_or(f64::NAN)).sum();
        let mean = sum / per_agent.len() as f64;
        js_num((mean * 1000.0).round() / 1000.0)
    };

    // stoplight(perAgent): unauth/skipped sums are identical to the totals computed above.
    let overall_health = if total_unauthorized == 0 && total_skipped <= 2 { "green" }
        else if total_unauthorized <= 2 && total_skipped <= 5 { "yellow" }
        else { "red" };

    let per_agent_out: Vec<Value> = per_agent.iter().map(|r| {
        let mut m = Map::new();
        m.insert("name".into(), r["agent_name"].clone());
        m.insert("coverage_pct".into(), r["coverage_pct"].clone());
        m.insert("unauthorized".into(), json!(r["drift"]["unauthorized_skills"].as_array().map(|x| x.len()).unwrap_or(0)));
        m.insert("skipped".into(), json!(r["drift"]["skipped_always_skills"].as_array().map(|x| x.len()).unwrap_or(0)));
        m.insert("summary".into(), r["drift"]["summary_md"].clone());
        Value::Object(m)
    }).collect();

    let window_since = if since_ts.is_null() { json!("all") } else { since_ts };
    let mut out = Map::new();
    out.insert("window".into(), json!({ "since_ts": window_since }));
    out.insert("agents_audited".into(), json!(per_agent.len()));
    out.insert("mean_coverage_pct".into(), mean_coverage);
    out.insert("total_unauthorized_skills".into(), json!(total_unauthorized));
    out.insert("total_skipped_always_skills".into(), json!(total_skipped));
    out.insert("worst_offender".into(), Value::Object(worst));
    out.insert("overall_health".into(), json!(overall_health));
    out.insert("per_agent".into(), json!(per_agent_out));
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seed() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE agent_stages (id INTEGER, agent_name TEXT, stage_name TEXT, order_index INTEGER); \
             CREATE TABLE agent_stage_actions (agent_stage_id INTEGER, action_name TEXT, invocation_pattern TEXT, order_index INTEGER); \
             CREATE TABLE runtime_events (id INTEGER PRIMARY KEY, ts TEXT, agent TEXT, action TEXT, outcome TEXT, duration_ms INTEGER); \
             INSERT INTO agent_stages VALUES (1,'be','work',0),(2,'pl','clarify',0); \
             INSERT INTO agent_stage_actions VALUES (1,'be.write','on_demand',0),(1,'be.read','on_demand',1),(2,'pl.ask','always',0),(2,'pl.think','on_demand',1); \
             INSERT INTO runtime_events VALUES (1,'t1','be','be.write','ok',5),(2,'t1','pl','pl.think','ok',5),(3,'t2','pl','pl.rogue','ok',9);",
        ).unwrap();
        c
    }

    #[test]
    fn rolls_up_two_agents() {
        let c = seed();
        let r = manAuditDrift(&json!({}), &c).unwrap();
        assert_eq!(r["agents_audited"], json!(2));
        assert_eq!(r["mean_coverage_pct"], json!(0.5));
        assert_eq!(r["total_unauthorized_skills"], json!(1));
        assert_eq!(r["total_skipped_always_skills"], json!(1));
        assert_eq!(r["worst_offender"], json!({ "agent_name": "pl", "drift_count": 2, "coverage_pct": 0.5 }));
        assert_eq!(r["overall_health"], json!("yellow"));
        assert_eq!(r["window"], json!({ "since_ts": "all" }));
        assert_eq!(r["per_agent"][0]["name"], json!("be"));
        assert_eq!(r["per_agent"][0]["summary"], json!("No drift detected."));
        assert_eq!(r["per_agent"][1]["unauthorized"], json!(1));
    }

    #[test]
    fn empty_is_green_with_sentinel_worst() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE agent_stages (id INTEGER, agent_name TEXT, stage_name TEXT, order_index INTEGER); \
             CREATE TABLE agent_stage_actions (agent_stage_id INTEGER, action_name TEXT, invocation_pattern TEXT, order_index INTEGER); \
             CREATE TABLE runtime_events (id INTEGER PRIMARY KEY, ts TEXT, agent TEXT, action TEXT, outcome TEXT, duration_ms INTEGER);",
        ).unwrap();
        let r = manAuditDrift(&json!({}), &c).unwrap();
        assert_eq!(r["agents_audited"], json!(0));
        assert_eq!(r["mean_coverage_pct"], json!(1));
        assert_eq!(r["worst_offender"], json!({ "agent_name": Value::Null, "drift_count": -1 }));
        assert_eq!(r["overall_health"], json!("green"));
    }

    #[test]
    fn agents_filter_intersects() {
        let c = seed();
        let r = manAuditDrift(&json!({ "agents": ["pl", "zzz"] }), &c).unwrap();
        assert_eq!(r["agents_audited"], json!(1));
        assert_eq!(r["per_agent"][0]["name"], json!("pl"));
        // empty array filter falls through to all agents
        let r2 = manAuditDrift(&json!({ "agents": [] }), &c).unwrap();
        assert_eq!(r2["agents_audited"], json!(2));
    }
}
