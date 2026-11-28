//! blueprint-describe.rs — twin of skills/blueprint/blueprint-describe.mjs (db + LLM-PARTIAL; cross-
//! db via brain.prompt_templates). NAMES ARE CONTRACT: default `blueprintDescribe`. The CHANGE
//! narrative (LOW-AI essay, cached): a deterministic gather of the change facts (the touched files +
//! what each does + the symbols it introduces + the delta, from `blueprint-get`'s output) hashed and
//! looked up in code_descriptions under origin=`blueprint:<slug>`, path=`(change)`; on a cache miss a
//! LOW model narrates what the change accomplishes.
//!
//! PARITY-DEFERRED: the `aiCall` leg (`crate::ai_ai_call::aiCall` — see that module's doc) always
//! returns Err, exactly mirroring the JS `try { ... } catch { summary_md = null; }` around the real
//! network call — this twin ports that same catch so the deferred boundary never propagates past
//! this function. Everything else is ported faithfully and IS the deterministic surface these tests
//! exercise: the `contract_slug` guard, the `blueprint-get` read + its `blueprint === null` early
//! return, the `changeFacts` gather (own private fn, directly unit-tested), the content-hash cache
//! lookup/short-circuit via `codebase_atlas_ingest::hashSource`, prompt-template ensure (reusing the
//! already-converted `ai_prompt_find`/`ai_prompt_add` twins), the `pick` response-parsing helper (fed
//! a canned `ai_calls`-shaped response, unit-tested directly since aiCall's Err short-circuits before
//! it is ever reached from the live call site), and the code_descriptions upsert (unreachable by this
//! file's own fixture — like book-run.rs's post-wave logic — because it runs only after a successful
//! aiCall, which the deferred stub never produces; correctness there is by inspection + the same
//! shape as blueprint-get.rs's read of the same row). `blueprint-get` reused via the already-converted
//! `blueprint_blueprint_get::blueprintGet` twin.
use rusqlite::Connection;
use serde_json::{json, Map, Value};

// ── standard helpers (playbook) ─────────────────────────────────────────────────────────────
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
fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}

const PROMPT: &str = "blueprint.describe.v1";
const SYSTEM: &str = "You are a precise software archivist describing a planned CHANGE. Given the files it touches, what each does, and the symbols it introduces, you write a SHORT plain-English narrative of what the change accomplishes conceptually and how it fits the system. State ONLY what the facts support — never invent. No code, no headers.";
const TEMPLATE: &str = "Write a 2–4 sentence conceptual description of what this CHANGE accomplishes and how it fits the system — grounded ONLY in the facts. Lead with the goal of the change, then its shape (which areas, what's new). Plain prose, no bullet points, no code.\n\nCHANGE: {{n_files}} file(s) — {{added}} new, {{changed}} changed.\nFILES:\n{{files}}\n\nPut the description into summary_md.";

pub fn change_origin(slug: &str) -> String { format!("blueprint:{slug}") }
pub const CHANGE_PATH: &str = "(change)";

// JS: `try { existing = promptFind(...) } catch { existing = null }; if (existing?.template_md ===
// TEMPLATE) return; promptAdd(...)`. promptFind's Err (not-found) is swallowed exactly like the JS
// catch; any other promptAdd Err propagates (JS does not catch promptAdd).
fn ensure_prompt(db: &Connection) -> Result<(), String> {
    let existing = crate::ai_prompt_find::promptFind(&json!({ "name": PROMPT }), db).ok();
    if let Some(e) = &existing {
        if e.get("template_md").and_then(|v| v.as_str()) == Some(TEMPLATE) {
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
            "max_output_tokens": 600,
            "output_fields": [{ "field_name": "summary_md", "field_type": "string" }],
        }),
        db,
    )?;
    Ok(())
}

// `res?.output_kv?.find(o => o.field_name === 'summary_md' && o.parent_output_id == null)?.value_text
// ?? null` — `== null` matches both JSON null and an absent key.
fn pick(res: &Value) -> Option<String> {
    let out_kv = res.get("output_kv")?.as_array()?;
    out_kv
        .iter()
        .find(|o| {
            o.get("field_name").and_then(|v| v.as_str()) == Some("summary_md")
                && matches!(o.get("parent_output_id"), None | Some(Value::Null))
        })
        .and_then(|o| o.get("value_text"))
        .filter(|v| !v.is_null())
        .map(js_string)
}

struct Facts { version: Value, n_files: i64, added: i64, changed: i64, files: Vec<Value> }

// JS: group `bp.design_docs` by `.path ?? '(unknown)'`; per group, `intent` from the matching
// top-level `bp.blocks` entry (`.find(b => b.path === p)?.intent ?? null`), `symbols` as
// `${name ?? '?'}${signature ?? ''}` per doc, `status` from the group's first doc.
fn change_facts(bp: &Value) -> Facts {
    let empty = Vec::new();
    let docs = bp.get("design_docs").and_then(|v| v.as_array()).unwrap_or(&empty);
    let mut by_file: std::collections::BTreeMap<String, Vec<&Value>> = std::collections::BTreeMap::new();
    for d in docs {
        // `d.path ?? '(unknown)'` — `??` only falls back on null/undefined, NOT on an empty string.
        let p = match d.get("path") {
            None | Some(Value::Null) => "(unknown)".to_string(),
            Some(v) => js_string(v),
        };
        by_file.entry(p).or_default().push(d);
    }
    let blocks = bp.get("blocks").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    let intent_of = |p: &str| -> Value {
        blocks
            .iter()
            .find(|b| b.get("path").and_then(|v| v.as_str()) == Some(p))
            .and_then(|b| b.get("intent").cloned())
            .filter(|v| !v.is_null())
            .unwrap_or(Value::Null)
    };
    let mut files: Vec<Value> = Vec::new();
    for (p, group) in &by_file {
        let symbols: Vec<Value> = group
            .iter()
            .map(|s| {
                let name = s.get("name").filter(|v| !v.is_null()).map(js_string).unwrap_or_else(|| "?".to_string());
                let sig = s.get("signature").filter(|v| !v.is_null()).map(js_string).unwrap_or_default();
                json!(format!("{name}{sig}"))
            })
            .collect();
        let status = group.first().and_then(|d| d.get("status").cloned()).filter(|v| !v.is_null()).unwrap_or(Value::Null);
        files.push(json!({ "path": p, "intent": intent_of(p), "symbols": symbols, "status": status }));
    }
    let c = bp.get("delta").and_then(|d| d.get("counts")).cloned().unwrap_or(Value::Null);
    let added = c.get("added").and_then(|v| v.as_i64()).unwrap_or(0);
    let changed = c.get("changed").and_then(|v| v.as_i64()).unwrap_or(0);
    Facts { version: bp.get("version").cloned().unwrap_or(Value::Null), n_files: files.len() as i64, added, changed, files }
}

fn facts_json(f: &Facts) -> Value {
    json!({ "version": f.version, "n_files": f.n_files, "added": f.added, "changed": f.changed, "files": f.files })
}

pub fn blueprintDescribe(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS: `const { db, provider_fn = null } = context ?? {}; if (!db) throw err.contextMissing(...)`
    // — unreachable here (Rust's `db: &Connection` is never absent). provider_fn is a JS function
    // reference threaded opaquely into aiCall's input; passed as `null` below (aiCall's dispatch
    // itself is the deferred boundary — there is no Rust equivalent to carry).
    let provider_fn = Value::Null;
    let slug = input.get("contract_slug").cloned().unwrap_or(Value::Null);
    if !truthy(Some(&slug)) {
        return Err("blueprint-describe: contract_slug is required".into());
    }
    let slug_s = js_string(&slug);

    let bp = crate::blueprint_blueprint_get::blueprintGet(&json!({ "contract_slug": slug.clone() }), db)?;
    if matches!(bp.get("blueprint"), Some(Value::Null)) {
        return Ok(json!({ "contract_slug": slug, "summary_md": Value::Null, "cached": false, "reason": "no_blueprint" }));
    }

    let facts = change_facts(&bp);
    let hash = crate::codebase_atlas_ingest::hashSource(&serde_json::to_string(&facts_json(&facts)).map_err(|e| e.to_string())?);
    let origin = change_origin(&slug_s);
    let path = CHANGE_PATH;
    let rows = crate::db_json::query_rows(
        db,
        "SELECT summary_md, content_hash FROM code_descriptions WHERE origin = ? AND path = ?",
        &[json!(origin), json!(path)],
    )?;
    let row = rows.as_array().and_then(|a| a.first()).cloned();
    let force = truthy(input.get("force"));
    if !force {
        if let Some(r) = &row {
            let has_summary = r.get("summary_md").filter(|v| !v.is_null() && v.as_str() != Some("")).is_some();
            let hash_matches = r.get("content_hash").and_then(|v| v.as_str()) == Some(hash.as_str());
            if has_summary && hash_matches {
                return Ok(json!({ "contract_slug": slug, "summary_md": r.get("summary_md").cloned().unwrap_or(Value::Null), "cached": true }));
            }
        }
    }

    ensure_prompt(db)?;
    let mut summary_md: Option<String> = None;
    let mut model_id: Value = Value::Null;
    {
        let files_block = facts
            .files
            .iter()
            .map(|f| {
                let path = js_string(f.get("path").unwrap_or(&Value::Null));
                let status = f.get("status").filter(|v| !v.is_null()).map(js_string).unwrap_or_else(|| "touched".to_string());
                let intent = f.get("intent").filter(|v| !v.is_null()).map(|v| format!(" — {}", js_string(v))).unwrap_or_default();
                let symbols: Vec<String> = f.get("symbols").and_then(|v| v.as_array()).map(|a| a.iter().map(js_string).collect()).unwrap_or_default();
                let syms = if symbols.is_empty() { String::new() } else { format!(" (defines {})", symbols.join(", ")) };
                format!("- {path} [{status}]{intent}{syms}")
            })
            .collect::<Vec<_>>()
            .join("\n");
        let files_block = if files_block.is_empty() { "(none)".to_string() } else { files_block };

        let ai_input = json!({
            "prompt_name": PROMPT,
            "input_kv": [
                { "key": "n_files", "value": facts.n_files.to_string() },
                { "key": "added", "value": facts.added.to_string() },
                { "key": "changed", "value": facts.changed.to_string() },
                { "key": "files", "value": files_block },
            ],
            "tier": "low", "temperature": "cold", "phase_flow": "single_only", "provider_fn": provider_fn,
        });
        // PARITY-DEFERRED: mirrors JS `try { const res = await aiCall(...); ... } catch { summary_md
        // = null; }` — aiCall's Err (always, per its module doc) is swallowed here exactly as the JS
        // catch swallows a live-call failure.
        if let Ok(res) = crate::ai_ai_call::aiCall(&ai_input, db) {
            summary_md = pick(&res);
            model_id = res.get("model_id").cloned().unwrap_or(Value::Null);
        }
    }

    if let Some(s) = &summary_md {
        if !s.trim().is_empty() {
            crate::db_json::run_write(
                db,
                "INSERT INTO code_descriptions (origin, path, facts_json, summary_md, model_id, content_hash, gathered_at, generated_at) \
                 VALUES (?,?,?,?,?,?,datetime('now'),datetime('now')) \
                 ON CONFLICT(origin, path) DO UPDATE SET facts_json=excluded.facts_json, summary_md=excluded.summary_md, model_id=excluded.model_id, content_hash=excluded.content_hash, generated_at=datetime('now')",
                &[json!(origin), json!(path), json!(serde_json::to_string(&facts_json(&facts)).map_err(|e| e.to_string())?), json!(s), model_id, json!(hash)],
            )?;
        }
    }
    Ok(json!({ "contract_slug": slug, "summary_md": summary_md, "cached": false }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "ATTACH ':memory:' AS judgments; ATTACH ':memory:' AS brain;
             CREATE TABLE blueprints (id INTEGER PRIMARY KEY AUTOINCREMENT, contract_slug TEXT, version INTEGER, blueprint_json TEXT, lint_passed INTEGER, superseded_by INTEGER);
             CREATE TABLE blueprint_snippets (id INTEGER PRIMARY KEY AUTOINCREMENT, blueprint_id INTEGER, block_id TEXT, code TEXT, imports_json TEXT);
             CREATE TABLE contracts (slug TEXT, card_slug TEXT, acceptance_json TEXT);
             CREATE TABLE code_descriptions (origin TEXT, path TEXT, facts_json TEXT, summary_md TEXT, model_id TEXT, content_hash TEXT, gathered_at TEXT, generated_at TEXT, UNIQUE(origin, path));
             CREATE TABLE judgments.judgment_cases (judgment_id TEXT, verdict TEXT, goal_line TEXT, n_items INTEGER, defender_trues INTEGER, prosecutor_trues INTEGER, defender_rate REAL, prosecutor_rate REAL, defender_accuracy REAL, prosecutor_accuracy REAL);
             CREATE TABLE judgments.court_evidence (judgment_id TEXT, n_acceptance INTEGER, n_microtasks INTEGER, n_blocks INTEGER, n_nested_blocks INTEGER, has_card INTEGER, has_blueprint INTEGER, has_screening INTEGER, evidence_md TEXT, created_at TEXT);
             CREATE TABLE judgments.judgment_items (judgment_id TEXT, side TEXT, item_id TEXT, statement TEXT, grounded INTEGER, judge_value INTEGER, judge_accuracy REAL, judge_why TEXT);
             CREATE TABLE brain.prompt_templates ( id INTEGER PRIMARY KEY, name TEXT NOT NULL, version INTEGER NOT NULL, template_md TEXT NOT NULL, default_tier TEXT NOT NULL, default_temperature TEXT NOT NULL, max_output_tokens INTEGER NOT NULL DEFAULT 1024, superseded_by INTEGER, created_at TEXT NOT NULL DEFAULT (datetime('now')), cross_field_rules_json TEXT, judging_criteria_json TEXT, system_md TEXT, phase_flow TEXT NOT NULL DEFAULT 'single', UNIQUE (name, version) );
             CREATE TABLE brain.prompt_output_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, grounded_refs_json TEXT, UNIQUE (prompt_template_id, field_name) );
             CREATE TABLE brain.prompt_input_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, UNIQUE (prompt_template_id, field_name) );",
        ).unwrap();
        c
    }

    #[test]
    fn missing_contract_slug_errors() {
        let c = db();
        assert!(blueprintDescribe(&json!({}), &c).unwrap_err().contains("contract_slug is required"));
    }

    #[test]
    fn no_blueprint_short_circuits() {
        let c = db();
        c.execute("INSERT INTO contracts (slug, card_slug) VALUES ('CT-N','CARD-N')", []).unwrap();
        let r = blueprintDescribe(&json!({ "contract_slug": "CT-N" }), &c).unwrap();
        assert_eq!(r, json!({ "contract_slug": "CT-N", "summary_md": null, "cached": false, "reason": "no_blueprint" }));
    }

    #[test]
    fn cache_hit_returns_stored_summary_without_calling_ai() {
        let c = db();
        c.execute("INSERT INTO contracts (slug, card_slug) VALUES ('CT-M','CARD-M')", []).unwrap();
        c.execute("INSERT INTO blueprints (contract_slug, version, blueprint_json, lint_passed) VALUES ('CT-M', 1, '{}', 1)", []).unwrap();
        let bp = crate::blueprint_blueprint_get::blueprintGet(&json!({ "contract_slug": "CT-M" }), &c).unwrap();
        let facts = change_facts(&bp);
        let hash = crate::codebase_atlas_ingest::hashSource(&serde_json::to_string(&facts_json(&facts)).unwrap());
        c.execute(
            "INSERT INTO code_descriptions (origin, path, summary_md, content_hash) VALUES ('blueprint:CT-M', '(change)', 'cached narrative', ?1)",
            [hash],
        ).unwrap();
        let r = blueprintDescribe(&json!({ "contract_slug": "CT-M" }), &c).unwrap();
        assert_eq!(r, json!({ "contract_slug": "CT-M", "summary_md": "cached narrative", "cached": true }));
    }

    #[test]
    fn stale_hash_falls_through_to_the_parity_deferred_aicall_boundary() {
        let c = db();
        c.execute("INSERT INTO contracts (slug, card_slug) VALUES ('CT-S','CARD-S')", []).unwrap();
        c.execute("INSERT INTO blueprints (contract_slug, version, blueprint_json, lint_passed) VALUES ('CT-S', 1, '{}', 1)", []).unwrap();
        c.execute("INSERT INTO code_descriptions (origin, path, summary_md, content_hash) VALUES ('blueprint:CT-S', '(change)', 'stale', 'deadbeef')", []).unwrap();
        // aiCall's Err is swallowed (mirrors JS's catch), so this returns Ok with summary_md null,
        // not an Err — proving every deterministic gate above (guard/blueprint-get/cache-miss/
        // prompt-ensure) ran cleanly.
        let r = blueprintDescribe(&json!({ "contract_slug": "CT-S" }), &c).unwrap();
        assert_eq!(r["summary_md"], Value::Null);
        assert_eq!(r["cached"], json!(false));
        let n: i64 = c.query_row("SELECT COUNT(*) FROM brain.prompt_templates WHERE name = 'blueprint.describe.v1'", [], |row| row.get(0)).unwrap();
        assert_eq!(n, 1);
        // stale cache row is left untouched — the upsert only fires on a real (non-empty) summary_md.
        let stale: String = c.query_row("SELECT summary_md FROM code_descriptions WHERE origin='blueprint:CT-S'", [], |row| row.get(0)).unwrap();
        assert_eq!(stale, "stale");
    }

    #[test]
    fn force_bypasses_a_matching_cache() {
        let c = db();
        c.execute("INSERT INTO contracts (slug, card_slug) VALUES ('CT-F','CARD-F')", []).unwrap();
        c.execute("INSERT INTO blueprints (contract_slug, version, blueprint_json, lint_passed) VALUES ('CT-F', 1, '{}', 1)", []).unwrap();
        let bp = crate::blueprint_blueprint_get::blueprintGet(&json!({ "contract_slug": "CT-F" }), &c).unwrap();
        let facts = change_facts(&bp);
        let hash = crate::codebase_atlas_ingest::hashSource(&serde_json::to_string(&facts_json(&facts)).unwrap());
        c.execute("INSERT INTO code_descriptions (origin, path, summary_md, content_hash) VALUES ('blueprint:CT-F', '(change)', 'cached', ?1)", [hash]).unwrap();
        let r = blueprintDescribe(&json!({ "contract_slug": "CT-F", "force": true }), &c).unwrap();
        assert_eq!(r["cached"], json!(false));
    }

    // ── change_facts (deterministic gather) ─────────────────────────────────────────────────
    #[test]
    fn change_facts_groups_by_path_sorted_with_intent_and_symbols() {
        let bp = json!({
            "version": 3,
            "design_docs": [
                { "path": "b.ts", "name": "fn2", "signature": "()", "status": "added" },
                { "path": "a.ts", "name": "fn1", "signature": "(x)", "status": "changed" },
                { "path": "a.ts", "name": "fn0", "status": "changed" },
            ],
            "blocks": [
                { "path": "a.ts", "intent": "the a file" },
                { "path": "b.ts", "intent": "the b file" },
            ],
            "delta": { "counts": { "added": 1, "changed": 2 } },
        });
        let f = change_facts(&bp);
        assert_eq!(f.version, json!(3));
        assert_eq!(f.n_files, 2);
        assert_eq!(f.added, 1);
        assert_eq!(f.changed, 2);
        assert_eq!(f.files[0], json!({ "path": "a.ts", "intent": "the a file", "symbols": ["fn1(x)", "fn0"], "status": "changed" }));
        assert_eq!(f.files[1], json!({ "path": "b.ts", "intent": "the b file", "symbols": ["fn2()"], "status": "added" }));
    }

    #[test]
    fn change_facts_defaults_missing_path_and_delta_counts() {
        let bp = json!({ "version": 1, "design_docs": [{ "name": "fn" }], "blocks": [] });
        let f = change_facts(&bp);
        assert_eq!(f.added, 0);
        assert_eq!(f.changed, 0);
        assert_eq!(f.files[0]["path"], json!("(unknown)"));
        assert_eq!(f.files[0]["intent"], Value::Null);
        assert_eq!(f.files[0]["symbols"], json!(["fn"]));
    }

    #[test]
    fn change_facts_empty_design_docs() {
        let bp = json!({ "version": 1 });
        let f = change_facts(&bp);
        assert_eq!(f.n_files, 0);
        assert_eq!(f.files, Vec::<Value>::new());
    }

    // ── pick (response-parsing helper, fed a canned ai_calls-shaped response) ─────────────────
    #[test]
    fn pick_finds_top_level_summary_md() {
        let res = json!({ "output_kv": [
            { "field_name": "other", "value_text": "x", "parent_output_id": null },
            { "field_name": "summary_md", "value_text": "the change adds X", "parent_output_id": null },
        ] });
        assert_eq!(pick(&res), Some("the change adds X".to_string()));
    }

    #[test]
    fn pick_ignores_nested_output_and_missing_field() {
        let nested = json!({ "output_kv": [
            { "field_name": "summary_md", "value_text": "nested, skip", "parent_output_id": 7 },
        ] });
        assert_eq!(pick(&nested), None);
        assert_eq!(pick(&json!({})), None);
        assert_eq!(pick(&json!({ "output_kv": [] })), None);
    }

    // ── ensure_prompt bookkeeping ───────────────────────────────────────────────────────────
    #[test]
    fn ensure_prompt_is_idempotent_when_template_matches() {
        let c = db();
        ensure_prompt(&c).unwrap();
        ensure_prompt(&c).unwrap();
        let n: i64 = c.query_row("SELECT COUNT(*) FROM brain.prompt_templates WHERE name = 'blueprint.describe.v1'", [], |row| row.get(0)).unwrap();
        assert_eq!(n, 1);
    }
}
