//! dev-exec-sandbox.rs — twin of skills/agents/dev/dev-exec-sandbox.mjs. NAMES ARE CONTRACT:
//! `devExecSandbox` (default export). Single-microtask executor: validates ONE microtask spec,
//! lazily registers its prompt template, renders the prompt inputs, dispatches the AI call inside
//! a budget envelope, and decomposes the returned `patches`/`notes` output rows. No file I/O, no
//! other action calls, no DB writes beyond the lazy prompt registration — the parent orchestrator
//! owns persistence.
//!
//! PARITY-DEFERRED: the `aiCall(...)` leg (`crate::ai_ai_call::aiCall`) is the module's real work —
//! a live network round-trip to a BYOK AI provider — and is genuinely non-deterministic, not
//! JSON-fixturable (same convention as ai-call.rs's own stub and every dependent that calls it by
//! name). The twin calls the deferred stub, which always returns `Err(..)`; this twin's own error
//! wrapping (message format, duration_ms bookkeeping) around that call is ported faithfully, as is
//! the post-hoc cost-cap check and the `patches`/`notes` decomposition logic that would run on a
//! real `aiResult` once the provider dispatch is live. `performance.now()` is frozen (playbook
//! convention): `duration_ms` is hardcoded `0.0`.
//!
//! Everything else — the `!microtask` guard, `validateMicrotask` delegation (reused twin:
//! `crate::agents_dev_dev_plan_spec::validateMicrotask`), `ensurePrompt`'s lazy
//! promptFind/promptAdd registration (reused twins: `crate::ai_prompt_find::promptFind` /
//! `crate::ai_prompt_add::promptAdd`), the prompt-input string assembly (files_touched join,
//! numbered acceptance list, current_files block), and the `ai_call_outputs` patch decomposition
//! query — is ported byte-faithfully and exercised by the `#[cfg(test)]` suite below.
// QUALITY: 80/100 — deterministic surface (guards, ensurePrompt idempotency, prompt-input assembly, patch decomposition) fully ported + tested; aiCall leg is the deferred boundary (always Err via the stub, so the happy-path return object is unreachable but written faithfully for when the stub lands)

use rusqlite::Connection;
use serde_json::{json, Map, Value};

const DEFAULT_PROMPT_NAME: &str = "dev.exec.microtask.v1";

const EXEC_PROMPT_TEMPLATE: &str = "You are EXECUTING one microtask from a larger plan written by a smarter model. Stay in-bounds. Follow the spec exactly.

MICROTASK SPEC:
  id:               {{microtask_id}}
  intent:           {{intent}}
  files_touched:    {{files_touched}}
  acceptance:       {{acceptance}}
  expected_diff_shape: {{expected_diff_shape}}

Hard constraints:
  - Touch ONLY the files listed in files_touched. No exceptions.
  - Output a SINGLE JSON object matching this shape:
      {
        \"patches\": [
          { \"file\": \"<one of files_touched>\", \"operation\": \"create\"|\"replace\"|\"edit\", \"content\": \"<new full file content OR a unified diff>\" }
        ],
        \"notes\": \"<one short sentence about what you did>\"
      }
  - Do not explain. Do not narrate. JSON only.

Read the current state of files (if the parent gave you their contents in CURRENT_FILES below) before producing patches.

CURRENT_FILES:
{{current_files}}

Output JSON of the exact shape above.";

fn exec_output_fields() -> Value {
    json!([
        { "field_name": "patches", "field_type": "array_of_object", "required": true },
        { "field_name": "notes",   "field_type": "string",          "required": false },
    ])
}

fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}
// JS `${v}` interpolation.
fn tmpl(v: Option<&Value>) -> String {
    match v {
        None => "undefined".into(),
        Some(Value::Null) => "null".into(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(),
        Some(o) => o.to_string(),
    }
}
fn js_number(v: Option<&Value>) -> f64 {
    match v {
        None => f64::NAN,
        Some(Value::Null) => 0.0,
        Some(Value::Bool(b)) => if *b { 1.0 } else { 0.0 },
        Some(Value::Number(n)) => n.as_f64().unwrap_or(f64::NAN),
        Some(Value::String(s)) => { let t = s.trim(); if t.is_empty() { 0.0 } else { t.parse::<f64>().unwrap_or(f64::NAN) } },
        Some(_) => f64::NAN,
    }
}

/// `async function ensurePrompt(db)` — lazy registration: promptFind first, promptAdd + re-find
/// on ANY failure (a blanket try/catch in the JS, ported as a blanket `Err(_)` match here).
fn ensure_prompt(db: &Connection) -> Result<Value, String> {
    match crate::ai_prompt_find::promptFind(&json!({ "name": DEFAULT_PROMPT_NAME }), db) {
        Ok(v) => Ok(v),
        Err(_) => {
            crate::ai_prompt_add::promptAdd(
                &json!({
                    "name": DEFAULT_PROMPT_NAME,
                    "template_md": EXEC_PROMPT_TEMPLATE,
                    "default_tier": "low",
                    "default_temperature": "cold",
                    "max_output_tokens": 2048,
                    "output_fields": exec_output_fields(),
                }),
                db,
            )?;
            crate::ai_prompt_find::promptFind(&json!({ "name": DEFAULT_PROMPT_NAME }), db)
        }
    }
}

/// Renders the three prompt-input strings from the (already-validated) microtask + current_files:
/// `files_touched.join(', ')`, a numbered acceptance list, and the current_files block (or its
/// fallback sentence when empty).
fn build_prompt_inputs(microtask: &Value, current_files: &Value) -> (String, String, String) {
    let files_list = microtask
        .get("files_touched")
        .and_then(Value::as_array)
        .map(|a| a.iter().map(|f| tmpl(Some(f))).collect::<Vec<_>>().join(", "))
        .unwrap_or_default();

    let acceptance = microtask
        .get("acceptance")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .enumerate()
                .map(|(i, item)| format!("  {}. {}", i + 1, tmpl(Some(item))))
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default();

    let empty_map = Map::new();
    let entries = current_files.as_object().unwrap_or(&empty_map);
    let current_files_block = if entries.is_empty() {
        "(parent did not provide any current-file contents)".to_string()
    } else {
        entries
            .iter()
            .map(|(path, contents)| format!("── {path} ──\n{}", tmpl(Some(contents))))
            .collect::<Vec<_>>()
            .join("\n\n")
    };

    (files_list, acceptance, current_files_block)
}

/// Decomposes `aiResult.output_kv` into `(patches, notes)`, mirroring the JS's two-pass read:
/// scan the flat rows for `notes`, then re-query `brain.ai_call_outputs` for each `patches`
/// parent row's children (by `ai_call_id` + `parent_output_id`), keeping only fully-formed
/// `{file, content}` patches.
fn decompose_patches(db: &Connection, ai_call_id: &Value, output_kv: &Value) -> (Vec<Value>, String) {
    let empty: Vec<Value> = Vec::new();
    let rows = output_kv.as_array().unwrap_or(&empty);

    let mut notes = String::new();
    for row in rows {
        if row.get("field_name").and_then(Value::as_str) == Some("notes") && !row.get("value_text").map_or(true, Value::is_null) {
            notes = row.get("value_text").and_then(Value::as_str).unwrap_or_default().to_string();
        }
    }

    let mut parent_rows: Vec<&Value> = rows
        .iter()
        .filter(|o| {
            o.get("field_name").and_then(Value::as_str) == Some("patches")
                && o.get("value_text").map_or(true, Value::is_null)
                && o.get("value_number").map_or(true, Value::is_null)
                && o.get("value_boolean").map_or(true, Value::is_null)
        })
        .collect();
    parent_rows.sort_by(|a, b| {
        js_number(a.get("order_index"))
            .partial_cmp(&js_number(b.get("order_index")))
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let mut patches: Vec<Value> = Vec::new();
    for parent in parent_rows {
        let parent_id = parent.get("id").cloned().unwrap_or(Value::Null);
        let children = crate::db_json::query_rows(
            db,
            "SELECT field_name, value_text FROM brain.ai_call_outputs WHERE ai_call_id = ? AND parent_output_id = ?",
            &[ai_call_id.clone(), parent_id],
        ).unwrap_or(json!([]));
        let mut patch = Map::new();
        for c in children.as_array().unwrap_or(&empty) {
            if let Some(fname) = c.get("field_name").and_then(Value::as_str) {
                patch.insert(fname.to_string(), c.get("value_text").cloned().unwrap_or(Value::Null));
            }
        }
        if truthy(patch.get("file")) && truthy(patch.get("content")) {
            patches.push(Value::Object(patch));
        }
    }

    (patches, notes)
}

/// `export default async function devExecSandbox(input, context)`.
pub fn devExecSandbox(input: &Value, db: &Connection) -> Result<Value, String> {
    let empty_input = json!({});
    let input = input.as_object().map(|_| input).unwrap_or(&empty_input);
    let microtask = input.get("microtask").cloned().unwrap_or(Value::Null);
    let current_files = input.get("current_files").cloned().unwrap_or(json!({}));
    let tier = input.get("tier").and_then(Value::as_str).unwrap_or("low").to_string();

    if !truthy(Some(&microtask)) {
        return Err("dev-exec-sandbox: microtask is required".to_string());
    }

    let microtask_errors = crate::agents_dev_dev_plan_spec::validateMicrotask(&microtask, &json!(0));
    let errors_arr = microtask_errors.as_array().cloned().unwrap_or_default();
    if !errors_arr.is_empty() {
        let joined = errors_arr.iter().map(|e| tmpl(Some(e))).collect::<Vec<_>>().join("; ");
        return Err(format!("dev-exec-sandbox: microtask validation failed: {joined}"));
    }

    ensure_prompt(db)?;

    let (files_list, acceptance, current_files_block) = build_prompt_inputs(&microtask, &current_files);
    let microtask_id = microtask.get("microtask_id").cloned().unwrap_or(Value::Null);

    let ai_input = json!({
        "prompt_name": DEFAULT_PROMPT_NAME,
        "input_kv": [
            { "key": "microtask_id", "value": microtask_id },
            { "key": "intent", "value": microtask.get("intent").cloned().unwrap_or(Value::Null) },
            { "key": "files_touched", "value": files_list },
            { "key": "acceptance", "value": acceptance },
            { "key": "expected_diff_shape", "value": microtask.get("expected_diff_shape").cloned().unwrap_or(Value::Null) },
            { "key": "current_files", "value": current_files_block },
        ],
        "tier": tier,
        "temperature": "cold",
        "budget_remaining_usd": microtask.get("max_cost_usd").cloned().unwrap_or(Value::Null),
    });

    // performance.now() is frozen — `end - start` is always exactly 0.0.
    let duration_ms = 0.0_f64;
    let ai_result = match crate::ai_ai_call::aiCall(&ai_input, db) {
        Ok(v) => v,
        Err(e) => {
            let snippet: String = e.chars().take(200).collect();
            return Err(format!(
                "dev-exec-sandbox: microtask exec failed: {snippet}"
            ));
        }
    };

    let cost_usd = ai_result.get("cost_usd").and_then(Value::as_f64).unwrap_or(0.0);
    let max_cost_usd = js_number(microtask.get("max_cost_usd"));
    if cost_usd > max_cost_usd + 1e-6 {
        return Err(format!(
            "dev-exec-sandbox: microtask exceeded max_cost_usd: actual={cost_usd} cap={max_cost_usd}"
        ));
    }

    let output_kv = ai_result.get("output_kv").cloned().unwrap_or(json!([]));
    let ai_call_id = ai_result.get("ai_call_id").cloned().unwrap_or(Value::Null);
    let (patches, notes) = decompose_patches(db, &ai_call_id, &output_kv);
    let tokens_in = output_kv.as_array().map(|a| a.len()).unwrap_or(0) as i64;

    Ok(json!({
        "microtask_id": microtask_id,
        "patches": patches,
        "notes": notes,
        "duration_ms": duration_ms,
        "tokens_in": tokens_in,
        "cost_usd": cost_usd,
        "ai_call_id": ai_call_id,
        "cached": ai_result.get("cached").and_then(Value::as_bool).unwrap_or(false),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    const BRAIN_SCHEMA: &str = "
        CREATE TABLE brain.prompt_templates ( id INTEGER PRIMARY KEY, name TEXT NOT NULL, version INTEGER NOT NULL, template_md TEXT NOT NULL, default_tier TEXT NOT NULL CHECK (default_tier IN ('low','mid','high')), default_temperature TEXT NOT NULL CHECK (default_temperature IN ('freezed','cold','warm','hot','hell')), max_output_tokens INTEGER NOT NULL DEFAULT 1024, superseded_by INTEGER, created_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')), cross_field_rules_json TEXT, judging_criteria_json TEXT, system_md TEXT, phase_flow TEXT NOT NULL DEFAULT 'single', UNIQUE (name, version) );
        CREATE TABLE brain.prompt_output_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL CHECK (field_type IN ('string','number','boolean','enum','array_of_string','array_of_object')), enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, grounded_refs_json TEXT, UNIQUE (prompt_template_id, field_name) );
        CREATE TABLE brain.prompt_input_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL CHECK (field_type IN ('string','number','boolean','enum','array_of_string','array_of_object')), enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, UNIQUE (prompt_template_id, field_name) );
        CREATE TABLE brain.ai_call_outputs ( id INTEGER PRIMARY KEY, ai_call_id INTEGER NOT NULL, parent_output_id INTEGER, field_name TEXT NOT NULL, order_index INTEGER NOT NULL DEFAULT 0, value_text TEXT, value_number REAL, value_boolean INTEGER );
    ";

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS brain").unwrap();
        c.execute_batch(BRAIN_SCHEMA).unwrap();
        c
    }

    fn valid_microtask() -> Value {
        json!({
            "microtask_id": "m1", "intent": "do the thing",
            "files_touched": ["a.ts", "b.ts"],
            "acceptance": ["it compiles", "tests pass"],
            "max_tokens": 512, "max_cost_usd": 0.05,
            "expected_diff_shape": "small_edit",
        })
    }

    #[test]
    fn missing_microtask_is_rejected() {
        let c = db();
        let e = devExecSandbox(&json!({}), &c).unwrap_err();
        assert_eq!(e, "dev-exec-sandbox: microtask is required");
    }

    #[test]
    fn invalid_microtask_shape_is_rejected_with_joined_errors() {
        let c = db();
        let bad = json!({ "intent": "", "files_touched": [], "max_tokens": 0, "expected_diff_shape": "wat" });
        let e = devExecSandbox(&json!({ "microtask": bad }), &c).unwrap_err();
        assert!(e.starts_with("dev-exec-sandbox: microtask validation failed: "), "{e}");
        assert!(e.contains("microtask_id"), "{e}");
    }

    // The happy path is unreachable through the public fn (aiCall is the deferred stub, always
    // Err) — assert the wrapping this twin adds around that stub's message.
    #[test]
    fn ai_call_failure_is_wrapped_with_skill_prefix() {
        let c = db();
        let e = devExecSandbox(&json!({ "microtask": valid_microtask() }), &c).unwrap_err();
        assert!(e.starts_with("dev-exec-sandbox: microtask exec failed: "), "{e}");
        assert!(e.contains("ai-call"), "{e}");
    }

    #[test]
    fn ensure_prompt_registers_once_then_reuses() {
        let c = db();
        let r1 = ensure_prompt(&c).unwrap();
        assert_eq!(r1["name"], json!(DEFAULT_PROMPT_NAME));
        assert_eq!(r1["version"], json!(1));
        let count: i64 = c.query_row("SELECT COUNT(*) FROM brain.prompt_templates", [], |r| r.get(0)).unwrap();
        assert_eq!(count, 1);

        let r2 = ensure_prompt(&c).unwrap();
        assert_eq!(r2["version"], json!(1), "second call finds the existing active row, does not re-register");
        let count2: i64 = c.query_row("SELECT COUNT(*) FROM brain.prompt_templates", [], |r| r.get(0)).unwrap();
        assert_eq!(count2, 1);
    }

    #[test]
    fn build_prompt_inputs_renders_files_acceptance_and_current_files_block() {
        let m = valid_microtask();
        let (files, acc, block) = build_prompt_inputs(&m, &json!({ "a.ts": "const x = 1;" }));
        assert_eq!(files, "a.ts, b.ts");
        assert_eq!(acc, "  1. it compiles\n  2. tests pass");
        assert_eq!(block, "── a.ts ──\nconst x = 1;");
    }

    #[test]
    fn build_prompt_inputs_falls_back_when_current_files_empty() {
        let m = valid_microtask();
        let (_, _, block) = build_prompt_inputs(&m, &json!({}));
        assert_eq!(block, "(parent did not provide any current-file contents)");
    }

    #[test]
    fn build_prompt_inputs_joins_multiple_current_files_in_insertion_order() {
        let m = valid_microtask();
        let (_, _, block) = build_prompt_inputs(&m, &json!({ "a.ts": "1", "b.ts": "2" }));
        assert_eq!(block, "── a.ts ──\n1\n\n── b.ts ──\n2");
    }

    #[test]
    fn decompose_patches_reads_notes_and_valid_patch_children_only() {
        let c = db();
        c.execute_batch(
            "INSERT INTO brain.ai_call_outputs (id, ai_call_id, parent_output_id, field_name, order_index, value_text, value_number, value_boolean) VALUES
             (1, 7, NULL, 'notes', 0, 'did the thing', NULL, NULL),
             (2, 7, NULL, 'patches', 1, NULL, NULL, NULL),
             (3, 7, 2, 'file', 0, 'a.ts', NULL, NULL),
             (4, 7, 2, 'content', 1, 'new content', NULL, NULL),
             (5, 7, NULL, 'patches', 2, NULL, NULL, NULL),
             (6, 7, 5, 'file', 0, 'b.ts', NULL, NULL)"
        ).unwrap();
        let output_kv = json!([
            { "id": 1, "field_name": "notes", "value_text": "did the thing", "order_index": 0 },
            { "id": 2, "field_name": "patches", "value_text": null, "value_number": null, "value_boolean": null, "order_index": 1 },
            { "id": 5, "field_name": "patches", "value_text": null, "value_number": null, "value_boolean": null, "order_index": 2 },
        ]);
        let (patches, notes) = decompose_patches(&c, &json!(7), &output_kv);
        assert_eq!(notes, "did the thing");
        // Second "patches" parent's child never gets a `content` row → dropped (patch.file && patch.content).
        assert_eq!(patches, vec![json!({ "file": "a.ts", "content": "new content" })]);
    }

    #[test]
    fn decompose_patches_empty_output_kv_yields_no_patches_and_empty_notes() {
        let c = db();
        let (patches, notes) = decompose_patches(&c, &json!(1), &json!([]));
        assert!(patches.is_empty());
        assert_eq!(notes, "");
    }
}
