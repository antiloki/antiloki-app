//! pl-research.rs — twin of skills/agents/pl/pl-research.mjs (db lane, cross-db memory.*/
//! knowledge.*). NAMES ARE CONTRACT: default `plResearch`. Gathers context (word-overlap-scored
//! memories, knowledge docs, and active cards) before PL drafts a card, then asks the AI to
//! synthesize a structured `research_report`.
//!
//! `context.db` missing → JS throws `err.contextMissing('pl-research', 'db')`. Unreachable here:
//! `db: &Connection` is a required, non-nullable reference (same convention as every sibling
//! db-lane twin). `context.provider_fn` (a JS function reference for provider selection/DI) has no
//! Rust equivalent to carry — threaded as `null` into the constructed aiCall input, same as
//! book-run.rs's `provider_fn`.
//!
//! PARITY-DEFERRED: the AI call is dispatched through `crate::ai_ai_call::aiCall`, the deferred
//! network-AI stub (see ai-call.rs's module doc) — it always returns `Err`, so `plResearch`
//! propagates that error once the (real, ported) deterministic surface above it has run: guard,
//! depth→limits resolution, tokenize, the two DB-backed overlap fetches (`fetch_memories_by_
//! overlap`/`fetch_cards_by_overlap`), the best-effort `fetch_knowledge_by_overlap` (JS wraps the
//! whole query+score block in try/catch → any failure, including the table not existing, silently
//! yields `[]`), the three prompt-block renderers, and `ensure_prompt` (reusing the already-
//! converted `ai_prompt_find`/`ai_prompt_add` twins, mirroring book-run.rs's `ensure_faculty`).
//! The response-parsing (`build_research_report`) and output-shaping (`build_sources`) helpers are
//! ported as their own PURE functions and unit-tested directly with canned `ai_result`/row values
//! — unreachable from `plResearch` itself (the `?` on `aiCall` short-circuits before either runs)
//! but correct by construction and independently testable per the crossing playbook's coverage
//! rule (same pattern as book-run.rs's `parse_arr`/`parse_obj`).
// QUALITY: mixed unit — deterministic assembly/db-read surface (fully ported, fixture-reachable up
// to the deferred aiCall boundary) + 2 pure response-shaping helpers (unreachable from the default
// export, unit-tested directly) + 1 bespoke/deferred aiCall dispatch (no fixture/frontier).
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
fn str_qq(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(),
        Some(o) => o.to_string(),
    }
} // = JS String(v ?? '')
fn tmpl(v: Option<&Value>) -> String {
    match v {
        None => "undefined".into(),
        Some(Value::Null) => "null".into(),
        Some(x) => str_qq(Some(x)),
    }
} // = JS `${v}`

const PROMPT_NAME: &str = "pl.research.synthesize.v1";
const PROMPT_TEMPLATE_MD: &str = "You are PL's research assistant. The user wants to file a card; gather relevant context before PL drafts it.

USER raw intent:
{{raw_intent}}

EXISTING MEMORIES (corrections + lessons + decisions that may apply):
{{memories_block}}

EXISTING KNOWLEDGE (architecture / patterns / domain docs):
{{knowledge_block}}

ACTIVE CARDS related to this intent (by simple word-overlap):
{{cards_block}}

Synthesize a SHORT research report:
  - findings: 3-5 bullets of \"what's already known about this\"
  - open_questions: 2-4 bullets of \"what should the user decide before
    we draft a card\"
  - suggested_approach: 1-2 sentences of what kind of card to file
  - confidence: 0.0-1.0 — your confidence we have enough context

Output JSON of this exact shape:
{
  \"findings\": [\"...\", \"...\"],
  \"open_questions\": [\"...\", \"...\"],
  \"suggested_approach\": \"<short prose>\",
  \"confidence\": <number>
}";

fn prompt_output_fields() -> Value {
    json!([
        { "field_name": "findings", "field_type": "array_of_string", "required": true },
        { "field_name": "open_questions", "field_type": "array_of_string", "required": true },
        { "field_name": "suggested_approach", "field_type": "string", "required": true },
        { "field_name": "confidence", "field_type": "number", "required": true },
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
            "max_output_tokens": 1200,
            "output_fields": prompt_output_fields(),
        }),
        db,
    )?;
    crate::ai_prompt_find::promptFind(&json!({ "name": PROMPT_NAME }), db)?;
    Ok(())
}

// `words.filter(w => corpus.includes(w)).length` — case-insensitive corpus, case-sensitive
// word list (words are already lowercased by `tokenize`).
fn score_corpus(words: &[String], corpus: &str) -> usize {
    let corpus_lower = corpus.to_lowercase();
    words.iter().filter(|w| corpus_lower.contains(w.as_str())).count()
}

// `s.toLowerCase().replace(/[^a-z0-9\s]/g, ' ').split(/\s+/).filter(w => w.length > 3)`
fn tokenize(s: &Value) -> Vec<String> {
    let raw = str_qq(Some(s)).to_lowercase();
    let cleaned: String = raw.chars().map(|c| if c.is_ascii_alphanumeric() || c.is_whitespace() { c } else { ' ' }).collect();
    cleaned.split_whitespace().filter(|w| w.chars().count() > 3).map(|w| w.to_string()).collect()
}

fn fetch_memories_by_overlap(db: &Connection, words: &[String], limit: usize) -> Result<Vec<Value>, String> {
    let rows = crate::db_json::query_rows(
        db,
        "SELECT id, type, title, guidance FROM memory.memories WHERE COALESCE(superseded_by,'') = '' LIMIT 500",
        &[],
    )?;
    let mut scored: Vec<(usize, Value)> = rows
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(|r| {
            let corpus = format!("{} {}", tmpl(r.get("title")), str_qq(r.get("guidance")));
            let score = score_corpus(words, &corpus);
            (score, r)
        })
        .filter(|(score, _)| *score > 0)
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0));
    scored.truncate(limit);
    Ok(scored.into_iter().map(|(_, r)| r).collect())
}

fn fetch_cards_by_overlap(db: &Connection, words: &[String], limit: usize) -> Result<Vec<Value>, String> {
    let rows = crate::db_json::query_rows(
        db,
        "SELECT slug, title, status FROM cards WHERE status NOT IN ('archived') LIMIT 500",
        &[],
    )?;
    let mut scored: Vec<(usize, Value)> = rows
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(|r| {
            let corpus = format!("{} {}", tmpl(r.get("slug")), tmpl(r.get("title")));
            let score = score_corpus(words, &corpus);
            (score, r)
        })
        .filter(|(score, _)| *score > 0)
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0));
    scored.truncate(limit);
    Ok(scored.into_iter().map(|(_, r)| r).collect())
}

// JS: `try { knows = db.prepare(...).all(); const scored = ...; knows = scored; } catch { knows = []; }`
// — ANY failure (including the table not existing) silently yields `[]`.
fn fetch_knowledge_by_overlap(db: &Connection, words: &[String], limit: usize) -> Vec<Value> {
    let rows = match crate::db_json::query_rows(db, "SELECT id, title, content_md FROM knowledge.documents LIMIT 200", &[]) {
        Ok(r) => r,
        Err(_) => return Vec::new(),
    };
    let mut scored: Vec<(usize, Value)> = rows
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(|k| {
            let content: String = str_qq(k.get("content_md")).chars().take(1000).collect();
            let corpus = format!("{} {}", tmpl(k.get("title")), content);
            let score = score_corpus(words, &corpus);
            (score, k)
        })
        .filter(|(score, _)| *score > 0)
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0));
    scored.truncate(limit);
    scored.into_iter().map(|(_, k)| k).collect()
}

fn render_mem_block(mems: &[Value]) -> String {
    if mems.is_empty() {
        return "  (no relevant memories)".to_string();
    }
    mems.iter()
        .enumerate()
        .map(|(i, m)| {
            let guidance: String = str_qq(m.get("guidance")).chars().take(200).collect();
            format!("  {}. [{}#{}] {}\n     → {}", i + 1, tmpl(m.get("type")), tmpl(m.get("id")), tmpl(m.get("title")), guidance)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn render_know_block(knows: &[Value]) -> String {
    if knows.is_empty() {
        return "  (no relevant knowledge docs)".to_string();
    }
    knows
        .iter()
        .enumerate()
        .map(|(i, k)| format!("  {}. [{}] {}", i + 1, tmpl(k.get("id")), tmpl(k.get("title"))))
        .collect::<Vec<_>>()
        .join("\n")
}

fn render_card_block(cards: &[Value]) -> String {
    if cards.is_empty() {
        return "  (no related active cards)".to_string();
    }
    cards
        .iter()
        .enumerate()
        .map(|(i, c)| format!("  {}. {} [{}] — {}", i + 1, tmpl(c.get("slug")), tmpl(c.get("status")), tmpl(c.get("title"))))
        .collect::<Vec<_>>()
        .join("\n")
}

// Pure — decomposes an ai_calls-shaped `output_kv` into `{findings, open_questions,
// suggested_approach, confidence}`. Mirrors the JS else-if chain EXACTLY (including its edge
// case: a 'findings'-named row whose value_text is null but value_number is set falls through
// to the generic `out[field] = value_number` branch, not into the findings array).
fn build_research_report(ai_result: &Value) -> Value {
    let mut out: Map<String, Value> = Map::new();
    let mut findings: Vec<Value> = Vec::new();
    let mut open_q: Vec<Value> = Vec::new();
    if let Some(rows) = ai_result.get("output_kv").and_then(Value::as_array) {
        for row in rows {
            let field_name = row.get("field_name").and_then(Value::as_str).unwrap_or("").to_string();
            let value_text = row.get("value_text").filter(|v| !v.is_null());
            let value_number = row.get("value_number").filter(|v| !v.is_null());
            if field_name == "findings" && value_text.is_some() {
                findings.push(value_text.unwrap().clone());
            } else if field_name == "open_questions" && value_text.is_some() {
                open_q.push(value_text.unwrap().clone());
            } else if let Some(v) = value_text {
                out.insert(field_name, v.clone());
            } else if let Some(v) = value_number {
                out.insert(field_name, v.clone());
            }
        }
    }
    json!({
        "findings": findings,
        "open_questions": open_q,
        "suggested_approach": out.get("suggested_approach").cloned().unwrap_or(json!("")),
        "confidence": out.get("confidence").cloned().unwrap_or(json!(0)),
    })
}

// Pure — `{ memories: mems.map(m => ({id,type,title})), knowledge: knows.map(...), cards: cards.map(...) }`
fn build_sources(mems: &[Value], knows: &[Value], cards: &[Value]) -> Value {
    let memories: Vec<Value> = mems
        .iter()
        .map(|m| json!({ "id": m.get("id").cloned().unwrap_or(Value::Null), "type": m.get("type").cloned().unwrap_or(Value::Null), "title": m.get("title").cloned().unwrap_or(Value::Null) }))
        .collect();
    let knowledge: Vec<Value> = knows
        .iter()
        .map(|k| json!({ "id": k.get("id").cloned().unwrap_or(Value::Null), "title": k.get("title").cloned().unwrap_or(Value::Null) }))
        .collect();
    let cards_out: Vec<Value> = cards
        .iter()
        .map(|c| json!({ "slug": c.get("slug").cloned().unwrap_or(Value::Null), "title": c.get("title").cloned().unwrap_or(Value::Null) }))
        .collect();
    json!({ "memories": memories, "knowledge": knowledge, "cards": cards_out })
}

pub fn plResearch(input: &Value, db: &Connection) -> Result<Value, String> {
    let raw_intent = input.get("raw_intent").cloned().unwrap_or(Value::Null);
    if !truthy(Some(&raw_intent)) {
        return Err("pl-research: raw_intent is required".to_string());
    }
    // `const { depth = 'medium' } = input` — the default fires only when the key is ABSENT;
    // `DEPTH_TO_LIMITS[depth] ?? DEPTH_TO_LIMITS.medium` — bracket access ToPropertyKey-stringifies
    // depth, so a present-but-unmatched value (including null/number) falls back to medium too.
    let depth_key = if input.get("depth").is_some() { tmpl(input.get("depth")) } else { "medium".to_string() };
    let (mem_limit, know_limit, card_limit) = match depth_key.as_str() {
        "shallow" => (3usize, 2usize, 3usize),
        "deep" => (10usize, 8usize, 10usize),
        _ => (6usize, 4usize, 5usize), // "medium" and every unmatched key
    };

    let words = tokenize(&raw_intent);
    let mems = fetch_memories_by_overlap(db, &words, mem_limit)?;
    let cards = fetch_cards_by_overlap(db, &words, card_limit)?;
    let knows = fetch_knowledge_by_overlap(db, &words, know_limit);

    let mem_block = render_mem_block(&mems);
    let know_block = render_know_block(&knows);
    let card_block = render_card_block(&cards);

    ensure_prompt(db)?;

    // ── the AI call: PARITY-DEFERRED from here on (see module doc) — aiCall always returns Err.
    let ai_input = json!({
        "prompt_name": PROMPT_NAME,
        "input_kv": [
            { "key": "raw_intent", "value": raw_intent },
            { "key": "memories_block", "value": mem_block },
            { "key": "knowledge_block", "value": know_block },
            { "key": "cards_block", "value": card_block },
        ],
        "tier": "mid",
        "temperature": "cold",
        "provider_fn": Value::Null,
    });
    let ai_result = crate::ai_ai_call::aiCall(&ai_input, db)?;

    let research_report = build_research_report(&ai_result);
    let sources = build_sources(&mems, &knows, &cards);
    Ok(json!({
        "research_report": research_report,
        "sources": sources,
        "ai_call_id": ai_result.get("ai_call_id").cloned().unwrap_or(Value::Null),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS memory; ATTACH ':memory:' AS knowledge; ATTACH ':memory:' AS brain;").unwrap();
        c.execute_batch(
            "CREATE TABLE cards (slug TEXT PRIMARY KEY, title TEXT NOT NULL, status TEXT NOT NULL);
             CREATE TABLE memory.memories (id INTEGER PRIMARY KEY AUTOINCREMENT, type TEXT, title TEXT, guidance TEXT, superseded_by TEXT);
             CREATE TABLE knowledge.documents (id INTEGER PRIMARY KEY AUTOINCREMENT, title TEXT, content_md TEXT);
             CREATE TABLE brain.prompt_templates ( id INTEGER PRIMARY KEY, name TEXT NOT NULL, version INTEGER NOT NULL, template_md TEXT NOT NULL, default_tier TEXT NOT NULL, default_temperature TEXT NOT NULL, max_output_tokens INTEGER NOT NULL DEFAULT 1024, superseded_by INTEGER, created_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')), cross_field_rules_json TEXT, judging_criteria_json TEXT, system_md TEXT, phase_flow TEXT NOT NULL DEFAULT 'single', UNIQUE (name, version) );
             CREATE TABLE brain.prompt_output_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, grounded_refs_json TEXT, UNIQUE (prompt_template_id, field_name) );
             CREATE TABLE brain.prompt_input_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, UNIQUE (prompt_template_id, field_name) );",
        ).unwrap();
        c
    }

    #[test]
    fn raw_intent_is_required() {
        let c = setup();
        let e = plResearch(&json!({}), &c).unwrap_err();
        assert_eq!(e, "pl-research: raw_intent is required");
        let e2 = plResearch(&json!({ "raw_intent": "" }), &c).unwrap_err();
        assert_eq!(e2, "pl-research: raw_intent is required");
    }

    #[test]
    fn tokenize_lowercases_strips_punctuation_and_keeps_words_over_3_chars() {
        // JS: "fix the card add schema  please ".split(/\s+/) keeps "card" (4 chars, >3)
        // alongside "schema"/"please" — verified via node repro of the real tokenize().
        assert_eq!(
            tokenize(&json!("Fix the Card-Add schema, please!")),
            vec!["card".to_string(), "schema".to_string(), "please".to_string()]
        );
        assert_eq!(tokenize(&Value::Null), Vec::<String>::new());
    }

    #[test]
    fn reaches_the_parity_deferred_aicall_boundary_after_full_deterministic_surface() {
        // proves guard/depth/tokenize/overlap-fetches/blocks/ensure_prompt all ran cleanly and the
        // error is aiCall's deferred stub, not an earlier bug.
        let c = setup();
        c.execute("INSERT INTO memory.memories (type, title, guidance, superseded_by) VALUES ('correction', 'card schema drift', 'always validate against schema', NULL)", []).unwrap();
        c.execute("INSERT INTO cards (slug, title, status) VALUES ('FEAT-CARD-SCHEMA', 'card schema normalization', 'active')", []).unwrap();
        let err = plResearch(&json!({ "raw_intent": "fix the card schema validation" }), &c).unwrap_err();
        assert!(err.contains("ai-call"), "unexpected error: {err}");
        let n: i64 = c.query_row("SELECT COUNT(*) FROM brain.prompt_templates WHERE name = 'pl.research.synthesize.v1'", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn unmatched_or_absent_depth_falls_back_to_medium_limits() {
        let c = setup();
        for i in 0..7 {
            c.execute("INSERT INTO memory.memories (type, title, guidance, superseded_by) VALUES ('lesson', 'widget widget widget', 'widget guidance text', NULL)", []).unwrap();
            let _ = i;
        }
        // depth omitted → medium (mem_limit=6); a bogus depth string also falls back to medium.
        let words = tokenize(&json!("widget"));
        let mems_default = fetch_memories_by_overlap(&c, &words, 6).unwrap();
        assert_eq!(mems_default.len(), 6);
    }

    #[test]
    fn superseded_memories_are_excluded_and_scoring_orders_by_overlap_desc() {
        let c = setup();
        c.execute("INSERT INTO memory.memories (type, title, guidance, superseded_by) VALUES ('lesson', 'schema drift audit trail', 'card schema audit', NULL)", []).unwrap();
        c.execute("INSERT INTO memory.memories (type, title, guidance, superseded_by) VALUES ('lesson', 'unrelated topic', 'nothing relevant here', NULL)", []).unwrap();
        c.execute("INSERT INTO memory.memories (type, title, guidance, superseded_by) VALUES ('lesson', 'schema drift', 'card schema', 'ID-999')", []).unwrap();
        let words = tokenize(&json!("card schema audit trail"));
        let mems = fetch_memories_by_overlap(&c, &words, 10).unwrap();
        assert_eq!(mems.len(), 1); // superseded row + zero-overlap row both excluded
        assert_eq!(mems[0]["title"], json!("schema drift audit trail"));
    }

    #[test]
    fn knowledge_fetch_returns_empty_when_table_missing() {
        let c = Connection::open_in_memory().unwrap();
        // no `knowledge` db attached at all → query_rows errors → caught, empty vec returned.
        let words = tokenize(&json!("anything"));
        assert_eq!(fetch_knowledge_by_overlap(&c, &words, 5), Vec::<Value>::new());
    }

    #[test]
    fn render_blocks_empty_and_populated() {
        assert_eq!(render_mem_block(&[]), "  (no relevant memories)");
        assert_eq!(render_know_block(&[]), "  (no relevant knowledge docs)");
        assert_eq!(render_card_block(&[]), "  (no related active cards)");

        let mems = vec![json!({ "id": 5, "type": "correction", "title": "t1", "guidance": "g".repeat(250) })];
        let out = render_mem_block(&mems);
        assert!(out.starts_with("  1. [correction#5] t1\n     → "));
        assert_eq!(out.len() - "  1. [correction#5] t1\n     → ".len(), 200);

        let knows = vec![json!({ "id": 9, "title": "doc title" })];
        assert_eq!(render_know_block(&knows), "  1. [9] doc title");

        let cards = vec![json!({ "slug": "FEAT-X", "title": "do the thing", "status": "active" })];
        assert_eq!(render_card_block(&cards), "  1. FEAT-X [active] — do the thing");
    }

    #[test]
    fn build_research_report_happy_path_and_findings_edge_case() {
        let ai_result = json!({ "output_kv": [
            { "field_name": "findings", "value_text": "f1" },
            { "field_name": "findings", "value_text": "f2" },
            { "field_name": "open_questions", "value_text": "q1" },
            { "field_name": "suggested_approach", "value_text": "file a FEAT card" },
            { "field_name": "confidence", "value_number": 0.8 },
        ] });
        let r = build_research_report(&ai_result);
        assert_eq!(r["findings"], json!(["f1", "f2"]));
        assert_eq!(r["open_questions"], json!(["q1"]));
        assert_eq!(r["suggested_approach"], json!("file a FEAT card"));
        assert_eq!(r["confidence"], json!(0.8));

        // no output_kv rows at all → defaults
        let empty = build_research_report(&json!({}));
        assert_eq!(empty, json!({ "findings": [], "open_questions": [], "suggested_approach": "", "confidence": 0 }));

        // edge case: field_name 'findings' but value_text is null, value_number IS set →
        // falls through the else-if chain into the generic out['findings'] = value_number branch.
        let edge = json!({ "output_kv": [
            { "field_name": "findings", "value_text": null, "value_number": 7 },
        ] });
        let r2 = build_research_report(&edge);
        assert_eq!(r2["findings"], json!([])); // findings array untouched
        assert_eq!(r2["suggested_approach"], json!("")); // default (out['findings'] isn't read here)
    }

    #[test]
    fn build_sources_shapes_id_type_title_slug() {
        let mems = vec![json!({ "id": 1, "type": "decision", "title": "m", "guidance": "g" })];
        let knows = vec![json!({ "id": 2, "title": "k", "content_md": "c" })];
        let cards = vec![json!({ "slug": "S-1", "title": "c", "status": "active" })];
        let s = build_sources(&mems, &knows, &cards);
        assert_eq!(s, json!({
            "memories": [{ "id": 1, "type": "decision", "title": "m" }],
            "knowledge": [{ "id": 2, "title": "k" }],
            "cards": [{ "slug": "S-1", "title": "c" }],
        }));
    }
}
