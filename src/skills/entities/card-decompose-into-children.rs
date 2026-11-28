//! entities/card-decompose-into-children.rs — twin of
//! skills/entities/card-decompose-into-children.mjs (db, cross-db brain.prompt_templates via the
//! prompt-find/prompt-add twins). NAMES ARE CONTRACT: default `cardDecomposeIntoChildren`.
//! AI-driven decomposition of an EPIC-shaped parent card into N S-sized child cards (each gets
//! parent_slug set), invoked explicitly via `system.card.decompose '{"slug":"EPIC-X"}'`.
//!
//! PARITY-DEFERRED: the single `aiCall` (see ai-call.rs's module doc) that classifies is_atomic +
//! proposes children. Everything around it is ported faithfully and IS the deterministic surface
//! these tests exercise: slug/card guards, the idempotency gate (existing children unless
//! force=true), prompt-template ensure (`ensure_prompt`, reusing the already-converted
//! `ai_prompt_find`/`ai_prompt_add` twins), and the parent body decode (`body_text`) fed into
//! aiCall's input_kv. The post-aiCall logic (is_atomic branch, output_kv child-row extraction,
//! per-child `with-audit`-wrapped `card-add` inserts) is ported byte-for-byte from the JS but is
//! unreachable by this file's own fixture, since it only runs after a successful aiCall, which the
//! deferred stub never produces — correctness there is by inspection, mirroring book-run.rs's
//! post-wave staging code. `card-add` reused via the already-converted
//! `entities_card_add::cardAdd` twin (through `engine_with_audit::withAuditAs`).
use rusqlite::Connection;
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
fn first(rows: &Value) -> Option<Value> { rows.as_array().and_then(|a| a.first()).cloned() }

const PROMPT_NAME: &str = "system.card.decompose.v1";

const PROMPT_TEMPLATE_MD: &str = r#"You are decomposing a project card into smaller child cards. Read the parent's title + body and decide:

  - is_atomic = TRUE if the card describes a single cohesive piece of work (1-2 files, one acceptance criterion, fits in ~1 day). Do NOT decompose; emit children=[].
  - is_atomic = FALSE if the card lists multiple deliverables, mentions phases/steps/children explicitly, or covers > 1 day of work.

When splitting, every child MUST:
  - Have size_estimate ∈ {XS, S}. If you can't shrink a child below S, prefer is_atomic=true and explain in reasoning_md.
  - Have a distinct slug_suggestion using a kebab-case noun-phrase. Prefix appropriately (FEAT-, FIX-, EPIC- only if the child is itself decomposable further).
  - Have its own title (≤ 80 chars) + body covering motivation + acceptance.
  - Be a leaf the substrate can process: name specific files, specific functions, specific tests.

PARENT card slug: {{parent_slug}}
PARENT title: {{title}}
PARENT body:
{{body}}

Output JSON:
{
  "is_atomic": <bool>,
  "reasoning_md": "<2-4 sentences justifying the call>",
  "children": [
    {
      "slug_suggestion": "<KEBAB-UPPER prefix-noun>",
      "title": "<short>",
      "body": "<motivation + acceptance, ~100-300 words>",
      "size_estimate": "<XS or S only>"
    }
  ]
}"#;

fn output_fields() -> Value {
    json!([
        { "field_name": "is_atomic",    "field_type": "boolean", "required": true },
        { "field_name": "reasoning_md", "field_type": "string",  "required": true },
        { "field_name": "children",     "field_type": "array_of_object" },
    ])
}

// try promptFind; on miss, promptAdd then promptFind again (mirrors JS ensurePrompt's try/catch).
fn ensure_prompt(db: &Connection) -> Result<(), String> {
    if crate::ai_prompt_find::promptFind(&json!({ "name": PROMPT_NAME }), db).is_ok() {
        return Ok(());
    }
    crate::ai_prompt_add::promptAdd(
        &json!({
            "name": PROMPT_NAME,
            "template_md": PROMPT_TEMPLATE_MD,
            "default_tier": "mid",
            "default_temperature": "cold",
            "max_output_tokens": 3000,
            "output_fields": output_fields(),
        }),
        db,
    )?;
    crate::ai_prompt_find::promptFind(&json!({ "name": PROMPT_NAME }), db)?;
    Ok(())
}

// `body_json` can be a JSON-encoded string OR a literal string. Decode for the prompt: parse it;
// if the parsed value is itself a string, use it verbatim; otherwise re-stringify pretty (2-space,
// matching JS `JSON.stringify(parsed, null, 2)`). Any parse failure falls back to `String(raw ?? '')`.
fn body_text(card: &Value) -> String {
    let raw = card.get("body_json").cloned().unwrap_or(Value::Null);
    if let Value::String(s) = &raw {
        if !s.is_empty() {
            return match serde_json::from_str::<Value>(s) {
                Ok(Value::String(inner)) => inner,
                Ok(parsed) => serde_json::to_string_pretty(&parsed).unwrap_or_default(),
                Err(_) => str_qq(Some(&raw)),
            };
        }
    }
    String::new()
}

pub fn cardDecomposeIntoChildren(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS: `const { db, actor = 'human:cli', provider_fn = null } = context ?? {}; if (!db) throw
    // err.contextMissing(...)` — unreachable here (Rust's `db: &Connection` is never absent).
    // `actor` threads into per-child with-audit calls below (PARITY-DEFERRED region); `provider_fn`
    // is a JS function reference threaded opaquely into aiCall's input, with no Rust equivalent to
    // carry (aiCall's dispatch itself is the deferred boundary) — passed as `null` below.
    let actor = "human:cli";
    let provider_fn = Value::Null;

    let slug = str_qq(input.get("slug"));
    if slug.is_empty() {
        return Err("card-decompose-into-children: slug is required".to_string());
    }

    let card_rows = crate::db_json::query_rows(db, "SELECT slug, title, body_json, status FROM cards WHERE slug = ?", &[json!(slug)])?;
    let Some(card) = first(&card_rows) else {
        return Err(format!("card-decompose-into-children: card '{slug}' not found"));
    };

    let existing_rows = crate::db_json::query_rows(db, "SELECT COUNT(*) AS c FROM cards WHERE parent_slug = ?", &[json!(slug)])?;
    let existing_children = first(&existing_rows).and_then(|r| r.get("c").and_then(|v| v.as_i64())).unwrap_or(0);
    let force = truthy(input.get("force"));
    if existing_children > 0 && !force {
        return Ok(json!({
            "parent_slug": slug,
            "decomposed": false,
            "skipped": true,
            "reason": format!("card already has {existing_children} children — pass force=true to re-decompose"),
            "existing_children": existing_children,
        }));
    }

    ensure_prompt(db)?;

    let parent_body: String = body_text(&card).chars().take(4000).collect();
    let title = card.get("title").cloned().unwrap_or(Value::Null);

    // ── the single AI call. PARITY-DEFERRED from here on (see module doc) — propagates aiCall's Err.
    let input_kv = json!([
        { "key": "parent_slug", "value": slug },
        { "key": "title",       "value": title },
        { "key": "body",        "value": parent_body },
    ]);
    let ai_input = json!({ "prompt_name": PROMPT_NAME, "input_kv": input_kv, "provider_fn": provider_fn });
    let ai_result = crate::ai_ai_call::aiCall(&ai_input, db)?;

    // ── unreachable past this point given the deferred aiCall stub — ported byte-for-byte from JS.
    let output_kv = ai_result.get("output_kv").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    let find_field = |name: &str| output_kv.iter().find(|o| o.get("field_name").and_then(|v| v.as_str()) == Some(name));
    let is_atomic = match find_field("is_atomic").and_then(|o| o.get("value_boolean")) {
        Some(v) => truthy(Some(v)),
        None => true,
    };
    let reasoning_md = find_field("reasoning_md")
        .and_then(|o| o.get("value_text"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| "(no reasoning)".to_string());
    let ai_call_id = ai_result.get("ai_call_id").cloned().unwrap_or(Value::Null);

    if is_atomic {
        return Ok(json!({
            "parent_slug": slug, "decomposed": false, "is_atomic": true,
            "reasoning_md": reasoning_md, "children": [], "ai_call_id": ai_call_id,
        }));
    }

    let mut child_parents: Vec<&Value> = output_kv.iter().filter(|o| {
        o.get("field_name").and_then(|v| v.as_str()) == Some("children")
            && o.get("value_text").map_or(true, Value::is_null)
            && o.get("value_number").map_or(true, Value::is_null)
            && o.get("value_boolean").map_or(true, Value::is_null)
    }).collect();
    child_parents.sort_by_key(|o| o.get("order_index").and_then(|v| v.as_i64()).unwrap_or(0));

    let max_children_f = js_number(input.get("max_children"));
    let max_children: usize = match input.get("max_children") {
        None => 8,
        Some(_) if max_children_f.is_finite() && max_children_f > 0.0 => max_children_f as usize,
        _ => 0,
    };

    let mut spawned: Vec<Value> = Vec::new();
    let mut skipped_invalid: Vec<Value> = Vec::new();
    for p in child_parents.into_iter().take(max_children) {
        let p_id = p.get("id").cloned().unwrap_or(Value::Null);
        let kids: Vec<&Value> = output_kv.iter().filter(|o| o.get("parent_output_id") == Some(&p_id)).collect();
        let get = |k: &str| -> Option<String> {
            kids.iter()
                .find(|c| c.get("field_name").and_then(|v| v.as_str()) == Some(k))
                .and_then(|c| c.get("value_text"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
        };
        let child_slug = get("slug_suggestion");
        let child_title = get("title");
        let body = get("body").unwrap_or_default();
        let size_estimate = get("size_estimate").unwrap_or_else(|| "S".to_string());
        let (Some(child_slug), Some(child_title)) = (child_slug, child_title) else {
            skipped_invalid.push(json!({ "p": p_id, "reason": "missing slug or title" }));
            continue;
        };
        // Hard constraint: only XS/S children — defense-in-depth (prompt already asks for this).
        if size_estimate != "XS" && size_estimate != "S" {
            skipped_invalid.push(json!({ "slug": child_slug, "reason": format!("size_estimate={size_estimate} > S") }));
            continue;
        }
        // Insert via with-audit so sacred_timeline has a card.add row.
        let audit_input = json!({
            "skill": "card-add",
            "wrappedInput": { "slug": child_slug, "title": child_title, "body": body, "status": "backlog", "parent_slug": slug },
            "operation": "card.add",
            "entity_type": "card",
            "entity_id": child_slug,
        });
        match crate::engine_with_audit::withAuditAs(&audit_input, db, actor) {
            Ok(_) => spawned.push(json!({ "slug": child_slug, "title": child_title, "size_estimate": size_estimate })),
            Err(e) => skipped_invalid.push(json!({ "slug": child_slug, "reason": e.chars().take(200).collect::<String>() })),
        }
    }

    Ok(json!({
        "parent_slug": slug,
        "decomposed": !spawned.is_empty(),
        "is_atomic": false,
        "reasoning_md": reasoning_md,
        "children": spawned,
        "skipped_invalid": skipped_invalid,
        "ai_call_id": ai_call_id,
        "cached": ai_result.get("cached").cloned().unwrap_or(Value::Null),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS brain;").unwrap();
        c.execute_batch(
            "CREATE TABLE cards (slug TEXT PRIMARY KEY, title TEXT, body_json TEXT, status TEXT NOT NULL DEFAULT 'backlog', parent_slug TEXT);
             CREATE TABLE brain.prompt_templates ( id INTEGER PRIMARY KEY, name TEXT NOT NULL, version INTEGER NOT NULL, template_md TEXT NOT NULL, default_tier TEXT NOT NULL, default_temperature TEXT NOT NULL, max_output_tokens INTEGER NOT NULL DEFAULT 1024, superseded_by INTEGER, created_at TEXT NOT NULL DEFAULT (datetime('now')), cross_field_rules_json TEXT, judging_criteria_json TEXT, system_md TEXT, phase_flow TEXT NOT NULL DEFAULT 'single', UNIQUE (name, version) );
             CREATE TABLE brain.prompt_output_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, grounded_refs_json TEXT, UNIQUE (prompt_template_id, field_name) );
             CREATE TABLE brain.prompt_input_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, UNIQUE (prompt_template_id, field_name) );",
        ).unwrap();
        c
    }

    #[test]
    fn requires_slug() {
        let c = setup();
        assert!(cardDecomposeIntoChildren(&json!({}), &c).unwrap_err().contains("slug is required"));
    }

    #[test]
    fn errors_when_card_not_found() {
        let c = setup();
        assert!(cardDecomposeIntoChildren(&json!({ "slug": "EPIC-X" }), &c).unwrap_err().contains("card 'EPIC-X' not found"));
    }

    #[test]
    fn skips_when_children_exist_without_force() {
        let c = setup();
        c.execute("INSERT INTO cards (slug, title) VALUES ('EPIC-X','Big epic')", []).unwrap();
        c.execute("INSERT INTO cards (slug, title, parent_slug) VALUES ('CHILD-1','Child','EPIC-X')", []).unwrap();
        let r = cardDecomposeIntoChildren(&json!({ "slug": "EPIC-X" }), &c).unwrap();
        assert_eq!(r["skipped"], json!(true));
        assert_eq!(r["decomposed"], json!(false));
        assert_eq!(r["existing_children"], json!(1));
        assert!(r["reason"].as_str().unwrap().contains("already has 1 children — pass force=true"));
    }

    #[test]
    fn force_bypasses_gate_and_reaches_parity_deferred_boundary() {
        let c = setup();
        c.execute("INSERT INTO cards (slug, title, body_json) VALUES ('EPIC-X','Big epic', '\"do many things\"')", []).unwrap();
        c.execute("INSERT INTO cards (slug, title, parent_slug) VALUES ('CHILD-1','Child','EPIC-X')", []).unwrap();
        let err = cardDecomposeIntoChildren(&json!({ "slug": "EPIC-X", "force": true }), &c).unwrap_err();
        assert!(err.contains("ai-call"), "unexpected error: {err}");
        // the prompt template WAS ensured (ensure_prompt ran) before the deferred call
        let n: i64 = c.query_row("SELECT COUNT(*) FROM brain.prompt_templates WHERE name = 'system.card.decompose.v1'", [], |row| row.get(0)).unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn no_existing_children_reaches_parity_deferred_boundary() {
        let c = setup();
        c.execute("INSERT INTO cards (slug, title) VALUES ('EPIC-Y','Solo epic')", []).unwrap();
        let err = cardDecomposeIntoChildren(&json!({ "slug": "EPIC-Y" }), &c).unwrap_err();
        assert!(err.contains("ai-call"), "unexpected error: {err}");
    }

    #[test]
    fn ensure_prompt_is_idempotent_on_a_second_call() {
        let c = setup();
        c.execute("INSERT INTO cards (slug, title) VALUES ('EPIC-Z','Z epic')", []).unwrap();
        let _ = cardDecomposeIntoChildren(&json!({ "slug": "EPIC-Z" }), &c);
        let _ = cardDecomposeIntoChildren(&json!({ "slug": "EPIC-Z", "force": true }), &c);
        let n: i64 = c.query_row("SELECT COUNT(*) FROM brain.prompt_templates WHERE name = 'system.card.decompose.v1'", [], |row| row.get(0)).unwrap();
        assert_eq!(n, 1, "second ensure_prompt call must not re-insert (promptFind hit on 2nd run)");
    }

    // ── body_text (parent body decode fed into aiCall's input_kv) ────────────────────────────
    #[test]
    fn body_text_unwraps_a_json_encoded_string() {
        let card = json!({ "body_json": "\"plain text body\"" });
        assert_eq!(body_text(&card), "plain text body");
    }

    #[test]
    fn body_text_pretty_prints_a_json_encoded_object() {
        let card = json!({ "body_json": "{\"a\":1}" });
        assert_eq!(body_text(&card), "{\n  \"a\": 1\n}");
    }

    #[test]
    fn body_text_defaults_to_empty_when_absent_or_null() {
        assert_eq!(body_text(&json!({})), "");
        assert_eq!(body_text(&json!({ "body_json": Value::Null })), "");
    }

    #[test]
    fn body_text_falls_back_to_raw_string_on_parse_failure() {
        let card = json!({ "body_json": "not json {" });
        assert_eq!(body_text(&card), "not json {");
    }
}
