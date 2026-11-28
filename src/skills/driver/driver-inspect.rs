//! driver-inspect.rs — twin of skills/driver/driver-inspect.mjs (db READ lane, cross-db
//! `brain.driver_ticks`/`brain.ai_calls`/`brain.ai_call_outputs`/`brain.prompt_templates`).
//! NAMES ARE CONTRACT: `driverInspect` (default). Pure read-only forensic readout of one driver
//! tick: header + state-machine reason + a recursive `runtime_events` tree + windowed `ai_calls`
//! (with parsed outputs) + `sacred_timeline` mutations in the tick's time window + (for
//! `git-manager.implement.code` ticks) a re-derived DEV bundle via the already-converted
//! `agents_dev_dev_bundle_slice::devBundleSlice` twin. Output is a structured object PLUS a
//! pre-rendered text transcript (`render`); `render_only:true` returns just the joined string.
//!
//! `context.db` missing → JS throws `err.contextMissing('driver-inspect', 'db')`. Unreachable
//! here: `db: &Connection` is a required, non-nullable reference (same convention every sibling
//! db-lane twin uses).
//!
//! `tick` not found → JS throws `err.notFound('driver-inspect', 'driver_tick', String(tick_id))`
//! (`"driver-inspect: driver_tick '<id>' not found"`) — a guard-throw, not exercised by the
//! fixture (playbook gotcha 9: fixture only the success path), but modeled faithfully as `Err`.
//!
//! `sacred_timeline` is queried UNQUALIFIED by the JS source (no `timeline.` prefix), same
//! convention as the already-converted `audit_verify_chain::verifyChain` twin — the fixture
//! defines `sacred_timeline` directly in the main schema (no `timeline` ATTACH needed), unlike
//! `system_card_process_events`, whose JS source qualifies it `timeline.sacred_timeline`.
//!
//! DEV bundle branch (`tick.decided_action === 'git-manager.implement.code'`): real code, not
//! stubbed — calls the already-converted `devBundleSlice` twin with a real `contract` object
//! (spread + `files_touched`/`acceptance_json` overridden, mirroring the JS object-literal
//! semantics: `files_touched` is a NEW key appended at the end, `acceptance_json` an EXISTING key
//! whose VALUE updates in place). `devBundleSlice` itself does real fs I/O when `files_touched`
//! is non-empty (see its own module doc) — this twin's fixture only ever seeds
//! `files_touched_json: '[]'` for its dev-bundle-branch cases, so the fs-touching path inside
//! `devBundleSlice` is never entered here (no testdata dir needed for THIS unit); the real fs
//! code in `devBundleSlice` remains exercised by ITS OWN fixture/tests. `JSON.parse` failures on
//! `files_touched_json`/`acceptance_json` (malformed TEXT) propagate through the JS `try/catch`
//! in `tryDeriveBundle` as `{skipped:true, reason:'bundle derivation failed: ...'}` — modeled the
//! same way, though not fixture-exercised (a malformed-JSON contract row is a data-corruption
//! edge case, not a code branch worth seeding deliberately here).
//!
//! `PRAGMA table_info(contracts)` (migration-048 `goal_md` detection) ported literally — same
//! technique as `contract_contract_add`.
// QUALITY: 78/100 — recursive CTE event tree; ai_calls union-by-id dedup (event-linked ∪
// window-fallback); nested ai_call_outputs (parent+children); transcript render mirrors every
// JS branch (facts/contract/qa/events/ai_calls/audit/recent_ticks/dev-bundle/goal_md)

use regex::Regex;
use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::collections::HashMap;

fn truthy(v: Option<&Value>) -> bool { match v { None|Some(Value::Null)=>false, Some(Value::Bool(b))=>*b, Some(Value::Number(n))=>n.as_f64().map_or(false,|f|f!=0.0&&!f.is_nan()), Some(Value::String(s))=>!s.is_empty(), Some(_)=>true } }
fn str_qq(v: Option<&Value>) -> String { match v { None|Some(Value::Null)=>String::new(), Some(Value::String(s))=>s.clone(), Some(Value::Bool(b))=>b.to_string(), Some(n@Value::Number(_))=>n.to_string(), Some(o)=>o.to_string() } }
fn tmpl(v: Option<&Value>) -> String { match v { None=>"undefined".into(), Some(Value::Null)=>"null".into(), Some(x)=>str_qq(Some(x)) } }
fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) }
fn js_number(v: Option<&Value>) -> f64 { match v { None=>f64::NAN, Some(Value::Null)=>0.0, Some(Value::Bool(b))=>if *b {1.0} else {0.0}, Some(Value::Number(n))=>n.as_f64().unwrap_or(f64::NAN), Some(Value::String(s))=>{let t=s.trim(); if t.is_empty(){0.0}else{t.parse().unwrap_or(f64::NAN)}}, Some(_)=>f64::NAN } }
fn js_string(v: &Value) -> String { match v { Value::Null=>"null".into(), Value::String(s)=>s.clone(), Value::Number(n)=>n.to_string(), Value::Bool(b)=>b.to_string(), Value::Array(a)=>a.iter().map(js_string).collect::<Vec<_>>().join(","), Value::Object(_)=>"[object Object]".into() } }

// = JS `.length` (UTF-16 code units) / `.slice(0,n)` (chars-based approximation — exact for the
// ASCII fixture content this unit renders).
fn js_len(s: &str) -> usize { s.encode_utf16().count() }
fn js_slice(s: &str, n: usize) -> String { s.chars().take(n).collect() }

fn g(row: &Value, key: &str) -> Value { row.get(key).cloned().unwrap_or(Value::Null) }
// `${x ?? default}` where `default` is a plain string literal.
fn nz_str(v: Option<&Value>, default: &str) -> String { match nz(v) { Some(x) => tmpl(Some(x)), None => default.to_string() } }
// JS `Math.round` (ties toward +Infinity) — used only where the JS source explicitly rounds
// (events/ai_calls duration_ms), never on tick.duration_ms (which the JS renders unrounded).
fn js_math_round(x: f64) -> i64 { (x + 0.5).floor() as i64 }
fn to_fixed(x: f64, d: usize) -> String { format!("{:.*}", d, x) }
// integer-valued Number(x) param → INTEGER SQL param (mirrors node:sqlite's int/float param
// binding so `id = ?` comparisons against an INTEGER PK behave identically).
fn num_param(n: f64) -> Value { if n.is_finite() && n.fract() == 0.0 && n.abs() < 9e15 { json!(n as i64) } else { json!(n) } }

fn load_tick(db: &Connection, tick_id: &Value) -> Result<Option<Value>, String> {
    let is_latest = tick_id.as_str() == Some("latest") || tick_id.is_null();
    let rows = if is_latest {
        crate::db_json::query_rows(db, "SELECT * FROM brain.driver_ticks ORDER BY id DESC LIMIT 1", &[])?
    } else {
        crate::db_json::query_rows(db, "SELECT * FROM brain.driver_ticks WHERE id = ?", &[num_param(js_number(Some(tick_id)))])?
    };
    Ok(rows.as_array().and_then(|a| a.first()).cloned())
}

fn load_driver_events(db: &Connection, tick: &Value) -> Result<Vec<Value>, String> {
    let started = g(tick, "started_at");
    let ended = nz(tick.get("ended_at")).cloned().unwrap_or_else(|| started.clone());
    let rows = crate::db_json::query_rows(db, "
        SELECT id, ts, actor, action, skill, outcome, error_code, duration_ms, context_json, parent_event_id
        FROM runtime_events
        WHERE actor LIKE 'driver:%'
          AND parent_event_id IS NULL
          AND ts >= datetime(?, '-2 seconds')
          AND ts <= datetime(?, '+2 seconds')
        ORDER BY id
    ", &[started, ended])?;
    Ok(rows.as_array().cloned().unwrap_or_default())
}

fn descend_event_tree(db: &Connection, root_ids: &[Value]) -> Result<Vec<Value>, String> {
    if root_ids.is_empty() { return Ok(vec![]); }
    let placeholders = vec!["?"; root_ids.len()].join(",");
    let sql = format!("
        WITH RECURSIVE tree(id, depth) AS (
          SELECT id, 0 FROM runtime_events WHERE id IN ({placeholders})
          UNION ALL
          SELECT re.id, t.depth + 1
          FROM runtime_events re JOIN tree t ON re.parent_event_id = t.id
        )
        SELECT re.id, re.parent_event_id, re.action, re.skill, re.outcome,
               re.error_code, re.duration_ms, re.ts, t.depth
        FROM tree t JOIN runtime_events re ON re.id = t.id
        ORDER BY re.id
    ");
    let rows = crate::db_json::query_rows(db, &sql, root_ids)?;
    Ok(rows.as_array().cloned().unwrap_or_default())
}

fn load_ai_calls(db: &Connection, event_ids: &[Value], started: &Value, ended: &Value) -> Result<Vec<Value>, String> {
    let cols = "id, ts, prompt_template_id, tier, temperature, provider, model_id, outcome, tokens_in, tokens_out, duration_ms, cost_usd_actual, cached, parent_event_id";
    let by_event: Vec<Value> = if !event_ids.is_empty() {
        let placeholders = vec!["?"; event_ids.len()].join(",");
        let sql = format!("SELECT {cols} FROM brain.ai_calls WHERE parent_event_id IN ({placeholders})");
        crate::db_json::query_rows(db, &sql, event_ids)?.as_array().cloned().unwrap_or_default()
    } else { vec![] };
    let by_window = crate::db_json::query_rows(
        db,
        &format!("SELECT {cols} FROM brain.ai_calls WHERE ts >= datetime(?, '-2 seconds') AND ts <= datetime(?, '+2 seconds')"),
        &[started.clone(), ended.clone()],
    )?.as_array().cloned().unwrap_or_default();
    // JS `new Map(); for (r of [...byEvent, ...byWindow]) merged.set(r.id, r);` — later entry
    // wins on a duplicate id (harmless: same underlying row), final order is re-sorted below
    // anyway so the Map's insertion order never needs to be replicated.
    let mut merged: HashMap<i64, Value> = HashMap::new();
    for r in by_event.into_iter().chain(by_window.into_iter()) {
        merged.insert(r.get("id").and_then(Value::as_i64).unwrap_or(0), r);
    }
    let mut out: Vec<Value> = merged.into_values().collect();
    out.sort_by_key(|r| r.get("id").and_then(Value::as_i64).unwrap_or(0));
    Ok(out)
}

fn load_ai_outputs(db: &Connection, ai_call_id: &Value) -> Result<Vec<Value>, String> {
    let parents = crate::db_json::query_rows(db, "
        SELECT id, field_name, value_text, value_number, value_boolean, order_index, parent_output_id
        FROM brain.ai_call_outputs WHERE ai_call_id = ? AND parent_output_id IS NULL
        ORDER BY order_index, id
    ", &[ai_call_id.clone()])?.as_array().cloned().unwrap_or_default();
    let mut out: Vec<Value> = Vec::new();
    for p in parents {
        let pid = g(&p, "id");
        let children = crate::db_json::query_rows(db, "
            SELECT id, field_name, value_text, value_number, value_boolean, order_index
            FROM brain.ai_call_outputs WHERE ai_call_id = ? AND parent_output_id = ?
            ORDER BY order_index, id
        ", &[ai_call_id.clone(), pid])?;
        let mut obj = p.as_object().cloned().unwrap_or_default();
        obj.insert("children".into(), children); // new key → appended (mirrors JS `{...p, children}`)
        out.push(Value::Object(obj));
    }
    Ok(out)
}

fn load_prompt_name(db: &Connection, prompt_template_id: &Value) -> Result<Option<Value>, String> {
    let rows = crate::db_json::query_rows(db, "SELECT name, version FROM brain.prompt_templates WHERE id = ?", &[prompt_template_id.clone()])?;
    Ok(rows.as_array().and_then(|a| a.first()).cloned())
}

fn load_audit_rows(db: &Connection, tick: &Value) -> Result<Vec<Value>, String> {
    let started = g(tick, "started_at");
    let ended = nz(tick.get("ended_at")).cloned().unwrap_or_else(|| started.clone());
    let rows = crate::db_json::query_rows(db, "
        SELECT id, ts, actor, operation, entity_type, entity_id
        FROM sacred_timeline
        WHERE ts >= datetime(?, '-2 seconds') AND ts <= datetime(?, '+2 seconds')
          AND actor LIKE 'driver:%'
        ORDER BY id
    ", &[started, ended])?;
    Ok(rows.as_array().cloned().unwrap_or_default())
}

// Two distinct no-value paths, and JSON.stringify tells them apart (gotcha #3): falsy card_slug
// is an explicit JS `return null` → Some(Null) → serializes as "contract":null; a present slug
// with no matching row is `.get()` → `undefined` → None → the key is omitted entirely.
fn load_active_contract(db: &Connection, card_slug: &Value) -> Result<Option<Value>, String> {
    if !truthy(Some(card_slug)) { return Ok(Some(Value::Null)); }
    let pragma = crate::db_json::query_rows(db, "PRAGMA table_info(contracts)", &[])?;
    let has_goal_md = pragma.as_array()
        .map(|a| a.iter().any(|c| c.get("name").and_then(Value::as_str) == Some("goal_md")))
        .unwrap_or(false);
    let goal_col = if has_goal_md { ", goal_md" } else { "" };
    let sql = format!(
        "SELECT slug, title, status, tier, signed_by, signed_at, sandbox_path, created_at, updated_at, \
         current_attempt, files_touched_json, briefing_md, acceptance_json{goal_col} \
         FROM contracts WHERE card_slug = ? AND superseded_by IS NULL ORDER BY id DESC LIMIT 1"
    );
    let rows = crate::db_json::query_rows(db, &sql, &[card_slug.clone()])?;
    Ok(rows.as_array().and_then(|a| a.first()).cloned())
}

fn load_last_qa_runs(db: &Connection, contract_slug: &Value, limit: i64) -> Vec<Value> {
    if !truthy(Some(contract_slug)) { return vec![]; }
    match crate::db_json::query_rows(db, "
        SELECT id, ts, all_passed, scope_passed, acceptance_pass, acceptance_fail
        FROM qa_runs WHERE contract_slug = ? ORDER BY id DESC LIMIT ?
    ", &[contract_slug.clone(), json!(limit)]) {
        Ok(v) => v.as_array().cloned().unwrap_or_default(),
        Err(_) => vec![], // JS try/catch: table missing (pre-migration DB) → []
    }
}

fn load_recent_ticks_for_card(db: &Connection, card_slug: &Value, limit: i64) -> Result<Vec<Value>, String> {
    if !truthy(Some(card_slug)) { return Ok(vec![]); }
    let rows = crate::db_json::query_rows(db, "
        SELECT id, started_at, decided_action, outcome, duration_ms, error_message
        FROM brain.driver_ticks WHERE card_slug = ? ORDER BY id DESC LIMIT ?
    ", &[card_slug.clone(), json!(limit)])?;
    Ok(rows.as_array().cloned().unwrap_or_default())
}

/// `tryDeriveBundle(db, tick)`. Real branch, GAP disclosed in the module doc: only exercised by
/// this unit's fixture on the `files_touched_json: '[]'` path (no fs reads reached).
fn try_derive_bundle(db: &Connection, tick: &Value) -> Option<Value> {
    if tick.get("decided_action").and_then(Value::as_str) != Some("git-manager.implement.code") {
        return None;
    }
    let attempt: Result<Value, String> = (|| {
        let contract = load_active_contract(db, &g(tick, "card_slug"))?;
        let contract = match contract {
            // JS `if (!contract)`: null (falsy slug) and undefined (no row) both skip.
            Some(c) if !c.is_null() => c,
            _ => return Ok(json!({ "skipped": true, "reason": "no active contract found for card" })),
        };
        let files_touched: Value = match contract.get("files_touched_json").and_then(Value::as_str) {
            None => json!([]),
            Some(s) => serde_json::from_str(s).map_err(|e| e.to_string())?,
        };
        let acceptance_json: Value = match contract.get("acceptance_json").and_then(Value::as_str) {
            None => json!([]),
            Some(s) => serde_json::from_str(s).map_err(|e| e.to_string())?,
        };
        // `{...contract, files_touched: X, acceptance_json: Y}`: files_touched is a NEW key
        // (appended), acceptance_json an EXISTING key (value updated in place) — key order is
        // never observed downstream (devBundleSlice reads named fields, never serializes the
        // whole contract), so a plain overwrite is faithful without replicating JS insertion order.
        let mut parsed = contract.as_object().cloned().unwrap_or_default();
        parsed.insert("acceptance_json".into(), acceptance_json);
        parsed.insert("files_touched".into(), files_touched);
        let b = crate::agents_dev_dev_bundle_slice::devBundleSlice(
            &json!({ "contract": Value::Object(parsed), "microtask": Value::Null }),
            db,
        )?;
        Ok(json!({
            "ok": true,
            "total_bytes": g(&b, "total_bytes"),
            "files": g(&b, "files"),
            "keywords_used": g(&b, "keywords_used"),
            "bundle_md": g(&b, "bundle_md"),
        }))
    })();
    match attempt {
        Ok(v) => Some(v),
        Err(e) => Some(json!({ "skipped": true, "reason": format!("bundle derivation failed: {e}") })),
    }
}

fn tick_decided_action_is_dev(tick: &Value) -> bool {
    tick.get("decided_action").and_then(Value::as_str) == Some("git-manager.implement.code")
}

fn wrap_chunks(s: &str) -> Vec<String> {
    let re = Regex::new(r".{1,110}").unwrap();
    let found: Vec<String> = re.find_iter(s).map(|m| m.as_str().to_string()).collect();
    if found.is_empty() { vec![s.to_string()] } else { found }
}

fn render_transcript(data: &Value) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let t = data.get("tick").cloned().unwrap_or(Value::Null);
    let events = data.get("events").and_then(Value::as_array).cloned().unwrap_or_default();

    // Deepest error_code: last non-null error_code in array order (events sorted by re.id ASC).
    let deepest_error_code: Option<Value> = events.iter().rev().find_map(|e| e.get("error_code").filter(|v| !v.is_null()).cloned());

    lines.push(format!("═══ Tick {} ═══", tmpl(t.get("id"))));
    lines.push(format!("  started:  {}", tmpl(t.get("started_at"))));
    lines.push(format!("  ended:    {}", nz_str(t.get("ended_at"), "(running)")));
    lines.push(format!("  card:     {}", nz_str(t.get("card_slug"), "(no card)")));
    lines.push(format!("  decision: {}   by: {}", nz_str(t.get("decided_action"), "(none)"), nz_str(t.get("decided_agent"), "-")));
    lines.push(format!("  reason:   {}", nz_str(t.get("decided_reason"), "-")));
    let outcome_suffix = if truthy(deepest_error_code.as_ref()) { format!(" ({})", tmpl(deepest_error_code.as_ref())) } else { String::new() };
    lines.push(format!("  outcome:  {}{}", tmpl(t.get("outcome")), outcome_suffix));
    let duration_txt = match nz(t.get("duration_ms")) { Some(d) => format!("{}ms", tmpl(Some(d))), None => "-".to_string() };
    let cost_v = nz(t.get("cost_usd")).cloned().unwrap_or(json!(0));
    lines.push(format!("  duration: {}    cost: ${}", duration_txt, to_fixed(js_number(Some(&cost_v)), 4)));

    let error_message = t.get("error_message").cloned().unwrap_or(Value::Null);
    if truthy(Some(&error_message)) {
        lines.push("  error:".to_string());
        let msg = js_slice(error_message.as_str().unwrap_or(""), 800);
        for seg in wrap_chunks(&msg) { lines.push(format!("    {seg}")); }
    }
    lines.push(String::new());

    let facts = data.get("facts").cloned().unwrap_or(Value::Null);
    if truthy(Some(&facts)) && (facts.is_object() || facts.is_array()) {
        lines.push("═══ Decision facts ═══".to_string());
        let entries: Vec<(String, Value)> = match &facts {
            Value::Object(m) => m.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
            Value::Array(a) => a.iter().enumerate().map(|(i, v)| (i.to_string(), v.clone())).collect(),
            _ => vec![],
        };
        for (k, v) in entries.iter().filter(|(_, v)| v.is_null() || !(v.is_object() || v.is_array())) {
            let vs = if v.is_null() { "null".to_string() } else { tmpl(Some(v)) };
            lines.push(format!("  {k} = {vs}"));
        }
        let objs: Vec<String> = entries.iter().filter(|(_, v)| !v.is_null() && (v.is_object() || v.is_array())).map(|(k, _)| k.clone()).collect();
        if !objs.is_empty() { lines.push(format!("  (object facts: {})", objs.join(", "))); }
        lines.push(String::new());
    }

    let contract = data.get("contract").cloned().unwrap_or(Value::Null);
    if truthy(Some(&contract)) {
        let c = &contract;
        lines.push("═══ Contract snapshot ═══".to_string());
        lines.push(format!("  slug:       {}    status: {}    tier: {}    attempt: {}",
            tmpl(c.get("slug")), tmpl(c.get("status")), nz_str(c.get("tier"), "-"), nz_str(c.get("current_attempt"), "-")));
        lines.push(format!("  signed_by:  {}    signed_at: {}", nz_str(c.get("signed_by"), "(unsigned)"), nz_str(c.get("signed_at"), "-")));
        lines.push(format!("  sandbox:    {}", nz_str(c.get("sandbox_path"), "(none)")));
        lines.push(format!("  updated:    {}    created: {}", tmpl(c.get("updated_at")), tmpl(c.get("created_at"))));
        let goal_md = c.get("goal_md");
        if truthy(goal_md) {
            lines.push(format!("  goal_md:    {} bytes  (rendered at the end)", js_len(goal_md.and_then(Value::as_str).unwrap_or(""))));
        } else if c.as_object().map(|o| o.contains_key("goal_md")).unwrap_or(false) {
            lines.push("  goal_md:    (null — contract pre-dates migration 048 OR no template matched)".to_string());
        }
        lines.push(String::new());
    }

    let qa_runs = data.get("qa_runs").and_then(Value::as_array).cloned().unwrap_or_default();
    if !qa_runs.is_empty() {
        lines.push(format!("═══ Last QA runs ({}) ═══", qa_runs.len()));
        for q in &qa_runs {
            let verdict = match q.get("all_passed").and_then(Value::as_i64) { Some(1) => "PASS", Some(0) => "FAIL", _ => "(no verdict)" };
            lines.push(format!("  [{}] {}  {}  scope={}  acc_pass={} acc_fail={}",
                tmpl(q.get("id")), tmpl(q.get("ts")), verdict,
                nz_str(q.get("scope_passed"), "-"), nz_str(q.get("acceptance_pass"), "-"), nz_str(q.get("acceptance_fail"), "-")));
        }
        lines.push(String::new());
    } else if truthy(Some(&contract)) {
        lines.push("═══ Last QA runs ═══".to_string());
        lines.push(format!("  (no qa_runs recorded for {} — QA has never fired)", tmpl(contract.get("slug"))));
        lines.push(String::new());
    }

    lines.push(format!("═══ Runtime events tree ({}) ═══", events.len()));
    if events.is_empty() {
        lines.push("  (no driver-fired events in the tick window — likely failed before dispatch)".to_string());
    } else {
        for r in &events {
            let depth = r.get("depth").and_then(Value::as_i64).unwrap_or(0).max(0) as usize;
            let pad = format!("  {}", "  ".repeat(depth));
            let dur = match nz(r.get("duration_ms")).map(|v| js_number(Some(v))) { Some(d) => format!("{}ms", js_math_round(d)), None => "-".to_string() };
            let err_suffix = if truthy(r.get("error_code")) { format!(" {}", tmpl(r.get("error_code"))) } else { String::new() };
            lines.push(format!("{pad}[{}] {} :: {} → {}{}{}",
                tmpl(r.get("id")), nz_str(r.get("action"), "-"), nz_str(r.get("skill"), "-"), tmpl(r.get("outcome")), err_suffix, format!(" ({dur})")));
        }
    }
    lines.push(String::new());

    let ai_calls = data.get("ai_calls").and_then(Value::as_array).cloned().unwrap_or_default();
    let ai_outputs = data.get("ai_outputs").cloned().unwrap_or(Value::Object(Map::new()));
    let prompt_names = data.get("prompt_names").cloned().unwrap_or(Value::Object(Map::new()));

    lines.push(format!("═══ AI calls ({}) ═══", ai_calls.len()));
    for c in &ai_calls {
        let ptid_key = tmpl(c.get("prompt_template_id"));
        let prompt = prompt_names.get(&ptid_key);
        let prompt_name = prompt.and_then(|p| nz(p.get("name"))).map(|v| tmpl(Some(v))).unwrap_or_else(|| "?".to_string());
        let prompt_version = prompt.and_then(|p| nz(p.get("version"))).map(|v| tmpl(Some(v))).unwrap_or_else(|| "?".to_string());
        let dur_txt = match nz(c.get("duration_ms")).map(|v| js_number(Some(v))) { Some(d) => format!("{}ms", js_math_round(d)), None => String::new() };
        let cost_v = nz(c.get("cost_usd_actual")).cloned().unwrap_or(json!(0));
        let cached_suffix = if truthy(c.get("cached")) { "  CACHED" } else { "" };
        lines.push(format!("  [{}] {}@v{}  {}/{}  {}  {}→{} tok  {}  ${}{}",
            tmpl(c.get("id")), prompt_name, prompt_version, tmpl(c.get("tier")), tmpl(c.get("temperature")),
            tmpl(c.get("outcome")), tmpl(c.get("tokens_in")), tmpl(c.get("tokens_out")), dur_txt,
            to_fixed(js_number(Some(&cost_v)), 4), cached_suffix));

        let call_key = tmpl(c.get("id"));
        let outs = ai_outputs.get(&call_key).and_then(Value::as_array).cloned().unwrap_or_default();
        let raw_dump = outs.iter().find(|p| p.get("field_name").and_then(Value::as_str) == Some("__raw_output_on_validation_fail__"));
        if let Some(rd) = raw_dump {
            let full = rd.get("value_text").and_then(Value::as_str).unwrap_or("").to_string();
            lines.push(format!("     ⚠ raw_output (validation failed) — {} bytes:", js_len(&full)));
            let preview = js_slice(&full, 800);
            for seg in wrap_chunks(&preview) { lines.push(format!("       {seg}")); }
            if js_len(&full) > 800 {
                lines.push(format!("       /* ... {} more bytes — query brain.ai_call_outputs id={} for full ... */", js_len(&full) - 800, tmpl(rd.get("id"))));
            }
        }
        for p in &outs {
            if p.get("field_name").and_then(Value::as_str) == Some("__raw_output_on_validation_fail__") { continue; }
            let children = p.get("children").and_then(Value::as_array).cloned().unwrap_or_default();
            let preview = match p.get("value_text") {
                Some(Value::String(s)) => format!("\"{}\"", js_slice(&s.replace('\n', "⏎"), 120)),
                _ => if !children.is_empty() { format!("[{} children]", children.len()) } else { String::new() },
            };
            lines.push(format!("     {} = {}", tmpl(p.get("field_name")), preview));
            for ch in children.iter().take(6) {
                let ch_preview = match ch.get("value_text") {
                    Some(Value::String(s)) => format!("\"{}\"", js_slice(&s.replace('\n', "⏎"), 100)),
                    _ => String::new(),
                };
                lines.push(format!("       ↳ {} = {}", tmpl(ch.get("field_name")), ch_preview));
            }
        }
    }
    if ai_calls.is_empty() { lines.push("  (no AI calls in window)".to_string()); }
    lines.push(String::new());

    let audit_rows = data.get("audit_rows").and_then(Value::as_array).cloned().unwrap_or_default();
    lines.push(format!("═══ Audit chain mutations in window ({}) ═══", audit_rows.len()));
    for r in &audit_rows {
        lines.push(format!("  [{}] {}  {}#{}  by {}", tmpl(r.get("id")), tmpl(r.get("operation")), tmpl(r.get("entity_type")), tmpl(r.get("entity_id")), tmpl(r.get("actor"))));
    }
    if audit_rows.is_empty() { lines.push("  (none — tick mutated nothing)".to_string()); }
    lines.push(String::new());

    let recent_ticks = data.get("recent_ticks").and_then(Value::as_array).cloned().unwrap_or_default();
    if !recent_ticks.is_empty() {
        lines.push(format!("═══ Recent ticks for {} ({}) ═══", tmpl(t.get("card_slug")), recent_ticks.len()));
        for r in &recent_ticks {
            let mark = if r.get("id").and_then(Value::as_i64) == t.get("id").and_then(Value::as_i64) { "★" } else { " " };
            let err_tail = match r.get("error_message") {
                Some(v) if truthy(Some(v)) => format!("  → {}", js_slice(v.as_str().unwrap_or(""), 70)),
                _ => String::new(),
            };
            lines.push(format!("  {mark} [{}] {}  {}  {}{}", tmpl(r.get("id")), tmpl(r.get("started_at")), nz_str(r.get("decided_action"), "-"), tmpl(r.get("outcome")), err_tail));
        }
        lines.push(String::new());
    }

    let bundle = data.get("bundle").cloned().unwrap_or(Value::Null);
    let ai_actually_fired = !ai_calls.is_empty();
    if truthy(Some(&bundle)) && ai_actually_fired {
        lines.push("═══ DEV bundle (re-derived from current contract) ═══".to_string());
        if truthy(bundle.get("skipped")) {
            lines.push(format!("  (skipped: {})", tmpl(bundle.get("reason"))));
        } else {
            let files_len = bundle.get("files").and_then(Value::as_array).map(|a| a.len()).unwrap_or(0);
            let keywords = bundle.get("keywords_used").and_then(Value::as_array).cloned().unwrap_or_default();
            let kw_join = keywords.iter().map(|v| tmpl(Some(v))).collect::<Vec<_>>().join(", ");
            lines.push(format!("  total_bytes: {}    files: {}    keywords: {}", tmpl(bundle.get("total_bytes")), files_len, kw_join));
            lines.push("  ── bundle_md (first 2000 chars) ──".to_string());
            let bundle_md = bundle.get("bundle_md").and_then(Value::as_str).unwrap_or("").to_string();
            lines.push(js_slice(&bundle_md, 2000));
            if js_len(&bundle_md) > 2000 { lines.push(format!("  /* ... {} more bytes truncated ... */", js_len(&bundle_md) - 2000)); }
        }
    } else if truthy(Some(&bundle)) && !ai_actually_fired && tick_decided_action_is_dev(&t) {
        lines.push("═══ DEV bundle ═══".to_string());
        let tb = nz(bundle.get("total_bytes")).map(|v| tmpl(Some(v))).unwrap_or_else(|| "?".to_string());
        lines.push(format!("  (no AI call fired this tick — bundle elided; would be {tb} bytes)"));
    }

    if truthy(contract.get("goal_md")) {
        let s = contract.get("goal_md").and_then(Value::as_str).unwrap_or("").to_string();
        lines.push(String::new());
        lines.push(format!("═══ Contract goal_md ({} bytes) ═══", js_len(&s)));
        lines.push(s);
    }

    lines
}

pub fn driverInspect(input: &Value, db: &Connection) -> Result<Value, String> {
    let tick_id_v: Value = nz(input.get("tick_id")).cloned().unwrap_or(json!("latest"));
    let render_only = input.get("render_only") == Some(&json!(true));

    let tick = match load_tick(db, &tick_id_v)? {
        Some(t) => t,
        None => return Err(format!("driver-inspect: driver_tick '{}' not found", js_string(&tick_id_v))),
    };

    let drv_roots = load_driver_events(db, &tick)?;
    let root_ids: Vec<Value> = drv_roots.iter().map(|r| g(r, "id")).collect();
    let events = descend_event_tree(db, &root_ids)?;
    let event_ids: Vec<Value> = events.iter().map(|e| g(e, "id")).collect();

    let started = g(&tick, "started_at");
    let ended = nz(tick.get("ended_at")).cloned().unwrap_or_else(|| started.clone());
    let ai_calls = load_ai_calls(db, &event_ids, &started, &ended)?;

    let mut ai_outputs = Map::new();
    let mut prompt_names = Map::new();
    for c in &ai_calls {
        let call_key = tmpl(c.get("id"));
        let outs = load_ai_outputs(db, &g(c, "id"))?;
        ai_outputs.insert(call_key, json!(outs));

        let ptid = g(c, "prompt_template_id");
        let ptid_key = tmpl(Some(&ptid));
        if !prompt_names.contains_key(&ptid_key) {
            if let Some(p) = load_prompt_name(db, &ptid)? { prompt_names.insert(ptid_key, p); }
        }
    }

    let audit_rows = load_audit_rows(db, &tick)?;
    let bundle = try_derive_bundle(db, &tick);
    let contract = load_active_contract(db, &g(&tick, "card_slug"))?;
    let qa_runs = match &contract { Some(c) if !c.is_null() => load_last_qa_runs(db, &g(c, "slug"), 3), _ => vec![] };
    let recent_ticks = load_recent_ticks_for_card(db, &g(&tick, "card_slug"), 6)?;

    // `tick.facts_json ? JSON.parse(...) : null` wrapped in try/catch → null on falsy OR parse error.
    let facts: Value = match tick.get("facts_json").and_then(Value::as_str) {
        None => Value::Null,
        Some(s) if s.is_empty() => Value::Null,
        Some(s) => serde_json::from_str(s).unwrap_or(Value::Null),
    };

    let mut data = Map::new();
    data.insert("tick".into(), tick);
    data.insert("facts".into(), facts);
    data.insert("events".into(), json!(events));
    data.insert("ai_calls".into(), json!(ai_calls));
    data.insert("ai_outputs".into(), Value::Object(ai_outputs));
    data.insert("prompt_names".into(), Value::Object(prompt_names));
    data.insert("audit_rows".into(), json!(audit_rows));
    data.insert("bundle".into(), bundle.unwrap_or(Value::Null));
    // Some(Null) → "contract":null (falsy slug); None → key omitted (slug present, no row).
    if let Some(c) = contract {
        data.insert("contract".into(), c);
    }
    data.insert("qa_runs".into(), json!(qa_runs));
    data.insert("recent_ticks".into(), json!(recent_ticks));

    let render = render_transcript(&Value::Object(data.clone()));

    if render_only { return Ok(json!(render.join("\n"))); }

    data.insert("render".into(), json!(render));
    Ok(Value::Object(data))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS brain").unwrap();
        c.execute_batch(
            "CREATE TABLE brain.driver_ticks (id INTEGER PRIMARY KEY, started_at TEXT, ended_at TEXT, \
             card_slug TEXT, decided_agent TEXT, decided_action TEXT, decided_reason TEXT, outcome TEXT, \
             duration_ms INTEGER, error_message TEXT, cost_usd REAL, facts_json TEXT);
             CREATE TABLE runtime_events (id INTEGER PRIMARY KEY, ts TEXT, actor TEXT, action TEXT, skill TEXT, \
             outcome TEXT, error_code TEXT, duration_ms REAL, context_json TEXT, parent_event_id INTEGER);
             CREATE TABLE sacred_timeline (id INTEGER PRIMARY KEY, ts TEXT, actor TEXT, operation TEXT, entity_type TEXT, entity_id TEXT);
             CREATE TABLE contracts (id INTEGER PRIMARY KEY AUTOINCREMENT, slug TEXT UNIQUE, title TEXT, status TEXT, tier TEXT, signed_by TEXT, \
             signed_at TEXT, sandbox_path TEXT, created_at TEXT, updated_at TEXT, current_attempt INTEGER, \
             files_touched_json TEXT, briefing_md TEXT, acceptance_json TEXT, goal_md TEXT, card_slug TEXT, superseded_by TEXT);
             CREATE TABLE qa_runs (id INTEGER PRIMARY KEY, contract_slug TEXT, all_passed INTEGER, scope_passed INTEGER, \
             acceptance_pass INTEGER, acceptance_fail INTEGER, ts TEXT);
             CREATE TABLE brain.ai_calls (id INTEGER PRIMARY KEY, ts TEXT, prompt_template_id INTEGER, tier TEXT, \
             temperature TEXT, provider TEXT, model_id TEXT, outcome TEXT, tokens_in INTEGER, tokens_out INTEGER, \
             duration_ms REAL, cost_usd_actual REAL, cached INTEGER, parent_event_id INTEGER);
             CREATE TABLE brain.ai_call_outputs (id INTEGER PRIMARY KEY, ai_call_id INTEGER, field_name TEXT, \
             value_text TEXT, value_number REAL, value_boolean INTEGER, order_index INTEGER, parent_output_id INTEGER);
             CREATE TABLE brain.prompt_templates (id INTEGER PRIMARY KEY, name TEXT, version INTEGER);"
        ).unwrap();
        c
    }

    fn seed_tick(c: &Connection, id: i64, card_slug: Option<&str>, decided_action: Option<&str>, outcome: &str) {
        c.execute(
            "INSERT INTO brain.driver_ticks (id, started_at, ended_at, card_slug, decided_agent, decided_action, decided_reason, outcome, duration_ms, error_message, cost_usd, facts_json) \
             VALUES (?1, '2026-05-22 12:00:00', '2026-05-22 12:00:01', ?2, 'dev', ?3, 'r', ?4, 10, NULL, 0.1, NULL)",
            rusqlite::params![id, card_slug, decided_action, outcome],
        ).unwrap();
    }

    #[test]
    fn not_found_throws() {
        let c = db();
        let e = driverInspect(&json!({ "tick_id": 999 }), &c).unwrap_err();
        assert_eq!(e, "driver-inspect: driver_tick '999' not found");
    }

    #[test]
    fn latest_resolves_to_highest_id_and_returns_full_object() {
        let c = db();
        seed_tick(&c, 1, Some("A"), None, "ok");
        seed_tick(&c, 2, Some("B"), None, "failed");
        let r = driverInspect(&json!({}), &c).unwrap();
        assert_eq!(r["tick"]["id"], json!(2));
        assert_eq!(r["tick"]["card_slug"], json!("B"));
        assert!(r["render"].as_array().unwrap().iter().any(|l| l.as_str().unwrap().contains("Tick 2")));
        assert_eq!(r["bundle"], Value::Null);
    }

    #[test]
    fn render_only_returns_joined_string() {
        let c = db();
        seed_tick(&c, 1, None, None, "ok");
        let r = driverInspect(&json!({ "tick_id": 1, "render_only": true }), &c).unwrap();
        assert!(r.is_string());
        assert!(r.as_str().unwrap().contains("Tick 1"));
        assert!(r.as_str().unwrap().contains("(no card)"));
    }

    #[test]
    fn numeric_string_tick_id_resolves_by_number_coercion() {
        let c = db();
        seed_tick(&c, 5, None, None, "ok");
        let r = driverInspect(&json!({ "tick_id": "5" }), &c).unwrap();
        assert_eq!(r["tick"]["id"], json!(5));
    }

    #[test]
    fn event_tree_recursion_and_ai_call_dedup_and_prompt_lookup() {
        let c = db();
        seed_tick(&c, 1, None, None, "failed");
        c.execute_batch(
            "INSERT INTO runtime_events (id, ts, actor, action, skill, outcome, error_code, duration_ms, parent_event_id) VALUES \
             (10, '2026-05-22 12:00:00', 'driver:auto', 'a1', 's1', 'error', 'VALIDATION_LOGIC', 123.6, NULL), \
             (11, '2026-05-22 12:00:01', 'driver:auto', 'a2', 's2', 'success', NULL, 45, 10), \
             (12, '2026-05-22 12:00:00', 'other:x', 'a3', 's3', 'success', NULL, 1, NULL);
             INSERT INTO brain.ai_calls (id, ts, prompt_template_id, tier, temperature, provider, model_id, outcome, tokens_in, tokens_out, duration_ms, cost_usd_actual, cached, parent_event_id) VALUES \
             (100, '2026-05-22 12:00:00', 5, 'mid', 'cold', 'anthropic', 'm1', 'success', 10, 20, 678.4, 0.01, 0, 10), \
             (101, '2026-05-22 12:00:01', 6, 'high', 'hot', 'anthropic', 'm2', 'error', 1, 0, NULL, NULL, 1, NULL);
             INSERT INTO brain.prompt_templates (id, name, version) VALUES (5, 'Contract Draft', 3);"
        ).unwrap();
        let r = driverInspect(&json!({ "tick_id": 1 }), &c).unwrap();
        assert_eq!(r["events"].as_array().unwrap().len(), 2); // event 12 excluded (not driver:%, not a descendant)
        assert_eq!(r["events"][1]["depth"], json!(1));
        assert_eq!(r["ai_calls"].as_array().unwrap().len(), 2); // 100 dedup'd (matched by both event+window), 101 window-only
        let render_text = driverInspect(&json!({ "tick_id": 1, "render_only": true }), &c).unwrap();
        let text = render_text.as_str().unwrap();
        assert!(text.contains("(VALIDATION_LOGIC)")); // deepest error_code bubbled to outcome line
        assert!(text.contains("Contract Draft@v3"));
        assert!(text.contains("?@v?")); // template 6 not found
        assert!(text.contains("CACHED"));
    }

    #[test]
    fn contract_and_qa_and_goal_md_rendering() {
        let c = db();
        seed_tick(&c, 1, Some("CARD-B"), None, "ok");
        c.execute(
            "INSERT INTO contracts (slug, title, status, tier, signed_by, signed_at, sandbox_path, created_at, updated_at, current_attempt, files_touched_json, briefing_md, acceptance_json, goal_md, card_slug, superseded_by) \
             VALUES ('C-1', 't', 'impl', NULL, NULL, NULL, '.antiloki/sandbox/b', '2026-01-01 00:00:00', '2026-01-01 00:30:00', 2, '[]', 'b', '[]', '## Mission', 'CARD-B', NULL)",
            [],
        ).unwrap();
        c.execute_batch(
            "INSERT INTO qa_runs (id, contract_slug, all_passed, scope_passed, acceptance_pass, acceptance_fail, ts) VALUES \
             (1, 'C-1', 1, 1, 2, 0, '2026-01-01 01:00:00'), (2, 'C-1', 0, 1, 1, 1, '2026-01-01 01:01:00'), (3, 'C-1', NULL, NULL, NULL, NULL, '2026-01-01 01:02:00')"
        ).unwrap();
        let r = driverInspect(&json!({ "tick_id": 1, "render_only": true }), &c).unwrap();
        let text = r.as_str().unwrap();
        assert!(text.contains("(unsigned)"));
        assert!(text.contains("tier: -"));
        assert!(text.contains("PASS"));
        assert!(text.contains("FAIL"));
        assert!(text.contains("(no verdict)"));
        let n = js_len("## Mission");
        assert!(text.contains(&format!("goal_md:    {n} bytes  (rendered at the end)")));
        assert!(text.contains(&format!("Contract goal_md ({n} bytes)")));
        assert!(text.ends_with("## Mission"));
    }

    #[test]
    fn dev_bundle_ok_real_call_and_null_goal_md_branch() {
        let c = db();
        seed_tick(&c, 1, Some("CARD-C"), Some("git-manager.implement.code"), "ok");
        c.execute(
            "INSERT INTO contracts (slug, title, status, tier, signed_by, signed_at, sandbox_path, created_at, updated_at, current_attempt, files_touched_json, briefing_md, acceptance_json, goal_md, card_slug, superseded_by) \
             VALUES ('C-2', 't', 'impl', 'standard', 'driver:auto', '2026-01-01 00:00:00', '.antiloki/sandbox/c', '2026-01-01 00:00:00', '2026-01-01 00:00:00', 1, '[]', 'b', '[\"a1\",\"a2\"]', NULL, 'CARD-C', NULL)",
            [],
        ).unwrap();
        c.execute(
            "INSERT INTO brain.ai_calls (id, ts, prompt_template_id, tier, temperature, provider, model_id, outcome, tokens_in, tokens_out, duration_ms, cost_usd_actual, cached, parent_event_id) \
             VALUES (1, '2026-05-22 12:00:00', NULL, 'low', 'freezed', 'anthropic', 'm3', 'success', 10, 20, NULL, NULL, 1, NULL)",
            [],
        ).unwrap();
        let r = driverInspect(&json!({ "tick_id": 1 }), &c).unwrap();
        assert_eq!(r["bundle"]["ok"], json!(true));
        assert_eq!(r["bundle"]["files"], json!([]));
        let text = driverInspect(&json!({ "tick_id": 1, "render_only": true }), &c).unwrap();
        let text = text.as_str().unwrap();
        assert!(text.contains("goal_md:    (null — contract pre-dates migration 048 OR no template matched)"));
        assert!(text.contains("DEV bundle (re-derived from current contract)"));
        assert!(!text.contains("skipped:"));
    }

    #[test]
    fn dev_bundle_skipped_with_ai_calls_shows_skipped_reason() {
        let c = db();
        seed_tick(&c, 1, Some("CARD-D"), Some("git-manager.implement.code"), "failed");
        c.execute(
            "INSERT INTO brain.ai_calls (id, ts, prompt_template_id, tier, temperature, provider, model_id, outcome, tokens_in, tokens_out, duration_ms, cost_usd_actual, cached, parent_event_id) \
             VALUES (1, '2026-05-22 12:00:00', NULL, 'low', 'freezed', 'anthropic', 'm', 'success', 1, 1, NULL, NULL, 0, NULL)",
            [],
        ).unwrap();
        let r = driverInspect(&json!({ "tick_id": 1, "render_only": true }), &c).unwrap();
        assert!(r.as_str().unwrap().contains("(skipped: no active contract found for card)"));
    }

    #[test]
    fn dev_bundle_elided_when_no_ai_calls_fired() {
        let c = db();
        seed_tick(&c, 1, None, Some("git-manager.implement.code"), "failed");
        let r = driverInspect(&json!({ "tick_id": 1, "render_only": true }), &c).unwrap();
        let text = r.as_str().unwrap();
        assert!(text.contains("no AI call fired this tick — bundle elided; would be ? bytes"));
        assert!(!text.contains("## Files in scope"));
    }

    #[test]
    fn facts_json_scalar_and_object_grouping() {
        let c = db();
        c.execute(
            "INSERT INTO brain.driver_ticks (id, started_at, ended_at, card_slug, decided_agent, decided_action, decided_reason, outcome, duration_ms, error_message, cost_usd, facts_json) \
             VALUES (1, '2026-05-22 12:00:00', '2026-05-22 12:00:01', NULL, NULL, NULL, NULL, 'ok', NULL, NULL, NULL, '{\"hasContract\":true,\"attempt\":2,\"note\":null,\"ctx\":{\"x\":1}}')",
            [],
        ).unwrap();
        let r = driverInspect(&json!({ "tick_id": 1, "render_only": true }), &c).unwrap();
        let text = r.as_str().unwrap();
        assert!(text.contains("hasContract = true"));
        assert!(text.contains("attempt = 2"));
        assert!(text.contains("note = null"));
        assert!(text.contains("(object facts: ctx)"));
        assert!(text.contains("cost: $0.0000")); // null cost_usd → 0 → toFixed(4)
        assert!(text.contains("duration: -")); // null duration_ms, unrounded path
    }

    #[test]
    fn malformed_facts_json_silently_yields_null() {
        let c = db();
        c.execute(
            "INSERT INTO brain.driver_ticks (id, started_at, ended_at, card_slug, decided_agent, decided_action, decided_reason, outcome, duration_ms, error_message, cost_usd, facts_json) \
             VALUES (1, '2026-05-22 12:00:00', NULL, NULL, NULL, NULL, NULL, 'ok', NULL, NULL, NULL, 'not-json{')",
            [],
        ).unwrap();
        let r = driverInspect(&json!({ "tick_id": 1 }), &c).unwrap();
        assert_eq!(r["facts"], Value::Null);
        assert!(!r["render"].as_array().unwrap().iter().any(|l| l.as_str().unwrap().contains("Decision facts")));
    }
}
