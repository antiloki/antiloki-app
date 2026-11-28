//! agent-detail.rs — twin of skills/system/agent-detail.mjs (db READ lane; cross-db via brain.*).
//! NAMES ARE CONTRACT: `agentDetail` (default) + `skillsOfSteps` (named, no-db helper). Full
//! profile for one agent/persona: identity + core prompt (context_json), operator custom prompt,
//! the RELATIONAL granted actions (agent_actions, revoked_at IS NULL) each expanded to the skills
//! its steps_json composes, live prompt templates (brain.prompt_templates), a usage rollup over
//! brain.ai_calls, the agent's driver-pipeline role (agent_stages + brain.driver_ticks), dashboard
//! stats, and the card→contract→microtask→patch decomposition tree the agent drove.
//!
//! The JS `if (!db) throw err.contextMissing(...)` guard is unreachable in the db lane (a
//! connection is always injected), so it is not transcribed. The `try { SELECT <9 cols> } catch {
//! SELECT <5 cols> }` fallbacks (legacy DB pre-migration-144) are transcribed as query_rows(full)
//! .or_else(query_rows(fallback)) — the fixture schema always carries the full column set, so the
//! fallback branch is never exercised (same status as any other guard-throw per the playbook).
//!
//! Frozen clock: `date('now', '-29 days')` in buildStats → `date('2026-01-01 00:00:00', '-29 days')`.
// QUALITY: 80/100 — identity+relational-grants+cross-db usage/driver/stats rollup, one big read

use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::collections::HashSet;

const DECOMPOSITION_CARD_LIMIT: i64 = 12;

fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}
fn str_qq(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(),
        Some(o) => o.to_string(),
    }
}
fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) }
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

// JS `+x.toFixed(d)`, collapsed to integer when whole (JSON.stringify(5.0) is "5").
fn to_fixed_num(x: f64, d: usize) -> Value {
    let p: f64 = format!("{:.*}", d, x).parse().unwrap();
    if p.fract() == 0.0 && p.abs() < 9.007_199_254_740_992e15 { json!(p as i64) } else { json!(p) }
}

// Mirror of personas-dashboard ARCHETYPES so the two screens agree.
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
        "git-manager" => "git-keeper",
        _ => "unspecified",
    }
}

// JS safeParse(json, fallback): falsy `json` → fallback (parse skipped); else JSON.parse with
// try/catch → fallback.
fn safe_parse(v: Option<&Value>, fallback: Value) -> Value {
    if !truthy(v) { return fallback.clone(); }
    match v.and_then(|x| x.as_str()) {
        Some(s) => serde_json::from_str(s).unwrap_or(fallback),
        None => fallback, // non-string truthy column: not modeled (every safeParse call site here reads a TEXT column)
    }
}

// Unique, order-preserving list of skill names referenced by an action's steps.
pub fn skillsOfSteps(steps_json: &Value) -> Value {
    let parsed = safe_parse(Some(steps_json), json!([]));
    let arr = match parsed.as_array() { Some(a) => a.clone(), None => return json!([]) };
    let mut seen: HashSet<String> = HashSet::new();
    let mut out: Vec<Value> = Vec::new();
    for s in &arr {
        if let Some(name) = s.get("skill").and_then(|v| v.as_str()) {
            if seen.insert(name.to_string()) { out.push(json!(name)); }
        }
        // with-audit composes an inner skill via input.skill — surface it too.
        if let Some(inner) = s.get("input").and_then(|i| i.get("skill")).and_then(|v| v.as_str()) {
            if seen.insert(inner.to_string()) { out.push(json!(inner)); }
        }
    }
    json!(out)
}

// Where this agent sits in the driver pipeline: stages (static seeded role) + ticks_by_action /
// tick_outcomes (live activity attributed via brain.driver_ticks.decided_agent).
fn build_driver_role(db: &Connection, agent: &str) -> Result<Value, String> {
    let stage_rows_v = crate::db_json::query_rows(
        db,
        "SELECT id, stage_name, order_index, description_md, entry_signal, exit_signal FROM agent_stages WHERE agent_name = ? ORDER BY order_index",
        &[json!(agent)],
    )?;
    let stage_rows = stage_rows_v.as_array().cloned().unwrap_or_default();

    let mut stages: Vec<Value> = Vec::new();
    for s in &stage_rows {
        let sid = s.get("id").cloned().unwrap_or(Value::Null);
        let stage_actions = crate::db_json::query_rows(
            db,
            "SELECT action_name, order_index, is_optional, invocation_pattern, rationale_md FROM agent_stage_actions WHERE agent_stage_id = ? ORDER BY order_index",
            &[sid],
        )?;
        stages.push(json!({
            "stage_name": s.get("stage_name").cloned().unwrap_or(Value::Null),
            "order_index": s.get("order_index").cloned().unwrap_or(Value::Null),
            "description_md": s.get("description_md").cloned().unwrap_or(Value::Null),
            "entry_signal": s.get("entry_signal").cloned().unwrap_or(Value::Null),
            "exit_signal": s.get("exit_signal").cloned().unwrap_or(Value::Null),
            "actions": stage_actions,
        }));
    }

    let ticks_by_action_v = crate::db_json::query_rows(
        db,
        "SELECT decided_action AS action, COUNT(*) AS ticks, \
         SUM(CASE WHEN outcome = 'fired' THEN 1 ELSE 0 END) AS fired, \
         SUM(CASE WHEN outcome IN ('failed','escalated') THEN 1 ELSE 0 END) AS failed, \
         COALESCE(SUM(cost_usd), 0) AS cost_usd, MAX(started_at) AS last_at \
         FROM brain.driver_ticks WHERE decided_agent = ? AND decided_action IS NOT NULL \
         GROUP BY decided_action ORDER BY ticks DESC",
        &[json!(agent)],
    )?;
    let ticks_by_action: Vec<Value> = ticks_by_action_v
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|r| {
            let mut m = r.as_object().cloned().unwrap_or_default();
            let cost = m.get("cost_usd").and_then(|v| v.as_f64()).unwrap_or(0.0);
            m.insert("cost_usd".into(), to_fixed_num(cost, 6));
            Value::Object(m)
        })
        .collect();

    let tick_outcomes = crate::db_json::query_rows(
        db,
        "SELECT outcome AS bucket, COUNT(*) AS n FROM brain.driver_ticks WHERE decided_agent = ? GROUP BY outcome ORDER BY n DESC",
        &[json!(agent)],
    )?;

    Ok(json!({ "stages": stages, "ticks_by_action": ticks_by_action, "tick_outcomes": tick_outcomes }))
}

// Aggregations for the persona dashboard charts. `daily` is windowed to the last 30 days
// (frozen clock: 2026-01-01 00:00:00); distributions are all-time to match the usage rollup.
fn build_stats(db: &Connection, like: &str) -> Result<Value, String> {
    let daily_v = crate::db_json::query_rows(
        db,
        &format!("SELECT date(ac.ts) AS day, COUNT(*) AS calls, \
         COALESCE(SUM(ac.cost_usd_actual), 0) AS cost_usd, \
         COALESCE(SUM(ac.tokens_in), 0) AS tokens_in, \
         COALESCE(SUM(ac.tokens_out), 0) AS tokens_out \
         FROM brain.ai_calls ac JOIN brain.prompt_templates pt ON pt.id = ac.prompt_template_id \
         WHERE pt.name LIKE ? AND ac.ts >= date({}, '-29 days') \
         GROUP BY day ORDER BY day", crate::clock::now_sql_inner()),
        &[json!(like)],
    )?;
    let daily: Vec<Value> = daily_v
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|r| {
            let mut m = r.as_object().cloned().unwrap_or_default();
            let cost = m.get("cost_usd").and_then(|v| v.as_f64()).unwrap_or(0.0);
            m.insert("cost_usd".into(), to_fixed_num(cost, 6));
            Value::Object(m)
        })
        .collect();

    let by_outcome = crate::db_json::query_rows(
        db,
        "SELECT CASE WHEN ac.cached = 1 THEN 'cached' ELSE ac.outcome END AS bucket, COUNT(*) AS n \
         FROM brain.ai_calls ac JOIN brain.prompt_templates pt ON pt.id = ac.prompt_template_id \
         WHERE pt.name LIKE ? GROUP BY bucket ORDER BY n DESC",
        &[json!(like)],
    )?;

    let by_tier_v = crate::db_json::query_rows(
        db,
        "SELECT ac.tier, COUNT(*) AS calls, COALESCE(SUM(ac.cost_usd_actual), 0) AS cost_usd \
         FROM brain.ai_calls ac JOIN brain.prompt_templates pt ON pt.id = ac.prompt_template_id \
         WHERE pt.name LIKE ? GROUP BY ac.tier",
        &[json!(like)],
    )?;
    let by_tier: Vec<Value> = by_tier_v
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|r| {
            let mut m = r.as_object().cloned().unwrap_or_default();
            let cost = m.get("cost_usd").and_then(|v| v.as_f64()).unwrap_or(0.0);
            m.insert("cost_usd".into(), to_fixed_num(cost, 6));
            Value::Object(m)
        })
        .collect();

    let by_prompt_v = crate::db_json::query_rows(
        db,
        "SELECT pt.name AS prompt_name, COUNT(*) AS calls, \
         COALESCE(SUM(ac.cost_usd_actual), 0) AS cost_usd, \
         COALESCE(SUM(ac.tokens_in + ac.tokens_out), 0) AS tokens, \
         COALESCE(AVG(ac.duration_ms), 0) AS avg_ms \
         FROM brain.ai_calls ac JOIN brain.prompt_templates pt ON pt.id = ac.prompt_template_id \
         WHERE pt.name LIKE ? GROUP BY pt.name ORDER BY cost_usd DESC, calls DESC",
        &[json!(like)],
    )?;
    let by_prompt: Vec<Value> = by_prompt_v
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|r| {
            let mut m = r.as_object().cloned().unwrap_or_default();
            let cost = m.get("cost_usd").and_then(|v| v.as_f64()).unwrap_or(0.0);
            m.insert("cost_usd".into(), to_fixed_num(cost, 6));
            let avg_ms = m.get("avg_ms").and_then(|v| v.as_f64()).unwrap_or(0.0);
            m.insert("avg_ms".into(), json!(crate::js_math::js_round(avg_ms) as i64));
            Value::Object(m)
        })
        .collect();

    Ok(json!({ "daily": daily, "by_outcome": by_outcome, "by_tier": by_tier, "by_prompt": by_prompt }))
}

// Card → contract → microtask → patch tree for cards this agent drove (brain.driver_ticks,
// most-recently-driven first, capped to cardLimit).
fn build_decomposition(db: &Connection, agent: &str, card_limit: i64) -> Result<Value, String> {
    let cards_v = crate::db_json::query_rows(
        db,
        "SELECT dt.card_slug AS slug, MAX(dt.started_at) AS last_tick FROM brain.driver_ticks dt \
         WHERE dt.decided_agent = ? AND dt.card_slug IS NOT NULL GROUP BY dt.card_slug ORDER BY last_tick DESC LIMIT ?",
        &[json!(agent), json!(card_limit)],
    )?;
    let cards = cards_v.as_array().cloned().unwrap_or_default();

    let mut out: Vec<Value> = Vec::new();
    for c in &cards {
        let slug = c.get("slug").cloned().unwrap_or(Value::Null);

        let contracts_v = crate::db_json::query_rows(
            db,
            "SELECT id, slug, title, status, microtasks_json FROM contracts WHERE card_slug = ? ORDER BY id",
            &[slug.clone()],
        )?;
        let mut contracts: Vec<Value> = Vec::new();
        if let Some(arr) = contracts_v.as_array() {
            for ct in arr {
                let mt_parsed = safe_parse(ct.get("microtasks_json"), json!([]));
                // JS `(safeParse(..., []) || []).map(...)`: a falsy parse (e.g. parsed `null`)
                // falls back to []; a truthy non-array is not modeled (out of scope — production
                // microtasks_json is always a JSON array or absent).
                let mt_arr: Vec<Value> = if truthy(Some(&mt_parsed)) { mt_parsed.as_array().cloned().unwrap_or_default() } else { vec![] };
                let microtasks: Vec<Value> = mt_arr
                    .iter()
                    .map(|m| {
                        let id = nz(m.get("id")).or_else(|| nz(m.get("microtask_id"))).cloned().unwrap_or(Value::Null);
                        let summary = nz(m.get("summary"))
                            .or_else(|| nz(m.get("title")))
                            .or_else(|| nz(m.get("description")))
                            .cloned()
                            .unwrap_or(Value::Null);
                        let patches_arr: Vec<Value> = m.get("patches").and_then(|v| v.as_array()).cloned().unwrap_or_default();
                        let patches: Vec<Value> = patches_arr
                            .iter()
                            .map(|p| json!({
                                "file": p.get("file").cloned().unwrap_or(Value::Null),
                                "op": p.get("op").cloned().unwrap_or(Value::Null),
                                "rationale_md": p.get("rationale_md").cloned().unwrap_or(Value::Null),
                            }))
                            .collect();
                        json!({ "id": id, "summary": summary, "patches": patches })
                    })
                    .collect();
                contracts.push(json!({
                    "id": ct.get("id").cloned().unwrap_or(Value::Null),
                    "slug": ct.get("slug").cloned().unwrap_or(Value::Null),
                    "title": ct.get("title").cloned().unwrap_or(Value::Null),
                    "status": ct.get("status").cloned().unwrap_or(Value::Null),
                    "microtasks": microtasks,
                }));
            }
        }

        let title_rows = crate::db_json::query_rows(db, "SELECT title FROM cards WHERE slug = ?", &[slug.clone()])?;
        let card_title = title_rows.as_array().and_then(|a| a.first()).and_then(|r| r.get("title")).cloned().unwrap_or(Value::Null);

        out.push(json!({
            "card_slug": slug,
            "card_title": card_title,
            "last_tick": c.get("last_tick").cloned().unwrap_or(Value::Null),
            "contracts": contracts,
        }));
    }
    Ok(json!(out))
}

pub fn agentDetail(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS `if (!db) throw err.contextMissing('agent-detail','db')` — unreachable in the db lane
    // (a connection is always injected).

    // JS `input?.agent ?? input?.name` then `if (!agent) throw`.
    let agent_val = nz(input.get("agent")).or_else(|| nz(input.get("name"))).cloned().unwrap_or(Value::Null);
    if !truthy(Some(&agent_val)) {
        return Err("agent-detail: agent is required".into());
    }
    let agent = str_qq(Some(&agent_val));

    // EPIC-SUBAGENT: also read the specialization-tree fields. Defensive — a legacy DB without
    // migration 144 falls back to the base columns.
    let full_rows = crate::db_json::query_rows(
        db,
        "SELECT name, description, context_json, model, custom_prompt_md, parent_agent, specialization_md, applies_to_json, is_abstract FROM agents WHERE name = ?",
        &[json!(agent.clone())],
    );
    let rows_v = match full_rows {
        Ok(r) => r,
        Err(_) => crate::db_json::query_rows(
            db,
            "SELECT name, description, context_json, model, custom_prompt_md FROM agents WHERE name = ?",
            &[json!(agent.clone())],
        )?,
    };
    let row = match rows_v.as_array().and_then(|a| a.first()) {
        Some(r) => r.clone(),
        None => return Err(format!("agent-detail: agent '{agent}' not found")),
    };

    let ctx = safe_parse(row.get("context_json"), json!({}));

    // EPIC-SUBAGENT: this agent's direct sub-agent children (the tree under it).
    let children_rows = crate::db_json::query_rows(
        db,
        "SELECT name, description, is_abstract FROM agents WHERE parent_agent = ? ORDER BY name",
        &[json!(agent.clone())],
    );
    let children: Vec<Value> = children_rows.ok().and_then(|r| r.as_array().cloned()).unwrap_or_default();

    let grants_v = crate::db_json::query_rows(
        db,
        "SELECT aa.action_name, aa.granted_at, aa.granted_by, a.description, a.steps_json, a.kind, a.owner_agent \
         FROM agent_actions aa LEFT JOIN actions a ON a.name = aa.action_name \
         WHERE aa.agent_name = ? AND aa.revoked_at IS NULL ORDER BY aa.action_name",
        &[json!(agent.clone())],
    )?;
    let grants = grants_v.as_array().cloned().unwrap_or_default();

    let actions: Vec<Value> = grants
        .iter()
        .map(|g| {
            json!({
                "name": g.get("action_name").cloned().unwrap_or(Value::Null),
                "description": g.get("description").cloned().unwrap_or(Value::Null),
                "kind": g.get("kind").cloned().unwrap_or(Value::Null),
                "owner_agent": g.get("owner_agent").cloned().unwrap_or(Value::Null),
                "granted_at": g.get("granted_at").cloned().unwrap_or(Value::Null),
                "granted_by": g.get("granted_by").cloned().unwrap_or(Value::Null),
                "skills": skillsOfSteps(g.get("steps_json").unwrap_or(&Value::Null)),
            })
        })
        .collect();

    let like = format!("{agent}.%");

    let prompts = crate::db_json::query_rows(
        db,
        "SELECT name, version, default_tier, default_temperature, max_output_tokens, template_md \
         FROM brain.prompt_templates WHERE name LIKE ? AND superseded_by IS NULL ORDER BY name",
        &[json!(like.clone())],
    )?;

    let usage_v = crate::db_json::query_rows(
        db,
        "SELECT COUNT(*) AS call_count, \
         COALESCE(SUM(ac.cost_usd_actual), 0) AS total_cost_usd, \
         COALESCE(SUM(ac.tokens_in), 0) AS tokens_in, \
         COALESCE(SUM(ac.tokens_out), 0) AS tokens_out, \
         COALESCE(AVG(ac.duration_ms), 0) AS avg_duration_ms, \
         SUM(CASE WHEN ac.cached = 1 THEN 1 ELSE 0 END) AS cached_count, \
         SUM(CASE WHEN ac.outcome = 'success' THEN 1 ELSE 0 END) AS success_count, \
         MAX(ac.ts) AS last_fired_at \
         FROM brain.ai_calls ac JOIN brain.prompt_templates pt ON pt.id = ac.prompt_template_id \
         WHERE pt.name LIKE ?",
        &[json!(like.clone())],
    )?;
    let usage = usage_v.as_array().and_then(|a| a.first()).cloned().unwrap_or_else(|| json!({}));

    let n = js_number(input.get("decomposition_card_limit"));
    let card_limit: i64 = if n != 0.0 && !n.is_nan() { n as i64 } else { DECOMPOSITION_CARD_LIMIT };
    let decomposition = build_decomposition(db, &agent, card_limit)?;
    let stats = build_stats(db, &like)?;
    let driver = build_driver_role(db, &agent)?;

    let call_count = usage.get("call_count").and_then(|v| v.as_i64()).unwrap_or(0);
    let total_cost_raw = usage.get("total_cost_usd").and_then(|v| v.as_f64()).unwrap_or(0.0);
    let total_cost = to_fixed_num(total_cost_raw, 6);
    let total_cost_f = total_cost.as_f64().unwrap_or(0.0);

    let avg_cost_per_call = if call_count != 0 { to_fixed_num(total_cost_f / call_count as f64, 6) } else { json!(0) };
    let tokens_in_f = usage.get("tokens_in").and_then(|v| v.as_f64()).unwrap_or(0.0);
    let tokens_out_f = usage.get("tokens_out").and_then(|v| v.as_f64()).unwrap_or(0.0);
    let avg_tokens_per_call = if call_count != 0 {
        json!(crate::js_math::js_round((tokens_in_f + tokens_out_f) / call_count as f64) as i64)
    } else {
        json!(0)
    };
    let avg_duration_ms_raw = usage.get("avg_duration_ms").and_then(|v| v.as_f64()).unwrap_or(0.0);
    let cached_count_f = usage.get("cached_count").and_then(|v| v.as_f64()).unwrap_or(0.0);
    let cache_hit_pct = if call_count != 0 { to_fixed_num(100.0 * cached_count_f / call_count as f64, 1) } else { json!(0) };
    let success_count_f = usage.get("success_count").and_then(|v| v.as_f64()).unwrap_or(0.0);
    let success_pct = if call_count != 0 { to_fixed_num(100.0 * success_count_f / call_count as f64, 1) } else { json!(0) };

    let is_abstract = truthy(row.get("is_abstract"));
    let children_out: Vec<Value> = children
        .iter()
        .map(|c| {
            json!({
                "name": c.get("name").cloned().unwrap_or(Value::Null),
                "description": c.get("description").cloned().unwrap_or(Value::Null),
                "is_abstract": truthy(c.get("is_abstract")),
            })
        })
        .collect();

    let mut core = Map::new();
    core.insert("role".into(), ctx.get("role").cloned().unwrap_or(Value::Null));
    core.insert("north_star".into(), ctx.get("north_star").cloned().unwrap_or(Value::Null));
    core.insert(
        "principles".into(),
        match ctx.get("principles") { Some(Value::Array(a)) => Value::Array(a.clone()), _ => json!([]) },
    );

    let mut subagent = Map::new();
    subagent.insert("parent_agent".into(), row.get("parent_agent").cloned().unwrap_or(Value::Null));
    subagent.insert("specialization_md".into(), row.get("specialization_md").cloned().unwrap_or(Value::Null));
    subagent.insert("applies_to".into(), safe_parse(row.get("applies_to_json"), Value::Null));
    subagent.insert("is_abstract".into(), json!(is_abstract));
    subagent.insert("children".into(), json!(children_out));

    let mut usage_out = Map::new();
    usage_out.insert("call_count".into(), json!(call_count));
    usage_out.insert("total_cost_usd".into(), total_cost);
    usage_out.insert("tokens_in".into(), usage.get("tokens_in").cloned().unwrap_or(json!(0)));
    usage_out.insert("tokens_out".into(), usage.get("tokens_out").cloned().unwrap_or(json!(0)));
    usage_out.insert("last_fired_at".into(), usage.get("last_fired_at").cloned().unwrap_or(Value::Null));
    usage_out.insert("cards_touched".into(), json!(decomposition.as_array().map(|a| a.len()).unwrap_or(0) as i64));
    usage_out.insert("avg_cost_per_call".into(), avg_cost_per_call);
    usage_out.insert("avg_tokens_per_call".into(), avg_tokens_per_call);
    usage_out.insert("avg_duration_ms".into(), json!(crate::js_math::js_round(avg_duration_ms_raw) as i64));
    usage_out.insert("cache_hit_pct".into(), cache_hit_pct);
    usage_out.insert("success_pct".into(), success_pct);

    let mut out = Map::new();
    out.insert("agent".into(), row.get("name").cloned().unwrap_or(Value::Null));
    out.insert("description".into(), row.get("description").cloned().unwrap_or(Value::Null));
    out.insert("archetype".into(), json!(archetype_of(row.get("name").and_then(|v| v.as_str()).unwrap_or(""))));
    out.insert("model".into(), row.get("model").cloned().unwrap_or(Value::Null));
    out.insert("core".into(), Value::Object(core));
    out.insert("custom_prompt_md".into(), row.get("custom_prompt_md").cloned().unwrap_or(Value::Null));
    out.insert("subagent".into(), Value::Object(subagent));
    out.insert("actions".into(), json!(actions));
    out.insert("prompts".into(), prompts);
    out.insert("usage".into(), Value::Object(usage_out));
    out.insert("stats".into(), stats);
    out.insert("driver".into(), driver);
    out.insert("decomposition".into(), decomposition);
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "ATTACH ':memory:' AS brain; \
             CREATE TABLE agents (name TEXT, description TEXT, context_json TEXT, model TEXT, custom_prompt_md TEXT, parent_agent TEXT, specialization_md TEXT, applies_to_json TEXT, is_abstract INTEGER); \
             CREATE TABLE actions (name TEXT, description TEXT, steps_json TEXT, kind TEXT, owner_agent TEXT); \
             CREATE TABLE agent_actions (id INTEGER PRIMARY KEY AUTOINCREMENT, agent_name TEXT, action_name TEXT, granted_at TEXT, granted_by TEXT, revoked_at TEXT); \
             CREATE TABLE agent_stages (id INTEGER PRIMARY KEY AUTOINCREMENT, agent_name TEXT, stage_name TEXT, order_index INTEGER, description_md TEXT, entry_signal TEXT, exit_signal TEXT); \
             CREATE TABLE agent_stage_actions (id INTEGER PRIMARY KEY AUTOINCREMENT, agent_stage_id INTEGER, action_name TEXT, order_index INTEGER, is_optional INTEGER, invocation_pattern TEXT, rationale_md TEXT); \
             CREATE TABLE cards (slug TEXT, title TEXT); \
             CREATE TABLE contracts (id INTEGER PRIMARY KEY AUTOINCREMENT, slug TEXT, title TEXT, status TEXT, card_slug TEXT, microtasks_json TEXT); \
             CREATE TABLE brain.prompt_templates (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT, version INTEGER, default_tier TEXT, default_temperature TEXT, max_output_tokens INTEGER, template_md TEXT, superseded_by INTEGER); \
             CREATE TABLE brain.ai_calls (id INTEGER PRIMARY KEY AUTOINCREMENT, ts TEXT, prompt_template_id INTEGER, tier TEXT, cost_usd_actual REAL, tokens_in INTEGER, tokens_out INTEGER, duration_ms REAL, cached INTEGER, outcome TEXT); \
             CREATE TABLE brain.driver_ticks (id INTEGER PRIMARY KEY AUTOINCREMENT, started_at TEXT, card_slug TEXT, decided_agent TEXT, decided_action TEXT, outcome TEXT, cost_usd REAL);",
        )
        .unwrap();
        c
    }

    #[test]
    fn skills_of_steps_dedupes_and_surfaces_inner_with_audit_skill() {
        let steps = json!([
            { "skill": "with-audit", "input": { "skill": "card-update-raw" }, "as": "card" },
            { "skill": "card-score", "input": "$.input", "as": "score" },
            { "skill": "with-audit", "input": { "skill": "card-update-raw" }, "as": "again" },
        ]);
        assert_eq!(skillsOfSteps(&json!(steps.to_string())), json!(["with-audit", "card-update-raw", "card-score"]));
        assert_eq!(skillsOfSteps(&Value::Null), json!([]));
        assert_eq!(skillsOfSteps(&json!("not json")), json!([]));
    }

    #[test]
    fn returns_identity_core_and_relational_actions_with_composed_skills() {
        let c = db();
        c.execute_batch(
            "INSERT INTO agents (name, description, context_json, model, custom_prompt_md, is_abstract) VALUES \
               ('git-manager', 'Merges branches.', '{\"role\":\"git-keeper\",\"north_star\":\"clean history\",\"principles\":[\"never force-push main\"]}', 'sonnet-mid', NULL, 0); \
             INSERT INTO actions (name, description, steps_json, kind, owner_agent) VALUES \
               ('board-keeper.card.update', 'Updates a card.', '[{\"skill\":\"with-audit\",\"input\":{\"skill\":\"card-update-raw\"}}]', 'atomic', 'board-keeper'); \
             INSERT INTO agent_actions (agent_name, action_name, granted_at, granted_by, revoked_at) VALUES \
               ('git-manager', 'board-keeper.card.update', '2025-12-01 00:00:00', 'human:dani', NULL);",
        )
        .unwrap();
        let r = agentDetail(&json!({ "agent": "git-manager" }), &c).unwrap();
        assert_eq!(r["agent"], json!("git-manager"));
        assert_eq!(r["archetype"], json!("git-keeper"));
        assert_eq!(r["core"]["role"], json!("git-keeper"));
        assert_eq!(r["core"]["principles"], json!(["never force-push main"]));
        assert_eq!(r["custom_prompt_md"], Value::Null);
        let actions = r["actions"].as_array().unwrap();
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0]["name"], json!("board-keeper.card.update"));
        assert_eq!(actions[0]["skills"], json!(["with-audit", "card-update-raw"]));
    }

    #[test]
    fn accepts_name_alias_and_throws_not_found() {
        let c = db();
        c.execute("INSERT INTO agents (name, description, context_json) VALUES ('qa', 'Verifies.', '{}')", []).unwrap();
        let r = agentDetail(&json!({ "name": "qa" }), &c).unwrap();
        assert_eq!(r["agent"], json!("qa"));
        assert!(agentDetail(&json!({ "agent": "nobody" }), &c).unwrap_err().contains("not found"));
    }

    #[test]
    fn subagent_tree_and_usage_rollup_and_decomposition() {
        let c = db();
        c.execute_batch(
            "INSERT INTO agents (name, description, context_json, parent_agent, is_abstract) VALUES \
               ('tl.dev', 'Base.', '{}', NULL, 1); \
             INSERT INTO agents (name, description, context_json, parent_agent, specialization_md, applies_to_json, is_abstract) VALUES \
               ('tl.dev.style', 'Styling.', '{}', 'tl.dev', 'You author CSS only.', '{\"globs\":[\"**/*.css\"]}', 0); \
             INSERT INTO brain.prompt_templates (id, name, version, default_tier, default_temperature, max_output_tokens, template_md, superseded_by) VALUES \
               (1, 'tl.dev.plan', 1, 'mid', 'warm', 1024, 'template', NULL); \
             INSERT INTO brain.ai_calls (ts, prompt_template_id, tier, cost_usd_actual, tokens_in, tokens_out, duration_ms, cached, outcome) VALUES \
               ('2025-12-30 10:00:00', 1, 'mid', 0.12, 100, 50, 200, 0, 'success'), \
               ('2025-12-31 10:00:00', 1, 'mid', 0.08, 40, 10, 100, 1, 'error'); \
             INSERT INTO brain.driver_ticks (started_at, card_slug, decided_agent, decided_action, outcome, cost_usd) VALUES \
               ('2025-12-31 09:00:00', 'CARD-A', 'tl.dev', 'tl.plan', 'fired', 0.2); \
             INSERT INTO contracts (slug, title, status, card_slug, microtasks_json) VALUES \
               ('CT-A', 'Contract A', 'impl', 'CARD-A', '[{\"id\":\"m1\",\"summary\":\"do thing\",\"patches\":[{\"file\":\"a.rs\",\"op\":\"edit\"}]}]'); \
             INSERT INTO cards (slug, title) VALUES ('CARD-A', 'Card A Title');",
        )
        .unwrap();

        let r = agentDetail(&json!({ "agent": "tl.dev" }), &c).unwrap();
        assert_eq!(r["subagent"]["is_abstract"], json!(true));
        assert_eq!(r["subagent"]["children"][0]["name"], json!("tl.dev.style"));
        assert_eq!(r["subagent"]["children"][0]["is_abstract"], json!(false));

        assert_eq!(r["usage"]["call_count"], json!(2));
        assert_eq!(r["usage"]["total_cost_usd"], json!(0.2));
        assert_eq!(r["usage"]["tokens_in"], json!(140));
        assert_eq!(r["usage"]["avg_tokens_per_call"], json!(100)); // (140+60)/2 = 100
        assert_eq!(r["usage"]["cache_hit_pct"], json!(50)); // whole-valued toFixed(1) renders as an integer (JSON.stringify(50) is "50")
        assert_eq!(r["usage"]["success_pct"], json!(50));
        assert_eq!(r["usage"]["cards_touched"], json!(1));

        let decomp = r["decomposition"].as_array().unwrap();
        assert_eq!(decomp.len(), 1);
        assert_eq!(decomp[0]["card_slug"], json!("CARD-A"));
        assert_eq!(decomp[0]["card_title"], json!("Card A Title"));
        assert_eq!(decomp[0]["contracts"][0]["microtasks"][0]["id"], json!("m1"));
        assert_eq!(decomp[0]["contracts"][0]["microtasks"][0]["patches"][0]["file"], json!("a.rs"));

        let driver = &r["driver"];
        assert_eq!(driver["ticks_by_action"][0]["action"], json!("tl.plan"));
        assert_eq!(driver["ticks_by_action"][0]["cost_usd"], json!(0.2));
        assert_eq!(driver["tick_outcomes"][0]["bucket"], json!("fired"));
    }

    #[test]
    fn subagent_applies_to_defaults_to_null_when_column_absent_or_unset() {
        let c = db();
        c.execute("INSERT INTO agents (name, description, context_json, applies_to_json) VALUES ('pl', 'Plans.', '{}', NULL)", []).unwrap();
        let r = agentDetail(&json!({ "agent": "pl" }), &c).unwrap();
        assert_eq!(r["subagent"]["applies_to"], Value::Null);
    }

    #[test]
    fn decomposition_card_limit_overrides_default() {
        let c = db();
        c.execute("INSERT INTO agents (name, description, context_json) VALUES ('pl', 'Plans.', '{}')", []).unwrap();
        c.execute_batch(
            "INSERT INTO brain.driver_ticks (started_at, card_slug, decided_agent, decided_action, outcome, cost_usd) VALUES \
               ('2025-12-01 00:00:00', 'CARD-1', 'pl', 'a', 'fired', 0), \
               ('2025-12-02 00:00:00', 'CARD-2', 'pl', 'a', 'fired', 0), \
               ('2025-12-03 00:00:00', 'CARD-3', 'pl', 'a', 'fired', 0);",
        )
        .unwrap();
        let r = agentDetail(&json!({ "agent": "pl", "decomposition_card_limit": 2 }), &c).unwrap();
        assert_eq!(r["decomposition"].as_array().unwrap().len(), 2);
        let r_default = agentDetail(&json!({ "agent": "pl" }), &c).unwrap();
        assert_eq!(r_default["decomposition"].as_array().unwrap().len(), 3);
    }
}
