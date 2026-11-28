//! agent-custom-prompt-set.rs — twin of skills/entities/agent-custom-prompt-set.mjs (db WRITE lane).
//! NAMES ARE CONTRACT: `agentCustomPromptSet` (default). Set/clear an agent's operator-authored
//! custom_prompt_md; empty/whitespace/null clears it (→ NULL). datetime('now') frozen 2026-01-01.
use rusqlite::Connection;
use serde_json::{json, Value};

fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}
// = JS String(v) — used for String(custom_prompt_md) and the (un-fixtured) not-found error.
fn js_string(v: &Value) -> String {
    match v {
        Value::Null => "null".into(),
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Array(a) => a.iter().map(js_string).collect::<Vec<_>>().join(","),
        Value::Object(_) => "[object Object]".into(),
    }
}

pub fn agentCustomPromptSet(input: &Value, db: &Connection) -> Result<Value, String> {
    let agent = input.get("agent");
    if !truthy(agent) {
        return Err("agent-custom-prompt-set: agent is required".into());
    }
    let agent = agent.unwrap();

    // NOT_FOUND guard (un-fixtured): the agent name must exist.
    let existing = crate::db_json::query_rows(db, "SELECT name FROM agents WHERE name = ?", &[agent.clone()])?;
    if existing.as_array().unwrap().is_empty() {
        return Err(format!("agent-custom-prompt-set: agent '{}' not found", js_string(agent)));
    }

    // custom_prompt_md defaults to null (absent/undefined) — `== null` also short-circuits an
    // explicit null. Else String(custom_prompt_md).trim(); empty trim → NULL (clears the prompt).
    let cpm = input.get("custom_prompt_md");
    let value: Value = match cpm {
        None | Some(Value::Null) => Value::Null,
        Some(v) => {
            let trimmed = js_string(v).trim().to_string();
            if trimmed.is_empty() { Value::Null } else { json!(trimmed) }
        }
    };

    // JS: UPDATE ... updated_at = datetime('now') — frozen-clock lane pins 'now' to the instant below.
    crate::db_json::run_write(
        db,
        &format!("UPDATE agents SET custom_prompt_md = ?, updated_at = {} WHERE name = ?", crate::clock::now_sql()),
        &[value, agent.clone()],
    )?;

    let row = crate::db_json::query_rows(
        db,
        "SELECT name, description, custom_prompt_md, updated_at FROM agents WHERE name = ?",
        &[agent.clone()],
    )?;
    Ok(row.as_array().and_then(|a| a.first()).cloned().unwrap_or(Value::Null))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn seeded() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE agents (name TEXT PRIMARY KEY, description TEXT, custom_prompt_md TEXT, updated_at TEXT); \
             INSERT INTO agents VALUES ('DEV', 'developer', NULL, '2025-12-01 00:00:00'); \
             INSERT INTO agents VALUES ('PL', 'planner', 'old prompt', '2025-12-01 00:00:00');",
        ).unwrap();
        c
    }

    #[test]
    fn sets_trimmed_prompt_and_stamps() {
        let c = seeded();
        let r = agentCustomPromptSet(&json!({ "agent": "DEV", "custom_prompt_md": "  Be concise.  " }), &c).unwrap();
        assert_eq!(r, json!({ "name": "DEV", "description": "developer", "custom_prompt_md": "Be concise.", "updated_at": "2026-01-01 00:00:00" }));
    }

    #[test]
    fn clears_via_null_empty_whitespace_and_absent() {
        // explicit null clears an existing prompt
        let c = seeded();
        let r = agentCustomPromptSet(&json!({ "agent": "PL", "custom_prompt_md": Value::Null }), &c).unwrap();
        assert_eq!(r["custom_prompt_md"], Value::Null);
        assert_eq!(r["updated_at"], json!("2026-01-01 00:00:00"));
        // empty string clears
        let c = seeded();
        assert_eq!(agentCustomPromptSet(&json!({ "agent": "PL", "custom_prompt_md": "" }), &c).unwrap()["custom_prompt_md"], Value::Null);
        // whitespace-only clears
        let c = seeded();
        assert_eq!(agentCustomPromptSet(&json!({ "agent": "PL", "custom_prompt_md": "   " }), &c).unwrap()["custom_prompt_md"], Value::Null);
        // absent key defaults to null → clears
        let c = seeded();
        assert_eq!(agentCustomPromptSet(&json!({ "agent": "PL" }), &c).unwrap()["custom_prompt_md"], Value::Null);
    }

    #[test]
    fn overwrites_existing_and_guards() {
        let c = seeded();
        let r = agentCustomPromptSet(&json!({ "agent": "PL", "custom_prompt_md": "New guidance" }), &c).unwrap();
        assert_eq!(r["custom_prompt_md"], json!("New guidance"));
        assert!(agentCustomPromptSet(&json!({ "custom_prompt_md": "x" }), &c).unwrap_err().contains("agent is required"));
        assert!(agentCustomPromptSet(&json!({ "agent": "GHOST" }), &c).unwrap_err().contains("not found"));
    }
}
