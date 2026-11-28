//! code-docs-overview.rs — twin of skills/codebase/code-docs-overview.mjs (db lane, cross-db
//! brain.*). NAMES ARE CONTRACT: default `codeDocsOverview`. Feeds the scope's FULL deterministic
//! documentation (from `codeDocsBuild`) to an AI call that synthesizes a grounded overview_md.
//!
//! `context.db` missing → JS throws `err.contextMissing('code-docs-overview', 'db')`. Unreachable
//! here: `db: &Connection` is a required, non-nullable reference (same convention as every sibling
//! db-lane twin). `context.provider_fn` (a JS function reference) has no Rust equivalent to carry —
//! threaded as `null` into the constructed aiCall input, same as pl-research.rs's `provider_fn`.
//!
//! PARITY-DEFERRED: the AI call is dispatched through `crate::ai_ai_call::aiCall`, the deferred
//! network-AI stub (see ai-call.rs's module doc) — it always returns `Err`, so `codeDocsOverview`
//! catches that error and returns the JS's own `overview unavailable: ...` shape (JS wraps the
//! `aiCall` in try/catch, never propagating the error), once the (real, ported) deterministic
//! surface above it has run: guard, `codeDocsBuild` doc fetch (reusing the already-converted
//! `codebase_code_docs_build::codeDocsBuild` twin, try/catch → empty string on any failure), the
//! no-doc early return, `scopeLabel` derivation, and `ensure_prompt` (reusing the already-converted
//! `ai_prompt_find`/`ai_prompt_add` twins, mirroring pl-research.rs's `ensure_prompt` — including
//! its re-add-on-template-drift branch, unlike book-run.rs's simpler miss-only variant). The
//! response-parsing `pick` helper is ported as its own PURE function and unit-tested directly with
//! canned `res.output_kv` values — unreachable from `codeDocsOverview` itself (aiCall's `Err` is
//! caught before `pick` ever runs) but correct by construction and independently testable per the
//! crossing playbook's coverage rule (same pattern as pl-research.rs's `build_research_report`).
// QUALITY: mixed unit — deterministic assembly/db-read surface (fully ported, fixture-reachable up
// to the deferred aiCall boundary, including its caught-error path) + 1 pure response-shaping
// helper (unreachable from the default export, unit-tested directly) + 1 bespoke/deferred aiCall
// dispatch (no fixture/frontier).
use regex::Regex;
use rusqlite::Connection;
use serde_json::{json, Value};

// ── standard helpers (playbook) ─────────────────────────────────────────────────────────────
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
} // = JS String(v ?? '')

const PROMPT: &str = "code.docs.overview.v3";
const SYSTEM: &str = "You are a staff engineer writing a concise OVERVIEW / code review of a piece of code (a whole codebase, a folder, or a single file) for a new team member. You are given the scope's FULL deterministic documentation. Write a clear, specific markdown overview that helps someone understand it fast. Ground STRICTLY in the documentation — never invent behaviour or names. No code fences.";
const TEMPLATE: &str = "Write an overview / code review of {{scope}} for a new team member, based on its DOCUMENTATION below. Use a few short, specific markdown sections appropriate to the scope (e.g. What this is / Architecture / Patterns & conventions / Health & risks / Key parts / Where to start). Ground STRICTLY in the documentation. No code fences.

DOCUMENTATION
{{doc}}

Put the whole overview into overview_md.";

fn prompt_output_fields() -> Value {
    json!([{ "field_name": "overview_md", "field_type": "string" }])
}

// `if (existing && existing.template_md === TEMPLATE) return;` else promptAdd (covers: not found,
// promptFind errored, or found-but-drifted template).
fn ensure_prompt(db: &Connection) -> Result<(), String> {
    let existing = crate::ai_prompt_find::promptFind(&json!({ "name": PROMPT }), db).ok();
    if let Some(e) = &existing {
        if e.get("template_md").and_then(Value::as_str) == Some(TEMPLATE) {
            return Ok(());
        }
    }
    crate::ai_prompt_add::promptAdd(
        &json!({
            "name": PROMPT,
            "template_md": TEMPLATE,
            "system_md": SYSTEM,
            "default_tier": "low",
            "default_temperature": "cold",
            "phase_flow": "single_only",
            "max_output_tokens": 1400,
            "output_fields": prompt_output_fields(),
        }),
        db,
    )?;
    Ok(())
}

// Pure — `res?.output_kv?.find(o => o.field_name === 'overview_md' && o.parent_output_id == null)
// ?.value_text ?? null`
fn pick(res: &Value) -> Value {
    let found = res.get("output_kv").and_then(Value::as_array).and_then(|rows| {
        rows.iter().find(|o| {
            o.get("field_name").and_then(Value::as_str) == Some("overview_md")
                && matches!(o.get("parent_output_id"), None | Some(Value::Null))
        })
    });
    match found.and_then(|o| o.get("value_text")) {
        Some(v) if !v.is_null() => v.clone(),
        _ => Value::Null,
    }
}

pub fn codeDocsOverview(input: &Value, db: &Connection) -> Value {
    let origin_v = input.get("origin").filter(|v| !v.is_null()).cloned().unwrap_or(json!("atlas"));
    let scope = match input.get("path").filter(|v| truthy(Some(v))) {
        Some(v) => v.clone(),
        None => Value::Null,
    };

    // the scope's FULL deterministic documentation = the AI's context (the rest of the doc).
    let scope_arg = if truthy(Some(&scope)) { str_qq(Some(&scope)) } else { String::new() };
    let doc = match crate::codebase_code_docs_build::codeDocsBuild(
        &json!({ "page": scope_arg, "format": "md", "origin": origin_v.clone() }),
        db,
    ) {
        Ok(r) => r.get("markdown").and_then(Value::as_str).map(|s| s.to_string()).unwrap_or_default(),
        Err(_) => String::new(),
    };
    if doc.trim().is_empty() {
        let scope_label = if truthy(Some(&scope)) { str_qq(Some(&scope)) } else { "codebase".to_string() };
        return json!({
            "origin": origin_v,
            "scope": scope,
            "overview_md": Value::Null,
            "error": format!("no documentation for \"{scope_label}\" — rebuild the atlas"),
        });
    }
    let has_ext = truthy(Some(&scope))
        && Regex::new(r"\.[a-zA-Z]+$").unwrap().is_match(&str_qq(Some(&scope)));
    let scope_label = if truthy(Some(&scope)) {
        if has_ext { format!("the file {}", str_qq(Some(&scope))) } else { format!("the folder {}/", str_qq(Some(&scope))) }
    } else {
        "the whole codebase".to_string()
    };

    if let Err(e) = ensure_prompt(db) {
        // JS ensurePrompt() is unguarded (not in the outer try) — a real promptAdd failure would
        // throw uncaught in JS too. Mirror that: propagate as the caught-shape error message so the
        // twin never panics, since Rust has no uncaught-exception equivalent for a `Result` chain.
        return json!({
            "origin": origin_v, "scope": scope, "overview_md": Value::Null, "model_id": Value::Null,
            "error": format!("overview unavailable: {}", e.chars().take(120).collect::<String>()),
        });
    }

    // ── the AI call: PARITY-DEFERRED from here on (see module doc) — aiCall always returns Err,
    // and JS's try/catch around this call means the twin must catch it the same way (unlike
    // pl-research.rs, which propagates via `?`).
    let ai_input = json!({
        "prompt_name": PROMPT,
        "input_kv": [
            { "key": "scope", "value": scope_label },
            { "key": "doc", "value": doc.chars().take(14000).collect::<String>() },
        ],
        "tier": input.get("tier").filter(|v| truthy(Some(v))).cloned().unwrap_or(json!("low")),
        "temperature": "cold",
        "phase_flow": "single_only",
        "provider_fn": Value::Null,
    });
    match crate::ai_ai_call::aiCall(&ai_input, db) {
        Ok(res) => json!({
            "origin": origin_v, "scope": scope, "overview_md": pick(&res),
            "model_id": res.get("model_id").filter(|v| !v.is_null()).cloned().unwrap_or(Value::Null),
        }),
        Err(e) => json!({
            "origin": origin_v, "scope": scope, "overview_md": Value::Null, "model_id": Value::Null,
            "error": format!("overview unavailable: {}", e.chars().take(120).collect::<String>()),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS brain;").unwrap();
        c.execute_batch(
            "CREATE TABLE code_decorators (path TEXT, name TEXT, kind TEXT, role TEXT, line INTEGER, end_line INTEGER, signature TEXT, summary TEXT, inputs_json TEXT, output_json TEXT, effects_json TEXT, calls_json TEXT, uses_json TEXT, complexity INTEGER, flags_json TEXT, steps_json TEXT, origin TEXT);
             CREATE TABLE brain.prompt_templates ( id INTEGER PRIMARY KEY, name TEXT NOT NULL, version INTEGER NOT NULL, template_md TEXT NOT NULL, default_tier TEXT NOT NULL, default_temperature TEXT NOT NULL, max_output_tokens INTEGER NOT NULL DEFAULT 1024, superseded_by INTEGER, created_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')), cross_field_rules_json TEXT, judging_criteria_json TEXT, system_md TEXT, phase_flow TEXT NOT NULL DEFAULT 'single', UNIQUE (name, version) );
             CREATE TABLE brain.prompt_output_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, grounded_refs_json TEXT, UNIQUE (prompt_template_id, field_name) );
             CREATE TABLE brain.prompt_input_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, UNIQUE (prompt_template_id, field_name) );",
        ).unwrap();
        c
    }

    #[test]
    fn no_documentation_returns_error_shape_without_touching_prompt() {
        let c = setup();
        let out = codeDocsOverview(&json!({ "path": "no/such/file.mjs" }), &c);
        assert_eq!(out["overview_md"], Value::Null);
        assert_eq!(out["error"], json!("no documentation for \"no/such/file.mjs\" — rebuild the atlas"));
        let n: i64 = c.query_row("SELECT COUNT(*) FROM brain.prompt_templates", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn no_documentation_whole_codebase_scope_label() {
        let c = setup();
        let out = codeDocsOverview(&json!({}), &c);
        assert_eq!(out["error"], json!("no documentation for \"codebase\" — rebuild the atlas"));
        assert_eq!(out["scope"], Value::Null);
        assert_eq!(out["origin"], json!("atlas"));
    }

    #[test]
    fn reaches_the_parity_deferred_aicall_boundary_and_catches_it() {
        // proves guard/doc-fetch/scope-label/ensure_prompt all ran cleanly and the caught error is
        // aiCall's deferred stub, shaped exactly like JS's catch block.
        let c = setup();
        c.execute("INSERT INTO code_decorators (path, name, kind, role, line, end_line, signature, summary, inputs_json, output_json, effects_json, calls_json, uses_json, complexity, flags_json, steps_json, origin) VALUES ('a.mjs','foo','function','logic',1,5,'foo()','does foo','[]','null','[]','[]','[]',1,'[]','[]','atlas')", []).unwrap();
        let out = codeDocsOverview(&json!({ "path": "a.mjs" }), &c);
        assert_eq!(out["overview_md"], Value::Null);
        assert_eq!(out["model_id"], Value::Null);
        let err = out["error"].as_str().unwrap();
        assert!(err.starts_with("overview unavailable: "), "unexpected error: {err}");
        let n: i64 = c.query_row("SELECT COUNT(*) FROM brain.prompt_templates WHERE name = 'code.docs.overview.v3'", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn ensure_prompt_reinserts_on_template_drift_but_not_on_match() {
        let c = setup();
        ensure_prompt(&c).unwrap();
        let n1: i64 = c.query_row("SELECT COUNT(*) FROM brain.prompt_templates WHERE name = ?", [PROMPT], |r| r.get(0)).unwrap();
        assert_eq!(n1, 1);
        // matching template → no-op, still 1 row
        ensure_prompt(&c).unwrap();
        let n2: i64 = c.query_row("SELECT COUNT(*) FROM brain.prompt_templates WHERE name = ?", [PROMPT], |r| r.get(0)).unwrap();
        assert_eq!(n2, 1);
        // drift the stored template, then ensure_prompt re-adds a new version
        c.execute("UPDATE brain.prompt_templates SET template_md = 'drifted' WHERE name = ?", [PROMPT]).unwrap();
        ensure_prompt(&c).unwrap();
        let n3: i64 = c.query_row("SELECT COUNT(*) FROM brain.prompt_templates WHERE name = ?", [PROMPT], |r| r.get(0)).unwrap();
        assert_eq!(n3, 2);
    }

    #[test]
    fn scope_label_file_vs_folder_vs_whole_codebase() {
        // exercised indirectly via the no-doc error path (cheap, deterministic, no aiCall needed):
        // file scope keeps `error` phrased around the raw path; folder/whole-codebase differ only
        // in the aiCall input_kv scope value, which is unreachable pre-aiCall — so assert the regex
        // directly, matching JS's `/\.[a-zA-Z]+$/.test(scope)`.
        let re = Regex::new(r"\.[a-zA-Z]+$").unwrap();
        assert!(re.is_match("skills/foo/bar.mjs"));
        assert!(!re.is_match("skills/foo/bar"));
        assert!(!re.is_match("skills/foo/"));
    }

    #[test]
    fn pick_extracts_overview_md_from_top_level_row_only() {
        let res = json!({ "output_kv": [
            { "field_name": "overview_md", "value_text": "# Overview\ntext", "parent_output_id": null },
        ] });
        assert_eq!(pick(&res), json!("# Overview\ntext"));

        // nested row (has a parent_output_id) is excluded
        let nested = json!({ "output_kv": [
            { "field_name": "overview_md", "value_text": "nested", "parent_output_id": 3 },
        ] });
        assert_eq!(pick(&nested), Value::Null);

        // no rows at all
        assert_eq!(pick(&json!({})), Value::Null);

        // value_text explicitly null
        let null_val = json!({ "output_kv": [
            { "field_name": "overview_md", "value_text": null, "parent_output_id": null },
        ] });
        assert_eq!(pick(&null_val), Value::Null);
    }
}
