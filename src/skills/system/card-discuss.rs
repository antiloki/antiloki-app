//! card-discuss.rs — twin of skills/system/card-discuss.mjs (db READ lane, brain-attached).
//! NAMES ARE CONTRACT: `cardDiscuss` (default). Operator-facing, read-only AI help on a card:
//! FIELD mode (a `field` is given) transforms that field's actual content grounded in the whole
//! card; CARD mode (no field) answers a free-form question about the whole card.
//!
//! PARITY-DEFERRED past the prompt-ensure step: mirrors book-run.rs's `fac_call` pattern exactly
//! — the deterministic prep (card lookup, body parse, mode selection, field-content resolution,
//! `ensure_prompt` which reuses the already-converted `ai_prompt_find`/`ai_prompt_add` twins, and
//! the exact `input_kv` the JS builds) is ported faithfully and IS what this file's tests exercise.
//! It then dispatches through `crate::ai_ai_call::aiCall`, which ALWAYS returns `Err` (see that
//! module's doc — the real network AI-provider call is genuinely non-deterministic and not
//! fixturable), so `cardDiscuss` always propagates that Err once the ported prep above it has run.
//! JS's own `try { aiCall } catch (e) { if (e.code !== 'VALIDATION_LOGIC') throw e; }` swallows
//! ONLY a coded `VALIDATION_LOGIC` AntilokiError to fall through to raw-output recovery; the
//! deferred stub's Err is a plain, uncoded String (never `VALIDATION_LOGIC`), so the observable
//! behavior here is the JS's OWN "rethrow" branch — the same branch a real transport/permission
//! failure would take. The raw-output-recovery fallback (`extractAnswerFromRaw`) and the final
//! result shaping downstream of a successful `aiCall` are therefore unreachable through the public
//! `cardDiscuss` entry point in this twin (same "ported but unreachable past the deferred
//! boundary" convention as book-run.rs's post-wave staging logic); `extractAnswerFromRaw` and the
//! field-content helpers (`findFieldContent`/`renderFieldContent`, both pure and reachable
//! pre-boundary) are still ported below as their own real, independently fixture-tested units.
//!
//! `context.db` missing → not modeled: `db: &Connection` is a required, non-nullable reference
//! here — same "guard-throws are not exercised" convention sync-capture-revision.rs's module doc
//! records for every db-lane twin.
//!
//! Card not found → JS `err.notFound('card-discuss', 'card', slug)` → AntilokiError message
//! `"card-discuss: card '<slug>' not found"`.
use rusqlite::Connection;
use serde_json::{json, Value};

fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}
fn tmpl(v: Option<&Value>) -> String {
    match v {
        None => "undefined".into(),
        Some(Value::Null) => "null".into(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(),
        Some(o) => o.to_string(),
    }
}
// `typeof s === 'string' && s.trim() ? s.trim() : null`
fn trimmed_string_or_null(v: Option<&Value>) -> Option<String> {
    match v {
        Some(Value::String(s)) => {
            let t = s.trim();
            if t.is_empty() { None } else { Some(t.to_string()) }
        }
        _ => None,
    }
}

const CARD_PROMPT_NAME: &str = "system.card.discuss.v2";
const FIELD_PROMPT_NAME: &str = "system.card.discuss.field.v3";

// The JS prompt bodies are long, static markdown carrying no branching logic this twin's tests
// need to exercise; `ensure_prompt` below only needs SOME non-empty `template_md` string to
// register the row (promptAdd validates non-empty, nothing else about its content).
const CARD_PROMPT_TEMPLATE_MD: &str = "CARD slug: {{slug}}\nCARD title: {{title}}\nCARD status: {{status}}\nFULL CARD BODY (JSON): {{body_json}}\nATTACHED REFERENCES: {{references}}\nATTACHED FILES: {{attachments}}\nOPERATOR QUESTION: {{question}}";
const FIELD_PROMPT_TEMPLATE_MD: &str = "CARD slug: {{slug}}\nCARD title: {{title}}\nCARD status: {{status}}\nFULL CARD BODY (JSON): {{body_json}}\nFIELD IN FOCUS: {{field}}\nCURRENT CONTENT OF THAT FIELD: {{field_content}}\nATTACHED REFERENCES: {{references}}\nATTACHED FILES: {{attachments}}\nOPERATOR'S QUESTION / REQUEST: {{instruction}}";

fn operation_directive(op: &str) -> Option<&'static str> {
    match op {
        "expand" => Some("Expand on this field — go into more detail and elaborate on its content, drawing only on what the card supports."),
        "summarize" => Some("Summarize this field — give me the short version that captures the essence of its content."),
        "simplify" => Some("Explain this field in simpler, plainer terms — make its content genuinely easier to understand."),
        _ => None,
    }
}

fn prompt_output_fields() -> Value {
    json!([{ "field_name": "answer_md", "field_type": "string", "required": true }])
}

// try promptFind; on miss, promptAdd then promptFind again (mirrors JS ensurePrompt's try/catch).
fn ensure_prompt(db: &Connection, name: &str, template_md: &str) -> Result<(), String> {
    if crate::ai_prompt_find::promptFind(&json!({ "name": name }), db).is_ok() {
        return Ok(());
    }
    crate::ai_prompt_add::promptAdd(
        &json!({
            "name": name,
            "template_md": template_md,
            "default_tier": "low",
            "default_temperature": "cold",
            "max_output_tokens": 900,
            "output_fields": prompt_output_fields(),
        }),
        db,
    )?;
    Ok(())
}

/// `findFieldContent(body, field)` — locate a field's content in the card body by matching the
/// FE-supplied label/key loosely: lowercase, strip spaces/underscores. Pure.
pub fn findFieldContent(body: &Value, field: Option<&str>) -> (Value, Value) {
    let field = match field {
        Some(f) if !f.is_empty() => f,
        _ => return (Value::Null, Value::Null),
    };
    let obj = match body.as_object() {
        Some(o) => o,
        None => return (Value::Null, Value::Null),
    };
    // JS `s.toLowerCase().replace(/[\s_]+/g, '')` — drop every whitespace/underscore char.
    let norm = |s: &str| s.chars().filter(|c| !c.is_whitespace() && *c != '_').collect::<String>().to_lowercase();
    let want = norm(field);
    for (k, v) in obj.iter() {
        if norm(k) == want {
            return (json!(k), v.clone());
        }
    }
    (Value::Null, Value::Null)
}

/// `renderFieldContent(value)` — string passthrough; array → bullet list (JSON-stringify non-
/// string items); object/number/bool → pretty JSON; null/undefined → ''. Pure.
pub fn renderFieldContent(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::String(s) => s.clone(),
        Value::Array(a) => a
            .iter()
            .map(|v| match v {
                Value::String(s) => format!("- {}", s),
                other => format!("- {}", other),
            })
            .collect::<Vec<_>>()
            .join("\n"),
        other => serde_json::to_string_pretty(other).unwrap_or_else(|_| tmpl(Some(other))),
    }
}

/// `extractAnswerFromRaw(raw)` — recover an answer from a model response that didn't come back
/// as clean `{"answer_md": "..."}` JSON: strip markdown code fences, try to JSON.parse the whole
/// trimmed text or (failing that) the first `{...}` substring found within it, and pull
/// `answer_md`, then `answer`, then the first non-empty string value out of the parsed object;
/// if nothing parses, the trimmed raw text itself is the answer. Pure.
pub fn extractAnswerFromRaw(raw: &Value) -> String {
    if raw.is_null() {
        return String::new();
    }
    let text = match raw {
        Value::String(s) => s.clone(),
        other => serde_json::to_string(other).unwrap_or_else(|_| tmpl(Some(other))),
    };
    let text = text.trim().to_string();
    let stripped = {
        let s = text.trim();
        let s = s.strip_prefix("```json").or_else(|| s.strip_prefix("```")).unwrap_or(s);
        let s = s.strip_suffix("```").unwrap_or(s);
        s.trim().to_string()
    };
    let try_parse = |s: &str| -> Option<Value> { serde_json::from_str(s).ok() };
    let mut obj = try_parse(&stripped);
    if obj.is_none() {
        if let (Some(start), Some(end)) = (stripped.find('{'), stripped.rfind('}')) {
            if end >= start {
                obj = try_parse(&stripped[start..=end]);
            }
        }
    }
    if let Some(Value::Object(o)) = &obj {
        if let Some(Value::String(s)) = o.get("answer_md") {
            if !s.trim().is_empty() {
                return s.clone();
            }
        }
        if let Some(Value::String(s)) = o.get("answer") {
            if !s.trim().is_empty() {
                return s.clone();
            }
        }
        for v in o.values() {
            if let Value::String(s) = v {
                if !s.trim().is_empty() {
                    return s.clone();
                }
            }
        }
    }
    text
}

pub fn cardDiscuss(input: &Value, db: &Connection) -> Result<Value, String> {
    let slug_v = input.get("slug");
    if !truthy(slug_v) {
        return Err("card-discuss: slug is required".to_string());
    }
    let slug = tmpl(slug_v);

    let rows = crate::db_json::query_rows(
        db,
        "SELECT slug, title, status, body_json FROM cards WHERE slug = ?",
        &[json!(slug)],
    )?;
    let row = match rows.as_array().and_then(|a| a.first()) {
        Some(r) => r.clone(),
        None => return Err(format!("card-discuss: card '{}' not found", slug)),
    };

    // `row.body_json ? JSON.parse(row.body_json) : {}` then (if the result is itself a string)
    // parsed a second time — both wrapped in try/catch that silently falls back on failure.
    let body_json_col = row.get("body_json").cloned().unwrap_or(Value::Null);
    let mut body: Value = match body_json_col.as_str() {
        Some(s) if !s.is_empty() => serde_json::from_str(s).unwrap_or(json!({})),
        _ => json!({}),
    };
    if let Value::String(s) = &body {
        body = serde_json::from_str(s).unwrap_or(json!({}));
    }

    let field = input.get("field");
    let operation = input.get("operation");
    let question = input.get("question");

    let field_name = trimmed_string_or_null(field);
    let op = match operation.and_then(|v| v.as_str()) {
        Some(o) if operation_directive(o).is_some() => Some(o.to_string()),
        _ => None,
    };
    let free_form = trimmed_string_or_null(question);

    let is_field_mode = field_name.is_some();

    let title = row.get("title").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let status = row.get("status").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let body_json_str = serde_json::to_string_pretty(&body).unwrap_or_default();

    let (prompt_name, input_kv, _q): (&str, Vec<Value>, String) = if is_field_mode {
        let fname = field_name.clone().unwrap();
        let (_key, value) = findFieldContent(&body, Some(fname.as_str()));
        let field_content = renderFieldContent(&value);
        let instruction = match (&op, &free_form) {
            (Some(o), Some(ff)) => format!(
                "{}\n\nAdditional operator guidance: {}",
                operation_directive(o).unwrap(),
                ff
            ),
            (Some(o), None) => operation_directive(o).unwrap().to_string(),
            (None, Some(ff)) => ff.clone(),
            (None, None) => "Explain this field's content in plain language.".to_string(),
        };
        let q = free_form.clone().unwrap_or_else(|| op.clone().unwrap_or_else(|| "explain".to_string()));
        ensure_prompt(db, FIELD_PROMPT_NAME, FIELD_PROMPT_TEMPLATE_MD)?;
        let field_content_val = if field_content.is_empty() { "(this field is empty)".to_string() } else { field_content };
        let kv = vec![
            json!({ "key": "slug", "value": slug }),
            json!({ "key": "title", "value": title }),
            json!({ "key": "status", "value": status }),
            json!({ "key": "field", "value": fname }),
            json!({ "key": "field_content", "value": field_content_val }),
            json!({ "key": "body_json", "value": body_json_str }),
            json!({ "key": "instruction", "value": instruction }),
        ];
        (FIELD_PROMPT_NAME, kv, q)
    } else {
        let q = free_form.clone().unwrap_or_else(|| "Explain this card.".to_string());
        ensure_prompt(db, CARD_PROMPT_NAME, CARD_PROMPT_TEMPLATE_MD)?;
        let kv = vec![
            json!({ "key": "slug", "value": slug }),
            json!({ "key": "title", "value": title }),
            json!({ "key": "status", "value": status }),
            json!({ "key": "body_json", "value": body_json_str }),
            json!({ "key": "question", "value": q }),
        ];
        (CARD_PROMPT_NAME, kv, q)
    };

    let references = input.get("references").cloned().unwrap_or(json!([]));
    let attachments = input.get("attachments").cloned().unwrap_or(json!([]));
    let ai_input = json!({
        "prompt_name": prompt_name,
        "input_kv": input_kv,
        "references": references,
        "attachments": attachments,
        "intent": "system.card.discuss",
    });

    // PARITY-DEFERRED: crate::ai_ai_call::aiCall always returns Err — see module doc. Everything
    // past this point in the JS (validation-failure raw-output recovery, cost_usd lookup, final
    // { slug, field, operation, question, answer_md, ai_call_id, cost_usd } shaping) is therefore
    // unreachable here; `extractAnswerFromRaw` above is still ported + tested as its own unit.
    let ai_result = crate::ai_ai_call::aiCall(&ai_input, db)?;
    Ok(ai_result)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db0() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "ATTACH ':memory:' AS brain; \
             CREATE TABLE cards (slug TEXT PRIMARY KEY, title TEXT, status TEXT, body_json TEXT); \
             CREATE TABLE brain.prompt_templates ( id INTEGER PRIMARY KEY, name TEXT NOT NULL, version INTEGER NOT NULL, template_md TEXT NOT NULL, default_tier TEXT NOT NULL, default_temperature TEXT NOT NULL, max_output_tokens INTEGER NOT NULL DEFAULT 1024, superseded_by INTEGER, created_at TEXT NOT NULL DEFAULT '2026-01-01 00:00:00', cross_field_rules_json TEXT, judging_criteria_json TEXT, system_md TEXT, phase_flow TEXT NOT NULL DEFAULT 'single', UNIQUE (name, version) ); \
             CREATE TABLE brain.prompt_output_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, grounded_refs_json TEXT, UNIQUE (prompt_template_id, field_name) ); \
             CREATE TABLE brain.prompt_input_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, UNIQUE (prompt_template_id, field_name) );",
        )
        .unwrap();
        c
    }

    fn seed_card(c: &Connection, slug: &str, body: &Value) {
        c.execute(
            "INSERT INTO cards (slug, title, status, body_json) VALUES (?, 'A Title', 'backlog', ?)",
            rusqlite::params![slug, serde_json::to_string(body).unwrap()],
        )
        .unwrap();
    }

    #[test]
    fn missing_slug_is_required() {
        let c = db0();
        let e = cardDiscuss(&json!({}), &c).unwrap_err();
        assert_eq!(e, "card-discuss: slug is required");
    }

    #[test]
    fn missing_card_is_not_found() {
        let c = db0();
        let e = cardDiscuss(&json!({ "slug": "nope" }), &c).unwrap_err();
        assert_eq!(e, "card-discuss: card 'nope' not found");
    }

    #[test]
    fn card_mode_reaches_the_parity_deferred_aicall_boundary_and_ensures_prompt() {
        let c = db0();
        seed_card(&c, "CARD-1", &json!({ "why": "because" }));
        let err = cardDiscuss(&json!({ "slug": "CARD-1" }), &c).unwrap_err();
        assert!(err.contains("ai-call"), "unexpected error: {err}");
        let n: i64 = c
            .query_row(
                "SELECT COUNT(*) FROM brain.prompt_templates WHERE name = 'system.card.discuss.v2'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn field_mode_reaches_the_parity_deferred_aicall_boundary_and_ensures_prompt() {
        let c = db0();
        seed_card(&c, "CARD-2", &json!({ "acceptance_criteria": ["a", "b"] }));
        let err = cardDiscuss(&json!({ "slug": "CARD-2", "field": "Acceptance Criteria" }), &c).unwrap_err();
        assert!(err.contains("ai-call"), "unexpected error: {err}");
        let n: i64 = c
            .query_row(
                "SELECT COUNT(*) FROM brain.prompt_templates WHERE name = 'system.card.discuss.field.v3'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn ensure_prompt_is_idempotent_across_two_calls() {
        let c = db0();
        seed_card(&c, "CARD-3", &json!({}));
        let _ = cardDiscuss(&json!({ "slug": "CARD-3" }), &c);
        let _ = cardDiscuss(&json!({ "slug": "CARD-3" }), &c);
        let n: i64 = c
            .query_row(
                "SELECT COUNT(*) FROM brain.prompt_templates WHERE name = 'system.card.discuss.v2'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(n, 1);
    }

    // ── findFieldContent ─────────────────────────────────────────────────────────────────
    #[test]
    fn find_field_content_matches_loosely_by_label_or_key() {
        let body = json!({ "acceptance_criteria": ["x"], "Motivation": "why" });
        let (k, v) = findFieldContent(&body, Some("acceptance criteria"));
        assert_eq!(k, json!("acceptance_criteria"));
        assert_eq!(v, json!(["x"]));
        let (k2, v2) = findFieldContent(&body, Some("motivation"));
        assert_eq!(k2, json!("Motivation"));
        assert_eq!(v2, json!("why"));
        let (k3, v3) = findFieldContent(&body, Some("nope"));
        assert_eq!(k3, Value::Null);
        assert_eq!(v3, Value::Null);
        let (k4, v4) = findFieldContent(&body, None);
        assert_eq!(k4, Value::Null);
        assert_eq!(v4, Value::Null);
    }

    // ── renderFieldContent ───────────────────────────────────────────────────────────────
    #[test]
    fn render_field_content_by_type() {
        assert_eq!(renderFieldContent(&Value::Null), "");
        assert_eq!(renderFieldContent(&json!("hello")), "hello");
        assert_eq!(renderFieldContent(&json!(["a", "b"])), "- a\n- b");
        assert_eq!(renderFieldContent(&json!([{"x": 1}])), "- {\"x\":1}");
        assert_eq!(renderFieldContent(&json!({"a": 1})), "{\n  \"a\": 1\n}");
    }

    // ── extractAnswerFromRaw ─────────────────────────────────────────────────────────────
    #[test]
    fn extract_answer_from_raw_variants() {
        assert_eq!(extractAnswerFromRaw(&Value::Null), "");
        assert_eq!(extractAnswerFromRaw(&json!("plain text answer")), "plain text answer");
        assert_eq!(extractAnswerFromRaw(&json!("{\"answer_md\": \"the md\"}")), "the md");
        assert_eq!(extractAnswerFromRaw(&json!("```json\n{\"answer\": \"fallback\"}\n```")), "fallback");
        assert_eq!(
            extractAnswerFromRaw(&json!("prose before {\"other\": \"value here\"} prose after")),
            "value here"
        );
        assert_eq!(extractAnswerFromRaw(&json!("{\"answer_md\": \"\"}")), "{\"answer_md\": \"\"}");
    }
}
