//! card-digest.rs — twin of skills/entities/card-digest.mjs (db, brain-attached via prompt-find/
//! prompt-add). NAMES ARE CONTRACT: default `cardDigest`, named `fetchCardsForDigest`,
//! `renderCardsBlock`. Fetches all cards (slug/title/status/body) ordered by status priority
//! (doing→impl→qa→backlog→done→blocked→archived→other), renders them into a plain-text block, and
//! asks the AI for a short natural-language digest — auto-registering the
//! 'board-keeper.card.digest' prompt on first use.
//!
//! PARITY-DEFERRED: `cardDigest`'s only AI step is `crate::ai_ai_call::aiCall`, which always
//! returns `Err` (see ai-call.rs's module doc). The JS `await`s it directly with no try/catch, so
//! its Err propagates straight out of `cardDigest` — mirrored here with `?`. Everything BEFORE that
//! call (prompt ensure via the already-converted `ai_prompt_find`/`ai_prompt_add` twins, the card
//! fetch + body-shaping + status ordering, the cards-block render) is ported faithfully and IS the
//! deterministic surface these tests exercise. The output-shaping code after the `aiCall` line is
//! transcribed byte-for-byte from the JS for inspection but is unreachable by this file's own
//! fixture, exactly like book-run.rs's post-wave logic.
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
fn str_qq(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(),
        Some(o) => o.to_string(),
    }
}

const PROMPT_NAME: &str = "board-keeper.card.digest";

const PROMPT_TEMPLATE_MD: &str = "You are summarizing an antiloki project board. Below are all current cards with titles, statuses, and bodies. Produce a SHORT, natural-language digest the user can read at a glance.

Rules:
  - Total length: 4–10 lines max.
  - Structure: brief framing line, then bullet points grouped by what's actively happening (doing / impl / qa) vs. what's queued (backlog).
  - Skip done/archived unless their absence would mislead.
  - Don't list every card — group them when titles share themes.
  - Don't include slugs unless picking out one specific card.
  - Plain prose, no markdown headers.

CARDS:
{{cards}}

Output JSON of this exact shape:
{
  \"digest_md\": \"<the digest, plain prose>\"
}";

fn prompt_output_fields() -> Value {
    json!([{ "field_name": "digest_md", "field_type": "string", "required": true }])
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
            "max_output_tokens": 800,
            "output_fields": prompt_output_fields(),
        }),
        db,
    )?;
    crate::ai_prompt_find::promptFind(&json!({ "name": PROMPT_NAME }), db)?;
    Ok(())
}

// `COALESCE(body_json,'')` → try JSON.parse; parsed string kept as-is, anything else
// re-stringified; parse failure falls back to the raw column text. Hard cap 240 chars/body.
fn shape_body(body_raw: &str) -> String {
    if body_raw.is_empty() {
        return String::new();
    }
    let body = match serde_json::from_str::<Value>(body_raw) {
        Ok(Value::String(s)) => s,
        Ok(other) => serde_json::to_string(&other).unwrap_or_default(),
        Err(_) => body_raw.to_string(),
    };
    if body.chars().count() > 240 {
        let truncated: String = body.chars().take(237).collect();
        format!("{truncated}…")
    } else {
        body
    }
}

pub fn fetchCardsForDigest(db: &Connection) -> Result<Value, String> {
    let rows = crate::db_json::query_rows(
        db,
        "SELECT slug, title, status, COALESCE(body_json, '') AS body_raw
         FROM cards
         ORDER BY
           CASE status
             WHEN 'doing'    THEN 0
             WHEN 'impl'     THEN 1
             WHEN 'qa'       THEN 2
             WHEN 'backlog'  THEN 3
             WHEN 'done'     THEN 4
             WHEN 'blocked'  THEN 5
             WHEN 'archived' THEN 6
             ELSE 9
           END,
           slug",
        &[],
    )?;
    let cards: Vec<Value> = rows
        .as_array()
        .map(|a| {
            a.iter()
                .map(|r| {
                    let body_raw = js_string(r.get("body_raw").unwrap_or(&Value::Null));
                    json!({
                        "slug": r.get("slug").cloned().unwrap_or(Value::Null),
                        "title": r.get("title").cloned().unwrap_or(Value::Null),
                        "status": r.get("status").cloned().unwrap_or(Value::Null),
                        "body": shape_body(&body_raw),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    Ok(json!(cards))
}

pub fn renderCardsBlock(cards: &Value) -> String {
    let arr: Vec<Value> = cards.as_array().cloned().unwrap_or_default();
    if arr.is_empty() {
        return "  (no cards yet — the board is empty)".to_string();
    }
    arr.iter()
        .map(|c| {
            let status = str_qq(c.get("status"));
            let slug = str_qq(c.get("slug"));
            let title = str_qq(c.get("title"));
            let head = format!("[{status}] {slug} — {title}");
            let body = c.get("body").cloned().unwrap_or(Value::Null);
            if truthy(Some(&body)) {
                format!("{head}\n    body: {}", str_qq(Some(&body)))
            } else {
                head
            }
        })
        .collect::<Vec<String>>()
        .join("\n")
}

// { field_name: value_text } built from an ai_calls response's output_kv rows.
fn kv_out(res: &Value) -> Value {
    let mut o = Map::new();
    if let Some(arr) = res.get("output_kv").and_then(|v| v.as_array()) {
        for row in arr {
            if let Some(field) = row.get("field_name").and_then(|v| v.as_str()) {
                if let Some(val) = row.get("value_text") {
                    if !val.is_null() {
                        o.insert(field.to_string(), val.clone());
                    }
                }
            }
        }
    }
    Value::Object(o)
}

pub fn cardDigest(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS: `const { db, provider_fn = null } = context ?? {}; if (!db) throw err.contextMissing(...)`
    // — unreachable here (Rust's `db: &Connection` is never absent). provider_fn is a JS function
    // reference threaded opaquely into aiCall's input for provider selection/DI — no Rust
    // equivalent to carry (aiCall's dispatch itself is the deferred boundary), so `null` here.
    let provider_fn = Value::Null;
    let _ = input; // JS never reads any field off `input`

    ensure_prompt(db)?;

    let cards = fetchCardsForDigest(db)?;
    let cards_block = renderCardsBlock(&cards);

    // ── PARITY-DEFERRED from here: aiCall always returns Err (see module doc); the JS awaits it
    // with no try/catch, so its Err propagates straight out via `?`.
    let ai_result = crate::ai_ai_call::aiCall(
        &json!({
            "prompt_name": PROMPT_NAME,
            "input_kv": [{ "key": "cards", "value": cards_block }],
            "provider_fn": provider_fn,
        }),
        db,
    )?;

    let outputs = kv_out(&ai_result);
    let digest_md = outputs
        .get("digest_md")
        .cloned()
        .unwrap_or_else(|| json!("(digest unavailable)"));
    let cards_considered = cards.as_array().map(|a| a.len()).unwrap_or(0) as i64;

    Ok(json!({
        "digest_md": digest_md,
        "cards_considered": cards_considered,
        "cost_usd": ai_result.get("cost_usd").cloned().unwrap_or(json!(0)),
        "cached": ai_result.get("cached").cloned().unwrap_or(json!(false)),
        "ai_call_id": ai_result.get("ai_call_id").cloned().unwrap_or(Value::Null),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    const BRAIN_SCHEMA: &str = "
        CREATE TABLE brain.prompt_templates ( id INTEGER PRIMARY KEY, name TEXT NOT NULL, version INTEGER NOT NULL, template_md TEXT NOT NULL, default_tier TEXT NOT NULL, default_temperature TEXT NOT NULL, max_output_tokens INTEGER NOT NULL DEFAULT 1024, superseded_by INTEGER, created_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')), cross_field_rules_json TEXT, judging_criteria_json TEXT, system_md TEXT, phase_flow TEXT NOT NULL DEFAULT 'single', UNIQUE (name, version) );
        CREATE TABLE brain.prompt_output_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, grounded_refs_json TEXT, UNIQUE (prompt_template_id, field_name) );
        CREATE TABLE brain.prompt_input_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, UNIQUE (prompt_template_id, field_name) );
    ";

    fn setup() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS brain").unwrap();
        c.execute_batch(BRAIN_SCHEMA).unwrap();
        c.execute_batch(
            "CREATE TABLE cards (id INTEGER PRIMARY KEY, slug TEXT UNIQUE, title TEXT, status TEXT, body_json TEXT)",
        )
        .unwrap();
        c
    }

    fn insert_card(c: &Connection, slug: &str, title: &str, status: &str, body_json: Option<&str>) {
        c.execute(
            "INSERT INTO cards (slug, title, status, body_json) VALUES (?, ?, ?, ?)",
            rusqlite::params![slug, title, status, body_json],
        )
        .unwrap();
    }

    #[test]
    fn fetch_cards_orders_by_status_and_truncates_body() {
        let c = setup();
        insert_card(&c, "A-1", "first thing", "doing", Some(&serde_json::to_string("short body").unwrap()));
        let long = "b".repeat(300);
        insert_card(&c, "B-2", "queued thing", "backlog", Some(&serde_json::to_string(&long).unwrap()));
        insert_card(&c, "C-3", "past thing", "done", None);

        let cards = fetchCardsForDigest(&c).unwrap();
        let arr = cards.as_array().unwrap();
        assert_eq!(arr.len(), 3);
        assert_eq!(arr[0]["slug"], json!("A-1"));
        assert_eq!(arr[1]["slug"], json!("B-2"));
        assert_eq!(arr[2]["slug"], json!("C-3"));
        let b1 = arr[1]["body"].as_str().unwrap();
        assert!(b1.chars().count() <= 240);
        assert!(b1.ends_with('…'));
        assert_eq!(arr[2]["body"], json!(""));
    }

    #[test]
    fn fetch_cards_stringifies_non_string_json_bodies() {
        let c = setup();
        insert_card(&c, "D-4", "obj body", "impl", Some(r#"{"a":1,"b":"x"}"#));
        insert_card(&c, "E-5", "unparseable body", "qa", Some("not json {"));
        let cards = fetchCardsForDigest(&c).unwrap();
        let arr = cards.as_array().unwrap();
        // impl (1) sorts before qa (2)
        assert_eq!(arr[0]["slug"], json!("D-4"));
        assert_eq!(arr[0]["body"], json!(r#"{"a":1,"b":"x"}"#));
        assert_eq!(arr[1]["slug"], json!("E-5"));
        assert_eq!(arr[1]["body"], json!("not json {"));
    }

    #[test]
    fn render_cards_block_formats_one_line_per_card_and_indents_body() {
        let block = renderCardsBlock(&json!([
            { "slug": "A-1", "title": "first", "status": "doing", "body": "tiny body" },
            { "slug": "B-2", "title": "no body", "status": "backlog", "body": "" },
        ]));
        assert!(block.contains("[doing] A-1 — first"));
        assert!(block.contains("body: tiny body"));
        assert!(block.contains("[backlog] B-2 — no body"));
        let body_lines = block.lines().filter(|l| l.starts_with("    body:")).count();
        assert_eq!(body_lines, 1);
    }

    #[test]
    fn render_cards_block_handles_empty_board() {
        assert!(renderCardsBlock(&json!([])).contains("no cards yet"));
    }

    #[test]
    fn card_digest_reaches_the_parity_deferred_aicall_boundary() {
        // proves the deterministic gates (prompt-ensure, card fetch, cards-block render) all ran
        // cleanly and the error is aiCall's deferred stub, not an earlier bug.
        let c = setup();
        insert_card(&c, "A-1", "do something", "doing", None);
        insert_card(&c, "B-2", "queued", "backlog", None);
        let err = cardDigest(&json!({}), &c).unwrap_err();
        assert!(err.contains("ai-call"), "unexpected error: {err}");
        // the prompt WAS ensured (ensure_prompt ran) before the deferred call
        let n: i64 = c
            .query_row("SELECT COUNT(*) FROM brain.prompt_templates WHERE name = 'board-keeper.card.digest'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
        let fields: i64 = c
            .query_row("SELECT COUNT(*) FROM brain.prompt_output_fields WHERE prompt_template_id = (SELECT id FROM brain.prompt_templates WHERE name = 'board-keeper.card.digest')", [], |r| r.get(0))
            .unwrap();
        assert_eq!(fields, 1);
    }

    #[test]
    fn card_digest_works_on_empty_board() {
        let c = setup();
        let err = cardDigest(&json!({}), &c).unwrap_err();
        assert!(err.contains("ai-call"));
    }
}
