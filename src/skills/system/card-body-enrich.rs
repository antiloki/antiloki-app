//! card-body-enrich.rs — twin of skills/system/card-body-enrich.mjs (db, LLM-partial). NAMES ARE
//! CONTRACT: default `cardBodyEnrich`.
//!
//! Proposes a richer card body via AI, with the anti-hallucination guard set applied. Returns a
//! PROPOSAL only — never writes. Reuses the already-converted `system_card_grounding_collect`,
//! `ai_prompt_find`, `ai_prompt_add` twins.
//!
//! PARITY-DEFERRED: this skill's entire raison d'être is one `await aiCall(...)` — the JS has NO
//! try/catch around it, so a thrown/rejected aiCall propagates straight out of the function (same
//! "uncaught aiCall" shape as most LLM-partial twins, mirrored here via `?` on
//! `crate::ai_ai_call::aiCall`, whose deferred stub always returns `Err`). Everything BEFORE the
//! call — the required-`slug` guard, the grounding collect, the current-body parse, the
//! placeholder-`why` detection, and the already-rich-card `no_change_needed` short-circuit — is
//! ported faithfully and IS the deterministic surface these tests exercise. Everything AFTER the
//! call (cost lookup, output_kv unpacking, confidence-floor gate, `proposed_body_json` parsing,
//! the hard grep anti-hallucination filter, kind/size/why locking, and schema validation) is
//! ported byte-for-byte from the JS — including a genuine JS quirk faithfully preserved: JS
//! `significantTokens` ends `.match(re) ?? [].filter(...)`, and because `.` binds tighter than
//! `??`, the `.filter` applies to the literal `[]` fallback, NOT to the match result — so
//! `significant_tokens` here returns the raw regex matches unfiltered too (the STOPWORDS filter
//! only actually happens where JS re-applies it, in `item_supported`). This tail is unreachable by
//! this file's own fixture (same "unreachable after a deferred aiCall" precedent as
//! book-run.rs/book-ingest.rs) — correctness there is by inspection + shape match with the JS.
//!
//! Schema path: same `CARGO_MANIFEST_DIR`-anchored `schemas/card-body.schema.json` seam as
//! card-lint.rs / card-normalize.rs / card-body-generate.rs / law-hook-card-body-schema.rs.
//!
//! `context.db` missing → JS throws `err.contextMissing('card-body-enrich','db')`; unreachable
//! here (Rust's `db: &Connection` is a required, non-nullable reference) — same convention as
//! every sibling twin.
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

fn schema_path() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("schemas")
        .join("card-body.schema.json")
}
fn load_schema() -> Result<Value, String> {
    let text = std::fs::read_to_string(schema_path()).map_err(|e| e.to_string())?;
    serde_json::from_str(&text).map_err(|e| e.to_string())
}

// PLACEHOLDER_WHY_RE, case-insensitive (JS `/i` flag → `(?i)`).
fn placeholder_why_re() -> Regex {
    Regex::new(r"(?i)\(no .*? recorded\)|\(operator to fill in\)|\(unknown\)|\(no motivation\)|\bTODO\b|\bTBD\b|\bFIXME\b|needs human content").unwrap()
}
fn is_placeholder_why(why: Option<&Value>) -> bool {
    let s = match why {
        Some(Value::String(s)) => s,
        _ => return true,
    };
    let t = s.trim();
    t.is_empty() || placeholder_why_re().is_match(t)
}

const PROMPT_NAME: &str = "system.card.body.enrich.v3";

const PROMPT_TEMPLATE_MD: &str = "You are enriching a card body. Given the grounding evidence (title, current motivation, real git history, similar already-rich cards, and any operator guidance), propose richer body fields that fit the canonical schema.\n\nCRITICAL ANTI-HALLUCINATION RULES (read carefully):\n  1. DO NOT INVENT claims. Every proposed item MUST be grounded in the title, current_why, git_evidence, similar_cards, OR the operator guidance. If you cannot point to evidence, OMIT the field.\n  2. Optional fields STAY OPTIONAL. Empty fields are fine and HONEST. Padding is forbidden.\n  3. Set confidence_overall LOW (\u{2264}0.4) if the evidence is thin. A low-confidence proposal will be rejected \u{2014} that is correct behavior.\n  4. Each string-array item must be ONE testable sentence. Avoid vague phrasing.\n\nENRICH EVERY FIELD THE EVIDENCE SUPPORTS \u{2014} not just why. Use these canonical keys:\n  - scope          (array): what this card WILL do \u{2014} concrete in-scope work items.\n  - out_of_scope   (array): what this card explicitly will NOT do \u{2014} boundaries.\n  - deliverables   (array): the artifacts/outputs produced when done.\n  - acceptance     (array): testable conditions that prove the card is complete.\n  - pre_conditions (array): what must be true/in place before work starts.\n  - risks          (array): things that could go wrong or block the work.\n  - files_touched  (array): file paths the work likely changes (only if grounded in git evidence or operator guidance).\n  - verify         (string): the command/recipe to confirm done.\n  - notes          (string): observations / catch-all context not covered above.\n  Plus kind-specific extras: diagnosis (fix), trigger+mitigation (prevent), alternatives_considered (decision).\n\nCARD slug: {{slug}}\nCARD title: {{title}}\nCARD kind: {{kind}}\nCARD size: {{size}}\nCARD current why: {{current_why}}\nWHY STATUS: {{why_status}}\n\nOPERATOR GUIDANCE (human-in-the-loop \u{2014} TREAT AS AUTHORITATIVE EVIDENCE; the operator\nknows this card's intent better than git history does. When present, use it to fill\nscope / out_of_scope / acceptance / notes and to anchor a real why):\n{{operator_guidance}}\n\nGIT EVIDENCE (commit subjects + file-path-matching commits):\n{{git_evidence}}\n\nSIMILAR CARDS (style anchors \u{2014} match this level of specificity):\n{{similar_cards}}\n\nWHY HANDLING:\n  - If WHY STATUS is \"real\", carry the existing why UNCHANGED. Do not rewrite it.\n  - If WHY STATUS is \"placeholder\", the current why is a synthesized stub with\n    NO real content. WRITE a real why: 1-3 sentences explaining the motivation /\n    problem this card addresses, grounded in the operator guidance + title + git\n    evidence + similar cards. If the evidence is too thin to state a real\n    motivation, set confidence_overall \u{2264} 0.4 and leave why as the title restated\n    \u{2014} do NOT invent a fake rationale.\n\nOutput JSON of EXACTLY this shape:\n{\n  \"proposed_body_json\": \"<a JSON string containing the proposed CardBody object>\",\n  \"confidence_overall\": <0.0 to 1.0>,\n  \"reasoning\": \"<one short sentence explaining what evidence anchored the proposal>\"\n}\n\nThe proposed_body_json MUST include kind + size + why. Carry kind + size unchanged. Handle why per WHY HANDLING above. Add every optional field the evidence supports.";

fn prompt_output_fields() -> Value {
    json!([
        { "field_name": "proposed_body_json", "field_type": "string", "required": true },
        { "field_name": "confidence_overall", "field_type": "number", "required": true },
        { "field_name": "reasoning",          "field_type": "string", "required": true },
    ])
}

// try promptFind; on miss (Err), promptAdd (mirrors JS try{...}catch{ return promptAdd(...) }).
fn ensure_prompt(db: &Connection) -> Result<(), String> {
    if crate::ai_prompt_find::promptFind(&json!({ "name": PROMPT_NAME }), db).is_ok() {
        return Ok(());
    }
    crate::ai_prompt_add::promptAdd(
        &json!({
            "name": PROMPT_NAME,
            "template_md": PROMPT_TEMPLATE_MD,
            "default_tier": "low",
            "default_temperature": "cold",
            "max_output_tokens": 1200,
            "output_fields": prompt_output_fields(),
        }),
        db,
    )?;
    Ok(())
}

// ── hard grep check (unreachable tail, ported for correctness) ─────────────────────────────
fn build_evidence_corpus(grounding: &Value) -> String {
    let mut parts: Vec<String> = vec![
        grounding.get("title").map(js_string).unwrap_or_default(),
        grounding.get("current_why").map(js_string).unwrap_or_default(),
    ];
    if let Some(arr) = grounding.get("git_evidence").and_then(Value::as_array) {
        for v in arr {
            parts.push(js_string(v));
        }
    }
    if let Some(arr) = grounding.get("similar_cards").and_then(Value::as_array) {
        for sc in arr {
            parts.push(sc.get("title").map(js_string).unwrap_or_default());
            parts.push(sc.get("body").map(|b| b.to_string()).unwrap_or_else(|| "{}".into()));
        }
    }
    parts.join("\n").to_lowercase()
}

fn stopwords() -> std::collections::HashSet<&'static str> {
    ["the", "and", "with", "this", "that", "from", "into", "have", "will", "when", "where", "card", "cards", "body", "json"]
        .into_iter()
        .collect()
}

// JS: `.match(/[a-z][a-z0-9-]{3,}/g) ?? [].filter(...)` — `.filter` binds to the literal `[]`
// fallback (member access > `??`), so the STOPWORDS filter here is a no-op; preserved faithfully.
fn significant_tokens(s: &str) -> Vec<String> {
    let re = Regex::new(r"[a-z][a-z0-9-]{3,}").unwrap();
    re.find_iter(&s.to_lowercase()).map(|m| m.as_str().to_string()).collect()
}

fn item_supported(item: &str, corpus: &str) -> bool {
    let sw = stopwords();
    let tokens: Vec<String> = significant_tokens(item).into_iter().filter(|t| !sw.contains(t.as_str())).collect();
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
            "reason": "no supporting evidence in title/git/similar",
        }));
    }
    if kept.is_empty() { None } else { Some(json!(kept)) }
}

fn validate_schema(body: &Map<String, Value>, schema: &Value) -> Option<String> {
    let allowed: std::collections::HashSet<String> = schema
        .get("properties")
        .and_then(Value::as_object)
        .map(|m| m.keys().cloned().collect())
        .unwrap_or_default();
    let required: Vec<String> = schema
        .get("required")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
        .unwrap_or_default();
    let forbidden: std::collections::HashSet<String> = schema
        .get("$defs")
        .and_then(|d| d.get("forbiddenKeys"))
        .and_then(|f| f.get("keys"))
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
        .unwrap_or_default();
    let kind_enum: Vec<String> = schema
        .get("properties").and_then(|p| p.get("kind")).and_then(|k| k.get("enum")).and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
        .unwrap_or_default();
    let size_enum: Vec<String> = schema
        .get("properties").and_then(|p| p.get("size")).and_then(|k| k.get("enum")).and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
        .unwrap_or_default();
    for k in body.keys() {
        if forbidden.contains(k) {
            return Some(format!("forbidden key '{k}'"));
        }
        if !allowed.contains(k) {
            return Some(format!("unknown key '{k}'"));
        }
    }
    for k in &required {
        if !body.contains_key(k) {
            return Some(format!("missing required '{k}'"));
        }
    }
    if let Some(Value::String(kind)) = body.get("kind") {
        if !kind_enum.is_empty() && !kind_enum.contains(kind) {
            return Some(format!("kind '{kind}' not in enum"));
        }
    }
    if let Some(Value::String(size)) = body.get("size") {
        if !size_enum.is_empty() && !size_enum.contains(size) {
            return Some(format!("size '{size}' not in enum"));
        }
    }
    None
}

/// `export default async function cardBodyEnrich(input, context)`
pub fn cardBodyEnrich(input: &Value, db: &Connection) -> Result<Value, String> {
    let slug_val = match input.get("slug").filter(|v| truthy(Some(v))) {
        Some(v) => v.clone(),
        None => return Err("card-body-enrich: slug is required".into()),
    };
    let slug = js_string(&slug_val);
    let confidence_floor = match input.get("confidence_floor") {
        None | Some(Value::Null) => 0.7,
        Some(v) => js_number(Some(v)),
    };
    let operator_guidance = input
        .get("operator_guidance")
        .and_then(Value::as_str)
        .map(|s| s.trim().to_string())
        .unwrap_or_default();

    // 1. Collect grounding (always cheap, deterministic).
    let grounding = crate::system_card_grounding_collect::cardGroundingCollect(&json!({ "slug": slug_val }), db)?;

    // Current body + placeholder-why detection (mirrors JS's double-parse-if-stringified guard).
    let rows = crate::db_json::query_rows(db, "SELECT body_json FROM cards WHERE slug = ?", &[slug_val.clone()])?;
    let body_json_val = rows.as_array().and_then(|a| a.first()).and_then(|r| r.get("body_json")).cloned();
    let mut current_body: Value = match &body_json_val {
        Some(Value::String(s)) if !s.is_empty() => serde_json::from_str(s).unwrap_or(json!({})),
        _ => json!({}),
    };
    if let Value::String(s) = current_body.clone() {
        if let Ok(reparsed) = serde_json::from_str(&s) {
            current_body = reparsed;
        }
    }
    let current_body_obj: Map<String, Value> = current_body.as_object().cloned().unwrap_or_default();
    let why_is_placeholder = is_placeholder_why(current_body_obj.get("why"));

    if current_body_obj.len() > 5 && !why_is_placeholder && operator_guidance.is_empty() {
        return Ok(json!({
            "slug": slug,
            "proposed_body": current_body,
            "confidence_overall": 1,
            "dropped_fields": [],
            "status": "no_change_needed",
            "ai_call_id": null,
            "cost_usd": 0,
        }));
    }

    // 2. Ensure prompt registered.
    ensure_prompt(db)?;

    // 3. AI call — JS has no try/catch here, so a thrown/rejected aiCall propagates straight out.
    let why_status = if why_is_placeholder { "placeholder" } else { "real" };
    let git_evidence_joined = grounding
        .get("git_evidence")
        .and_then(Value::as_array)
        .map(|a| a.iter().map(js_string).collect::<Vec<_>>().join("\n"))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "(none — no git history matching this slug)".into());
    let similar_cards_json = json!((grounding.get("similar_cards").and_then(Value::as_array).cloned().unwrap_or_default())
        .into_iter()
        .map(|c| json!({ "slug": c.get("slug").cloned().unwrap_or(Value::Null), "title": c.get("title").cloned().unwrap_or(Value::Null), "body": c.get("body").cloned().unwrap_or(Value::Null) }))
        .collect::<Vec<_>>())
        .to_string();
    let operator_guidance_field = if operator_guidance.is_empty() { "(none provided)".to_string() } else { operator_guidance.clone() };

    let ai_result = crate::ai_ai_call::aiCall(
        &json!({
            "prompt_name": PROMPT_NAME,
            "input_kv": [
                { "key": "slug",              "value": slug },
                { "key": "title",             "value": grounding.get("title").cloned().unwrap_or(json!("")) },
                { "key": "kind",               "value": grounding.get("kind").cloned().filter(|v| !v.is_null()).unwrap_or(json!("feat")) },
                { "key": "size",               "value": grounding.get("size").cloned().filter(|v| !v.is_null()).unwrap_or(json!("M")) },
                { "key": "current_why",        "value": grounding.get("current_why").cloned().unwrap_or(json!("")) },
                { "key": "why_status",         "value": why_status },
                { "key": "operator_guidance",  "value": operator_guidance_field },
                { "key": "git_evidence",       "value": git_evidence_joined },
                { "key": "similar_cards",      "value": similar_cards_json },
            ],
        }),
        db,
    )?;

    // ── unreachable tail past this point (deferred aiCall always Err above) — ported
    // byte-for-byte from the JS for correctness by inspection, same precedent as
    // book-run.rs/book-ingest.rs.
    let ai_call_id = ai_result.get("ai_call_id").cloned().filter(|v| !v.is_null());
    let mut cost = 0.0;
    if let Some(id) = &ai_call_id {
        if let Ok(rows) = crate::db_json::query_rows(db, "SELECT cost_usd_actual FROM brain.ai_calls WHERE id = ?", &[id.clone()]) {
            if let Some(v) = rows.as_array().and_then(|a| a.first()).and_then(|r| r.get("cost_usd_actual")) {
                cost = js_number(Some(v));
                if cost.is_nan() { cost = 0.0; }
            }
        }
    }

    let mut out: Map<String, Value> = Map::new();
    if let Some(arr) = ai_result.get("output_kv").and_then(Value::as_array) {
        for row in arr {
            let field = match row.get("field_name").and_then(Value::as_str) { Some(f) => f, None => continue };
            if let Some(v) = row.get("value_text").filter(|v| !v.is_null()) {
                out.insert(field.to_string(), v.clone());
            } else if let Some(v) = row.get("value_number").filter(|v| !v.is_null()) {
                out.insert(field.to_string(), v.clone());
            } else if let Some(v) = row.get("value_boolean").filter(|v| !v.is_null()) {
                out.insert(field.to_string(), json!(v.as_bool().unwrap_or(false)));
            }
        }
    }

    let confidence_overall = { let n = js_number(out.get("confidence_overall")); if n.is_nan() { 0.0 } else { n } };
    let mut dropped_fields: Vec<Value> = Vec::new();

    // 4. Confidence floor.
    if confidence_overall < confidence_floor {
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

    // 5. Parse the proposed body JSON.
    let proposed_body_json_str = out.get("proposed_body_json").and_then(Value::as_str).unwrap_or("{}");
    let parsed: Result<Value, _> = serde_json::from_str(proposed_body_json_str);
    let mut proposed = match parsed {
        Ok(v @ Value::Object(_)) => v.as_object().cloned().unwrap(),
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

    // 6. Hard grep check on every string-array field.
    let mut corpus = build_evidence_corpus(&grounding);
    if !operator_guidance.is_empty() {
        corpus.push('\n');
        corpus.push_str(&operator_guidance.to_lowercase());
    }
    for f in ["scope", "out_of_scope", "deliverables", "acceptance", "pre_conditions", "risks", "files_touched", "alternatives_considered"] {
        if let Some(v) = proposed.get(f).cloned() {
            match filter_array_field(&v, &corpus, &mut dropped_fields, f) {
                Some(kept) => { proposed.insert(f.to_string(), kept); }
                None => { proposed.remove(f); }
            }
        }
    }

    // 7. Force kind/size to current values — the AI must never change those.
    let kind = current_body_obj.get("kind").cloned()
        .or_else(|| proposed.get("kind").cloned())
        .unwrap_or(json!("feat"));
    proposed.insert("kind".to_string(), kind);
    let size = current_body_obj.get("size").cloned()
        .or_else(|| proposed.get("size").cloned())
        .unwrap_or(json!("M"));
    proposed.insert("size".to_string(), size);
    // `why` is the one required field the AI MAY rewrite — only when the current why was a
    // synthesized placeholder AND the AI produced a real (non-placeholder) replacement.
    if !(why_is_placeholder && !is_placeholder_why(proposed.get("why"))) {
        let why = current_body_obj.get("why").cloned()
            .or_else(|| proposed.get("why").cloned())
            .or_else(|| grounding.get("title").cloned().filter(|v| !v.is_null()))
            .unwrap_or(json!("(no motivation)"));
        proposed.insert("why".to_string(), why);
    }

    // 8. Schema validation. Drop unknown / forbidden keys silently.
    let schema = load_schema()?;
    let allowed: std::collections::HashSet<String> = schema
        .get("properties").and_then(Value::as_object).map(|m| m.keys().cloned().collect()).unwrap_or_default();
    let forbidden: std::collections::HashSet<String> = schema
        .get("$defs").and_then(|d| d.get("forbiddenKeys")).and_then(|f| f.get("keys")).and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect()).unwrap_or_default();
    let keys_to_drop: Vec<String> = proposed.keys().filter(|k| forbidden.contains(*k) || !allowed.contains(*k)).cloned().collect();
    for k in keys_to_drop {
        dropped_fields.push(json!({ "field": k, "reason": "forbidden/unknown key dropped" }));
        proposed.remove(&k);
    }
    if let Some(schema_err) = validate_schema(&proposed, &schema) {
        return Ok(json!({
            "slug": slug, "proposed_body": current_body, "confidence_overall": confidence_overall,
            "dropped_fields": dropped_fields, "status": "no_evidence",
            "ai_call_id": ai_call_id, "cost_usd": cost, "schema_error": schema_err,
        }));
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
    use std::path::PathBuf;

    fn with_substrate_root<T>(root: &str, f: impl FnOnce() -> T) -> T {
        let _guard = crate::test_env_lock::ENV_LOCK.lock().unwrap();
        unsafe { std::env::set_var("ANTILOKI_SUBSTRATE_ROOT", root); }
        let result = f();
        unsafe { std::env::remove_var("ANTILOKI_SUBSTRATE_ROOT"); }
        result
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!("antiloki-card-body-enrich-rs-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

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
        let err = cardBodyEnrich(&json!({}), &c).unwrap_err();
        assert!(err.contains("slug is required"));
    }

    #[test]
    fn unknown_card_propagates_grounding_error() {
        let root = temp_dir("unknown-card");
        let c = db();
        with_substrate_root(root.to_str().unwrap(), || {
            let err = cardBodyEnrich(&json!({ "slug": "NOPE-1" }), &c).unwrap_err();
            assert!(err.contains("not found"), "unexpected error: {err}");
        });
    }

    #[test]
    fn rich_card_real_why_short_circuits_no_change_needed() {
        let root = temp_dir("rich-real-why");
        let c = db();
        let body = json!({
            "kind": "feat", "size": "M", "why": "Real motivation text here.",
            "scope": ["a"], "acceptance": ["b"], "verify": "run tests", "notes": "n",
        })
        .to_string();
        ins_card(&c, "FEAT-RICH-1", "Rich card", &body);
        with_substrate_root(root.to_str().unwrap(), || {
            let out = cardBodyEnrich(&json!({ "slug": "FEAT-RICH-1" }), &c).unwrap();
            assert_eq!(out["status"], json!("no_change_needed"));
            assert_eq!(out["confidence_overall"], json!(1));
            assert_eq!(out["dropped_fields"], json!([]));
            assert_eq!(out["ai_call_id"], Value::Null);
            assert_eq!(out["cost_usd"], json!(0));
            assert_eq!(out["proposed_body"]["why"], json!("Real motivation text here."));
        });
    }

    #[test]
    fn rich_card_placeholder_why_still_proceeds_to_ai_call() {
        let root = temp_dir("rich-placeholder-why");
        let c = db();
        let body = json!({
            "kind": "feat", "size": "M", "why": "(no motivation recorded)",
            "scope": ["a"], "acceptance": ["b"], "verify": "run tests", "notes": "n",
        })
        .to_string();
        ins_card(&c, "FEAT-RICH-2", "Rich card placeholder", &body);
        with_substrate_root(root.to_str().unwrap(), || {
            let err = cardBodyEnrich(&json!({ "slug": "FEAT-RICH-2" }), &c).unwrap_err();
            assert!(err.contains("ai-call"), "expected deferred aiCall error, got: {err}");
        });
    }

    #[test]
    fn sparse_card_proceeds_to_ai_call() {
        let root = temp_dir("sparse-card");
        let c = db();
        ins_card(&c, "FEAT-SPARSE-1", "Sparse card", &json!({ "kind": "feat", "size": "S", "why": "Some real why." }).to_string());
        with_substrate_root(root.to_str().unwrap(), || {
            let err = cardBodyEnrich(&json!({ "slug": "FEAT-SPARSE-1" }), &c).unwrap_err();
            assert!(err.contains("ai-call"), "expected deferred aiCall error, got: {err}");
        });
    }

    #[test]
    fn operator_guidance_never_short_circuits_even_rich_card() {
        let root = temp_dir("operator-guidance");
        let c = db();
        let body = json!({
            "kind": "feat", "size": "M", "why": "Real motivation text here.",
            "scope": ["a"], "acceptance": ["b"], "verify": "run tests", "notes": "n",
        })
        .to_string();
        ins_card(&c, "FEAT-RICH-3", "Rich card", &body);
        with_substrate_root(root.to_str().unwrap(), || {
            let err = cardBodyEnrich(&json!({ "slug": "FEAT-RICH-3", "operator_guidance": "please redo" }), &c).unwrap_err();
            assert!(err.contains("ai-call"), "expected deferred aiCall error, got: {err}");
        });
    }

    // ── unreachable-tail unit tests (exercise the ported-but-unreachable helpers directly) ──
    #[test]
    fn is_placeholder_why_matches_stub_patterns() {
        assert!(is_placeholder_why(Some(&json!("(no motivation recorded)"))));
        assert!(is_placeholder_why(Some(&json!("TODO"))));
        assert!(is_placeholder_why(Some(&json!(""))));
        assert!(is_placeholder_why(None));
        assert!(!is_placeholder_why(Some(&json!("A real explanation of the motivation."))));
    }

    #[test]
    fn item_supported_requires_token_in_corpus() {
        let corpus = "the quick brown fox jumps grounding-evidence";
        assert!(item_supported("the grounding-evidence is here", corpus));
        assert!(!item_supported("completely fabricated nonsense claim", corpus));
    }

    #[test]
    fn validate_schema_flags_unknown_and_missing() {
        let schema = load_schema().unwrap();
        let mut body = Map::new();
        body.insert("kind".into(), json!("feat"));
        body.insert("size".into(), json!("M"));
        body.insert("why".into(), json!("x"));
        body.insert("bogus_key".into(), json!("y"));
        let err = validate_schema(&body, &schema);
        assert!(err.is_some());
    }
}
