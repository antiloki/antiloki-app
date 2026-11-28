//! agent-persona-resolve.rs — twin of skills/entities/agent-persona-resolve.mjs (db lane). NAMES ARE
//! CONTRACT: `agentPersonaResolve` (default) + `resolvePersona` (named, db-first helper). Inheritance engine.
// QUALITY: 80/100 — parent-chain walk (cycle+depth guard), per-level render, root→leaf compose
use rusqlite::Connection;
use serde_json::{json, Value};
use std::collections::HashSet;

const MAX_DEPTH: i32 = 8;

fn trimmed(v: &Value) -> Option<String> {
    v.as_str().map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

fn render_level(row: &Value) -> String {
    let ctx: Value = row.get("context_json").and_then(|v| v.as_str())
        .and_then(|s| serde_json::from_str::<Value>(s).ok()).filter(|v| v.is_object()).unwrap_or(json!({}));
    let mut parts: Vec<String> = Vec::new();
    if let Some(role) = ctx.get("role").filter(|v| !v.is_null()).and_then(|v| v.as_str()).filter(|s| !s.is_empty()) {
        parts.push(format!("ROLE: {role}"));
    }
    if let Some(ns) = ctx.get("north_star").filter(|v| !v.is_null()).and_then(|v| v.as_str()).filter(|s| !s.is_empty()) {
        parts.push(format!("NORTH STAR: {ns}"));
    }
    if let Some(pr) = ctx.get("principles").and_then(|v| v.as_array()).filter(|a| !a.is_empty()) {
        let body = pr.iter().map(|p| format!("- {}", p.as_str().map(|s| s.to_string()).unwrap_or_else(|| p.to_string()))).collect::<Vec<_>>().join("\n");
        parts.push(format!("PRINCIPLES:\n{body}"));
    }
    if let Some(sp) = row.get("specialization_md").and_then(trimmed) { parts.push(format!("SPECIALIZATION:\n{sp}")); }
    if let Some(cp) = row.get("custom_prompt_md").and_then(trimmed) { parts.push(format!("OPERATOR GUIDANCE:\n{cp}")); }
    parts.join("\n\n")
}

pub fn resolvePersona(db: &Connection, agent_name: &str) -> Value {
    let get_row = |name: &str| -> Option<Value> {
        let full = crate::db_json::query_rows(db, "SELECT name, context_json, custom_prompt_md, specialization_md, parent_agent, is_abstract FROM agents WHERE name = ?", &[json!(name)]);
        let rows = match full { Ok(r) => r, Err(_) => crate::db_json::query_rows(db, "SELECT name, context_json, custom_prompt_md FROM agents WHERE name = ?", &[json!(name)]).unwrap_or(json!([])) };
        rows.as_array().and_then(|a| a.first()).cloned()
    };
    let mut rows: Vec<Value> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut cur: Option<String> = Some(agent_name.to_string());
    let mut depth = 0;
    while let Some(c) = cur.clone() {
        if depth >= MAX_DEPTH || seen.contains(&c) { break; }
        seen.insert(c.clone());
        let row = match get_row(&c) { Some(r) => r, None => break };
        cur = row.get("parent_agent").and_then(|v| v.as_str()).filter(|s| !s.is_empty()).map(|s| s.to_string());
        rows.push(row);
        depth += 1;
    }
    if rows.is_empty() { return Value::Null; }
    rows.reverse(); // root → leaf
    let chain: Vec<Value> = rows.iter().map(|r| r.get("name").cloned().unwrap_or(Value::Null)).collect();
    let system_md = rows.iter().map(render_level).filter(|s| !s.is_empty()).collect::<Vec<_>>().join("\n\n───\n\n");
    let leaf = rows.last().unwrap();
    let is_abstract = leaf.get("is_abstract").map(|v| v.as_i64().map(|i| i != 0).unwrap_or(v.as_bool().unwrap_or(false))).unwrap_or(false);
    json!({ "agent": agent_name, "chain": chain, "system_md": system_md, "is_abstract": is_abstract })
}

pub fn agentPersonaResolve(input: &Value, db: &Connection) -> Result<Value, String> {
    let agent = match input.get("agent").filter(|v| v.as_str().map(|s| !s.is_empty()).unwrap_or(false)) {
        Some(v) => v.as_str().unwrap().to_string(), None => return Err("agent-persona-resolve: agent is required".into()),
    };
    let resolved = resolvePersona(db, &agent);
    if resolved.is_null() { return Err(format!("agent-persona-resolve: agent '{agent}' not found")); }
    Ok(resolved)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn composes_chain() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE agents (name TEXT, description TEXT, context_json TEXT, custom_prompt_md TEXT, specialization_md TEXT, parent_agent TEXT, is_abstract INTEGER); \
          INSERT INTO agents (name,context_json) VALUES ('base','{\"role\":\"reviewer\",\"principles\":[\"ground every claim\"]}'); \
          INSERT INTO agents (name,context_json,parent_agent,specialization_md) VALUES ('base.lawyer','{}','base','You argue it is CORRECT.');").unwrap();
        let r = agentPersonaResolve(&json!({ "agent": "base.lawyer" }), &c).unwrap();
        assert_eq!(r["chain"], json!(["base", "base.lawyer"]));
        assert!(r["system_md"].as_str().unwrap().contains("reviewer"));
        assert!(r["system_md"].as_str().unwrap().contains("argue it is CORRECT"));
        assert!(agentPersonaResolve(&json!({ "agent": "ghost" }), &c).is_err());
        // self-cycle broken
        c.execute("INSERT INTO agents (name,context_json,parent_agent) VALUES ('loop','{}','loop')", []).unwrap();
        assert_eq!(resolvePersona(&c, "loop")["chain"], json!(["loop"]));
    }
}
