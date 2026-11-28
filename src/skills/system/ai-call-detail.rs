//! ai-call-detail.rs — twin of skills/system/ai-call-detail.mjs (db READ lane, no mutations).
//! NAMES ARE CONTRACT: `aiCallDetail` (default export). Detail mode (`ai_call_id` truthy) renders
//! one AI interaction record in full; list mode (otherwise) returns a filtered/paginated rowset.
//!
//! Composes 3 already-converted twins verbatim (no reimplementation):
//!   `crate::ai_ai_call::reconstructRenderedPrompt`            — pure, read-time prompt rebuild
//!   `crate::entities_agent_persona_resolve::resolvePersona`   — db-first, subagent -> persona chain
//!   `crate::codebase_context_bundle_get::contextBundleGet`    — db cross-schema bundle read
//!
//! `context.db` is always injected by the db lane in production, so the JS `if (!db) throw
//! err.contextMissing(...)` guard is unreachable here (Rust's `db: &Connection` is never absent) —
//! same convention as feature-inventory.rs / benchmark-status.rs.
//!
//! `days_from_civil`/`parse_sql_ts_ms` (SQLite `'YYYY-MM-DD HH:MM:SS'` cell -> UTC epoch ms, mirrors
//! JS `new Date(s.replace(' ','T')+'Z').getTime()`) are copied verbatim from benchmark-status.rs —
//! used only by the swarm-aggregate wall-clock span (`Math.max(...ends) - Math.min(...starts)`).
//!
//! The heimdall block mirrors the JS's SINGLE try/catch precisely: a throw while reading
//! heimdall_castings resets the whole block to null (and skips the human_tag read entirely,
//! since the JS throw never reaches that statement); a throw while reading heimdall_human_tags
//! (after castings already succeeded) is swallowed and the casting-only result survives — these
//! are NOT symmetric, so they are ported as two separate phases, not one flattened `?` chain.
use rusqlite::Connection;
use serde_json::{json, Map, Value};

// Standard helper functions (playbook §Standard helper functions) — copied verbatim (only the
// ones this module actually uses).
fn truthy(v: Option<&Value>) -> bool { match v { None|Some(Value::Null)=>false, Some(Value::Bool(b))=>*b, Some(Value::Number(n))=>n.as_f64().map_or(false,|f|f!=0.0&&!f.is_nan()), Some(Value::String(s))=>!s.is_empty(), Some(_)=>true } }
fn str_qq(v: Option<&Value>) -> String { match v { None|Some(Value::Null)=>String::new(), Some(Value::String(s))=>s.clone(), Some(Value::Bool(b))=>b.to_string(), Some(n@Value::Number(_))=>n.to_string(), Some(o)=>o.to_string() } } // = JS String(v ?? '')
fn js_string(v:&Value)->String{ match v{Value::Null=>"null".into(),Value::String(s)=>s.clone(),Value::Number(n)=>n.to_string(),Value::Bool(b)=>b.to_string(),Value::Array(a)=>a.iter().map(js_string).collect::<Vec<_>>().join(","),Value::Object(_)=>"[object Object]".into()} } // = JS String(v)
fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) } // JS `??` right operand: nz(a).or(nz(b))
fn js_number(v: Option<&Value>) -> f64 { match v { None=>f64::NAN, Some(Value::Null)=>0.0, Some(Value::Bool(b))=>if *b {1.0} else {0.0}, Some(Value::Number(n))=>n.as_f64().unwrap_or(f64::NAN), Some(Value::String(s))=>{let t=s.trim(); if t.is_empty(){0.0}else{t.parse().unwrap_or(f64::NAN)}}, Some(_)=>f64::NAN } } // = JS Number(v)

// days since 1970-01-01 for a proleptic-Gregorian y/m/d (m in 1..=12) — Howard Hinnant's algorithm.
// Copied verbatim from benchmark-status.rs.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = (if y >= 0 { y } else { y - 399 }) / 400;
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

// Parse a 'YYYY-MM-DD HH:MM:SS' sqlite datetime cell as a UTC epoch-ms instant — mirrors JS
// `new Date(s.replace(' ','T') + 'Z').getTime()`. Malformed input -> None. Copied verbatim from
// benchmark-status.rs.
fn parse_sql_ts_ms(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    if b.len() < 19 || b[4] != b'-' || b[7] != b'-' || (b[10] != b'T' && b[10] != b' ') || b[13] != b':' || b[16] != b':' {
        return None;
    }
    let num = |lo: usize, hi: usize| s.get(lo..hi)?.parse::<i64>().ok();
    let y = num(0, 4)?;
    let mo = num(5, 7)?;
    let d = num(8, 10)?;
    let hh = num(11, 13)?;
    let mm = num(14, 16)?;
    let ss = num(17, 19)?;
    Some((((days_from_civil(y, mo, d) * 24 + hh) * 60 + mm) * 60 + ss) * 1000)
}

// JS: `function safeParse(json) { if (json == null) return null; try { return JSON.parse(json); } catch { return null; } }`
fn safe_parse(v: Option<&Value>) -> Value {
    let s = match v {
        None | Some(Value::Null) => return Value::Null,
        Some(Value::String(s)) => s.clone(),
        Some(o) => js_string(o), // JSON.parse coerces its argument via String()
    };
    serde_json::from_str::<Value>(&s).unwrap_or(Value::Null)
}

// JS: `(prompt_name ?? '').split('.')[0] || 'unknown'` — shared by list()'s per-row map and
// detail()'s `call.agent`.
fn derive_agent(prompt_name: Option<&Value>) -> String {
    let s = str_qq(prompt_name);
    let first = s.split('.').next().unwrap_or("");
    if first.is_empty() { "unknown".to_string() } else { first.to_string() }
}

fn sum_i64(rows: &[Value], k: &str) -> i64 { rows.iter().map(|r| js_number(r.get(k)) as i64).sum() }
fn sum_f64(rows: &[Value], k: &str) -> f64 { rows.iter().map(|r| js_number(r.get(k))).sum() }
// JS `tsMs`: `String(t).replace(' ', 'T') + 'Z'` then `Date.parse(...)`. `parse_sql_ts_ms` accepts
// the raw ' ' separator directly (no literal replace needed) and returns the same instant.
fn row_ts_ms(t: Option<&Value>) -> Option<f64> {
    match t { Some(Value::String(s)) => parse_sql_ts_ms(s).map(|ms| ms as f64), _ => None }
}

const CASTING_SQL: &str = "SELECT id, ai_call_id, category, template_base, requested_tier, effective_model, reason, trace_json, ts FROM brain.heimdall_castings WHERE ai_call_id = ? ORDER BY id DESC LIMIT 1";

fn read_casting_for(db: &Connection, id: &Value) -> Result<Option<Value>, String> {
    let r = crate::db_json::query_rows(db, CASTING_SQL, &[id.clone()])?;
    Ok(r.as_array().and_then(|a| a.first()).cloned())
}

// The casting-only half of the heimdall block — a throw anywhere in here (JS: the `readCasting`
// prepare/get calls) means the WHOLE heimdall block resets to null in detail(), matching the JS
// try/catch. Found-nothing (Ok(Value::Null)) is NOT an error — the tag phase still runs.
fn casting_phase(db: &Connection, call_id_v: &Value, src_id: &Value) -> Result<Value, String> {
    let c = match read_casting_for(db, call_id_v)? {
        Some(v) => Some(v),
        None => if src_id != call_id_v { read_casting_for(db, src_id)? } else { None },
    };
    Ok(match c {
        Some(c) => json!({
            "casting_id": c.get("id").cloned().unwrap_or(Value::Null),
            "from_call_id": c.get("ai_call_id").cloned().unwrap_or(Value::Null),
            "category": c.get("category").cloned().unwrap_or(Value::Null),
            "template_base": c.get("template_base").cloned().unwrap_or(Value::Null),
            "requested_tier": c.get("requested_tier").cloned().unwrap_or(Value::Null),
            "effective_model": c.get("effective_model").cloned().unwrap_or(Value::Null),
            "reason": c.get("reason").cloned().unwrap_or(Value::Null),
            "ts": c.get("ts").cloned().unwrap_or(Value::Null),
            "trace": safe_parse(c.get("trace_json")),
        }),
        None => Value::Null,
    })
}

// JS: the whole `if (call.session_id) { ... }` try block — a throw anywhere resets `swarm` to its
// initial `null` (single assignment at the very end in JS, so unwrap_or(Null) here is exact, no
// partial-preservation nuance like the heimdall block has).
fn swarm_phase(db: &Connection, call: &Value) -> Result<Value, String> {
    let session_id = call.get("session_id");
    if !truthy(session_id) {
        return Ok(Value::Null);
    }
    let rows_v = crate::db_json::query_rows(
        db,
        "SELECT id, ts, tier, model_id, outcome, cached, COALESCE(tokens_in, 0) AS tokens_in, COALESCE(tokens_out, 0) AS tokens_out, COALESCE(tokens_cached, 0) AS tokens_cached, COALESCE(cost_usd_actual, 0) AS cost_usd, COALESCE(duration_ms, 0) AS duration_ms FROM brain.ai_calls WHERE session_id = ? ORDER BY id",
        &[session_id.unwrap().clone()],
    )?;
    let rows: Vec<Value> = rows_v.as_array().cloned().unwrap_or_default();
    if rows.len() <= 1 {
        return Ok(Value::Null);
    }

    let starts: Vec<f64> = rows.iter().filter_map(|r| row_ts_ms(r.get("ts"))).collect();
    let ends: Vec<f64> = rows.iter().filter_map(|r| row_ts_ms(r.get("ts")).map(|s| s + js_number(r.get("duration_ms")))).collect();
    let wall_ms: Value = if !starts.is_empty() && !ends.is_empty() {
        let max_end = ends.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
        let min_start = starts.iter().cloned().fold(f64::INFINITY, f64::min);
        json!(max_end - min_start)
    } else {
        Value::Null
    };
    let success = rows.iter().filter(|r| r.get("outcome").and_then(Value::as_str) == Some("success") && !truthy(r.get("cached"))).count() as i64;
    let cached_n = rows.iter().filter(|r| truthy(r.get("cached"))).count() as i64;
    let failed = rows.iter().filter(|r| r.get("outcome").and_then(Value::as_str) != Some("success")).count() as i64;
    let tokens_in_sum = sum_i64(&rows, "tokens_in");
    let tokens_out_sum = sum_i64(&rows, "tokens_out");
    let tokens_cached_sum = sum_i64(&rows, "tokens_cached");
    let cost_usd_sum = sum_f64(&rows, "cost_usd");
    let duration_ms_sum = sum_f64(&rows, "duration_ms");
    let calls_len = rows.len() as i64;
    let session_id_v = session_id.unwrap().clone();

    Ok(json!({
        "session_id": session_id_v,
        "calls": rows,
        "totals": {
            "calls": calls_len,
            "tokens_in": tokens_in_sum,
            "tokens_out": tokens_out_sum,
            "tokens_cached": tokens_cached_sum,
            "cost_usd": cost_usd_sum,
            "compute_ms": duration_ms_sum,
            "wall_ms": wall_ms,
            "success": success,
            "cached": cached_n,
            "failed": failed,
        },
    }))
}

pub fn aiCallDetail(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS: `if (!db) throw err.contextMissing('ai-call-detail', 'db')` — unreachable here, the db
    // lane always injects a real Connection (same convention as feature-inventory.rs et al).
    let ai_call_id = input.get("ai_call_id").cloned();
    let agent = input.get("agent").cloned();
    let tier = input.get("tier").cloned();
    let prompt_name = input.get("prompt_name").cloned();
    let since_ts = input.get("since_ts").cloned();
    let contains = input.get("contains").cloned();
    // JS destructuring default `limit = 50` fires only when the key is ABSENT (undefined), not
    // when it is explicitly `null` — mirrored via `None` vs `Some(Value::Null)`.
    let limit = match input.get("limit") {
        None => json!(50),
        Some(v) => v.clone(),
    };

    if truthy(ai_call_id.as_ref()) {
        return detail(db, &ai_call_id.unwrap());
    }
    list(db, agent.as_ref(), tier.as_ref(), prompt_name.as_ref(), since_ts.as_ref(), contains.as_ref(), &limit)
}

fn list(
    db: &Connection,
    agent: Option<&Value>,
    tier: Option<&Value>,
    prompt_name: Option<&Value>,
    since_ts: Option<&Value>,
    contains: Option<&Value>,
    limit: &Value,
) -> Result<Value, String> {
    let mut wh: Vec<&str> = Vec::new();
    let mut params: Vec<Value> = Vec::new();
    if truthy(agent) { wh.push("p.name LIKE ? || '.%'"); params.push(agent.unwrap().clone()); }
    if truthy(tier) { wh.push("a.tier = ?"); params.push(tier.unwrap().clone()); }
    if truthy(prompt_name) { wh.push("p.name LIKE '%' || ? || '%'"); params.push(prompt_name.unwrap().clone()); }
    if truthy(since_ts) { wh.push("a.ts >= ?"); params.push(since_ts.unwrap().clone()); }
    let contains_clause = "(EXISTS (SELECT 1 FROM ai_call_rendered ar WHERE ar.ai_call_id = a.id AND ar.rendered_prompt_md LIKE '%' || ? || '%') OR EXISTS (SELECT 1 FROM ai_call_outputs ao WHERE ao.ai_call_id = a.id AND ao.value_text LIKE '%' || ? || '%'))";
    if truthy(contains) {
        wh.push(contains_clause);
        params.push(contains.unwrap().clone());
        params.push(contains.unwrap().clone());
    }
    let where_sql = if wh.is_empty() { String::new() } else { format!("WHERE {}", wh.join(" AND ")) };
    let sql = format!("SELECT a.id, a.ts, p.name AS prompt_name, p.version AS prompt_version, a.tier, a.temperature, a.model_id, a.provider, a.tokens_in, a.tokens_out, a.cost_usd_actual, a.cost_usd_actual AS cost_usd, a.duration_ms, a.outcome, a.cached FROM brain.ai_calls a LEFT JOIN brain.prompt_templates p ON p.id = a.prompt_template_id {where_sql} ORDER BY a.id DESC LIMIT ?");
    params.push(limit.clone());

    let rows: Vec<Value> = match crate::db_json::query_rows(db, &sql, &params) {
        Ok(r) => r.as_array().cloned().unwrap_or_default(),
        Err(e) => {
            let msg: String = e.chars().take(200).collect();
            return Ok(json!({ "error": msg, "rows": [] }));
        }
    };
    // JS: `rows.map((r) => ({ ...r, agent: ... }))` — `agent` is not an existing column, so the
    // spread-then-set always APPENDS it as the row's last key (gotcha #1).
    let out: Vec<Value> = rows
        .into_iter()
        .map(|r| {
            let mut m = r.as_object().cloned().unwrap_or_default();
            let agent_str = derive_agent(m.get("prompt_name"));
            m.insert("agent".to_string(), json!(agent_str));
            Value::Object(m)
        })
        .collect();
    let count = out.len() as i64;
    Ok(json!({ "rows": out, "count": count }))
}

fn detail(db: &Connection, ai_call_id: &Value) -> Result<Value, String> {
    let call_rows = crate::db_json::query_rows(
        db,
        "SELECT a.*, p.name AS prompt_name, p.version AS prompt_version, p.template_md, p.system_md FROM brain.ai_calls a LEFT JOIN brain.prompt_templates p ON p.id = a.prompt_template_id WHERE a.id = ?",
        &[ai_call_id.clone()],
    )?;
    let call = match call_rows.as_array().and_then(|a| a.first()) {
        Some(c) => c.clone(),
        None => return Ok(json!({ "error": "ai_call not found", "id": ai_call_id.clone() })),
    };
    let call_id_v = call.get("id").cloned().unwrap_or(Value::Null);

    // Cache hits carry no inputs/outputs of their own — follow the source row (ai_calls.cache_source_call_id).
    let cached_truthy = truthy(call.get("cached"));
    let cache_source = call.get("cache_source_call_id");
    let src_id: Value = if cached_truthy && truthy(cache_source) {
        cache_source.unwrap().clone()
    } else {
        call_id_v.clone()
    };

    let context_bundle: Value = crate::codebase_context_bundle_get::contextBundleGet(&json!({ "ai_call_id": src_id.clone() }), db)
        .ok()
        .filter(|cb| truthy(cb.get("found")))
        .map(|cb| cb.get("bundle").cloned().unwrap_or(Value::Null))
        .unwrap_or(Value::Null);

    let inputs: Vec<Value> = crate::db_json::query_rows(
        db,
        "SELECT key, value_text, order_index FROM brain.ai_call_inputs WHERE ai_call_id = ? ORDER BY order_index, id",
        &[src_id.clone()],
    )?
    .as_array()
    .cloned()
    .unwrap_or_default();
    let outputs: Vec<Value> = crate::db_json::query_rows(
        db,
        "SELECT id, field_name, value_text, value_number, value_boolean, order_index, parent_output_id FROM brain.ai_call_outputs WHERE ai_call_id = ? ORDER BY parent_output_id IS NOT NULL, order_index, id",
        &[src_id.clone()],
    )?
    .as_array()
    .cloned()
    .unwrap_or_default();

    // Rendered prompt: prefer the persisted row for call.id, else for srcId, else reconstruct.
    const RENDERED_SQL: &str = "SELECT rendered_prompt_md, system_message, request_meta_json FROM brain.ai_call_rendered WHERE ai_call_id = ?";
    let rendered: Option<Value> = {
        let r1 = crate::db_json::query_rows(db, RENDERED_SQL, &[call_id_v.clone()])?.as_array().and_then(|a| a.first()).cloned();
        match r1 {
            Some(v) => Some(v),
            None => crate::db_json::query_rows(db, RENDERED_SQL, &[src_id.clone()])?.as_array().and_then(|a| a.first()).cloned(),
        }
    };

    let input_kv_for_render: Vec<Value> = inputs
        .iter()
        .map(|i| json!({ "key": i.get("key").cloned().unwrap_or(Value::Null), "value": i.get("value_text").cloned().unwrap_or(Value::Null) }))
        .collect();
    let input_kv_for_render_v = Value::Array(input_kv_for_render);

    let rendered_prompt_md = rendered.as_ref().and_then(|r| r.get("rendered_prompt_md"));
    let rendered_prompt: Value = match nz(rendered_prompt_md) {
        Some(v) => v.clone(),
        None => {
            let tmpl_md = call.get("template_md");
            if truthy(tmpl_md) {
                crate::ai_ai_call::reconstructRenderedPrompt(tmpl_md.unwrap(), &input_kv_for_render_v)
            } else {
                Value::Null
            }
        }
    };
    let rendered_source: Value = if rendered.is_some() {
        json!("persisted")
    } else if truthy(Some(&rendered_prompt)) {
        json!("reconstructed")
    } else {
        Value::Null
    };

    let attempts: Vec<Value> = match crate::db_json::query_rows(
        db,
        "SELECT attempt_no, ts, outcome, correction_md, raw_output_md, validation_errors_json FROM brain.ai_call_attempts WHERE ai_call_id = ? ORDER BY attempt_no",
        &[src_id.clone()],
    ) {
        Ok(r) => r
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .map(|a| {
                let mut m = a.as_object().cloned().unwrap_or_default();
                let ve = safe_parse(m.get("validation_errors_json"));
                m.insert("validation_errors".to_string(), ve);
                Value::Object(m)
            })
            .collect(),
        Err(_) => Vec::new(),
    };

    let mut pair_format_ids: Vec<Value> = Vec::new();
    if let Ok(r) = crate::db_json::query_rows(db, "SELECT id FROM brain.ai_calls WHERE reason_call_id = ? ORDER BY id", &[call_id_v.clone()]) {
        pair_format_ids = r.as_array().cloned().unwrap_or_default().into_iter().map(|row| row.get("id").cloned().unwrap_or(Value::Null)).collect();
    }
    let pair = json!({
        "reason_call_id": call.get("reason_call_id").cloned().unwrap_or(Value::Null),
        "format_call_ids": pair_format_ids,
    });

    let report: Value = match crate::db_json::query_rows(
        db,
        "SELECT id, ts, trigger, failure_class, severity, diagnosis_md, narrative_md, recommendation_md FROM brain.ai_call_reports WHERE ai_call_id = ? ORDER BY id DESC LIMIT 1",
        &[src_id.clone()],
    ) {
        Ok(r) => r.as_array().and_then(|a| a.first()).cloned().unwrap_or(Value::Null),
        Err(_) => Value::Null,
    };

    // Failure surfaces: last failed attempt's structured errors; raw output from the legacy
    // sentinel row (else the last failed attempt's raw output).
    let last_failed: Option<&Value> = attempts.iter().rev().find(|a| a.get("outcome").and_then(Value::as_str) != Some("success"));
    let sentinel: Option<&Value> = outputs.iter().find(|o| o.get("field_name").and_then(Value::as_str) == Some("__raw_output_on_validation_fail__"));
    let validation_errors: Value = nz(last_failed.and_then(|a| a.get("validation_errors"))).cloned().unwrap_or(Value::Null);
    let raw_output_on_failure: Value = nz(sentinel.and_then(|s| s.get("value_text")))
        .or_else(|| nz(last_failed.and_then(|a| a.get("raw_output_md"))))
        .cloned()
        .unwrap_or(Value::Null);

    let retry_attempts: Vec<Value> = match crate::db_json::query_rows(
        db,
        "SELECT id, ts, outcome, tier, COALESCE(cost_usd_actual, 0) AS cost_usd, COALESCE(duration_ms, 0) AS duration_ms FROM brain.ai_calls WHERE prompt_template_id = ? AND input_hash = ? AND id != ? ORDER BY id DESC LIMIT 10",
        &[call.get("prompt_template_id").cloned().unwrap_or(Value::Null), call.get("input_hash").cloned().unwrap_or(Value::Null), call_id_v.clone()],
    ) {
        Ok(r) => r.as_array().cloned().unwrap_or_default(),
        Err(_) => Vec::new(),
    };

    // Sub-agent leaf: re-resolve the PERSONA it ran under (not stored per-call). Falls back to
    // the template's static system_md (already the default below).
    let mut persona_system_md: Value = nz(call.get("system_md")).cloned().unwrap_or(Value::Null);
    let subagent = call.get("subagent");
    if truthy(subagent) {
        if let Some(sub_str) = subagent.and_then(Value::as_str) {
            let pr = crate::entities_agent_persona_resolve::resolvePersona(db, sub_str);
            if truthy(pr.get("system_md")) {
                persona_system_md = pr.get("system_md").cloned().unwrap_or(Value::Null);
            }
        }
    }

    // HEIMDALL: casting trace for this call (cache hits fall back to the source's casting) plus
    // the operator's latest tag. See module doc for why this is two phases, not one `?` chain.
    let casting_result = casting_phase(db, &call_id_v, &src_id);
    let heimdall_val: Value = match casting_result {
        Err(_) => Value::Null,
        Ok(mut hv) => {
            if let Ok(tag_rows) = crate::db_json::query_rows(
                db,
                "SELECT id, verdict, reason_md, ts FROM brain.heimdall_human_tags WHERE ai_call_id = ? ORDER BY id DESC LIMIT 1",
                &[call_id_v.clone()],
            ) {
                if let Some(tag) = tag_rows.as_array().and_then(|a| a.first()).cloned() {
                    let mut base = if hv.is_object() { hv.as_object().cloned().unwrap() } else { Map::new() };
                    base.insert(
                        "human_tag".to_string(),
                        json!({
                            "id": tag.get("id").cloned().unwrap_or(Value::Null),
                            "verdict": tag.get("verdict").cloned().unwrap_or(Value::Null),
                            "reason_md": tag.get("reason_md").cloned().unwrap_or(Value::Null),
                            "ts": tag.get("ts").cloned().unwrap_or(Value::Null),
                        }),
                    );
                    hv = Value::Object(base);
                }
            }
            hv
        }
    };

    // THE SWARM: sibling calls riding the same session_id (>1 row only).
    let swarm = swarm_phase(db, &call).unwrap_or(Value::Null);

    let call_out = json!({
        "id": call.get("id").cloned().unwrap_or(Value::Null),
        "ts": call.get("ts").cloned().unwrap_or(Value::Null),
        "prompt_name": call.get("prompt_name").cloned().unwrap_or(Value::Null),
        "prompt_version": call.get("prompt_version").cloned().unwrap_or(Value::Null),
        "tier": call.get("tier").cloned().unwrap_or(Value::Null),
        "temperature": call.get("temperature").cloned().unwrap_or(Value::Null),
        "provider": call.get("provider").cloned().unwrap_or(Value::Null),
        "model_id": call.get("model_id").cloned().unwrap_or(Value::Null),
        "tokens_in": call.get("tokens_in").cloned().unwrap_or(Value::Null),
        "tokens_out": call.get("tokens_out").cloned().unwrap_or(Value::Null),
        "cost_usd": nz(call.get("cost_usd_actual")).cloned().unwrap_or(json!(0)),
        "duration_ms": nz(call.get("duration_ms")).cloned().unwrap_or(json!(0)),
        "outcome": call.get("outcome").cloned().unwrap_or(Value::Null),
        "cached": truthy(call.get("cached")),
        "outcome_class": call.get("outcome_class").cloned().unwrap_or(Value::Null),
        "cached_from_id": call.get("cache_source_call_id").cloned().unwrap_or(Value::Null),
        "reason_call_id": call.get("reason_call_id").cloned().unwrap_or(Value::Null),
        "subagent": call.get("subagent").cloned().unwrap_or(Value::Null),
        "session_id": call.get("session_id").cloned().unwrap_or(Value::Null),
        "agent": derive_agent(call.get("prompt_name")),
    });

    let outputs_v = Value::Array(outputs);
    Ok(json!({
        "call": call_out,
        "swarm": swarm,
        "heimdall": heimdall_val,
        "template_md": call.get("template_md").cloned().unwrap_or(Value::Null),
        "system_md": persona_system_md,
        "rendered_prompt": rendered_prompt,
        "rendered_source": rendered_source,
        "input_kv": input_kv_for_render_v,
        "output_kv": outputs_v.clone(),
        "context_bundle": context_bundle,
        "validation_errors": validation_errors,
        "raw_output_on_failure": raw_output_on_failure,
        "attempts": attempts,
        "retry_attempts": retry_attempts,
        "pair": pair,
        "report": report,
        "inputs": inputs,
        "rendered": rendered.unwrap_or(Value::Null),
        "outputs": outputs_v,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "ATTACH ':memory:' AS brain;
            CREATE TABLE brain.prompt_templates ( id INTEGER PRIMARY KEY, name TEXT NOT NULL, version INTEGER NOT NULL, template_md TEXT NOT NULL, default_tier TEXT NOT NULL, default_temperature TEXT NOT NULL, max_output_tokens INTEGER NOT NULL DEFAULT 1024, superseded_by INTEGER, created_at TEXT NOT NULL, cross_field_rules_json TEXT, judging_criteria_json TEXT, system_md TEXT, phase_flow TEXT NOT NULL DEFAULT 'single' );
            CREATE TABLE brain.ai_calls ( id INTEGER PRIMARY KEY, ts TEXT NOT NULL, prompt_template_id INTEGER NOT NULL, tier TEXT NOT NULL, temperature TEXT NOT NULL, provider TEXT, model_id TEXT, input_hash TEXT NOT NULL, duration_ms REAL, cost_usd_actual REAL, tokens_in INTEGER, tokens_out INTEGER, tokens_cached INTEGER NOT NULL DEFAULT 0, cached INTEGER NOT NULL DEFAULT 0, cache_source_call_id INTEGER, parent_event_id INTEGER, outcome TEXT NOT NULL, replay_of_id INTEGER, envelope_json TEXT, reason_call_id INTEGER, session_id TEXT, openrouter_generation_id TEXT, subagent TEXT, contract_slug TEXT, reasoning_md TEXT, outcome_class TEXT );
            CREATE TABLE brain.ai_call_outputs ( id INTEGER PRIMARY KEY, ai_call_id INTEGER NOT NULL, field_name TEXT NOT NULL, value_text TEXT, value_number REAL, value_boolean INTEGER, order_index INTEGER NOT NULL DEFAULT 0, parent_output_id INTEGER );
            CREATE TABLE brain.ai_call_inputs ( id INTEGER PRIMARY KEY, ai_call_id INTEGER NOT NULL, key TEXT NOT NULL, value_text TEXT, order_index INTEGER NOT NULL DEFAULT 0 );
            CREATE TABLE brain.ai_call_rendered ( ai_call_id INTEGER PRIMARY KEY, rendered_prompt_md TEXT NOT NULL, system_message TEXT, request_meta_json TEXT );
            CREATE TABLE brain.context_bundles ( ai_call_id INTEGER PRIMARY KEY, agent TEXT, phase TEXT, bone_tokens INTEGER NOT NULL DEFAULT 0, sliced_tokens INTEGER NOT NULL DEFAULT 0, total_tokens INTEGER NOT NULL DEFAULT 0, budget INTEGER, over_budget INTEGER NOT NULL DEFAULT 0, items_json TEXT, dropped_json TEXT, skipped_json TEXT, created_at TEXT NOT NULL );
            CREATE TABLE brain.ai_call_attempts ( id INTEGER PRIMARY KEY, ai_call_id INTEGER NOT NULL, attempt_no INTEGER NOT NULL, ts TEXT NOT NULL, outcome TEXT NOT NULL, correction_md TEXT, raw_output_md TEXT, validation_errors_json TEXT );
            CREATE TABLE brain.ai_call_reports ( id INTEGER PRIMARY KEY, ts TEXT NOT NULL, ai_call_id INTEGER, prompt_template_id INTEGER, prompt_name TEXT, prompt_version INTEGER, tier TEXT, model_id TEXT, related_action TEXT, related_contract_slug TEXT, related_card_slug TEXT, trigger TEXT NOT NULL, failure_class TEXT NOT NULL, severity TEXT NOT NULL DEFAULT 'warn', evidence_json TEXT NOT NULL, diagnosis_md TEXT NOT NULL, narrative_md TEXT, recommendation_md TEXT, retry_recovered INTEGER NOT NULL DEFAULT 0, created_by TEXT NOT NULL DEFAULT 'ai-call-postmortem' );
            CREATE TABLE brain.heimdall_castings ( id INTEGER PRIMARY KEY, ai_call_id INTEGER, template_base TEXT, category TEXT NOT NULL, requested_tier TEXT NOT NULL, effective_model TEXT, reason TEXT NOT NULL, trace_json TEXT NOT NULL, ts TEXT NOT NULL );
            CREATE TABLE brain.heimdall_human_tags ( id INTEGER PRIMARY KEY, verdict INTEGER NOT NULL, reason_md TEXT, surface TEXT NOT NULL, ai_call_id INTEGER, card_slug TEXT, message_ref TEXT, tagged_by TEXT NOT NULL DEFAULT 'operator', ts TEXT NOT NULL );
            CREATE TABLE agents ( name TEXT PRIMARY KEY, description TEXT, context_json TEXT, custom_prompt_md TEXT, specialization_md TEXT, parent_agent TEXT, is_abstract INTEGER );",
        )
        .unwrap();
        c
    }

    fn call_fn(input: Value, c: &Connection) -> Value {
        aiCallDetail(&input, c).unwrap()
    }

    #[test]
    fn list_no_filters_orders_desc_and_derives_agent_and_dual_cost_columns() {
        let c = db();
        c.execute_batch(
            "INSERT INTO brain.prompt_templates (id,name,version,template_md,default_tier,default_temperature,created_at) VALUES
              (1,'qa.gate.verify',1,'Check: {{x}}','low','cold','2025-12-01 00:00:00'),
              (2,'pl.plan.compose',1,'Plan: {{x}}','mid','warm','2025-12-01 00:00:00');
            INSERT INTO brain.ai_calls (id,ts,prompt_template_id,tier,temperature,provider,model_id,input_hash,duration_ms,cost_usd_actual,tokens_in,tokens_out,outcome,cached) VALUES
              (1,'2025-12-20 10:00:00',1,'low','cold','anthropic','m1','h1',120.0,0.002,10,5,'success',0),
              (2,'2025-12-21 10:00:00',2,'mid','warm','anthropic','m2','h2',340.0,0.01,20,8,'success',0),
              (3,'2025-12-22 10:00:00',1,'low','cold','anthropic','m1','h3',90.0,0.001,8,4,'error',0);",
        )
        .unwrap();
        let r = call_fn(json!({}), &c);
        assert_eq!(r["count"], json!(3));
        let ids: Vec<i64> = r["rows"].as_array().unwrap().iter().map(|x| x["id"].as_i64().unwrap()).collect();
        assert_eq!(ids, vec![3, 2, 1]);
        assert_eq!(r["rows"][0]["agent"], json!("qa"));
        assert_eq!(r["rows"][1]["agent"], json!("pl"));
        assert_eq!(r["rows"][2]["cost_usd_actual"], json!(0.002));
        assert_eq!(r["rows"][2]["cost_usd"], json!(0.002));
    }

    #[test]
    fn list_filters_combine_with_and() {
        let c = db();
        c.execute_batch(
            "INSERT INTO brain.prompt_templates (id,name,version,template_md,default_tier,default_temperature,created_at) VALUES
              (1,'qa.gate.verify',1,'x','low','cold','2025-12-01 00:00:00'),
              (2,'qa.gate.other',1,'x','mid','warm','2025-12-01 00:00:00'),
              (3,'pl.plan.compose',1,'x','low','cold','2025-12-01 00:00:00');
            INSERT INTO brain.ai_calls (id,ts,prompt_template_id,tier,temperature,input_hash,outcome) VALUES
              (1,'2025-12-25 00:00:00',1,'low','cold','h1','success'),
              (2,'2025-12-25 00:00:00',2,'mid','warm','h2','success'),
              (3,'2025-12-25 00:00:00',3,'low','cold','h3','success'),
              (4,'2025-12-10 00:00:00',1,'low','cold','h4','success');",
        )
        .unwrap();
        let r = call_fn(json!({ "agent": "qa", "tier": "low", "prompt_name": "gate", "since_ts": "2025-12-20 00:00:00" }), &c);
        let ids: Vec<i64> = r["rows"].as_array().unwrap().iter().map(|x| x["id"].as_i64().unwrap()).collect();
        assert_eq!(ids, vec![1]);
    }

    #[test]
    fn list_contains_matches_rendered_or_output_and_limit_caps() {
        let c = db();
        c.execute_batch(
            "INSERT INTO brain.prompt_templates (id,name,version,template_md,default_tier,default_temperature,created_at) VALUES (1,'qa.gate.verify',1,'x','low','cold','2025-12-01 00:00:00');
            INSERT INTO brain.ai_calls (id,ts,prompt_template_id,tier,temperature,input_hash,outcome) VALUES
              (1,'2025-12-01 00:00:00',1,'low','cold','h1','success'),
              (2,'2025-12-02 00:00:00',1,'low','cold','h2','success'),
              (3,'2025-12-03 00:00:00',1,'low','cold','h3','success');
            INSERT INTO brain.ai_call_rendered (ai_call_id,rendered_prompt_md) VALUES (2,'contains UNIQUETOKEN here');
            INSERT INTO brain.ai_call_outputs (ai_call_id,field_name,value_text,order_index) VALUES (3,'summary','has UNIQUETOKEN inside',0);",
        )
        .unwrap();
        let r = call_fn(json!({ "contains": "UNIQUETOKEN", "limit": 1 }), &c);
        // 2 rows match (id 2 via rendered, id 3 via output) but LIMIT 1 + ORDER BY id DESC keeps only id 3.
        assert_eq!(r["rows"].as_array().unwrap().len(), 1);
        assert_eq!(r["rows"][0]["id"], json!(3));
    }

    #[test]
    fn detail_not_found_returns_error_shape() {
        let c = db();
        let r = call_fn(json!({ "ai_call_id": 999 }), &c);
        assert_eq!(r, json!({ "error": "ai_call not found", "id": 999 }));
    }

    #[test]
    fn detail_baseline_reconstructs_prompt_defaults_zero_and_reads_context_and_heimdall() {
        let c = db();
        c.execute_batch(
            "INSERT INTO brain.prompt_templates (id,name,version,template_md,system_md,default_tier,default_temperature,created_at)
              VALUES (1,'detail.check',3,'Question: {{q}}
Answer well.','You are terse.','low','freezed','2025-12-01 00:00:00');
            INSERT INTO brain.ai_calls (id,ts,prompt_template_id,tier,temperature,provider,model_id,input_hash,tokens_in,tokens_out,outcome,cached)
              VALUES (10,'2025-12-28 09:00:00',1,'low','freezed','anthropic','claude','hh1',12,6,'success',0);
            INSERT INTO brain.ai_call_inputs (ai_call_id,key,value_text,order_index) VALUES (10,'q','why is the sky blue?',0);
            INSERT INTO brain.ai_call_outputs (ai_call_id,field_name,value_text,order_index) VALUES (10,'summary','a valid summary here',0);
            INSERT INTO brain.context_bundles (ai_call_id,agent,phase,bone_tokens,sliced_tokens,total_tokens,budget,over_budget,items_json,dropped_json,skipped_json,created_at)
              VALUES (10,'interviewer','chat',100,200,300,1000,0,'[]','[]','[]','2025-12-28 09:00:00');
            INSERT INTO brain.heimdall_castings (id,ai_call_id,template_base,category,requested_tier,effective_model,reason,trace_json,ts)
              VALUES (1,10,'detail','chat','low','claude-a','observe','{\"steps\":[1,2,3]}','2025-12-28 09:00:01');
            INSERT INTO brain.heimdall_human_tags (id,verdict,reason_md,surface,ai_call_id,tagged_by,ts)
              VALUES (1,1,'looks good','chat',10,'operator','2025-12-28 09:05:00');",
        )
        .unwrap();
        let r = call_fn(json!({ "ai_call_id": 10 }), &c);
        assert_eq!(r["rendered_prompt"], json!("Question: why is the sky blue?\nAnswer well."));
        assert_eq!(r["rendered_source"], json!("reconstructed"));
        assert_eq!(r["system_md"], json!("You are terse."));
        assert_eq!(r["call"]["cost_usd"], json!(0));
        assert_eq!(r["call"]["duration_ms"], json!(0));
        assert_eq!(r["call"]["cached"], json!(false));
        assert_eq!(r["call"]["agent"], json!("detail"));
        assert_eq!(r["context_bundle"]["agent"], json!("interviewer"));
        assert_eq!(r["heimdall"]["casting_id"], json!(1));
        assert_eq!(r["heimdall"]["trace"], json!({"steps":[1,2,3]}));
        assert_eq!(r["heimdall"]["human_tag"]["verdict"], json!(1));
        assert_eq!(r["attempts"], json!([]));
        assert_eq!(r["report"], Value::Null);
        assert_eq!(r["swarm"], Value::Null);
    }

    #[test]
    fn detail_cached_follows_source_for_inputs_rendered_and_heimdall_fallback() {
        let c = db();
        c.execute_batch(
            "INSERT INTO brain.prompt_templates (id,name,version,template_md,system_md,default_tier,default_temperature,created_at)
              VALUES (1,'detail.check',1,'Q: {{q}}','sys','low','cold','2025-12-01 00:00:00');
            INSERT INTO brain.ai_calls (id,ts,prompt_template_id,tier,temperature,input_hash,outcome,cached,tokens_in,tokens_out,duration_ms,cost_usd_actual)
              VALUES (20,'2025-12-29 08:00:00',1,'low','cold','hc1','success',0,5,5,50.0,0.001);
            INSERT INTO brain.ai_calls (id,ts,prompt_template_id,tier,temperature,input_hash,outcome,cached,cache_source_call_id,tokens_in,tokens_out,duration_ms,cost_usd_actual)
              VALUES (21,'2025-12-29 08:05:00',1,'low','cold','hc1','success',1,20,5,5,1.0,0.0);
            INSERT INTO brain.ai_call_inputs (ai_call_id,key,value_text,order_index) VALUES (20,'q','cache me',0);
            INSERT INTO brain.ai_call_outputs (ai_call_id,field_name,value_text,order_index) VALUES (20,'summary','cached answer',0);
            INSERT INTO brain.ai_call_rendered (ai_call_id,rendered_prompt_md,system_message) VALUES (20,'Q: cache me [PERSISTED]','sys');
            INSERT INTO brain.heimdall_castings (id,ai_call_id,template_base,category,requested_tier,effective_model,reason,trace_json,ts)
              VALUES (5,20,'detail','chat','low','claude-a','observe','{\"a\":1}','2025-12-29 08:00:01');",
        )
        .unwrap();
        let r = call_fn(json!({ "ai_call_id": 21 }), &c);
        assert_eq!(r["call"]["cached"], json!(true));
        assert_eq!(r["call"]["cached_from_id"], json!(20));
        assert_eq!(r["input_kv"], json!([{"key":"q","value":"cache me"}]));
        assert_eq!(r["rendered_prompt"], json!("Q: cache me [PERSISTED]"));
        assert_eq!(r["rendered_source"], json!("persisted"));
        assert_eq!(r["heimdall"]["casting_id"], json!(5));
        assert_eq!(r["heimdall"]["from_call_id"], json!(20));
    }

    #[test]
    fn detail_validation_failed_surfaces_sentinel_over_last_attempt_and_report() {
        let c = db();
        c.execute_batch(
            "INSERT INTO brain.prompt_templates (id,name,version,template_md,default_tier,default_temperature,created_at)
              VALUES (1,'qa.check',1,'Verify: {{x}}','mid','cold','2025-12-01 00:00:00');
            INSERT INTO brain.ai_calls (id,ts,prompt_template_id,tier,temperature,input_hash,outcome,tokens_in,tokens_out)
              VALUES (30,'2025-12-30 12:00:00',1,'mid','cold','hv1','validation_failed',9,3);
            INSERT INTO brain.ai_call_attempts (id,ai_call_id,attempt_no,ts,outcome,correction_md,raw_output_md,validation_errors_json) VALUES
              (1,30,1,'2025-12-30 12:00:00','validation_failed',NULL,'first raw output','[\"field x too short\"]'),
              (2,30,2,'2025-12-30 12:00:05','validation_failed','please fix','second raw output','[\"field x still too short\"]');
            INSERT INTO brain.ai_call_outputs (id,ai_call_id,field_name,value_text,order_index) VALUES (1,30,'__raw_output_on_validation_fail__','SENTINEL raw text',0);
            INSERT INTO brain.ai_call_reports (id,ts,ai_call_id,prompt_template_id,prompt_name,prompt_version,tier,model_id,trigger,failure_class,severity,evidence_json,diagnosis_md,created_by)
              VALUES (1,'2025-12-30 12:01:00',30,1,'qa.check',1,'mid','m','ai_call_validation','format_noncompliance','warn','{}','diag text','ai-call-postmortem');",
        )
        .unwrap();
        let r = call_fn(json!({ "ai_call_id": 30 }), &c);
        assert_eq!(r["validation_errors"], json!(["field x still too short"]));
        assert_eq!(r["raw_output_on_failure"], json!("SENTINEL raw text"));
        assert_eq!(r["attempts"].as_array().unwrap().len(), 2);
        assert_eq!(r["report"]["diagnosis_md"], json!("diag text"));
    }

    #[test]
    fn detail_pair_navigation_both_directions() {
        let c = db();
        c.execute_batch(
            "INSERT INTO brain.prompt_templates (id,name,version,template_md,default_tier,default_temperature,created_at) VALUES
              (1,'pair.reason',1,'Think: {{q}}','high','warm','2025-12-01 00:00:00'),
              (2,'pair.format',1,'{{reasoning_output}}
Format: {{q}}','mid','cold','2025-12-01 00:00:00');
            INSERT INTO brain.ai_calls (id,ts,prompt_template_id,tier,temperature,input_hash,outcome) VALUES
              (40,'2025-12-31 10:00:00',1,'high','warm','hr1','success');
            INSERT INTO brain.ai_calls (id,ts,prompt_template_id,tier,temperature,input_hash,outcome,reason_call_id) VALUES
              (41,'2025-12-31 10:00:05',2,'mid','cold','hf1','success',40);",
        )
        .unwrap();
        let reason_side = call_fn(json!({ "ai_call_id": 40 }), &c);
        assert_eq!(reason_side["pair"]["reason_call_id"], Value::Null);
        assert_eq!(reason_side["pair"]["format_call_ids"], json!([41]));
        let format_side = call_fn(json!({ "ai_call_id": 41 }), &c);
        assert_eq!(format_side["call"]["reason_call_id"], json!(40));
        assert_eq!(format_side["pair"]["reason_call_id"], json!(40));
        assert_eq!(format_side["pair"]["format_call_ids"], json!([]));
    }

    #[test]
    fn detail_subagent_resolves_persona_over_template_system_md() {
        let c = db();
        c.execute_batch(
            "INSERT INTO brain.prompt_templates (id,name,version,template_md,system_md,default_tier,default_temperature,created_at)
              VALUES (1,'swarm.leaf',1,'Do: {{x}}','template-level system','mid','cold','2025-12-01 00:00:00');
            INSERT INTO agents (name,context_json,parent_agent,specialization_md) VALUES
              ('base','{\"role\":\"base reviewer\"}',NULL,NULL),
              ('base.qa.lawyer','{}','base','You argue the CORRECT interpretation.');
            INSERT INTO brain.ai_calls (id,ts,prompt_template_id,tier,temperature,input_hash,outcome,subagent)
              VALUES (50,'2026-01-01 00:00:00',1,'mid','cold','hs1','success','base.qa.lawyer');",
        )
        .unwrap();
        let r = call_fn(json!({ "ai_call_id": 50 }), &c);
        let sys = r["system_md"].as_str().unwrap();
        assert!(sys.contains("base reviewer"));
        assert!(sys.contains("argue the CORRECT interpretation"));
        assert!(!sys.contains("template-level system"));
    }

    #[test]
    fn detail_swarm_aggregates_and_solo_session_stays_null() {
        let c = db();
        c.execute_batch(
            "INSERT INTO brain.prompt_templates (id,name,version,template_md,default_tier,default_temperature,created_at)
              VALUES (1,'swarm.step',1,'Step: {{x}}','low','cold','2025-12-01 00:00:00');
            INSERT INTO brain.ai_calls (id,ts,prompt_template_id,tier,temperature,input_hash,outcome,session_id,cached,tokens_in,tokens_out,tokens_cached,cost_usd_actual,duration_ms) VALUES
              (60,'2026-01-01 00:00:10',1,'low','cold','hw1','success','sess-A',0,10,5,2,1.0,100.0),
              (61,'2026-01-01 00:00:12',1,'low','cold','hw2','success','sess-A',1,10,5,0,2.0,200.0),
              (62,'2026-01-01 00:00:20',1,'low','cold','hw3','error','sess-A',0,3,0,0,3.0,300.0);
            INSERT INTO brain.ai_calls (id,ts,prompt_template_id,tier,temperature,input_hash,outcome,session_id) VALUES
              (70,'2026-01-01 00:00:00',1,'low','cold','hs1','success','sess-solo');",
        )
        .unwrap();
        let r = call_fn(json!({ "ai_call_id": 60 }), &c);
        let totals = &r["swarm"]["totals"];
        assert_eq!(totals["calls"], json!(3));
        assert_eq!(totals["tokens_in"], json!(23));
        assert_eq!(totals["tokens_out"], json!(10));
        assert_eq!(totals["tokens_cached"], json!(2));
        assert_eq!(totals["cost_usd"], json!(6.0));
        assert_eq!(totals["compute_ms"], json!(600.0));
        assert_eq!(totals["success"], json!(1));
        assert_eq!(totals["cached"], json!(1));
        assert_eq!(totals["failed"], json!(1));
        // starts=[10000,12000,20000]ms-of-minute; ends=[10100,12200,20300]; wall = 20300-10000.
        assert_eq!(totals["wall_ms"], json!(10300.0));

        let solo = call_fn(json!({ "ai_call_id": 70 }), &c);
        assert_eq!(solo["swarm"], Value::Null);
    }
}
