//! pl-screening-clarify.rs — twin of skills/blueprint/pl-screening-clarify.mjs (db lane). NAMES
//! ARE CONTRACT: default `plScreeningClarify`. STEP 2 of the PL screening swarm: CLARIFY (MID).
//! Using the research (step 1), clarifies the goal into a LIST OF FEATURES — each simple and
//! specific, a SEPARATE item — persisted as the card's structured acceptance (`cards.
//! acceptance_json`) so the contract's S-tasks (step 3 / refine) can link back to each feature.
//!
//! `context.db` missing → JS throws `err.contextMissing('pl-screening-clarify', 'db')`.
//! Unreachable here: `db: &Connection` is a required, non-nullable reference (same convention as
//! every sibling db-lane twin). `context.provider_fn` has no Rust equivalent to carry — threaded
//! as `null` into the constructed aiCall input, same as pl-research.rs/pl-ratify-contract.rs.
//!
//! PARITY-DEFERRED (partial): `contextBundle` (`crate::codebase_context_bundle::contextBundle`) is
//! itself fully deterministic (its own deferred edges — `ragRecall`/`smartContextBundle` — fold to
//! `None`/degrade internally per that twin's own doc) and is called here plainly, wrapped in a
//! best-effort catch mirroring the JS `try { bundleObj = await contextBundle(...) } catch {
//! code_context = '' }`. The AI call is dispatched through `crate::ai_ai_call::aiCall`, the
//! deferred network-AI stub (see ai-call.rs's module doc) — it always returns `Err`, and UNLIKE
//! pl-ratify-contract.rs's swallowed AI call, the JS here has NO try/catch around `await aiCall`
//! ("FAIL LOUD" per the source comment) — so this twin propagates that `Err` via `?` once the
//! entire deterministic surface above it (guard, card lookup, `ensure_prompt`, the context-bundle
//! assembly) has run, same pattern as pl-research.rs/book-run.rs. The response-parsing
//! (`parse_array`) and feature-shaping (`build_features`) helpers, plus the no-valid-features error
//! path, are unreachable from the default export (the `?` on `aiCall` short-circuits before they
//! run) but ported as their own PURE functions and unit-tested directly per the crossing
//! playbook's coverage rule (same pattern as pl-research.rs's `build_research_report`).
// QUALITY: mixed unit — deterministic guard/lookup/ensure_prompt/context-bundle surface (fully
// ported, fixture-reachable up to the deferred aiCall boundary) + 2 pure response-shaping helpers
// (unreachable from the default export, unit-tested directly) + 1 bespoke/deferred aiCall dispatch
// (no fixture/frontier).
use regex::Regex;
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
fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}
// `.slice(0, n)` on a JS string = take `n` chars (playbook rule 5; fixtures stay ASCII/BMP).
fn take_chars(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

const PROMPT: &str = "pl.screening.clarify.v2";
const SHAPE: &str = r#"[{"id":"ca1","intent":"<a simple, specific feature — one coherent unit of user-visible value, no code>"}]"#;
const SYSTEM: &str = "You are the PRODUCT LEAD, STEP 2 of 3: CLARIFY. Using the research, you clarify the goal into a LIST OF FEATURES — each simple, specific, a separate item. You decide, never defer. You output only a strict JSON array.";

fn template() -> String {
    format!(
        "STEP 2 of 3 — CLARIFY. Using the research, clarify the goal into a LIST OF FEATURES, each one SIMPLE and SPECIFIC, as a SEPARATE item.

=== THE CARD ===
title: {{{{card_title}}}}
body: {{{{card_body}}}}

=== RESEARCH (step 1) ===
{{{{research_md}}}}

=== STACK & CONVENTIONS (the real project — ground features here; do NOT assume absent tech) ===
{{{{code_context}}}}

Resolve EVERY ambiguity by DECIDING (never ask, never defer). Each feature is the smallest
coherent unit of user-visible value, stated concretely so it's unmistakable what is and isn't
included. No code, and NO task breakdown yet (that is step 3).

Output a JSON ARRAY of features (nothing else — no prose, no fences) into features_json,
EXACTLY: {SHAPE}"
    )
}

// try promptFind; on miss (or template drift), promptAdd. Mirrors JS ensurePrompt: skip the
// re-add if the existing template already matches verbatim.
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
            "default_tier": "mid",
            "default_temperature": "cold",
            "phase_flow": "single_only",
            "max_output_tokens": 4000,
            "output_fields": [{ "field_name": "features_json", "field_type": "string" }],
        }),
        db,
    )?;
    Ok(())
}

// `String(raw ?? '').replace(/```(?:json)?/gi, '').replace(/```/g, '').trim()`, then tries the
// whole string, falling back to a `/\[[\s\S]*\]/` greedy bracket match — `JSON.parse` + `Array.
// isArray` guard on each candidate, `null` if neither parses to an array.
fn parse_array(raw: Option<&Value>) -> Option<Vec<Value>> {
    let s = str_qq(raw);
    let fence_json = Regex::new(r"(?i)```(?:json)?").unwrap();
    let fence_bare = Regex::new(r"```").unwrap();
    let step1 = fence_json.replace_all(&s, "");
    let step2 = fence_bare.replace_all(&step1, "");
    let trimmed = step2.trim().to_string();

    let bracket_re = Regex::new(r"(?s)\[.*\]").unwrap();
    let bracket = bracket_re.find(&trimmed).map(|m| m.as_str().to_string());

    for candidate in [Some(trimmed.clone()), bracket] {
        let Some(c) = candidate else { continue };
        if c.is_empty() {
            continue;
        }
        if let Ok(Value::Array(v)) = serde_json::from_str::<Value>(&c) {
            return Some(v);
        }
    }
    None
}

// `(parseArray(raw) ?? []).map((f, i) => ({ id: String(f?.id ?? \`ca${i+1}\`), intent:
// String((typeof f === 'string' ? f : f?.intent) ?? '').trim() })).filter(f => f.intent)`
fn build_features(arr: Vec<Value>) -> Vec<Value> {
    arr.iter()
        .enumerate()
        .filter_map(|(i, f)| {
            let id = match obj_field(f, "id") {
                Some(v) if !v.is_null() => js_string(v),
                _ => format!("ca{}", i + 1),
            };
            let intent_src: Option<&Value> = if matches!(f, Value::String(_)) { Some(f) } else { obj_field(f, "intent") };
            let intent = match intent_src {
                Some(v) if !v.is_null() => js_string(v),
                _ => String::new(),
            };
            let intent = intent.trim().to_string();
            if intent.is_empty() {
                None
            } else {
                Some(json!({ "id": id, "intent": intent }))
            }
        })
        .collect()
}

pub fn plScreeningClarify(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS: `const { db, provider_fn = null } = context ?? {}; if (!db) throw err.contextMissing(...)`
    // — unreachable here (Rust's `db: &Connection` is never absent).
    let provider_fn = Value::Null;
    let card_slug_v = input.get("card_slug");
    if !truthy(card_slug_v) {
        return Err("pl-screening-clarify: card_slug is required".to_string());
    }
    let card_slug = str_qq(card_slug_v);

    let card_rows = crate::db_json::query_rows(db, "SELECT slug, title, body_json FROM cards WHERE slug = ?", &[json!(card_slug)])?;
    let Some(card) = card_rows.as_array().and_then(|a| a.first()).cloned() else {
        return Err(format!("pl-screening-clarify: card '{card_slug}' not found"));
    };

    ensure_prompt(db)?;

    // Facts bone (docs/context-bundle.md, pl:clarify contract): the stack / conventions /
    // invariants, so features are clarified against the REAL project — don't propose a feature
    // that assumes absent tech. Never empty (feed-quality gate); persisted for the Context tab.
    let task = format!("{}\n{}", tmpl_str(obj_field(&card, "title")), take_chars(&str_qq(obj_field(&card, "body_json")), 2000));
    let bundle_input = json!({ "agent": "pl", "phase": "clarify", "task": task, "budget": 4000 });
    let (bundle_obj, mut code_context) = match crate::codebase_context_bundle::contextBundle(&bundle_input, db) {
        Ok(b) => {
            let cc = b.get("bundle_md").and_then(Value::as_str).unwrap_or("").to_string();
            (Some(b), cc)
        }
        Err(_) => (None, String::new()),
    };
    if code_context.trim().is_empty() {
        code_context = "(no codebase context yet — greenfield; do NOT assume any framework, store, or library exists)".to_string();
    }

    // FAIL LOUD: let aiCall's validation_failed (a truncated/malformed response) PROPAGATE. The
    // old code swallowed it (catch → null) and fell back to a one-feature title stub, so the
    // whole screening marched forward on garbage — the exact loop the operator hit.
    let ai_input = json!({
        "prompt_name": PROMPT,
        "input_kv": [
            { "key": "card_title", "value": obj_field(&card, "title").cloned().unwrap_or(json!("")) },
            { "key": "card_body", "value": take_chars(&str_qq(obj_field(&card, "body_json")), 8000) },
            { "key": "code_context", "value": take_chars(&code_context, 4000) },
            { "key": "research_md", "value": take_chars(&str_qq(input.get("research_md")), 6000) },
        ],
        "tier": "mid",
        "temperature": "cold",
        "phase_flow": "single_only",
        "provider_fn": provider_fn,
    });
    let res = crate::ai_ai_call::aiCall(&ai_input, db)?;

    if let (Some(ai_call_id), Some(bundle)) = (res.get("ai_call_id").filter(|v| !v.is_null()), bundle_obj.as_ref()) {
        // JS: `try { contextBundlePersist(...) } catch { /* audit is additive */ }` — this twin
        // never errors (best-effort no-op on any bad input), so the catch is unreachable here too.
        let _ = crate::codebase_context_bundle_persist::contextBundlePersist(&json!({ "ai_call_id": ai_call_id.clone(), "bundle": bundle }), db);
    }
    let raw = crate::blueprint_screening_gate::pickField(&res, &json!("features_json"));

    let features = build_features(parse_array(Some(&raw)).unwrap_or_default());
    if features.is_empty() {
        return Err(format!(
            "pl-screening-clarify: CLARIFY produced no valid features — raw output {:?}. Likely truncated or malformed; check pl.screening.clarify.v2 max_output_tokens or the model.",
            take_chars(&str_qq(Some(&raw)), 160)
        ));
    }

    // The card's acceptance IS this feature list (a structured array on the card).
    crate::db_json::run_write(
        db,
        &format!("UPDATE cards SET acceptance_json = ?, updated_at = {} WHERE slug = ?", crate::clock::now_sql()),
        &[json!(serde_json::to_string(&features).map_err(|e| e.to_string())?), json!(card_slug)],
    )?;

    let clarify_md = features
        .iter()
        .enumerate()
        .map(|(i, f)| format!("{}. [{}] {}", i + 1, tmpl_str(obj_field(f, "id")), tmpl_str(obj_field(f, "intent"))))
        .collect::<Vec<_>>()
        .join("\n");

    Ok(json!({ "card_slug": card_slug, "features": features, "clarify_md": clarify_md }))
}

// `${v}` — JS template-literal coercion (undefined -> "undefined", null -> "null").
fn tmpl_str(v: Option<&Value>) -> String {
    match v {
        None => "undefined".into(),
        Some(Value::Null) => "null".into(),
        Some(x) => str_qq(Some(x)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS memory; ATTACH ':memory:' AS brain; ATTACH ':memory:' AS code_atlas;").unwrap();
        c.execute_batch(
            "CREATE TABLE cards (slug TEXT PRIMARY KEY, title TEXT, body_json TEXT, acceptance_json TEXT, updated_at TEXT);
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
        c.execute("INSERT INTO workspaces (name, root_path, is_active) VALUES ('w', '.', 1)", []).unwrap();
        c
    }

    #[test]
    fn card_slug_is_required() {
        let c = setup();
        let e = plScreeningClarify(&json!({}), &c).unwrap_err();
        assert_eq!(e, "pl-screening-clarify: card_slug is required");
        let e2 = plScreeningClarify(&json!({ "card_slug": "" }), &c).unwrap_err();
        assert_eq!(e2, "pl-screening-clarify: card_slug is required");
    }

    #[test]
    fn card_not_found_errors() {
        let c = setup();
        let e = plScreeningClarify(&json!({ "card_slug": "CARD-404" }), &c).unwrap_err();
        assert_eq!(e, "pl-screening-clarify: card 'CARD-404' not found");
    }

    #[test]
    fn reaches_the_parity_deferred_aicall_boundary_after_full_deterministic_surface() {
        // proves guard/card-lookup/ensure_prompt/context-bundle assembly all ran cleanly and the
        // propagated error is aiCall's deferred stub, not an earlier bug (matches the JS's FAIL
        // LOUD contract: no try/catch around aiCall).
        let c = setup();
        c.execute("INSERT INTO cards (slug, title, body_json) VALUES ('CARD-1', 'Add export', '{\"kind\":\"feat\"}')", []).unwrap();
        let err = plScreeningClarify(&json!({ "card_slug": "CARD-1", "research_md": "some research" }), &c).unwrap_err();
        assert!(err.contains("ai-call"), "unexpected error: {err}");
        let n: i64 = c.query_row("SELECT COUNT(*) FROM brain.prompt_templates WHERE name = 'pl.screening.clarify.v2'", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn ensure_prompt_skips_readd_when_template_matches_verbatim() {
        let c = setup();
        c.execute("INSERT INTO cards (slug, title, body_json) VALUES ('CARD-2', 't', 'b')", []).unwrap();
        let _ = plScreeningClarify(&json!({ "card_slug": "CARD-2" }), &c);
        let _ = plScreeningClarify(&json!({ "card_slug": "CARD-2" }), &c);
        let n: i64 = c.query_row("SELECT COUNT(*) FROM brain.prompt_templates WHERE name = 'pl.screening.clarify.v2'", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1); // second call found the matching template, did not re-add
    }

    #[test]
    fn parse_array_strips_fences_and_falls_back_to_bracket_scan() {
        assert_eq!(parse_array(Some(&json!("[1,2,3]"))), Some(vec![json!(1), json!(2), json!(3)]));
        assert_eq!(
            parse_array(Some(&json!("```json\n[{\"id\":\"ca1\",\"intent\":\"x\"}]\n```"))),
            Some(vec![json!({ "id": "ca1", "intent": "x" })])
        );
        // no clean parse of the whole trimmed string (leading prose) -> bracket-scan fallback.
        assert_eq!(parse_array(Some(&json!("here you go: [1,2]"))), Some(vec![json!(1), json!(2)]));
        assert_eq!(parse_array(Some(&json!("not an array at all"))), None);
        assert_eq!(parse_array(Some(&json!({ "a": 1 }))), None); // valid JSON, not an array -> None
        assert_eq!(parse_array(None), None);
        assert_eq!(parse_array(Some(&Value::Null)), None);
    }

    #[test]
    fn build_features_ids_intents_and_filters_empty_intent() {
        let arr = vec![
            json!({ "id": "ca1", "intent": " do the thing " }),
            json!({ "intent": "no explicit id" }),      // id defaults to ca{i+1}
            json!("bare string feature"),                 // typeof f === 'string' -> intent = f itself
            json!({ "id": "ca4", "intent": "" }),         // empty intent after trim -> filtered out
            json!({ "id": "ca5", "intent": "   " }),      // whitespace-only -> filtered out
            json!(null),                                    // f?.id / f?.intent both undefined -> filtered
        ];
        let out = build_features(arr);
        assert_eq!(
            out,
            vec![
                json!({ "id": "ca1", "intent": "do the thing" }),
                json!({ "id": "ca2", "intent": "no explicit id" }),
                json!({ "id": "ca3", "intent": "bare string feature" }),
            ]
        );
    }

    #[test]
    fn empty_code_context_falls_back_to_greenfield_placeholder() {
        // no code_nodes/code_decorators/etc rows -> contextBundle's bundle_md is empty ->
        // code_context.trim() is empty -> replaced with the greenfield placeholder string, which
        // then flows into the (deferred) aiCall input untouched.
        let c = setup();
        c.execute("INSERT INTO cards (slug, title, body_json) VALUES ('CARD-3', 't', 'b')", []).unwrap();
        let err = plScreeningClarify(&json!({ "card_slug": "CARD-3" }), &c).unwrap_err();
        assert!(err.contains("ai-call"));
    }
}
