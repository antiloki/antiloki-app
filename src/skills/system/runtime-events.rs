//! runtime-events.rs — twin of skills/system/runtime-events.mjs (db READ lane). NAMES ARE CONTRACT:
//! `runtimeEvents` (named default). Filtered reader of runtime_events for the FE Timeline.
// QUALITY: 81/100 — dynamic WHERE (id</> , goal, source, outcome, LIKE q), events + total + sources
use rusqlite::Connection;
use serde_json::{json, Value};

fn num(v: &Value) -> i64 {
    v.as_i64().or_else(|| v.as_str().and_then(|s| s.trim().parse::<i64>().ok())).unwrap_or(0)
}
fn present(v: Option<&Value>) -> bool { matches!(v, Some(x) if !x.is_null()) }
fn truthy(v: Option<&Value>) -> bool {
    match v { Some(Value::String(s)) => !s.is_empty(), Some(Value::Null) | None => false, Some(Value::Bool(b)) => *b, _ => true }
}

pub fn runtimeEvents(input: &Value, db: &Connection) -> Result<Value, String> {
    let limit_n = input.get("limit").map(num).unwrap_or(0);
    let cap = (if limit_n != 0 { limit_n } else { 200 }).clamp(1, 500);

    let mut wh: Vec<&str> = Vec::new();
    let mut params: Vec<Value> = Vec::new();
    if present(input.get("since_id")) { wh.push("id < ?"); params.push(json!(num(input.get("since_id").unwrap()))); }
    if present(input.get("after_id")) { wh.push("id > ?"); params.push(json!(num(input.get("after_id").unwrap()))); }
    if present(input.get("user_goal_id")) { wh.push("user_goal_id = ?"); params.push(json!(num(input.get("user_goal_id").unwrap()))); }
    if truthy(input.get("source")) { wh.push("source = ?"); params.push(json!(input.get("source").unwrap().as_str().unwrap_or(""))); }
    if truthy(input.get("outcome")) { wh.push("outcome = ?"); params.push(json!(input.get("outcome").unwrap().as_str().unwrap_or(""))); }
    if truthy(input.get("q")) {
        wh.push("(action LIKE ? OR skill LIKE ? OR actor LIKE ?)");
        let like = format!("%{}%", input.get("q").unwrap().as_str().unwrap_or(""));
        params.push(json!(like)); params.push(json!(like)); params.push(json!(like));
    }
    let where_clause = if wh.is_empty() { String::new() } else { format!("WHERE {}", wh.join(" AND ")) };

    let events_sql = format!(
        "SELECT id, ts, parent_event_id, actor, agent, action, skill, duration_ms, outcome, error_code, user_goal_id, source, context_json \
         FROM runtime_events {where_clause} ORDER BY id DESC LIMIT ?"
    );
    let mut ev_params = params.clone();
    ev_params.push(json!(cap));
    let events = match crate::db_json::query_rows(db, &events_sql, &ev_params) {
        Ok(e) => e,
        Err(e) => return Ok(json!({ "events": [], "total": 0, "sources": [], "error": e.chars().take(200).collect::<String>() })),
    };

    let total = crate::db_json::query_rows(db, &format!("SELECT COUNT(*) c FROM runtime_events {where_clause}"), &params)
        .ok().and_then(|r| r.as_array().and_then(|a| a.first()).and_then(|r| r.get("c")).and_then(|v| v.as_i64())).unwrap_or(0);
    let sources: Vec<Value> = crate::db_json::query_rows(db, "SELECT DISTINCT source FROM runtime_events WHERE COALESCE(source,'') != '' ORDER BY source", &[])
        .ok().and_then(|r| r.as_array().cloned()).unwrap_or_default()
        .iter().map(|r| r.get("source").cloned().unwrap_or(Value::Null)).collect();

    Ok(json!({ "events": events, "total": total, "sources": sources }))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn filters_and_counts() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE runtime_events (id INTEGER PRIMARY KEY, ts TEXT, parent_event_id INTEGER, actor TEXT, agent TEXT, action TEXT, skill TEXT, duration_ms INTEGER, outcome TEXT, error_code TEXT, user_goal_id INTEGER, source TEXT, context_json TEXT); \
          INSERT INTO runtime_events (actor,action,skill,outcome,source) VALUES ('a','card.add','card-add','ok','mcp'),('b','card.list','card-list','ok','tui'),('c','x','y','error','mcp');").unwrap();
        let r = runtimeEvents(&json!({}), &c).unwrap();
        assert_eq!(r["total"], json!(3));
        assert_eq!(r["events"][0]["id"], json!(3)); // id DESC
        assert_eq!(r["sources"], json!(["mcp", "tui"]));
        let f = runtimeEvents(&json!({ "outcome": "error" }), &c).unwrap();
        assert_eq!(f["total"], json!(1));
    }
}
