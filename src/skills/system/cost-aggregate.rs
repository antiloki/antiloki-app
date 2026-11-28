//! cost-aggregate.rs — twin of skills/system/cost-aggregate.mjs (db READ, cross-db brain.*).
//! NAMES ARE CONTRACT: `costAggregate` (default). Five `view` rollups behind the FE Cost Center:
//! today (spend vs cap, top-3 slugs, trend), sub_skill_matrix (per prompt/tier/model actual vs
//! Opus-baseline cost), provider_mix / agent_mix (donut/bar shares), file (per-file cost/token
//! attribution via contracts.files_touched_json, split evenly across a call's touched files).
//!
//! Frozen clock: the JS builds `datetime('now', '-${hours} hours')`, `date('now')`, and
//! `date('now', '-1 day')`; the conform JS lane pins 'now' to 2026-01-01 00:00:00, so this twin
//! writes the frozen literal in all three places. The JS `if (!db) throw err.contextMissing(...)`
//! guard is unreachable in the db lane (a connection is always injected), so it is not transcribed.
// QUALITY: 80/100 — 5-view dispatch, Opus-baseline savings math, insertion-ordered agent/file
// grouping with JS-stable sort, JSON.parse-with-catch file list (skip malformed row, not default)

use rusqlite::Connection;
use serde_json::{json, Value};
use std::collections::HashMap;

const OPUS_INPUT_PER_1M: f64 = 15.0;
const OPUS_OUTPUT_PER_1M: f64 = 75.0;
fn now_ts() -> String { crate::clock::now_ts_nodb() }

fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}
fn js_number(v: Option<&Value>) -> f64 {
    match v {
        None => f64::NAN,
        Some(Value::Null) => 0.0,
        Some(Value::Bool(b)) => if *b { 1.0 } else { 0.0 },
        Some(Value::Number(n)) => n.as_f64().unwrap_or(f64::NAN),
        Some(Value::String(s)) => { let t = s.trim(); if t.is_empty() { 0.0 } else { t.parse().unwrap_or(f64::NAN) } }
        Some(_) => f64::NAN,
    }
}
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
// JS Math.round: nearest integer, ties toward +Infinity.
fn js_round(x: f64) -> f64 { let f = x.floor(); if x - f >= 0.5 { f + 1.0 } else { f } }
// `${n}` for the hours value in the SQL interpolation (finite, fixture-controlled).
fn num_str(n: f64) -> String { if n.fract() == 0.0 && n.abs() < 9e15 { format!("{}", n as i64) } else { format!("{}", n) } }
// JS `+x.toFixed(d)`, collapsed to integer when whole (JSON.stringify(5.0) is "5").
fn to_fixed_num(x: f64, d: usize) -> Value {
    let p: f64 = format!("{:.*}", d, x).parse().unwrap();
    if p.fract() == 0.0 && p.abs() < 9.007_199_254_740_992e15 { json!(p as i64) } else { json!(p) }
}
// JS number output (no toFixed): whole values render as integers, fractionals stay f64.
fn js_num(n: f64) -> Value { if n.is_finite() && n.fract() == 0.0 && n.abs() < 9e15 { json!(n as i64) } else { json!(n) } }
// JS `v ?? '(unknown)'`
fn or_unknown(v: Option<&Value>) -> Value { match v { None | Some(Value::Null) => json!("(unknown)"), Some(x) => x.clone() } }

// JS agentOf(promptName): falsy (null/undefined/empty) -> None; else the prefix before the first
// '.', or the whole string if there is none or it starts with '.'.
fn agent_of(v: Option<&Value>) -> Option<String> {
    let s = match v { Some(Value::String(s)) if !s.is_empty() => s.clone(), _ => return None };
    match s.find('.') {
        Some(idx) if idx > 0 => Some(s[..idx].to_string()),
        _ => Some(s),
    }
}

pub fn costAggregate(input: &Value, db: &Connection) -> Result<Value, String> {
    let now = now_ts();
    let view = input.get("view").and_then(|v| v.as_str()).unwrap_or("");

    // JS `const { window_hours = 24 } = input ?? {}` — default applies only when the key is
    // absent (undefined), NOT when it is explicitly null. Then `Number(window_hours) || 24`.
    let window_hours_resolved: Value = match input.get("window_hours") {
        None => json!(24),
        Some(v) => v.clone(),
    };
    let n = js_number(Some(&window_hours_resolved));
    let hours = if n != 0.0 && !n.is_nan() { n } else { 24.0 };
    let since_clause = format!("datetime('{now}', '-{} hours')", num_str(hours));

    match view {
        "today" => today_spend(db),
        "sub_skill_matrix" => Ok(sub_skill_matrix(db, &since_clause)),
        "provider_mix" => Ok(provider_mix(db, &since_clause)),
        "agent_mix" => Ok(agent_mix(db, &since_clause)),
        "file" => Ok(file_attribution(db, &since_clause, input.get("path"))),
        _ => Err(format!(
            "cost-aggregate: view must be one of: today, sub_skill_matrix, provider_mix, agent_mix, file (got '{view}')"
        )),
    }
}

fn today_spend(db: &Connection) -> Result<Value, String> {
    let now = now_ts();
    let today_clause = format!("date(ts) = date('{now}')");
    let yesterday_clause = format!("date(ts) = date('{now}', '-1 day')");
    let today_clause_a = format!("date(a.ts) = date('{now}')"); // todayClause.replace(/ts/g,'a.ts')

    let attempt: Result<(Value, Value, Value), String> = (|| {
        let spend_sql = format!(
            "SELECT COALESCE(SUM(cost_usd_actual), 0) AS s, COUNT(*) AS c, \
             COALESCE(SUM(tokens_in), 0) AS tin, COALESCE(SUM(tokens_out), 0) AS tout, \
             COALESCE(SUM(tokens_cached), 0) AS tcached, \
             COALESCE(SUM(CASE WHEN cached = 1 THEN 1 ELSE 0 END), 0) AS cached \
             FROM brain.ai_calls WHERE {today_clause}"
        );
        let spend_rows = crate::db_json::query_rows(db, &spend_sql, &[])?;
        let yesterday_sql = format!("SELECT COALESCE(SUM(cost_usd_actual), 0) AS s FROM brain.ai_calls WHERE {yesterday_clause}");
        let yesterday_rows = crate::db_json::query_rows(db, &yesterday_sql, &[])?;
        let top_sql = format!(
            "SELECT pt.name AS prompt_name, SUM(a.cost_usd_actual) AS cost_usd, COUNT(*) AS calls \
             FROM brain.ai_calls a LEFT JOIN brain.prompt_templates pt ON pt.id = a.prompt_template_id \
             WHERE {today_clause_a} GROUP BY pt.name ORDER BY cost_usd DESC LIMIT 3"
        );
        let top_rows = crate::db_json::query_rows(db, &top_sql, &[])?;
        Ok((spend_rows, yesterday_rows, top_rows))
    })();

    let (spend_rows, yesterday_rows, top_rows) = match attempt {
        Ok(v) => v,
        Err(_) => {
            return Ok(json!({
                "spend_usd": 0, "cap_usd": 50, "cap_pct": 0, "top_3_slugs": [],
                "yesterday_spend_usd": 0, "trend": "flat"
            }))
        }
    };

    let spend_row = spend_rows.as_array().and_then(|a| a.first()).cloned().unwrap_or(Value::Null);
    let yesterday_row = yesterday_rows.as_array().and_then(|a| a.first()).cloned().unwrap_or(Value::Null);
    let top_arr = top_rows.as_array().cloned().unwrap_or_default();

    let spend_usd = js_number(spend_row.get("s"));
    let yesterday_spend_usd = js_number(yesterday_row.get("s"));

    let mut cap_usd = 50.0f64;
    if let Ok(cap_rows) = crate::db_json::query_rows(db, "SELECT value FROM cost_settings WHERE key='daily_cap_usd'", &[]) {
        if let Some(row) = cap_rows.as_array().and_then(|a| a.first()) {
            if truthy(row.get("value")) {
                let v = js_number(row.get("value"));
                cap_usd = if v != 0.0 && !v.is_nan() { v } else { 50.0 };
            }
        }
    }

    let cap_pct = if cap_usd > 0.0 { js_round((spend_usd / cap_usd) * 100.0) as i64 } else { 0 };
    let trend = if spend_usd > yesterday_spend_usd * 1.1 {
        "rising"
    } else if spend_usd < yesterday_spend_usd * 0.9 {
        "falling"
    } else {
        "flat"
    };

    let calls_today = js_number(spend_row.get("c"));
    let tokens_in = js_number(spend_row.get("tin"));
    let tokens_out = js_number(spend_row.get("tout"));
    let cached_calls = js_number(spend_row.get("cached"));
    let provider_cached_tokens = js_number(spend_row.get("tcached"));

    let estimated_usd = (tokens_in * OPUS_INPUT_PER_1M / 1_000_000.0) + (tokens_out * OPUS_OUTPUT_PER_1M / 1_000_000.0);

    let top_3: Vec<Value> = top_arr
        .iter()
        .map(|r| {
            json!({
                "slug": or_unknown(r.get("prompt_name")),
                "cost_usd": to_fixed_num(js_number(r.get("cost_usd")), 4),
                "calls": r.get("calls").cloned().unwrap_or(Value::Null),
            })
        })
        .collect();

    Ok(json!({
        "spend_usd": to_fixed_num(spend_usd, 4),
        "cap_usd": js_num(cap_usd),
        "cap_pct": cap_pct,
        "top_3_slugs": top_3,
        "yesterday_spend_usd": to_fixed_num(yesterday_spend_usd, 4),
        "trend": trend,
        "calls_today": js_num(calls_today),
        "tokens_in": js_num(tokens_in),
        "tokens_out": js_num(tokens_out),
        "tokens_total": js_num(tokens_in + tokens_out),
        "cached_calls": js_num(cached_calls),
        "cached_pct": if calls_today > 0.0 { js_round((cached_calls / calls_today) * 100.0) as i64 } else { 0 },
        "provider_cached_tokens": js_num(provider_cached_tokens),
        "provider_cached_pct": if tokens_in > 0.0 { js_round((provider_cached_tokens / tokens_in) * 100.0) as i64 } else { 0 },
        "estimated_usd": to_fixed_num(estimated_usd, 4),
    }))
}

fn sub_skill_matrix(db: &Connection, since_clause: &str) -> Value {
    let sql = format!(
        "SELECT pt.name AS prompt_name, a.tier, a.model_id, COUNT(*) AS calls, \
         SUM(a.cost_usd_actual) AS cost_actual_usd, \
         SUM(CASE WHEN a.cached = 1 THEN 1 ELSE 0 END) AS cached_calls, \
         SUM(a.tokens_in) AS tokens_in, SUM(a.tokens_out) AS tokens_out \
         FROM brain.ai_calls a LEFT JOIN brain.prompt_templates pt ON pt.id = a.prompt_template_id \
         WHERE a.ts >= {since_clause} GROUP BY pt.name, a.tier, a.model_id \
         ORDER BY cost_actual_usd DESC LIMIT 50"
    );
    let rows = match crate::db_json::query_rows(db, &sql, &[]) {
        Ok(v) => v.as_array().cloned().unwrap_or_default(),
        Err(_) => return json!([]),
    };
    let out: Vec<Value> = rows
        .iter()
        .map(|r| {
            let cost_actual = js_number(r.get("cost_actual_usd"));
            let tokens_in = js_number(r.get("tokens_in"));
            let tokens_out = js_number(r.get("tokens_out"));
            let cost_baseline = (tokens_in * OPUS_INPUT_PER_1M / 1_000_000.0) + (tokens_out * OPUS_OUTPUT_PER_1M / 1_000_000.0);
            let calls_v = r.get("calls").cloned().unwrap_or(Value::Null);
            let calls_n = js_number(Some(&calls_v));
            let cached_calls_v = r.get("cached_calls").cloned().unwrap_or(Value::Null);
            let cached_calls_n = js_number(Some(&cached_calls_v));
            json!({
                "prompt_name": or_unknown(r.get("prompt_name")),
                "tier": r.get("tier").cloned().unwrap_or(Value::Null),
                "model_id": r.get("model_id").cloned().unwrap_or(Value::Null),
                "calls": calls_v,
                "cost_actual_usd": to_fixed_num(cost_actual, 6),
                "cost_baseline_usd": to_fixed_num(cost_baseline, 6),
                "savings_usd": to_fixed_num(cost_baseline - cost_actual, 6),
                "cached_calls": cached_calls_v,
                "cached_pct": if calls_n > 0.0 { json!(js_round((cached_calls_n / calls_n) * 100.0) as i64) } else { json!(0) },
                "tokens_in": js_num(tokens_in),
                "tokens_out": js_num(tokens_out),
                "tokens_total": js_num(tokens_in + tokens_out),
            })
        })
        .collect();
    json!(out)
}

fn provider_mix(db: &Connection, since_clause: &str) -> Value {
    let sql = format!(
        "SELECT provider, COUNT(*) AS calls, SUM(cost_usd_actual) AS cost_usd \
         FROM brain.ai_calls WHERE ts >= {since_clause} GROUP BY provider ORDER BY cost_usd DESC"
    );
    let rows = match crate::db_json::query_rows(db, &sql, &[]) {
        Ok(v) => v.as_array().cloned().unwrap_or_default(),
        Err(_) => return json!([]),
    };
    let total_calls: f64 = rows.iter().map(|r| js_number(r.get("calls"))).sum();
    let total_cost: f64 = rows.iter().map(|r| js_number(r.get("cost_usd"))).sum();

    let out: Vec<Value> = rows
        .iter()
        .map(|r| {
            let calls_n = js_number(r.get("calls"));
            let cost_n = js_number(r.get("cost_usd"));
            json!({
                "provider": or_unknown(r.get("provider")),
                "calls": r.get("calls").cloned().unwrap_or(Value::Null),
                "cost_usd": to_fixed_num(cost_n, 6),
                "share_pct": if total_cost > 0.0 { to_fixed_num(cost_n / total_cost * 100.0, 2) } else { json!(0) },
                "share_calls_pct": if total_calls > 0.0 { to_fixed_num(calls_n / total_calls * 100.0, 2) } else { json!(0) },
            })
        })
        .collect();
    json!(out)
}

fn agent_mix(db: &Connection, since_clause: &str) -> Value {
    let sql = format!(
        "SELECT pt.name AS prompt_name, COUNT(*) AS calls, SUM(a.cost_usd_actual) AS cost_usd \
         FROM brain.ai_calls a LEFT JOIN brain.prompt_templates pt ON pt.id = a.prompt_template_id \
         WHERE a.ts >= {since_clause} GROUP BY pt.name"
    );
    let rows = match crate::db_json::query_rows(db, &sql, &[]) {
        Ok(v) => v.as_array().cloned().unwrap_or_default(),
        Err(_) => return json!([]),
    };

    let mut order: Vec<String> = Vec::new();
    let mut by_agent: HashMap<String, (f64, f64)> = HashMap::new(); // (calls, cost_usd)
    let mut total_cost = 0.0f64;
    for r in &rows {
        let agent = agent_of(r.get("prompt_name")).unwrap_or_else(|| "system".to_string());
        let calls_n = js_number(r.get("calls"));
        let cost_n = js_number(r.get("cost_usd"));
        if !by_agent.contains_key(&agent) {
            order.push(agent.clone());
            by_agent.insert(agent.clone(), (0.0, 0.0));
        }
        let entry = by_agent.get_mut(&agent).unwrap();
        entry.0 += calls_n;
        entry.1 += cost_n;
        total_cost += cost_n;
    }

    let mut out: Vec<Value> = order
        .iter()
        .map(|agent| {
            let (calls, cost_usd) = by_agent[agent];
            json!({
                "agent": agent,
                "calls": js_num(calls),
                "cost_usd": to_fixed_num(cost_usd, 6),
                "share_pct": if total_cost > 0.0 { to_fixed_num(cost_usd / total_cost * 100.0, 2) } else { json!(0) },
                "avg_cost_per_call_usd": if calls > 0.0 { to_fixed_num(cost_usd / calls, 6) } else { json!(0) },
            })
        })
        .collect();

    out.sort_by(|a, b| {
        let ac = a["cost_usd"].as_f64().unwrap_or(0.0);
        let bc = b["cost_usd"].as_f64().unwrap_or(0.0);
        bc.partial_cmp(&ac).unwrap_or(std::cmp::Ordering::Equal)
    });
    json!(out)
}

fn file_attribution(db: &Connection, since_clause: &str, only_path: Option<&Value>) -> Value {
    let sql = format!(
        "SELECT a.cost_usd_actual AS cost, a.tokens_in AS tin, a.tokens_out AS tout, \
         c.files_touched_json AS files \
         FROM brain.ai_calls a \
         JOIN sacred_timeline st ON st.id = a.parent_event_id AND st.entity_type = 'contract' \
         JOIN contracts c ON c.slug = st.entity_id \
         WHERE a.ts >= {since_clause} AND c.files_touched_json IS NOT NULL"
    );
    let rows = match crate::db_json::query_rows(db, &sql, &[]) {
        Ok(v) => v.as_array().cloned().unwrap_or_default(),
        Err(_) => return json!({ "files": [], "attributed_calls": 0, "note": "attribution query failed" }),
    };

    let mut order: Vec<String> = Vec::new();
    let mut by_file: HashMap<String, (f64, f64, f64, i64)> = HashMap::new(); // (cost, tin, tout, calls)
    let mut attributed: i64 = 0;

    for r in &rows {
        let files_col = r.get("files");
        let files_str: String = if truthy(files_col) {
            files_col.and_then(|v| v.as_str()).unwrap_or("[]").to_string()
        } else {
            "[]".to_string()
        };
        // JS `try { files = JSON.parse(r.files || '[]'); } catch { continue; }` — a parse error
        // skips the WHOLE row (not a fallback to []).
        let parsed: Value = match serde_json::from_str(&files_str) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let arr = match parsed { Value::Array(a) => a, _ => Vec::new() };
        let files: Vec<String> = arr
            .into_iter()
            .filter_map(|f| match f {
                Value::String(s) if !s.is_empty() && !s.starts_with('<') => Some(s),
                _ => None,
            })
            .collect();
        if files.is_empty() { continue; }
        attributed += 1;
        let n = files.len() as f64;
        let cost = js_number(r.get("cost")) / n;
        let tin = js_number(r.get("tin")) / n;
        let tout = js_number(r.get("tout")) / n;
        for f in &files {
            let rel = f.replace('\\', "/");
            if !by_file.contains_key(&rel) {
                order.push(rel.clone());
                by_file.insert(rel.clone(), (0.0, 0.0, 0.0, 0));
            }
            let acc = by_file.get_mut(&rel).unwrap();
            acc.0 += cost;
            acc.1 += tin;
            acc.2 += tout;
            acc.3 += 1;
        }
    }

    let mut files_out: Vec<Value> = order
        .iter()
        .map(|rel| {
            let (cost_usd, tin, tout, calls) = by_file[rel];
            json!({
                "path": rel,
                "cost_usd": to_fixed_num(cost_usd, 6),
                "tokens_in": js_round(tin) as i64,
                "tokens_out": js_round(tout) as i64,
                "tokens_total": js_round(tin + tout) as i64,
                "calls": calls,
            })
        })
        .collect();

    files_out.sort_by(|x, y| {
        let xc = x["cost_usd"].as_f64().unwrap_or(0.0);
        let yc = y["cost_usd"].as_f64().unwrap_or(0.0);
        yc.partial_cmp(&xc).unwrap_or(std::cmp::Ordering::Equal).then_with(|| {
            let xt = x["tokens_total"].as_i64().unwrap_or(0);
            let yt = y["tokens_total"].as_i64().unwrap_or(0);
            yt.cmp(&xt)
        })
    });

    if truthy(only_path) {
        let rel = js_string(only_path.unwrap()).replace('\\', "/");
        let found = files_out.iter().find(|f| f.get("path").and_then(|v| v.as_str()) == Some(rel.as_str())).cloned();
        return found.unwrap_or_else(|| {
            json!({ "path": rel, "cost_usd": 0, "tokens_in": 0, "tokens_out": 0, "tokens_total": 0, "calls": 0 })
        });
    }

    json!({ "files": files_out, "attributed_calls": attributed })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "ATTACH ':memory:' AS brain; \
             CREATE TABLE brain.ai_calls (id INTEGER PRIMARY KEY, ts TEXT, cost_usd_actual REAL, tokens_in INTEGER, tokens_out INTEGER, tokens_cached INTEGER, cached INTEGER, provider TEXT, tier TEXT, model_id TEXT, prompt_template_id INTEGER, parent_event_id INTEGER); \
             CREATE TABLE brain.prompt_templates (id INTEGER PRIMARY KEY, name TEXT); \
             CREATE TABLE cost_settings (key TEXT PRIMARY KEY, value TEXT); \
             CREATE TABLE sacred_timeline (id INTEGER PRIMARY KEY, entity_type TEXT, entity_id TEXT); \
             CREATE TABLE contracts (slug TEXT PRIMARY KEY, files_touched_json TEXT);",
        )
        .unwrap();
        c
    }

    #[allow(clippy::too_many_arguments)]
    fn call(
        c: &Connection, id: i64, ts: &str, cost: f64, tin: Option<i64>, tout: Option<i64>,
        tcached: i64, cached: i64, provider: Option<&str>, tier: Option<&str>, model_id: Option<&str>,
        prompt_template_id: Option<i64>, parent_event_id: Option<i64>,
    ) {
        c.execute(
            "INSERT INTO brain.ai_calls (id, ts, cost_usd_actual, tokens_in, tokens_out, tokens_cached, cached, provider, tier, model_id, prompt_template_id, parent_event_id) \
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
            rusqlite::params![id, ts, cost, tin, tout, tcached, cached, provider, tier, model_id, prompt_template_id, parent_event_id],
        )
        .unwrap();
    }
    fn tpl(c: &Connection, id: i64, name: &str) {
        c.execute("INSERT INTO brain.prompt_templates (id, name) VALUES (?1,?2)", rusqlite::params![id, name]).unwrap();
    }

    #[test]
    fn today_default_cap_top3_truncation_and_unknown_fallback() {
        let c = db();
        tpl(&c, 1, "dev.contract.load");
        tpl(&c, 2, "pl.plan.review");
        tpl(&c, 3, "qa.verify.acceptance");
        tpl(&c, 4, "man.audit.compliance");
        call(&c, 1, "2026-01-01 08:00:00", 1.0, Some(1000), Some(200), 100, 1, Some("anthropic"), Some("mid"), Some("m1"), Some(1), None);
        call(&c, 2, "2026-01-01 09:30:00", 3.0, Some(500), Some(100), 0, 0, Some("openrouter"), Some("high"), Some("m2"), Some(2), None);
        call(&c, 3, "2026-01-01 10:00:00", 0.5, Some(200), Some(50), 0, 0, Some("deepseek"), Some("low"), Some("m3"), Some(3), None);
        call(&c, 4, "2026-01-01 11:00:00", 2.0, Some(300), Some(80), 50, 1, Some("anthropic"), Some("mid"), Some("m1"), Some(4), None);
        call(&c, 5, "2026-01-01 12:00:00", 2.5, Some(100), Some(20), 0, 0, Some("openrouter"), Some("low"), Some("m4"), None, None);
        call(&c, 6, "2025-12-31 10:00:00", 4.0, Some(100), Some(20), 0, 0, None, None, None, Some(1), None);

        let r = costAggregate(&json!({ "view": "today" }), &c).unwrap();
        assert_eq!(r["spend_usd"], json!(9));
        assert_eq!(r["cap_usd"], json!(50));
        assert_eq!(r["cap_pct"], json!(18));
        assert_eq!(
            r["top_3_slugs"],
            json!([
                { "slug": "pl.plan.review", "cost_usd": 3, "calls": 1 },
                { "slug": "(unknown)", "cost_usd": 2.5, "calls": 1 },
                { "slug": "man.audit.compliance", "cost_usd": 2, "calls": 1 },
            ])
        );
        assert_eq!(r["yesterday_spend_usd"], json!(4));
        assert_eq!(r["trend"], json!("rising"));
        assert_eq!(r["calls_today"], json!(5));
        assert_eq!(r["tokens_in"], json!(2100));
        assert_eq!(r["tokens_out"], json!(450));
        assert_eq!(r["tokens_total"], json!(2550));
        assert_eq!(r["cached_calls"], json!(2));
        assert_eq!(r["cached_pct"], json!(40));
        assert_eq!(r["provider_cached_tokens"], json!(150));
        assert_eq!(r["provider_cached_pct"], json!(7));
        assert_eq!(r["estimated_usd"], json!(0.0653));
    }

    #[test]
    fn today_configured_cap_falling_trend() {
        let c = db();
        c.execute("INSERT INTO cost_settings (key, value) VALUES ('daily_cap_usd','10')", []).unwrap();
        call(&c, 1, "2026-01-01 08:00:00", 1.0, Some(100), Some(20), 0, 0, None, None, None, None, None);
        call(&c, 2, "2025-12-31 08:00:00", 5.0, Some(100), Some(20), 0, 0, None, None, None, None, None);
        let r = costAggregate(&json!({ "view": "today" }), &c).unwrap();
        assert_eq!(r["cap_usd"], json!(10));
        assert_eq!(r["cap_pct"], json!(10));
        assert_eq!(r["trend"], json!("falling"));
        assert_eq!(r["top_3_slugs"][0]["slug"], json!("(unknown)"));
    }

    #[test]
    fn today_empty_defaults() {
        let c = db();
        let r = costAggregate(&json!({ "view": "today" }), &c).unwrap();
        assert_eq!(
            r,
            json!({
                "spend_usd": 0, "cap_usd": 50, "cap_pct": 0, "top_3_slugs": [],
                "yesterday_spend_usd": 0, "trend": "flat", "calls_today": 0, "tokens_in": 0,
                "tokens_out": 0, "tokens_total": 0, "cached_calls": 0, "cached_pct": 0,
                "provider_cached_tokens": 0, "provider_cached_pct": 0, "estimated_usd": 0
            })
        );
    }

    #[test]
    fn sub_skill_matrix_groups_baseline_and_window() {
        let c = db();
        tpl(&c, 1, "dev.contract.load");
        tpl(&c, 2, "pl.plan.review");
        call(&c, 1, "2026-01-01 08:00:00", 1.0, Some(1000), Some(200), 0, 1, None, Some("mid"), Some("m1"), Some(1), None);
        call(&c, 2, "2026-01-01 09:00:00", 0.5, Some(500), Some(100), 0, 0, None, Some("mid"), Some("m1"), Some(1), None);
        call(&c, 3, "2026-01-01 10:00:00", 3.0, Some(2000), Some(750), 0, 0, None, Some("high"), Some("m2"), Some(2), None);
        call(&c, 4, "2025-12-30 10:00:00", 9.0, Some(999), Some(999), 0, 0, None, Some("high"), Some("m2"), Some(2), None);
        call(&c, 5, "2026-01-01 11:00:00", 0.1, Some(10), Some(5), 0, 0, None, Some("low"), Some("m5"), None, None);

        let r = sub_skill_matrix(&c, "datetime('2026-01-01 00:00:00', '-24 hours')");
        let rows = r.as_array().unwrap();
        assert_eq!(rows.len(), 3); // 2025-12-30 row excluded
        assert_eq!(
            rows[0],
            json!({ "prompt_name": "pl.plan.review", "tier": "high", "model_id": "m2", "calls": 1,
                "cost_actual_usd": 3, "cost_baseline_usd": 0.08625, "savings_usd": -2.91375,
                "cached_calls": 0, "cached_pct": 0, "tokens_in": 2000, "tokens_out": 750, "tokens_total": 2750 })
        );
        assert_eq!(
            rows[1],
            json!({ "prompt_name": "dev.contract.load", "tier": "mid", "model_id": "m1", "calls": 2,
                "cost_actual_usd": 1.5, "cost_baseline_usd": 0.045, "savings_usd": -1.455,
                "cached_calls": 1, "cached_pct": 50, "tokens_in": 1500, "tokens_out": 300, "tokens_total": 1800 })
        );
        assert_eq!(rows[2]["prompt_name"], json!("(unknown)"));
    }

    #[test]
    fn provider_mix_shares_and_unknown_fallback() {
        let c = db();
        call(&c, 1, "2026-01-01 08:00:00", 1.0, None, None, 0, 0, Some("anthropic"), None, None, None, None);
        call(&c, 2, "2026-01-01 09:00:00", 2.0, None, None, 0, 0, Some("anthropic"), None, None, None, None);
        call(&c, 3, "2026-01-01 10:00:00", 3.0, None, None, 0, 0, Some("openrouter"), None, None, None, None);
        call(&c, 4, "2026-01-01 10:30:00", 0.5, None, None, 0, 0, None, None, None, None, None);
        call(&c, 5, "2025-12-30 10:00:00", 99.0, None, None, 0, 0, Some("openrouter"), None, None, None, None);

        let r = provider_mix(&c, "datetime('2026-01-01 00:00:00', '-24 hours')");
        assert_eq!(
            r,
            json!([
                { "provider": "openrouter", "calls": 1, "cost_usd": 3, "share_pct": 46.15, "share_calls_pct": 25 },
                { "provider": "anthropic", "calls": 2, "cost_usd": 3, "share_pct": 46.15, "share_calls_pct": 50 },
                { "provider": "(unknown)", "calls": 1, "cost_usd": 0.5, "share_pct": 7.69, "share_calls_pct": 25 },
            ])
        );
    }

    #[test]
    fn agent_mix_prefix_bucketing_and_sort() {
        let c = db();
        tpl(&c, 1, "dev.contract.load");
        tpl(&c, 2, "dev.microtask.run");
        tpl(&c, 3, "pl.plan.review");
        tpl(&c, 4, "standalone");
        call(&c, 1, "2026-01-01 08:00:00", 1.0, None, None, 0, 0, None, None, None, Some(1), None);
        call(&c, 2, "2026-01-01 09:00:00", 2.0, None, None, 0, 0, None, None, None, Some(2), None);
        call(&c, 3, "2026-01-01 10:00:00", 0.5, None, None, 0, 0, None, None, None, Some(3), None);
        call(&c, 4, "2026-01-01 10:30:00", 4.0, None, None, 0, 0, None, None, None, Some(4), None);
        call(&c, 5, "2026-01-01 11:00:00", 0.25, None, None, 0, 0, None, None, None, None, None);

        let r = agent_mix(&c, "datetime('2026-01-01 00:00:00', '-24 hours')");
        assert_eq!(
            r,
            json!([
                { "agent": "standalone", "calls": 1, "cost_usd": 4, "share_pct": 51.61, "avg_cost_per_call_usd": 4 },
                { "agent": "dev", "calls": 2, "cost_usd": 3, "share_pct": 38.71, "avg_cost_per_call_usd": 1.5 },
                { "agent": "pl", "calls": 1, "cost_usd": 0.5, "share_pct": 6.45, "avg_cost_per_call_usd": 0.5 },
                { "agent": "system", "calls": 1, "cost_usd": 0.25, "share_pct": 3.23, "avg_cost_per_call_usd": 0.25 },
            ])
        );
    }

    #[test]
    fn file_attribution_splits_normalizes_and_filters() {
        let c = db();
        c.execute(
            "INSERT INTO contracts (slug, files_touched_json) VALUES ('CONTR-1', ?1)",
            rusqlite::params![serde_json::to_string(&json!(["src/a.js", "src\\nested\\b.js", "<generated>", "", 123])).unwrap()],
        )
        .unwrap();
        c.execute(
            "INSERT INTO contracts (slug, files_touched_json) VALUES ('CONTR-2', ?1)",
            rusqlite::params![serde_json::to_string(&json!(["src/a.js", "src/c.js"])).unwrap()],
        )
        .unwrap();
        c.execute("INSERT INTO contracts (slug, files_touched_json) VALUES ('CONTR-3', 'not-json{{')", []).unwrap();
        c.execute(
            "INSERT INTO contracts (slug, files_touched_json) VALUES ('CONTR-4', ?1)",
            rusqlite::params![serde_json::to_string(&json!({"not":"array"})).unwrap()],
        )
        .unwrap();
        c.execute("INSERT INTO sacred_timeline (id, entity_type, entity_id) VALUES (101,'contract','CONTR-1')", []).unwrap();
        c.execute("INSERT INTO sacred_timeline (id, entity_type, entity_id) VALUES (102,'contract','CONTR-2')", []).unwrap();
        c.execute("INSERT INTO sacred_timeline (id, entity_type, entity_id) VALUES (103,'contract','CONTR-3')", []).unwrap();
        c.execute("INSERT INTO sacred_timeline (id, entity_type, entity_id) VALUES (104,'contract','CONTR-4')", []).unwrap();
        c.execute("INSERT INTO sacred_timeline (id, entity_type, entity_id) VALUES (105,'card','CONTR-1')", []).unwrap();
        call(&c, 1, "2026-01-01 08:00:00", 1.0, Some(400), Some(100), 0, 0, None, None, None, None, Some(101));
        call(&c, 2, "2026-01-01 09:00:00", 3.0, Some(600), Some(200), 0, 0, None, None, None, None, Some(102));
        call(&c, 3, "2026-01-01 10:00:00", 5.0, Some(10), Some(10), 0, 0, None, None, None, None, Some(103));
        call(&c, 4, "2026-01-01 10:30:00", 7.0, Some(10), Some(10), 0, 0, None, None, None, None, Some(104));
        call(&c, 5, "2026-01-01 11:00:00", 9.0, Some(10), Some(10), 0, 0, None, None, None, None, Some(999));
        call(&c, 6, "2025-12-30 08:00:00", 100.0, Some(10), Some(10), 0, 0, None, None, None, None, Some(101));

        let since = "datetime('2026-01-01 00:00:00', '-24 hours')";
        let r = file_attribution(&c, since, None);
        assert_eq!(r["attributed_calls"], json!(2));
        assert_eq!(
            r["files"],
            json!([
                { "path": "src/a.js", "cost_usd": 2, "tokens_in": 500, "tokens_out": 150, "tokens_total": 650, "calls": 2 },
                { "path": "src/c.js", "cost_usd": 1.5, "tokens_in": 300, "tokens_out": 100, "tokens_total": 400, "calls": 1 },
                { "path": "src/nested/b.js", "cost_usd": 0.5, "tokens_in": 200, "tokens_out": 50, "tokens_total": 250, "calls": 1 },
            ])
        );

        let hit = file_attribution(&c, since, Some(&json!("src/a.js")));
        assert_eq!(hit, json!({ "path": "src/a.js", "cost_usd": 2, "tokens_in": 500, "tokens_out": 150, "tokens_total": 650, "calls": 2 }));

        let miss = file_attribution(&c, since, Some(&json!("src/does-not-exist.js")));
        assert_eq!(miss, json!({ "path": "src/does-not-exist.js", "cost_usd": 0, "tokens_in": 0, "tokens_out": 0, "tokens_total": 0, "calls": 0 }));

        let backslash = file_attribution(&c, since, Some(&json!("src\\nested\\b.js")));
        assert_eq!(backslash["path"], json!("src/nested/b.js"));
    }

    #[test]
    fn window_hours_override_narrows_cutoff() {
        let c = db();
        call(&c, 1, "2026-01-01 00:30:00", 1.0, None, None, 0, 0, Some("p1"), None, None, None, None);
        call(&c, 2, "2025-12-31 22:00:00", 5.0, None, None, 0, 0, Some("p1"), None, None, None, None);
        let one_hour = costAggregate(&json!({ "view": "provider_mix", "window_hours": 1 }), &c).unwrap();
        assert_eq!(one_hour, json!([{ "provider": "p1", "calls": 1, "cost_usd": 1, "share_pct": 100, "share_calls_pct": 100 }]));
        let default_window = costAggregate(&json!({ "view": "provider_mix" }), &c).unwrap();
        assert_eq!(default_window, json!([{ "provider": "p1", "calls": 2, "cost_usd": 6, "share_pct": 100, "share_calls_pct": 100 }]));
    }
}
