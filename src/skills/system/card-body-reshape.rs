//! card-body-reshape.rs — twin of skills/system/card-body-reshape.mjs (db, LLM-partial). NAMES
//! ARE CONTRACT: default `cardBodyReshape`.
//!
//! CONVERT a whole card body into the canonical PRODUCT shape via AI. Returns a PROPOSAL only —
//! never writes. Reuses the already-converted `system_card_grounding_collect`, `ai_prompt_find`,
//! `ai_prompt_add` twins.
//!
//! PARITY-DEFERRED: the JS runs a self-refinement (ACC) loop of up to 3 attempts, each ending in
//! an UNCAUGHT `await aiCall(...)` — there is no try/catch anywhere around it (nor around the
//! loop), so the first attempt's thrown/rejected aiCall propagates straight out of the async
//! function. Mirrored here as `run_draft`'s `?` on `crate::ai_ai_call::aiCall`, whose deferred stub
//! always returns `Err`, which the loop's own `?` (on `run_draft(...)?`) propagates out of
//! `cardBodyReshape` on attempt 0 — the refinement steer-building, the confidence comparison
//! across attempts, and everything past the loop never execute. Everything BEFORE the call — the
//! required-`slug` guard, `operator_context` trim, the grounding collect, the double-parse
//! current-body guard (byte-for-byte the same shape as `card-grounding-collect.rs`'s `parse_body`
//! but with a `{}` — not `null` — starting value, matching the JS's own distinct initialization),
//! kind/size derivation, and the drift-registering `ensure_prompt` — is ported faithfully and IS
//! the deterministic surface these tests exercise. Everything AFTER the call (cost lookup,
//! output_kv unpacking, the ACC self-review steer, the confidence-floor gate, `proposed_body_json`
//! parsing, the grep anti-hallucination filter over the 4 product-shape arrays, kind/size
//! locking, the motivation fallback chain, the reproduction/kind=fix rule, the PRODUCT_KEYS strip,
//! and the final schema-allowed-keys strip) is ported byte-for-byte from the JS for correctness by
//! inspection — same "unreachable after a deferred aiCall" precedent as book-run.rs /
//! card-body-enrich.rs.
//!
//! Schema-allowed-keys check: the JS `loadSchema()` reads `schemas/card-body.schema.json` at call
//! time and derives `Object.keys(schema.properties)`. This twin hardcodes that same property-name
//! set (`SCHEMA_KEYS`) rather than re-parsing the JSON file on every call — the set is a superset
//! of `PRODUCT_KEYS` (the prior strip already removes everything this check would catch), so this
//! is a defense-in-depth no-op on the reachable path today; keep `SCHEMA_KEYS` in sync with the
//! schema file's `properties` block by hand if that file's key set ever changes.
//!
//! `context.db` missing → JS throws `err.contextMissing('card-body-reshape','db')`; unreachable
//! here (Rust's `db: &Connection` is a required, non-nullable reference) — same convention as
//! every sibling twin. `provider_fn` is a JS function reference threaded opaquely from context
//! into aiCall's input for provider selection/DI — there is no Rust equivalent to carry (aiCall's
//! dispatch itself is the deferred boundary), so it is passed as `Value::Null` (book-run.rs
//! precedent).
use regex::Regex;
use rusqlite::Connection;
use serde_json::{json, Map, Value};

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
fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) } // JS `??` right operand

// PLACEHOLDER_RE, case-insensitive (JS `/i` flag → `(?i)`). Same pattern text as
// card-body-enrich.rs's PLACEHOLDER_WHY_RE.
fn placeholder_re() -> Regex {
    Regex::new(r"(?i)\(no .*? recorded\)|\(operator to fill in\)|\(unknown\)|\(no motivation\)|\bTODO\b|\bTBD\b|\bFIXME\b|needs human content").unwrap()
}
fn is_placeholder(v: Option<&Value>) -> bool {
    let s = match v {
        Some(Value::String(s)) => s,
        _ => return true,
    };
    let t = s.trim();
    t.is_empty() || placeholder_re().is_match(t)
}

const PROMPT_NAME: &str = "system.card.body.reshape.v3";

const PROMPT_TEMPLATE_MD: &str = r#"You are CONVERTING a card into the canonical PRODUCT shape.

A card is a product ticket — like a Jira issue. It states the MOTIVATION and what
DONE looks like, in product terms a human reads in 20 seconds. It must NOT
contain the solution: no file lists, no commands, no technical task breakdown.
The technical plan is the contract's job, drafted later by the team lead FROM
this card. Your output feeds that step.

Convert the given card (which may be in an old, over-detailed, or near-empty
shape) into EXACTLY these fields. Every field EXCEPT motivation is a LIST of
short strings (one point per item):

  - motivation (string, REQUIRED): What's wrong / missing / wanted, and WHY it
              matters. The user/business pain or goal. 1-3 sentences, product
              language. This is the ONLY non-list field.
  - acceptance_criteria (array of strings): Observable "done" conditions in
              PRODUCT terms — "a user can X", "Y no longer happens". NEVER test
              commands or file paths. One checkable sentence per item.
  - context   (array of strings): Background the reader/AI needs — how it works
              today, constraints, links, prior decisions. Fold genuinely useful
              detail from the old body (notes, scope prose, diagnosis) into
              here. One point per item.
  - out_of_scope (array of strings, optional): Explicit out-of-bounds — "not
              this card / don't touch X". Only real boundaries. One per item.
  - reproduction (array of strings, ONLY when kind=fix): steps to reproduce +
              expected vs actual, one step/observation per item. Omit entirely
              for non-fix cards.

RULES:
  1. DO NOT INVENT. Every claim must trace to the current body, title, git
     evidence, or the operator context. If the card is too empty to state a
     real motivation and the operator gave no context, set confidence_overall
     ≤ 0.4 and restate the title as motivation — do NOT fabricate.
  2. DROP all solution/technical fields (scope, deliverables, files_touched,
     verify, diagnosis, trigger, mitigation, alternatives_considered,
     pre_conditions, risks, acceptance-as-tests). Their PRODUCT essence, if
     any, belongs in acceptance_criteria or context.
  3. Keep it SHORT and human-readable. Padding is forbidden.

CARD slug: {{slug}}
CARD title: {{title}}
CARD kind: {{kind}}
CARD size: {{size}}

CURRENT BODY (the thing you are converting — any shape):
{{current_body}}

OPERATOR DIRECTIVE (human-in-the-loop — TREAT AS AUTHORITATIVE; ALWAYS apply it, even if
the card already looks complete. If it names a field, rework that field. A content-free
one like "go" or "do it" is no steer — then just re-express the card faithfully):
{{operator_context}}

GIT EVIDENCE (commit subjects + matching commits):
{{git_evidence}}

Output JSON of EXACTLY this shape:
{
  "proposed_body_json": "<a JSON string containing the product-shape CardBody>",
  "confidence_overall": <0.0 to 1.0>,
  "reasoning": "<one short sentence on what anchored the conversion>"
}

The proposed_body_json MUST include motivation (non-empty string). Carry kind +
size from the inputs unchanged. Include acceptance_criteria/context/out_of_scope
as arrays when supported; reproduction (array) only when kind=fix."#;

fn prompt_output_fields() -> Value {
    json!([
        { "field_name": "proposed_body_json", "field_type": "string", "required": true },
        { "field_name": "confidence_overall", "field_type": "number", "required": true },
        { "field_name": "reasoning",          "field_type": "string", "required": true },
    ])
}

// try promptFind; if found AND its template_md already matches PROMPT_TEMPLATE_MD, return —
// else (miss, or a stale template) promptAdd (auto-bumps version) then promptFind again. Mirrors
// JS ensurePrompt's drift-register: a plain find-or-add would keep serving a stale template.
fn ensure_prompt(db: &Connection) -> Result<(), String> {
    let existing = crate::ai_prompt_find::promptFind(&json!({ "name": PROMPT_NAME }), db).ok();
    let current = existing
        .as_ref()
        .map(|e| e.get("template_md").and_then(Value::as_str) == Some(PROMPT_TEMPLATE_MD))
        .unwrap_or(false);
    if current {
        return Ok(());
    }
    crate::ai_prompt_add::promptAdd(
        &json!({
            "name": PROMPT_NAME,
            "template_md": PROMPT_TEMPLATE_MD,
            "default_tier": "mid",
            "default_temperature": "cold",
            "max_output_tokens": 1000,
            "output_fields": prompt_output_fields(),
        }),
        db,
    )?;
    crate::ai_prompt_find::promptFind(&json!({ "name": PROMPT_NAME }), db)?;
    Ok(())
}

fn stopwords() -> std::collections::HashSet<&'static str> {
    ["the", "and", "with", "this", "that", "from", "into", "have", "will", "when", "where", "card", "cards", "body", "json"]
        .into_iter()
        .collect()
}

// JS: `(String(s).toLowerCase().match(/[a-z][a-z0-9-]{3,}/g) ?? []).filter((t) =>
// !STOPWORDS.has(t))` — parens group the `??` first, so (unlike card-body-enrich's sibling
// helper, which lacks those parens) the STOPWORDS filter genuinely applies here.
fn significant_tokens(s: &str) -> Vec<String> {
    let re = Regex::new(r"[a-z][a-z0-9-]{3,}").unwrap();
    let sw = stopwords();
    re.find_iter(&s.to_lowercase())
        .map(|m| m.as_str().to_string())
        .filter(|t| !sw.contains(t.as_str()))
        .collect()
}
fn item_supported(item: &str, corpus: &str) -> bool {
    let tokens = significant_tokens(item);
    if tokens.is_empty() {
        return false;
    }
    tokens.iter().any(|t| corpus.contains(t.as_str()))
}
fn filter_array_field(items: &Value, corpus: &str, dropped: &mut Vec<Value>, field_name: &str) -> Option<Value> {
    let arr = items.as_array()?;
    let mut kept: Vec<Value> = Vec::new();
    let mut dropped_items: Vec<Value> = Vec::new();
    for item in arr {
        let s = match item.as_str() {
            Some(s) => s,
            None => continue,
        };
        if item_supported(s, corpus) {
            kept.push(json!(s));
        } else {
            dropped_items.push(json!(s));
        }
    }
    if !dropped_items.is_empty() {
        dropped.push(json!({
            "field": field_name,
            "items": dropped_items,
            "reason": "no supporting evidence in current body/git/operator context",
        }));
    }
    if kept.is_empty() { None } else { Some(json!(kept)) }
}

// mirrors JS's own `{ field_name: value }` unpack from an ai_calls response's output_kv rows.
fn out_kv(res: &Value) -> Map<String, Value> {
    let mut o = Map::new();
    if let Some(arr) = res.get("output_kv").and_then(Value::as_array) {
        for row in arr {
            let field = match row.get("field_name").and_then(Value::as_str) { Some(f) => f, None => continue };
            if let Some(v) = row.get("value_text").filter(|v| !v.is_null()) {
                o.insert(field.to_string(), v.clone());
            } else if let Some(v) = row.get("value_number").filter(|v| !v.is_null()) {
                o.insert(field.to_string(), v.clone());
            } else if let Some(v) = row.get("value_boolean").filter(|v| !v.is_null()) {
                o.insert(field.to_string(), json!(v.as_bool().unwrap_or(false)));
            }
        }
    }
    o
}

struct Draft {
    ai_result: Value,
    out: Map<String, Value>,
    conf: f64,
    cost: f64,
}

// `const runDraft = async (steer) => {...}` — the one `await aiCall(...)` in the whole file, with
// NO surrounding try/catch, so its Err propagates straight through the `?` below and out of every
// caller up to `cardBodyReshape` (same "uncaught aiCall" shape as book-run.rs's `fac_call`).
#[allow(clippy::too_many_arguments)]
fn run_draft(
    db: &Connection,
    provider_fn: &Value,
    slug: &str,
    title: &Value,
    kind: &Value,
    size: &Value,
    current_body_str: &str,
    steer: &str,
    git_evidence_joined: &str,
) -> Result<Draft, String> {
    let steer_field = if steer.is_empty() { "(none provided)".to_string() } else { steer.to_string() };
    let input = json!({
        "prompt_name": PROMPT_NAME,
        "tier": "mid",
        "input_kv": [
            { "key": "slug",             "value": slug },
            { "key": "title",            "value": title },
            { "key": "kind",             "value": kind },
            { "key": "size",             "value": size },
            { "key": "current_body",     "value": current_body_str },
            { "key": "operator_context", "value": steer_field },
            { "key": "git_evidence",     "value": git_evidence_joined },
        ],
        "provider_fn": provider_fn,
    });
    let ai_result = crate::ai_ai_call::aiCall(&input, db)?;

    // ── unreachable tail (aiCall's deferred stub always Err above) — ported byte-for-byte from
    // the JS for correctness by inspection, same precedent as book-run.rs/card-body-enrich.rs.
    let out = out_kv(&ai_result);
    let mut cost = 0.0;
    if let Some(id) = ai_result.get("ai_call_id").filter(|v| !v.is_null()) {
        if let Ok(rows) = crate::db_json::query_rows(db, "SELECT cost_usd_actual FROM brain.ai_calls WHERE id = ?", &[id.clone()]) {
            if let Some(v) = rows.as_array().and_then(|a| a.first()).and_then(|r| r.get("cost_usd_actual")) {
                let c = js_number(Some(v));
                if !c.is_nan() { cost = c; }
            }
        }
    }
    let conf = { let n = js_number(out.get("confidence_overall")); if n.is_nan() { 0.0 } else { n } };
    Ok(Draft { ai_result, out, conf, cost })
}

const MAX_REFINE: i32 = 2;

// `PRODUCT_KEYS` — the reshape output is stripped to exactly these; the schema allows legacy keys
// for the transition but a RESHAPE result should be clean product shape.
const PRODUCT_KEYS: &[&str] = &["kind", "size", "motivation", "acceptance_criteria", "context", "out_of_scope", "reproduction"];

// `Object.keys(schema.properties)` from schemas/card-body.schema.json — see module doc for why
// this is hardcoded rather than re-read from disk on every call.
const SCHEMA_KEYS: &[&str] = &[
    "kind", "size", "motivation", "acceptance_criteria", "context", "out_of_scope", "reproduction",
    "problem", "outcome", "non_goals", "why",
    "scope", "deliverables", "acceptance", "pre_conditions", "risks", "files_touched",
    "verify", "notes", "diagnosis", "trigger", "mitigation", "alternatives_considered",
];

/// `export default async function cardBodyReshape(input, context)`
pub fn cardBodyReshape(input: &Value, db: &Connection) -> Result<Value, String> {
    let provider_fn = Value::Null;
    let slug_val = match input.get("slug").filter(|v| truthy(Some(v))) {
        Some(v) => v.clone(),
        None => return Err("card-body-reshape: slug is required".into()),
    };
    let slug = js_string(&slug_val);
    let operator_context = input
        .get("operator_context")
        .and_then(Value::as_str)
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    let confidence_floor = match input.get("confidence_floor") {
        None => 0.0,
        Some(v) => js_number(Some(v)),
    };

    let grounding = crate::system_card_grounding_collect::cardGroundingCollect(&json!({ "slug": slug_val }), db)?;

    // `let currentBody = {}; try { currentBody = cardRow?.body_json ? JSON.parse(...) : {} }
    // catch {} if (typeof currentBody === 'string') { try { reparse } catch {} }
    // if (!currentBody || typeof !== 'object' || Array.isArray) currentBody = {};`
    let rows = crate::db_json::query_rows(db, "SELECT body_json FROM cards WHERE slug = ?", &[slug_val.clone()])?;
    let body_json_str = rows.as_array().and_then(|a| a.first()).and_then(|r| r.get("body_json")).and_then(Value::as_str).map(str::to_string);
    let mut current_body: Value = match &body_json_str {
        Some(s) if !s.is_empty() => serde_json::from_str(s).unwrap_or(json!({})),
        _ => json!({}),
    };
    if let Value::String(inner) = current_body.clone() {
        if let Ok(reparsed) = serde_json::from_str::<Value>(&inner) {
            current_body = reparsed;
        }
    }
    if !matches!(current_body, Value::Object(_)) {
        current_body = json!({});
    }
    let current_body_obj: Map<String, Value> = current_body.as_object().cloned().unwrap_or_default();

    let kind = nz(current_body_obj.get("kind")).or_else(|| nz(grounding.get("kind"))).cloned().unwrap_or(json!("feat"));
    let size = nz(current_body_obj.get("size")).or_else(|| nz(grounding.get("size"))).cloned().unwrap_or(json!("M"));

    ensure_prompt(db)?;

    // ── SELF-REFINEMENT (ACC loop) — see module doc: the loop's first `run_draft` call errors
    // via aiCall's deferred stub, so the `?` below propagates out of this function immediately
    // and nothing past it (comparison across attempts, the self-review steer, the code following
    // the loop) ever executes on this twin's reachable path. Ported for correctness by inspection.
    let acc_target = match nz(input.get("acc_target")) { Some(v) => js_number(Some(v)), None => 0.70 };
    let title_val = nz(grounding.get("title")).cloned().unwrap_or(json!(""));
    let current_body_str = serde_json::to_string(&current_body).unwrap_or_else(|_| "{}".to_string());
    let current_body_str = if current_body_str.is_empty() { "{}".to_string() } else { current_body_str };
    let git_evidence_joined = grounding
        .get("git_evidence")
        .and_then(Value::as_array)
        .map(|a| a.iter().map(js_string).collect::<Vec<_>>().join("\n"))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "(none — no git history matching this slug)".into());

    let mut cost = 0.0;
    let mut best: Option<Draft> = None;
    let mut steer = operator_context.clone();
    for _attempt in 0..=MAX_REFINE {
        let r = run_draft(db, &provider_fn, &slug, &title_val, &kind, &size, &current_body_str, &steer, &git_evidence_joined)?;
        cost += r.cost;
        let r_conf = r.conf;
        let reasoning = r.out.get("reasoning").and_then(Value::as_str).unwrap_or("(none)").to_string();
        if best.as_ref().map_or(true, |b| r_conf > b.conf) {
            best = Some(r);
        }
        let best_conf = best.as_ref().map(|b| b.conf).unwrap_or(0.0);
        if best_conf >= acc_target {
            break;
        }
        let sep = if operator_context.is_empty() { "" } else { "\n\n" };
        steer = format!(
            "{operator_context}{sep}SELF-REVIEW: your previous draft self-scored confidence {r_conf:.2}, below the {acc_target} target. Its stated anchor was: \"{reasoning}\". Improve the WEAKEST fields — make acceptance_criteria and context more specific and strictly grounded in the evidence corpus, and drop anything you cannot support. Then re-score honestly."
        );
    }

    let best = best.expect("MAX_REFINE >= 0 so the loop runs at least once");
    let ai_result = best.ai_result;
    let out = best.out;
    let confidence_overall = best.conf;
    let ai_call_id = ai_result.get("ai_call_id").cloned().unwrap_or(Value::Null);
    let mut dropped_fields: Vec<Value> = Vec::new();

    if confidence_floor > 0.0 && confidence_overall < confidence_floor {
        return Ok(json!({
            "slug": slug,
            "proposed_body": current_body,
            "confidence_overall": confidence_overall,
            "dropped_fields": [{ "field": "<entire proposal>", "reason": format!("confidence {confidence_overall} below floor {confidence_floor}") }],
            "status": "low_confidence",
            "ai_call_id": ai_call_id,
            "cost_usd": cost,
        }));
    }

    let proposed_body_json_str = out.get("proposed_body_json").and_then(Value::as_str).unwrap_or("{}");
    let mut proposed: Map<String, Value> = match serde_json::from_str::<Value>(proposed_body_json_str) {
        Ok(Value::Object(m)) => m,
        Ok(_) => {
            return Ok(json!({
                "slug": slug, "proposed_body": current_body, "confidence_overall": confidence_overall,
                "dropped_fields": [{ "field": "proposed_body_json", "reason": "AI output was not a JSON object" }],
                "status": "no_evidence", "ai_call_id": ai_call_id, "cost_usd": cost,
            }));
        }
        Err(_) => {
            return Ok(json!({
                "slug": slug, "proposed_body": current_body, "confidence_overall": confidence_overall,
                "dropped_fields": [{ "field": "proposed_body_json", "reason": "AI produced invalid JSON" }],
                "status": "no_evidence", "ai_call_id": ai_call_id, "cost_usd": cost,
            }));
        }
    };

    // Corpus = full current body + grounding + operator context.
    let mut corpus_parts: Vec<String> = vec![
        grounding.get("title").map(js_string).unwrap_or_default(),
        serde_json::to_string(&current_body).unwrap_or_else(|_| "{}".into()),
    ];
    if let Some(arr) = grounding.get("git_evidence").and_then(Value::as_array) {
        for v in arr {
            corpus_parts.push(js_string(v));
        }
    }
    corpus_parts.push(operator_context.clone());
    let corpus = corpus_parts.join("\n").to_lowercase();

    // Grep-filter the product arrays.
    for f in ["acceptance_criteria", "context", "out_of_scope", "reproduction"] {
        if let Some(v) = proposed.get(f).cloned() {
            match filter_array_field(&v, &corpus, &mut dropped_fields, f) {
                Some(kept) => { proposed.insert(f.to_string(), kept); }
                None => { proposed.remove(f); }
            }
        }
    }

    // Carry kind/size; never let the AI change them.
    proposed.insert("kind".to_string(), kind.clone());
    proposed.insert("size".to_string(), size.clone());

    // motivation is required + non-empty; fall back through legacy problem/why, then title.
    if !matches!(proposed.get("motivation"), Some(v) if !is_placeholder(Some(v))) {
        let legacy = [current_body_obj.get("problem"), current_body_obj.get("why")]
            .into_iter()
            .flatten()
            .find(|v| !is_placeholder(Some(*v)))
            .cloned();
        let fallback = legacy
            .or_else(|| nz(grounding.get("current_why")).cloned())
            .or_else(|| nz(grounding.get("title")).cloned())
            .unwrap_or(json!("(no motivation stated)"));
        proposed.insert("motivation".to_string(), fallback);
    }

    // reproduction only on fix cards.
    if js_string(&kind) != "fix" {
        proposed.remove("reproduction");
    }

    // Drop anything outside the product shape.
    let keys: Vec<String> = proposed.keys().cloned().collect();
    for k in keys {
        if !PRODUCT_KEYS.contains(&k.as_str()) {
            dropped_fields.push(json!({ "field": k, "reason": "not part of the product shape — dropped on reshape" }));
            proposed.remove(&k);
        }
    }

    // Final schema sanity.
    let keys2: Vec<String> = proposed.keys().cloned().collect();
    for k in keys2 {
        if !SCHEMA_KEYS.contains(&k.as_str()) {
            dropped_fields.push(json!({ "field": k, "reason": "unknown key dropped" }));
            proposed.remove(&k);
        }
    }

    Ok(json!({
        "slug": slug,
        "proposed_body": proposed,
        "confidence_overall": confidence_overall,
        "dropped_fields": dropped_fields,
        "status": "ok",
        "ai_call_id": ai_call_id,
        "cost_usd": cost,
        "reasoning": out.get("reasoning").cloned().unwrap_or(Value::Null),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    const BRAIN_SCHEMA: &str = "
        CREATE TABLE brain.prompt_templates ( id INTEGER PRIMARY KEY, name TEXT NOT NULL, version INTEGER NOT NULL, template_md TEXT NOT NULL, default_tier TEXT NOT NULL CHECK (default_tier IN ('low','mid','high')), default_temperature TEXT NOT NULL CHECK (default_temperature IN ('freezed','cold','warm','hot','hell')), max_output_tokens INTEGER NOT NULL DEFAULT 1024, superseded_by INTEGER, created_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')), cross_field_rules_json TEXT, judging_criteria_json TEXT, system_md TEXT, phase_flow TEXT NOT NULL DEFAULT 'single', UNIQUE (name, version) );
        CREATE TABLE brain.prompt_output_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL CHECK (field_type IN ('string','number','boolean','enum','array_of_string','array_of_object')), enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, grounded_refs_json TEXT, UNIQUE (prompt_template_id, field_name) );
        CREATE TABLE brain.prompt_input_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL CHECK (field_type IN ('string','number','boolean','enum','array_of_string','array_of_object')), enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, UNIQUE (prompt_template_id, field_name) );
    ";

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE cards (slug TEXT PRIMARY KEY, title TEXT, body_json TEXT, \
             risk INTEGER, complexity INTEGER, priority INTEGER, score REAL, parent_slug TEXT)",
        )
        .unwrap();
        c.execute_batch("ATTACH ':memory:' AS brain").unwrap();
        c.execute_batch(BRAIN_SCHEMA).unwrap();
        c
    }
    fn ins_card(c: &Connection, slug: &str, title: &str, body_json: &str) {
        c.execute(
            "INSERT INTO cards (slug, title, body_json, priority) VALUES (?1, ?2, ?3, 3)",
            rusqlite::params![slug, title, body_json],
        )
        .unwrap();
    }

    #[test]
    fn missing_slug_errors() {
        let c = db();
        let err = cardBodyReshape(&json!({}), &c).unwrap_err();
        assert_eq!(err, "card-body-reshape: slug is required");
    }

    #[test]
    fn unknown_card_propagates_grounding_error() {
        let c = db();
        // no substrate-root env dance needed: cardGroundingCollect fails at the DB lookup,
        // before it would ever touch git.
        let err = cardBodyReshape(&json!({ "slug": "NOPE-1" }), &c).unwrap_err();
        assert!(err.contains("card 'NOPE-1' not found"), "unexpected error: {err}");
    }

    #[test]
    fn reaches_the_parity_deferred_aicall_boundary_and_ensures_prompt() {
        // proves every deterministic gate ran cleanly (slug guard, grounding collect, current-body
        // parse, kind/size derivation, prompt ensure) and the error is aiCall's deferred stub, not
        // an earlier bug.
        let c = db();
        ins_card(&c, "FEAT-RESHAPE-1", "Some Title", r#"{"why":"legacy why here"}"#);
        let err = cardBodyReshape(&json!({ "slug": "FEAT-RESHAPE-1" }), &c).unwrap_err();
        assert!(err.contains("ai-call"), "unexpected error: {err}");
        let n: i64 = c
            .query_row("SELECT COUNT(*) FROM brain.prompt_templates WHERE name = ?1", ["system.card.body.reshape.v3"], |row| row.get(0))
            .unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn ensure_prompt_reregisters_on_template_drift() {
        let c = db();
        c.execute(
            "INSERT INTO brain.prompt_templates (name, version, template_md, default_tier, default_temperature, max_output_tokens) VALUES (?1, 1, 'stale text', 'mid', 'cold', 500)",
            ["system.card.body.reshape.v3"],
        )
        .unwrap();
        ensure_prompt(&c).unwrap();
        let count: i64 = c
            .query_row("SELECT COUNT(*) FROM brain.prompt_templates WHERE name = ?1", ["system.card.body.reshape.v3"], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 2, "stale template must be superseded by a fresh add, not reused");
        let live_md: String = c
            .query_row(
                "SELECT template_md FROM brain.prompt_templates WHERE name = ?1 AND superseded_by IS NULL",
                ["system.card.body.reshape.v3"],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(live_md, PROMPT_TEMPLATE_MD);
    }

    #[test]
    fn ensure_prompt_is_a_noop_when_template_already_current() {
        let c = db();
        ensure_prompt(&c).unwrap();
        ensure_prompt(&c).unwrap();
        let count: i64 = c
            .query_row("SELECT COUNT(*) FROM brain.prompt_templates WHERE name = ?1", ["system.card.body.reshape.v3"], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 1);
    }

    // ── current-body double-parse guard (matches card-grounding-collect.rs's parse_body shape,
    // but this fn's own default is `{}` not `null`) ────────────────────────────────────────────
    #[test]
    fn malformed_body_json_collapses_to_empty_object_not_crash() {
        let c = db();
        ins_card(&c, "FEAT-BAD-1", "t", "{not json");
        let err = cardBodyReshape(&json!({ "slug": "FEAT-BAD-1" }), &c).unwrap_err();
        // still reaches the deferred boundary — the malformed body degraded to {} instead of
        // crashing the parse.
        assert!(err.contains("ai-call"), "unexpected error: {err}");
    }

    // ── is_placeholder ──────────────────────────────────────────────────────────────────────
    #[test]
    fn is_placeholder_matches_todo_tbd_fixme_and_blank() {
        assert!(is_placeholder(None));
        assert!(is_placeholder(Some(&json!(42))));
        assert!(is_placeholder(Some(&json!(""))));
        assert!(is_placeholder(Some(&json!("   "))));
        assert!(is_placeholder(Some(&json!("TODO: fill this in"))));
        assert!(is_placeholder(Some(&json!("still TBD"))));
        assert!(is_placeholder(Some(&json!("FIXME later"))));
        assert!(is_placeholder(Some(&json!("(no motivation)"))));
        assert!(is_placeholder(Some(&json!("(operator to fill in)"))));
        assert!(is_placeholder(Some(&json!("needs human content"))));
        assert!(!is_placeholder(Some(&json!("Users can't export CSV reports, blocking finance's month-end close."))));
    }

    // ── significant_tokens / item_supported / filter_array_field (the grep anti-hallucination
    // gate) ─────────────────────────────────────────────────────────────────────────────────
    #[test]
    fn significant_tokens_filters_stopwords_and_short_words() {
        let toks = significant_tokens("The card body and this json export a report");
        assert!(!toks.contains(&"the".to_string()));
        assert!(!toks.contains(&"and".to_string()));
        assert!(!toks.contains(&"card".to_string()));
        assert!(!toks.contains(&"body".to_string()));
        assert!(!toks.contains(&"json".to_string()));
        assert!(toks.contains(&"export".to_string()));
        assert!(toks.contains(&"report".to_string()));
    }

    #[test]
    fn item_supported_requires_a_token_present_in_corpus() {
        let corpus = "users cannot export csv reports for finance";
        assert!(item_supported("Users can export CSV reports", corpus));
        assert!(!item_supported("Add a dashboard widget for sales", corpus));
        assert!(!item_supported("the and with", corpus)); // all stopwords → no tokens
    }

    #[test]
    fn filter_array_field_splits_kept_and_dropped_with_reason() {
        let corpus = "users cannot export csv reports for finance month-end close";
        let items = json!(["Users can export CSV reports", "Add a random dashboard widget"]);
        let mut dropped: Vec<Value> = Vec::new();
        let kept = filter_array_field(&items, corpus, &mut dropped, "acceptance_criteria").unwrap();
        assert_eq!(kept, json!(["Users can export CSV reports"]));
        assert_eq!(dropped.len(), 1);
        assert_eq!(dropped[0]["field"], json!("acceptance_criteria"));
        assert_eq!(dropped[0]["items"], json!(["Add a random dashboard widget"]));
    }

    #[test]
    fn filter_array_field_returns_none_when_everything_dropped() {
        let mut dropped: Vec<Value> = Vec::new();
        let kept = filter_array_field(&json!(["totally unrelated text here"]), "users export csv reports", &mut dropped, "context");
        assert!(kept.is_none());
        assert_eq!(dropped.len(), 1);
    }

    #[test]
    fn filter_array_field_none_on_non_array_input() {
        let mut dropped: Vec<Value> = Vec::new();
        assert!(filter_array_field(&json!("not an array"), "corpus", &mut dropped, "context").is_none());
        assert!(dropped.is_empty());
    }
}
