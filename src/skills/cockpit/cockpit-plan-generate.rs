//! cockpit-plan-generate.rs — twin of skills/cockpit/cockpit-plan-generate.mjs (db lane,
//! cross-db code.* reads). NAMES ARE CONTRACT: default `cockpitPlanGenerate`. The ULTRA PLAN +
//! BREAKDOWN + wizard/baseplan/shapetree/filemap/ideate/prepare/blueprint/brainstorm/decisions
//! panes — every one of these ten `input.mode`s (plus the pure `plan-save`/`plan-get` persistence
//! ops) funnels into exactly ONE `aiCall` per invocation.
//!
//! PARITY-DEFERRED: every mode except `plan-save`/`plan-get` ends in `crate::ai_ai_call::aiCall`
//! (the deferred network-AI stub — see ai-call.rs's module doc), which always returns `Err`. Ported
//! faithfully and IS the deterministic, fixture-reachable surface: op/guard validation, the
//! session_id/pane_id/ambition gates, EVERY early `ok:false` short-circuit that fires BEFORE the
//! call (empty shape tree, missing atlas onboarding, empty catalog, unknown ideate op, missing
//! item), prompt-template ensure/bootstrap (`ensure_prompt` reusing the already-converted
//! `ai_prompt_find`/`ai_prompt_add` twins, mirroring book-run.rs's `ensure_faculty`), grounding/
//! context assembly (`shared_context_text`, `resolve_selected_context`, both reusing the
//! already-converted `cockpit__helpers_context_brief::resolveSourceText` twin), and the two pure
//! `ultra_plans` persistence ops (`plan-save`/`plan-get`, no AI, fully reachable + fixture-tested).
//! The response-parsing helpers for every mode (`parse_plan`, `clean_branch` + shapetree salvage,
//! `parse_ideas_salvage`, wizard's `clean_dimensions`/`salvage_dimensions`, `parse_decision_list`,
//! `parse_prepare_response`, `parse_blueprint_response`, `parse_filemap_assignments` +
//! `apply_filemap_assignments`) are ported byte-for-byte from the JS as their own PURE functions,
//! unit-tested directly with canned response text — unreachable from the default export itself
//! (the `?` on `aiCall` short-circuits every mode before its own parser runs) but correct by
//! construction, same pattern as book-run.rs's `parse_arr`/`parse_obj` and pl-research.rs's
//! `build_research_report`. `context.provider_fn` (a JS function reference for provider selection/
//! DI) has no Rust equivalent to carry — threaded as `null` into every constructed aiCall input,
//! same as book-run.rs's/pl-research.rs's `provider_fn`.
// QUALITY: mixed unit — fully ported deterministic gate/assembly surface (10 modes, all
// fixture-reachable up to their own deferred aiCall dispatch) + 10 sets of pure response-parsing
// helpers (unreachable from the default export, unit-tested directly) + 10 bespoke/deferred aiCall
// dispatches (no fixture/frontier for the calls themselves — see ai-call.rs's module doc).
use regex::Regex;
use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::collections::HashSet;
use std::sync::OnceLock;

// ── standard helpers (playbook) ─────────────────────────────────────────────────────────────
fn truthy(v: Option<&Value>) -> bool { match v { None|Some(Value::Null)=>false, Some(Value::Bool(b))=>*b, Some(Value::Number(n))=>n.as_f64().map_or(false,|f|f!=0.0&&!f.is_nan()), Some(Value::String(s))=>!s.is_empty(), Some(_)=>true } }
fn str_qq(v: Option<&Value>) -> String { match v { None|Some(Value::Null)=>String::new(), Some(Value::String(s))=>s.clone(), Some(Value::Bool(b))=>b.to_string(), Some(n@Value::Number(_))=>n.to_string(), Some(o)=>o.to_string() } } // = JS String(v ?? '')
fn js_string(v: &Value) -> String { match v { Value::Null=>"null".into(), Value::String(s)=>s.clone(), Value::Number(n)=>n.to_string(), Value::Bool(b)=>b.to_string(), Value::Array(a)=>a.iter().map(js_string).collect::<Vec<_>>().join(","), Value::Object(_)=>"[object Object]".into() } } // = JS String(v)
fn tmpl(v: Option<&Value>) -> String { match v { None=>"undefined".into(), Some(Value::Null)=>"null".into(), Some(x)=>str_qq(Some(x)) } } // = JS `${v}`
fn js_number(v: Option<&Value>) -> f64 { match v { None=>f64::NAN, Some(Value::Null)=>0.0, Some(Value::Bool(b))=>if *b {1.0} else {0.0}, Some(Value::Number(n))=>n.as_f64().unwrap_or(f64::NAN), Some(Value::String(s))=>{let t=s.trim(); if t.is_empty(){0.0}else{t.parse().unwrap_or(f64::NAN)}}, Some(_)=>f64::NAN } } // = JS Number(v)
fn take_chars(s: &str, n: usize) -> String { s.chars().take(n).collect() } // JS .slice(0,n) (BMP-equivalent)
fn first(rows: &Value) -> Option<Value> { rows.as_array().and_then(|a| a.first()).cloned() }

// = JS `t.replace(/```[a-z]*\n?|```/g, '')`
fn strip_fences(s: &str) -> String {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"```[a-z]*\n?|```").unwrap()).replace_all(s, "").to_string()
}
// = JS `(t.match(/\{[\s\S]*\}/) ?? [''])[0]` after fence-stripping: greedy first-`{`..last-`}`.
fn extract_json_object(s: &str) -> String {
    let body = strip_fences(s);
    match (body.find('{'), body.rfind('}')) {
        (Some(a), Some(b)) if b > a => body[a..=b].to_string(),
        _ => String::new(),
    }
}
// = JS `[...t.matchAll(/"((?:[^"\\]|\\.)*)"/g)].map(m => m[1]...)`
fn quoted_strings(s: &str) -> Vec<String> {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r#""((?:[^"\\]|\\.)*)""#).unwrap())
        .captures_iter(s)
        .map(|c| c[1].to_string())
        .collect()
}

// ── prompt bodies (verbatim from the JS module) ─────────────────────────────────────────────
const PROMPT: &str = "cockpit-plan-generate";
const SYSTEM: &str = "You are a planning engine for a multi-agent coding cockpit. Output ONLY valid JSON matching: {\"phases\":[{\"id\":\"p1\",\"title\":\"…\",\"items\":[{\"id\":\"p1i1\",\"title\":\"…\",\"intent\":\"one concrete deliverable\",\"files_hint\":[\"path/or/glob\"],\"acceptance\":[\"verifiable criterion\",…],\"agent\":\"forge|muse|patch|prism|sage|metro\",\"size\":\"xs|s|m|l\"}]}]}. Every item MUST carry 1-4 acceptance criteria that a judge could verify against diffs and files. Prefer 2-4 phases for ultra mode, exactly 1 phase for breakdown mode. No prose outside the JSON.";

const PROMPT_BS: &str = "cockpit-plan-brainstorm";
const SYSTEM_BS: &str = "You brainstorm ambitions for a coding session. Given the operator's structured intent (kind of work, areas, boldness, constraints) and the codebase digest, propose exactly 4 DIFFERENT one-line ambitions — each concrete, outcome-shaped, and distinct in angle (e.g. user-facing vs structural vs quick-win vs bold). Output ONLY valid JSON: {\"ideas\":[\"…\",\"…\",\"…\",\"…\"]}. Each idea ONE short sentence, UNDER 22 WORDS, no numbering, no markdown fences.";

const PROMPT_PREP: &str = "cockpit-session-prepare";
const SYSTEM_PREP: &str = "You are the session concierge for a multi-agent coding cockpit: you PREPARE the whole session from the operator's goal. Given the goal, the pane catalog, the formation catalog and the CONTEXT SOURCE catalog, output ONLY valid JSON: {\"mission\":\"one short mission paragraph\",\"shared\":[\"source-key\",…],\"formation\":\"formation-key or null\",\"panes\":[{\"kind\":\"catalog-key\",\"label\":\"short name\",\"agent\":\"forge|muse|patch|prism|sage|metro\",\"why\":\"one short line\",\"reads\":[\"source-key\",…]}]}. Rules: 2-5 panes; at least one code-writing agent kind; include a review pane when code will change; \"shared\" = 2-5 context sources the SESSION needs (only keys from the source catalog); each pane's \"reads\" = the SUBSET of shared that pane actually needs (empty array = it reads everything shared — use that for code-writing agents; give narrow panes narrow reads); propose a formation ONLY when the goal clearly matches one; labels 3 words max; each why UNDER 15 words. No prose outside the JSON.";

const PROMPT_BP: &str = "cockpit-plan-blueprint";
const SYSTEM_BP: &str = "You are the blueprint designer for one work item. Design the SHAPE of the change — no prose, no code bodies. Output ONLY valid JSON: {\"files\":[{\"path\":\"real/relative/path.ext\",\"action\":\"create|modify\",\"symbols\":[{\"name\":\"…\",\"kind\":\"function|component|hook|type|const\",\"signature\":\"(args) => return\",\"intent\":\"one short line\"}]}]}. Max 8 files, max 6 symbols per file. Prefer MODIFYING real existing paths from the grounding over inventing new ones.";

const PROMPT_WIZ: &str = "cockpit-plan-wizard";
const WIZ_DIMS: &str = "product, features, business, market, ux, code";

const PROMPT_BASE: &str = "cockpit-plan-baseplan";
const SYSTEM_BASE: &str = "You forge a BASE PLAN from the ideas the operator picked, then decompose it into DECISION TREES. Output ONLY valid JSON: {\"base_plan_md\":\"a 120-220 word plan in 2-3 short paragraphs (plain markdown, no headers)\",\"decisions\":[{\"q\":\"a real fork the plan forces, as a question\",\"options\":[\"branch A\",\"branch B\",…2-4]}]}. 3-6 decisions spanning the picked dimensions (product, business, market, ux, code) — never trivia; options genuinely different, ≤80 chars. Ground everything in the picks + context; invent nothing beyond them.";

const PROMPT_IDEATE: &str = "cockpit-plan-ideate";
const SYSTEM_IDEATE: &str = "You operate on brainstorm ideas for one software project. Follow the OP instruction exactly. Output ONLY valid JSON: {\"ideas\":[\"…\",…]}. Each idea ONE sentence under 24 words, concrete, grounded in the given context. No prose outside the JSON.";

const PROMPT_SHAPE: &str = "cockpit-plan-shapetree";
const SYSTEM_SHAPE: &str = "You decompose a BASE PLAN into a decision tree of MICROTASKS. Output ONLY valid JSON, nothing else — no prose, no markdown. Shape: {\"branches\":[{\"key\":\"product\",\"title\":\"the product\",\"nodes\":[{\"title\":\"microtask ≤10 words\",\"detail\":\"the nuance, ≤22 words\",\"alts\":[{\"title\":\"path A ≤8 words\",\"why\":\"justification ≤16 words\"}]}]}],\"phases\":[{\"title\":\"phase 1 — …\",\"refs\":[\"product:0\",\"code:2\"]}]}. Use 4-5 branches (dimension keys: product, features, business, market, ux, code — only those the plan truly touches), 3-4 microtask nodes per branch. Give alts ONLY where a real choice exists (max 2, genuinely different, each with a short WHY); most nodes have NO alts. 2-3 phases; refs are \"branchKey:nodeIndex\" strings. Keep every string SHORT so the JSON stays compact. No prose outside the JSON.";

const PROMPT_FILEMAP: &str = "cockpit-plan-filemap";
const SYSTEM_FILEMAP: &str = "You map a software plan's microtasks to the REAL files each would touch. You are given the microtask NODES and the codebase FILE CATALOG — the catalog is the ONLY set of files that exist. Output ONLY valid JSON: {\"assignments\":[{\"node\":\"branchKey:index\",\"files\":[\"exact/path/from/catalog\"]}]}. Rules: use ONLY paths copied VERBATIM from the catalog — never invent, alter or abbreviate a path; map every node you can ground and SKIP nodes that touch no code (product/market ideas); 1-4 files per node, most-directly-touched first. No prose outside the JSON.";

const PROMPT_DEC: &str = "cockpit-plan-decisions";
const SYSTEM_DEC: &str = "You are a planning engine surfacing the DECISIONS a goal forces. Output ONLY valid JSON: {\"decisions\":[{\"q\":\"the decision, phrased as a question\",\"options\":[\"branch A\",\"branch B\",…2-4]}]}. 3 to 7 decisions, each a REAL fork (architecture, storage, UX, sequencing, risk) — never trivia. Options must be genuinely different paths, each ≤80 chars. No prose outside the JSON.";

// generic ensure-prompt (mirrors every JS ensureXPrompt's try/catch-and-compare shape):
// promptFind by name; if found AND template_md matches (AND system_md matches, when checked)
// → no-op; else promptAdd (re)registers it.
fn ensure_prompt_row(db: &Connection, name: &str, template_md: &str, system_md: Option<&str>, check_system: bool, tier: &str, temp: &str, max_tokens: i64, output_fields: Value) -> Result<(), String> {
    if let Ok(row) = crate::ai_prompt_find::promptFind(&json!({ "name": name }), db) {
        let tmpl_ok = row.get("template_md").and_then(|v| v.as_str()) == Some(template_md);
        let sys_ok = !check_system || row.get("system_md").and_then(|v| v.as_str()) == system_md;
        if tmpl_ok && sys_ok { return Ok(()); }
    }
    let mut obj: Map<String, Value> = Map::new();
    obj.insert("name".into(), json!(name));
    obj.insert("template_md".into(), json!(template_md));
    obj.insert("default_tier".into(), json!(tier));
    obj.insert("default_temperature".into(), json!(temp));
    obj.insert("phase_flow".into(), json!("single_only"));
    obj.insert("max_output_tokens".into(), json!(max_tokens));
    obj.insert("output_fields".into(), output_fields);
    if let Some(s) = system_md { obj.insert("system_md".into(), json!(s)); }
    crate::ai_prompt_add::promptAdd(&Value::Object(obj), db)?;
    Ok(())
}
fn ensure_prompt(db: &Connection) -> Result<(), String> {
    ensure_prompt_row(db, PROMPT, "MODE: {{mode}}\nAMBITION:\n{{ambition}}\n\nGROUNDING (the codebase as the substrate knows it):\n{{grounding}}\n\nProduce the plan JSON.", Some(SYSTEM), false, "mid", "cold", 4000, json!([{ "field_name": "plan_json", "field_type": "string" }]))
}
fn ensure_brainstorm_prompt(db: &Connection) -> Result<(), String> {
    ensure_prompt_row(db, PROMPT_BS, "INTENT:\n{{seed}}\n\nCODEBASE DIGEST:\n{{grounding}}\n\nPropose the 4 ambitions (each under 22 words).", Some(SYSTEM_BS), false, "low", "warm", 1600, json!([{ "field_name": "ideas_json", "field_type": "string" }]))
}
fn ensure_prepare_prompt(db: &Connection) -> Result<(), String> {
    ensure_prompt_row(db, PROMPT_PREP, "GOAL:\n{{goal}}\n\nPANE CATALOG (kind — what it does):\n{{catalog}}\n\nCONTEXT SOURCES (key — what it carries):\n{{sources}}\n\nFORMATIONS:\n{{formations}}\n\nCODEBASE DIGEST:\n{{grounding}}\n\nPrepare the session JSON.", Some(SYSTEM_PREP), false, "high", "cold", 2200, json!([{ "field_name": "crew_json", "field_type": "string" }]))
}
fn ensure_blueprint_prompt(db: &Connection) -> Result<(), String> {
    ensure_prompt_row(db, PROMPT_BP, "ITEM: {{item_title}}\nINTENT: {{intent}}\nACCEPTANCE:\n{{criteria}}\nFILES HINT: {{files_hint}}\n\nGROUNDING (the codebase as the substrate knows it):\n{{grounding}}\n\nDesign the blueprint JSON.", Some(SYSTEM_BP), false, "mid", "cold", 2500, json!([{ "field_name": "blueprint_json", "field_type": "string" }]))
}
fn wizard_system() -> String { format!("You are a planning brainstorm wizard for one software project. You think across MULTIPLE DIMENSIONS at once — never collapse to a single angle. Output ONLY valid JSON: {{\"dimensions\":[{{\"key\":\"product\",\"title\":\"the product\",\"ideas\":[\"…\",\"…\",\"…\",\"…\",\"…\"]}},…]}}. You MUST return a SEPARATE dimension object for EVERY one of these keys that the work plausibly touches: {WIZ_DIMS}. Include at least 4 of them (5-6 strongly preferred) — even a code-heavy task has product, ux and market angles worth naming. 5 ideas per dimension, each ONE sentence under 24 words, concrete, distinct in angle, grounded strictly in the context given. NEVER return only one dimension. No prose outside the JSON.") }
fn wizard_template() -> String { format!("OPERATOR SEED: {{{{seed}}}}\n\nCONTEXT (the sources the operator selected):\n{{{{context}}}}\n\nBrainstorm across the dimensions — produce a dimension object for EACH of these that applies (at least 4): {WIZ_DIMS}. Output the dimensions JSON.") }
fn ensure_wizard_prompt(db: &Connection) -> Result<(), String> {
    ensure_prompt_row(db, PROMPT_WIZ, &wizard_template(), Some(&wizard_system()), true, "mid", "warm", 5000, json!([{ "field_name": "dimensions_json", "field_type": "string" }]))
}
fn ensure_baseplan_prompt(db: &Connection) -> Result<(), String> {
    ensure_prompt_row(db, PROMPT_BASE, "OPERATOR SEED: {{seed}}\n\nPICKED IDEAS (the operator chose exactly these):\n{{picks}}\n\nSESSION MISSION:\n{{mission}}\n\nSHARED CONTEXT:\n{{shared}}\n\nForge the base-plan + decisions JSON.", Some(SYSTEM_BASE), false, "mid", "warm", 2200, json!([{ "field_name": "baseplan_json", "field_type": "string" }]))
}
fn ensure_ideate_prompt(db: &Connection) -> Result<(), String> {
    ensure_prompt_row(db, PROMPT_IDEATE, "OP: {{op}}\n\nSEED / GOAL:\n{{seed}}\n\nDIMENSION: {{dimension}}\nEXISTING IDEAS (never repeat these):\n{{existing}}\n\nTARGET IDEA: {{idea}}\nOPERATOR INTENT: {{intent}}\n\nApply the OP and output the ideas JSON.", Some(SYSTEM_IDEATE), false, "mid", "warm", 900, json!([{ "field_name": "ideas_json", "field_type": "string" }]))
}
fn ideate_op(op: &str) -> Option<&'static str> {
    match op {
        "more" => Some("MORE — produce exactly 5 NEW ideas for the DIMENSION, clearly distinct from every existing idea."),
        "expand" => Some("EXPAND — decompose the TARGET IDEA into 4-6 micro-ideas that TOGETHER deliver it (ordered, concrete steps or facets)."),
        "refine" => Some("REFINE — rewrite the TARGET IDEA following the OPERATOR INTENT. Output exactly 1 idea."),
        _ => None,
    }
}
fn ensure_shape_prompt(db: &Connection) -> Result<(), String> {
    ensure_prompt_row(db, PROMPT_SHAPE, "THE BASE PLAN (the artifact to decompose):\n{{artifact}}\n\nSESSION MISSION:\n{{mission}}\n\nSHARED CONTEXT:\n{{shared}}\n\nCODEBASE DIGEST:\n{{grounding}}\n\nDecompose into the compact shape-tree JSON — short strings, only real alternatives.", Some(SYSTEM_SHAPE), true, "mid", "warm", 8000, json!([{ "field_name": "shape_json", "field_type": "string" }]))
}
fn ensure_filemap_prompt(db: &Connection) -> Result<(), String> {
    ensure_prompt_row(db, PROMPT_FILEMAP, "THE MICROTASK NODES (branchKey:index — title — detail):\n{{nodes}}\n\nTHE FILE CATALOG (the only real paths):\n{{catalog}}\n\nMap the nodes to files. Output the JSON.", Some(SYSTEM_FILEMAP), true, "mid", "cold", 2500, json!([{ "field_name": "filemap_json", "field_type": "string" }]))
}
fn ensure_decisions_prompt(db: &Connection) -> Result<(), String> {
    ensure_prompt_row(db, PROMPT_DEC, "GOAL: {{goal}}\n\nSESSION MISSION (may be empty):\n{{mission}}\n\nGROUNDING (the codebase digest):\n{{grounding}}\n\nSurface the decision-tree JSON.", Some(SYSTEM_DEC), false, "mid", "cold", 1200, json!([{ "field_name": "decisions_json", "field_type": "string" }]))
}

// ── response-parsing helpers, ported byte-for-byte from the JS (see module doc: unreachable from
// the default export — every branch's `?` on aiCall short-circuits before its own parser runs —
// but correct by construction and unit-tested directly below) ──────────────────────────────────

// parsePlan (ultra/breakdown mode)
fn parse_plan(text: &str) -> Result<Value, String> {
    let t = text.trim();
    let raw = if t.starts_with('{') { t.to_string() } else { extract_json_object(t) };
    let v: Value = serde_json::from_str(&raw).map_err(|e| e.to_string())?;
    let phases_in = v.get("phases").and_then(|x| x.as_array()).cloned().unwrap_or_default();
    let mut phases: Vec<Value> = Vec::new();
    for (pi, ph) in phases_in.iter().enumerate() {
        let id = match ph.get("id").filter(|x| !x.is_null()) { Some(x) => js_string(x), None => format!("p{}", pi + 1) };
        let title = match ph.get("title").filter(|x| !x.is_null()) { Some(x) => take_chars(&js_string(x), 120), None => take_chars(&format!("phase {}", pi + 1), 120) };
        let items_in = ph.get("items").and_then(|x| x.as_array()).cloned().unwrap_or_default();
        let mut items: Vec<Value> = Vec::new();
        for (ii, it) in items_in.iter().enumerate() {
            let iid = match it.get("id").filter(|x| !x.is_null()) { Some(x) => js_string(x), None => format!("p{}i{}", pi + 1, ii + 1) };
            let ititle = take_chars(&str_qq(it.get("title")), 140);
            if ititle.is_empty() { continue; }
            let intent = take_chars(&str_qq(it.get("intent")), 600);
            let files_hint: Vec<Value> = it.get("files_hint").and_then(|x| x.as_array()).cloned().unwrap_or_default().into_iter().take(12).map(|x| json!(js_string(&x))).collect();
            let acceptance: Vec<Value> = it.get("acceptance").and_then(|x| x.as_array()).cloned().unwrap_or_default().into_iter().take(4).map(|x| json!(js_string(&x))).collect();
            let agent_raw = it.get("agent").and_then(|x| x.as_str()).unwrap_or("");
            let agent = if ["forge", "muse", "patch", "prism", "sage", "metro"].contains(&agent_raw) { agent_raw } else { "forge" };
            let size_raw = it.get("size").and_then(|x| x.as_str()).unwrap_or("");
            let size = if ["xs", "s", "m", "l"].contains(&size_raw) { size_raw } else { "s" };
            items.push(json!({ "id": iid, "title": ititle, "intent": intent, "files_hint": files_hint, "acceptance": acceptance, "agent": agent, "size": size, "status": "planned" }));
        }
        if items.is_empty() { continue; }
        phases.push(json!({ "id": id, "title": title, "items": items }));
    }
    if phases.is_empty() { return Err("plan had no items".to_string()); }
    Ok(json!({ "phases": phases }))
}

// shapetree's cleanBranch
fn clean_branch(b: &Value) -> Value {
    let key = take_chars(&str_qq(b.get("key")), 20);
    let title_src = b.get("title").filter(|x| !x.is_null()).or_else(|| b.get("key").filter(|x| !x.is_null()));
    let title = take_chars(&title_src.map(js_string).unwrap_or_default(), 60);
    let nodes: Vec<Value> = b.get("nodes").and_then(|x| x.as_array()).cloned().unwrap_or_default().into_iter().filter_map(|n| {
        let ntitle = take_chars(&str_qq(n.get("title")), 90);
        if ntitle.is_empty() { return None; }
        let detail = take_chars(&str_qq(n.get("detail")), 200);
        let alts: Vec<Value> = n.get("alts").and_then(|x| x.as_array()).cloned().unwrap_or_default().into_iter().filter_map(|a| {
            let atitle = take_chars(&str_qq(a.get("title")), 80);
            if atitle.is_empty() { return None; }
            Some(json!({ "title": atitle, "why": take_chars(&str_qq(a.get("why")), 160) }))
        }).take(3).collect();
        Some(json!({ "title": ntitle, "detail": detail, "alts": alts, "chosen": Value::Null }))
    }).take(8).collect();
    json!({ "key": key, "title": title, "nodes": nodes })
}
// strict parse: `{branches:[...], phases:[...]}` filtered to non-empty branches
fn shapetree_strict_parse(text: &str) -> Option<(Vec<Value>, Vec<Value>)> {
    let raw = extract_json_object(text);
    let v: Value = serde_json::from_str(&raw).ok()?;
    let branches: Vec<Value> = v.get("branches").and_then(|x| x.as_array()).cloned().unwrap_or_default()
        .iter().map(clean_branch).filter(|b| !str_qq(b.get("key")).is_empty() && b.get("nodes").and_then(|n| n.as_array()).map_or(false, |n| !n.is_empty())).take(6).collect();
    let phases: Vec<Value> = v.get("phases").and_then(|x| x.as_array()).cloned().unwrap_or_default().into_iter().map(|f| {
        json!({ "title": take_chars(&str_qq(f.get("title")), 80), "refs": f.get("refs").and_then(|x| x.as_array()).cloned().unwrap_or_default().into_iter().take(8).map(|x| json!(js_string(&x))).collect::<Vec<_>>() })
    }).filter(|f| !str_qq(f.get("title")).is_empty() && f.get("refs").and_then(|r| r.as_array()).map_or(false, |r| !r.is_empty())).take(4).collect();
    Some((branches, phases))
}
// SALVAGE: recover every COMPLETE `{"key":"…"…"nodes":[…]…}` object even from a truncated array.
fn shapetree_salvage(text: &str) -> Vec<Value> {
    static R: OnceLock<Regex> = OnceLock::new();
    let re = R.get_or_init(|| Regex::new(r#"\{\s*"key"\s*:\s*"([^"]+)"[\s\S]*?"nodes"\s*:\s*(\[[\s\S]*?\])\s*\}"#).unwrap());
    let body = strip_fences(text);
    static TITLE_RE: OnceLock<Regex> = OnceLock::new();
    let title_re = TITLE_RE.get_or_init(|| Regex::new(r#""title"\s*:\s*"([^"]*)""#).unwrap());
    let mut out = Vec::new();
    for m in re.captures_iter(&body) {
        let key = m[1].to_string();
        let whole = m.get(0).unwrap().as_str();
        let title = title_re.captures(whole).map(|c| c[1].to_string()).unwrap_or_else(|| key.clone());
        let nodes: Value = match serde_json::from_str(&m[2]) { Ok(v) => v, Err(_) => continue };
        let b = clean_branch(&json!({ "key": key, "title": title, "nodes": nodes }));
        if !str_qq(b.get("key")).is_empty() && b.get("nodes").and_then(|n| n.as_array()).map_or(false, |n| !n.is_empty()) {
            out.push(b);
        }
        if out.len() >= 6 { break; }
    }
    out
}

// ideate's 3-tier ideas salvage (also reused, with different length caps, by brainstorm)
fn parse_ideas_field(text: &str, max: usize, idea_cap: usize) -> Vec<String> {
    let raw = extract_json_object(text);
    if let Ok(v) = serde_json::from_str::<Value>(&raw) {
        let ideas: Vec<String> = v.get("ideas").and_then(|x| x.as_array()).cloned().unwrap_or_default().iter().map(|x| take_chars(&js_string(x), idea_cap)).filter(|s| !s.is_empty()).take(max).collect();
        if !ideas.is_empty() { return ideas; }
    }
    let quoted: Vec<String> = quoted_strings(text).into_iter().map(|s| s.trim().to_string()).filter(|s| s.chars().count() > 12 && s.chars().count() < 300).take(max).collect();
    if !quoted.is_empty() { return quoted; }
    strip_fences(text).split(|c| c == '\n').flat_map(|s| s.split_inclusive(|c: char| c == '.' || c == '!' || c == '?'))
        .map(|s| s.trim_start_matches(|c: char| c == '-' || c == '*' || c.is_ascii_digit() || c == '.' || c == ')' || c.is_whitespace()).trim().to_string())
        .filter(|s| s.chars().count() > 12 && s.chars().count() < 300).take(max).collect()
}
// ideate mode's op-specific caller (idea cap 240, max 6; refine caller then slices to 1)
fn ideate_parse_ideas(text: &str) -> Vec<String> { parse_ideas_field(text, 6, 240) }
// brainstorm mode's caller (idea cap 240, max 4)
fn brainstorm_parse_ideas(text: &str) -> Vec<String> { parse_ideas_field(text, 4, 240) }

// wizard's dimension cleaner + strict/salvage parse
fn clean_dimensions(arr: &[Value]) -> Vec<Value> {
    arr.iter().filter_map(|d| {
        let key = take_chars(&str_qq(d.get("key")), 20);
        if key.is_empty() { return None; }
        let title_src = d.get("title").filter(|x| !x.is_null()).or_else(|| d.get("key").filter(|x| !x.is_null()));
        let title = take_chars(&title_src.map(js_string).unwrap_or_default(), 60);
        let ideas: Vec<Value> = d.get("ideas").and_then(|x| x.as_array()).cloned().unwrap_or_default().into_iter().map(|i| json!(take_chars(&js_string(&i), 240))).filter(|s| !s.as_str().unwrap_or("").is_empty()).take(8).collect();
        if ideas.is_empty() { return None; }
        Some(json!({ "key": key, "title": title, "ideas": ideas }))
    }).take(6).collect()
}
fn wizard_strict_parse(text: &str) -> Vec<Value> {
    let raw = extract_json_object(text);
    match serde_json::from_str::<Value>(&raw) {
        Ok(v) => clean_dimensions(&v.get("dimensions").and_then(|x| x.as_array()).cloned().unwrap_or_default()),
        Err(_) => vec![],
    }
}
fn salvage_dimensions(text: &str) -> Vec<Value> {
    static R: OnceLock<Regex> = OnceLock::new();
    let re = R.get_or_init(|| Regex::new(r#"\{[^{}]*?"key"\s*:\s*"([^"]+)"[^{}]*?"ideas"\s*:\s*\[([^\]]*)\][^{}]*?\}"#).unwrap());
    let body = strip_fences(text);
    let mut out = Vec::new();
    for m in re.captures_iter(&body) {
        let key = m[1].to_string();
        let ideas: Vec<String> = quoted_strings(&m[2]).into_iter().map(|s| s.replace("\\\"", "\"").trim().to_string()).filter(|s| s.chars().count() > 4).collect();
        if !key.is_empty() && !ideas.is_empty() {
            out.push(json!({ "key": key, "title": key, "ideas": ideas.into_iter().map(Value::String).collect::<Vec<_>>() }));
        }
    }
    out
}
// combinator: strict parse; if < 2 dimensions, prefer the salvage pass if it recovered more.
fn wizard_parse(text: &str) -> Vec<Value> {
    let mut dims = wizard_strict_parse(text);
    if dims.len() < 2 {
        let salv = clean_dimensions(&salvage_dimensions(text));
        if salv.len() > dims.len() { dims = salv; }
    }
    dims
}
// last-resort single-bucket fallback when even salvage found nothing
fn wizard_fallback_bucket(text: &str) -> Option<Value> {
    let ideas: Vec<Value> = quoted_strings(text).into_iter().map(|s| s.trim().to_string()).filter(|s| s.chars().count() > 12 && s.chars().count() < 300).take(8).map(Value::String).collect();
    if ideas.is_empty() { None } else { Some(json!({ "key": "ideas", "title": "ideas", "ideas": ideas })) }
}

// shared by baseplan + decisions modes
fn parse_decision_list(v: &Value) -> Vec<Value> {
    v.as_array().cloned().unwrap_or_default().into_iter().filter_map(|d| {
        let q = take_chars(&str_qq(d.get("q")), 200);
        let options: Vec<Value> = d.get("options").and_then(|x| x.as_array()).cloned().unwrap_or_default().into_iter().map(|o| json!(take_chars(&js_string(&o), 90))).filter(|o| !o.as_str().unwrap_or("").is_empty()).take(4).collect();
        if q.is_empty() || options.len() < 2 { None } else { Some(json!({ "q": q, "options": options })) }
    }).take(7).collect()
}
fn baseplan_parse(text: &str) -> Result<(String, Vec<Value>), String> {
    let raw = extract_json_object(text);
    let v: Value = serde_json::from_str(&raw).map_err(|e| e.to_string())?;
    let base_plan_md = take_chars(&str_qq(v.get("base_plan_md")), 2500);
    let decisions = parse_decision_list(v.get("decisions").unwrap_or(&Value::Null));
    if base_plan_md.is_empty() && decisions.is_empty() { return Err("empty forge".to_string()); }
    Ok((base_plan_md, decisions))
}
fn decisions_parse(text: &str) -> Result<Vec<Value>, String> {
    let raw = extract_json_object(text);
    let v: Value = serde_json::from_str(&raw).map_err(|e| e.to_string())?;
    let decisions = parse_decision_list(v.get("decisions").unwrap_or(&Value::Null));
    if decisions.is_empty() { return Err("no valid decisions".to_string()); }
    Ok(decisions)
}

// prepare mode's crew shape
fn prepare_parse(text: &str, valid_kinds: &HashSet<String>, valid_forms: &HashSet<String>, valid_src: &HashSet<String>) -> Result<Value, String> {
    let raw = extract_json_object(text);
    let v: Value = serde_json::from_str(&raw).map_err(|e| e.to_string())?;
    let shared: Vec<Value> = v.get("shared").and_then(|x| x.as_array()).cloned().unwrap_or_default().into_iter().map(|x| js_string(&x)).filter(|k| valid_src.contains(k)).take(6).map(Value::String).collect();
    let shared_set: HashSet<String> = shared.iter().map(|x| x.as_str().unwrap_or("").to_string()).collect();
    let formation = v.get("formation").and_then(|x| x.as_str()).filter(|f| valid_forms.contains(*f)).map(|f| json!(f)).unwrap_or(Value::Null);
    let panes: Vec<Value> = v.get("panes").and_then(|x| x.as_array()).cloned().unwrap_or_default().into_iter().take(6).map(|p| {
        let kind = p.get("kind").and_then(|x| x.as_str()).filter(|k| valid_kinds.contains(*k)).unwrap_or("agent");
        let mut label = take_chars(&str_qq(p.get("label")), 40);
        if label.is_empty() { label = "agent".to_string(); }
        let agent_raw = p.get("agent").and_then(|x| x.as_str()).unwrap_or("");
        let agent = if ["forge", "muse", "patch", "prism", "sage", "metro"].contains(&agent_raw) { agent_raw } else { "forge" };
        let why = take_chars(&str_qq(p.get("why")), 140);
        let reads: Vec<Value> = p.get("reads").and_then(|x| x.as_array()).cloned().unwrap_or_default().into_iter().map(|x| js_string(&x)).filter(|k| shared_set.contains(k)).take(6).map(Value::String).collect();
        json!({ "kind": kind, "label": label, "agent": agent, "why": why, "reads": reads })
    }).collect();
    if panes.is_empty() && formation.is_null() { return Err("empty crew".to_string()); }
    Ok(json!({ "mission": take_chars(&str_qq(v.get("mission")), 800), "shared": shared, "formation": formation, "panes": panes }))
}

// blueprint mode's shape
fn blueprint_parse(text: &str) -> Result<Value, String> {
    let raw = extract_json_object(text);
    let v: Value = serde_json::from_str(&raw).map_err(|e| e.to_string())?;
    let files: Vec<Value> = v.get("files").and_then(|x| x.as_array()).cloned().unwrap_or_default().into_iter().take(8).filter_map(|f| {
        let path = take_chars(&str_qq(f.get("path")), 200);
        if path.is_empty() { return None; }
        let action = if f.get("action").and_then(|x| x.as_str()) == Some("create") { "create" } else { "modify" };
        let symbols: Vec<Value> = f.get("symbols").and_then(|x| x.as_array()).cloned().unwrap_or_default().into_iter().take(6).map(|sy| {
            let kind_raw = sy.get("kind").filter(|v| !v.is_null()).map(js_string).unwrap_or_else(|| "function".to_string());
            json!({
                "name": take_chars(&str_qq(sy.get("name")), 80),
                "kind": take_chars(&kind_raw, 20),
                "signature": take_chars(&str_qq(sy.get("signature")), 160),
                "intent": take_chars(&str_qq(sy.get("intent")), 160),
            })
        }).collect();
        Some(json!({ "path": path, "action": action, "symbols": symbols }))
    }).collect();
    if files.is_empty() { return Err("no files".to_string()); }
    Ok(json!({ "files": files }))
}

// filemap mode's assignments parse (strict, then regex salvage) + deterministic validation/apply
fn filemap_parse_assignments(text: &str) -> Vec<Value> {
    let raw = extract_json_object(text);
    if let Ok(v) = serde_json::from_str::<Value>(&raw) {
        if let Some(a) = v.get("assignments").and_then(|x| x.as_array()) {
            if !a.is_empty() { return a.clone(); }
        }
    }
    static R: OnceLock<Regex> = OnceLock::new();
    let re = R.get_or_init(|| Regex::new(r#"\{[^{}]*?"node"\s*:\s*"([^"]+)"[^{}]*?"files"\s*:\s*\[([^\]]*)\][^{}]*?\}"#).unwrap());
    re.captures_iter(text).map(|m| {
        let node = m[1].to_string();
        let files: Vec<Value> = quoted_strings(&m[2]).into_iter().map(Value::String).collect();
        json!({ "node": node, "files": files })
    }).collect()
}
// mutates `tree` in place (mirrors the JS's direct node.files mutation); returns (mapped, files_total)
fn apply_filemap_assignments(tree: &mut Value, assignments: &[Value], path_set: &HashSet<String>) -> (usize, usize) {
    static R: OnceLock<Regex> = OnceLock::new();
    let re = R.get_or_init(|| Regex::new(r"^(.+):(\d+)$").unwrap());
    let mut mapped = 0usize;
    let mut files_total = 0usize;
    for a in assignments {
        let node_str = str_qq(a.get("node"));
        let Some(caps) = re.captures(&node_str) else { continue };
        let key = caps[1].to_string();
        let Ok(idx) = caps[2].parse::<usize>() else { continue };
        let Some(branches) = tree.get_mut("branches").and_then(|x| x.as_array_mut()) else { continue };
        let Some(branch) = branches.iter_mut().find(|b| str_qq(b.get("key")) == key) else { continue };
        let Some(nodes) = branch.get_mut("nodes").and_then(|x| x.as_array_mut()) else { continue };
        let Some(node) = nodes.get_mut(idx) else { continue };
        let fs: Vec<String> = a.get("files").and_then(|x| x.as_array()).cloned().unwrap_or_default().into_iter().map(|x| js_string(&x)).filter(|p| path_set.contains(p)).take(4).collect();
        if fs.is_empty() { continue; }
        let mut existing: Vec<String> = node.get("files").and_then(|x| x.as_array()).cloned().unwrap_or_default().into_iter().map(|x| js_string(&x)).collect();
        for f in &fs { if !existing.contains(f) { existing.push(f.clone()); } }
        node["files"] = json!(existing);
        mapped += 1;
        files_total += fs.len();
    }
    (mapped, files_total)
}

// ── grounding/context assembly (reachable, deterministic) ───────────────────────────────────
fn shared_context_text(db: &Connection, session_id: i64, cap: usize) -> String {
    let rows = match crate::db_json::query_rows(db, "SELECT body_md FROM cockpit_blackboard WHERE session_id = ? AND kind = 'context' ORDER BY id DESC LIMIT 8", &[json!(session_id)]) {
        Ok(r) => r,
        Err(_) => return "(none yet)".to_string(),
    };
    let parts: Vec<String> = rows.as_array().cloned().unwrap_or_default().into_iter().map(|r| str_qq(r.get("body_md"))).collect();
    let joined = parts.join("\n---\n");
    if joined.is_empty() { "(none yet)".to_string() } else { take_chars(&joined, cap) }
}
fn resolve_source_text(db: &Connection, kind: &str) -> String {
    let v = crate::cockpit__helpers_context_brief::resolveSourceText(db, &json!(kind), &json!({}));
    str_qq(Some(&v))
}
fn resolve_selected_context(db: &Connection, session_id: i64, sources: &Value) -> String {
    let sel: Vec<String> = match sources.as_array() {
        Some(a) if !a.is_empty() => a.iter().map(js_string).collect(),
        _ => vec!["mission".into(), "shared".into(), "digest".into()],
    };
    let per = std::cmp::max(600, 6000 / std::cmp::max(1, sel.len()));
    let mut parts: Vec<String> = Vec::new();
    for kind in sel.iter().take(20) {
        let text = if kind == "mission" {
            first(&crate::db_json::query_rows(db, "SELECT mission_md FROM cockpit_sessions WHERE id = ?", &[json!(session_id)]).unwrap_or(json!([]))).map(|r| str_qq(r.get("mission_md"))).unwrap_or_default()
        } else if kind == "shared" {
            shared_context_text(db, session_id, per)
        } else {
            resolve_source_text(db, kind)
        };
        if !text.trim().is_empty() { parts.push(format!("【{kind}】\n{}", take_chars(&text, per))); }
    }
    let joined = parts.join("\n\n");
    if joined.is_empty() { "(no context selected)".to_string() } else { take_chars(&joined, 7000) }
}

pub fn cockpitPlanGenerate(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS: `const db = context?.db; if (!db) throw err.contextMissing(...)` — unreachable here
    // (`db: &Connection` is a required, non-nullable reference, same convention as every sibling
    // db-lane twin). `ensureCockpitSchema(db, '')` is a real, ported side effect (self-healing DDL).
    let _ = crate::cockpit_cockpit_schema::ensureCockpitSchema(db, &json!(""));

    let session_id_f = js_number(input.get("session_id"));
    if session_id_f == 0.0 || session_id_f.is_nan() {
        return Err("cockpit-plan-generate: session_id is required".to_string());
    }
    let session_id = session_id_f as i64;
    let pane_id_f = js_number(input.get("pane_id"));
    let ambition = str_qq(input.get("ambition")).trim().to_string();
    let mode_raw = input.get("mode").and_then(|v| v.as_str()).unwrap_or("");

    // ── ULTRA PLAN persistence (no AI) ──────────────────────────────────────────────────────
    if mode_raw == "plan-save" {
        let bj = input.get("brainstorm_json").filter(|v| !v.is_null()).map(|v| take_chars(&js_string(v), 60000));
        let bp = input.get("baseplan_md").filter(|v| !v.is_null()).map(|v| take_chars(&js_string(v), 20000));
        let title = { let t = take_chars(&str_qq(input.get("title")), 160); if t.is_empty() { None } else { Some(t) } };
        let bj_v = bj.map(Value::String).unwrap_or(Value::Null);
        let bp_v = bp.map(Value::String).unwrap_or(Value::Null);
        let title_v = title.map(Value::String).unwrap_or(Value::Null);
        let existing = first(&crate::db_json::query_rows(db, "SELECT id FROM ultra_plans WHERE session_id = ? ORDER BY id DESC LIMIT 1", &[json!(session_id)])?);
        let id: Value = if let Some(row) = existing {
            let id = row.get("id").cloned().unwrap_or(Value::Null);
            crate::db_json::run_write(db, &format!("UPDATE ultra_plans SET brainstorm_json = COALESCE(?, brainstorm_json), baseplan_md = COALESCE(?, baseplan_md), title = COALESCE(?, title), updated_at = {} WHERE id = ?", crate::clock::now_sql()), &[bj_v, bp_v, title_v, id.clone()])?;
            id
        } else {
            let (_, rowid) = crate::db_json::run_write(db, &format!("INSERT INTO ultra_plans (session_id, title, brainstorm_json, baseplan_md, updated_at) VALUES (?,?,?,?, {})", crate::clock::now_sql()), &[json!(session_id), title_v, bj_v, bp_v])?;
            json!(rowid)
        };
        return Ok(json!({ "ok": true, "ultra_plan_id": id }));
    }
    if mode_raw == "plan-get" {
        let row = first(&crate::db_json::query_rows(db, "SELECT id, title, brainstorm_json, baseplan_md, created_at, updated_at FROM ultra_plans WHERE session_id = ? ORDER BY id DESC LIMIT 1", &[json!(session_id)])?);
        let Some(row) = row else { return Ok(json!({ "ok": true, "plan": Value::Null })); };
        let brainstorm = row.get("brainstorm_json").and_then(|v| v.as_str()).filter(|s| !s.is_empty()).and_then(|s| serde_json::from_str::<Value>(s).ok()).unwrap_or(Value::Null);
        return Ok(json!({ "ok": true, "plan": {
            "id": row.get("id").cloned().unwrap_or(Value::Null),
            "title": row.get("title").cloned().unwrap_or(Value::Null),
            "brainstorm": brainstorm,
            "baseplan_md": row.get("baseplan_md").filter(|v| !v.is_null()).cloned().unwrap_or(json!("")),
            "created_at": row.get("created_at").cloned().unwrap_or(Value::Null),
            "updated_at": row.get("updated_at").cloned().unwrap_or(Value::Null),
        } }));
    }

    // ── FILEMAP ──────────────────────────────────────────────────────────────────────────────
    if mode_raw == "filemap" {
        let shape_row = first(&crate::db_json::query_rows(db, "SELECT id, body_md FROM cockpit_blackboard WHERE session_id = ? AND kind = 'shape' ORDER BY id DESC LIMIT 1", &[json!(session_id)])?);
        let tree: Value = shape_row.as_ref().and_then(|r| r.get("body_md")).and_then(|v| v.as_str()).and_then(|s| serde_json::from_str(s).ok()).unwrap_or(Value::Null);
        let branches_empty = tree.get("branches").and_then(|v| v.as_array()).map_or(true, |a| a.is_empty());
        if branches_empty {
            return Ok(json!({ "ok": false, "error": "no shape tree yet — grow it first" }));
        }
        let origin = first(&crate::db_json::query_rows(db, "SELECT origin, COUNT(*) c FROM code.code_nodes WHERE type = 'file' AND origin NOT LIKE 'blueprint:%' GROUP BY origin ORDER BY c DESC LIMIT 1", &[]).unwrap_or(json!([]))).and_then(|r| r.get("origin").cloned()).filter(|v| !v.is_null());
        let atlas_files: Vec<Value> = if let Some(o) = &origin {
            crate::db_json::query_rows(db, "SELECT path, intent FROM code.code_nodes WHERE origin = ? AND type = 'file' ORDER BY path", &[o.clone()]).unwrap_or(json!([])).as_array().cloned().unwrap_or_default()
        } else { vec![] };
        if atlas_files.is_empty() {
            return Ok(json!({ "ok": false, "error": "the code atlas is not onboarded for this workspace" }));
        }
        let mut node_lines: Vec<String> = Vec::new();
        if let Some(branches) = tree.get("branches").and_then(|v| v.as_array()) {
            for b in branches {
                let key = tmpl(b.get("key"));
                for (i, n) in b.get("nodes").and_then(|v| v.as_array()).cloned().unwrap_or_default().iter().enumerate() {
                    let mut line = format!("{key}:{i} — {}", tmpl(n.get("title")));
                    if truthy(n.get("detail")) { line.push_str(&format!(" — {}", tmpl(n.get("detail")))); }
                    node_lines.push(line);
                }
            }
        }
        if node_lines.is_empty() {
            return Ok(json!({ "ok": false, "error": "the shape tree has no nodes yet" }));
        }
        ensure_filemap_prompt(db)?;
        let nodes_val = node_lines.into_iter().take(40).collect::<Vec<_>>().join("\n");
        let catalog_val: String = take_chars(
            &atlas_files.iter().take(220).map(|f| {
                let path = tmpl(f.get("path"));
                if truthy(f.get("intent")) { format!("{path} — {}", take_chars(&tmpl(f.get("intent")), 70)) } else { path }
            }).collect::<Vec<_>>().join("\n"),
            9000,
        );
        let ai_input = json!({ "prompt_name": PROMPT_FILEMAP, "input_kv": [ { "key": "nodes", "value": nodes_val }, { "key": "catalog", "value": catalog_val } ], "provider_fn": Value::Null });
        let _res = crate::ai_ai_call::aiCall(&ai_input, db)?;
        unreachable!("aiCall twin always returns Err — see module doc")
    }

    const MODES: [&str; 9] = ["breakdown", "brainstorm", "blueprint", "prepare", "decisions", "wizard", "baseplan", "ideate", "shapetree"];
    let mode = if MODES.contains(&mode_raw) { mode_raw.to_string() } else { "ultra".to_string() };

    // ── SHAPETREE ────────────────────────────────────────────────────────────────────────────
    if mode == "shapetree" {
        ensure_shape_prompt(db)?;
        let sess = first(&crate::db_json::query_rows(db, "SELECT mission_md FROM cockpit_sessions WHERE id = ?", &[json!(session_id)])?);
        let mission = sess.as_ref().map(|r| str_qq(r.get("mission_md"))).unwrap_or_default();
        let mission_val = if mission.is_empty() { "(none)".to_string() } else { take_chars(&mission, 1000) };
        let grounding_val = { let d = resolve_source_text(db, "digest"); if d.is_empty() { "(no digest yet)".to_string() } else { take_chars(&d, 2000) } };
        let ai_input = json!({
            "prompt_name": PROMPT_SHAPE,
            "input_kv": [
                { "key": "artifact", "value": take_chars(&ambition, 3000) },
                { "key": "mission", "value": mission_val },
                { "key": "shared", "value": shared_context_text(db, session_id, 2400) },
                { "key": "grounding", "value": grounding_val },
            ],
            "provider_fn": Value::Null,
        });
        let _res = crate::ai_ai_call::aiCall(&ai_input, db)?;
        unreachable!("aiCall twin always returns Err — see module doc")
    }

    // ── IDEATE ───────────────────────────────────────────────────────────────────────────────
    if mode == "ideate" {
        let op = input.get("op").and_then(|v| v.as_str()).unwrap_or("");
        let Some(op_text) = ideate_op(op) else { return Ok(json!({ "ok": false, "error": format!("unknown ideate op '{op}' (more | expand | refine)") })); };
        if op == "refine" && str_qq(input.get("intent")).trim().is_empty() {
            return Ok(json!({ "ok": false, "error": "refine needs the operator intent" }));
        }
        ensure_ideate_prompt(db)?;
        let existing_lines: String = input.get("existing").and_then(|v| v.as_array()).cloned().unwrap_or_default().into_iter().take(16).map(|s| format!("- {}", take_chars(&js_string(&s), 160))).collect::<Vec<_>>().join("\n");
        let seed_val = { let a = take_chars(&ambition, 600); if a.is_empty() { "(none)".to_string() } else { a } };
        let dimension_val = { let d = take_chars(&str_qq(input.get("dimension")), 80); if d.is_empty() { "(none)".to_string() } else { d } };
        let existing_val = if existing_lines.is_empty() { "(none)".to_string() } else { existing_lines };
        let idea_val = { let i = take_chars(&str_qq(input.get("idea")), 300); if i.is_empty() { "(none)".to_string() } else { i } };
        let intent_val = { let i = take_chars(&str_qq(input.get("intent")), 300); if i.is_empty() { "(none)".to_string() } else { i } };
        let ai_input = json!({
            "prompt_name": PROMPT_IDEATE,
            "input_kv": [
                { "key": "op", "value": op_text },
                { "key": "seed", "value": seed_val },
                { "key": "dimension", "value": dimension_val },
                { "key": "existing", "value": existing_val },
                { "key": "idea", "value": idea_val },
                { "key": "intent", "value": intent_val },
            ],
            "provider_fn": Value::Null,
        });
        let _res = crate::ai_ai_call::aiCall(&ai_input, db)?;
        unreachable!("aiCall twin always returns Err — see module doc")
    }

    // ── WIZARD ───────────────────────────────────────────────────────────────────────────────
    if mode == "wizard" {
        ensure_wizard_prompt(db)?;
        let ai_input = json!({
            "prompt_name": PROMPT_WIZ,
            "input_kv": [
                { "key": "seed", "value": take_chars(&ambition, 800) },
                { "key": "context", "value": resolve_selected_context(db, session_id, input.get("sources").unwrap_or(&Value::Null)) },
            ],
            "provider_fn": Value::Null,
        });
        let _res = crate::ai_ai_call::aiCall(&ai_input, db)?;
        unreachable!("aiCall twin always returns Err — see module doc")
    }

    // ── BASEPLAN ─────────────────────────────────────────────────────────────────────────────
    if mode == "baseplan" {
        let picks: Vec<String> = input.get("picks").and_then(|v| v.as_array()).cloned().unwrap_or_default().into_iter().map(|s| take_chars(&js_string(&s), 240)).filter(|s| !s.is_empty()).take(14).collect();
        if picks.is_empty() {
            return Ok(json!({ "ok": false, "error": "baseplan needs the picked ideas" }));
        }
        ensure_baseplan_prompt(db)?;
        let sess = first(&crate::db_json::query_rows(db, "SELECT mission_md FROM cockpit_sessions WHERE id = ?", &[json!(session_id)])?);
        let mission = sess.as_ref().map(|r| str_qq(r.get("mission_md"))).unwrap_or_default();
        let seed_val = { let a = take_chars(&ambition, 600); if a.is_empty() { "(the picks speak for themselves)".to_string() } else { a } };
        let mission_val = { let m = take_chars(&mission, 1200); if m.is_empty() { "(none)".to_string() } else { m } };
        let ai_input = json!({
            "prompt_name": PROMPT_BASE,
            "input_kv": [
                { "key": "seed", "value": seed_val },
                { "key": "picks", "value": picks.iter().enumerate().map(|(i, s)| format!("{}. {s}", i + 1)).collect::<Vec<_>>().join("\n") },
                { "key": "mission", "value": mission_val },
                { "key": "shared", "value": shared_context_text(db, session_id, 3200) },
            ],
            "provider_fn": Value::Null,
        });
        let _res = crate::ai_ai_call::aiCall(&ai_input, db)?;
        unreachable!("aiCall twin always returns Err — see module doc")
    }

    // ── DECISIONS ────────────────────────────────────────────────────────────────────────────
    if mode == "decisions" {
        ensure_decisions_prompt(db)?;
        let sess = first(&crate::db_json::query_rows(db, "SELECT mission_md FROM cockpit_sessions WHERE id = ?", &[json!(session_id)])?);
        let mission = sess.as_ref().map(|r| str_qq(r.get("mission_md"))).unwrap_or_default();
        let mission_val = { let m = take_chars(&mission, 1500); if m.is_empty() { "(none)".to_string() } else { m } };
        let grounding_val = { let d = resolve_source_text(db, "digest"); if d.is_empty() { "(no digest yet)".to_string() } else { take_chars(&d, 2500) } };
        let ai_input = json!({
            "prompt_name": PROMPT_DEC,
            "input_kv": [
                { "key": "goal", "value": take_chars(&ambition, 600) },
                { "key": "mission", "value": mission_val },
                { "key": "grounding", "value": grounding_val },
            ],
            "provider_fn": Value::Null,
        });
        let _res = crate::ai_ai_call::aiCall(&ai_input, db)?;
        unreachable!("aiCall twin always returns Err — see module doc")
    }

    if pane_id_f == 0.0 || pane_id_f.is_nan() {
        if mode != "prepare" { return Err("cockpit-plan-generate: pane_id is required".to_string()); }
    }
    let pane_id = pane_id_f as i64;
    if ambition.is_empty() && mode != "blueprint" {
        return Err("cockpit-plan-generate: ambition is required".to_string());
    }

    // ── PREPARE ──────────────────────────────────────────────────────────────────────────────
    if mode == "prepare" {
        let catalog: Vec<Value> = input.get("catalog").and_then(|v| v.as_array()).cloned().unwrap_or_default().into_iter().map(|c| json!({ "key": take_chars(&str_qq(c.get("key")), 40), "desc": take_chars(&str_qq(c.get("desc")), 200) })).filter(|c| !str_qq(c.get("key")).is_empty()).take(40).collect();
        let formations: Vec<Value> = input.get("formations").and_then(|v| v.as_array()).cloned().unwrap_or_default().into_iter().map(|f| json!({ "key": take_chars(&str_qq(f.get("key")), 40), "desc": take_chars(&str_qq(f.get("desc")), 200) })).filter(|f| !str_qq(f.get("key")).is_empty()).take(12).collect();
        let sources: Vec<Value> = input.get("sources").and_then(|v| v.as_array()).cloned().unwrap_or_default().into_iter().map(|s| json!({ "key": take_chars(&str_qq(s.get("key")), 40), "desc": take_chars(&str_qq(s.get("desc")), 120) })).filter(|s| !str_qq(s.get("key")).is_empty()).take(24).collect();
        if catalog.is_empty() {
            return Ok(json!({ "ok": false, "error": "prepare needs the pane catalog" }));
        }
        ensure_prepare_prompt(db)?;
        let sources_val = { let s = sources.iter().map(|s| format!("{} — {}", str_qq(s.get("key")), str_qq(s.get("desc")))).collect::<Vec<_>>().join("\n"); if s.is_empty() { "(none available)".to_string() } else { s } };
        let formations_val = { let f = formations.iter().map(|f| format!("{} — {}", str_qq(f.get("key")), str_qq(f.get("desc")))).collect::<Vec<_>>().join("\n"); if f.is_empty() { "(none available)".to_string() } else { f } };
        let grounding_val = { let d = resolve_source_text(db, "digest"); if d.is_empty() { "(no digest yet)".to_string() } else { take_chars(&d, 2500) } };
        let ai_input = json!({
            "prompt_name": PROMPT_PREP,
            "tier": "high",
            "input_kv": [
                { "key": "goal", "value": take_chars(&ambition, 1200) },
                { "key": "catalog", "value": catalog.iter().map(|c| format!("{} — {}", str_qq(c.get("key")), str_qq(c.get("desc")))).collect::<Vec<_>>().join("\n") },
                { "key": "sources", "value": sources_val },
                { "key": "formations", "value": formations_val },
                { "key": "grounding", "value": grounding_val },
            ],
            "provider_fn": Value::Null,
        });
        let _res = crate::ai_ai_call::aiCall(&ai_input, db)?;
        unreachable!("aiCall twin always returns Err — see module doc")
    }

    // ── BLUEPRINT ────────────────────────────────────────────────────────────────────────────
    if mode == "blueprint" {
        let item_id = str_qq(input.get("item_id"));
        if item_id.is_empty() { return Err("cockpit-plan-generate: item_id is required".to_string()); }
        let plan_row = first(&crate::db_json::query_rows(db, "SELECT id, body_md FROM cockpit_blackboard WHERE session_id = ? AND pane_id = ? AND kind = 'plan' ORDER BY id DESC LIMIT 1", &[json!(session_id), json!(pane_id)])?);
        let Some(plan_row) = plan_row else { return Ok(json!({ "ok": false, "error": "no plan on this pane yet" })); };
        let plan_obj: Value = match plan_row.get("body_md").and_then(|v| v.as_str()).and_then(|s| serde_json::from_str(s).ok()) {
            Some(v) => v,
            None => return Ok(json!({ "ok": false, "error": "plan row unreadable" })),
        };
        let mut item: Option<Value> = None;
        for ph in plan_obj.get("phases").and_then(|v| v.as_array()).cloned().unwrap_or_default() {
            if let Some(hit) = ph.get("items").and_then(|v| v.as_array()).and_then(|items| items.iter().find(|x| str_qq(x.get("id")) == item_id)) {
                item = Some(hit.clone());
                break;
            }
        }
        let Some(item) = item else { return Ok(json!({ "ok": false, "error": format!("item {item_id} not found") })); };
        ensure_blueprint_prompt(db)?;
        let digest = resolve_source_text(db, "digest");
        let ast = resolve_source_text(db, "ast");
        let grounding_parts: Vec<String> = [take_chars(&digest, 3500), take_chars(&ast, 4500)].into_iter().filter(|s| !s.is_empty()).collect();
        let grounding = if grounding_parts.is_empty() { "(atlas not onboarded)".to_string() } else { grounding_parts.join("\n\n") };
        let criteria: String = item.get("acceptance").and_then(|v| v.as_array()).cloned().unwrap_or_default().iter().enumerate().map(|(i, c)| format!("{}. {}", i + 1, js_string(c))).collect::<Vec<_>>().join("\n");
        let files_hint: String = item.get("files_hint").and_then(|v| v.as_array()).cloned().unwrap_or_default().iter().map(js_string).collect::<Vec<_>>().join(", ");
        let intent_val = { let i = str_qq(item.get("intent")); if i.is_empty() { "(none)".to_string() } else { i } };
        let criteria_val = if criteria.is_empty() { "(none)".to_string() } else { criteria };
        let files_hint_val = if files_hint.is_empty() { "(none)".to_string() } else { files_hint };
        let ai_input = json!({
            "prompt_name": PROMPT_BP,
            "tier": "mid",
            "input_kv": [
                { "key": "item_title", "value": item.get("title").cloned().unwrap_or(Value::Null) },
                { "key": "intent", "value": intent_val },
                { "key": "criteria", "value": criteria_val },
                { "key": "files_hint", "value": files_hint_val },
                { "key": "grounding", "value": grounding },
            ],
            "provider_fn": Value::Null,
        });
        let _res = crate::ai_ai_call::aiCall(&ai_input, db)?;
        unreachable!("aiCall twin always returns Err — see module doc")
    }

    // ── BRAINSTORM ───────────────────────────────────────────────────────────────────────────
    if mode == "brainstorm" {
        ensure_brainstorm_prompt(db)?;
        let grounding_val = { let d = resolve_source_text(db, "digest"); if d.is_empty() { "(no digest)".to_string() } else { take_chars(&d, 4000) } };
        let ai_input = json!({
            "prompt_name": PROMPT_BS,
            "tier": "low",
            "input_kv": [
                { "key": "seed", "value": take_chars(&ambition, 2000) },
                { "key": "grounding", "value": grounding_val },
            ],
            "provider_fn": Value::Null,
        });
        let _res = crate::ai_ai_call::aiCall(&ai_input, db)?;
        unreachable!("aiCall twin always returns Err — see module doc")
    }

    // ── ULTRA / BREAKDOWN ────────────────────────────────────────────────────────────────────
    let grounding = if mode == "ultra" {
        let mut parts: Vec<String> = Vec::new();
        for kind in ["docs", "digest", "failures", "memory"] {
            let t = resolve_source_text(db, kind);
            if !t.is_empty() { parts.push(format!("── {} ──\n{}", kind.to_uppercase(), take_chars(&t, 4500))); }
        }
        let joined = take_chars(&parts.join("\n\n"), 16000);
        if joined.is_empty() { "(atlas not onboarded — plan from the ambition alone)".to_string() } else { joined }
    } else {
        let d = resolve_source_text(db, "digest");
        if d.is_empty() { "(no digest)".to_string() } else { take_chars(&d, 5000) }
    };
    ensure_prompt(db)?;
    let ai_input = json!({
        "prompt_name": PROMPT,
        "tier": if mode == "ultra" { "high" } else { "mid" },
        "input_kv": [
            { "key": "mode", "value": mode },
            { "key": "ambition", "value": take_chars(&ambition, 4000) },
            { "key": "grounding", "value": grounding },
        ],
        "provider_fn": Value::Null,
    });
    let _res = crate::ai_ai_call::aiCall(&ai_input, db)?;
    unreachable!("aiCall twin always returns Err — see module doc")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS brain; ATTACH ':memory:' AS code;").unwrap();
        c.execute_batch(
            "CREATE TABLE cockpit_sessions (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL, status TEXT NOT NULL DEFAULT 'active', mission_md TEXT, base_branch TEXT NOT NULL DEFAULT '', base_sha TEXT NOT NULL DEFAULT '', mode TEXT NOT NULL DEFAULT 'steer', created_at TEXT, updated_at TEXT);
             CREATE TABLE cockpit_panes (id INTEGER PRIMARY KEY AUTOINCREMENT, session_id INTEGER NOT NULL, idx INTEGER NOT NULL, subagent_name TEXT NOT NULL DEFAULT '', scope_json TEXT NOT NULL DEFAULT '[]', grants_json TEXT NOT NULL DEFAULT '[]', worktree_path TEXT, branch TEXT, tier TEXT NOT NULL DEFAULT 'low', base_agent TEXT, extra_prompt_md TEXT, model_id TEXT, calls INTEGER NOT NULL DEFAULT 0, cost_usd REAL NOT NULL DEFAULT 0, status TEXT NOT NULL DEFAULT 'idle', last_activity TEXT);
             CREATE TABLE cockpit_blackboard (id INTEGER PRIMARY KEY AUTOINCREMENT, session_id INTEGER NOT NULL, pane_id INTEGER, kind TEXT NOT NULL DEFAULT 'note', body_md TEXT NOT NULL DEFAULT '', created_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')));
             CREATE TABLE cockpit_claims (id INTEGER PRIMARY KEY AUTOINCREMENT, session_id INTEGER NOT NULL, pane_id INTEGER NOT NULL, path_glob TEXT NOT NULL, created_at TEXT);
             CREATE TABLE ultra_plans (id INTEGER PRIMARY KEY AUTOINCREMENT, session_id INTEGER NOT NULL, title TEXT, brainstorm_json TEXT, baseplan_md TEXT, created_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')), updated_at TEXT);
             CREATE TABLE code_nodes (path TEXT, origin TEXT, type TEXT, name TEXT, signature TEXT);
             CREATE TABLE code.code_nodes (path TEXT, origin TEXT, type TEXT, intent TEXT);
             CREATE TABLE brain.prompt_templates ( id INTEGER PRIMARY KEY, name TEXT NOT NULL, version INTEGER NOT NULL, template_md TEXT NOT NULL, default_tier TEXT NOT NULL, default_temperature TEXT NOT NULL, max_output_tokens INTEGER NOT NULL DEFAULT 1024, superseded_by INTEGER, created_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')), cross_field_rules_json TEXT, judging_criteria_json TEXT, system_md TEXT, phase_flow TEXT NOT NULL DEFAULT 'single', UNIQUE (name, version) );
             CREATE TABLE brain.prompt_output_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, grounded_refs_json TEXT, UNIQUE (prompt_template_id, field_name) );
             CREATE TABLE brain.prompt_input_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, UNIQUE (prompt_template_id, field_name) );",
        ).unwrap();
        c
    }

    fn a_session(c: &Connection) -> i64 {
        c.execute("INSERT INTO cockpit_sessions (name) VALUES ('S')", []).unwrap();
        c.last_insert_rowid()
    }

    #[test]
    fn session_id_is_required() {
        let c = setup();
        let e = cockpitPlanGenerate(&json!({}), &c).unwrap_err();
        assert!(e.contains("session_id is required"));
        let e2 = cockpitPlanGenerate(&json!({ "session_id": 0 }), &c).unwrap_err();
        assert!(e2.contains("session_id is required"));
    }

    #[test]
    fn plan_save_inserts_then_updates_in_place() {
        let c = setup();
        let sid = a_session(&c);
        let r = cockpitPlanGenerate(&json!({ "session_id": sid, "mode": "plan-save", "brainstorm_json": {"a":1}, "title": "Ambition One" }), &c).unwrap();
        assert_eq!(r["ok"], json!(true));
        let id1 = r["ultra_plan_id"].clone();
        let r2 = cockpitPlanGenerate(&json!({ "session_id": sid, "mode": "plan-save", "baseplan_md": "the plan" }), &c).unwrap();
        assert_eq!(r2["ultra_plan_id"], id1); // updated in place, not a new row
        let n: i64 = c.query_row("SELECT COUNT(*) FROM ultra_plans", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1);
        let title: String = c.query_row("SELECT title FROM ultra_plans WHERE id = ?", [id1.as_i64().unwrap()], |r| r.get(0)).unwrap();
        assert_eq!(title, "Ambition One"); // COALESCE kept it on the second (title-less) save
    }

    #[test]
    fn plan_get_returns_null_when_nothing_saved_and_parses_brainstorm() {
        let c = setup();
        let sid = a_session(&c);
        let none = cockpitPlanGenerate(&json!({ "session_id": sid, "mode": "plan-get" }), &c).unwrap();
        assert_eq!(none["plan"], Value::Null);
        // brainstorm_json must be a JSON STRING, not a raw object: JS's `String(v).slice(...)`
        // on a plain object yields "[object Object]" (verified via a node repro against real
        // cockpitPlanGenerate — raw-object input round-trips to plan.brainstorm === null in
        // production too), which plan-get's JSON.parse then can't read back. Every real caller
        // pre-stringifies (see cockpit-plan-generate.test.mjs:126, `JSON.stringify({...})`).
        let bs = json!({"x": [1,2]}).to_string();
        cockpitPlanGenerate(&json!({ "session_id": sid, "mode": "plan-save", "brainstorm_json": bs }), &c).unwrap();
        let got = cockpitPlanGenerate(&json!({ "session_id": sid, "mode": "plan-get" }), &c).unwrap();
        assert_eq!(got["plan"]["brainstorm"], json!({"x":[1,2]}));
        assert_eq!(got["plan"]["baseplan_md"], json!(""));
    }

    #[test]
    fn filemap_requires_a_shape_tree_first() {
        let c = setup();
        let sid = a_session(&c);
        let r = cockpitPlanGenerate(&json!({ "session_id": sid, "pane_id": 1, "ambition": "x", "mode": "filemap" }), &c).unwrap();
        assert_eq!(r, json!({ "ok": false, "error": "no shape tree yet — grow it first" }));
    }

    #[test]
    fn filemap_requires_an_onboarded_atlas() {
        let c = setup();
        let sid = a_session(&c);
        c.execute("INSERT INTO cockpit_blackboard (session_id, kind, body_md) VALUES (?, 'shape', ?)", rusqlite::params![sid, r#"{"branches":[{"key":"code","nodes":[{"title":"t"}]}]}"#]).unwrap();
        let r = cockpitPlanGenerate(&json!({ "session_id": sid, "pane_id": 1, "ambition": "x", "mode": "filemap" }), &c).unwrap();
        assert_eq!(r, json!({ "ok": false, "error": "the code atlas is not onboarded for this workspace" }));
    }

    #[test]
    fn filemap_reaches_the_parity_deferred_aicall_boundary_once_grounded() {
        let c = setup();
        let sid = a_session(&c);
        c.execute("INSERT INTO cockpit_blackboard (session_id, kind, body_md) VALUES (?, 'shape', ?)", rusqlite::params![sid, r#"{"branches":[{"key":"code","nodes":[{"title":"t"}]}]}"#]).unwrap();
        c.execute("INSERT INTO code.code_nodes (path, origin, type, intent) VALUES ('src/a.mjs','atlas','file','does a')", []).unwrap();
        let err = cockpitPlanGenerate(&json!({ "session_id": sid, "pane_id": 1, "ambition": "x", "mode": "filemap" }), &c).unwrap_err();
        assert!(err.contains("ai-call"), "unexpected error: {err}");
        let n: i64 = c.query_row("SELECT COUNT(*) FROM brain.prompt_templates WHERE name = 'cockpit-plan-filemap'", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn pane_id_required_unless_prepare_and_ambition_required_unless_blueprint() {
        let c = setup();
        let sid = a_session(&c);
        let e = cockpitPlanGenerate(&json!({ "session_id": sid, "ambition": "do it" }), &c).unwrap_err();
        assert!(e.contains("pane_id is required"));
        let e2 = cockpitPlanGenerate(&json!({ "session_id": sid, "pane_id": 1 }), &c).unwrap_err();
        assert!(e2.contains("ambition is required"));
    }

    #[test]
    fn ideate_rejects_unknown_op_and_refine_without_intent() {
        let c = setup();
        let sid = a_session(&c);
        let r = cockpitPlanGenerate(&json!({ "session_id": sid, "pane_id": 1, "mode": "ideate", "op": "bogus" }), &c).unwrap();
        assert_eq!(r["ok"], json!(false));
        assert!(r["error"].as_str().unwrap().contains("unknown ideate op"));
        let r2 = cockpitPlanGenerate(&json!({ "session_id": sid, "pane_id": 1, "mode": "ideate", "op": "refine" }), &c).unwrap();
        assert_eq!(r2["error"], json!("refine needs the operator intent"));
    }

    #[test]
    fn ideate_reaches_the_parity_deferred_aicall_boundary() {
        let c = setup();
        let sid = a_session(&c);
        let err = cockpitPlanGenerate(&json!({ "session_id": sid, "pane_id": 1, "mode": "ideate", "op": "more", "dimension": "code" }), &c).unwrap_err();
        assert!(err.contains("ai-call"));
    }

    #[test]
    fn baseplan_needs_picks() {
        let c = setup();
        let sid = a_session(&c);
        let r = cockpitPlanGenerate(&json!({ "session_id": sid, "pane_id": 1, "mode": "baseplan" }), &c).unwrap();
        assert_eq!(r, json!({ "ok": false, "error": "baseplan needs the picked ideas" }));
    }

    #[test]
    fn prepare_needs_catalog_and_does_not_require_pane_id() {
        let c = setup();
        let sid = a_session(&c);
        let r = cockpitPlanGenerate(&json!({ "session_id": sid, "ambition": "goal", "mode": "prepare" }), &c).unwrap();
        assert_eq!(r, json!({ "ok": false, "error": "prepare needs the pane catalog" }));
        let err = cockpitPlanGenerate(&json!({ "session_id": sid, "ambition": "goal", "mode": "prepare", "catalog": [{"key":"forge","desc":"writes code"}] }), &c).unwrap_err();
        assert!(err.contains("ai-call"));
    }

    #[test]
    fn blueprint_requires_item_id_and_an_existing_plan() {
        let c = setup();
        let sid = a_session(&c);
        let e = cockpitPlanGenerate(&json!({ "session_id": sid, "pane_id": 1, "mode": "blueprint" }), &c).unwrap_err();
        assert!(e.contains("item_id is required"));
        let r = cockpitPlanGenerate(&json!({ "session_id": sid, "pane_id": 1, "mode": "blueprint", "item_id": "p1i1" }), &c).unwrap();
        assert_eq!(r, json!({ "ok": false, "error": "no plan on this pane yet" }));
    }

    #[test]
    fn blueprint_finds_item_across_phases_then_reaches_deferred_boundary() {
        let c = setup();
        let sid = a_session(&c);
        let plan = json!({ "phases": [ { "id": "p1", "items": [ { "id": "p1i1", "title": "Item One", "intent": "do it", "acceptance": ["works"], "files_hint": ["a.mjs"] } ] } ] });
        c.execute("INSERT INTO cockpit_blackboard (session_id, pane_id, kind, body_md) VALUES (?, 1, 'plan', ?)", rusqlite::params![sid, plan.to_string()]).unwrap();
        let r = cockpitPlanGenerate(&json!({ "session_id": sid, "pane_id": 1, "mode": "blueprint", "item_id": "nope" }), &c).unwrap();
        assert_eq!(r, json!({ "ok": false, "error": "item nope not found" }));
        let err = cockpitPlanGenerate(&json!({ "session_id": sid, "pane_id": 1, "mode": "blueprint", "item_id": "p1i1" }), &c).unwrap_err();
        assert!(err.contains("ai-call"));
    }

    #[test]
    fn ultra_and_breakdown_and_brainstorm_and_wizard_and_shapetree_and_decisions_reach_deferred_boundary() {
        let c = setup();
        let sid = a_session(&c);
        for (input, needs_pane) in [
            (json!({ "mode": "brainstorm", "ambition": "x" }), true),
            (json!({ "mode": "wizard", "ambition": "x" }), false),
            (json!({ "mode": "shapetree", "ambition": "x" }), false),
            (json!({ "mode": "decisions", "ambition": "x" }), false),
            (json!({ "mode": "breakdown", "ambition": "x" }), true),
            (json!({ "ambition": "x" }), true), // ultra (default mode)
        ] {
            let mut obj = input.as_object().unwrap().clone();
            obj.insert("session_id".into(), json!(sid));
            if needs_pane { obj.insert("pane_id".into(), json!(1)); }
            let err = cockpitPlanGenerate(&Value::Object(obj), &c).unwrap_err();
            assert!(err.contains("ai-call"), "mode failed to reach deferred boundary: {err}");
        }
    }

    // ── pure response-parsing helpers ───────────────────────────────────────────────────────
    #[test]
    fn parse_plan_happy_path_and_empty_error() {
        let ok = parse_plan(r#"{"phases":[{"title":"P1","items":[{"title":"do X","agent":"muse","size":"m","acceptance":["it works"]}]}]}"#).unwrap();
        assert_eq!(ok["phases"][0]["title"], json!("P1"));
        assert_eq!(ok["phases"][0]["items"][0]["title"], json!("do X"));
        assert_eq!(ok["phases"][0]["items"][0]["agent"], json!("muse"));
        assert_eq!(ok["phases"][0]["items"][0]["status"], json!("planned"));
        assert_eq!(parse_plan(r#"{"phases":[]}"#).unwrap_err(), "plan had no items");
        assert_eq!(parse_plan(r#"{"phases":[{"items":[{"title":""}]}]}"#).unwrap_err(), "plan had no items"); // blank titles dropped
    }

    #[test]
    fn parse_plan_defaults_bad_agent_size_and_ids() {
        let v = parse_plan(r#"{"phases":[{"items":[{"title":"t","agent":"bogus","size":"xl"}]}]}"#).unwrap();
        assert_eq!(v["phases"][0]["id"], json!("p1"));
        assert_eq!(v["phases"][0]["items"][0]["id"], json!("p1i1"));
        assert_eq!(v["phases"][0]["items"][0]["agent"], json!("forge"));
        assert_eq!(v["phases"][0]["items"][0]["size"], json!("s"));
    }

    #[test]
    fn shapetree_strict_and_salvage_parse() {
        let (branches, phases) = shapetree_strict_parse(r#"{"branches":[{"key":"code","title":"Code","nodes":[{"title":"t1","alts":[{"title":"A","why":"faster"}]}]}],"phases":[{"title":"Ph1","refs":["code:0"]}]}"#).unwrap();
        assert_eq!(branches.len(), 1);
        assert_eq!(branches[0]["key"], json!("code"));
        assert_eq!(phases.len(), 1);

        // truncated mid-array — strict parse fails, salvage recovers the complete branch
        let truncated = r#"{"branches":[{"key":"code","title":"Code","nodes":[{"title":"n1"}]},{"key":"ux","nodes":["#;
        assert!(shapetree_strict_parse(truncated).is_none());
        let salv = shapetree_salvage(truncated);
        assert_eq!(salv.len(), 1);
        assert_eq!(salv[0]["key"], json!("code"));
    }

    #[test]
    fn ideate_and_brainstorm_ideas_salvage_tiers() {
        assert_eq!(ideate_parse_ideas(r#"{"ideas":["a idea one","a idea two"]}"#), vec!["a idea one", "a idea two"]);
        // quoted-string salvage when JSON is truncated
        let salv = ideate_parse_ideas(r#"{"ideas":["a complete idea here""#);
        assert_eq!(salv, vec!["a complete idea here"]);
        // plain-prose fallback (no JSON, no quotes)
        let prose = brainstorm_parse_ideas("First idea sentence here. Second idea sentence too.");
        assert_eq!(prose, vec!["First idea sentence here.", "Second idea sentence too."]);
    }

    #[test]
    fn wizard_parse_prefers_salvage_when_strict_yields_too_few() {
        let full = wizard_parse(r#"{"dimensions":[{"key":"product","ideas":["i1 long enough","i2 long enough"]},{"key":"ux","ideas":["i3 long enough"]}]}"#);
        assert_eq!(full.len(), 2);
        let truncated = r#"{"dimensions":[{"key":"product","ideas":["idea one here","idea two here"]},{"key":"ux","ideas":["#;
        let salv = wizard_parse(truncated);
        assert_eq!(salv.len(), 1);
        assert_eq!(salv[0]["key"], json!("product"));
        assert_eq!(wizard_fallback_bucket("no json here \"just a quoted phrase over twelve chars\" more text").unwrap()["key"], json!("ideas"));
    }

    #[test]
    fn baseplan_and_decisions_parse() {
        let (md, decs) = baseplan_parse(r#"{"base_plan_md":"the plan","decisions":[{"q":"storage?","options":["sqlite","postgres"]}]}"#).unwrap();
        assert_eq!(md, "the plan");
        assert_eq!(decs.len(), 1);
        assert_eq!(baseplan_parse(r#"{}"#).unwrap_err(), "empty forge");
        let decs2 = decisions_parse(r#"{"decisions":[{"q":"ux?","options":["a","b"]},{"q":"bad","options":["only one"]}]}"#).unwrap();
        assert_eq!(decs2.len(), 1); // the single-option decision is dropped
        assert_eq!(decisions_parse(r#"{"decisions":[]}"#).unwrap_err(), "no valid decisions");
    }

    #[test]
    fn prepare_parse_validates_against_catalogs() {
        let kinds: HashSet<String> = ["forge", "muse"].iter().map(|s| s.to_string()).collect();
        let forms: HashSet<String> = ["squad"].iter().map(|s| s.to_string()).collect();
        let srcs: HashSet<String> = ["digest", "cards"].iter().map(|s| s.to_string()).collect();
        let v = prepare_parse(r#"{"mission":"go","shared":["digest","bogus"],"formation":"squad","panes":[{"kind":"forge","label":"Builder","agent":"forge","why":"builds","reads":["digest","bogus"]}]}"#, &kinds, &forms, &srcs).unwrap();
        assert_eq!(v["shared"], json!(["digest"]));
        assert_eq!(v["formation"], json!("squad"));
        assert_eq!(v["panes"][0]["reads"], json!(["digest"]));
        assert_eq!(v["panes"][0]["kind"], json!("forge"));
        assert_eq!(prepare_parse(r#"{"panes":[],"formation":null}"#, &kinds, &forms, &srcs).unwrap_err(), "empty crew");
    }

    #[test]
    fn blueprint_parse_shapes_files_and_symbols() {
        let v = blueprint_parse(r#"{"files":[{"path":"src/a.mjs","action":"create","symbols":[{"name":"foo","kind":"function","signature":"() => void","intent":"do a thing"}]}]}"#).unwrap();
        assert_eq!(v["files"][0]["path"], json!("src/a.mjs"));
        assert_eq!(v["files"][0]["action"], json!("create"));
        assert_eq!(v["files"][0]["symbols"][0]["name"], json!("foo"));
        assert_eq!(blueprint_parse(r#"{"files":[]}"#).unwrap_err(), "no files");
        assert_eq!(blueprint_parse(r#"{"files":[{"path":""}]}"#).unwrap_err(), "no files"); // blank paths dropped
    }

    #[test]
    fn filemap_parse_and_apply_assignments() {
        let mut tree = json!({ "branches": [ { "key": "code", "nodes": [ { "title": "n0" }, { "title": "n1", "files": ["src/existing.mjs"] } ] } ] });
        let assignments = filemap_parse_assignments(r#"{"assignments":[{"node":"code:0","files":["src/a.mjs","src/bogus.mjs"]},{"node":"code:1","files":["src/b.mjs"]},{"node":"missing:9","files":["x"]}]}"#);
        assert_eq!(assignments.len(), 3);
        let path_set: HashSet<String> = ["src/a.mjs", "src/b.mjs", "src/existing.mjs"].iter().map(|s| s.to_string()).collect();
        let (mapped, files_total) = apply_filemap_assignments(&mut tree, &assignments, &path_set);
        assert_eq!(mapped, 2); // the out-of-range/unknown-branch node contributes nothing
        assert_eq!(files_total, 2);
        assert_eq!(tree["branches"][0]["nodes"][0]["files"], json!(["src/a.mjs"])); // bogus path dropped
        assert_eq!(tree["branches"][0]["nodes"][1]["files"], json!(["src/existing.mjs", "src/b.mjs"])); // merged, de-duped

        // salvage path: truncated JSON, regex recovers complete assignment objects
        let salv = filemap_parse_assignments(r#"{"assignments":[{"node":"code:0","files":["src/a.mjs"]},{"node":"code:1","files":["#);
        assert_eq!(salv.len(), 1);
        assert_eq!(salv[0]["node"], json!("code:0"));
    }

    #[test]
    fn shared_context_text_defaults_and_joins() {
        let c = setup();
        let sid = a_session(&c);
        assert_eq!(shared_context_text(&c, sid, 3200), "(none yet)");
        c.execute("INSERT INTO cockpit_blackboard (session_id, kind, body_md) VALUES (?, 'context', 'doc one')", [sid]).unwrap();
        c.execute("INSERT INTO cockpit_blackboard (session_id, kind, body_md) VALUES (?, 'context', 'doc two')", [sid]).unwrap();
        let joined = shared_context_text(&c, sid, 3200);
        assert!(joined.contains("doc one") && joined.contains("doc two"));
    }

    #[test]
    fn resolve_selected_context_defaults_and_reads_mission() {
        let c = setup();
        let sid = a_session(&c);
        c.execute("UPDATE cockpit_sessions SET mission_md = 'Ship the thing' WHERE id = ?", [sid]).unwrap();
        let ctx = resolve_selected_context(&c, sid, &json!(["mission"]));
        assert!(ctx.contains("Ship the thing"));
        let empty = resolve_selected_context(&c, sid, &Value::Null); // defaults to mission/shared/digest
        assert!(!empty.is_empty());
    }
}
