//! card-evaluate.rs — twin of skills/entities/card-evaluate.mjs (db, cross-db brain.*). NAMES ARE
//! CONTRACT: default `cardEvaluate`. AI-driven KEEP/MODIFY/DEFER/ARCHIVE recommendation for a
//! single backlog card: loads card + parent EPIC + siblings + last 5 sacred_timeline events,
//! checks a cache (body.ai_evaluations[-1].card_updated_at === card.updated_at, unless
//! force_refresh), ensures the prompt template, queries the AI (tier=mid temp=cold), parses the
//! structured recommendation, and appends it to body.ai_evaluations[] via `card-update` composed
//! through `with-audit` (so the mutation lands in sacred_timeline).
//!
//! PARITY-DEFERRED: `crate::ai_ai_call::aiCall` (see ai-call.rs's module doc) always returns Err —
//! the JS `await aiCall(...)` is NOT wrapped in try/catch, it PROPAGATES the error straight to the
//! caller (mirrored here with `?`). Everything BEFORE that call is real, ported, deterministic
//! surface exercised by this file's own tests: the card_slug/force_refresh guards, `load_context`
//! (card/parent/siblings/timeline reads), the cache-hit short-circuit, and `ensure_prompt`
//! (reusing the already-converted `ai_prompt_find`/`ai_prompt_add` twins — same
//! try-promptFind/catch-promptAdd shape as book-run.rs's `ensure_faculty`). Everything AFTER the
//! call — `get_kv` (output_kv value_text/value reader), `build_evaluation` (recommendation
//! validation + reasoning truncation + evaluation-object shaping) and the final
//! `with-audit`-composed `card-update` append — is ported byte-for-byte from the JS but is
//! unreachable by this file's own fixture (the deferred stub's Err always fires first); it is
//! covered instead by direct unit tests against canned `ai_call`-shaped `Value`s, per the
//! crossing playbook's "response-parse helpers … tested with canned values" rule.
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
// JS strict `===` — the two comparison sites here (card_updated_at cache key) are always
// TEXT-column strings (or one side Value::Null when a snapshot lacks the field entirely).
fn strict_eq(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Null, Value::Null) => true,
        (Value::String(x), Value::String(y)) => x == y,
        (Value::Number(x), Value::Number(y)) => x.as_f64() == y.as_f64(),
        (Value::Bool(x), Value::Bool(y)) => x == y,
        _ => false,
    }
}
fn first(rows: &Value) -> Option<Value> { rows.as_array().and_then(|a| a.first()).cloned() }

const PROMPT_NAME: &str = "board-keeper.card.evaluate.v1";
const PROMPT_TEMPLATE_MD: &str = "You are auditing a single backlog card to decide whether it should still be on the board.

CARD slug:    {{card_slug}}
CARD title:   {{card_title}}
CARD status:  {{card_status}}
CARD age:     created {{card_created_at}}, updated {{card_updated_at}}

CARD body (truncated to 1200 chars):
{{card_body}}

PARENT EPIC (if any):
{{parent_epic}}

SIBLING CARDS in the same EPIC (slug · status):
{{siblings}}

LAST 5 sacred_timeline events:
{{timeline}}

Decide which recommendation fits this card today:

  KEEP    — still wanted, body is good, no changes needed
  MODIFY  — still wanted but the body has drifted; scope/acceptance/files need updating
  DEFER   — right idea, wrong time; park for later
  ARCHIVE — superseded, obsolete, or duplicated by other work; remove from board

Output JSON of this exact shape:
{ \"recommendation\": \"<KEEP|MODIFY|DEFER|ARCHIVE>\", \"reasoning\": \"<one short paragraph, max 300 chars>\", \"suggested_modifications\": \"<only when recommendation=MODIFY, otherwise empty string>\" }";

const VALID_RECOMMENDATIONS: [&str; 4] = ["KEEP", "MODIFY", "DEFER", "ARCHIVE"];

// mirrors JS ensurePrompt's try-promptFind/catch(promptAdd; promptFind) shape (book-run.rs's
// ensure_faculty precedent). The JS caller discards the returned row — so does this one.
fn ensure_prompt(db: &Connection) -> Result<(), String> {
    if crate::ai_prompt_find::promptFind(&json!({ "name": PROMPT_NAME }), db).is_ok() {
        return Ok(());
    }
    let output_fields = json!([
        { "field_name": "recommendation", "field_type": "string", "required": true },
        { "field_name": "reasoning", "field_type": "string", "required": true },
        { "field_name": "suggested_modifications", "field_type": "string", "required": false },
    ]);
    crate::ai_prompt_add::promptAdd(
        &json!({
            "name": PROMPT_NAME,
            "template_md": PROMPT_TEMPLATE_MD,
            "default_tier": "mid",
            "default_temperature": "cold",
            "max_output_tokens": 600,
            "output_fields": output_fields,
        }),
        db,
    )?;
    crate::ai_prompt_find::promptFind(&json!({ "name": PROMPT_NAME }), db)?;
    Ok(())
}

struct Ctx { card: Value, body: String, parent: Option<Value>, siblings: Vec<Value>, timeline: Vec<Value> }

// `JSON.parse(card.body_json ?? '""')` → typeof string ? itself : JSON.stringify(_, null, 2);
// catch → card.body_json ?? ''.
fn compute_body(body_json: &Value) -> String {
    let src = match body_json {
        Value::Null => "\"\"".to_string(),
        Value::String(s) => s.clone(),
        other => js_string(other),
    };
    match serde_json::from_str::<Value>(&src) {
        Ok(Value::String(s)) => s,
        Ok(v) => serde_json::to_string_pretty(&v).unwrap_or_default(),
        Err(_) => match body_json {
            Value::Null => String::new(),
            Value::String(s) => s.clone(),
            other => js_string(other),
        },
    }
}

fn load_context(db: &Connection, slug: &str) -> Result<Ctx, String> {
    let card_rows = crate::db_json::query_rows(
        db,
        "SELECT slug, title, status, body_json, parent_slug, created_at, updated_at FROM cards WHERE slug = ?",
        &[json!(slug)],
    )?;
    let Some(card) = first(&card_rows) else {
        return Err(format!("card-evaluate: card '{slug}' not found"));
    };
    let body = compute_body(card.get("body_json").unwrap_or(&Value::Null));
    let parent_slug = card.get("parent_slug").cloned().unwrap_or(Value::Null);
    let parent = match parent_slug.as_str() {
        Some(ps) if !ps.is_empty() => {
            let rows = crate::db_json::query_rows(db, "SELECT slug, title FROM cards WHERE slug = ?", &[json!(ps)])?;
            first(&rows)
        }
        _ => None,
    };
    let siblings = match parent_slug.as_str() {
        Some(ps) if !ps.is_empty() => {
            let rows = crate::db_json::query_rows(
                db,
                "SELECT slug, status FROM cards WHERE parent_slug = ? AND slug != ? ORDER BY id LIMIT 12",
                &[json!(ps), json!(slug)],
            )?;
            rows.as_array().cloned().unwrap_or_default()
        }
        _ => vec![],
    };
    let timeline_rows = crate::db_json::query_rows(
        db,
        "SELECT ts, operation, actor FROM sacred_timeline WHERE entity_type = 'card' AND entity_id = ? ORDER BY id DESC LIMIT 5",
        &[json!(slug)],
    )?;
    let timeline = timeline_rows.as_array().cloned().unwrap_or_default();
    Ok(Ctx { card, body, parent, siblings, timeline })
}

// `JSON.parse(card.body_json ?? '"{}"')`; if typeof !== 'object' → null. Note the JS default
// literal is the STRING `"{}"` (a quoted string), which parses to the JS string "{}" — typeof
// 'string', so it always resets to null for a NULL body_json column (same net effect as treating
// Null as "no prevBody" below).
fn parse_prev_body(body_json: &Value) -> Option<Value> {
    let src = match body_json {
        Value::Null => "\"{}\"".to_string(),
        Value::String(s) => s.clone(),
        other => js_string(other),
    };
    match serde_json::from_str::<Value>(&src) {
        Ok(v) if v.is_object() || v.is_array() => Some(v),
        _ => None,
    }
}

fn prev_evaluations(prev_body: Option<&Value>) -> Vec<Value> {
    prev_body
        .and_then(|v| v.get("ai_evaluations"))
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default()
}

// output_kv rows expose value_text / value_number / value_boolean columns depending on field
// type; recommendation/reasoning/suggested_modifications are strings → value_text, falling back
// to a generic `value` key (matches the JS `row?.value_text ?? row?.value ?? ''`).
fn get_kv(ai: &Value, name: &str) -> String {
    let row = ai.get("output_kv").and_then(|v| v.as_array()).and_then(|arr| {
        arr.iter().find(|kv| kv.get("field_name").and_then(|v| v.as_str()) == Some(name))
    });
    match row {
        Some(r) => match r.get("value_text").filter(|v| !v.is_null()) {
            Some(v) => js_string(v),
            None => match r.get("value").filter(|v| !v.is_null()) {
                Some(v) => js_string(v),
                None => String::new(),
            },
        },
        None => String::new(),
    }
}

// builds the `evaluation` object appended to body.ai_evaluations[] — recommendation uppercased +
// validated (KEEP fallback), reasoning capped to 500 chars, evaluated_at frozen (playbook
// Date.now()/toISOString() convention).
fn build_evaluation(ai: &Value, card_updated_at: &Value, db: &Connection) -> Value {
    let mut rec = get_kv(ai, "recommendation").to_uppercase();
    if !VALID_RECOMMENDATIONS.contains(&rec.as_str()) {
        rec = "KEEP".to_string();
    }
    let reasoning: String = get_kv(ai, "reasoning").chars().take(500).collect();
    let suggested = get_kv(ai, "suggested_modifications");
    let mut o = Map::new();
    o.insert("recommendation".into(), json!(rec));
    o.insert("reasoning".into(), json!(reasoning));
    o.insert("suggested_modifications".into(), json!(suggested));
    o.insert("ai_call_id".into(), ai.get("ai_call_id").cloned().unwrap_or(Value::Null));
    o.insert("tier_used".into(), ai.get("tier_used").cloned().unwrap_or(Value::Null));
    o.insert("cost_usd".into(), ai.get("cost_usd").cloned().unwrap_or(Value::Null));
    o.insert("card_updated_at".into(), card_updated_at.clone());
    o.insert("evaluated_at".into(), json!(crate::clock::now_iso(db)));
    Value::Object(o)
}

pub fn cardEvaluate(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS: `const { db, provider_fn = null } = context ?? {}; if (!db) throw err.contextMissing(...)`
    // — unreachable here (Rust's `db: &Connection` is never absent). provider_fn threads opaquely
    // into aiCall's input; no Rust equivalent to carry, so `null` (mirrors book-run.rs).
    let provider_fn = Value::Null;

    let card_slug = match input.get("card_slug").and_then(|v| v.as_str()).filter(|s| !s.is_empty()) {
        Some(s) => s,
        None => return Err("card-evaluate: card_slug is required".into()),
    };
    let force_refresh = input.get("force_refresh").map(|v| truthy(Some(v))).unwrap_or(false);

    let ctx = load_context(db, card_slug)?;
    let card_updated_at = ctx.card.get("updated_at").cloned().unwrap_or(Value::Null);

    let prev_body = parse_prev_body(ctx.card.get("body_json").unwrap_or(&Value::Null));
    let prev_evals = prev_evaluations(prev_body.as_ref());
    let last_eval = prev_evals.last().cloned();
    if !force_refresh {
        if let Some(le) = &last_eval {
            if strict_eq(le.get("card_updated_at").unwrap_or(&Value::Null), &card_updated_at) {
                let mut o = le.as_object().cloned().unwrap_or_default();
                o.insert("cached".into(), json!(true));
                o.insert("card_slug".into(), json!(card_slug));
                return Ok(Value::Object(o));
            }
        }
    }

    ensure_prompt(db)?;

    let card_slug_v = ctx.card.get("slug").cloned().unwrap_or(json!(card_slug));
    let card_title = ctx.card.get("title").cloned().unwrap_or(Value::Null);
    let card_status = ctx.card.get("status").cloned().unwrap_or(Value::Null);
    let card_created_at = ctx.card.get("created_at").cloned().unwrap_or(Value::Null);
    let card_body_v: String = ctx.body.chars().take(1200).collect();
    let parent_epic = match &ctx.parent {
        Some(p) => format!("{}: {}", js_string(p.get("slug").unwrap_or(&Value::Null)), js_string(p.get("title").unwrap_or(&Value::Null))),
        None => "(none)".to_string(),
    };
    let siblings_v = if ctx.siblings.is_empty() {
        "(none)".to_string()
    } else {
        ctx.siblings
            .iter()
            .map(|s| format!("  {} · {}", js_string(s.get("slug").unwrap_or(&Value::Null)), js_string(s.get("status").unwrap_or(&Value::Null))))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let timeline_v = if ctx.timeline.is_empty() {
        "(none)".to_string()
    } else {
        ctx.timeline
            .iter()
            .map(|t| format!("  {}  {}  {}", js_string(t.get("ts").unwrap_or(&Value::Null)), js_string(t.get("operation").unwrap_or(&Value::Null)), js_string(t.get("actor").unwrap_or(&Value::Null))))
            .collect::<Vec<_>>()
            .join("\n")
    };

    let ai_input = json!({
        "prompt_name": PROMPT_NAME,
        "input_kv": [
            { "key": "card_slug", "value": card_slug_v },
            { "key": "card_title", "value": card_title },
            { "key": "card_status", "value": card_status },
            { "key": "card_created_at", "value": card_created_at },
            { "key": "card_updated_at", "value": card_updated_at },
            { "key": "card_body", "value": card_body_v },
            { "key": "parent_epic", "value": parent_epic },
            { "key": "siblings", "value": siblings_v },
            { "key": "timeline", "value": timeline_v },
        ],
        "provider_fn": provider_fn,
    });

    // PARITY-DEFERRED from here: JS `await aiCall(...)` is NOT try/catch-wrapped — it PROPAGATES
    // the rejection straight out of cardEvaluate. `?` mirrors that exactly; everything below is
    // unreachable by this file's own fixture (see module doc) but ported for correctness+
    // coverage via build_evaluation/get_kv's own canned-value tests.
    let ai = crate::ai_ai_call::aiCall(&ai_input, db)?;

    let evaluation = build_evaluation(&ai, &card_updated_at, db);

    let mut new_body = prev_body.and_then(|v| v.as_object().cloned()).unwrap_or_default();
    let mut evals = prev_evals;
    evals.push(evaluation.clone());
    let start = evals.len().saturating_sub(10);
    let capped: Vec<Value> = evals[start..].to_vec();
    new_body.insert("ai_evaluations".into(), json!(capped));

    crate::engine_with_audit::withAudit(
        &json!({
            "skill": "card-update",
            "wrappedInput": { "slug": card_slug, "body": Value::Object(new_body) },
            "operation": "card.evaluate",
            "entity_type": "card",
            "entity_id": card_slug,
        }),
        db,
    )?;

    let mut out = evaluation.as_object().cloned().unwrap_or_default();
    out.insert("cached".into(), json!(false));
    out.insert("card_slug".into(), json!(card_slug));
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    const BRAIN_SCHEMA: &str = "
        CREATE TABLE brain.prompt_templates ( id INTEGER PRIMARY KEY, name TEXT NOT NULL, version INTEGER NOT NULL, template_md TEXT NOT NULL, default_tier TEXT NOT NULL CHECK (default_tier IN ('low','mid','high')), default_temperature TEXT NOT NULL CHECK (default_temperature IN ('freezed','cold','warm','hot','hell')), max_output_tokens INTEGER NOT NULL DEFAULT 1024, superseded_by INTEGER, created_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')), cross_field_rules_json TEXT, judging_criteria_json TEXT, system_md TEXT, phase_flow TEXT NOT NULL DEFAULT 'single', UNIQUE (name, version) );
        CREATE TABLE brain.prompt_output_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL CHECK (field_type IN ('string','number','boolean','enum','array_of_string','array_of_object')), enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, grounded_refs_json TEXT, UNIQUE (prompt_template_id, field_name) );
        CREATE TABLE brain.prompt_input_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL CHECK (field_type IN ('string','number','boolean','enum','array_of_string','array_of_object')), enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, UNIQUE (prompt_template_id, field_name) );
    ";

    fn setup() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS brain;").unwrap();
        c.execute_batch(BRAIN_SCHEMA).unwrap();
        c.execute_batch(
            "CREATE TABLE cards (id INTEGER PRIMARY KEY AUTOINCREMENT, slug TEXT UNIQUE, title TEXT, status TEXT, body_json TEXT, parent_slug TEXT, created_at TEXT, updated_at TEXT);
             CREATE TABLE sacred_timeline (id INTEGER PRIMARY KEY AUTOINCREMENT, ts TEXT, actor TEXT, operation TEXT, entity_type TEXT, entity_id TEXT, before_json TEXT, after_json TEXT, payload TEXT, prev_print TEXT, digital_print TEXT, user_goal_id TEXT, exec_env TEXT, tick_id TEXT);",
        ).unwrap();
        c
    }

    fn a_card(c: &Connection, slug: &str, body_json: Option<&str>, updated_at: &str) {
        c.execute(
            "INSERT INTO cards (slug, title, status, body_json, parent_slug, created_at, updated_at) VALUES (?, 'T', 'backlog', ?, NULL, '2025-12-01 00:00:00', ?)",
            rusqlite::params![slug, body_json, updated_at],
        ).unwrap();
    }

    #[test]
    fn card_slug_required() {
        let c = setup();
        assert!(cardEvaluate(&json!({}), &c).unwrap_err().contains("card_slug is required"));
    }

    #[test]
    fn card_not_found() {
        let c = setup();
        assert!(cardEvaluate(&json!({ "card_slug": "GHOST" }), &c).unwrap_err().contains("card 'GHOST' not found"));
    }

    #[test]
    fn cache_hit_returns_last_eval_without_calling_ai() {
        let c = setup();
        let body = json!({ "ai_evaluations": [
            { "recommendation": "KEEP", "reasoning": "fine", "card_updated_at": "2025-12-05 00:00:00", "evaluated_at": "2025-12-05T00:00:00.000Z" }
        ]});
        a_card(&c, "C-1", Some(&serde_json::to_string(&body).unwrap()), "2025-12-05 00:00:00");
        let r = cardEvaluate(&json!({ "card_slug": "C-1" }), &c).unwrap();
        assert_eq!(r["cached"], json!(true));
        assert_eq!(r["recommendation"], json!("KEEP"));
        assert_eq!(r["card_slug"], json!("C-1"));
        // prompt was never ensured — no AI call attempted on the cache-hit path.
        let n: i64 = c.query_row("SELECT COUNT(*) FROM brain.prompt_templates", [], |row| row.get(0)).unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn stale_cache_falls_through_to_the_parity_deferred_aicall_boundary() {
        let c = setup();
        let body = json!({ "ai_evaluations": [
            { "recommendation": "KEEP", "reasoning": "fine", "card_updated_at": "2025-11-01 00:00:00" }
        ]});
        a_card(&c, "C-1", Some(&serde_json::to_string(&body).unwrap()), "2025-12-05 00:00:00");
        let err = cardEvaluate(&json!({ "card_slug": "C-1" }), &c).unwrap_err();
        assert!(err.contains("ai-call"), "unexpected error: {err}");
        let n: i64 = c.query_row("SELECT COUNT(*) FROM brain.prompt_templates WHERE name = ?", [PROMPT_NAME], |row| row.get(0)).unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn force_refresh_bypasses_a_fresh_cache_and_reaches_the_deferred_boundary() {
        let c = setup();
        let body = json!({ "ai_evaluations": [
            { "recommendation": "KEEP", "reasoning": "fine", "card_updated_at": "2025-12-05 00:00:00" }
        ]});
        a_card(&c, "C-1", Some(&serde_json::to_string(&body).unwrap()), "2025-12-05 00:00:00");
        let err = cardEvaluate(&json!({ "card_slug": "C-1", "force_refresh": true }), &c).unwrap_err();
        assert!(err.contains("ai-call"));
    }

    #[test]
    fn no_cache_reaches_the_deferred_boundary_and_ensures_the_prompt() {
        let c = setup();
        a_card(&c, "C-1", None, "2025-12-05 00:00:00");
        let err = cardEvaluate(&json!({ "card_slug": "C-1" }), &c).unwrap_err();
        assert!(err.contains("ai-call"));
        let n: i64 = c.query_row("SELECT COUNT(*) FROM brain.prompt_templates WHERE name = ?", [PROMPT_NAME], |row| row.get(0)).unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn load_context_pulls_parent_siblings_and_timeline() {
        let c = setup();
        a_card(&c, "P-1", None, "2025-12-01 00:00:00");
        c.execute("UPDATE cards SET title = 'The Parent' WHERE slug = 'P-1'", []).unwrap();
        c.execute(
            "INSERT INTO cards (slug, title, status, body_json, parent_slug, created_at, updated_at) VALUES ('C-1','Kid','backlog',NULL,'P-1','2025-12-01 00:00:00','2025-12-05 00:00:00')",
            [],
        ).unwrap();
        c.execute(
            "INSERT INTO cards (slug, title, status, body_json, parent_slug, created_at, updated_at) VALUES ('C-2','Sib','doing',NULL,'P-1','2025-12-01 00:00:00','2025-12-01 00:00:00')",
            [],
        ).unwrap();
        c.execute(
            "INSERT INTO sacred_timeline (ts, actor, operation, entity_type, entity_id) VALUES ('2025-12-05 00:00:00','human:dani','card.add','card','C-1')",
            [],
        ).unwrap();
        let ctx = load_context(&c, "C-1").unwrap();
        assert_eq!(ctx.parent.as_ref().unwrap()["title"], json!("The Parent"));
        assert_eq!(ctx.siblings.len(), 1);
        assert_eq!(ctx.siblings[0]["slug"], json!("C-2"));
        assert_eq!(ctx.timeline.len(), 1);
        assert_eq!(ctx.timeline[0]["operation"], json!("card.add"));
    }

    #[test]
    fn compute_body_unwraps_a_json_string_and_pretty_prints_an_object() {
        assert_eq!(compute_body(&Value::Null), "");
        assert_eq!(compute_body(&json!("\"a plain string\"")), "a plain string");
        let obj_src = json!({ "kind": "feature", "why": "x" }).to_string();
        assert_eq!(compute_body(&json!(obj_src)), "{\n  \"kind\": \"feature\",\n  \"why\": \"x\"\n}");
    }

    // ── build_evaluation / get_kv (unreachable-by-fixture response-parse helpers) ──────────
    fn canned_ai(rec: &str, reasoning: &str, suggested: &str) -> Value {
        json!({
            "ai_call_id": 42,
            "tier_used": "mid",
            "cost_usd": 0.002,
            "output_kv": [
                { "field_name": "recommendation", "value_text": rec },
                { "field_name": "reasoning", "value_text": reasoning },
                { "field_name": "suggested_modifications", "value_text": suggested },
            ],
        })
    }

    #[test]
    fn build_evaluation_uppercases_and_validates_recommendation() {
        let ai = canned_ai("modify", "the body drifted", "update acceptance criteria");
        let ev = build_evaluation(&ai, &json!("2025-12-05 00:00:00"), &Connection::open_in_memory().unwrap());
        assert_eq!(ev["recommendation"], json!("MODIFY"));
        assert_eq!(ev["reasoning"], json!("the body drifted"));
        assert_eq!(ev["suggested_modifications"], json!("update acceptance criteria"));
        assert_eq!(ev["ai_call_id"], json!(42));
        assert_eq!(ev["tier_used"], json!("mid"));
        assert_eq!(ev["cost_usd"], json!(0.002));
        assert_eq!(ev["card_updated_at"], json!("2025-12-05 00:00:00"));
        assert_eq!(ev["evaluated_at"], json!("2026-01-01T00:00:00.000Z"));
    }

    #[test]
    fn build_evaluation_falls_back_to_keep_on_an_invalid_recommendation() {
        let ai = canned_ai("bogus", "r", "");
        let ev = build_evaluation(&ai, &Value::Null, &Connection::open_in_memory().unwrap());
        assert_eq!(ev["recommendation"], json!("KEEP"));
    }

    #[test]
    fn build_evaluation_caps_reasoning_to_500_chars() {
        let long = "x".repeat(600);
        let ai = canned_ai("KEEP", &long, "");
        let ev = build_evaluation(&ai, &Value::Null, &Connection::open_in_memory().unwrap());
        assert_eq!(ev["reasoning"].as_str().unwrap().chars().count(), 500);
    }

    #[test]
    fn get_kv_falls_back_to_empty_string_when_field_missing() {
        let ai = json!({ "output_kv": [] });
        assert_eq!(get_kv(&ai, "recommendation"), "");
    }
}
