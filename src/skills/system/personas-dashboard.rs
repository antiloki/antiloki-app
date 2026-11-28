//! personas-dashboard.rs — twin of skills/system/personas-dashboard.mjs (db READ, cross-db brain.*).
//! NAMES ARE CONTRACT: `personasDashboard` (default). Per-agent activity rollup for the FE
//! Personas screen: busy/idle, last action, recent fires, tools, 7d cost, call count. Cost/call
//! data is attributed to an agent via `prompt_templates.name` prefix up to the first '.'
//! (agentOf); unmatched calls (no prompt_template row, or a name without an agent prefix) bucket
//! under 'system'.
//!
//! Frozen clock: the JS builds `datetime('now', '-${hours} hours')` and a hardcoded
//! `datetime('now', '-60 seconds')` busy-window; the conform JS lane pins 'now' to
//! 2026-01-01 00:00:00, so this twin writes the frozen literal in both places. The JS
//! `if (!db) throw err.contextMissing(...)` guard is unreachable in the db lane (a connection is
//! always injected), so it is not transcribed.
// QUALITY: 80/100 — agent-prefix attribution bucket, frozen dual time windows (168h agg / 60s busy), JSON.parse-with-catch tools

use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::collections::{HashMap, HashSet};

fn js_number(v: Option<&Value>) -> f64 { match v { None=>f64::NAN, Some(Value::Null)=>0.0, Some(Value::Bool(b))=>if *b {1.0} else {0.0}, Some(Value::Number(n))=>n.as_f64().unwrap_or(f64::NAN), Some(Value::String(s))=>{let t=s.trim(); if t.is_empty(){0.0}else{t.parse().unwrap_or(f64::NAN)}}, Some(_)=>f64::NAN } } // = JS Number(v)

// JS `+x.toFixed(d)`, collapsed to integer when whole (JSON.stringify(5.0) is "5").
fn to_fixed_num(x: f64, d: usize) -> Value {
    let p: f64 = format!("{:.*}", d, x).parse().unwrap();
    if p.fract() == 0.0 && p.abs() < 9.007_199_254_740_992e15 { json!(p as i64) } else { json!(p) }
}
// JS number output (no toFixed): whole values render as integers, fractionals stay f64.
fn js_num(n: f64) -> Value { if n.is_finite() && n.fract() == 0.0 && n.abs() < 9e15 { json!(n as i64) } else { json!(n) } }
// `${n}` for the numbers that appear in this file's SQL interpolation (finite, fixture-controlled).
fn num_str(n: f64) -> String { if n.fract() == 0.0 && n.abs() < 9e15 { format!("{}", n as i64) } else { format!("{}", n) } }

// JS agentOf(promptName): falsy (null/undefined/empty) → None; else the prefix before the first
// '.', or the whole string if there is none or it starts with '.'.
fn agent_of(v: Option<&Value>) -> Option<String> {
    let s = match v { Some(Value::String(s)) if !s.is_empty() => s.clone(), _ => return None };
    match s.find('.') {
        Some(idx) if idx > 0 => Some(s[..idx].to_string()),
        _ => Some(s),
    }
}

fn archetype_of(name: &str) -> &'static str {
    match name {
        "pl" => "atlas",
        "tl" => "forge",
        "dev" => "forge",
        "qa" => "warden",
        "man" => "keeper",
        "system" => "system",
        "god" => "sovereign",
        "in" => "interviewer",
        "board-keeper" => "keeper",
        "keeper" => "keeper",
        _ => "unspecified",
    }
}
fn prompt_summary_of(name: &str) -> Option<&'static str> {
    match name {
        "pl" => Some("Plans drives by clarifying operator intent."),
        "tl" => Some("Refines contracts: scope, microtasks, briefings."),
        "dev" => Some("Implements microtasks within sandbox."),
        "qa" => Some("Verifies acceptance, scope, and the audit chain."),
        "man" => Some("Audits + closes contracts; manages card lifecycle."),
        "system" => Some("Substrate operations: state, queries, audits."),
        "god" => Some("Privileged escapes — only via signed TTY."),
        "in" => Some("Interviewer; routes operator turns into actions."),
        _ => None,
    }
}

// JS `try { tools = JSON.parse(a.skills_json) ?? []; } catch { tools = []; }` — SQL NULL, a parse
// error, or a parsed `null` all land on []; a parsed array is used as-is. (A parsed non-array,
// non-null value is not modeled — production skills_json is always a JSON array or absent.)
fn tools_from_skills_json(v: Option<&Value>) -> Vec<Value> {
    let s = match v {
        Some(Value::String(s)) => s.clone(),
        _ => "null".to_string(), // JSON.parse(null-ish) → JSON.parse("null") → null → ?? []
    };
    match serde_json::from_str::<Value>(&s) {
        Ok(Value::Array(a)) => a,
        _ => vec![],
    }
}

fn recent_fires_for(db: &Connection, agent: &str) -> Result<Value, String> {
    let rows = crate::db_json::query_rows(
        db,
        "SELECT id, ts, operation, entity_id FROM sacred_timeline WHERE actor = ? ORDER BY id DESC LIMIT 5",
        &[json!(agent)],
    )?;
    let arr = rows.as_array().cloned().unwrap_or_default();
    Ok(json!(arr
        .iter()
        .map(|r| json!({
            "slug": r.get("entity_id").cloned().unwrap_or(Value::Null),
            "ts": r.get("ts").cloned().unwrap_or(Value::Null),
            "action_name": r.get("operation").cloned().unwrap_or(Value::Null),
            "verdict": "success",
        }))
        .collect::<Vec<_>>()))
}

fn last_action_for(db: &Connection, agent: &str) -> Result<Value, String> {
    let rows = crate::db_json::query_rows(
        db,
        "SELECT operation, entity_id, ts FROM sacred_timeline WHERE actor = ? ORDER BY id DESC LIMIT 1",
        &[json!(agent)],
    )?;
    let arr = rows.as_array().cloned().unwrap_or_default();
    Ok(match arr.first() {
        None => Value::Null,
        Some(row) => json!({
            "action_name": row.get("operation").cloned().unwrap_or(Value::Null),
            "card_slug": row.get("entity_id").cloned().unwrap_or(Value::Null),
            "ts": row.get("ts").cloned().unwrap_or(Value::Null),
            "outcome": "success",
        }),
    })
}

pub fn personasDashboard(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS `const { window_hours = 168 } = input ?? {};` — default applies only when the key is
    // absent (undefined), NOT when it is explicitly null.
    let window_hours_raw: Value = match input.get("window_hours") {
        Some(v) if !v.is_null() => v.clone(),
        Some(_) => Value::Null,
        None => json!(168),
    };
    let wh_num = js_number(Some(&window_hours_raw));
    let effective_hours = if wh_num != 0.0 && !wh_num.is_nan() { wh_num } else { 168.0 };
    let since_clause = format!("datetime({}, '-{} hours')", crate::clock::now_sql_inner(), num_str(effective_hours));

    let agents_v = crate::db_json::query_rows(db, "SELECT name, description, skills_json, model FROM agents ORDER BY name", &[])?;
    let agents = agents_v.as_array().cloned().unwrap_or_default();

    let ai_agg_v = crate::db_json::query_rows(
        db,
        &format!(
            "SELECT pt.name AS prompt_name, a.cost_usd_actual AS cost_usd, a.duration_ms, a.outcome \
             FROM brain.ai_calls a \
             LEFT JOIN brain.prompt_templates pt ON pt.id = a.prompt_template_id \
             WHERE a.ts >= {since_clause}"
        ),
        &[],
    )?;
    let ai_agg = ai_agg_v.as_array().cloned().unwrap_or_default();

    let mut calls_by_agent: HashMap<String, (i64, f64)> = HashMap::new();
    let mut total_cost_7d_usd = 0.0f64;
    for row in &ai_agg {
        let agent = agent_of(row.get("prompt_name")).unwrap_or_else(|| "system".to_string());
        let cost_v = row.get("cost_usd").cloned().unwrap_or(Value::Null);
        let cost = if cost_v.is_null() { 0.0 } else { cost_v.as_f64().unwrap_or(0.0) };
        let entry = calls_by_agent.entry(agent).or_insert((0, 0.0));
        entry.0 += 1;
        entry.1 += cost;
        total_cost_7d_usd += cost;
    }

    let busy_rows_v = crate::db_json::query_rows(
        db,
        &format!("SELECT pt.name AS prompt_name \
         FROM brain.ai_calls a \
         LEFT JOIN brain.prompt_templates pt ON pt.id = a.prompt_template_id \
         WHERE a.ts >= datetime({}, '-60 seconds')", crate::clock::now_sql_inner()),
        &[],
    )?;
    let busy_set: HashSet<String> = busy_rows_v
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|r| agent_of(r.get("prompt_name")))
        .collect();

    let mut personas: Vec<Value> = Vec::new();
    for a in &agents {
        let name = a.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let tools = tools_from_skills_json(a.get("skills_json"));
        let (count, cost) = calls_by_agent.get(&name).cloned().unwrap_or((0, 0.0));
        let archetype = archetype_of(&name);
        let prompt_summary = match prompt_summary_of(&name) {
            Some(s) => json!(s),
            None => {
                let desc = a.get("description").and_then(|v| v.as_str()).unwrap_or("");
                json!(desc.chars().take(100).collect::<String>())
            }
        };
        let busy = busy_set.contains(&name);
        let last_action = last_action_for(db, &name)?;
        let recent_fires = recent_fires_for(db, &name)?;
        let tools_count = tools.len() as i64;
        let tools_head: Vec<Value> = tools.iter().take(10).cloned().collect();

        personas.push(json!({
            "agent": name,
            "archetype": archetype,
            "glyph": Value::Null,
            "default_tier": Value::Null,
            "prompt_summary": prompt_summary,
            "busy": busy,
            "last_action": last_action,
            "tools_count": tools_count,
            "tools": tools_head,
            "kb_tags": Value::Array(vec![]),
            "cost_7d_usd": to_fixed_num(cost, 6),
            "call_count_7d": count,
            "cli_share_pct": Value::Null,
            "recent_fires": recent_fires,
        }));
    }

    let mut out = Map::new();
    out.insert("personas".into(), Value::Array(personas));
    out.insert("total_cost_7d_usd".into(), to_fixed_num(total_cost_7d_usd, 6));
    out.insert("window_hours".into(), js_num(effective_hours));
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "ATTACH ':memory:' AS brain; \
             CREATE TABLE agents (name TEXT, description TEXT, skills_json TEXT, model TEXT); \
             CREATE TABLE sacred_timeline (id INTEGER PRIMARY KEY, ts TEXT, operation TEXT, entity_id TEXT, actor TEXT); \
             CREATE TABLE brain.prompt_templates (id INTEGER PRIMARY KEY, name TEXT); \
             CREATE TABLE brain.ai_calls (id INTEGER PRIMARY KEY, prompt_template_id INTEGER, cost_usd_actual REAL, duration_ms INTEGER, outcome TEXT, ts TEXT);",
        )
        .unwrap();
        c
    }

    #[test]
    fn attributes_cost_by_agent_prefix_and_defaults_unmatched_to_system() {
        let c = db();
        c.execute_batch(
            "INSERT INTO agents (name, description, skills_json, model) VALUES \
               ('designer', NULL, NULL, 'design-1'), \
               ('pl', 'Plans.', '[\"a\",\"b\",\"c\",\"d\",\"e\",\"f\",\"g\",\"h\",\"i\",\"j\",\"k\",\"l\"]', 'sonnet-mid'); \
             INSERT INTO brain.prompt_templates (id, name) VALUES (1, 'pl.plan.drive'); \
             INSERT INTO brain.ai_calls (prompt_template_id, cost_usd_actual, duration_ms, outcome, ts) VALUES \
               (1, 0.001, 120, 'success', '2025-12-26 10:00:00'), \
               (2, 0.002, 90, 'success', '2025-12-20 10:00:00'), \
               (NULL, 0.003, 50, 'success', '2025-12-27 08:00:00'), \
               (1, 0.005, 75, 'success', '2025-12-31 23:59:45');",
        )
        .unwrap();
        let r = personasDashboard(&json!({}), &c).unwrap();
        assert_eq!(r["window_hours"], json!(168));
        let personas = r["personas"].as_array().unwrap();
        let pl = personas.iter().find(|p| p["agent"] == json!("pl")).unwrap();
        assert_eq!(pl["archetype"], json!("atlas"));
        assert_eq!(pl["prompt_summary"], json!("Plans drives by clarifying operator intent."));
        assert_eq!(pl["call_count_7d"], json!(2)); // rows 1 + 4 (row 2 outside 168h window)
        assert_eq!(pl["cost_7d_usd"], json!(0.006));
        assert_eq!(pl["busy"], json!(true)); // row 4 within last 60s
        assert_eq!(pl["tools_count"], json!(12));
        assert_eq!(pl["tools"].as_array().unwrap().len(), 10); // capped
        let designer = personas.iter().find(|p| p["agent"] == json!("designer")).unwrap();
        assert_eq!(designer["archetype"], json!("unspecified"));
        assert_eq!(designer["prompt_summary"], json!("")); // null description → ''
        assert_eq!(designer["call_count_7d"], json!(0));
        assert_eq!(designer["cost_7d_usd"], json!(0));
        assert_eq!(designer["busy"], json!(false));
        assert_eq!(designer["tools_count"], json!(0)); // NULL skills_json → []
        // unmatched (row 3, prompt_template_id NULL) buckets under 'system' — not in `agents`
        // here, so it only shows up in the aggregate total, not a persona row.
        assert!((r["total_cost_7d_usd"].as_f64().unwrap() - 0.009).abs() < 1e-12); // 0.001+0.003+0.005
    }

    #[test]
    fn malformed_skills_json_falls_back_to_empty_tools() {
        let c = db();
        c.execute_batch("INSERT INTO agents (name, description, skills_json, model) VALUES ('tl', 'Refines.', 'not-json', NULL);").unwrap();
        let r = personasDashboard(&json!({}), &c).unwrap();
        let tl = &r["personas"][0];
        assert_eq!(tl["archetype"], json!("forge"));
        assert_eq!(tl["tools_count"], json!(0));
        assert_eq!(tl["tools"], json!([]));
        assert_eq!(tl["last_action"], Value::Null);
        assert_eq!(tl["recent_fires"], json!([]));
    }

    #[test]
    fn recent_fires_limited_to_five_most_recent_and_last_action_matches_top() {
        let c = db();
        c.execute_batch(
            "INSERT INTO agents (name, description, skills_json, model) VALUES ('pl', 'Plans.', '[]', NULL); \
             INSERT INTO sacred_timeline (id, ts, operation, entity_id, actor) VALUES \
               (1, '2025-12-01 08:00:00', 'card.add', 'CARD-A', 'pl'), \
               (2, '2025-12-05 08:00:00', 'card.update', 'CARD-B', 'pl'), \
               (3, '2025-12-10 08:00:00', 'card.status.change', 'CARD-C', 'pl'), \
               (4, '2025-12-15 08:00:00', 'law.add', 'LAW-D', 'pl'), \
               (5, '2025-12-20 08:00:00', 'memory.add', 'MEM-E', 'pl'), \
               (6, '2025-12-25 08:00:00', 'card.discuss', 'CARD-F', 'pl');",
        )
        .unwrap();
        let r = personasDashboard(&json!({}), &c).unwrap();
        let pl = &r["personas"][0];
        assert_eq!(pl["last_action"], json!({ "action_name": "card.discuss", "card_slug": "CARD-F", "ts": "2025-12-25 08:00:00", "outcome": "success" }));
        let fires = pl["recent_fires"].as_array().unwrap();
        assert_eq!(fires.len(), 5); // capped, id 1 dropped
        assert_eq!(fires[0], json!({ "slug": "CARD-F", "ts": "2025-12-25 08:00:00", "action_name": "card.discuss", "verdict": "success" }));
        assert_eq!(fires[4]["slug"], json!("CARD-B")); // id 2, the 5th most recent
    }

    #[test]
    fn window_hours_coercion_zero_null_and_string() {
        let c = db();
        c.execute_batch("INSERT INTO agents (name, description, skills_json, model) VALUES ('pl', 'Plans.', '[]', NULL);").unwrap();
        assert_eq!(personasDashboard(&json!({ "window_hours": 0 }), &c).unwrap()["window_hours"], json!(168)); // falsy → default
        assert_eq!(personasDashboard(&json!({ "window_hours": Value::Null }), &c).unwrap()["window_hours"], json!(168)); // explicit null → Number(null)=0 → default
        assert_eq!(personasDashboard(&json!({ "window_hours": "48" }), &c).unwrap()["window_hours"], json!(48)); // string coercion
        assert_eq!(personasDashboard(&Value::Null, &c).unwrap()["window_hours"], json!(168)); // whole input null
        assert_eq!(personasDashboard(&json!({}), &c).unwrap()["window_hours"], json!(168)); // key absent
    }

    #[test]
    fn window_hours_override_narrows_the_aggregation_cutoff() {
        let c = db();
        c.execute_batch(
            "INSERT INTO agents (name, description, skills_json, model) VALUES ('analyst', 'Analyzes.', '[]', NULL); \
             INSERT INTO brain.prompt_templates (id, name) VALUES (1, 'analyst.report.build'); \
             INSERT INTO brain.ai_calls (prompt_template_id, cost_usd_actual, duration_ms, outcome, ts) VALUES \
               (1, 0.1234567, 300, 'success', '2025-12-31 12:00:00'), \
               (1, 9.9, 400, 'success', '2025-12-26 00:00:00');",
        )
        .unwrap();
        let r = personasDashboard(&json!({ "window_hours": 24 }), &c).unwrap();
        assert_eq!(r["window_hours"], json!(24));
        let analyst = &r["personas"][0];
        assert_eq!(analyst["call_count_7d"], json!(1)); // only the in-window row
        assert_eq!(analyst["cost_7d_usd"], json!(0.123457)); // toFixed(6) rounding
    }
}
