//! blueprint-design.rs — twin of skills/blueprint/blueprint-design.mjs (db lane, cross-db
//! judgments.*/brain.*). NAMES ARE CONTRACT: default `blueprintDesign`; named
//! `ensureBlueprintPrompts`, `deriveBlocks`, `groundingBlock`.
//!
//! PARITY-DEFERRED (same pattern as interviewer-chat.rs / book-run.rs): `blueprintDesign` fans
//! out to `crate::ai_ai_call::aiCall` — the deferred network-AI stub (ai-call.rs's own doc) —
//! twice: once for the SKELETON call, once per file for the STUB call. `aiCall` always returns
//! `Err`, and the JS never wraps either call in try/catch, so the FIRST call (the skeleton) short-
//! circuits the whole function via `?`. Everything BEFORE it is real, ported, reachable code: the
//! `contract_slug` guard, the naming-realign call, the contract fetch, decided-names resolution,
//! files/acceptance/goal/must_obey assembly, `ensureBlueprintPrompts`, the three redesign-feedback
//! blocks (lint / design-court / snippet-court, each its own non-fatal try/catch), the skeleton
//! context-bundle grounding (+ its `groundingBlock` degrade path), and the risk-tag prevention
//! injection. All of that is exercised directly by this file's own tests.
//!
//! Past the first `aiCall`, the skeleton-JSON parse + fail-loud guard, the workspace path-snap,
//! the per-file STUB loop (its own `aiCall` + retry-on-reject + `acceptAuthoredCode` gate), the
//! stub-parse (`blueprintParse`), `deriveBlocks`, the lint/coverage gates, and the blueprint
//! persist/supersede are ported byte-for-byte from the JS but are UNREACHABLE by this file's own
//! fixtures, since the very first `aiCall` inside them already short-circuits via `?` — exactly
//! book-run.rs's precedent for this situation. Correctness there is by inspection + shape parity
//! with the reachable code above it. `deriveBlocks` itself (a pure function) is additionally
//! exercised standalone since it needs no `aiCall` to run.
//!
//! `context.provider_fn` (a JS function reference) has no carrier in this twin's `(input, db)`
//! signature — omitted from the `aiCall` input the same way book-run.rs / interviewer-chat.rs
//! drop it (the deferred `aiCall` boundary is where it would be consumed anyway).
//!
//! PARITY-NOTE: the JS re-queries `contracts.card_slug` independently inside each of the two
//! judgment-feedback try-blocks (`db.prepare('SELECT card_slug FROM contracts WHERE slug = ?')`).
//! Since this is a single synchronous execution over an already-fetched immutable row, this twin
//! reads `contract.card_slug` directly instead of re-querying — behaviorally identical, one fewer
//! round trip.
// QUALITY: mixed unit — real ported setup/grounding (fixture-tested) + PARITY-DEFERRED aiCall fan-out (by inspection)

use regex::Regex;
use rusqlite::Connection;
use serde_json::{json, Value};
use std::sync::{LazyLock, OnceLock};

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
}
fn tmpl(v: Option<&Value>) -> String {
    match v {
        None => "undefined".into(),
        Some(Value::Null) => "null".into(),
        Some(x) => str_qq(Some(x)),
    }
}

// ── prompt constants (verbatim from the JS module) ──────────────────────────────────────────
const SKELETON_PROMPT: &str = "blueprint.skeleton.v1";
const STUB_PROMPT: &str = "blueprint.stub.v1";

static OUTPUT_SKEL: LazyLock<Value> = LazyLock::new(|| json!([{ "field_name": "skeleton_json", "field_type": "string" }]));
static OUTPUT_STUB: LazyLock<Value> = LazyLock::new(|| json!([{ "field_name": "stub_source", "field_type": "string" }]));

const SKELETON_SHAPE: &str = r#"{"blocks":[{"path":"<exact file path>","intent":"<what this file does + why, 1-2 sentences>"}],"coverage":[{"acceptance_id":"<ac id>","maps_to_blocks":["<path>"]}]}"#;

const SKELETON_SYSTEM: &str = "You are the ARCHITECT of an autonomous software substrate. You draw the SPINE of a change before any code: which files (file blocks) are needed and which acceptance task each one satisfies. The draw of the house, not the house. You output only strict JSON — no nested blocks, no code.";

static SKELETON_TEMPLATE: LazyLock<String> = LazyLock::new(|| format!(
    "Draw the BLUEPRINT SKELETON (the spine) for this contract: the FILES needed + which acceptance task each satisfies. No code, no nested blocks yet — just the files and the coverage.\n\n\
=== THE CONTRACT ===\n\
title: {{{{contract_title}}}}\n\
goal: {{{{goal_md}}}}\n\
files: {{{{files_touched}}}}\n\
acceptance (S-tasks — each MUST map to >=1 file block): {{{{acceptance}}}}\n\
must_obey: {{{{must_obey}}}}\n\
out_of_scope: {{{{out_of_scope}}}}\n\n\
{{{{redesign_feedback}}}}\n\n\
{{{{codebase_context}}}}\n\n\
RULES:\n\
- DECIDE the files (file blocks) needed to satisfy the goal + acceptance, using paths + a\n\
  structure conventional for the project's stack (infer the stack from the goal — a React+Vite+TS\n\
  app uses src/*.tsx, not a .py file). If specific files were listed above, author EXACTLY those.\n\
  path = the exact file path; intent = what the file does + why (1-2 sentences).\n\
- COVERAGE: for EVERY acceptance S-task, list the file path(s) that satisfy it. Every file block\n\
  must be referenced by >=1 S-task (no orphan files).\n\n\
Put the skeleton JSON (nothing else — no prose, no fences) into skeleton_json, EXACTLY: {shape}",
    shape = SKELETON_SHAPE,
));

const STUB_SYSTEM: &str = "You are the ARCHITECT writing ONE file of a change as a CODE STUB — real, parseable code: the imports it needs + every function/class/type signature the file defines, each body left a TODO. You output ONLY the code stub — no prose, no fences, no JSON. Emit the code immediately — your very first character is code, never analysis.";

const STUB_TEMPLATE: &str = "Write the CODE STUB for ONE file of this change — REAL, PARSEABLE code in the file's language (infer it from the path: a .tsx file is TypeScript+JSX, a .py file is Python, …).\n\
{{retry_correction}}\n\n\
=== CONTRACT ===\n\
goal: {{goal_md}}\n\
must_obey: {{must_obey}}\n\n\
=== THIS FILE ===\n\
path: {{path}}\n\
role (intent): {{intent}}\n\
acceptance it must satisfy: {{block_acceptance}}\n\
sibling files in this change (for reference, do NOT write them): {{sibling_paths}}\n\
DECIDED NAMES (the naming authority already chose these — use them EXACTLY, never rename): {{decided_names}}\n\
NAMES ROLE: {{names_role}}\n\n\
{{codebase_context}}\n\n\
RULES:\n\
- Include the imports the file needs and a concrete top-level declaration (function / class / const)\n\
  for EVERYTHING this file must define — each with its FULL signature, parameters, and types — but\n\
  leave each BODY a stub: `throw new Error('TODO: <what this should do>')` (or the language's idiomatic\n\
  TODO). Signatures + structure ONLY — NO implementation logic.\n\
- BIND THE IDENTIFIERS — SINGLE OWNER: every name or value the contract/acceptance mandates —\n\
  storage keys, route paths, event names, labels, config values — is declared in EXACTLY ONE file.\n\
  If NAMES ROLE says OWNER, declare each decided name here as a REAL exported constant with its\n\
  LITERAL value (e.g. `export const STORAGE_KEY = 'app:click-count';`). If NAMES ROLE says\n\
  CONSUMER, IMPORT what you need from the owner file — NEVER re-declare a decided constant or\n\
  component in a second file (the review court REJECTS duplicate declarations as an\n\
  inconsistency risk). Naming is an ARCHITECTURE decision — the court also REJECTS a design\n\
  that leaves a mandated identifier unnamed.\n\
- The stub is PARSED into the architecture graph and the snippet author later fills each body within the\n\
  EXACT signature, so it MUST parse and the signatures must be the real ones the implementation will use.\n\n\
Your ENTIRE reply is the file's code. Output ONLY the raw code stub — no prose, no fences, no JSON, no field names.\n\
Your VERY FIRST character MUST be code (an import or a declaration). Do NOT analyze, explain, or deliberate first — a stub is small; a reply that reasons before the code runs out of space, gets truncated mid-expression, and fails the build.";

fn ensure_one(db: &Connection, name: &str, template_md: &str, default_tier: &str, system_md: &str, output_fields: &Value, maxout: i64) {
    let existing = crate::ai_prompt_find::promptFind(&json!({ "name": name }), db).ok();
    if let Some(e) = &existing {
        if e.get("template_md").and_then(|v| v.as_str()) == Some(template_md) {
            return;
        }
    }
    let _ = crate::ai_prompt_add::promptAdd(&json!({
        "name": name, "template_md": template_md, "system_md": system_md,
        "default_tier": default_tier, "default_temperature": "cold",
        "phase_flow": "single_only", "max_output_tokens": maxout, "output_fields": output_fields,
    }), db);
}

#[allow(non_snake_case)]
pub fn ensureBlueprintPrompts(db: &Connection) {
    ensure_one(db, SKELETON_PROMPT, &SKELETON_TEMPLATE, "mid", SKELETON_SYSTEM, &OUTPUT_SKEL, 4000);
    ensure_one(db, STUB_PROMPT, STUB_TEMPLATE, "low", STUB_SYSTEM, &OUTPUT_STUB, 5000);
}

// ── parseLoose / safeArr / pick ──────────────────────────────────────────────────────────────
fn parse_loose(s: Option<&str>) -> Option<Value> {
    let s = s?;
    static FENCE: OnceLock<Regex> = OnceLock::new();
    let fence_re = FENCE.get_or_init(|| Regex::new(r"(?is)```(?:json)?\s*(.*?)```").unwrap());
    static OBJ: OnceLock<Regex> = OnceLock::new();
    let obj_re = OBJ.get_or_init(|| Regex::new(r"(?s)\{.*\}").unwrap());

    let mut candidates: Vec<String> = Vec::new();
    if let Some(c) = fence_re.captures(s) {
        candidates.push(c.get(1).map(|m| m.as_str()).unwrap_or("").to_string());
    }
    candidates.push(s.to_string());
    if let Some(m) = obj_re.find(s) {
        candidates.push(m.as_str().to_string());
    }
    for c in candidates {
        if let Ok(v) = serde_json::from_str::<Value>(c.trim()) {
            return Some(v);
        }
    }
    None
}

fn safe_arr(s: Option<&Value>) -> Vec<Value> {
    let raw = match s {
        Some(Value::String(st)) => st.clone(),
        _ => "[]".to_string(),
    };
    match serde_json::from_str::<Value>(&raw) {
        Ok(Value::Array(a)) => a,
        _ => Vec::new(),
    }
}

fn pick(ai_result: &Value, field: &str) -> Option<String> {
    ai_result
        .get("output_kv")
        .and_then(|v| v.as_array())
        .and_then(|a| {
            a.iter().find(|o| {
                o.get("field_name").and_then(|v| v.as_str()) == Some(field)
                    && o.get("parent_output_id").map_or(true, |v| v.is_null())
            })
        })
        .and_then(|o| o.get("value_text"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

/// `export function deriveBlocks(skelFiles, plannedNodes)` — pure. DERIVEs the legacy block tree
/// from the parsed stub's planned nodes (docs/blueprint-ast-skeleton.md). A file with no parsed
/// function degrades to a single TODO `module` child.
#[allow(non_snake_case)]
pub fn deriveBlocks(skel_files: &[Value], planned_nodes: &[Value]) -> Vec<Value> {
    let mut fn_by_path: std::collections::HashMap<String, Vec<&Value>> = std::collections::HashMap::new();
    for n in planned_nodes {
        if n.is_null() || n.get("type").and_then(|v| v.as_str()) == Some("file") {
            continue;
        }
        let p = n.get("path").map(|v| str_qq(Some(v))).unwrap_or_default();
        fn_by_path.entry(p).or_default().push(n);
    }
    skel_files
        .iter()
        .map(|f| {
            let path = str_qq(f.get("path"));
            let intent = str_qq(f.get("intent"));
            let fns = fn_by_path.get(&path).cloned().unwrap_or_default();
            let mut children: Vec<Value> = fns
                .iter()
                .map(|n| {
                    let name = n.get("name").cloned().unwrap_or(Value::Null);
                    json!({
                        "id": format!("{path}.{}", str_qq(Some(&name))),
                        "type": n.get("type").filter(|v| !v.is_null()).cloned().unwrap_or(json!("function")),
                        "name": name,
                        "signature": n.get("signature").filter(|v| !v.is_null()).cloned().unwrap_or(json!("")),
                        "exported": truthy(n.get("exported")),
                        "intent": intent,
                        "children": [],
                    })
                })
                .collect();
            if children.is_empty() {
                children = vec![json!({ "id": format!("{path}.main"), "type": "module", "intent": intent, "children": [] })];
            }
            json!({ "id": path, "type": "file", "path": path, "intent": intent, "children": children })
        })
        .collect()
}

/// `function groundingBlock(db, paths)` — Wave G grounding via `codebaseContext`. Never throws;
/// empty when ungrounded.
#[allow(non_snake_case)]
pub fn groundingBlock(db: &Connection, paths: &[String]) -> String {
    let paths_json: Vec<Value> = paths.iter().map(|p| json!(p)).collect();
    match crate::codebase_codebase_context::codebaseContext(&json!({ "paths": paths_json, "hops": 1 }), db) {
        Ok(r) => {
            let grounded = r.get("grounded").and_then(|v| v.as_bool()).unwrap_or(false);
            let context_md = r.get("context_md").and_then(|v| v.as_str()).unwrap_or("");
            if !grounded || context_md.is_empty() {
                return String::new();
            }
            format!(
                "{context_md}\n\n⚠ The above is the REAL existing code in these file(s). DESIGN TO EXTEND IT — reuse what's there, match the patterns, and respect the blast radius (the listed importers depend on the current signatures). Do NOT re-design code that already exists."
            )
        }
        Err(_) => String::new(),
    }
}

fn norm_ident(s: &str) -> String {
    static R: OnceLock<Regex> = OnceLock::new();
    let re = R.get_or_init(|| Regex::new(r"[^a-z0-9]").unwrap());
    re.replace_all(&s.to_lowercase(), "").to_string()
}
fn ext_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"(?i)\.[a-z]+$").unwrap())
}
fn seg_of(p: &str) -> String {
    let rel = p.replace('\\', "/");
    let mut parts: Vec<&str> = rel.split('/').collect();
    let base_raw = parts.pop().unwrap_or("");
    let base = ext_re().replace(base_raw, "").to_string();
    let final_base = if base.to_lowercase() == "index" {
        parts.pop().map(|s| s.to_string()).unwrap_or(base)
    } else {
        base
    };
    norm_ident(&final_base)
}

#[allow(non_snake_case)]
pub fn blueprintDesign(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS: `if (!db) throw err.contextMissing('blueprint-design', 'db');` — db always present here.
    let slug_v = input.get("contract_slug");
    if !truthy(slug_v) {
        return Err("blueprint-design: contract_slug is required".into());
    }
    let slug = str_qq(slug_v);

    // NAMES REALIGN BEFORE EVERY DESIGN (meli r21) — additive, non-fatal.
    let _ = crate::system_naming_derive::namingDerive(&json!({ "contract_slug": slug, "op": "realign" }), db);

    let rows = crate::db_json::query_rows(
        db,
        "SELECT slug, title, card_slug, goal_md, files_touched_json, acceptance_json, must_obey_json, out_of_scope_json, naming_json FROM contracts WHERE slug = ?",
        &[json!(slug.clone())],
    )?;
    let contract = match rows.as_array().and_then(|a| a.first()) {
        Some(c) => c.clone(),
        None => return Err(format!("blueprint-design: contract '{slug}' not found")),
    };

    // FEAT-NAMING-AUTHORITY: the contract's DECIDED identifier set.
    let naming_parsed: Value = contract
        .get("naming_json")
        .and_then(|v| v.as_str())
        .and_then(|s| serde_json::from_str::<Value>(s).ok())
        .unwrap_or(json!({}));
    let decided_names = match &naming_parsed {
        Value::Object(m) if !m.is_empty() => serde_json::to_string(&naming_parsed).unwrap_or_else(|_| "(none decided)".to_string()),
        _ => "(none decided)".to_string(),
    };

    let files: Vec<String> = safe_arr(contract.get("files_touched_json"))
        .into_iter()
        .map(|v| str_qq(Some(&v)))
        .filter(|s| !s.is_empty())
        .collect();
    let acceptance: Vec<Value> = safe_arr(contract.get("acceptance_json"));
    let goal: String = {
        let goal_md = contract.get("goal_md").and_then(|v| v.as_str());
        let first_line = goal_md.map(|s| s.split('\n').next().unwrap_or("")).filter(|s| !s.is_empty());
        if let Some(fl) = first_line {
            fl.to_string()
        } else if let Some(t) = contract.get("title").and_then(|v| v.as_str()).filter(|s| !s.is_empty()) {
            t.to_string()
        } else {
            format!("deliver {slug}")
        }
    };
    let must_obey: Vec<Value> = safe_arr(contract.get("must_obey_json"));

    ensureBlueprintPrompts(db);

    let mut redesign_feedback = "(first design attempt)".to_string();
    // ── redesign feedback 1: last lint-failed blueprint for this contract ──
    if let Ok(rows) = crate::db_json::query_rows(
        db,
        "SELECT id, version, lint_json FROM blueprints WHERE contract_slug = ? AND lint_passed = 0 ORDER BY id DESC LIMIT 1",
        &[json!(slug.clone())],
    ) {
        if let Some(red) = rows.as_array().and_then(|a| a.first()) {
            if let Some(lj_str) = red.get("lint_json").and_then(|v| v.as_str()) {
                if !lj_str.is_empty() && lj_str != "{}" {
                    let mut hint = String::new();
                    if let Ok(lj) = serde_json::from_str::<Value>(lj_str) {
                        if lj.get("stub_parse").and_then(|v| v.as_array()).map_or(false, |a| !a.is_empty()) {
                            hint = "\nWHAT 'does not parse' USUALLY MEANS HERE: the file extension routes the parser — JSX belongs in .jsx/.tsx files. If a stub uses JSX (<Component />), give it a .jsx extension (and PREFER the path that ALREADY EXISTS in the workspace — never invent a sibling extension).".into();
                        }
                    }
                    let version = red.get("version").map(|v| tmpl(Some(v))).unwrap_or_else(|| "?".into());
                    let id = red.get("id").map(|v| tmpl(Some(v))).unwrap_or_default();
                    redesign_feedback = format!("=== YOUR PREVIOUS BLUEPRINT (v{version}, bp#{id}) FAILED THE LINT — FIX EXACTLY THESE ===\n{lj_str}{hint}");
                }
            }
        }
    }
    // ── redesign feedback 2: DESIGN COURT sustained defects (FIX-V2-REDESIGN-FEEDBACK) ──
    let card_slug = contract.get("card_slug").filter(|v| !v.is_null()).map(|v| str_qq(Some(v))).unwrap_or_else(|| slug.clone());
    if let Ok(rows) = crate::db_json::query_rows(
        db,
        "SELECT judgment_id, updated_at FROM judgments.judgment_cases WHERE card_slug = ? AND scope_kind = 'blueprint' AND scope_ref LIKE 'design-v%' AND verdict = 'fail' ORDER BY updated_at DESC, judgment_id DESC LIMIT 1",
        &[json!(card_slug.clone())],
    ) {
        if let Some(jc) = rows.as_array().and_then(|a| a.first()) {
            let judgment_id = jc.get("judgment_id").cloned().unwrap_or(Value::Null);
            if let Ok(sustained) = crate::db_json::query_rows(
                db,
                "SELECT statement, judge_why FROM judgments.judgment_items WHERE judgment_id = ? AND side = 'prosecutor' AND judge_value = 1",
                &[judgment_id],
            ) {
                let sustained_arr = sustained.as_array().cloned().unwrap_or_default();
                if !sustained_arr.is_empty() {
                    let lines: Vec<String> = sustained_arr
                        .iter()
                        .map(|s| {
                            let stmt = str_qq(s.get("statement"));
                            let why = s.get("judge_why").filter(|v| !v.is_null()).map(|v| format!(" ({})", str_qq(Some(v)))).unwrap_or_default();
                            format!("- {stmt}{why}")
                        })
                        .collect();
                    let updated_at = jc.get("updated_at").filter(|v| !v.is_null()).map(|v| str_qq(Some(v))).unwrap_or_else(|| "unknown".into());
                    let court_block = format!(
                        "=== YOUR PREVIOUS DESIGN WAS REJECTED BY THE REVIEW COURT (judged {updated_at}) — FIX EXACTLY THESE IN THIS DESIGN ===\n{}\nState the fix EXPLICITLY in the relevant file's intent (e.g. name the exact constant/key/value the contract demands).",
                        lines.join("\n")
                    );
                    redesign_feedback = if redesign_feedback == "(first design attempt)" {
                        court_block
                    } else {
                        format!("{redesign_feedback}\n\n{court_block}")
                    };
                }
            }
        }
    }
    // ── redesign feedback 3: CODE COURT sustained defects on snippets built from this design ──
    if let Ok(rows) = crate::db_json::query_rows(
        db,
        "SELECT judgment_id, updated_at FROM judgments.judgment_cases WHERE card_slug = ? AND scope_kind = 'blueprint' AND scope_ref LIKE 'snippets-v%' AND verdict = 'fail' ORDER BY updated_at DESC, judgment_id DESC LIMIT 1",
        &[json!(card_slug.clone())],
    ) {
        if let Some(sj) = rows.as_array().and_then(|a| a.first()) {
            let judgment_id = sj.get("judgment_id").cloned().unwrap_or(Value::Null);
            if let Ok(sustained) = crate::db_json::query_rows(
                db,
                "SELECT statement, judge_why FROM judgments.judgment_items WHERE judgment_id = ? AND side = 'prosecutor' AND judge_value = 1",
                &[judgment_id],
            ) {
                let sustained_arr = sustained.as_array().cloned().unwrap_or_default();
                if !sustained_arr.is_empty() {
                    let lines: Vec<String> = sustained_arr
                        .iter()
                        .map(|s| {
                            let stmt = str_qq(s.get("statement"));
                            let why = s.get("judge_why").filter(|v| !v.is_null()).map(|v| format!(" ({})", str_qq(Some(v)))).unwrap_or_default();
                            format!("- {stmt}{why}")
                        })
                        .collect();
                    let updated_at = sj.get("updated_at").filter(|v| !v.is_null()).map(|v| str_qq(Some(v))).unwrap_or_else(|| "unknown".into());
                    let code_block = format!(
                        "=== THE CODE COURT REJECTED SNIPPETS BUILT FROM YOUR PREVIOUS DESIGN (judged {updated_at}) — the defects originate IN the design; fix them AT THE DESIGN LEVEL ===\n{}\nName the exact constant/key/text the contract demands in the relevant file's intent — the author reproduces the design verbatim.",
                        lines.join("\n")
                    );
                    redesign_feedback = if redesign_feedback == "(first design attempt)" {
                        code_block
                    } else {
                        format!("{redesign_feedback}\n\n{code_block}")
                    };
                }
            }
        }
    }

    // ── 1. SKELETON grounding ──
    let mut skel_bundle: Option<Value> = None;
    let mut skel_grounding = String::new();
    let skel_task = format!("{goal}\n{}", serde_json::to_string(&acceptance).unwrap_or_default());
    let files_json: Vec<Value> = files.iter().map(|f| json!(f)).collect();
    match crate::codebase_context_bundle::contextBundle(
        &json!({ "agent": "tl", "phase": "design", "task": skel_task, "paths": files_json, "budget": 8000 }),
        db,
    ) {
        Ok(b) => {
            if let Some(bundle_md) = b.get("bundle_md").and_then(|v| v.as_str()).filter(|s| !s.is_empty()) {
                skel_grounding = format!("{bundle_md}\n\n⚠ The above is the REAL stack + existing code. DESIGN TO EXTEND IT — reuse what's there, match the patterns, respect the blast radius, and do NOT design against any framework, store, or library not listed above.");
            }
            skel_bundle = Some(b);
        }
        Err(_) => {}
    }
    if skel_grounding.is_empty() {
        skel_grounding = groundingBlock(db, &files);
    }
    // EPIC-FAILURE-TAGGING: known-risk prevention injection (additive, degrade-safe).
    if let Ok(rt) = crate::entities_risk_tag::riskTag(
        &json!({ "op": "match", "card_slug": contract.get("card_slug").cloned().unwrap_or(Value::Null), "files": files_json, "intent": goal }),
        db,
    ) {
        let hits: Vec<Value> = rt
            .get("matched")
            .and_then(|v| v.as_array())
            .map(|a| a.iter().filter(|t| truthy(t.get("prevention_md"))).cloned().collect())
            .unwrap_or_default();
        if !hits.is_empty() {
            let lines: Vec<String> = hits
                .iter()
                .map(|t| {
                    let slug_v = str_qq(t.get("slug"));
                    let retry = t
                        .get("retryability")
                        .filter(|v| truthy(Some(v)))
                        .map(|v| format!(" · {}", str_qq(Some(v))))
                        .unwrap_or_default();
                    format!("- [{slug_v}{retry}] {}", str_qq(t.get("prevention_md")))
                })
                .collect();
            let block = format!("=== KNOWN RISKS ON THIS SHAPE (learned from past failures — APPLY the prevention) ===\n{}\n\n", lines.join("\n"));
            skel_grounding = format!("{block}{skel_grounding}");
        }
    }

    // ── SKELETON dispatch — PARITY-DEFERRED from here: `aiCall` always returns Err; the JS never
    // wraps this call, so `?` propagates exactly as the un-caught JS throw would. ──
    let skel_res = crate::ai_ai_call::aiCall(
        &json!({
            "prompt_name": SKELETON_PROMPT,
            "input_kv": [
                { "key": "contract_title", "value": contract.get("title").and_then(|v| v.as_str()).filter(|s| !s.is_empty()).unwrap_or("(untitled)") },
                { "key": "goal_md", "value": goal },
                { "key": "files_touched", "value": if files.is_empty() { "(none pre-specified — DECIDE the files needed from the goal + acceptance)".to_string() } else { serde_json::to_string(&files).unwrap_or_default() } },
                { "key": "acceptance", "value": serde_json::to_string_pretty(&acceptance).unwrap_or_default() },
                { "key": "must_obey", "value": serde_json::to_string(&must_obey).unwrap_or_default() },
                { "key": "out_of_scope", "value": serde_json::to_string(&safe_arr(contract.get("out_of_scope_json"))).unwrap_or_default() },
                { "key": "redesign_feedback", "value": redesign_feedback },
                { "key": "codebase_context", "value": skel_grounding },
            ],
            "contract_slug": slug,
            "tier": "mid", "temperature": "cold", "phase_flow": "single_only",
        }),
        db,
    )?;

    // ── UNREACHABLE past this point (see module doc) — ported byte-for-byte, by inspection. ──
    if let Some(ai_call_id) = skel_res.get("ai_call_id").filter(|v| truthy(Some(v))) {
        if let Some(b) = &skel_bundle {
            let _ = crate::codebase_context_bundle_persist::contextBundlePersist(&json!({ "ai_call_id": ai_call_id, "bundle": b }), db);
        }
    }
    let skeleton_raw = pick(&skel_res, "skeleton_json");
    let mut skeleton = parse_loose(skeleton_raw.as_deref());
    let mut skel_files: Vec<Value> = skeleton
        .as_ref()
        .and_then(|s| s.get("blocks"))
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter(|b| truthy(b.get("path")))
                .map(|b| json!({ "path": b.get("path").cloned().unwrap_or(Value::Null), "intent": b.get("intent").cloned().unwrap_or(json!("")) }))
                .collect()
        })
        .unwrap_or_default();
    if skel_files.is_empty() {
        let raw = skeleton_raw.unwrap_or_default();
        let raw_trunc: String = raw.chars().take(160).collect();
        return Err(format!(
            "blueprint-design: SKELETON produced no file blocks — raw {} Truncated/malformed; check {}.",
            serde_json::to_string(&raw_trunc).unwrap_or_default(),
            SKELETON_PROMPT
        ));
    }

    // PATH SNAP-TO-WORKSPACE (meli r22) — additive, non-fatal.
    {
        let root_v = crate::engine_workspace_root::getWorkspaceRoot(db);
        let root = root_v.as_str().unwrap_or("").to_string();
        let mut remap: std::collections::HashMap<String, String> = std::collections::HashMap::new();
        for b in skel_files.iter_mut() {
            let rel = str_qq(b.get("path")).replace('\\', "/");
            let abs_path = std::path::Path::new(&root).join(&rel);
            if abs_path.exists() {
                continue;
            }
            let (dir, filename) = match rel.rfind('/') {
                Some(i) => (rel[..i].to_string(), rel[i + 1..].to_string()),
                None => (String::new(), rel.clone()),
            };
            let stem = ext_re().replace(&filename, "").to_string();
            let abs_dir = std::path::Path::new(&root).join(&dir);
            if !abs_dir.exists() {
                continue;
            }
            let sibling = std::fs::read_dir(&abs_dir).ok().and_then(|rd| {
                rd.filter_map(|e| e.ok())
                    .map(|e| e.file_name().to_string_lossy().to_string())
                    .find(|f| ext_re().replace(f, "").to_string() == stem && *f != filename)
            });
            if let Some(sib) = sibling {
                let new_path = format!("{}{}", if dir.is_empty() { String::new() } else { format!("{dir}/") }, sib);
                remap.insert(rel.clone(), new_path.clone());
                if let Some(obj) = b.as_object_mut() {
                    obj.insert("path".into(), json!(new_path));
                }
            }
        }
        if !remap.is_empty() {
            if let Some(cov_arr) = skeleton.as_mut().and_then(|s| s.get_mut("coverage")).and_then(|v| v.as_array_mut()) {
                for c in cov_arr.iter_mut() {
                    if let Some(maps) = c.get_mut("maps_to_blocks").and_then(|v| v.as_array_mut()) {
                        for p in maps.iter_mut() {
                            if let Some(np) = remap.get(&str_qq(Some(p))) {
                                *p = json!(np);
                            }
                        }
                    }
                }
            }
        }
    }

    let coverage: Vec<Value> = skeleton
        .as_ref()
        .and_then(|s| s.get("coverage"))
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .map(|c| json!({ "acceptance_id": c.get("acceptance_id").cloned().unwrap_or(Value::Null), "maps_to_blocks": c.get("maps_to_blocks").and_then(|v| v.as_array()).cloned().unwrap_or_default() }))
                .collect()
        })
        .unwrap_or_default();

    // ── 2. PER-FILE STUB — one LOW call per file. ──
    let all_paths: Vec<String> = skel_files.iter().map(|b| str_qq(b.get("path"))).collect();

    let names_owner_path: String = {
        let comp = norm_ident(&str_qq(naming_parsed.get("component")));
        let feat = norm_ident(&str_qq(naming_parsed.get("feature")));
        let hit = all_paths.iter().find(|p| {
            let seg = seg_of(p.as_str());
            if seg.is_empty() {
                return false;
            }
            (!comp.is_empty() && (comp.contains(&seg) || seg.contains(&comp))) || (!feat.is_empty() && (feat.contains(&seg) || seg.contains(&feat)))
        });
        hit.cloned().or_else(|| all_paths.first().cloned()).unwrap_or_default()
    };

    let mut source = "swarm".to_string();
    let mut stub_files: Vec<Value> = Vec::new();
    for f in &skel_files {
        let path = str_qq(f.get("path"));
        let intent = str_qq(f.get("intent"));
        let file_acc: Vec<Value> = acceptance
            .iter()
            .filter(|ac| {
                coverage.iter().any(|c| {
                    str_qq(c.get("acceptance_id")) == str_qq(ac.get("id"))
                        && c.get("maps_to_blocks").and_then(|v| v.as_array()).map_or(false, |a| a.iter().any(|p| str_qq(Some(p)) == path))
                })
            })
            .cloned()
            .collect();

        let mut file_bundle: Option<Value> = None;
        let mut file_grounding = String::new();
        let file_task = format!("{path}\n{intent}");
        match crate::codebase_context_bundle::contextBundle(&json!({ "agent": "tl", "phase": "author", "task": file_task, "paths": [path.clone()], "budget": 4000 }), db) {
            Ok(b) => {
                if let Some(md) = b.get("bundle_md").and_then(|v| v.as_str()).filter(|s| !s.is_empty()) {
                    file_grounding = format!("{md}\n\n⚠ The above is the REAL stack + existing code. Write the stub to EXTEND it — match the patterns, and do NOT import any framework, store, or library not listed above.");
                }
                file_bundle = Some(b);
            }
            Err(_) => {}
        }
        if file_grounding.is_empty() {
            file_grounding = groundingBlock(db, std::slice::from_ref(&path));
        }
        // r38 stuck-escalate: inline the REAL current content deterministically.
        {
            let ws_root_v = crate::engine_workspace_root::getWorkspaceRoot(db);
            let ws_root = ws_root_v.as_str().unwrap_or("");
            let abs = std::path::Path::new(ws_root).join(&path);
            if abs.exists() {
                if let Ok(src_full) = std::fs::read_to_string(&abs) {
                    let src: String = src_full.chars().take(6000).collect();
                    file_grounding = format!(
                        "=== CURRENT CONTENT OF {path} (this file EXISTS — your whole-file design REPLACES it; PRESERVE every existing export, import, route and component that must survive, and never re-declare what you keep) ===\n{src}\n\n{file_grounding}"
                    );
                }
            }
        }

        let names_role = if path == names_owner_path {
            "OWNER — this file DECLARES the decided names (exported consts with their literal values).".to_string()
        } else {
            format!("CONSUMER — the decided names are declared in {names_owner_path}; IMPORT them from there, never re-declare.")
        };
        let sibling_paths: Vec<String> = all_paths.iter().filter(|p| **p != path).cloned().collect();
        let block_acceptance: Vec<Value> = file_acc
            .iter()
            .map(|x| x.get("intent").filter(|v| truthy(Some(v))).cloned().unwrap_or_else(|| x.clone()))
            .filter(|v| truthy(Some(v)))
            .collect();

        let mk_kv = |retry_correction: &str, file_grounding: &str| {
            json!([
                { "key": "goal_md", "value": goal },
                { "key": "must_obey", "value": serde_json::to_string(&must_obey).unwrap_or_default() },
                { "key": "path", "value": path },
                { "key": "intent", "value": intent },
                { "key": "block_acceptance", "value": serde_json::to_string(&block_acceptance).unwrap_or_default() },
                { "key": "sibling_paths", "value": serde_json::to_string(&sibling_paths).unwrap_or_default() },
                { "key": "decided_names", "value": decided_names },
                { "key": "names_role", "value": names_role },
                { "key": "retry_correction", "value": retry_correction },
                { "key": "codebase_context", "value": file_grounding },
            ])
        };

        let mut stub_res = crate::ai_ai_call::aiCall(
            &json!({
                "prompt_name": STUB_PROMPT, "input_kv": mk_kv("", &file_grounding), "contract_slug": slug,
                "tier": "low", "temperature": "cold", "phase_flow": "single_only", "structured_output": false,
            }),
            db,
        )?;
        let stub_val = crate::blueprint__helpers_code_checks::extractRawCode(&json!(pick(&stub_res, "stub_source")));
        let mut stub_src = str_qq(Some(&stub_val));

        let accept = crate::blueprint__helpers_author_accept::acceptAuthoredCode(
            db,
            &json!({ "code": stub_src, "path": path, "ai_call_id": stub_res.get("ai_call_id").cloned().unwrap_or(Value::Null) }),
        );
        let reason = accept.get("reason").and_then(|v| v.as_str()).unwrap_or("");
        let degenerate = reason == "degenerate" || reason == "empty";
        if accept.get("ok").and_then(|v| v.as_bool()) != Some(true) {
            if let Some(id) = stub_res.get("ai_call_id").filter(|v| truthy(Some(v))) {
                let _ = crate::ai_ai_outcome_report::aiOutcomeReport(&json!({ "ai_call_id": id, "outcome_class": if degenerate { "degenerate" } else { "truncated" } }), db);
            }
            let echo: String = stub_src.trim().chars().take(40).collect();
            let correction = if degenerate {
                format!(
                    "⚠ YOUR PREVIOUS OUTPUT WAS INVALID — you returned only {} ({} chars). Output the COMPLETE parseable code stub for {path}: the imports + EVERY top-level declaration with its full signature, bodies as TODO. Never echo the field name, never wrap the code in quotes, never output a lone directive line.",
                    serde_json::to_string(&echo).unwrap_or_default(),
                    echo.chars().count()
                )
            } else {
                format!(
                    "⚠ YOUR PREVIOUS OUTPUT DID NOT PARSE — it was cut off (you deliberated too long before the code). Output the COMPLETE, parseable stub for {path} IMMEDIATELY — your VERY FIRST character MUST be code: the imports + EVERY top-level declaration with its signature, bodies as TODO. No analysis, no prose, no fences."
                )
            };
            stub_res = crate::ai_ai_call::aiCall(
                &json!({
                    "prompt_name": STUB_PROMPT, "input_kv": mk_kv(&correction, &file_grounding), "contract_slug": slug,
                    "tier": "mid", "temperature": "cold", "phase_flow": "single_only", "structured_output": false, "bypass_cache": true,
                }),
                db,
            )?;
            let retry_val = crate::blueprint__helpers_code_checks::extractRawCode(&json!(pick(&stub_res, "stub_source")));
            stub_src = str_qq(Some(&retry_val));
        }
        if let Some(id) = stub_res.get("ai_call_id").filter(|v| truthy(Some(v))) {
            if let Some(b) = &file_bundle {
                let _ = crate::codebase_context_bundle_persist::contextBundlePersist(&json!({ "ai_call_id": id, "bundle": b }), db);
            }
        }
        if !stub_src.trim().is_empty() {
            stub_files.push(json!({ "path": path, "source": stub_src }));
        } else {
            source = "swarm-degraded".to_string();
        }
    }

    // Parse the stubs → the atlas-native planned graph + delta.
    let mut skeleton_plan: Value = json!({ "stub_files": stub_files.clone() });
    if !stub_files.is_empty() {
        match crate::blueprint_blueprint_parse::blueprintParse(&json!({ "stub_files": stub_files.clone() }), db) {
            Ok(p) => {
                let errors = p.get("errors").cloned().unwrap_or(json!([]));
                let ok = errors.as_array().map_or(true, |a| a.is_empty());
                skeleton_plan = json!({
                    "stub_files": stub_files.clone(),
                    "planned_nodes": p.get("nodes").cloned().unwrap_or(json!([])),
                    "planned_edges": p.get("edges").cloned().unwrap_or(json!([])),
                    "delta": p.get("delta").cloned().unwrap_or(Value::Null),
                    "stub_parse_ok": ok,
                    "parse_errors": errors,
                });
            }
            Err(e) => {
                skeleton_plan = json!({ "stub_files": stub_files.clone(), "stub_parse_ok": false, "parse_error": e });
            }
        }
    }

    let planned_nodes: Vec<Value> = skeleton_plan.get("planned_nodes").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    let blocks = deriveBlocks(&skel_files, &planned_nodes);
    let mut blueprint = json!({ "blocks": blocks, "bindings": [], "coverage": coverage, "contract_slug": slug });
    if let (Some(obj), Value::Object(plan_obj)) = (blueprint.as_object_mut(), &skeleton_plan) {
        for (k, v) in plan_obj {
            obj.insert(k.clone(), v.clone());
        }
    }

    // ── 3. Deterministic gates + persist. ──
    let lint = crate::blueprint_blueprint_lint::blueprintLint(&json!({ "blueprint": blueprint, "files_touched": files }), db)?;
    let acceptance_with_maps: Vec<Value> = acceptance
        .iter()
        .map(|ac| {
            let cov = coverage.iter().find(|c| str_qq(c.get("acceptance_id")) == str_qq(ac.get("id")));
            let maps = cov.and_then(|c| c.get("maps_to_blocks")).and_then(|v| v.as_array()).cloned().unwrap_or_default();
            let mut o = ac.as_object().cloned().unwrap_or_default();
            o.insert("maps_to_blocks".into(), json!(maps));
            Value::Object(o)
        })
        .collect();
    let cov = crate::blueprint_blueprint_coverage::blueprintCoverage(&json!({ "acceptance": acceptance_with_maps, "blocks": blocks }), db)?;

    let stub_parse_ok_true = skeleton_plan.get("stub_parse_ok").and_then(|v| v.as_bool()) == Some(true);
    let stub_gate = stub_files.is_empty() || stub_parse_ok_true;
    let lint_passed = lint.get("passed").and_then(|v| v.as_bool()).unwrap_or(false);
    let cov_passed = cov.get("passed").and_then(|v| v.as_bool()).unwrap_or(false);
    let passed: i64 = if lint_passed && cov_passed && stub_gate { 1 } else { 0 };

    let prev_ver: i64 = crate::db_json::query_rows(db, "SELECT MAX(version) AS v FROM blueprints WHERE contract_slug = ?", &[json!(slug.clone())])?
        .as_array()
        .and_then(|a| a.first())
        .and_then(|r| r.get("v"))
        .and_then(|v| v.as_i64())
        .unwrap_or(0);

    let mut lint_json_obj = serde_json::Map::new();
    lint_json_obj.insert("source".into(), json!(source));
    lint_json_obj.insert("lint".into(), lint.get("issues").cloned().unwrap_or(json!([])));
    lint_json_obj.insert("coverage".into(), cov.get("issues").cloned().unwrap_or(json!([])));
    if !stub_files.is_empty() && !stub_gate {
        let stub_parse = skeleton_plan
            .get("parse_errors")
            .cloned()
            .unwrap_or_else(|| skeleton_plan.get("parse_error").map(|e| json!([{ "error": e }])).unwrap_or(json!([])));
        lint_json_obj.insert("stub_parse".into(), stub_parse);
    }

    let created_by = input.get("created_by").filter(|v| truthy(Some(v))).cloned().unwrap_or(json!("tl:auto"));
    let (_changes, blueprint_id) = crate::db_json::run_write(
        db,
        "INSERT INTO blueprints (contract_slug, version, blueprint_json, lint_passed, lint_json, created_by) VALUES (?, ?, ?, ?, ?, ?)",
        &[
            json!(slug.clone()),
            json!(prev_ver + 1),
            json!(serde_json::to_string(&blueprint).unwrap_or_default()),
            json!(passed),
            json!(serde_json::to_string(&Value::Object(lint_json_obj)).unwrap_or_default()),
            created_by,
        ],
    )?;
    crate::db_json::run_write(
        db,
        "UPDATE blueprints SET superseded_by = ? WHERE contract_slug = ? AND superseded_by IS NULL AND id != ?",
        &[json!(blueprint_id), json!(slug.clone()), json!(blueprint_id)],
    )?;

    if passed == 1 {
        let mut seen = std::collections::HashSet::new();
        let mut paths_out: Vec<String> = Vec::new();
        for b in &blocks {
            if let Some(p) = b.get("path").filter(|v| truthy(Some(v))) {
                let ps = str_qq(Some(p));
                if seen.insert(ps.clone()) {
                    paths_out.push(ps);
                }
            }
        }
        if !paths_out.is_empty() {
            crate::db_json::run_write(
                db,
                &format!("UPDATE contracts SET files_touched_json = ?, updated_at = {} WHERE slug = ?", crate::clock::now_sql()),
                &[json!(serde_json::to_string(&paths_out).unwrap_or_default()), json!(slug.clone())],
            )?;
        }
    }

    Ok(json!({
        "contract_slug": slug,
        "blueprint_id": blueprint_id,
        "version": prev_ver + 1,
        "source": source,
        "block_count": lint.get("block_count").cloned().unwrap_or(json!(0)),
        "nested_count": lint.get("nested_count").cloned().unwrap_or(json!(0)),
        "lint": lint,
        "coverage": cov,
        "passed": passed == 1,
        "stub_parse_ok": if !stub_files.is_empty() { json!(stub_parse_ok_true) } else { Value::Null },
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "ATTACH ':memory:' AS brain; ATTACH ':memory:' AS judgments; ATTACH ':memory:' AS memory;
             CREATE TABLE contracts (id INTEGER PRIMARY KEY AUTOINCREMENT, slug TEXT UNIQUE, title TEXT, card_slug TEXT, goal_md TEXT, files_touched_json TEXT, acceptance_json TEXT, must_obey_json TEXT, out_of_scope_json TEXT, naming_json TEXT, updated_at TEXT);
             CREATE TABLE blueprints (id INTEGER PRIMARY KEY AUTOINCREMENT, contract_slug TEXT NOT NULL, version INTEGER, blueprint_json TEXT, lint_passed INTEGER, lint_json TEXT, created_by TEXT, superseded_by INTEGER);
             CREATE TABLE workspaces (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL UNIQUE, root_path TEXT NOT NULL, vcs TEXT NOT NULL DEFAULT 'git', is_active INTEGER NOT NULL DEFAULT 0, created_at TEXT NOT NULL DEFAULT '2026-01-01 00:00:00');
             CREATE TABLE code_nodes (id TEXT PRIMARY KEY, origin TEXT, path TEXT, type TEXT, name TEXT, signature TEXT, intent TEXT, parent_id TEXT);
             CREATE TABLE code_edges (id INTEGER PRIMARY KEY AUTOINCREMENT, origin TEXT, from_node TEXT, to_node TEXT, type TEXT, kind TEXT);
             CREATE TABLE code_decorators (origin TEXT, path TEXT, name TEXT, kind TEXT, signature TEXT, summary TEXT, line INTEGER);
             CREATE TABLE code_entry_points (id INTEGER PRIMARY KEY AUTOINCREMENT, origin TEXT, kind TEXT, subtype TEXT, path TEXT, label TEXT, line INTEGER, framework TEXT, confidence REAL DEFAULT 0.7);
             CREATE TABLE code_data_stories (id INTEGER PRIMARY KEY, origin TEXT, name TEXT, description TEXT, hops_json TEXT);
             CREATE TABLE code_digests (id INTEGER PRIMARY KEY, origin TEXT, narrative_md TEXT);
             CREATE TABLE code_descriptions (origin TEXT, path TEXT, summary_md TEXT);
             CREATE TABLE tags (slug TEXT PRIMARY KEY, kind TEXT, description TEXT, applies_to_json TEXT, prevention_md TEXT, retryability TEXT, source_pattern_key TEXT, fix_commit TEXT, canary_test TEXT, weight INTEGER, status TEXT);
             CREATE TABLE memory.memories (id INTEGER PRIMARY KEY, type TEXT, title TEXT, guidance TEXT, validated INTEGER, superseded_by INTEGER);
             CREATE TABLE judgments.judgment_cases (judgment_id TEXT PRIMARY KEY, card_slug TEXT NOT NULL, scope_kind TEXT NOT NULL, scope_ref TEXT NOT NULL DEFAULT '', verdict TEXT, updated_at TEXT);
             CREATE TABLE judgments.judgment_items (id INTEGER PRIMARY KEY AUTOINCREMENT, judgment_id TEXT NOT NULL, side TEXT NOT NULL, item_id TEXT NOT NULL, statement TEXT NOT NULL DEFAULT '', judge_value INTEGER, judge_why TEXT);
             CREATE TABLE brain.prompt_templates ( id INTEGER PRIMARY KEY, name TEXT NOT NULL, version INTEGER NOT NULL, template_md TEXT NOT NULL, default_tier TEXT NOT NULL, default_temperature TEXT NOT NULL, max_output_tokens INTEGER NOT NULL DEFAULT 1024, superseded_by INTEGER, created_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')), cross_field_rules_json TEXT, judging_criteria_json TEXT, system_md TEXT, phase_flow TEXT NOT NULL DEFAULT 'single', UNIQUE (name, version) );
             CREATE TABLE brain.prompt_output_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, grounded_refs_json TEXT, UNIQUE (prompt_template_id, field_name) );
             CREATE TABLE brain.prompt_input_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, UNIQUE (prompt_template_id, field_name) );
             CREATE TABLE brain.ai_calls (id INTEGER PRIMARY KEY, prompt_template_id INTEGER, tokens_out INTEGER, outcome_class TEXT, cached INTEGER DEFAULT 0);",
        )
        .unwrap();
        c
    }

    // ── pure helpers ─────────────────────────────────────────────────────────
    #[test]
    fn parse_loose_extracts_fenced_and_bare_json() {
        assert_eq!(parse_loose(Some("```json\n{\"a\":1}\n```")), Some(json!({"a":1})));
        assert_eq!(parse_loose(Some("noise {\"a\":1} noise")), Some(json!({"a":1})));
        assert_eq!(parse_loose(Some("not json at all")), None);
        assert_eq!(parse_loose(None), None);
    }

    #[test]
    fn safe_arr_parses_or_falls_back_empty() {
        assert_eq!(safe_arr(Some(&json!("[1,2,3]"))), vec![json!(1), json!(2), json!(3)]);
        assert_eq!(safe_arr(Some(&json!("not json"))), Vec::<Value>::new());
        assert_eq!(safe_arr(None), Vec::<Value>::new());
        assert_eq!(safe_arr(Some(&json!("{\"a\":1}"))), Vec::<Value>::new()); // object, not array
    }

    #[test]
    fn pick_finds_top_level_field_only() {
        let ai_result = json!({ "output_kv": [
            { "field_name": "skeleton_json", "value_text": "{}", "parent_output_id": null },
            { "field_name": "skeleton_json", "value_text": "NESTED", "parent_output_id": 5 },
        ]});
        assert_eq!(pick(&ai_result, "skeleton_json"), Some("{}".to_string()));
        assert_eq!(pick(&ai_result, "missing"), None);
    }

    #[test]
    fn derive_blocks_falls_back_to_todo_module_when_no_functions_parsed() {
        let skel_files = vec![json!({ "path": "src/a.ts", "intent": "does a" })];
        let blocks = deriveBlocks(&skel_files, &[]);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0]["type"], json!("file"));
        assert_eq!(blocks[0]["children"][0]["id"], json!("src/a.ts.main"));
        assert_eq!(blocks[0]["children"][0]["type"], json!("module"));
    }

    #[test]
    fn derive_blocks_projects_planned_function_nodes_as_children() {
        let skel_files = vec![json!({ "path": "src/a.ts", "intent": "does a" })];
        let planned = vec![
            json!({ "type": "file", "path": "src/a.ts" }), // excluded — file node
            json!({ "type": "function", "path": "src/a.ts", "name": "doA", "signature": "function doA()", "exported": true }),
        ];
        let blocks = deriveBlocks(&skel_files, &planned);
        assert_eq!(blocks[0]["children"].as_array().unwrap().len(), 1);
        assert_eq!(blocks[0]["children"][0]["id"], json!("src/a.ts.doA"));
        assert_eq!(blocks[0]["children"][0]["exported"], json!(true));
        assert_eq!(blocks[0]["children"][0]["signature"], json!("function doA()"));
    }

    #[test]
    fn grounding_block_degrades_to_empty_when_atlas_ungrounded() {
        let c = setup();
        assert_eq!(groundingBlock(&c, &["src/missing.ts".to_string()]), "");
    }

    // ── ensureBlueprintPrompts ───────────────────────────────────────────────
    #[test]
    fn ensure_blueprint_prompts_seeds_both_prompts_once() {
        let c = setup();
        ensureBlueprintPrompts(&c);
        let n: i64 = c.query_row("SELECT COUNT(*) FROM brain.prompt_templates", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 2);
        // idempotent — same template_md → no re-insert/version bump.
        ensureBlueprintPrompts(&c);
        let n2: i64 = c.query_row("SELECT COUNT(*) FROM brain.prompt_templates", [], |r| r.get(0)).unwrap();
        assert_eq!(n2, 2);
    }

    // ── blueprintDesign guards ───────────────────────────────────────────────
    #[test]
    fn requires_contract_slug() {
        let c = setup();
        let err = blueprintDesign(&json!({}), &c).unwrap_err();
        assert_eq!(err, "blueprint-design: contract_slug is required");
    }

    #[test]
    fn contract_not_found() {
        let c = setup();
        let err = blueprintDesign(&json!({ "contract_slug": "CT-MISSING" }), &c).unwrap_err();
        assert_eq!(err, "blueprint-design: contract 'CT-MISSING' not found");
    }

    #[test]
    fn production_path_reaches_the_parity_deferred_aicall_boundary() {
        // Proves every deterministic gate ran cleanly (naming realign, contract fetch, decided
        // names, files/acceptance/goal assembly, ensureBlueprintPrompts, the three redesign-
        // feedback try-blocks, skeleton grounding, risk-tag injection, skeleton input_kv
        // assembly) and the error is aiCall's deferred stub, not an earlier bug.
        let c = setup();
        c.execute(
            "INSERT INTO contracts (slug, title, card_slug, goal_md, files_touched_json, acceptance_json, must_obey_json, out_of_scope_json, naming_json) VALUES ('CT-1', 'Add click counter', 'CARD-1', 'add a click counter\\nextra', '[\"src/App.jsx\"]', '[{\"id\":\"AC1\",\"intent\":\"clicking increments\"}]', '[]', '[]', '{}')",
            [],
        )
        .unwrap();
        let err = blueprintDesign(&json!({ "contract_slug": "CT-1" }), &c).unwrap_err();
        assert!(err.to_lowercase().contains("deferred") || err.contains("ai-call"), "unexpected error: {err}");
        // ensureBlueprintPrompts ran for real (both prompts seeded) before the deferred boundary.
        let n: i64 = c.query_row("SELECT COUNT(*) FROM brain.prompt_templates WHERE name IN (?, ?)", rusqlite::params![SKELETON_PROMPT, STUB_PROMPT], |r| r.get(0)).unwrap();
        assert_eq!(n, 2);
    }

    #[test]
    fn goal_falls_back_to_title_then_slug_when_goal_md_absent() {
        let c = setup();
        c.execute("INSERT INTO contracts (slug, title, card_slug) VALUES ('CT-2', 'My Title', 'CARD-2')", []).unwrap();
        let err = blueprintDesign(&json!({ "contract_slug": "CT-2" }), &c).unwrap_err();
        // still reaches the aiCall boundary — goal/title fallback didn't panic or early-error.
        assert!(err.contains("ai-call"), "unexpected error: {err}");
    }

    #[test]
    fn redesign_feedback_lint_block_is_built_from_prior_failed_blueprint() {
        let c = setup();
        c.execute("INSERT INTO contracts (slug, title, card_slug) VALUES ('CT-3', 'T', 'CARD-3')", []).unwrap();
        c.execute(
            "INSERT INTO blueprints (contract_slug, version, lint_passed, lint_json) VALUES ('CT-3', 1, 0, '{\"stub_parse\":[{\"error\":\"boom\"}]}')",
            [],
        )
        .unwrap();
        // Reaching the aiCall boundary (not an earlier panic) proves the lint-feedback branch,
        // including the stub_parse JSX hint, assembled without error.
        let err = blueprintDesign(&json!({ "contract_slug": "CT-3" }), &c).unwrap_err();
        assert!(err.contains("ai-call"), "unexpected error: {err}");
    }
}
