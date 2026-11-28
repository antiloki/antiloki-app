//! ai-log-export.rs — twin of skills/system/ai-log-export.mjs (db READ lane). NAMES ARE CONTRACT:
//! `aiLogExport` (default export).
//!
//! BESPOKE-NATIVE — NOT conform-db fixture-testable: this skill writes REAL files to a
//! caller-supplied `out_dir` (`mkdirSync(..., {recursive:true})` + `writeFileSync` per call plus
//! an `INDEX.md`), a real fs side effect outside conform-db's db-before/db-after two-lane
//! contract. It dispatches to `crate::system_ai_call_detail::aiCallDetail` (the already-converted
//! twin of `ai-call-detail.mjs`) for every `brain.ai_calls` row it lists — a faithful port calls
//! that twin verbatim rather than reimplementing prompt/response reconstruction.
//!
//! `context.db` missing -> JS throws `err.contextMissing(...)`; unreachable here since
//! `db: &Connection` is a required, non-nullable reference (stack-facts.rs / qa-sandbox-reset.rs
//! precedent). `out_dir` missing -> JS throws `err.required(...)`; modeled as a plain
//! `Err(String)` (dev-plan-spec.rs / atlas-build.rs convention), since this IS the skill's own
//! input validation, not a context guard.
//!
//! `formatResponse` rebuilds the `output_kv` parent/child tree exactly as the JS does (rows keyed
//! by `parent_output_id ?? 0`, children rendered recursively, array/object structure surfaced via
//! indentation) — a flat top-level-only render would hide nested response fields.
//!
//! `fence()`'s escape for embedded triple-backticks inserts a U+200B ZERO WIDTH SPACE between the
//! first and remaining two backticks (`` ` `` + U+200B + `` `` `` ), copied byte-for-byte from the
//! JS regex replacement (verified via a `node -e` codepoint dump, not eyeballed).
// QUALITY: — db-read fan-out over aiCallDetail + real fs writes (mkdir/write), no reimplementation of the reused twin.

use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::fs;
use std::path::Path;

fn truthy(v: Option<&Value>) -> bool { match v { None|Some(Value::Null)=>false, Some(Value::Bool(b))=>*b, Some(Value::Number(n))=>n.as_f64().map_or(false,|f|f!=0.0&&!f.is_nan()), Some(Value::String(s))=>!s.is_empty(), Some(_)=>true } }
fn str_qq(v: Option<&Value>) -> String { match v { None|Some(Value::Null)=>String::new(), Some(Value::String(s))=>s.clone(), Some(Value::Bool(b))=>b.to_string(), Some(n@Value::Number(_))=>n.to_string(), Some(o)=>o.to_string() } }
fn js_string(v: &Value) -> String { match v { Value::Null=>"null".into(), Value::String(s)=>s.clone(), Value::Number(n)=>n.to_string(), Value::Bool(b)=>b.to_string(), Value::Array(a)=>a.iter().map(js_string).collect::<Vec<_>>().join(","), Value::Object(_)=>"[object Object]".into() } } // = JS String(v)
fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) } // JS `??` right operand: nz(a).or(nz(b))
fn js_number(v: Option<&Value>) -> f64 { match v { None=>f64::NAN, Some(Value::Null)=>0.0, Some(Value::Bool(b))=>if *b {1.0} else {0.0}, Some(Value::Number(n))=>n.as_f64().unwrap_or(f64::NAN), Some(Value::String(s))=>{let t=s.trim(); if t.is_empty(){0.0}else{t.parse().unwrap_or(f64::NAN)}}, Some(_)=>f64::NAN } } // = JS Number(v)

// JS: `'```\n' + String(s ?? '(none)').replace(/```/g, '`​``') + '\n```'` — avoid breaking the
// outer code fence when the content itself contains a literal triple-backtick.
fn fence(v: Option<&Value>) -> String {
    let inner = match nz(v) { None => "(none)".to_string(), Some(x) => js_string(x) };
    format!("```\n{}\n```", inner.replace("```", "`\u{200B}``"))
}

// JS: `r.value_text ?? (r.value_number != null ? r.value_number : r.value_boolean)`.
fn row_val(r: &Value) -> Value {
    if let Some(v) = nz(r.get("value_text")) { return v.clone(); }
    if let Some(v) = nz(r.get("value_number")) { return v.clone(); }
    r.get("value_boolean").cloned().unwrap_or(Value::Null)
}

fn render_tree(by_parent: &HashMap<i64, Vec<Value>>, parent_id: i64, depth: usize) -> String {
    let mut kids = by_parent.get(&parent_id).cloned().unwrap_or_default();
    kids.sort_by(|a, b| {
        let oa = js_number(a.get("order_index"));
        let ob = js_number(b.get("order_index"));
        oa.partial_cmp(&ob).unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| js_number(a.get("id")).partial_cmp(&js_number(b.get("id"))).unwrap_or(std::cmp::Ordering::Equal))
    });
    kids.iter()
        .map(|r| {
            let pad = "  ".repeat(depth);
            let id = r.get("id").and_then(Value::as_i64).unwrap_or(0);
            let label = match nz(r.get("field_name")) { Some(v) => js_string(v), None => "-".to_string() };
            if by_parent.contains_key(&id) {
                format!("{pad}{label}:\n{}", render_tree(by_parent, id, depth + 1))
            } else {
                let val = row_val(r);
                let val_str = match &val { Value::Null => "(empty)".to_string(), other => js_string(other) };
                format!("{pad}{label}: {val_str}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

// Reconstructs the nested output_kv tree (array/object children live as child rows via
// parent_output_id) — a flat top-level-only render would hide array contents.
fn format_response(d: &Value) -> String {
    if truthy(d.get("raw_output_on_failure")) {
        return str_qq(d.get("raw_output_on_failure"));
    }
    let rows: Vec<Value> = d.get("output_kv").and_then(Value::as_array).cloned().unwrap_or_default();
    if rows.is_empty() {
        return "(no stored response)".to_string();
    }
    let mut by_parent: HashMap<i64, Vec<Value>> = HashMap::new();
    for r in &rows {
        let k = nz(r.get("parent_output_id")).and_then(Value::as_i64).unwrap_or(0);
        by_parent.entry(k).or_default().push(r.clone());
    }
    render_tree(&by_parent, 0, 0)
}

// JS: `name.replace(/[^\w.-]/g, '_')` — \w = [A-Za-z0-9_].
fn sanitize_name(name: &str) -> String {
    name.chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '.' || c == '-' { c } else { '_' })
        .collect()
}

fn build_call_md(name: &str, c: &Value, d: &Value) -> String {
    let cost = js_number(c.get("cost_usd_actual"));
    let cost = if cost.is_nan() { 0.0 } else { cost };
    let duration = js_number(c.get("duration_ms"));
    let duration = if duration.is_nan() { 0.0 } else { duration };

    let mut lines: Vec<String> = vec![
        format!("# {} v{} — call #{}", name, match nz(c.get("pv")) { Some(v) => js_string(v), None => "?".to_string() }, js_string(c.get("id").unwrap_or(&Value::Null))),
        String::new(),
        "| field | value |".to_string(),
        "|---|---|".to_string(),
        format!("| ts | {} |", str_qq(c.get("ts"))),
        format!("| tier | {} |", str_qq(c.get("tier"))),
        format!("| outcome | {} |", str_qq(c.get("outcome"))),
        format!("| model | {} |", match nz(c.get("model_id")) { Some(v) => js_string(v), None => "?".to_string() }),
        format!("| tokens | in {} / out {} |", match nz(c.get("tokens_in")) { Some(v) => js_string(v), None => "0".to_string() }, match nz(c.get("tokens_out")) { Some(v) => js_string(v), None => "0".to_string() }),
        format!("| cost | ${:.6} |", cost),
        format!("| duration | {} ms |", duration.round() as i64),
        if truthy(d.get("report")) {
            format!("| failure_class | {} ({}) |", str_qq(d.get("report").and_then(|r| r.get("failure_class"))), str_qq(d.get("report").and_then(|r| r.get("severity"))))
        } else {
            String::new()
        },
        String::new(),
        format!("## PROMPT  _({})_", match nz(d.get("rendered_source")) { Some(v) => js_string(v), None => "n/a".to_string() }),
        fence(d.get("rendered_prompt")),
        String::new(),
        "## RESPONSE".to_string(),
        fence(Some(&json!(format_response(d)))),
    ];

    if truthy(d.get("validation_errors")) {
        let pretty = serde_json::to_string_pretty(d.get("validation_errors").unwrap()).unwrap_or_default();
        lines.push(format!("\n## VALIDATION ERRORS\n```json\n{}\n```", pretty));
    } else {
        lines.push(String::new());
    }
    let diag = d.get("report").and_then(|r| r.get("diagnosis_md"));
    if truthy(diag) {
        lines.push(format!("\n## POSTMORTEM DIAGNOSIS\n{}", str_qq(diag)));
    } else {
        lines.push(String::new());
    }
    let rec = d.get("report").and_then(|r| r.get("recommendation_md"));
    if truthy(rec) {
        lines.push(format!("\n## POSTMORTEM RECOMMENDATION\n{}", str_qq(rec)));
    } else {
        lines.push(String::new());
    }

    lines.into_iter().filter(|l| !l.is_empty()).collect::<Vec<_>>().join("\n")
}

pub fn aiLogExport(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS: `if (!db) throw err.contextMissing('ai-log-export', 'db')` — unreachable here, db lane
    // always injects a real Connection.
    let out_dir = match nz(input.get("out_dir")).and_then(Value::as_str) {
        Some(s) if !s.is_empty() => s.to_string(),
        _ => return Err("ai-log-export: 'out_dir' is required".to_string()),
    };
    let prompt_name = input.get("prompt_name").cloned();
    let since_ts = input.get("since_ts").cloned();
    // JS destructuring default `real_only = true` fires only when the key is ABSENT.
    let real_only = match input.get("real_only") { None => true, Some(v) => truthy(Some(v)) };
    let limit = match input.get("limit") { None => json!(5000), Some(v) => v.clone() };

    let mut wh: Vec<&str> = Vec::new();
    let mut params: Vec<Value> = Vec::new();
    if real_only { wh.push("a.cached = 0"); }
    if truthy(prompt_name.as_ref()) { wh.push("p.name LIKE '%' || ? || '%'"); params.push(prompt_name.clone().unwrap()); }
    if truthy(since_ts.as_ref()) { wh.push("a.ts >= ?"); params.push(since_ts.clone().unwrap()); }
    let where_sql = if wh.is_empty() { String::new() } else { format!("WHERE {}", wh.join(" AND ")) };
    let sql = format!(
        "SELECT a.id, p.name AS prompt_name, p.version AS pv, a.tier, a.outcome, a.ts, a.tokens_in, a.tokens_out, a.cost_usd_actual, a.duration_ms, a.model_id FROM brain.ai_calls a LEFT JOIN brain.prompt_templates p ON p.id = a.prompt_template_id {where_sql} ORDER BY p.name, a.id LIMIT ?"
    );
    params.push(limit);

    let calls: Vec<Value> = crate::db_json::query_rows(db, &sql, &params)?.as_array().cloned().unwrap_or_default();

    fs::create_dir_all(&out_dir).map_err(|e| e.to_string())?;
    let mut by_prompt: Map<String, Value> = Map::new();
    let mut index_rows: Vec<Value> = Vec::new();

    for c in &calls {
        let d = crate::system_ai_call_detail::aiCallDetail(&json!({ "ai_call_id": c.get("id").cloned().unwrap_or(Value::Null) }), db)?;
        if truthy(d.get("error")) {
            continue;
        }
        let name = match nz(c.get("prompt_name")) { Some(v) => js_string(v), None => "unknown".to_string() };
        let safe = sanitize_name(&name);
        let dir = Path::new(&out_dir).join(&safe);
        fs::create_dir_all(&dir).map_err(|e| e.to_string())?;

        let md = build_call_md(&name, c, &d);
        let call_id = c.get("id").cloned().unwrap_or(Value::Null);
        fs::write(dir.join(format!("call-{}.md", js_string(&call_id))), md).map_err(|e| e.to_string())?;

        let cur = by_prompt.get(&name).and_then(Value::as_i64).unwrap_or(0);
        by_prompt.insert(name.clone(), json!(cur + 1));
        index_rows.push(json!({
            "id": call_id,
            "prompt_name": name,
            "tier": c.get("tier").cloned().unwrap_or(Value::Null),
            "outcome": c.get("outcome").cloned().unwrap_or(Value::Null),
            "ts": c.get("ts").cloned().unwrap_or(Value::Null),
        }));
    }

    // JS: `Object.entries(byPrompt).sort((a,b) => b[1]-a[1])` — stable sort, ties keep insertion order.
    let mut by_prompt_entries: Vec<(String, i64)> = by_prompt.iter().map(|(k, v)| (k.clone(), v.as_i64().unwrap_or(0))).collect();
    by_prompt_entries.sort_by(|a, b| b.1.cmp(&a.1));

    let mut index_md_lines: Vec<String> = vec![
        "# AI call log export".to_string(),
        String::new(),
        format!(
            "Exported **{}** {}calls to `{}`.",
            index_rows.len(),
            if real_only { "real (non-cached) " } else { "" },
            out_dir
        ),
        "(OpenRouter's API cannot return prompt/response text — this is sourced from the local brain.ai_calls audit.)".to_string(),
        String::new(),
        "## By prompt (volume)".to_string(),
    ];
    for (n, ct) in &by_prompt_entries {
        index_md_lines.push(format!("- **{}** — {}", n, ct));
    }
    fs::write(Path::new(&out_dir).join("INDEX.md"), index_md_lines.join("\n")).map_err(|e| e.to_string())?;

    Ok(json!({
        "exported": index_rows.len(),
        "by_prompt": Value::Object(by_prompt),
        "out_dir": out_dir,
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

    fn scratch_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("ai-log-export-rs-test-{}-{}", tag, std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn out_dir_required() {
        let c = db();
        let r = aiLogExport(&json!({}), &c);
        assert_eq!(r, Err("ai-log-export: 'out_dir' is required".to_string()));
    }

    #[test]
    fn exports_real_calls_writes_files_and_index() {
        let c = db();
        c.execute_batch(
            "INSERT INTO brain.prompt_templates (id,name,version,template_md,default_tier,default_temperature,created_at)
              VALUES (1,'qa.gate.verify',2,'Check: {{x}}','low','cold','2025-12-01 00:00:00');
            INSERT INTO brain.ai_calls (id,ts,prompt_template_id,tier,temperature,input_hash,outcome,cached,tokens_in,tokens_out,duration_ms,cost_usd_actual)
              VALUES (10,'2026-01-01 09:00:00',1,'low','cold','h1','success',0,12,6,123.7,0.0025);
            INSERT INTO brain.ai_call_inputs (ai_call_id,key,value_text,order_index) VALUES (10,'x','why?',0);
            INSERT INTO brain.ai_call_outputs (ai_call_id,field_name,value_text,order_index) VALUES (10,'summary','a fine summary',0);",
        )
        .unwrap();

        let dir = scratch_dir("basic");
        let r = aiLogExport(&json!({ "out_dir": dir.to_str().unwrap() }), &c).unwrap();
        assert_eq!(r["exported"], json!(1));
        assert_eq!(r["by_prompt"], json!({ "qa.gate.verify": 1 }));

        let call_md = fs::read_to_string(dir.join("qa.gate.verify").join("call-10.md")).unwrap();
        assert!(call_md.starts_with("# qa.gate.verify v2 — call #10"));
        assert!(call_md.contains("| tier | low |"));
        assert!(call_md.contains("| cost | $0.002500 |"));
        assert!(call_md.contains("| duration | 124 ms |"));
        assert!(call_md.contains("## PROMPT"));
        assert!(call_md.contains("summary: a fine summary"));

        let index_md = fs::read_to_string(dir.join("INDEX.md")).unwrap();
        assert!(index_md.contains("Exported **1** real (non-cached) calls"));
        assert!(index_md.contains("- **qa.gate.verify** — 1"));

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn real_only_filters_cached_and_sanitizes_dir_name() {
        let c = db();
        c.execute_batch(
            "INSERT INTO brain.prompt_templates (id,name,version,template_md,default_tier,default_temperature,created_at)
              VALUES (1,'weird/name here',1,'x','low','cold','2025-12-01 00:00:00');
            INSERT INTO brain.ai_calls (id,ts,prompt_template_id,tier,temperature,input_hash,outcome,cached) VALUES
              (1,'2026-01-01 00:00:00',1,'low','cold','h1','success',0),
              (2,'2026-01-01 00:00:01',1,'low','cold','h2','success',1);",
        )
        .unwrap();
        let dir = scratch_dir("sanitize");
        let r = aiLogExport(&json!({ "out_dir": dir.to_str().unwrap() }), &c).unwrap();
        assert_eq!(r["exported"], json!(1));
        assert!(dir.join("weird_name_here").join("call-1.md").exists());
        assert!(!dir.join("weird/name here").exists());
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn real_only_false_includes_cached_calls() {
        let c = db();
        c.execute_batch(
            "INSERT INTO brain.prompt_templates (id,name,version,template_md,default_tier,default_temperature,created_at)
              VALUES (1,'qa.gate.verify',1,'x','low','cold','2025-12-01 00:00:00');
            INSERT INTO brain.ai_calls (id,ts,prompt_template_id,tier,temperature,input_hash,outcome,cached) VALUES
              (1,'2026-01-01 00:00:00',1,'low','cold','h1','success',0),
              (2,'2026-01-01 00:00:01',1,'low','cold','h2','success',1);",
        )
        .unwrap();
        let dir = scratch_dir("realonlyfalse");
        let r = aiLogExport(&json!({ "out_dir": dir.to_str().unwrap(), "real_only": false }), &c).unwrap();
        assert_eq!(r["exported"], json!(2));
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn prompt_name_and_since_ts_filters_apply() {
        let c = db();
        c.execute_batch(
            "INSERT INTO brain.prompt_templates (id,name,version,template_md,default_tier,default_temperature,created_at) VALUES
              (1,'qa.gate.verify',1,'x','low','cold','2025-12-01 00:00:00'),
              (2,'pl.plan.compose',1,'x','low','cold','2025-12-01 00:00:00');
            INSERT INTO brain.ai_calls (id,ts,prompt_template_id,tier,temperature,input_hash,outcome,cached) VALUES
              (1,'2025-12-01 00:00:00',1,'low','cold','h1','success',0),
              (2,'2026-01-05 00:00:00',1,'low','cold','h2','success',0),
              (3,'2026-01-05 00:00:00',2,'low','cold','h3','success',0);",
        )
        .unwrap();
        let dir = scratch_dir("filters");
        let r = aiLogExport(&json!({ "out_dir": dir.to_str().unwrap(), "prompt_name": "gate", "since_ts": "2026-01-01 00:00:00" }), &c).unwrap();
        assert_eq!(r["exported"], json!(1));
        assert!(dir.join("qa.gate.verify").join("call-2.md").exists());
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn nested_output_tree_renders_in_response_section() {
        // No attempts/sentinel rows -> raw_output_on_failure stays null, so formatResponse falls
        // through to the output_kv tree render (raw_output_on_failure takes priority when set —
        // covered separately below).
        let c = db();
        c.execute_batch(
            "INSERT INTO brain.prompt_templates (id,name,version,template_md,default_tier,default_temperature,created_at)
              VALUES (1,'qa.check',1,'Verify: {{x}}','mid','cold','2025-12-01 00:00:00');
            INSERT INTO brain.ai_calls (id,ts,prompt_template_id,tier,temperature,input_hash,outcome,tokens_in,tokens_out)
              VALUES (30,'2025-12-30 12:00:00',1,'mid','cold','hv1','success',9,3);
            INSERT INTO brain.ai_call_outputs (id,ai_call_id,field_name,value_text,order_index,parent_output_id) VALUES
              (1,30,'microtasks',NULL,0,NULL),
              (2,30,'0',NULL,0,1),
              (3,30,'title','first item',0,2);",
        )
        .unwrap();
        let dir = scratch_dir("nested");
        let r = aiLogExport(&json!({ "out_dir": dir.to_str().unwrap() }), &c).unwrap();
        assert_eq!(r["exported"], json!(1));
        let md = fs::read_to_string(dir.join("qa.check").join("call-30.md")).unwrap();
        assert!(md.contains("microtasks:\n  0:\n    title: first item"));
        assert!(!md.contains("## VALIDATION ERRORS"));
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn raw_output_on_failure_wins_over_output_tree_and_validation_errors_render() {
        let c = db();
        c.execute_batch(
            "INSERT INTO brain.prompt_templates (id,name,version,template_md,default_tier,default_temperature,created_at)
              VALUES (1,'qa.check',1,'Verify: {{x}}','mid','cold','2025-12-01 00:00:00');
            INSERT INTO brain.ai_calls (id,ts,prompt_template_id,tier,temperature,input_hash,outcome,tokens_in,tokens_out)
              VALUES (30,'2025-12-30 12:00:00',1,'mid','cold','hv1','validation_failed',9,3);
            INSERT INTO brain.ai_call_attempts (id,ai_call_id,attempt_no,ts,outcome,correction_md,raw_output_md,validation_errors_json) VALUES
              (1,30,1,'2025-12-30 12:00:00','validation_failed',NULL,'first raw output','[\"field x too short\"]'),
              (2,30,2,'2025-12-30 12:00:05','validation_failed','please fix','second raw output','[\"field x still too short\"]');
            INSERT INTO brain.ai_call_outputs (id,ai_call_id,field_name,value_text,order_index,parent_output_id) VALUES
              (1,30,'microtasks',NULL,0,NULL);",
        )
        .unwrap();
        let dir = scratch_dir("raw-wins");
        let r = aiLogExport(&json!({ "out_dir": dir.to_str().unwrap(), "real_only": false }), &c).unwrap();
        assert_eq!(r["exported"], json!(1));
        let md = fs::read_to_string(dir.join("qa.check").join("call-30.md")).unwrap();
        assert!(md.contains("second raw output"));
        assert!(!md.contains("microtasks:"));
        assert!(md.contains("## VALIDATION ERRORS"));
        assert!(md.contains("field x still too short"));
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn fence_escapes_embedded_triple_backticks() {
        let got = fence(Some(&json!("has ```nested``` fence")));
        assert_eq!(got, "```\nhas `\u{200B}``nested`\u{200B}`` fence\n```");
    }

    #[test]
    fn fence_defaults_missing_to_none_literal() {
        assert_eq!(fence(None), "```\n(none)\n```");
        assert_eq!(fence(Some(&Value::Null)), "```\n(none)\n```");
    }
}
