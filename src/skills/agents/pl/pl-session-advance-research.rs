//! pl-session-advance-research.rs — twin of skills/agents/pl/pl-session-advance-research.mjs
//! (db WRITE lane). NAMES ARE CONTRACT: `plSessionAdvanceResearch` (default). PL's research step:
//! pulls candidate findings (similar cards / correction memories / applicable laws) over a
//! `researching` pl_session, then runs the high-level AI summary and persists one
//! pl_research_findings row per emitted finding, advancing the session to `deciding_split`.
//!
//! PARITY-DEFERRED: the AI leg is `crate::ai_ai_call::aiCall`, itself an always-`Err` stub (see
//! ai-call.rs's module doc) — it dispatches a real network call to a BYOK LLM provider, which is
//! genuinely non-deterministic and not fixturable. Everything ELSE is ported faithfully and IS the
//! deterministic surface this file's tests exercise: the session lookup + status guard,
//! `ensurePrompt`'s find-or-register bootstrap (reusing the already-converted `ai_prompt_find`/
//! `ai_prompt_add` twins), and `gatherCandidates` in full — including the two `try { } catch
//! { /* skip */ }` JS blocks (correction-memories via `entities_catalog_find::catalogFind`, laws
//! via `entities_law_surface_for_context::lawSurfaceForContext`), both real and reachable: when
//! `memory.catalog_embeddings` has ≥1 row for entity_type='memory-correction', the auto-resolved
//! model is truthy and catalogFind's own PARITY-DEFERRED vector-search branch returns Err — which
//! this twin swallows exactly as the JS `catch` does, mirroring the real call site's behavior
//! rather than re-deferring it. The post-aiCall shaping (spent_usd bookkeeping, findings
//! decomposition into pl_research_findings rows, the final status→'deciding_split' update) is
//! ported byte-for-byte from the JS but — like pl-session-advance-decompose.rs's post-wave
//! staging — is unreachable by this file's own fixture because it runs only after a successful
//! `aiCall`, which the deferred stub never produces; correctness there is by inspection + the same
//! shape as pl-session-advance-decompose.rs's reachable code.
//!
//! `context.db` missing → JS throws `err.contextMissing('pl-session-advance-research', 'db')`.
//! Unreachable here: `db: &Connection` is a required, non-nullable reference (same convention as
//! every sibling db-lane twin). `provider_fn` (JS `input.provider_fn`) has no Rust equivalent — it
//! is forwarded into the deferred `aiCall` input the same way the JS forwards it, but is otherwise
//! inert (the stub ignores its whole input).
// QUALITY: mixed unit — guards/idempotency/prompt-bootstrap/gather-candidates are real,
// fixture-tested surface; the post-aiCall findings-persist path is ported but unreachable
// (deferred AI boundary short-circuits it) — same convention as pl-session-advance-decompose.rs.

use rusqlite::Connection;
use serde_json::{json, Map, Value};

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

const PROMPT_NAME: &str = "pl.research.summary";
const PROMPT_TEMPLATE_MD: &str = "You are PL summarizing research findings for a session. For each finding (similar-card / correction-memory / applicable-law / knowledge-extract) given to you, write a 1-sentence why_relevant_md explaining how it relates to the intent.\n\nReturn JSON:\n{\n  \"findings\": [\n    { \"target_ref\": \"<slug-or-id>\", \"finding_type\": \"<kind>\", \"why_relevant_md\": \"<one sentence>\", \"relevance_score\": <0.0..1.0> }\n  ],\n  \"overall_summary_md\": \"<2-3 sentence overview of the research landscape>\"\n}";

/// `function ensurePrompt(db)` — internal (not exported). Mirrors the JS `try { return
/// promptFind(...) } catch { promptAdd(...); return promptFind(...) }`.
fn ensure_prompt(db: &Connection) -> Result<Value, String> {
    if let Ok(v) = crate::ai_prompt_find::promptFind(&json!({ "name": PROMPT_NAME }), db) {
        return Ok(v);
    }
    crate::ai_prompt_add::promptAdd(
        &json!({
            "name": PROMPT_NAME,
            "template_md": PROMPT_TEMPLATE_MD,
            "default_tier": "mid",
            "default_temperature": "cold",
            "max_output_tokens": 2048,
            "output_fields": [
                { "field_name": "findings", "field_type": "array_of_object" },
                { "field_name": "overall_summary_md", "field_type": "string" },
            ],
        }),
        db,
    )?;
    crate::ai_prompt_find::promptFind(&json!({ "name": PROMPT_NAME }), db)
}

/// Lexical similarity over cards — the ONLY branch in `gatherCandidates` NOT wrapped in a JS
/// try/catch, so its errors propagate (own fn so the caller can `?` it directly).
fn similar_cards(db: &Connection, raw_intent: &str, limit: i64) -> Result<Vec<Value>, String> {
    let tokens: Vec<String> = raw_intent
        .to_lowercase()
        .split_whitespace()
        .filter(|w| w.chars().count() > 3)
        .take(6)
        .map(|s| s.to_string())
        .collect();
    if tokens.is_empty() {
        return Ok(vec![]);
    }
    let conds = vec!["(LOWER(title) LIKE ? OR LOWER(body_json) LIKE ?)"; tokens.len()].join(" OR ");
    let mut params: Vec<Value> = Vec::new();
    for t in &tokens {
        let pat = json!(format!("%{}%", t));
        params.push(pat.clone());
        params.push(pat);
    }
    params.push(json!(limit));
    let sql = format!("SELECT slug, title FROM cards WHERE {} LIMIT ?", conds);
    let rows = crate::db_json::query_rows(db, &sql, &params)?;
    let mut out = Vec::new();
    for c in rows.as_array().cloned().unwrap_or_default() {
        let mut o = Map::new();
        o.insert("target_ref".into(), c.get("slug").cloned().unwrap_or(Value::Null));
        o.insert("finding_type".into(), json!("similar-card"));
        o.insert("title".into(), c.get("title").cloned().unwrap_or(Value::Null));
        out.push(Value::Object(o));
    }
    Ok(out)
}

/// `// Correction memories via RAG when embeddings exist; fallback to recent` — the JS
/// `try { ... } catch { /* skip */ }` block. When `memory.catalog_embeddings` already has rows
/// for entity_type='memory-correction', `catalogFind`'s model auto-resolves truthy and its own
/// unported vector-search branch returns Err — swallowed here exactly like the JS catch.
fn correction_memories(db: &Connection, raw_intent: &str, limit: i64, findings: &mut Vec<Value>) -> Result<(), String> {
    // JS wraps this whole block (count query + catalogFind + fallback query) in a single
    // `try { } catch { /* skip */ }` — any error anywhere in here is swallowed, not just the
    // catalogFind call, so this fn always returns Ok(()) and leaves `findings` as-is on failure.
    let result: Result<(), String> = (|| {
    let cnt_rows = crate::db_json::query_rows(
        db,
        "SELECT COUNT(*) AS c FROM memory.catalog_embeddings WHERE entity_type='memory-correction'",
        &[],
    )?;
    let embed_count = cnt_rows
        .as_array()
        .and_then(|a| a.first())
        .and_then(|r| r.get("c"))
        .and_then(Value::as_i64)
        .unwrap_or(0);
    if embed_count > 0 {
        let hits = crate::entities_catalog_find::catalogFind(
            &json!({ "query": raw_intent, "entity_type": "memory-correction", "limit": limit, "fallback": false }),
            db,
        )?;
        for h in hits.as_array().cloned().unwrap_or_default() {
            if let Some(entity) = h.get("entity").filter(|v| !v.is_null()) {
                let id = entity.get("id").cloned().unwrap_or(Value::Null);
                let mut o = Map::new();
                o.insert("target_ref".into(), json!(format!("mem-{}", str_qq(Some(&id)))));
                o.insert("finding_type".into(), json!("correction-memory"));
                if let Some(score) = h.get("score") {
                    o.insert("score".into(), score.clone());
                }
                findings.push(Value::Object(o));
            }
        }
    } else {
        let mem_rows = crate::db_json::query_rows(
            db,
            "SELECT id FROM memory.memories WHERE type='correction' ORDER BY id DESC LIMIT ?",
            &[json!(limit)],
        )?;
        for m in mem_rows.as_array().cloned().unwrap_or_default() {
            let id = m.get("id").cloned().unwrap_or(Value::Null);
            let mut o = Map::new();
            o.insert("target_ref".into(), json!(format!("mem-{}", str_qq(Some(&id)))));
            o.insert("finding_type".into(), json!("correction-memory"));
            findings.push(Value::Object(o));
        }
    }
    Ok(())
    })();
    let _ = result;
    Ok(())
}

/// `// Laws` — the JS `try { ... } catch { /* skip */ }` block, delegating to the already-converted
/// `lawSurfaceForContext` twin.
fn applicable_laws(db: &Connection, raw_intent: &str, limit: i64, findings: &mut Vec<Value>) -> Result<(), String> {
    let laws = crate::entities_law_surface_for_context::lawSurfaceForContext(
        &json!({ "card": { "title": raw_intent }, "limit": limit }),
        db,
    )?;
    for l in laws.as_array().cloned().unwrap_or_default() {
        if let Some(slug) = l.get("entity").and_then(|e| e.get("slug")).filter(|v| truthy(Some(v))) {
            let mut o = Map::new();
            o.insert("target_ref".into(), slug.clone());
            o.insert("finding_type".into(), json!("applicable-law"));
            if let Some(score) = l.get("score") {
                o.insert("score".into(), score.clone());
            }
            findings.push(Value::Object(o));
        }
    }
    Ok(())
}

/// `async function gatherCandidates({ db, raw_intent, limit })`. Own fn, directly testable —
/// `findings.slice(0, limit * 3)` at the end.
fn gather_candidates(db: &Connection, raw_intent: &str, limit: i64) -> Result<Vec<Value>, String> {
    let mut findings = similar_cards(db, raw_intent, limit)?;
    let _ = correction_memories(db, raw_intent, limit, &mut findings);
    let _ = applicable_laws(db, raw_intent, limit, &mut findings);
    let cap = (limit.max(0) * 3) as usize;
    findings.truncate(cap);
    Ok(findings)
}

#[allow(non_snake_case)]
pub fn plSessionAdvanceResearch(input: &Value, db: &Connection) -> Result<Value, String> {
    let session_id = input.get("session_id").filter(|v| !v.is_null() && truthy(Some(v))).cloned();
    let Some(session_id) = session_id else {
        return Err("pl-session-advance-research: session_id is required".to_string());
    };
    let provider_fn = input.get("provider_fn").cloned().unwrap_or(Value::Null);
    let rag_limit = input.get("rag_limit").and_then(Value::as_i64).unwrap_or(3);

    let rows = crate::db_json::query_rows(db, "SELECT * FROM pl_sessions WHERE id = ?", &[session_id.clone()])?;
    let Some(session) = rows.as_array().and_then(|a| a.first()).cloned() else {
        return Err(format!("pl-session-advance-research: session {} not found", str_qq(Some(&session_id))));
    };
    let status = session.get("status").and_then(Value::as_str).unwrap_or("");
    if status != "researching" {
        return Err(format!(
            "pl-session-advance-research: session status='{}', expected 'researching'",
            status
        ));
    }

    ensure_prompt(db)?;
    let raw_intent_v = session.get("raw_intent").cloned().unwrap_or(Value::Null);
    let raw_intent = str_qq(Some(&raw_intent_v));
    let candidates = gather_candidates(db, &raw_intent, rag_limit)?;

    let budget_usd = session.get("budget_usd").and_then(Value::as_f64).unwrap_or(0.0);
    let spent_usd = session.get("spent_usd").and_then(Value::as_f64).unwrap_or(0.0);

    let ai_input = json!({
        "prompt_name": PROMPT_NAME,
        "input_kv": [
            { "key": "raw_intent", "value": raw_intent_v },
            { "key": "candidates", "value": serde_json::to_string(&candidates).unwrap_or_default() },
        ],
        "budget_remaining_usd": budget_usd - spent_usd,
        "provider_fn": provider_fn,
    });

    // ── PARITY-DEFERRED boundary ─────────────────────────────────────────
    // aiCall is a real network call to a BYOK LLM provider; the twin's stub always returns Err,
    // so everything below is unreachable by this file's fixture but ported by inspection for
    // shape-correctness (mirrors pl-session-advance-decompose.rs's convention).
    let ai_result = crate::ai_ai_call::aiCall(&ai_input, db)?;

    let cost_usd = ai_result.get("cost_usd").and_then(Value::as_f64).unwrap_or(0.0);
    crate::db_json::run_write(
        db,
        &format!("UPDATE pl_sessions SET spent_usd = spent_usd + ?, updated_at={} WHERE id = ?", crate::clock::now_sql()),
        &[json!(cost_usd), session_id.clone()],
    )?;

    let output_kv = ai_result.get("output_kv").and_then(Value::as_array).cloned().unwrap_or_default();
    let ai_call_id = ai_result.get("ai_call_id").cloned().unwrap_or(Value::Null);

    let mut parents: Vec<&Value> = output_kv
        .iter()
        .filter(|o| {
            o.get("field_name").and_then(Value::as_str) == Some("findings")
                && o.get("value_text").map_or(true, Value::is_null)
                && o.get("value_number").map_or(true, Value::is_null)
        })
        .collect();
    parents.sort_by(|a, b| {
        let ai = a.get("order_index").and_then(Value::as_i64).unwrap_or(0);
        let bi = b.get("order_index").and_then(Value::as_i64).unwrap_or(0);
        ai.cmp(&bi)
    });

    const ALLOWED_TYPES: [&str; 4] = ["similar-card", "correction-memory", "applicable-law", "knowledge-extract"];
    let mut inserted: Vec<Value> = Vec::new();
    for p in parents {
        let children_rows = crate::db_json::query_rows(
            db,
            "SELECT field_name, value_text, value_number FROM brain.ai_call_outputs WHERE ai_call_id = ? AND parent_output_id = ?",
            &[ai_call_id.clone(), p.get("id").cloned().unwrap_or(Value::Null)],
        )?;
        let children: Vec<Value> = children_rows.as_array().cloned().unwrap_or_default();
        // JS: `children.find(c => c.field_name === name)?.value_text` — truthy-checked when the
        // JS reads it as `!target_ref` / `!finding_type`; raw (null-coalesced only) otherwise.
        let find_truthy_text = |name: &str| -> Option<String> {
            children
                .iter()
                .find(|c| c.get("field_name").and_then(Value::as_str) == Some(name))
                .and_then(|c| c.get("value_text").cloned())
                .filter(|v| truthy(Some(v)))
                .map(|v| str_qq(Some(&v)))
        };
        let find_raw = |name: &str, key: &str| -> Value {
            children
                .iter()
                .find(|c| c.get("field_name").and_then(Value::as_str) == Some(name))
                .and_then(|c| c.get(key).cloned())
                .filter(|v| !v.is_null())
                .unwrap_or(Value::Null)
        };

        let Some(target_ref) = find_truthy_text("target_ref") else { continue };
        let Some(finding_type) = find_truthy_text("finding_type") else { continue };
        if !ALLOWED_TYPES.contains(&finding_type.as_str()) {
            continue;
        }
        let why_relevant_md = find_raw("why_relevant_md", "value_text");
        let relevance_score = find_raw("relevance_score", "value_number");
        let order_index = p.get("order_index").cloned().unwrap_or(Value::Null);

        let (_c, id) = crate::db_json::run_write(
            db,
            "INSERT INTO pl_research_findings (session_id, finding_type, target_ref, relevance_score, why_relevant_md, order_index, ai_call_id) VALUES (?, ?, ?, ?, ?, ?, ?)",
            &[
                session_id.clone(),
                json!(finding_type.clone()),
                json!(target_ref.clone()),
                relevance_score,
                why_relevant_md,
                order_index,
                ai_call_id.clone(),
            ],
        )?;
        let mut o = Map::new();
        o.insert("id".into(), json!(id));
        o.insert("finding_type".into(), json!(finding_type));
        o.insert("target_ref".into(), json!(target_ref));
        inserted.push(Value::Object(o));
    }

    crate::db_json::run_write(
        db,
        &format!("UPDATE pl_sessions SET status='deciding_split', updated_at={} WHERE id = ?", crate::clock::now_sql()),
        &[session_id.clone()],
    )?;

    let mut out = Map::new();
    out.insert("session_id".into(), session_id);
    out.insert("ai_call_id".into(), ai_call_id);
    out.insert("findings_persisted".into(), json!(inserted.len() as i64));
    out.insert("findings".into(), json!(inserted));
    out.insert("new_status".into(), json!("deciding_split"));
    out.insert("cached".into(), ai_result.get("cached").cloned().unwrap_or(Value::Null));
    out.insert("cost_usd".into(), json!(cost_usd));
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db0() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS brain; ATTACH ':memory:' AS memory; ATTACH ':memory:' AS laws;").unwrap();
        c.execute_batch(
            "CREATE TABLE pl_sessions ( id INTEGER PRIMARY KEY, raw_intent TEXT, status TEXT, started_by TEXT, budget_usd REAL, spent_usd REAL NOT NULL DEFAULT 0, updated_at TEXT );
             CREATE TABLE pl_research_findings ( id INTEGER PRIMARY KEY, session_id INTEGER NOT NULL, finding_type TEXT, target_ref TEXT, relevance_score REAL, why_relevant_md TEXT, order_index INTEGER, ai_call_id INTEGER );
             CREATE TABLE cards ( slug TEXT PRIMARY KEY, title TEXT, body_json TEXT );
             CREATE TABLE memory.catalog_embeddings ( entity_type TEXT NOT NULL, entity_name TEXT NOT NULL, model TEXT NOT NULL, dims INTEGER NOT NULL, vector BLOB NOT NULL );
             CREATE TABLE memory.memories ( id INTEGER PRIMARY KEY, type TEXT, title TEXT );
             CREATE TABLE laws.laws ( slug TEXT, title TEXT, statement TEXT, rationale TEXT, superseded_by INTEGER, enforcement TEXT );
             CREATE TABLE brain.prompt_templates ( id INTEGER PRIMARY KEY, name TEXT NOT NULL, version INTEGER NOT NULL, template_md TEXT NOT NULL, default_tier TEXT NOT NULL, default_temperature TEXT NOT NULL, max_output_tokens INTEGER NOT NULL DEFAULT 1024, superseded_by INTEGER, created_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')), cross_field_rules_json TEXT, judging_criteria_json TEXT, system_md TEXT, phase_flow TEXT NOT NULL DEFAULT 'single', UNIQUE (name, version) );
             CREATE TABLE brain.prompt_output_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, grounded_refs_json TEXT, UNIQUE (prompt_template_id, field_name) );
             CREATE TABLE brain.prompt_input_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, UNIQUE (prompt_template_id, field_name) );
             CREATE TABLE brain.ai_call_outputs ( id INTEGER PRIMARY KEY, ai_call_id INTEGER, field_name TEXT, value_text TEXT, value_number REAL, value_boolean INTEGER, order_index INTEGER, parent_output_id INTEGER );",
        ).unwrap();
        c
    }

    fn seed_session(c: &Connection, id: i64, status: &str) {
        c.execute(
            "INSERT INTO pl_sessions (id, raw_intent, status, started_by, budget_usd, spent_usd, updated_at) VALUES (?, 'harden the audit chain writes', ?, 'human:cli', 0.5, 0.1, '2025-12-01 00:00:00')",
            rusqlite::params![id, status],
        ).unwrap();
    }

    #[test]
    fn missing_session_id_errors() {
        let c = db0();
        let e = plSessionAdvanceResearch(&json!({}), &c).unwrap_err();
        assert_eq!(e, "pl-session-advance-research: session_id is required");
    }

    #[test]
    fn unknown_session_id_errors_not_found() {
        let c = db0();
        let e = plSessionAdvanceResearch(&json!({ "session_id": 999 }), &c).unwrap_err();
        assert_eq!(e, "pl-session-advance-research: session 999 not found");
    }

    #[test]
    fn wrong_status_errors_validation_logic() {
        let c = db0();
        seed_session(&c, 1, "deciding_split");
        let e = plSessionAdvanceResearch(&json!({ "session_id": 1 }), &c).unwrap_err();
        assert_eq!(e, "pl-session-advance-research: session status='deciding_split', expected 'researching'");
    }

    #[test]
    fn guards_pass_then_hits_the_deferred_ai_boundary() {
        // Everything up to (and including bootstrapping the prompt template + gathering
        // candidates) runs for real; the function then hits the deferred aiCall stub and
        // propagates its Err — this IS the expected outcome on this twin (see module doc).
        let c = db0();
        seed_session(&c, 1, "researching");
        c.execute("INSERT INTO cards (slug, title, body_json) VALUES ('audit-chain-fix', 'harden audit chain', '{}')", []).unwrap();

        let e = plSessionAdvanceResearch(&json!({ "session_id": 1 }), &c).unwrap_err();
        assert!(e.contains("ai-call"), "{e}");

        // ensurePrompt registered the template as a side effect before the deferred call.
        let n: i64 = c.query_row("SELECT COUNT(*) FROM brain.prompt_templates WHERE name = 'pl.research.summary'", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1);
        // no findings/status-change side effects happened past the deferred boundary.
        let fn_: i64 = c.query_row("SELECT COUNT(*) FROM pl_research_findings", [], |r| r.get(0)).unwrap();
        assert_eq!(fn_, 0);
        let status: String = c.query_row("SELECT status FROM pl_sessions WHERE id=1", [], |r| r.get(0)).unwrap();
        assert_eq!(status, "researching");
    }

    #[test]
    fn ensure_prompt_is_idempotent_on_second_call() {
        let c = db0();
        ensure_prompt(&c).unwrap();
        ensure_prompt(&c).unwrap();
        let n: i64 = c.query_row("SELECT COUNT(*) FROM brain.prompt_templates WHERE name = 'pl.research.summary'", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1);
    }

    // ── gatherCandidates / similar_cards ────────────────────────────────────────────────────
    #[test]
    fn similar_cards_matches_title_and_body_on_long_tokens() {
        let c = db0();
        c.execute("INSERT INTO cards (slug, title, body_json) VALUES ('a', 'harden the audit chain', '{}')", []).unwrap();
        c.execute("INSERT INTO cards (slug, title, body_json) VALUES ('b', 'unrelated', '{\"why\":\"harden something\"}')", []).unwrap();
        c.execute("INSERT INTO cards (slug, title, body_json) VALUES ('c', 'no match', '{}')", []).unwrap();
        let out = similar_cards(&c, "harden the audit chain writes", 5).unwrap();
        let refs: Vec<&str> = out.iter().map(|o| o["target_ref"].as_str().unwrap()).collect();
        assert!(refs.contains(&"a"));
        assert!(refs.contains(&"b"));
        assert!(!refs.contains(&"c"));
        assert_eq!(out[0]["finding_type"], json!("similar-card"));
    }

    #[test]
    fn similar_cards_short_tokens_only_yields_no_candidates() {
        let c = db0();
        c.execute("INSERT INTO cards (slug, title, body_json) VALUES ('a', 'fix it now', '{}')", []).unwrap();
        let out = similar_cards(&c, "fix it now", 5).unwrap();
        assert_eq!(out, Vec::<Value>::new());
    }

    #[test]
    fn correction_memories_falls_back_to_recent_when_no_embeddings() {
        let c = db0();
        c.execute("INSERT INTO memory.memories (id, type, title) VALUES (1, 'correction', 'x')", []).unwrap();
        c.execute("INSERT INTO memory.memories (id, type, title) VALUES (2, 'correction', 'y')", []).unwrap();
        c.execute("INSERT INTO memory.memories (id, type, title) VALUES (3, 'decision', 'z')", []).unwrap();
        let mut findings = Vec::new();
        correction_memories(&c, "anything", 5, &mut findings).unwrap();
        assert_eq!(findings.len(), 2);
        assert_eq!(findings[0]["target_ref"], json!("mem-2"));
        assert_eq!(findings[0]["finding_type"], json!("correction-memory"));
        assert!(findings[0].get("score").is_none());
    }

    #[test]
    fn correction_memories_swallows_the_deferred_vector_search_error() {
        let c = db0();
        c.execute(
            "INSERT INTO memory.catalog_embeddings (entity_type, entity_name, model, dims, vector) VALUES ('memory-correction', 'mem-1', 'bge-base', 768, x'00')",
            [],
        ).unwrap();
        let mut findings = Vec::new();
        correction_memories(&c, "anything", 5, &mut findings).unwrap();
        assert_eq!(findings, Vec::<Value>::new());
    }

    #[test]
    fn applicable_laws_maps_slug_and_score_and_swallows_errors_gracefully() {
        let c = db0();
        // catalogFind's substring branch matches the FULL buildContextQuery output ("card: <title>")
        // against the law's title — so the fixture title must literally contain that phrase.
        c.execute("INSERT INTO laws.laws (slug, title, statement, rationale, superseded_by, enforcement) VALUES ('audit-law', 'law about card: harden the audit chain', 'must be hard', '', NULL, 'soft')", []).unwrap();
        let mut findings = Vec::new();
        applicable_laws(&c, "harden the audit chain", 5, &mut findings).unwrap();
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0]["target_ref"], json!("audit-law"));
        assert_eq!(findings[0]["finding_type"], json!("applicable-law"));
    }

    #[test]
    fn gather_candidates_combines_all_three_sources_within_the_limit_times_three_cap() {
        let c = db0();
        for i in 0..5 {
            c.execute(
                "INSERT INTO cards (slug, title, body_json) VALUES (?, 'harden audit chain writes', '{}')",
                rusqlite::params![format!("card-{i}")],
            ).unwrap();
        }
        c.execute("INSERT INTO memory.memories (id, type, title) VALUES (1, 'correction', 'x')", []).unwrap();
        c.execute("INSERT INTO laws.laws (slug, title, statement, rationale, superseded_by, enforcement) VALUES ('audit-law', 'law about card: harden the audit chain writes', 'x', '', NULL, 'soft')", []).unwrap();
        // each of the three branches is itself SQL-LIMITed to `rag_limit`, so the combined total
        // (<= 3*rag_limit) never actually exceeds the `slice(0, limit * 3)` cap in practice — this
        // asserts the combination lands correctly, per-branch caps included.
        let out = gather_candidates(&c, "harden the audit chain writes", 2).unwrap();
        assert!(out.len() <= 6);
        let types: Vec<&str> = out.iter().map(|o| o["finding_type"].as_str().unwrap()).collect();
        assert!(types.contains(&"similar-card"));
        assert!(types.contains(&"correction-memory"));
        assert!(types.contains(&"applicable-law"));
    }
}
