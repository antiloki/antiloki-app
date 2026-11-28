//! manifest-design.rs — twin of skills/swarm/manifest-design.mjs (db WRITE lane, `swarm_manifests`
//! + `contract_qa_reviews` + `brain.prompt_templates`). NAMES ARE CONTRACT: default `manifestDesign`;
//! named `ensureManifestPrompts`. Produces the blueprint NUCLEUS (the manifest) for a contract and
//! persists it (versioned, superseding the prior active row).
//!
//! PARITY-DEFERRED: the MID-tier "architect" design leg and the LOW-tier "reformat" leg both end in
//! `crate::ai_ai_call::aiCall` (the deferred network-AI stub — see ai-call.rs's module doc). The JS
//! wraps BOTH calls in one try/catch that CATCHES the aiCall error (not propagate) and falls through
//! to a deterministic SCAFFOLD manifest (one block per file, no bindings) — this is the "GRACEFUL
//! FALLBACK" the module doc describes: a contract is never hard-blocked here. That catch-and-fallback
//! is ported faithfully, which means — unlike most LLM-partial twins — this file's `manifestDesign`
//! ALWAYS returns `Ok` in this deferred build (aiCall's Err is caught, source becomes "scaffold") and
//! is fully exercised end-to-end by the tests below: guards, contract lookup, prompt-ensure
//! (`ensureManifestPrompts`, reusing the already-converted `ai_prompt_find`/`ai_prompt_add` twins),
//! redesign-feedback lookup, the scaffold builder, goal/bindings normalization, per-block subagent_id
//! stamping (reusing the already-converted `swarm_subagent_route::routeFile` twin), and the
//! versioned INSERT + supersede. The only code never exercised (since aiCall never succeeds in this
//! build) is the AI-response parse/reformat path inside `try_ai_design` — its helpers (`parse_loose`,
//! `looks_like_manifest`, `pick_manifest`) are pure and unit-tested directly.
use regex::Regex;
use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::collections::HashMap;

const DESIGN_PROMPT: &str = "swarm.manifest.design.v1";
const REFORMAT_PROMPT: &str = "swarm.manifest.reformat.v1";

const SHAPE: &str = r#"{
  "version": 1,
  "goal": "<one measurable line>",
  "blocks": [{"id":"<short unique>","file":"<exact path>","role":"<one line>","produces":[{"name":"<exported symbol>","type":"<type>"}],"consumes":[{"name":"<input>","type":"<type>","external":false}]}],
  "bindings": [{"id":"b1","from":{"block":"<producer id>","port":"<produce name>"},"to":{"block":"<consumer id>","port":"<consume name>"},"type":"<type>"}]
}"#;

const DESIGN_SYSTEM: &str = "You are the ARCHITECT of an autonomous software substrate. You design the typed information-flow MANIFEST for a feature — the blueprint. You think in dataflow: not imports, but the JOURNEY of information through the files. You output only the manifest, as strict JSON.";

fn design_template() -> String {
    format!(
        "Design the MANIFEST (the typed dataflow graph — the blueprint) for this contract.\n\n\
═══ THE CONTRACT ═══\n\
title: {{{{contract_title}}}}\n\
goal: {{{{goal_md}}}}\n\
files to author (one block each): {{{{files_touched}}}}\n\
acceptance: {{{{acceptance}}}}\n\
must_obey: {{{{must_obey}}}}\n\
out_of_scope: {{{{out_of_scope}}}}\n\n\
{{{{redesign_feedback}}}}\n\n\
DESIGN RULES:\n\
- One BLOCK per file in files_touched. id = a short unique name; file = the exact path; role = one line.\n\
- produces = the named, typed OUTPUTS the file exposes (its exported symbols): [{{name, type}}].\n\
- consumes = the named, typed INPUTS the file needs: [{{name, type, external}}]. Set external:true for inputs from OUTSIDE the blueprint (a package or global) — those need no binding.\n\
- BINDINGS are the typed edges = the story. For every NON-external consume (one block needs another block's output), add a binding from the producing block's port to the consuming block's port. from.port must be a produce name on from.block; to.port a consume name on to.block; the binding type must match BOTH ends.\n\
- Every non-external consume MUST be fed by exactly one binding (no dangling inputs). The graph must read as a DAG (no cycles). Do not connect a block to itself.\n\n\
Put the manifest JSON (and nothing else — no prose, no code fences) into the manifest_json field, EXACTLY this shape:\n{SHAPE}"
    )
}
fn reformat_template() -> String {
    format!(
        "REFORMAT TASK. Below is a DRAFT manifest that may be malformed JSON or wrapped in prose/fences. Re-emit it as STRICT, valid JSON of exactly the target shape — do not change the design, only fix the format. Output ONLY the JSON into manifest_json, no prose, no fences.\n\n\
TARGET SHAPE:\n{SHAPE}\n\n\
DRAFT:\n{{{{draft}}}}"
    )
}
fn output_fields() -> Value {
    json!([{ "field_name": "manifest_json", "field_type": "string" }])
}

// ── JS-parity helpers ────────────────────────────────────────────────────────
fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
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

// `function ensureOne(...)` — promptFind failures are swallowed (JS try/catch); promptAdd errors
// propagate (unwrapped in JS), so this returns Result and the caller `?`s it.
fn ensure_one(db: &Connection, name: &str, template_md: &str, default_tier: &str, system_md: Option<&str>) -> Result<(), String> {
    let existing = crate::ai_prompt_find::promptFind(&json!({ "name": name }), db).ok();
    if let Some(e) = &existing {
        if e.get("template_md").and_then(|v| v.as_str()) == Some(template_md) {
            return Ok(());
        }
    }
    crate::ai_prompt_add::promptAdd(
        &json!({
            "name": name,
            "template_md": template_md,
            "system_md": system_md,
            "default_tier": default_tier,
            "default_temperature": "cold",
            "phase_flow": "single_only",
            "max_output_tokens": 4000,
            "output_fields": output_fields(),
        }),
        db,
    )?;
    Ok(())
}
pub fn ensureManifestPrompts(db: &Connection) -> Result<(), String> {
    ensure_one(db, DESIGN_PROMPT, &design_template(), "mid", Some(DESIGN_SYSTEM))?;
    ensure_one(db, REFORMAT_PROMPT, &reformat_template(), "low", None)?;
    Ok(())
}

// `function parseLoose(s)` — tolerant JSON extract: strip fences, else take the outermost {...}.
fn parse_loose(s: &Value) -> Value {
    if s.is_null() {
        return Value::Null;
    }
    let str_ = js_string(s);
    let mut candidates: Vec<String> = Vec::new();
    if let Ok(re) = Regex::new(r"(?is)```(?:json)?\s*(.*?)```") {
        if let Some(cap) = re.captures(&str_) {
            candidates.push(cap[1].to_string());
        }
    }
    candidates.push(str_.clone());
    if let Ok(re2) = Regex::new(r"(?s)\{.*\}") {
        if let Some(m) = re2.find(&str_) {
            candidates.push(m.as_str().to_string());
        }
    }
    for c in &candidates {
        if let Ok(v) = serde_json::from_str::<Value>(c.trim()) {
            return v;
        }
    }
    Value::Null
}
// `const looksLikeManifest = (o) => !!(o && typeof o === 'object' && Array.isArray(o.blocks));`
fn looks_like_manifest(o: &Value) -> bool {
    (o.is_object() || o.is_array()) && o.get("blocks").map_or(false, |v| v.is_array())
}

// `function blockIdsFor(files)` — dedupes basename-derived ids, appending `_N` on collision.
fn block_ids_for(files: &[String]) -> Vec<String> {
    let re_ext = Regex::new(r"\.[^.]+$").unwrap();
    let re_nonword = Regex::new(r"[^A-Za-z0-9_]+").unwrap();
    let re_trim = Regex::new(r"^_+|_+$").unwrap();
    let mut used: HashMap<String, i64> = HashMap::new();
    files
        .iter()
        .map(|f| {
            let normalized = f.replace('\\', "/");
            let base_name = match normalized.rsplit('/').next() {
                Some(s) if !s.is_empty() => s.to_string(),
                _ => f.clone(), // JS `split('/').pop() || String(f)`
            };
            let mut id = re_ext.replace(&base_name, "").to_string();
            id = re_nonword.replace_all(&id, "_").to_string();
            id = re_trim.replace_all(&id, "").to_string();
            if id.is_empty() {
                id = "block".to_string();
            }
            let n = *used.get(&id).unwrap_or(&0);
            used.insert(id.clone(), n + 1);
            if n == 0 { id } else { format!("{id}_{n}") }
        })
        .collect()
}
fn build_scaffold_manifest(files: &[String], goal: &str) -> Value {
    let ids = block_ids_for(files);
    let blocks: Vec<Value> = files
        .iter()
        .enumerate()
        .map(|(i, file)| json!({ "id": ids[i], "file": file, "role": format!("authored file {file}"), "produces": [], "consumes": [] }))
        .collect();
    json!({ "version": 1, "goal": goal, "blocks": blocks, "bindings": [] })
}

// `function safeArr(s)` — JSON.parse(s ?? '[]'), Array.isArray guard, [] on any failure.
fn safe_arr(v: Option<&Value>) -> Vec<Value> {
    let s = match v {
        None | Some(Value::Null) => "[]".to_string(),
        Some(Value::String(s)) => s.clone(),
        Some(other) => js_string(other),
    };
    match serde_json::from_str::<Value>(&s) {
        Ok(Value::Array(a)) => a,
        _ => vec![],
    }
}

// `const pickManifest = (aiResult) => aiResult?.output_kv?.find(...)?.value_text ?? null;`
fn pick_manifest(ai_result: &Value) -> Value {
    if let Some(arr) = ai_result.get("output_kv").and_then(|v| v.as_array()) {
        for o in arr {
            let is_field = o.get("field_name").and_then(|v| v.as_str()) == Some("manifest_json");
            let parent_null = o.get("parent_output_id").map_or(true, |v| v.is_null());
            if is_field && parent_null {
                return o.get("value_text").cloned().unwrap_or(Value::Null);
            }
        }
    }
    Value::Null
}

// ── the AI design leg (PARITY-DEFERRED past the aiCall dispatch) ─────────────
// Mirrors the JS try-block body up to (and including) the reformat leg. The caller catches this
// Err exactly like JS's surrounding `catch { manifest = null; }`.
fn try_ai_design(db: &Connection, provider_fn: &Value, contract: &Value, goal: &str, files: &[String], redesign_feedback: &str) -> Result<(Value, String), String> {
    let title = contract.get("title").and_then(|v| v.as_str()).filter(|s| !s.is_empty()).unwrap_or("(untitled)");
    let design_kv = json!([
        { "key": "contract_title", "value": title },
        { "key": "goal_md", "value": goal },
        { "key": "files_touched", "value": json!(files).to_string() },
        { "key": "acceptance", "value": serde_json::to_string_pretty(&safe_arr(contract.get("acceptance_json"))).unwrap_or_default() },
        { "key": "must_obey", "value": json!(safe_arr(contract.get("must_obey_json"))).to_string() },
        { "key": "out_of_scope", "value": json!(safe_arr(contract.get("out_of_scope_json"))).to_string() },
        { "key": "redesign_feedback", "value": redesign_feedback },
    ]);
    let design_input = json!({
        "prompt_name": DESIGN_PROMPT, "input_kv": design_kv,
        "tier": "mid", "temperature": "cold", "phase_flow": "single_only", "provider_fn": provider_fn,
    });
    let design_res = crate::ai_ai_call::aiCall(&design_input, db)?;
    let raw = pick_manifest(&design_res);
    let mut manifest = parse_loose(&raw);
    let mut source = "ai".to_string();
    if !looks_like_manifest(&manifest) {
        // double-call: a low leg reformats the draft to strict JSON.
        let reformat_input = json!({
            "prompt_name": REFORMAT_PROMPT,
            "input_kv": [{ "key": "draft", "value": js_string(&raw) }],
            "tier": "low", "temperature": "freezed", "phase_flow": "single_only", "provider_fn": provider_fn,
        });
        let reformat_res = crate::ai_ai_call::aiCall(&reformat_input, db)?;
        manifest = parse_loose(&pick_manifest(&reformat_res));
        if looks_like_manifest(&manifest) {
            source = "ai-reformat".to_string();
        }
    }
    Ok((manifest, source))
}

pub fn manifestDesign(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS: `const { db, provider_fn = null } = context ?? {}; if (!db) throw err.contextMissing(...)`
    // — unreachable here (Rust's `db: &Connection` is never absent). provider_fn is a JS function
    // reference threaded opaquely into aiCall's input; there is no Rust equivalent to carry, so it
    // is passed as `null` (mirrors book-run.rs's identical convention).
    let provider_fn = Value::Null;
    let slug_v = input.get("contract_slug");
    if !truthy(slug_v) {
        return Err("manifest-design: contract_slug is required".into());
    }
    let slug = js_string(slug_v.unwrap());

    let rows = crate::db_json::query_rows(
        db,
        "SELECT slug, title, files_touched_json, briefing_md, acceptance_json, must_obey_json, out_of_scope_json FROM contracts WHERE slug = ?",
        &[json!(slug)],
    )?;
    let contract = match rows.as_array().and_then(|a| a.first()) {
        Some(c) => c.clone(),
        None => return Err(format!("manifest-design: contract '{slug}' not found")),
    };

    let files: Vec<String> = safe_arr(contract.get("files_touched_json"))
        .into_iter()
        .map(|v| js_string(&v))
        .filter(|s| !s.is_empty())
        .collect();

    // `(contract.briefing_md ? String(contract.briefing_md).split('\n')[0] : null) || contract.title || 'deliver ${slug}'`
    let goal = {
        let from_briefing = match contract.get("briefing_md") {
            Some(Value::String(s)) if !s.is_empty() => Some(s.split('\n').next().unwrap_or("").to_string()),
            _ => None,
        };
        from_briefing
            .filter(|s| !s.is_empty())
            .or_else(|| contract.get("title").and_then(|v| v.as_str()).filter(|s| !s.is_empty()).map(|s| s.to_string()))
            .unwrap_or_else(|| format!("deliver {slug}"))
    };

    ensureManifestPrompts(db)?;

    // Re-design feedback: the latest RED manifest-lint findings, so the architect fixes exactly
    // the structural errors instead of re-emitting the same graph. JS wraps this in a bare
    // try/catch (table/column absent → first attempt) — mirrored via `if let Ok(...)`.
    let mut redesign_feedback = "(first design attempt)".to_string();
    if let Ok(red_rows) = crate::db_json::query_rows(
        db,
        "SELECT findings_md FROM contract_qa_reviews WHERE contract_slug = ? AND review_mode = 'manifest-lint' AND passed = 0 ORDER BY id DESC LIMIT 1",
        &[json!(slug)],
    ) {
        if let Some(red) = red_rows.as_array().and_then(|a| a.first()) {
            if let Some(findings) = red.get("findings_md").and_then(|v| v.as_str()).filter(|s| !s.is_empty()) {
                redesign_feedback = format!("═══ YOUR PREVIOUS MANIFEST FAILED LINT — FIX EXACTLY THESE ═══\n{findings}");
            }
        }
    }

    let (mut manifest, mut source) = match try_ai_design(db, &provider_fn, &contract, &goal, &files, &redesign_feedback) {
        Ok((m, s)) => (m, s),
        Err(_) => (Value::Null, "ai".to_string()),
    };

    if !looks_like_manifest(&manifest) {
        manifest = build_scaffold_manifest(&files, &goal);
        source = "scaffold".to_string();
    }

    // Normalize the load-bearing fields; everything else is the architect's.
    if let Value::Object(ref mut m) = manifest {
        let has_goal = m.get("goal").and_then(|v| v.as_str()).map_or(false, |s| !s.is_empty());
        if !has_goal {
            m.insert("goal".into(), json!(goal));
        }
        if !m.get("bindings").map_or(false, |v| v.is_array()) {
            m.insert("bindings".into(), json!([]));
        }

        // EPIC-SUBAGENT: stamp each block's authoring LEAF — the contract-as-router's deterministic
        // half (file-type → most-specific tl.dev leaf, fallback to the branch root). We don't clobber
        // a subagent_id the architect may have set (the future high-tier pick).
        if let Some(Value::Array(blocks)) = m.get_mut("blocks") {
            for block in blocks.iter_mut() {
                if let Value::Object(ref mut b) = block {
                    let file_v = b.get("file").cloned();
                    let has_file = file_v.as_ref().map_or(false, |v| truthy(Some(v)));
                    let has_sub = truthy(b.get("subagent_id"));
                    if has_file && !has_sub {
                        let route = crate::swarm_subagent_route::routeFile(db, file_v.as_ref().unwrap(), &json!("tl.dev"), &Value::Null);
                        b.insert("subagent_id".into(), route.get("subagent_id").cloned().unwrap_or(Value::Null));
                    }
                }
            }
        }
    }

    let prior_rows = crate::db_json::query_rows(
        db,
        "SELECT id, version FROM swarm_manifests WHERE contract_slug = ? AND superseded_by IS NULL ORDER BY version DESC, id DESC LIMIT 1",
        &[json!(slug)],
    )?;
    let prior = prior_rows.as_array().and_then(|a| a.first()).cloned();
    let version = prior.as_ref().and_then(|p| p.get("version").and_then(|v| v.as_i64())).unwrap_or(0) + 1;
    if let Value::Object(ref mut m) = manifest {
        m.insert("version".into(), json!(version));
    }

    let manifest_json = serde_json::to_string(&manifest).map_err(|e| e.to_string())?;
    let goal_final = manifest.get("goal").cloned().unwrap_or_else(|| json!(goal));
    let created_by = input
        .get("created_by")
        .filter(|v| !v.is_null())
        .cloned()
        .unwrap_or_else(|| json!(format!("driver:auto:{source}")));

    let (_changes, manifest_id) = crate::db_json::run_write(
        db,
        "INSERT INTO swarm_manifests (contract_slug, version, goal, manifest_json, created_by) VALUES (?, ?, ?, ?, ?)",
        &[json!(slug), json!(version), goal_final, json!(manifest_json), created_by],
    )?;
    if let Some(p) = &prior {
        crate::db_json::run_write(
            db,
            "UPDATE swarm_manifests SET superseded_by = ? WHERE id = ?",
            &[json!(manifest_id), p.get("id").cloned().unwrap_or(Value::Null)],
        )?;
    }

    let block_count = manifest.get("blocks").and_then(|v| v.as_array()).map(|a| a.len()).unwrap_or(0);
    let binding_count = manifest.get("bindings").and_then(|v| v.as_array()).map(|a| a.len()).unwrap_or(0);

    Ok(json!({
        "contract_slug": slug,
        "version": version,
        "block_count": block_count,
        "binding_count": binding_count,
        "manifest_id": manifest_id,
        "persisted": true,
        "source": source,
        "manifest": manifest,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS brain;").unwrap();
        c.execute_batch(
            "CREATE TABLE contracts (id INTEGER PRIMARY KEY AUTOINCREMENT, slug TEXT NOT NULL UNIQUE, title TEXT, files_touched_json TEXT NOT NULL DEFAULT '[]', briefing_md TEXT, acceptance_json TEXT NOT NULL DEFAULT '[]', must_obey_json TEXT NOT NULL DEFAULT '[]', out_of_scope_json TEXT NOT NULL DEFAULT '[]');
             CREATE TABLE swarm_manifests (id INTEGER PRIMARY KEY AUTOINCREMENT, contract_slug TEXT, version INTEGER, goal TEXT, manifest_json TEXT, superseded_by INTEGER, created_by TEXT, lint_passed INTEGER, lint_json TEXT);
             CREATE TABLE contract_qa_reviews (id INTEGER PRIMARY KEY AUTOINCREMENT, contract_slug TEXT, passed INTEGER, verdict TEXT, findings_md TEXT, issues_json TEXT, reviewer TEXT, failure_target TEXT, review_mode TEXT);
             CREATE TABLE agents (name TEXT, parent_agent TEXT, is_abstract INTEGER, applies_to_json TEXT);
             CREATE TABLE brain.prompt_templates ( id INTEGER PRIMARY KEY, name TEXT NOT NULL, version INTEGER NOT NULL, template_md TEXT NOT NULL, default_tier TEXT NOT NULL, default_temperature TEXT NOT NULL, max_output_tokens INTEGER NOT NULL DEFAULT 1024, superseded_by INTEGER, created_at TEXT NOT NULL DEFAULT (datetime('now')), cross_field_rules_json TEXT, judging_criteria_json TEXT, system_md TEXT, phase_flow TEXT NOT NULL DEFAULT 'single', UNIQUE (name, version) );
             CREATE TABLE brain.prompt_output_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, grounded_refs_json TEXT, UNIQUE (prompt_template_id, field_name) );
             CREATE TABLE brain.prompt_input_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, UNIQUE (prompt_template_id, field_name) );",
        )
        .unwrap();
        c
    }

    fn a_contract(c: &Connection, slug: &str, files: &[&str]) -> i64 {
        c.execute(
            "INSERT INTO contracts (slug, title, files_touched_json) VALUES (?, 'A Contract', ?)",
            rusqlite::params![slug, serde_json::to_string(files).unwrap()],
        )
        .unwrap();
        c.last_insert_rowid()
    }

    #[test]
    fn requires_contract_slug() {
        let c = setup();
        let err = manifestDesign(&json!({}), &c).unwrap_err();
        assert!(err.contains("contract_slug is required"), "unexpected: {err}");
    }

    #[test]
    fn contract_not_found() {
        let c = setup();
        let err = manifestDesign(&json!({ "contract_slug": "NOPE" }), &c).unwrap_err();
        assert!(err.contains("contract 'NOPE' not found"), "unexpected: {err}");
    }

    #[test]
    fn scaffold_fallback_persists_a_manifest_since_aicall_is_deferred() {
        let c = setup();
        a_contract(&c, "CT-1", &["src/config.js", "src/server.js"]);
        let r = manifestDesign(&json!({ "contract_slug": "CT-1" }), &c).unwrap();
        assert_eq!(r["source"], json!("scaffold"));
        assert_eq!(r["version"], json!(1));
        assert_eq!(r["persisted"], json!(true));
        assert_eq!(r["block_count"], json!(2));
        assert_eq!(r["binding_count"], json!(0));
        assert_eq!(r["manifest"]["blocks"][0]["id"], json!("config"));
        assert_eq!(r["manifest"]["blocks"][0]["role"], json!("authored file src/config.js"));
        // goal falls back to contract.title (not briefing_md) before `deliver ${slug}`; contract has no briefing_md but does have a title
        assert_eq!(r["manifest"]["goal"], json!("A Contract"));

        // the design + reformat prompt templates WERE ensured before the deferred aiCall
        let n: i64 = c.query_row("SELECT COUNT(*) FROM brain.prompt_templates WHERE name IN ('swarm.manifest.design.v1','swarm.manifest.reformat.v1')", [], |row| row.get(0)).unwrap();
        assert_eq!(n, 2);

        // persisted row matches the returned manifest
        let stored: String = c.query_row("SELECT manifest_json FROM swarm_manifests WHERE contract_slug = 'CT-1'", [], |row| row.get(0)).unwrap();
        let stored_v: Value = serde_json::from_str(&stored).unwrap();
        assert_eq!(stored_v["blocks"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn uses_briefing_md_first_line_as_goal_when_present() {
        let c = setup();
        c.execute("INSERT INTO contracts (slug, title, files_touched_json, briefing_md) VALUES ('CT-2', 'T', '[]', 'ship the thing\nmore detail')", []).unwrap();
        let r = manifestDesign(&json!({ "contract_slug": "CT-2" }), &c).unwrap();
        assert_eq!(r["manifest"]["goal"], json!("ship the thing"));
    }

    #[test]
    fn redesign_bumps_version_and_supersedes_prior() {
        let c = setup();
        a_contract(&c, "CT-3", &["a.js"]);
        let first = manifestDesign(&json!({ "contract_slug": "CT-3" }), &c).unwrap();
        assert_eq!(first["version"], json!(1));
        let first_id = first["manifest_id"].as_i64().unwrap();

        // seed a RED manifest-lint review, exercising the redesign-feedback lookup path
        c.execute("INSERT INTO contract_qa_reviews (contract_slug, passed, review_mode, findings_md) VALUES ('CT-3', 0, 'manifest-lint', 'binding-malformed: fix it')", []).unwrap();

        let second = manifestDesign(&json!({ "contract_slug": "CT-3" }), &c).unwrap();
        assert_eq!(second["version"], json!(2));
        let superseded_by: i64 = c.query_row("SELECT superseded_by FROM swarm_manifests WHERE id = ?", [first_id], |row| row.get(0)).unwrap();
        assert_eq!(superseded_by, second["manifest_id"].as_i64().unwrap());
    }

    #[test]
    fn stamps_subagent_id_from_route_file_without_clobbering_existing() {
        let c = setup();
        c.execute_batch(
            "INSERT INTO agents VALUES ('tl.dev', NULL, 1, NULL), ('tl.dev.style', 'tl.dev', 0, '{\"globs\":[\"**/*.css\"]}');",
        )
        .unwrap();
        a_contract(&c, "CT-4", &["src/Button.css", "README.md"]);
        let r = manifestDesign(&json!({ "contract_slug": "CT-4" }), &c).unwrap();
        let blocks = r["manifest"]["blocks"].as_array().unwrap();
        assert_eq!(blocks[0]["file"], json!("src/Button.css"));
        assert_eq!(blocks[0]["subagent_id"], json!("tl.dev.style"));
        // no matching leaf → falls back to the branch root 'tl.dev'
        assert_eq!(blocks[1]["subagent_id"], json!("tl.dev"));
    }

    #[test]
    fn created_by_defaults_to_driver_auto_source_but_respects_explicit_value() {
        let c = setup();
        a_contract(&c, "CT-5", &["a.js"]);
        manifestDesign(&json!({ "contract_slug": "CT-5" }), &c).unwrap();
        let cb: String = c.query_row("SELECT created_by FROM swarm_manifests WHERE contract_slug = 'CT-5'", [], |row| row.get(0)).unwrap();
        assert_eq!(cb, "driver:auto:scaffold");

        a_contract(&c, "CT-6", &["a.js"]);
        manifestDesign(&json!({ "contract_slug": "CT-6", "created_by": "human:dani" }), &c).unwrap();
        let cb2: String = c.query_row("SELECT created_by FROM swarm_manifests WHERE contract_slug = 'CT-6'", [], |row| row.get(0)).unwrap();
        assert_eq!(cb2, "human:dani");
    }

    // ── pure helpers ───────────────────────────────────────────────────────
    #[test]
    fn block_ids_for_dedupes_basenames() {
        let ids = block_ids_for(&["src/a/config.js".to_string(), "src/b/config.js".to_string(), "weird/../.hidden".to_string()]);
        assert_eq!(ids[0], "config");
        assert_eq!(ids[1], "config_1");
    }

    #[test]
    fn block_ids_for_falls_back_to_block_when_no_letters() {
        let ids = block_ids_for(&["***".to_string()]);
        assert_eq!(ids[0], "block");
    }

    #[test]
    fn looks_like_manifest_requires_blocks_array() {
        assert!(looks_like_manifest(&json!({ "blocks": [] })));
        assert!(!looks_like_manifest(&json!({ "blocks": "nope" })));
        assert!(!looks_like_manifest(&json!("just a string")));
        assert!(!looks_like_manifest(&Value::Null));
    }

    #[test]
    fn parse_loose_strips_fences_and_extracts_outer_object() {
        assert_eq!(parse_loose(&json!("```json\n{\"a\":1}\n```")), json!({"a":1}));
        assert_eq!(parse_loose(&json!("prose before {\"a\":1} prose after")), json!({"a":1}));
        assert_eq!(parse_loose(&json!("garbage, not json at all")), Value::Null);
        assert_eq!(parse_loose(&Value::Null), Value::Null);
    }

    #[test]
    fn pick_manifest_finds_top_level_field_only() {
        let res = json!({ "output_kv": [
            { "field_name": "manifest_json", "value_text": "nested", "parent_output_id": 5 },
            { "field_name": "manifest_json", "value_text": "{\"blocks\":[]}", "parent_output_id": null },
        ]});
        assert_eq!(pick_manifest(&res), json!("{\"blocks\":[]}"));
        assert_eq!(pick_manifest(&json!({})), Value::Null);
    }

    #[test]
    fn ensure_manifest_prompts_is_idempotent() {
        let c = setup();
        ensureManifestPrompts(&c).unwrap();
        ensureManifestPrompts(&c).unwrap();
        let n: i64 = c.query_row("SELECT COUNT(*) FROM brain.prompt_templates WHERE name = 'swarm.manifest.design.v1'", [], |row| row.get(0)).unwrap();
        assert_eq!(n, 1); // same template_md both times → no version bump
    }
}
