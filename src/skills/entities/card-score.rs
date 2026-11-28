//! card-score.rs — twin of skills/entities/card-score.mjs (db lane). NAMES ARE CONTRACT:
//! `cardScore` (default). AI auto-scores a card on three 1-5 facets (risk/complexity/priority);
//! score = risk × complexity × priority (1..125). Persists all four fields to the cards row via
//! card-score-persist.
//!
//! PARITY-DEFERRED: the AI leg is `crate::ai_ai_call::aiCall`, an always-`Err` stub (see
//! ai-call.rs's module doc) — it dispatches a real network call to a BYOK LLM provider, genuinely
//! non-deterministic and not fixturable. Everything ELSE is ported faithfully and IS the
//! deterministic surface this file's tests exercise: the slug guard, the card lookup,
//! `ensurePrompt`'s find-or-register bootstrap (reusing the already-converted
//! `ai_prompt_find`/`ai_prompt_add` twins), `buildRubricMd` (reads laws.scoring_rubric, throws
//! loudly when unseeded), the single-decode body_json rendering + 1600-char cap, and
//! `clampFacet`. The post-aiCall shaping (facet clamp-and-validate from `output_kv`, the
//! card-score-persist `withAudit` call) is ported byte-for-byte from the JS but is unreachable by
//! this file's own fixture because it only runs after a successful `aiCall`, which the deferred
//! stub never produces — same convention as card-accuracy-score.rs / book-run.rs.
//!
//! `context.db` missing → JS throws `err.contextMissing('card-score', 'db')`. Unreachable here:
//! `db: &Connection` is a required, non-nullable reference (same convention as every sibling
//! db-lane twin). `provider_fn` (JS `context.provider_fn`) has no Rust equivalent — it is
//! forwarded into the deferred `aiCall` input the same way the JS forwards it, but is otherwise
//! inert (the stub ignores its whole input).
// QUALITY: mixed unit — slug guard/lookup/prompt-bootstrap/rubric-build/clampFacet are real,
// fixture-tested surface; the post-aiCall persist path is ported but unreachable (deferred AI
// boundary short-circuits it) — same convention as card-accuracy-score.rs.

use rusqlite::Connection;
use serde_json::{json, Value};

fn str_qq(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(),
        Some(o) => o.to_string(),
    }
}

const PROMPT_NAME: &str = "board-keeper.card.score.v3";

const PROMPT_TEMPLATE_MD: &str = "You are SCORING a project card. Read the card's title + body and rate it on three independent 1-5 facets:\n\n{{rubric}}\n\nReturn INTEGERS only (1-5 each). Don't be diplomatic — pick the most\nhonest single number per facet. Rationale is ONE short sentence\nexplaining what drove the highest of the three facets.\n\nThe operator asked for this (re-)scoring with the following reason — weigh it\nwhen judging the facets:\n{{reason}}\n\nCARD slug: {{slug}}\nCARD title: {{title}}\nCARD body:\n{{body}}\n\nOutput JSON of this exact shape:\n{\n  \"risk\":       <1..5>,\n  \"complexity\": <1..5>,\n  \"priority\":   <1..5>,\n  \"rationale\":  \"<one short sentence>\"\n}";

// FACET_LABELS / FACET_ORDER — stable one-line headline per facet (mirrors the JS module-level
// consts). Order matters: buildRubricMd renders blocks in this order.
const FACET_ORDER: [&str; 3] = ["risk", "complexity", "priority"];
fn facet_label(facet: &str) -> &'static str {
    match facet {
        "risk" => "risk        — likelihood of going wrong × blast radius if it does",
        "complexity" => "complexity  — implementation effort + cognitive load",
        "priority" => "priority    — urgency / business value",
        _ => "",
    }
}

/// `buildRubricMd(db)` — reads laws.scoring_rubric, throws when unseeded (JS: NOT_FOUND
/// AntilokiError; twin: a plain Err string carrying the same cause text).
pub fn build_rubric_md(db: &Connection) -> Result<String, String> {
    let rows = crate::db_json::query_rows(
        db,
        "SELECT facet, level, definition FROM laws.scoring_rubric ORDER BY facet, level",
        &[],
    )?;
    let rows = rows.as_array().cloned().unwrap_or_default();
    if rows.is_empty() {
        return Err("card-score: laws.scoring_rubric is empty — run migration 20260529-108".to_string());
    }
    let mut by_facet: std::collections::HashMap<String, Vec<&Value>> = std::collections::HashMap::new();
    for r in &rows {
        let facet = r.get("facet").and_then(Value::as_str).unwrap_or("").to_string();
        by_facet.entry(facet).or_default().push(r);
    }
    let mut blocks: Vec<String> = Vec::new();
    for facet in FACET_ORDER {
        let Some(levels) = by_facet.get(facet) else { continue };
        let lines: Vec<String> = levels
            .iter()
            .map(|r| {
                format!(
                    "                  {} = {}",
                    str_qq(r.get("level")),
                    str_qq(r.get("definition"))
                )
            })
            .collect();
        blocks.push(format!("  {}\n{}", facet_label(facet), lines.join("\n")));
    }
    Ok(blocks.join("\n\n"))
}

/// `ensurePrompt(db)` — internal (not exported). Mirrors the JS
/// `try { return promptFind(...) } catch { promptAdd(...); return promptFind(...) }`.
fn ensure_prompt(db: &Connection) -> Result<Value, String> {
    if let Ok(v) = crate::ai_prompt_find::promptFind(&json!(PROMPT_NAME), db) {
        return Ok(v);
    }
    crate::ai_prompt_add::promptAdd(
        &json!({
            "name": PROMPT_NAME,
            "template_md": PROMPT_TEMPLATE_MD,
            "default_tier": "low",
            "default_temperature": "cold",
            "max_output_tokens": 400,
            "output_fields": [
                { "field_name": "risk", "field_type": "number", "required": true },
                { "field_name": "complexity", "field_type": "number", "required": true },
                { "field_name": "priority", "field_type": "number", "required": true },
                { "field_name": "rationale", "field_type": "string", "required": true },
            ],
        }),
        db,
    )?;
    crate::ai_prompt_find::promptFind(&json!(PROMPT_NAME), db)
}

/// `clampFacet(n)` — `Math.round(Number(n))` clamped to 1..5; `NaN`/non-finite → `None`.
pub fn clamp_facet(n: Option<&Value>) -> Option<i64> {
    let f = match n {
        None | Some(Value::Null) => f64::NAN,
        Some(Value::Bool(b)) => if *b { 1.0 } else { 0.0 },
        Some(Value::Number(num)) => num.as_f64().unwrap_or(f64::NAN),
        Some(Value::String(s)) => {
            let t = s.trim();
            if t.is_empty() { 0.0 } else { t.parse().unwrap_or(f64::NAN) }
        }
        Some(_) => f64::NAN,
    };
    if !f.is_finite() {
        return None;
    }
    let v = f.round() as i64;
    Some(v.clamp(1, 5))
}

#[allow(non_snake_case)]
pub fn cardScore(input: &Value, db: &Connection) -> Result<Value, String> {
    let slug = match input.get("slug").filter(|v| !v.is_null()) {
        Some(v) if v.as_str().map(|s| !s.is_empty()).unwrap_or(true) => v.clone(),
        _ => return Err("card-score: slug is required".to_string()),
    };
    let dry_run = input.get("dry_run").and_then(Value::as_bool).unwrap_or(false);
    let reason = input.get("reason").cloned().unwrap_or(Value::Null);
    let provider_fn = input.get("provider_fn").cloned().unwrap_or(Value::Null);

    let rows = crate::db_json::query_rows(db, "SELECT slug, title, body_json FROM cards WHERE slug = ?", &[slug.clone()])?;
    let Some(card) = rows.as_array().and_then(|a| a.first()).cloned() else {
        return Err(format!("card-score: card '{}' not found", str_qq(Some(&slug))));
    };

    ensure_prompt(db)?;

    // `body_json` may be null (many seed cards have no body) or a JSON-encoded value. Render
    // whatever we have as plain text for the prompt (single JSON.parse; string passes through
    // as-is, anything else re-stringified; parse failure falls back to the raw text).
    let mut body = match card.get("body_json").and_then(|v| v.as_str()) {
        Some(s) if !s.is_empty() => match serde_json::from_str::<Value>(s) {
            Ok(Value::String(inner)) => inner,
            Ok(v) => serde_json::to_string(&v).unwrap_or_default(),
            Err(_) => s.to_string(),
        },
        _ => String::new(),
    };
    if body.trim().is_empty() {
        body = "(no body — score from title only)".to_string();
    }
    if body.chars().count() > 1600 {
        body = body.chars().take(1597).collect::<String>() + "\u{2026}";
    }

    let reason_value = if str_qq(Some(&reason)).trim().is_empty() {
        "(no specific reason — routine score)".to_string()
    } else {
        str_qq(Some(&reason)).trim().to_string()
    };

    let ai_input = json!({
        "prompt_name": PROMPT_NAME,
        "input_kv": [
            { "key": "rubric", "value": build_rubric_md(db)? },
            { "key": "reason", "value": reason_value },
            { "key": "slug", "value": slug },
            { "key": "title", "value": card.get("title").cloned().unwrap_or(Value::Null) },
            { "key": "body", "value": body },
        ],
        "provider_fn": provider_fn,
    });

    // ── PARITY-DEFERRED boundary ─────────────────────────────────────────
    // aiCall is a real network call to a BYOK LLM provider; the twin's stub always returns Err,
    // so everything below is unreachable by this file's fixture but ported by inspection for
    // shape-correctness (mirrors card-accuracy-score.rs's convention).
    let ai_result = crate::ai_ai_call::aiCall(&ai_input, db)?;

    let output_kv = ai_result.get("output_kv").and_then(Value::as_array).cloned().unwrap_or_default();
    let mut out: serde_json::Map<String, Value> = serde_json::Map::new();
    for row in &output_kv {
        let field_name = row.get("field_name").and_then(Value::as_str).unwrap_or("").to_string();
        if let Some(v) = row.get("value_text").filter(|v| !v.is_null()) {
            out.insert(field_name, v.clone());
        } else if let Some(v) = row.get("value_number").filter(|v| !v.is_null()) {
            out.insert(field_name, v.clone());
        } else if let Some(v) = row.get("value_boolean").filter(|v| !v.is_null()) {
            out.insert(field_name, json!(v.as_i64().map(|b| b != 0).unwrap_or(false)));
        }
    }

    let risk = clamp_facet(out.get("risk"));
    let complexity = clamp_facet(out.get("complexity"));
    let priority = clamp_facet(out.get("priority"));
    let (Some(risk), Some(complexity), Some(priority)) = (risk, complexity, priority) else {
        return Err("card-score: AI did not return valid 1-5 integers for all three facets".to_string());
    };
    let score = risk * complexity * priority;
    let rationale: String = out.get("rationale").map(|v| str_qq(Some(v))).unwrap_or_default().chars().take(240).collect();

    if !dry_run {
        crate::engine_with_audit::withAudit(
            &json!({
                "skill": "card-score-persist",
                "wrappedInput": { "slug": slug, "risk": risk, "complexity": complexity, "priority": priority, "score": score },
                "operation": "card.score",
                "entity_type": "card",
                "entity_id": slug,
            }),
            db,
        )?;
    }

    Ok(json!({
        "slug": slug,
        "risk": risk,
        "complexity": complexity,
        "priority": priority,
        "score": score,
        "rationale": rationale,
        "dry_run": dry_run,
        "cached": ai_result.get("cached").cloned().unwrap_or(json!(false)),
        "ai_call_id": ai_result.get("ai_call_id").cloned().unwrap_or(Value::Null),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db0() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS brain; ATTACH ':memory:' AS laws").unwrap();
        c.execute_batch(
            "CREATE TABLE cards ( slug TEXT PRIMARY KEY, title TEXT, body_json TEXT, risk INTEGER, complexity INTEGER, priority INTEGER, score INTEGER, updated_at TEXT );
             CREATE TABLE laws.scoring_rubric ( facet TEXT NOT NULL, level INTEGER NOT NULL, definition TEXT NOT NULL );
             CREATE TABLE brain.prompt_templates ( id INTEGER PRIMARY KEY, name TEXT NOT NULL, version INTEGER NOT NULL, template_md TEXT NOT NULL, default_tier TEXT NOT NULL, default_temperature TEXT NOT NULL, max_output_tokens INTEGER NOT NULL DEFAULT 1024, superseded_by INTEGER, created_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')), cross_field_rules_json TEXT, judging_criteria_json TEXT, system_md TEXT, phase_flow TEXT NOT NULL DEFAULT 'single', UNIQUE (name, version) );
             CREATE TABLE brain.prompt_output_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, grounded_refs_json TEXT, UNIQUE (prompt_template_id, field_name) );
             CREATE TABLE brain.prompt_input_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, UNIQUE (prompt_template_id, field_name) );",
        ).unwrap();
        c
    }

    fn seed_rubric(c: &Connection) {
        c.execute_batch(
            "INSERT INTO laws.scoring_rubric (facet, level, definition) VALUES
               ('risk', 1, 'trivial'), ('risk', 5, 'critical'),
               ('complexity', 1, 'trivial'), ('complexity', 5, 'epic'),
               ('priority', 1, 'can wait'), ('priority', 5, 'do now');",
        )
        .unwrap();
    }

    #[test]
    fn missing_slug_errors() {
        let c = db0();
        let e = cardScore(&json!({}), &c).unwrap_err();
        assert_eq!(e, "card-score: slug is required");
    }

    #[test]
    fn unknown_slug_errors_not_found() {
        let c = db0();
        seed_rubric(&c);
        let e = cardScore(&json!({ "slug": "FEAT-X" }), &c).unwrap_err();
        assert_eq!(e, "card-score: card 'FEAT-X' not found");
    }

    #[test]
    fn empty_rubric_table_throws_loudly() {
        let c = db0();
        c.execute("INSERT INTO cards (slug, title) VALUES ('FEAT-X', 'Do the thing')", []).unwrap();
        let e = cardScore(&json!({ "slug": "FEAT-X" }), &c).unwrap_err();
        assert!(e.contains("laws.scoring_rubric is empty"), "{e}");
    }

    #[test]
    fn guards_pass_then_hits_the_deferred_ai_boundary() {
        let c = db0();
        seed_rubric(&c);
        c.execute(
            "INSERT INTO cards (slug, title, body_json) VALUES ('FEAT-X', 'Do the thing', ?)",
            rusqlite::params![r#""some plain body text""#],
        )
        .unwrap();

        let e = cardScore(&json!({ "slug": "FEAT-X" }), &c).unwrap_err();
        assert!(e.contains("ai-call"), "{e}");

        let n: i64 = c
            .query_row("SELECT COUNT(*) FROM brain.prompt_templates WHERE name = 'board-keeper.card.score.v3'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn ensure_prompt_is_idempotent_on_second_call() {
        let c = db0();
        ensure_prompt(&c).unwrap();
        ensure_prompt(&c).unwrap();
        let n: i64 = c
            .query_row("SELECT COUNT(*) FROM brain.prompt_templates WHERE name = 'board-keeper.card.score.v3'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn clamp_facet_rounds_and_clamps() {
        assert_eq!(clamp_facet(Some(&json!(3.4))), Some(3));
        assert_eq!(clamp_facet(Some(&json!(3.6))), Some(4));
        assert_eq!(clamp_facet(Some(&json!(9))), Some(5));
        assert_eq!(clamp_facet(Some(&json!(-1))), Some(1));
        assert_eq!(clamp_facet(Some(&json!("not a number"))), None);
        assert_eq!(clamp_facet(None), None);
    }

    #[test]
    fn build_rubric_md_renders_facets_in_stable_order() {
        let c = db0();
        seed_rubric(&c);
        let md = build_rubric_md(&c).unwrap();
        let risk_pos = md.find("risk        —").unwrap();
        let complexity_pos = md.find("complexity  —").unwrap();
        let priority_pos = md.find("priority    —").unwrap();
        assert!(risk_pos < complexity_pos && complexity_pos < priority_pos);
        assert!(md.contains("1 = trivial"));
        assert!(md.contains("5 = critical"));
    }
}
