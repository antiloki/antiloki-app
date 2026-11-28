//! agent-subagent-update.rs — twin of skills/entities/agent-subagent-update.mjs (db WRITE lane).
//! NAMES ARE CONTRACT: `agentSubagentUpdate` (named default). Partial edit of a sub-agent's TUNABLE
//! fields (specialization_md, description, is_abstract, applies_to); structural identity immutable.
// QUALITY: 80/100 — dynamic partial UPDATE; frozen updated_at; empty-set → {...row,unchanged:true}
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
fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) }

pub fn agentSubagentUpdate(input: &Value, db: &Connection) -> Result<Value, String> {
    // guard-throws (context.db, agent required, agent not found) are never fixtured (success path only)
    let agent = input.get("agent").cloned().unwrap_or(Value::Null);
    if !truthy(Some(&agent)) { return Err("agent-subagent-update: agent is required".into()); }

    let existing = crate::db_json::query_rows(db, "SELECT name FROM agents WHERE name = ?", &[agent.clone()])?;
    if existing.as_array().map(|a| a.is_empty()).unwrap_or(true) {
        return Err(format!("agent-subagent-update: agent '{}' not found", agent.as_str().unwrap_or("")));
    }

    let mut sets: Vec<String> = Vec::new();
    let mut args: Vec<Value> = Vec::new();
    // specialization_md !== undefined → set (?? null); present-null becomes NULL
    if input.get("specialization_md").is_some() {
        sets.push("specialization_md = ?".into());
        args.push(nz(input.get("specialization_md")).cloned().unwrap_or(Value::Null));
    }
    // description !== undefined && description (only a truthy description is applied)
    if truthy(input.get("description")) {
        sets.push("description = ?".into());
        args.push(input.get("description").cloned().unwrap());
    }
    // is_abstract !== undefined → 1|0 (present-null → 0)
    if input.get("is_abstract").is_some() {
        sets.push("is_abstract = ?".into());
        args.push(json!(if truthy(input.get("is_abstract")) { 1 } else { 0 }));
    }
    // applies_to !== undefined → null | string passthrough | JSON.stringify(other)
    if let Some(at) = input.get("applies_to") {
        let v = match at {
            Value::Null => Value::Null,
            Value::String(s) => json!(s),
            other => json!(serde_json::to_string(other).unwrap()),
        };
        sets.push("applies_to_json = ?".into());
        args.push(v);
    }

    if sets.is_empty() {
        let rows = crate::db_json::query_rows(db, "SELECT * FROM agents WHERE name = ?", &[agent.clone()])?;
        let mut m = match rows.as_array().and_then(|a| a.first()).cloned() {
            Some(Value::Object(o)) => o,   // { ...row, unchanged: true }
            _ => Map::new(),               // spread of undefined → {}
        };
        m.insert("unchanged".into(), json!(true));
        return Ok(Value::Object(m));
    }

    // datetime('now') is frozen to the canonical instant on the JS side; write the literal here.
    sets.push(format!("updated_at = {}", crate::clock::now_sql()));
    let sql = format!("UPDATE agents SET {} WHERE name = ?", sets.join(", "));
    let mut params = args;
    params.push(agent.clone());
    crate::db_json::run_write(db, &sql, &params)?;

    let rows = crate::db_json::query_rows(db, "SELECT * FROM agents WHERE name = ?", &[agent])?;
    Ok(rows.as_array().and_then(|a| a.first()).cloned().unwrap_or(Value::Null))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn seed(c: &Connection) {
        c.execute_batch(
            "CREATE TABLE agents ( id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL UNIQUE, description TEXT NOT NULL, \
             skills_json TEXT NOT NULL DEFAULT '[]', context_json TEXT NOT NULL DEFAULT '{}', permissions_json TEXT NOT NULL DEFAULT '{}', \
             model TEXT, custom_prompt_md TEXT, parent_agent TEXT, specialization_md TEXT, applies_to_json TEXT, \
             is_abstract INTEGER NOT NULL DEFAULT 0, created_at TEXT NOT NULL DEFAULT (datetime('now')), updated_at TEXT NOT NULL DEFAULT (datetime('now')) ); \
             INSERT INTO agents (name, description, parent_agent, specialization_md, applies_to_json, is_abstract, created_at, updated_at) \
             VALUES ('tl.smoke','d','tl','old',NULL,0,'2025-11-01 00:00:00','2025-12-01 00:00:00');").unwrap();
    }

    #[test]
    fn partial_update_tunes_fields_and_stamps_frozen_time() {
        let c = Connection::open_in_memory().unwrap();
        seed(&c);
        let r = agentSubagentUpdate(&json!({
            "agent": "tl.smoke", "specialization_md": "new and improved",
            "applies_to": { "globs": ["**/*.x"] }, "is_abstract": 1
        }), &c).unwrap();
        assert_eq!(r["specialization_md"], json!("new and improved"));
        assert_eq!(r["applies_to_json"], json!(r#"{"globs":["**/*.x"]}"#));
        assert_eq!(r["is_abstract"], json!(1));
        assert_eq!(r["parent_agent"], json!("tl")); // edge untouched
        assert_eq!(r["name"], json!("tl.smoke"));    // name untouched
        assert_eq!(r["updated_at"], json!("2026-01-01 00:00:00")); // frozen
    }

    #[test]
    fn empty_update_returns_unchanged() {
        let c = Connection::open_in_memory().unwrap();
        seed(&c);
        let r = agentSubagentUpdate(&json!({ "agent": "tl.smoke" }), &c).unwrap();
        assert_eq!(r["unchanged"], json!(true));
        assert_eq!(r["updated_at"], json!("2025-12-01 00:00:00")); // untouched — no update fired
        // "unchanged" is appended AFTER the columns (spread order preserved)
        let keys: Vec<&str> = r.as_object().unwrap().keys().map(|k| k.as_str()).collect();
        assert_eq!(*keys.last().unwrap(), "unchanged");
    }

    #[test]
    fn null_and_falsy_edges() {
        let c = Connection::open_in_memory().unwrap();
        seed(&c);
        // description "" is skipped; is_abstract null → 0; specialization_md present-null → NULL
        let r = agentSubagentUpdate(&json!({
            "agent": "tl.smoke", "description": "", "is_abstract": Value::Null, "specialization_md": Value::Null
        }), &c).unwrap();
        assert_eq!(r["description"], json!("d")); // unchanged (falsy skipped)
        assert_eq!(r["is_abstract"], json!(0));
        assert_eq!(r["specialization_md"], Value::Null);
        // applies_to as a raw string passes through unchanged
        let r2 = agentSubagentUpdate(&json!({ "agent": "tl.smoke", "applies_to": "raw vocab" }), &c).unwrap();
        assert_eq!(r2["applies_to_json"], json!("raw vocab"));
    }
}
