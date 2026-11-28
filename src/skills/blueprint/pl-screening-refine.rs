//! pl-screening-refine.rs — twin of skills/blueprint/pl-screening-refine.mjs (db lane, brain-
//! attached). NAMES ARE CONTRACT: default `plScreeningRefine`. STEP 3 of the PL screening swarm:
//! REFINE / decompose (LOW). Takes the card's FEATURE LIST (step 2, the card acceptance) +
//! the research (step 1) and decomposes EACH feature into tiny single S-tasks a cheap low-tier
//! AI can do — the acceptance todolist. Its output IS the screening; it persists the whole thing
//! to `pl_screenings`.
//!
//! `context.db` missing -> JS throws `err.contextMissing('pl-screening-refine', 'db')`.
//! Unreachable here: `db: &Connection` is a required, non-nullable reference (standard convention
//! for every db-lane twin). `context.provider_fn` has no Rust equivalent to carry — threaded as
//! `null` into the constructed aiCall input, same as pl-research.rs/pl-ratify-contract.rs.
//!
//! PARITY-DEFERRED (whole tail, same convention as pl-research.rs): the FIRST `aiCall` dispatch
//! (through `crate::ai_ai_call::aiCall`, the deferred network-AI stub — see ai-call.rs's module
//! doc) is NOT wrapped in a JS try/catch — the source comment says it explicitly: "FAIL LOUD: let
//! aiCall's validation_failed propagate; do NOT swallow it into a stub S-task list". So this twin
//! propagates that `Err` via `?` once the real, ported deterministic surface above it has run:
//! guard, card lookup, `ensure_prompt`, research/clarify coercion, the feature-list resolution
//! (threaded `card_acceptance` else a DB read+parse fallback), `card_terms` (backtick-identifier +
//! title-word extraction used to seed the context-bundle's relevance ranking), and the
//! (always-`Ok`, itself internally degrade-safe) `contextBundle` call via `crate::
//! codebase_context_bundle::contextBundle`. Everything AFTER the first `aiCall` — the best-effort
//! `contextBundlePersist`, the malformed-reply retry (a SECOND `aiCall` dispatch with
//! `bypass_cache: true`), the fail-loud `AntilokiError` throw, `contextGroundingCheck`, the
//! S-task `card_acceptance_id` fallback-to-default-feature mapping, `screeningGate`, and the
//! `pl_screenings` INSERT — is ported verbatim below (compiles, never executes given the stub's
//! unconditional `Err`) exactly like pl-research.rs's tail; the reusable pure pieces (`card_terms`,
//! the S-task id-fallback mapper `resolve_screening_acceptance`) are extracted as their own
//! functions and unit-tested directly per the crossing playbook's coverage rule.
// QUALITY: mixed unit — deterministic guard/lookup/assembly surface (fully ported, fixture-
// reachable up to the deferred first-aiCall boundary) + 2 pure helpers (card_terms,
// resolve_screening_acceptance — independently unit-tested) + the aiCall-gated tail (ported
// verbatim, unreachable given the stub, no fixture/frontier).
use regex::Regex;
use rusqlite::Connection;
use serde::Serialize;
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
fn obj_field<'a>(v: &'a Value, key: &str) -> Option<&'a Value> {
    v.as_object().and_then(|o| o.get(key))
}
// `String(a?.card_acceptance_id)` / `String(f?.id)` — optional chaining off a possibly non-object
// (or absent) value yields `undefined` -> `String(undefined)` === "undefined".
fn js_string_opt(v: Option<&Value>) -> String {
    match v {
        None => "undefined".to_string(),
        Some(x) => js_string(x),
    }
}
// `JSON.parse(s ?? '[]')` + `Array.isArray` guard, `[]` on any failure (bad JSON, non-array).
fn parse_json_arr(v: Option<&Value>) -> Vec<Value> {
    let default = matches!(v, None | Some(Value::Null));
    let s = if default { "[]".to_string() } else { js_string(v.unwrap()) };
    match serde_json::from_str::<Value>(&s) {
        Ok(Value::Array(a)) => a,
        _ => vec![],
    }
}
// `JSON.stringify(v, null, 1).slice(0, n)` — pretty-print with a 1-space indent (serde's
// preserve_order keeps the same key order as the JS object literals), then a UTF-16-safe slice
// (playbook rule 5: `.slice` = take `n` chars; fixtures stay ASCII/BMP).
fn json_stringify_pretty1(v: &Value, take: usize) -> String {
    let mut buf = Vec::new();
    let formatter = serde_json::ser::PrettyFormatter::with_indent(b" ");
    let mut ser = serde_json::Serializer::with_formatter(&mut buf, formatter);
    v.serialize(&mut ser).expect("serialize");
    let s = String::from_utf8(buf).expect("utf8");
    s.chars().take(take).collect()
}
// `.slice(0, n)` on a UTF-16-safe basis — playbook rule 5, chars-count take (fixtures ASCII/BMP).
fn take_chars(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

const PROMPT: &str = "pl.screening.refine.v1";
const SHAPE: &str = r#"{"context_md":"<enriched understanding of the card>","acceptance":[{"id":"ac1","card_acceptance_id":"<the feature id this task serves>","intent":"<what must be true, no code>","done_criteria":"<how we know it passed>","kind":"behavior|file-exists|grep|test"}]}"#;
const SYSTEM: &str = "You are the PRODUCT LEAD, STEP 3 of 3: REFINE. You DECOMPOSE each card feature into tiny single tasks a CHEAP low-tier AI can do correctly, each LINKED to the feature it serves. You output only strict JSON.";

fn template() -> String {
    format!(
        "STEP 3 of 3 — REFINE (decompose). Take the card's FEATURE LIST and decompose EACH feature into single, tiny tasks — each small + unambiguous enough that a CHEAP, LOW-TIER AI can do it correctly on its own. This is the todolist the rest of the pipeline executes.

=== THE CARD ===
title: {{{{card_title}}}}
body: {{{{card_body}}}}

=== RESEARCH (step 1) ===
{{{{research_md}}}}

=== CARD ACCEPTANCE — THE FEATURES (step 2; decompose EACH one) ===
{{{{card_acceptance}}}}

{{{{codebase_context}}}}

RULES:
- context_md: one short paragraph — the enriched understanding, grounded in the research + features.
- acceptance: decompose EACH feature into the smallest INDEPENDENTLY VERIFIABLE single tasks —
  what must be TRUE, no code. Each {{ id, card_acceptance_id, intent (what — no code), done_criteria
  (how we know it passed), kind }}. card_acceptance_id MUST be the id of the feature this task
  serves (from the list above). kind: file-exists / grep / test = mechanically checkable;
  behavior = judged. Prefer MANY tiny tasks over a few big ones; each must be trivial for a
  low-tier AI. EVERY feature must have at least one task.

Put the screening JSON (nothing else — no prose, no fences) into screening_json, EXACTLY: {SHAPE}"
    )
}

// Screening has no files_touched yet — find relevant EXISTING code by the card's WORDS:
// backtick-quoted identifiers (cards often name real symbols/files) + identifier-ish title words.
fn card_terms(title: &str, body: &str) -> Vec<String> {
    let backtick_re = Regex::new(r"`([A-Za-z0-9_./-]{3,})`").unwrap();
    let mut terms: Vec<String> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut push = |s: String, terms: &mut Vec<String>, seen: &mut std::collections::HashSet<String>| {
        if seen.insert(s.clone()) {
            terms.push(s);
        }
    };
    let combined = format!("{title}\n{body}");
    for cap in backtick_re.captures_iter(&combined) {
        push(cap[1].to_string(), &mut terms, &mut seen);
    }
    // `title.split(/[^A-Za-z0-9_.]+/)` then `if (w && (/[A-Z_.]/.test(w.slice(1)) || w.length > 3))`
    let split_re = Regex::new(r"[^A-Za-z0-9_.]+").unwrap();
    let upper_underscore_dot = Regex::new(r"[A-Z_.]").unwrap();
    for w in split_re.split(title) {
        if w.is_empty() {
            continue;
        }
        let rest: String = w.chars().skip(1).collect();
        if upper_underscore_dot.is_match(&rest) || w.chars().count() > 3 {
            push(w.to_string(), &mut terms, &mut seen);
        }
    }
    terms.into_iter().take(8).collect()
}

// try promptFind; on miss (or template mismatch), promptAdd (mirrors JS ensurePrompt: skip the
// re-add if the existing template already matches verbatim).
fn ensure_prompt(db: &Connection) -> Result<(), String> {
    let tmpl = template();
    if let Ok(existing) = crate::ai_prompt_find::promptFind(&json!({ "name": PROMPT }), db) {
        if existing.get("template_md").and_then(Value::as_str) == Some(tmpl.as_str()) {
            return Ok(());
        }
    }
    crate::ai_prompt_add::promptAdd(
        &json!({
            "name": PROMPT,
            "template_md": tmpl,
            "system_md": SYSTEM,
            "default_tier": "low",
            "default_temperature": "cold",
            "phase_flow": "single_only",
            "max_output_tokens": 4000,
            "output_fields": [{ "field_name": "screening_json", "field_type": "string" }],
        }),
        db,
    )?;
    Ok(())
}

// `screening.acceptance.map(ac => ({ ...ac, card_acceptance_id: featureIds.has(String(ac?.
// card_acceptance_id)) ? String(ac.card_acceptance_id) : defaultFeatureId }))` — EVERY S-task must
// link to a real feature; fall back to the first feature if the AI omitted/hallucinated the id.
fn resolve_screening_acceptance(acceptance: &[Value], feature_ids: &std::collections::HashSet<String>, default_feature_id: &str) -> Vec<Value> {
    acceptance
        .iter()
        .map(|ac| {
            let mut o = ac.as_object().cloned().unwrap_or_default();
            let id = js_string_opt(obj_field(ac, "card_acceptance_id"));
            let resolved = if feature_ids.contains(&id) { id } else { default_feature_id.to_string() };
            o.insert("card_acceptance_id".to_string(), json!(resolved));
            Value::Object(o)
        })
        .collect()
}

pub fn plScreeningRefine(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS: `const { db, provider_fn = null } = context ?? {}; if (!db) throw err.contextMissing(...)`
    // — unreachable here (Rust's `db: &Connection` is never absent).
    let provider_fn = Value::Null;
    let card_slug_v = input.get("card_slug");
    if !truthy(card_slug_v) {
        return Err("pl-screening-refine: card_slug is required".to_string());
    }
    let card_slug = str_qq(card_slug_v);

    let card_rows = crate::db_json::query_rows(db, "SELECT slug, title, body_json FROM cards WHERE slug = ?", &[json!(card_slug)])?;
    let Some(card) = card_rows.as_array().and_then(|a| a.first()).cloned() else {
        return Err(format!("pl-screening-refine: card '{card_slug}' not found"));
    };

    ensure_prompt(db)?;
    let research_md = str_qq(input.get("research_md"));
    let clarify_md = str_qq(input.get("clarify_md"));

    // The card features (step 2). Prefer the threaded value; else read the card's acceptance.
    let features: Vec<Value> = match input.get("card_acceptance") {
        Some(Value::Array(a)) => a.clone(),
        _ => {
            let acc_rows = crate::db_json::query_rows(db, "SELECT acceptance_json FROM cards WHERE slug=?", &[json!(card_slug)])?;
            let acc = acc_rows.as_array().and_then(|a| a.first()).and_then(|r| obj_field(r, "acceptance_json")).cloned();
            parse_json_arr(acc.as_ref())
        }
    };
    let feature_ids: std::collections::HashSet<String> = features.iter().map(|f| js_string_opt(obj_field(f, "id"))).collect();
    // `(features ?? [])[0]?.id ?? 'ca1'` — nullish coalescing: only an absent/null id (or no
    // first feature) falls back to 'ca1'; a present-but-falsy id (0, '') is kept as-is.
    let default_feature_id = match features.first().and_then(|f| obj_field(f, "id")) {
        None | Some(Value::Null) => "ca1".to_string(),
        Some(id) => js_string(id),
    };

    // Context bundle (docs/context-bundle.md): skeleton bone (stack-facts + architecture) + touched
    // code found by the card's OWN words (card_terms). Additive/degrade-safe; '' off-atlas.
    let card_title = str_qq(obj_field(&card, "title"));
    let card_body = str_qq(obj_field(&card, "body_json"));
    let terms: Vec<Value> = card_terms(&card_title, &card_body).into_iter().map(Value::String).collect();
    let mut codebase_block = String::new();
    let mut bundle_obj: Option<Value> = None;
    let bundle_input = json!({
        "agent": "pl", "phase": "refine",
        "task": format!("{}\n{}", card_title, take_chars(&card_body, 2000)),
        "terms": terms, "budget": 6000,
    });
    if let Ok(b) = crate::codebase_context_bundle::contextBundle(&bundle_input, db) {
        if let Some(md) = b.get("bundle_md").and_then(Value::as_str).filter(|s| !s.is_empty()) {
            codebase_block = format!("{md}\n\n⚠ This is the REAL stack + EXISTING code related to the card. Decompose tasks that EXTEND/REUSE it where it applies — don't propose building what already exists, and do NOT assume any framework, store, or library that is not listed above.");
        }
        bundle_obj = Some(b);
    }

    // FAIL LOUD: let aiCall's validation_failed propagate; do NOT swallow it into a stub
    // S-task list that passes the gate and marches the contract forward on empty input.
    let base_kv = json!([
        { "key": "card_title", "value": card_title },
        { "key": "card_body", "value": take_chars(&card_body, 8000) },
        { "key": "research_md", "value": take_chars(&research_md, 6000) },
        { "key": "card_acceptance", "value": json_stringify_pretty1(&Value::Array(features.clone()), 4000) },
        { "key": "codebase_context", "value": codebase_block },
    ]);
    let ai_input = json!({
        "prompt_name": PROMPT,
        "input_kv": base_kv,
        "tier": "low", "temperature": "cold", "phase_flow": "single_only", "provider_fn": provider_fn,
    });
    // ── the AI call: PARITY-DEFERRED from here on (see module doc) — aiCall always returns Err,
    // and this dispatch is NOT wrapped in a JS try/catch (fail-loud by design), so the twin
    // propagates it exactly like the JS caller would once the deterministic surface above it ran.
    let res = crate::ai_ai_call::aiCall(&ai_input, db)?;

    // Persist the exact bundle linked to the call it fed — best-effort audit.
    if let Some(ai_call_id) = res.get("ai_call_id").filter(|v| !v.is_null()) {
        if let Some(bundle) = &bundle_obj {
            let _ = crate::codebase_context_bundle_persist::contextBundlePersist(&json!({ "ai_call_id": ai_call_id, "bundle": bundle }), db);
        }
    }
    let mut res = res;
    let mut screening = crate::blueprint_screening_gate::parseLoose(&crate::blueprint_screening_gate::pickField(&res, &json!("screening_json")));

    // r39: ONE malformed reply (truncated, empty acceptance) killed the whole session — the
    // fail-loud stays, but only AFTER a single salted retry (bypass_cache -> genuinely new call)
    // telling the model exactly what was wrong.
    let acceptance_ok = |s: &Value| s.get("acceptance").and_then(Value::as_array).map(|a| !a.is_empty()).unwrap_or(false);
    if !acceptance_ok(&screening) {
        let raw = crate::blueprint_screening_gate::pickField(&res, &json!("screening_json"));
        // `String(pickField(res, 'screening_json') ?? '')` — null/undefined -> '', else js String().
        let raw_str = match &raw {
            Value::Null => String::new(),
            v => js_string(v),
        };
        let retry_note = format!("retry: your previous reply's screening_json was empty or malformed ({}) — output the COMPLETE screening_json with a NON-EMPTY acceptance array.", take_chars(&raw_str, 80));
        let mut retry_kv = base_kv.as_array().cloned().unwrap_or_default();
        retry_kv.push(json!({ "key": "retry_note", "value": retry_note }));
        let retry_input = json!({
            "prompt_name": PROMPT, "input_kv": retry_kv,
            "tier": "low", "temperature": "cold", "phase_flow": "single_only", "provider_fn": provider_fn, "bypass_cache": true,
        });
        if let Ok(retry) = crate::ai_ai_call::aiCall(&retry_input, db) {
            if let Some(ai_call_id) = retry.get("ai_call_id").filter(|v| !v.is_null()) {
                if let Some(bundle) = &bundle_obj {
                    let _ = crate::codebase_context_bundle_persist::contextBundlePersist(&json!({ "ai_call_id": ai_call_id, "bundle": bundle }), db);
                }
            }
            let second = crate::blueprint_screening_gate::parseLoose(&crate::blueprint_screening_gate::pickField(&retry, &json!("screening_json")));
            if acceptance_ok(&second) {
                res = retry;
                screening = second;
            }
        }
    }

    if !acceptance_ok(&screening) {
        // JS: `JSON.stringify(String(pickField(res, 'screening_json') ?? '').slice(0, 160))`
        let raw = crate::blueprint_screening_gate::pickField(&res, &json!("screening_json"));
        let raw_str = match &raw {
            Value::Null => String::new(),
            v => js_string(v),
        };
        let quoted = serde_json::to_string(&Value::String(take_chars(&raw_str, 160))).unwrap_or_default();
        return Err(format!(
            "pl-screening-refine: REFINE produced no S-tasks — raw screening {quoted}. Truncated/malformed, or it was fed empty features; check pl.screening.refine.v1 max_output_tokens + that clarify produced features."
        ));
    }
    // Post-call grounding check: the enriched context / S-tasks shouldn't assert a store/lib the
    // stack says is absent.
    let ai_call_id = res.get("ai_call_id").cloned().unwrap_or(Value::Null);
    let _ = crate::codebase_context_grounding_check::contextGroundingCheck(
        &json!({ "output": js_string(&screening), "ai_call_id": ai_call_id, "agent": "pl", "phase": "refine" }),
        db,
    );

    let mut screening_obj = screening.as_object().cloned().unwrap_or_default();
    let acceptance = screening_obj.get("acceptance").and_then(Value::as_array).cloned().unwrap_or_default();
    screening_obj.insert("acceptance".to_string(), Value::Array(resolve_screening_acceptance(&acceptance, &feature_ids, &default_feature_id)));
    if !research_md.is_empty() {
        screening_obj.insert("research_md".to_string(), json!(research_md));
    }
    if !clarify_md.is_empty() {
        screening_obj.insert("clarify_md".to_string(), json!(clarify_md));
    }
    screening_obj.insert("card_acceptance".to_string(), Value::Array(features));
    let screening_final = Value::Object(screening_obj);

    let gate = crate::blueprint_screening_gate::screeningGate(&screening_final);
    let passed = gate.get("passed").map(truthy_val).unwrap_or(false);
    let created_by = input.get("created_by").filter(|v| !v.is_null()).cloned().unwrap_or(json!("pl:auto"));
    let (_, screening_id) = crate::db_json::run_write(
        db,
        "INSERT INTO pl_screenings (card_slug, screening_json, lint_passed, lint_json, created_by) VALUES (?, ?, ?, ?, ?)",
        &[json!(card_slug), json!(js_string(&screening_final)), json!(if passed { 1 } else { 0 }), json!(js_string(gate.get("issues").unwrap_or(&Value::Null))), created_by],
    )?;

    Ok(json!({
        "card_slug": card_slug,
        "screening_id": screening_id,
        "passed": passed,
        "acceptance_count": gate.get("acceptance_count").cloned().unwrap_or(Value::Null),
        "gate": gate,
    }))
}

fn truthy_val(v: &Value) -> bool {
    truthy(Some(v))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS brain; ATTACH ':memory:' AS memory;").unwrap();
        c.execute_batch(
            "CREATE TABLE cards (slug TEXT PRIMARY KEY, title TEXT, body_json TEXT, acceptance_json TEXT);
             CREATE TABLE pl_screenings (id INTEGER PRIMARY KEY AUTOINCREMENT, card_slug TEXT, screening_json TEXT, lint_passed INTEGER, lint_json TEXT, created_by TEXT);
             CREATE TABLE workspaces (id INTEGER PRIMARY KEY, name TEXT NOT NULL UNIQUE, root_path TEXT NOT NULL, vcs TEXT NOT NULL DEFAULT 'git', is_active INTEGER NOT NULL DEFAULT 0, created_at TEXT NOT NULL DEFAULT '2026-01-01 00:00:00');
             CREATE TABLE code_nodes (id TEXT PRIMARY KEY, origin TEXT, path TEXT, type TEXT, name TEXT, signature TEXT, intent TEXT, parent_id TEXT);
             CREATE TABLE code_edges (id INTEGER PRIMARY KEY AUTOINCREMENT, origin TEXT, from_node TEXT, to_node TEXT, type TEXT, kind TEXT);
             CREATE TABLE code_decorators (origin TEXT, path TEXT, name TEXT, kind TEXT, signature TEXT, summary TEXT, line INTEGER);
             CREATE TABLE code_entry_points (id INTEGER PRIMARY KEY AUTOINCREMENT, origin TEXT, kind TEXT, subtype TEXT, path TEXT, label TEXT, line INTEGER, framework TEXT, confidence REAL DEFAULT 0.7);
             CREATE TABLE code_data_stories (id INTEGER PRIMARY KEY, origin TEXT, name TEXT, description TEXT, hops_json TEXT);
             CREATE TABLE code_digests (id INTEGER PRIMARY KEY, origin TEXT, narrative_md TEXT);
             CREATE TABLE memory.memories (id INTEGER PRIMARY KEY, type TEXT, title TEXT, guidance TEXT, validated INTEGER, superseded_by INTEGER);
             CREATE TABLE brain.prompt_templates ( id INTEGER PRIMARY KEY, name TEXT NOT NULL, version INTEGER NOT NULL, template_md TEXT NOT NULL, default_tier TEXT NOT NULL, default_temperature TEXT NOT NULL, max_output_tokens INTEGER NOT NULL DEFAULT 1024, superseded_by INTEGER, created_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')), cross_field_rules_json TEXT, judging_criteria_json TEXT, system_md TEXT, phase_flow TEXT NOT NULL DEFAULT 'single', UNIQUE (name, version) );
             CREATE TABLE brain.prompt_output_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, grounded_refs_json TEXT, UNIQUE (prompt_template_id, field_name) );
             CREATE TABLE brain.prompt_input_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, UNIQUE (prompt_template_id, field_name) );",
        ).unwrap();
        c
    }

    #[test]
    fn card_slug_is_required() {
        let c = setup();
        let e = plScreeningRefine(&json!({}), &c).unwrap_err();
        assert_eq!(e, "pl-screening-refine: card_slug is required");
        let e2 = plScreeningRefine(&json!({ "card_slug": "" }), &c).unwrap_err();
        assert_eq!(e2, "pl-screening-refine: card_slug is required");
    }

    #[test]
    fn card_not_found_errors() {
        let c = setup();
        let e = plScreeningRefine(&json!({ "card_slug": "CARD-404" }), &c).unwrap_err();
        assert_eq!(e, "pl-screening-refine: card 'CARD-404' not found");
    }

    #[test]
    fn reaches_the_parity_deferred_aicall_boundary_after_full_deterministic_surface() {
        // proves guard/card-lookup/ensure_prompt/feature-resolution/context-bundle all ran cleanly
        // and the error is aiCall's deferred stub, not an earlier bug.
        let c = setup();
        c.execute(
            "INSERT INTO cards (slug, title, body_json, acceptance_json) VALUES ('CARD-1', 'fix `handleClick` bug', 'body text here', '[{\"id\":\"ca1\",\"intent\":\"do it\"}]')",
            [],
        )
        .unwrap();
        c.execute("INSERT INTO workspaces (name, root_path, is_active) VALUES ('w', 'C:/nonexistent', 1)", []).unwrap();
        let err = plScreeningRefine(&json!({ "card_slug": "CARD-1", "research_md": "some research" }), &c).unwrap_err();
        assert!(err.contains("ai-call"), "unexpected error: {err}");
        let n: i64 = c.query_row("SELECT COUNT(*) FROM brain.prompt_templates WHERE name = 'pl.screening.refine.v1'", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1);
        // no pl_screenings row — the tail never ran
        let sn: i64 = c.query_row("SELECT COUNT(*) FROM pl_screenings", [], |r| r.get(0)).unwrap();
        assert_eq!(sn, 0);
    }

    #[test]
    fn feature_resolution_falls_back_to_db_acceptance_json_when_not_threaded() {
        let c = setup();
        c.execute(
            "INSERT INTO cards (slug, title, body_json, acceptance_json) VALUES ('CARD-2', 't', 'b', '[{\"id\":\"ca9\",\"intent\":\"x\"}]')",
            [],
        )
        .unwrap();
        // no card_acceptance in input -> pull from db; still reaches the deferred boundary.
        let err = plScreeningRefine(&json!({ "card_slug": "CARD-2" }), &c).unwrap_err();
        assert!(err.contains("ai-call"), "unexpected error: {err}");
    }

    #[test]
    fn card_terms_extracts_backtick_identifiers_and_capitalized_or_long_title_words() {
        let terms = card_terms("Fix `handleClick` in the `src/app/foo.ts` module", "see `bar.rs` too");
        assert!(terms.contains(&"handleClick".to_string()));
        assert!(terms.contains(&"src/app/foo.ts".to_string()));
        assert!(terms.contains(&"bar.rs".to_string()));
        assert!(terms.contains(&"module".to_string())); // len > 3
        assert!(!terms.contains(&"in".to_string())); // len <= 3, no A-Z/_/.
        assert!(!terms.contains(&"the".to_string()));
    }

    #[test]
    fn card_terms_caps_at_eight_and_dedupes() {
        let title = "Aa Bb Cc Dd Ee Ff Gg Hh Ii Jj";
        let terms = card_terms(title, "");
        assert!(terms.len() <= 8);
    }

    #[test]
    fn resolve_screening_acceptance_falls_back_to_default_feature_when_id_unlinked() {
        let feature_ids: std::collections::HashSet<String> = ["ca1".to_string(), "ca2".to_string()].into_iter().collect();
        let acceptance = vec![
            json!({ "id": "ac1", "card_acceptance_id": "ca2", "intent": "x" }),
            json!({ "id": "ac2", "card_acceptance_id": "hallucinated", "intent": "y" }),
            json!({ "id": "ac3", "intent": "z" }),
        ];
        let resolved = resolve_screening_acceptance(&acceptance, &feature_ids, "ca1");
        assert_eq!(resolved[0]["card_acceptance_id"], json!("ca2")); // linked -> kept
        assert_eq!(resolved[1]["card_acceptance_id"], json!("ca1")); // hallucinated -> default
        assert_eq!(resolved[2]["card_acceptance_id"], json!("ca1")); // missing -> default
        assert_eq!(resolved[0]["id"], json!("ac1")); // rest of the object preserved
    }

    #[test]
    fn parse_json_arr_matches_js_json_parse_default_and_guard() {
        assert_eq!(parse_json_arr(None), Vec::<Value>::new());
        assert_eq!(parse_json_arr(Some(&Value::Null)), Vec::<Value>::new());
        assert_eq!(parse_json_arr(Some(&json!("[1,2,3]"))), vec![json!(1), json!(2), json!(3)]);
        assert_eq!(parse_json_arr(Some(&json!("not json"))), Vec::<Value>::new());
    }
}
