//! code-regenerate.rs — twin of skills/codebase/code-regenerate.mjs (db lane, cross-db brain.*).
//! NAMES ARE CONTRACT: default `codeRegenerate`. P1 of the regenerate-block epic: propose an AI
//! rewrite of a SELECTED block/line range, grounded in the surrounding code. PROPOSAL ONLY — it
//! reads the file and returns `{ original, proposed, rationale }`; it NEVER writes the file (the
//! sandbox apply + gated merge are code-regenerate-stage.rs / code-regenerate-merge.rs).
//!
//! `context.db` missing → JS throws `err.contextMissing('code-regenerate', 'db')`. Unreachable
//! here: `db: &Connection` is a required, non-nullable reference (same convention as every
//! sibling db-lane twin). `context.provider_fn` (a JS function reference for provider
//! selection/DI) has no Rust equivalent to carry — threaded as `null` into the constructed
//! aiCall input, same as book-run.rs's `provider_fn`.
//!
//! PARITY-DEFERRED: the AI call is dispatched through `crate::ai_ai_call::aiCall`, the deferred
//! network-AI stub (see ai-call.rs's module doc) — it always returns `Err`, so `codeRegenerate`
//! surfaces that as `{ ok: false, path, error }` (mirroring the JS try/catch around `aiCall`)
//! once the (real, ported) deterministic surface above it has run: input guards, the file read
//! via the already-converted `codebase_codebase_file_read::codebaseFileRead` twin, line-range clamping,
//! before/after context slicing, and `ensure_prompt` (reusing the already-converted
//! `ai_prompt_find`/`ai_prompt_add` twins, mirroring book-run.rs's `ensure_faculty`). The
//! response-picking helper (`pick`) is ported as its own PURE function and unit-tested directly
//! with a canned `output_kv` shape — unreachable from `codeRegenerate` itself (the deferred
//! `aiCall` always errs before it runs) but correct by construction, same pattern as
//! book-run.rs's `parse_arr`/`parse_obj`.
// QUALITY: mixed unit — deterministic assembly/db-read surface (fully ported, fixture-reachable
// up to the deferred aiCall boundary) + 1 pure response-picking helper (unreachable from the
// default export, unit-tested directly) + 1 bespoke/deferred aiCall dispatch (no fixture).
use rusqlite::Connection;
use serde_json::{json, Value};

// ── standard helpers (playbook) ─────────────────────────────────────────────────────────────
fn str_qq(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
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
        Some(Value::String(s)) => { let t = s.trim(); if t.is_empty() { 0.0 } else { t.parse().unwrap_or(f64::NAN) } }
        Some(_) => f64::NAN,
    }
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
fn err_obj(path: Option<&str>, msg: String) -> Value {
    match path {
        Some(p) => json!({ "ok": false, "path": p, "error": msg }),
        None => json!({ "ok": false, "error": msg }),
    }
}

const PROMPT: &str = "code.regenerate.v1";
const SYSTEM: &str = "You are a precise refactoring engine. You rewrite ONLY a specified block of code, preserving its behavior and public interface unless told otherwise, and you output strict JSON.";
const TEMPLATE: &str = "Rewrite the SELECTED block below. Return ONLY the replacement for the selected lines — match the surrounding indentation + style, keep the same public interface and behavior UNLESS the instruction says otherwise. Do NOT echo the context; output only the new version of the selected lines.\n\nFILE: {{path}}  (lines {{range}})\n\nCONTEXT BEFORE:\n{{before}}\n\n>>> SELECTED BLOCK (rewrite this) >>>\n{{original}}\n<<< END SELECTED BLOCK <<<\n\nCONTEXT AFTER:\n{{after}}\n\nINSTRUCTION: {{instruction}}\n\nReturn JSON exactly as: {\"proposed_code\":\"<the rewritten selected lines>\",\"rationale\":\"<one sentence: what changed and why>\"}";

// mirrors JS `ensurePrompt`: `try { existing = promptFind(...) } catch { existing = null }`,
// skip re-registering when the template text is unchanged, else `promptAdd`.
fn ensure_prompt(db: &Connection) -> Result<(), String> {
    let existing = crate::ai_prompt_find::promptFind(&json!({ "name": PROMPT }), db).ok();
    if let Some(row) = existing.as_ref() {
        if row.get("template_md").and_then(Value::as_str) == Some(TEMPLATE) {
            return Ok(());
        }
    }
    crate::ai_prompt_add::promptAdd(
        &json!({
            "name": PROMPT,
            "template_md": TEMPLATE,
            "system_md": SYSTEM,
            "default_tier": "mid",
            "default_temperature": "cold",
            "phase_flow": "single_only",
            "max_output_tokens": 2000,
            "output_fields": [
                { "field_name": "proposed_code", "field_type": "string", "required": true, "min_length": 1 },
                { "field_name": "rationale", "field_type": "string" },
            ],
        }),
        db,
    )?;
    Ok(())
}

// `res?.output_kv?.find((o) => o.field_name === name && o.parent_output_id == null)?.value_text
// ?? null` — `== null` is JS loose-eq, true for both absent-key and explicit `null`.
fn pick(res: &Value, name: &str) -> Option<Value> {
    let rows = res.get("output_kv")?.as_array()?;
    for row in rows {
        let field = row.get("field_name").and_then(Value::as_str);
        let parent_is_null = row.get("parent_output_id").map_or(true, |v| v.is_null());
        if field == Some(name) && parent_is_null {
            let val = row.get("value_text").cloned().unwrap_or(Value::Null);
            if !val.is_null() {
                return Some(val);
            }
        }
    }
    None
}

pub fn codeRegenerate(input: &Value, db: &Connection) -> Value {
    let provider_fn = Value::Null;
    let path = str_qq(input.get("path"));
    let start_num = js_number(input.get("start_line"));
    let end_num = {
        let e = input.get("end_line");
        if truthy(e) { js_number(e) } else { js_number(input.get("start_line")) }
    };
    if path.is_empty() || start_num == 0.0 || start_num.is_nan() {
        return err_obj(None, "need { path, start_line, end_line }".to_string());
    }

    let read_input = json!({ "path": path, "root": input.get("root").cloned().unwrap_or(Value::Null) });
    let content = match crate::codebase_codebase_file_read::codebaseFileRead(&read_input, db) {
        Ok(v) => v.get("content").and_then(Value::as_str).unwrap_or("").to_string(),
        Err(e) => {
            let msg: String = e.chars().take(120).collect();
            return err_obj(Some(&path), format!("unreadable: {msg}"));
        }
    };
    let lines: Vec<&str> = content.split('\n').collect();
    let n = lines.len() as i64;
    let lo = (1i64).max(start_num as i64);
    let hi = n.min(lo.max(end_num as i64));
    if hi < 1 {
        // n == 0 (empty file split gives one empty-string element, so this is effectively
        // unreachable, matching JS's Math.min(lines.length, ...) never going below 1 in practice)
    }
    let lo_idx = (lo - 1).max(0) as usize;
    let hi_idx = hi.max(0) as usize;
    let original = lines.get(lo_idx..hi_idx.min(lines.len())).unwrap_or(&[]).join("\n");
    if original.trim().is_empty() {
        return err_obj(Some(&path), "the selected range is empty".to_string());
    }
    let before_lo = ((lo_idx as i64) - 24).max(0) as usize;
    let before = lines.get(before_lo..lo_idx.min(lines.len())).unwrap_or(&[]).join("\n");
    let after_hi = (hi_idx + 24).min(lines.len());
    let after = lines.get(hi_idx.min(lines.len())..after_hi).unwrap_or(&[]).join("\n");

    if let Err(e) = ensure_prompt(db) {
        return err_obj(Some(&path), e.chars().take(200).collect());
    }

    let instruction = {
        let i = input.get("instruction");
        if truthy(i) { str_qq(i) } else { "(refactor for clarity; preserve behavior)".to_string() }
    };
    let ai_input = json!({
        "prompt_name": PROMPT,
        "input_kv": [
            { "key": "path", "value": path },
            { "key": "range", "value": format!("{lo}-{hi}") },
            { "key": "before", "value": before },
            { "key": "after", "value": after },
            { "key": "original", "value": original },
            { "key": "instruction", "value": instruction },
        ],
        "tier": "mid", "temperature": "cold", "phase_flow": "single_only", "provider_fn": provider_fn,
    });
    let res = match crate::ai_ai_call::aiCall(&ai_input, db) {
        Ok(v) => v,
        Err(e) => {
            let msg: String = e.chars().take(200).collect();
            return err_obj(Some(&path), msg);
        }
    };
    let proposed = pick(&res, "proposed_code");
    let rationale = pick(&res, "rationale");
    let proposed_str = proposed.as_ref().map(|v| str_qq(Some(v))).unwrap_or_default();
    if proposed_str.trim().is_empty() {
        return err_obj(Some(&path), "the model returned no replacement".to_string());
    }
    json!({
        "ok": true, "path": path, "start_line": lo, "end_line": hi,
        "original": original, "proposed": proposed_str,
        "rationale": rationale.unwrap_or(Value::Null),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS brain;").unwrap();
        c.execute_batch(
            "CREATE TABLE workspaces (id INTEGER PRIMARY KEY AUTOINCREMENT, root_path TEXT NOT NULL, is_active INTEGER NOT NULL DEFAULT 0);
             CREATE TABLE brain.prompt_templates ( id INTEGER PRIMARY KEY, name TEXT NOT NULL, version INTEGER NOT NULL, template_md TEXT NOT NULL, default_tier TEXT NOT NULL, default_temperature TEXT NOT NULL, max_output_tokens INTEGER NOT NULL DEFAULT 1024, superseded_by INTEGER, created_at TEXT NOT NULL DEFAULT (datetime('now')), cross_field_rules_json TEXT, judging_criteria_json TEXT, system_md TEXT, phase_flow TEXT NOT NULL DEFAULT 'single', UNIQUE (name, version) );
             CREATE TABLE brain.prompt_output_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, grounded_refs_json TEXT, UNIQUE (prompt_template_id, field_name) );
             CREATE TABLE brain.prompt_input_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, UNIQUE (prompt_template_id, field_name) );",
        ).unwrap();
        c
    }

    #[test]
    fn missing_required_input_returns_error() {
        let c = setup();
        let r = codeRegenerate(&json!({}), &c);
        assert_eq!(r["ok"], json!(false));
        assert_eq!(r["error"], json!("need { path, start_line, end_line }"));
        let r2 = codeRegenerate(&json!({ "path": "a.rs" }), &c);
        assert_eq!(r2["error"], json!("need { path, start_line, end_line }"));
    }

    #[test]
    fn unreadable_file_reports_unreadable_error() {
        let c = setup();
        let r = codeRegenerate(&json!({ "path": "nope.rs", "start_line": 1, "root": "C:/nowhere-xyz" }), &c);
        assert_eq!(r["ok"], json!(false));
        assert_eq!(r["path"], json!("nope.rs"));
        assert!(r["error"].as_str().unwrap().starts_with("unreadable:"), "{r}");
    }

    #[test]
    fn empty_selected_range_reports_error() {
        let c = setup();
        let dir = std::env::temp_dir().join(format!("antiloki-code-regen-rs-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("f.rs"), "\n\n\n").unwrap();
        let r = codeRegenerate(&json!({ "path": "f.rs", "start_line": 1, "end_line": 1, "root": dir.to_string_lossy() }), &c);
        assert_eq!(r["ok"], json!(false));
        assert_eq!(r["error"], json!("the selected range is empty"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reaches_the_parity_deferred_aicall_boundary_and_ensures_the_prompt() {
        // proves every deterministic gate (guard/read/range-clamp/context-slice/prompt-ensure) ran
        // cleanly and the error is aiCall's deferred stub, not an earlier bug.
        let c = setup();
        let dir = std::env::temp_dir().join(format!("antiloki-code-regen-rs2-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("f.rs"), "line1\nline2\nline3\nline4\nline5\n").unwrap();
        let r = codeRegenerate(&json!({ "path": "f.rs", "start_line": 2, "end_line": 3, "root": dir.to_string_lossy() }), &c);
        assert_eq!(r["ok"], json!(false));
        assert_eq!(r["path"], json!("f.rs"));
        assert!(r["error"].as_str().unwrap().contains("ai-call"), "unexpected error: {r}");
        let n: i64 = c.query_row("SELECT COUNT(*) FROM brain.prompt_templates WHERE name = 'code.regenerate.v1'", [], |row| row.get(0)).unwrap();
        assert_eq!(n, 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ensure_prompt_skips_reinsert_when_template_unchanged() {
        let c = setup();
        ensure_prompt(&c).unwrap();
        ensure_prompt(&c).unwrap();
        let n: i64 = c.query_row("SELECT COUNT(*) FROM brain.prompt_templates WHERE name = 'code.regenerate.v1'", [], |row| row.get(0)).unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn pick_reads_top_level_field_only() {
        let res = json!({
            "output_kv": [
                { "field_name": "rationale", "value_text": "nested", "parent_output_id": 5 },
                { "field_name": "rationale", "value_text": "top", "parent_output_id": null },
                { "field_name": "proposed_code", "value_text": Value::Null },
            ]
        });
        assert_eq!(pick(&res, "rationale"), Some(json!("top")));
        assert_eq!(pick(&res, "proposed_code"), None);
        assert_eq!(pick(&res, "missing"), None);
    }
}
