//! card-accuracy-score.rs — twin of skills/entities/card-accuracy-score.mjs (db lane). NAMES ARE
//! CONTRACT: `cardAccuracyScore` (default). The PL evaluates whether a card is ACCURATE and
//! COMPLETE enough for an AI to execute it with certainty: 0..100 accuracy_score + a per-field
//! breakdown ({field, score, issue}), persisted to the cards row via card-accuracy-persist.
//!
//! PARITY-DEFERRED: the AI leg is `crate::ai_ai_call::aiCall`, an always-`Err` stub (see
//! ai-call.rs's module doc) — it dispatches a real network call to a BYOK LLM provider, genuinely
//! non-deterministic and not fixturable. Everything ELSE is ported faithfully and IS the
//! deterministic surface this file's tests exercise: the slug guard, the card lookup, `ensurePrompt`'s
//! find-or-register bootstrap (reusing the already-converted `ai_prompt_find`/`ai_prompt_add`
//! twins), the body_json double-decode, the programmatic pre-check via the already-converted
//! `entities_card_fields_check::evaluateBody` twin, and `clampScore`. The post-aiCall shaping
//! (scalar/field_scores reconstruction from `brain.ai_call_outputs` nested rows, the
//! card-accuracy-persist `withAudit` call) is ported byte-for-byte from the JS but is unreachable
//! by this file's own fixture because it only runs after a successful `aiCall`, which the deferred
//! stub never produces — same convention as pl-session-advance-decompose.rs / book-run.rs.
//!
//! `context.db` missing → JS throws `err.contextMissing('card-accuracy-score', 'db')`.
//! Unreachable here: `db: &Connection` is a required, non-nullable reference (same convention as
//! every sibling db-lane twin). `provider_fn` (JS `input.provider_fn`) has no Rust equivalent — it
//! is forwarded into the deferred `aiCall` input the same way the JS forwards it, but is otherwise
//! inert (the stub ignores its whole input).
// QUALITY: mixed unit — slug guard/lookup/prompt-bootstrap/programmatic-precheck/clampScore are
// real, fixture-tested surface; the post-aiCall persist path is ported but unreachable (deferred
// AI boundary short-circuits it) — same convention as pl-session-advance-decompose.rs.

use rusqlite::Connection;
use serde_json::{json, Map, Value};

fn str_qq(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(),
        Some(o) => o.to_string(),
    }
}

const PROMPT_NAME: &str = "card.pl.accuracy.v2";
const PROMPT_TEMPLATE_MD: &str = "You are the PL (Product Lead) judging whether a project card is ACCURATE and\nCOMPLETE enough for an AI engineer to execute it WITHOUT guessing. You are not\njudging how big or risky the work is — only whether the card, as written, gives\nunambiguous, sufficient, internally-consistent information to act on.\n\nRate the card 0..100:\n  0-39   = not executable: critical information missing or contradictory.\n  40-69  = partial: an engineer would have to guess on at least one field.\n  70-89  = executable: enough to proceed; minor gaps only.\n  90-100 = airtight: every field is specific, consistent, and sufficient.\n\nFor EACH meaningful field present in the body, give a 0..100 sub-score and, when\nbelow 70, a one-line issue saying exactly what is missing or unclear.\n\nA deterministic pre-check already flagged these field problems (use as signal,\ndo not just echo them):\n{{programmatic_findings}}\n\nThe operator asked for this (re-)evaluation with the following reason — weigh it\nwhen deciding what to scrutinise:\n{{reason}}\n\nCARD slug: {{slug}}\nCARD kind: {{kind}}\nCARD title: {{title}}\nCARD body (JSON):\n{{body}}\n\nOutput JSON of EXACTLY this shape:\n{\n  \"accuracy_score\": <0..100 integer — overall>,\n  \"field_scores\": [ { \"field\": \"<name>\", \"score\": <0..100>, \"issue\": \"<one line or empty>\" } ],\n  \"summary\": \"<one short sentence: the single biggest accuracy gap, or 'airtight'>\"\n}";

/// `function ensurePrompt(db)` — internal (not exported). Mirrors the JS
/// `try { return promptFind(...) } catch { promptAdd(...); return promptFind(...) }`.
fn ensure_prompt(db: &Connection) -> Result<Value, String> {
    if let Ok(v) = crate::ai_prompt_find::promptFind(&json!(PROMPT_NAME), db) {
        return Ok(v);
    }
    crate::ai_prompt_add::promptAdd(
        &json!({
            "name": PROMPT_NAME,
            "template_md": PROMPT_TEMPLATE_MD,
            "default_tier": "mid",
            "default_temperature": "cold",
            "max_output_tokens": 700,
            "output_fields": [
                { "field_name": "accuracy_score", "field_type": "number", "required": true },
                { "field_name": "field_scores", "field_type": "array_of_object", "required": false, "description": "each: {field, score, issue}" },
                { "field_name": "summary", "field_type": "string", "required": false },
            ],
        }),
        db,
    )?;
    crate::ai_prompt_find::promptFind(&json!(PROMPT_NAME), db)
}

/// `clampScore(n)` — `Math.round(Number(n))` clamped to 0..100; `NaN`/non-finite → `null`.
pub fn clamp_score(n: Option<&Value>) -> Option<i64> {
    let f = match n {
        None | Some(Value::Null) => f64::NAN,
        Some(Value::Bool(b)) => {
            if *b {
                1.0
            } else {
                0.0
            }
        }
        Some(Value::Number(num)) => num.as_f64().unwrap_or(f64::NAN),
        Some(Value::String(s)) => {
            let t = s.trim();
            if t.is_empty() {
                0.0
            } else {
                t.parse().unwrap_or(f64::NAN)
            }
        }
        Some(_) => f64::NAN,
    };
    if !f.is_finite() {
        return None;
    }
    let v = f.round() as i64;
    Some(v.clamp(0, 100))
}

/// `parseBody` — JS `JSON.parse(card.body_json)`, then `if (typeof parsedBody === 'string')
/// JSON.parse` again (double-decoded); any parse failure → `{}`.
fn parse_body(body_json: Option<&Value>) -> Value {
    let s = match body_json.and_then(|v| v.as_str()) {
        Some(s) if !s.is_empty() => s.to_string(),
        _ => return json!({}),
    };
    match serde_json::from_str::<Value>(&s) {
        Ok(Value::String(inner)) => serde_json::from_str::<Value>(&inner).unwrap_or(json!({})),
        Ok(v) => v,
        Err(_) => json!({}),
    }
}

#[allow(non_snake_case)]
pub fn cardAccuracyScore(input: &Value, db: &Connection) -> Result<Value, String> {
    let slug = match input.get("slug").filter(|v| !v.is_null() && v.as_str().map(|s| !s.is_empty()).unwrap_or(true)) {
        Some(v) if v.as_str().map(|s| !s.is_empty()).unwrap_or(true) => v.clone(),
        _ => return Err("card-accuracy-score: slug is required".to_string()),
    };
    let dry_run = input.get("dry_run").and_then(Value::as_bool).unwrap_or(false);
    let reason = input.get("reason").cloned().unwrap_or(Value::Null);
    let provider_fn = input.get("provider_fn").cloned().unwrap_or(Value::Null);

    let rows = crate::db_json::query_rows(db, "SELECT slug, title, body_json FROM cards WHERE slug = ?", &[slug.clone()])?;
    let Some(card) = rows.as_array().and_then(|a| a.first()).cloned() else {
        return Err(format!("card-accuracy-score: card '{}' not found", str_qq(Some(&slug))));
    };

    ensure_prompt(db)?;

    let parsed_body = parse_body(card.get("body_json"));

    let programmatic = crate::entities_card_fields_check::evaluateBody(&parsed_body);
    let issues = programmatic.get("issues").and_then(Value::as_array).cloned().unwrap_or_default();
    let findings_md = if issues.is_empty() {
        "- (none — all required fields present and above minimum length)".to_string()
    } else {
        issues
            .iter()
            .map(|i| {
                format!(
                    "- {}: {} — {}",
                    str_qq(i.get("field")),
                    str_qq(i.get("severity")),
                    str_qq(i.get("problem"))
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    };

    let mut body_str = serde_json::to_string(&parsed_body).unwrap_or_else(|_| "{}".to_string());
    if body_str.chars().count() > 4000 {
        body_str = body_str.chars().take(3997).collect::<String>() + "\u{2026}";
    }

    let reason_value = if str_qq(Some(&reason)).trim().is_empty() {
        "(no specific reason — routine recompute)".to_string()
    } else {
        str_qq(Some(&reason)).trim().to_string()
    };
    let kind = parsed_body.get("kind").cloned().unwrap_or_else(|| json!("(unknown)"));

    let ai_input = json!({
        "prompt_name": PROMPT_NAME,
        "input_kv": [
            { "key": "programmatic_findings", "value": findings_md },
            { "key": "reason", "value": reason_value },
            { "key": "slug", "value": slug },
            { "key": "kind", "value": kind },
            { "key": "title", "value": card.get("title").cloned().unwrap_or(Value::Null) },
            { "key": "body", "value": body_str },
        ],
        "provider_fn": provider_fn,
    });

    // ── PARITY-DEFERRED boundary ─────────────────────────────────────────
    // aiCall is a real network call to a BYOK LLM provider; the twin's stub always returns Err,
    // so everything below is unreachable by this file's fixture but ported by inspection for
    // shape-correctness (mirrors pl-session-advance-decompose.rs's convention).
    let ai_result = crate::ai_ai_call::aiCall(&ai_input, db)?;

    let output_kv = ai_result.get("output_kv").and_then(Value::as_array).cloned().unwrap_or_default();
    let mut scalars: Map<String, Value> = Map::new();
    for row in &output_kv {
        if row.get("field_name").and_then(Value::as_str) == Some("field_scores") {
            continue;
        }
        let field_name = row.get("field_name").and_then(Value::as_str).unwrap_or("").to_string();
        if let Some(v) = row.get("value_text").filter(|v| !v.is_null()) {
            scalars.insert(field_name, v.clone());
        } else if let Some(v) = row.get("value_number").filter(|v| !v.is_null()) {
            scalars.insert(field_name, v.clone());
        } else if let Some(v) = row.get("value_boolean").filter(|v| !v.is_null()) {
            scalars.insert(field_name, json!(v.as_i64().map(|b| b != 0).unwrap_or(false)));
        }
    }

    let Some(accuracy_score) = clamp_score(scalars.get("accuracy_score")) else {
        return Err("card-accuracy-score: AI did not return a valid 0..100 accuracy_score".to_string());
    };

    // Reconstruct field_scores (array_of_object): parent rows carry no value; their child
    // key/value rows are joined via parent_output_id in brain.ai_call_outputs (same pattern as
    // pl-session-advance-decompose.rs).
    let ai_call_id = ai_result.get("ai_call_id").cloned().unwrap_or(Value::Null);
    let mut parents: Vec<&Value> = output_kv
        .iter()
        .filter(|o| {
            o.get("field_name").and_then(Value::as_str) == Some("field_scores")
                && o.get("value_text").map_or(true, Value::is_null)
                && o.get("value_number").map_or(true, Value::is_null)
        })
        .collect();
    parents.sort_by(|a, b| {
        let ai = a.get("order_index").and_then(Value::as_i64).unwrap_or(0);
        let bi = b.get("order_index").and_then(Value::as_i64).unwrap_or(0);
        ai.cmp(&bi)
    });
    let mut field_scores: Vec<Value> = Vec::new();
    for p in parents {
        let kids = crate::db_json::query_rows(
            db,
            "SELECT field_name, value_text, value_number FROM brain.ai_call_outputs WHERE ai_call_id = ? AND parent_output_id = ?",
            &[ai_call_id.clone(), p.get("id").cloned().unwrap_or(Value::Null)],
        )?;
        let kids: Vec<Value> = kids.as_array().cloned().unwrap_or_default();
        let get = |k: &str| -> Option<Value> { kids.iter().find(|c| c.get("field_name").and_then(Value::as_str) == Some(k)).cloned() };
        let field_row = get("field");
        let score_row = get("score");
        let issue_row = get("issue");
        let score_val = score_row
            .as_ref()
            .and_then(|r| r.get("value_number").filter(|v| !v.is_null()).cloned())
            .or_else(|| {
                score_row
                    .as_ref()
                    .and_then(|r| r.get("value_text"))
                    .filter(|v| !v.is_null())
                    .and_then(|v| v.as_str())
                    .map(|s| json!(s.parse::<f64>().unwrap_or(f64::NAN)))
            })
            .unwrap_or(Value::Null);
        field_scores.push(json!({
            "field": field_row.as_ref().and_then(|r| r.get("value_text")).cloned().unwrap_or(Value::Null),
            "score": score_val,
            "issue": issue_row.as_ref().and_then(|r| r.get("value_text")).cloned().unwrap_or_else(|| json!("")),
        }));
    }
    let summary: String = scalars.get("summary").map(|v| str_qq(Some(v))).unwrap_or_default().chars().take(240).collect();

    let detail = json!({ "field_scores": field_scores, "summary": summary, "programmatic_issues": issues });

    if !dry_run {
        crate::engine_with_audit::withAudit(
            &json!({
                "skill": "card-accuracy-persist",
                "wrappedInput": { "slug": slug, "accuracy_score": accuracy_score, "accuracy_detail": detail },
                "operation": "card.accuracy.score",
                "entity_type": "card",
                "entity_id": slug,
            }),
            db,
        )?;
    }

    Ok(json!({
        "slug": slug,
        "accuracy_score": accuracy_score,
        "field_scores": field_scores,
        "summary": summary,
        "dry_run": dry_run,
        "cached": ai_result.get("cached").cloned().unwrap_or(json!(false)),
        "ai_call_id": ai_call_id,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db0() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS brain").unwrap();
        c.execute_batch(
            "CREATE TABLE cards ( slug TEXT PRIMARY KEY, title TEXT, body_json TEXT, accuracy_score INTEGER, accuracy_detail_json TEXT, updated_at TEXT );
             CREATE TABLE brain.prompt_templates ( id INTEGER PRIMARY KEY, name TEXT NOT NULL, version INTEGER NOT NULL, template_md TEXT NOT NULL, default_tier TEXT NOT NULL, default_temperature TEXT NOT NULL, max_output_tokens INTEGER NOT NULL DEFAULT 1024, superseded_by INTEGER, created_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')), cross_field_rules_json TEXT, judging_criteria_json TEXT, system_md TEXT, phase_flow TEXT NOT NULL DEFAULT 'single', UNIQUE (name, version) );
             CREATE TABLE brain.prompt_output_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, grounded_refs_json TEXT, UNIQUE (prompt_template_id, field_name) );
             CREATE TABLE brain.prompt_input_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, UNIQUE (prompt_template_id, field_name) );
             CREATE TABLE brain.ai_call_outputs ( id INTEGER PRIMARY KEY, ai_call_id INTEGER, field_name TEXT, value_text TEXT, value_number REAL, value_boolean INTEGER, order_index INTEGER, parent_output_id INTEGER );",
        ).unwrap();
        c
    }

    #[test]
    fn missing_slug_errors() {
        let c = db0();
        let e = cardAccuracyScore(&json!({}), &c).unwrap_err();
        assert_eq!(e, "card-accuracy-score: slug is required");
    }

    #[test]
    fn unknown_slug_errors_not_found() {
        let c = db0();
        let e = cardAccuracyScore(&json!({ "slug": "FEAT-X" }), &c).unwrap_err();
        assert_eq!(e, "card-accuracy-score: card 'FEAT-X' not found");
    }

    #[test]
    fn guards_pass_then_hits_the_deferred_ai_boundary() {
        // Everything up to (and including bootstrapping the prompt template + running the
        // programmatic pre-check) runs for real; the function then hits the deferred aiCall stub
        // and propagates its Err — this IS the expected outcome on this twin (see module doc).
        let c = db0();
        c.execute(
            "INSERT INTO cards (slug, title, body_json) VALUES ('FEAT-X', 'Do the thing', ?)",
            rusqlite::params![r#"{"kind":"feat","motivation":"short"}"#],
        )
        .unwrap();

        let e = cardAccuracyScore(&json!({ "slug": "FEAT-X" }), &c).unwrap_err();
        assert!(e.contains("ai-call"), "{e}");

        let n: i64 = c
            .query_row("SELECT COUNT(*) FROM brain.prompt_templates WHERE name = 'card.pl.accuracy.v2'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn ensure_prompt_is_idempotent_on_second_call() {
        let c = db0();
        ensure_prompt(&c).unwrap();
        ensure_prompt(&c).unwrap();
        let n: i64 = c
            .query_row("SELECT COUNT(*) FROM brain.prompt_templates WHERE name = 'card.pl.accuracy.v2'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn clamp_score_rounds_and_clamps() {
        assert_eq!(clamp_score(Some(&json!(72.4))), Some(72));
        assert_eq!(clamp_score(Some(&json!(72.6))), Some(73));
        assert_eq!(clamp_score(Some(&json!(150))), Some(100));
        assert_eq!(clamp_score(Some(&json!(-10))), Some(0));
        assert_eq!(clamp_score(Some(&json!("not a number"))), None);
        assert_eq!(clamp_score(None), None);
    }

    #[test]
    fn parse_body_double_decodes_and_defaults() {
        assert_eq!(parse_body(Some(&json!(r#"{"kind":"feat"}"#))), json!({"kind":"feat"}));
        assert_eq!(parse_body(Some(&json!(r#""{\"kind\":\"fix\"}""#))), json!({"kind":"fix"}));
        assert_eq!(parse_body(Some(&json!(""))), json!({}));
        assert_eq!(parse_body(None), json!({}));
        assert_eq!(parse_body(Some(&json!("not json"))), json!({}));
    }
}
