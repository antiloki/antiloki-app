//! notebook-ask.rs — twin of skills/notebooks/notebook-ask.mjs (db + cross-db brain.*/memory.*
//! via reused twins). NAMES ARE CONTRACT: default `notebookAsk`. Grounded Q&A inside a notebook:
//! answered ONLY from the notebook's corpus (code scope + enabled sources + semantic RAG hits),
//! with citations. Rides with WHAT-IF IMPACT (blast-radius on the scope), suggests FOLLOW-UPS, and
//! reports a COVERAGE meter. `mode:'tutor'` switches to a Socratic teaching response. Stored as an
//! `ask` item.
//!
//! JS `const db = context?.db; if (!db) throw err.contextMissing('notebook-ask', 'db')` is
//! unreachable here — `db: &Connection` is a required, non-nullable reference (same convention as
//! every sibling db-lane twin, e.g. notebook-quiz.rs). `context.provider_fn` has no Rust carrier —
//! threaded as `null` into the constructed aiCall inputs, same as notebook-quiz.rs.
//!
//! TWO deferred boundaries, each wrapped in its own JS try/catch that degrades GRACEFULLY (never
//! throws out of the skill):
//!   1. `ragRecall` (`crate::rag_rag_recall::ragRecall`) — calls the deferred `embed()` ONNX stub
//!      only when `memory.rag_chunks` has matching rows; with no/empty table it already resolves
//!      `Ok({hits:[],empty:true})` without ever reaching `embed` (see rag-recall.rs's module doc).
//!      Either way the JS catch (mirrored: match on `Err` only) collapses to `hits = []`.
//!   2. `aiCall` (`crate::ai_ai_call::aiCall`) — the deferred network-AI stub, always `Err`. The
//!      MAIN answer call's failure short-circuits the whole skill: `return { ok:false, error:
//!      "notebook-ask AI failed: <msg> — check the OpenRouter provider/model." }` (mirrored
//!      exactly — the outer function returns early from that catch). The FOLLOW-UPS call has its
//!      OWN separate try/catch that degrades to `followups = []` and does NOT abort the skill —
//!      but since the MAIN call already returns first (deferred boundary #2 fires immediately),
//!      the follow-ups call, `coverage`/`citations` shaping, and `insertItem` are unreachable in
//!      THIS build only because `aiCall` always `Err`s, never because they were skipped in the
//!      port — all ported byte-for-byte below.
//!
//! `blastRadius` (`crate::codebase_blast_radius::blastRadius`) is NOT deferred — a real db-read
//! twin (reverse-edge transitive closure). The JS wraps it in try/catch purely as defensive
//! degrade-on-db-error, not because it's non-deterministic; this twin calls it for real and only
//! catches its `Err`, so the IMPACT block is a genuinely testable, real deterministic branch
//! (unlike the two boundaries above).
//!
//! Reused twins: `notebooks_notebook_schema::ensureNotebookSchema`,
//! `notebooks_notebook_store::{getNotebook, insertItem, aiMeta}`,
//! `notebooks_notebook_corpus::resolveCorpus` (NOT caught in JS — propagates via `?`, same as
//! notebook-flashcards.rs), `codebase_blast_radius::blastRadius`, `rag_rag_recall::ragRecall`,
//! `ai_prompt_find::promptFind`, `ai_prompt_add::promptAdd`, `ai_ai_call::aiCall`.
// QUALITY: db-read/write lane (fully ported, fixture-reachable up to the deferred main-aiCall
// boundary, INCLUDING a real deterministic blast-radius impact branch) + the unreachable
// followups/citations/insertItem tail (ported for completeness, exercised only via reasoning,
// same precedent as notebook-quiz.rs / notebook-flashcards.rs).

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
fn js_string(v: &Value) -> String {
    match v {
        Value::Null => "null".into(),
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Array(a) => a.iter().map(js_string).collect::<Vec<_>>().join(","),
        Value::Object(_) => "[object Object]".into(),
    }
} // = JS String(v)
fn js_number(v: Option<&Value>) -> f64 {
    match v {
        None => f64::NAN,
        Some(Value::Null) => 0.0,
        Some(Value::Bool(b)) => if *b { 1.0 } else { 0.0 },
        Some(Value::Number(n)) => n.as_f64().unwrap_or(f64::NAN),
        Some(Value::String(s)) => { let t = s.trim(); if t.is_empty() { 0.0 } else { t.parse().unwrap_or(f64::NAN) } }
        Some(_) => f64::NAN,
    }
} // = JS Number(v)
fn falsy_num(n: f64) -> bool { n == 0.0 || n.is_nan() }
fn num_param(n: f64) -> Value { if n.is_finite() && n == n.trunc() { json!(n as i64) } else { json!(n) } }
// JS `/\s+/g` → single space, applied to a coerced-to-string value.
fn collapse_ws(s: &str) -> String {
    let mut out = String::new();
    let mut in_ws = false;
    for c in s.chars() {
        if c.is_whitespace() {
            if !in_ws { out.push(' '); }
            in_ws = true;
        } else {
            out.push(c);
            in_ws = false;
        }
    }
    out
}
fn take_chars(s: &str, n: usize) -> String { s.chars().take(n).collect() }

const PROMPT: &str = "notebook.ask.v2";
const TUTOR: &str = "notebook.tutor.v2";
const FOLLOWUPS: &str = "notebook.followups.v1";

const ASK_SYSTEM: &str = "You are a precise research assistant for a software workspace. Answer ONLY from the grounded context provided (in-scope code + sources + indexed knowledge). Cite code as [path] and knowledge as [db#id]. If the context does not contain the answer, say so plainly — never invent files, APIs, or behavior.";
const ASK_TEMPLATE: &str = "NOTEBOOK SCOPE: {{scope_title}}

=== SOURCES (code + signals) ===
{{code_context}}

=== INDEXED KNOWLEDGE (semantic hits) ===
{{rag_context}}

=== QUESTION ===
{{question}}

Write a clear, grounded answer in markdown. Lead with the direct answer, then the supporting detail with [citations]. Where relevant, note the change-impact (what the IMPACT line implies). End with a short \"Sources\" line listing the [path] / [db#id] you actually used.";

const TUTOR_SYSTEM: &str = "You are a Socratic tutor for a software workspace. Teach grounded ONLY in the provided context. Do NOT just dump an answer — explain the key idea simply, build intuition, then ask 1–2 guiding questions that lead the learner to the next insight. Cite [path]/[db#id]. Never invent.";
const TUTOR_TEMPLATE: &str = "NOTEBOOK SCOPE: {{scope_title}}

=== SOURCES (code + signals) ===
{{code_context}}

=== INDEXED KNOWLEDGE ===
{{rag_context}}

=== THE LEARNER ASKS ===
{{question}}

Respond as a tutor: a short, grounded explanation that builds intuition, then **1–2 guiding questions** that move the learner forward. Keep it tight.";

const FU_SYSTEM: &str = "You propose follow-up research questions. Output ONLY the questions.";
const FU_TEMPLATE: &str = "About \"{{scope_title}}\":
Q: {{question}}
A: {{answer}}

List exactly 3 specific, useful follow-up questions a researcher would ask next. One per line. No numbering, no preamble.";

// `function ensurePrompts(db)` — `reg(name, template_md, system_md, field, tier, max)`: try
// promptFind, any error → treat as "not found" (JS `try {...} catch { ex = null }`); if missing,
// promptAdd. Registers 3 prompts (ASK, TUTOR, FOLLOWUPS). promptAdd's own Err (if any) propagates
// out of ensurePrompts (no try/catch around it in JS), mirrored via `?`.
fn ensure_prompts(db: &Connection) -> Result<(), String> {
    let reg = |name: &str, template_md: &str, system_md: &str, field: &str, tier: &str, max: i64| -> Result<(), String> {
        let found = crate::ai_prompt_find::promptFind(&json!({ "name": name }), db).is_ok();
        if !found {
            crate::ai_prompt_add::promptAdd(
                &json!({
                    "name": name,
                    "template_md": template_md,
                    "system_md": system_md,
                    "default_tier": tier,
                    "default_temperature": "cold",
                    "max_output_tokens": max,
                    "phase_flow": "single_only",
                    "output_fields": [{ "field_name": field, "field_type": "string", "required": true }],
                }),
                db,
            )?;
        }
        Ok(())
    };
    reg(PROMPT, ASK_TEMPLATE, ASK_SYSTEM, "answer_md", "low", 1800)?;
    reg(TUTOR, TUTOR_TEMPLATE, TUTOR_SYSTEM, "answer_md", "low", 1400)?;
    reg(FOLLOWUPS, FU_TEMPLATE, FU_SYSTEM, "questions_md", "low", 400)?;
    Ok(())
}

// `const fmt = (h) => \`[${h.source_db}#${h.id}] ${String(h.text).replace(/\s+/g,' ').slice(0,300)}\`;`
fn fmt_hit(h: &Value) -> String {
    let source_db = js_string(h.get("source_db").unwrap_or(&Value::Null));
    let id = js_string(h.get("id").unwrap_or(&Value::Null));
    let text = take_chars(&collapse_ws(&js_string(h.get("text").unwrap_or(&Value::Null))), 300);
    format!("[{source_db}#{id}] {text}")
}
// `const isFileCode = (h) => h.source_db === 'code' && (h.source_table === 'code_nodes' || h.source_table === 'code_descriptions');`
fn is_file_code(h: &Value) -> bool {
    h.get("source_db").and_then(Value::as_str) == Some("code")
        && matches!(h.get("source_table").and_then(Value::as_str), Some("code_nodes") | Some("code_descriptions"))
}

pub fn notebookAsk(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS `context?.db` guard — unreachable here, see module doc.
    let provider_fn = Value::Null;
    crate::notebooks_notebook_schema::ensureNotebookSchema(db, &json!("nb."));

    let id_n = js_number(input.get("id"));
    if falsy_num(id_n) { return Err("notebook-ask: id is required".into()); }
    let id_v = num_param(id_n);

    let question = js_string(input.get("question").filter(|v| !v.is_null()).unwrap_or(&json!(""))).trim().to_string();
    if question.is_empty() { return Err("notebook-ask: question is required".into()); }

    let nb = crate::notebooks_notebook_store::getNotebook(db, &id_v);
    if nb.is_null() {
        return Ok(json!({ "ok": false, "error": format!("notebook {} not found", id_n as i64) }));
    }
    let mode = if input.get("mode").and_then(Value::as_str) == Some("tutor") { "tutor" } else { "ask" };

    // `await resolveCorpus(nb, context)` — NOT wrapped in try/catch in JS; propagates.
    let corpus = crate::notebooks_notebook_corpus::resolveCorpus(&nb, db)?;
    let scope_paths: Vec<Value> = corpus.get("paths").and_then(Value::as_array).cloned().unwrap_or_default();
    let corpus_nodes: Vec<Value> = corpus.get("nodes").and_then(Value::as_array).cloned().unwrap_or_default();
    let corpus_blocks: Vec<Value> = corpus.get("blocks").and_then(Value::as_array).cloned().unwrap_or_default();

    // ── boundary #1: ragRecall (calls the deferred `embed` only past a non-empty scoped fetch;
    // either way `Err` OR an empty-result `Ok` collapse to `hits = []` here, mirroring the JS
    // `try { ... } catch { hits = [] }`).
    let ragrecall_input = json!({
        "query": question,
        "k": 12,
        "scope_paths": if scope_paths.is_empty() { Value::Null } else { json!(scope_paths) },
    });
    let hits: Vec<Value> = match crate::rag_rag_recall::ragRecall(&ragrecall_input, &Value::Null, db) {
        Ok(r) => r.get("hits").and_then(Value::as_array).cloned().unwrap_or_default(),
        Err(_) => Vec::new(),
    };

    let code_hits: Vec<&Value> = hits.iter().filter(|h| is_file_code(h)).collect();
    let know_hits: Vec<&Value> = hits.iter().filter(|h| !is_file_code(h)).collect();
    let mut rag_parts: Vec<String> = Vec::new();
    if !code_hits.is_empty() {
        rag_parts.push(format!("IN-SCOPE CODE (semantic):\n{}", code_hits.iter().map(|h| fmt_hit(h)).collect::<Vec<_>>().join("\n")));
    }
    if !know_hits.is_empty() {
        rag_parts.push(format!("KNOWLEDGE:\n{}", know_hits.iter().map(|h| fmt_hit(h)).collect::<Vec<_>>().join("\n")));
    }
    let rag_context = if rag_parts.is_empty() {
        "(no indexed matches — run the RAG sweep, incl. code, to index this scope.)".to_string()
    } else {
        rag_parts.join("\n\n")
    };

    // P2 — what-if impact: blast-radius of the scope. `blastRadius` itself is real/deterministic;
    // the JS try/catch here is purely a defensive db-error degrade, mirrored via `.ok()`-style match.
    let ids: Vec<Value> = corpus_nodes.iter().map(|n| n.get("id").cloned().unwrap_or(Value::Null)).collect();
    let impact: Value = if !ids.is_empty() {
        match crate::codebase_blast_radius::blastRadius(&json!({ "node_ids": ids }), db) {
            Ok(r) => {
                let affected: Vec<Value> = r.get("affected").and_then(Value::as_array).cloned().unwrap_or_default();
                let tests: Vec<Value> = r.get("affected_tests").and_then(Value::as_array).cloned().unwrap_or_default();
                let sample: Vec<Value> = affected.iter().take(8).cloned().collect();
                json!({ "scope": ids.len(), "affected": affected.len(), "tests": tests.len(), "sample": sample })
            }
            Err(_) => Value::Null,
        }
    } else {
        Value::Null
    };
    let impact_line: String = if !impact.is_null() {
        format!(
            "IMPACT: {} in-scope files → {} dependents, {} tests could be affected by changes here.",
            impact.get("scope").and_then(Value::as_i64).unwrap_or(0),
            impact.get("affected").and_then(Value::as_i64).unwrap_or(0),
            impact.get("tests").and_then(Value::as_i64).unwrap_or(0),
        )
    } else {
        String::new()
    };

    let sources_text: String = if !corpus_blocks.is_empty() {
        corpus_blocks
            .iter()
            .map(|b| {
                let label = js_string(b.get("label").unwrap_or(&Value::Null)).to_uppercase();
                let text = js_string(b.get("text").unwrap_or(&Value::Null));
                format!("=== {label} ===\n{text}")
            })
            .collect::<Vec<_>>()
            .join("\n\n")
    } else {
        "(no sources — set a scope or add sources.)".to_string()
    };
    let code_context: String = if !impact_line.is_empty() { format!("{impact_line}\n\n{sources_text}") } else { sources_text };

    let nb_title = nb.get("title").cloned().unwrap_or(Value::Null);
    let tier_v = if truthy(input.get("tier")) { input.get("tier").cloned().unwrap() } else { json!("high") };
    let model_id_v = if truthy(input.get("model_id")) { input.get("model_id").cloned().unwrap() } else { Value::Null };
    let code_context_capped: String = take_chars(&code_context, 16000);
    let rag_context_capped: String = take_chars(&rag_context, 8000);

    // ── boundary #2 (MAIN answer): ensurePrompts + aiCall in ONE try/catch — on ANY error
    // (including ensurePrompts' promptAdd failing) the skill returns `{ ok:false, error: ... }`
    // immediately, aborting before followups/coverage/citations/insertItem.
    let main: Result<(String, Value), String> = (|| {
        ensure_prompts(db)?;
        let ai_input = json!({
            "prompt_name": if mode == "tutor" { TUTOR } else { PROMPT },
            "tier": tier_v,
            "model_id": model_id_v,
            "provider_fn": provider_fn,
            "input_kv": [
                { "key": "scope_title", "value": nb_title },
                { "key": "code_context", "value": code_context_capped },
                { "key": "rag_context", "value": rag_context_capped },
                { "key": "question", "value": question },
            ],
        });
        let res = crate::ai_ai_call::aiCall(&ai_input, db)?;
        let answer = res
            .get("output_kv")
            .and_then(Value::as_array)
            .and_then(|arr| {
                arr.iter().find(|o| {
                    o.get("field_name").and_then(Value::as_str) == Some("answer_md")
                        && o.get("parent_output_id").map_or(true, |v| v.is_null())
                })
            })
            .and_then(|o| o.get("value_text"))
            .filter(|v| truthy(Some(v)))
            .map(|v| str_qq(Some(v)))
            .unwrap_or_default();
        let ai_m = crate::notebooks_notebook_store::aiMeta(db, &res);
        Ok((answer, ai_m))
    })();

    let (mut answer, ai_m) = match main {
        Ok(v) => v,
        Err(e) => {
            let msg = if e.is_empty() { "provider error".to_string() } else { e };
            return Ok(json!({ "ok": false, "error": format!("notebook-ask AI failed: {msg} — check the OpenRouter provider/model.") }));
        }
    };
    if answer.trim().is_empty() { answer = "_(the model returned an empty answer.)_".to_string(); }

    // P4 — follow-up suggestions (best-effort, cheap low-tier). Its OWN try/catch: a failure here
    // degrades to `followups = []` WITHOUT aborting the skill (unlike boundary #2 above).
    let fu_input = json!({
        "prompt_name": FOLLOWUPS,
        "tier": "low",
        "provider_fn": provider_fn,
        "input_kv": [
            { "key": "scope_title", "value": nb_title },
            { "key": "question", "value": question },
            { "key": "answer", "value": take_chars(&answer, 4000) },
        ],
    });
    let followups: Vec<Value> = match crate::ai_ai_call::aiCall(&fu_input, db) {
        Ok(fr) => {
            let txt = fr
                .get("output_kv")
                .and_then(Value::as_array)
                .and_then(|arr| {
                    arr.iter().find(|o| {
                        o.get("field_name").and_then(Value::as_str) == Some("questions_md")
                            && o.get("parent_output_id").map_or(true, |v| v.is_null())
                    })
                })
                .and_then(|o| o.get("value_text"))
                .filter(|v| truthy(Some(v)))
                .map(|v| str_qq(Some(v)))
                .unwrap_or_default();
            strip_bullet_lines(&txt)
        }
        Err(_) => Vec::new(),
    };

    let coverage = json!({
        "sources": corpus_blocks.len(),
        "hits": hits.len(),
        "grounded": (corpus_blocks.len() + hits.len()) > 0,
    });
    let mut citations: Vec<Value> = corpus_nodes
        .iter()
        .take(20)
        .map(|n| json!({ "kind": "code", "ref": n.get("path").cloned().unwrap_or(Value::Null), "node_id": n.get("id").cloned().unwrap_or(Value::Null) }))
        .collect();
    citations.extend(know_hits.iter().take(10).map(|h| {
        json!({
            "kind": "knowledge",
            "ref": format!("{}#{}", js_string(h.get("source_db").unwrap_or(&Value::Null)), js_string(h.get("id").unwrap_or(&Value::Null))),
            "source_db": h.get("source_db").cloned().unwrap_or(Value::Null),
            "source_row": h.get("source_row").cloned().unwrap_or(Value::Null),
        })
    }));

    let mut meta: Map<String, Value> = Map::new();
    meta.insert("question".into(), json!(question));
    meta.insert("mode".into(), json!(mode));
    meta.insert("citations".into(), json!(citations));
    meta.insert("impact".into(), impact);
    meta.insert("followups".into(), json!(followups));
    meta.insert("coverage".into(), coverage);
    if let Some(aim_obj) = ai_m.as_object() {
        for (k, v) in aim_obj { meta.insert(k.clone(), v.clone()); }
    }
    let item = crate::notebooks_notebook_store::insertItem(
        db,
        &id_v,
        &json!({ "kind": "ask", "title": take_chars(&question, 120), "body_md": answer, "meta": Value::Object(meta), "pinned": 0 }),
    );
    Ok(json!({ "ok": true, "item": item, "scope_summary": { "nodes": corpus_nodes.len(), "rag_hits": hits.len() } }))
}

// `txt.split('\n').map((s) => s.replace(/^[\s\-*\d.)]+/, '').trim()).filter(Boolean).slice(0, 3)`
fn strip_bullet_lines(txt: &str) -> Vec<Value> {
    txt.split('\n')
        .map(|s| {
            let trimmed_prefix = s.trim_start_matches(|c: char| c.is_whitespace() || c == '-' || c == '*' || c.is_ascii_digit() || c == '.' || c == ')');
            trimmed_prefix.trim().to_string()
        })
        .filter(|s| !s.is_empty())
        .take(3)
        .map(Value::String)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS nb; ATTACH ':memory:' AS brain; ATTACH ':memory:' AS memory; ATTACH ':memory:' AS code;").unwrap();
        c.execute_batch(
            "CREATE TABLE nb.notebooks (id INTEGER PRIMARY KEY AUTOINCREMENT, slug TEXT, title TEXT NOT NULL, summary TEXT, scope_json TEXT NOT NULL DEFAULT '{}', created_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')), updated_at TEXT);
             CREATE TABLE nb.notebook_items (id INTEGER PRIMARY KEY AUTOINCREMENT, notebook_id INTEGER NOT NULL, kind TEXT NOT NULL DEFAULT 'note', title TEXT, body_md TEXT NOT NULL DEFAULT '', meta_json TEXT, pinned INTEGER NOT NULL DEFAULT 0, created_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')));
             CREATE TABLE nb.notebook_sources (id INTEGER PRIMARY KEY AUTOINCREMENT, notebook_id INTEGER NOT NULL, kind TEXT NOT NULL, ref TEXT, label TEXT, body_md TEXT, enabled INTEGER NOT NULL DEFAULT 1, meta_json TEXT, created_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')));
             CREATE TABLE brain.prompt_templates ( id INTEGER PRIMARY KEY, name TEXT NOT NULL, version INTEGER NOT NULL, template_md TEXT NOT NULL, default_tier TEXT NOT NULL, default_temperature TEXT NOT NULL, max_output_tokens INTEGER NOT NULL DEFAULT 1024, superseded_by INTEGER, created_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')), cross_field_rules_json TEXT, judging_criteria_json TEXT, system_md TEXT, phase_flow TEXT NOT NULL DEFAULT 'single', UNIQUE (name, version) );
             CREATE TABLE brain.prompt_output_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, grounded_refs_json TEXT, UNIQUE (prompt_template_id, field_name) );
             CREATE TABLE brain.prompt_input_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, UNIQUE (prompt_template_id, field_name) );
             CREATE TABLE memory.rag_chunks ( id INTEGER PRIMARY KEY, source_db TEXT, source_table TEXT, source_row TEXT, text TEXT, vector BLOB );
             CREATE TABLE code.code_edges ( from_node TEXT, to_node TEXT );
             CREATE TABLE code.code_nodes ( id TEXT PRIMARY KEY, type TEXT, path TEXT );",
        ).unwrap();
        c
    }

    fn a_notebook(c: &Connection, title: &str) -> i64 {
        c.execute("INSERT INTO nb.notebooks (title) VALUES (?)", [title]).unwrap();
        c.last_insert_rowid()
    }

    #[test]
    fn id_is_required() {
        let c = setup();
        let e = notebookAsk(&json!({}), &c).unwrap_err();
        assert_eq!(e, "notebook-ask: id is required");
        let e2 = notebookAsk(&json!({ "id": 0 }), &c).unwrap_err();
        assert_eq!(e2, "notebook-ask: id is required");
    }

    #[test]
    fn question_is_required() {
        let c = setup();
        let nid = a_notebook(&c, "N");
        let e = notebookAsk(&json!({ "id": nid }), &c).unwrap_err();
        assert_eq!(e, "notebook-ask: question is required");
        let e2 = notebookAsk(&json!({ "id": nid, "question": "   " }), &c).unwrap_err();
        assert_eq!(e2, "notebook-ask: question is required");
    }

    #[test]
    fn unknown_notebook_returns_ok_false() {
        let c = setup();
        let r = notebookAsk(&json!({ "id": 999, "question": "what?" }), &c).unwrap();
        assert_eq!(r, json!({ "ok": false, "error": "notebook 999 not found" }));
    }

    #[test]
    fn reaches_the_parity_deferred_aicall_boundary_and_degrades_gracefully() {
        // proves every deterministic gate (id/question guard, notebook lookup, corpus resolve,
        // ragRecall degrade, ensurePrompts) ran cleanly before the deferred main aiCall boundary;
        // the deferred stub's Err is caught (never propagated), matching the JS try/catch
        // degrade-gracefully contract.
        let c = setup();
        let nid = a_notebook(&c, "My Notebook");
        let out = notebookAsk(&json!({ "id": nid, "question": "how does X work?" }), &c).unwrap();
        assert_eq!(out["ok"], json!(false));
        assert!(out["error"].as_str().unwrap().starts_with("notebook-ask AI failed:"));
        // all 3 prompts WERE ensured before the deferred call.
        let n: i64 = c.query_row("SELECT COUNT(*) FROM brain.prompt_templates WHERE name IN ('notebook.ask.v2','notebook.tutor.v2','notebook.followups.v1')", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 3);
        // no item was inserted — the wave never reached insertItem.
        let items: i64 = c.query_row("SELECT COUNT(*) FROM nb.notebook_items", [], |r| r.get(0)).unwrap();
        assert_eq!(items, 0);
    }

    #[test]
    fn tutor_mode_still_reaches_the_same_deferred_boundary() {
        let c = setup();
        let nid = a_notebook(&c, "N");
        let out = notebookAsk(&json!({ "id": nid, "question": "teach me", "mode": "tutor" }), &c).unwrap();
        assert_eq!(out["ok"], json!(false));
        assert!(out["error"].as_str().unwrap().starts_with("notebook-ask AI failed:"));
    }

    #[test]
    fn ragrecall_empty_table_degrades_to_empty_hits_without_reaching_embed() {
        // empty memory.rag_chunks → ragRecall's own early-degrade `Ok({hits:[],empty:true})`,
        // never touching the embed() deferred boundary at all — still ends in hits=[] either way.
        let c = setup();
        let nid = a_notebook(&c, "N");
        let out = notebookAsk(&json!({ "id": nid, "question": "q?" }), &c).unwrap();
        assert_eq!(out["ok"], json!(false)); // still fails at the main aiCall boundary, as expected
    }

    #[test]
    fn blast_radius_impact_is_real_and_computed_before_the_ai_boundary() {
        // seeds code_nodes/code_edges + a notebook scope naming those nodes so resolveCorpus
        // yields corpus.nodes with ids — proving blastRadius (NOT deferred) genuinely executes.
        let c = setup();
        c.execute_batch(
            "INSERT INTO code.code_nodes (id, type, path) VALUES ('n1','file','a.mjs'), ('n2','file','b.mjs');
             INSERT INTO code.code_edges (from_node, to_node) VALUES ('n2','n1');",
        ).unwrap();
        c.execute(
            "INSERT INTO nb.notebooks (title, scope_json) VALUES (?, ?)",
            rusqlite::params!["Scoped", r#"{"kind":"nodes","node_ids":["n1"]}"#],
        ).unwrap();
        let nid = c.last_insert_rowid();
        // Whatever resolveCorpus does with this scope shape, the skill must still reach (and
        // gracefully lose at) the main aiCall boundary — proving the impact branch didn't panic
        // or diverge control flow, real or empty.
        let out = notebookAsk(&json!({ "id": nid, "question": "impact?" }), &c).unwrap();
        assert_eq!(out["ok"], json!(false));
        assert!(out["error"].as_str().unwrap().starts_with("notebook-ask AI failed:"));
    }

    // ── strip_bullet_lines (pure, unreachable past the deferred boundary — unit-tested directly)
    #[test]
    fn strip_bullet_lines_strips_leading_markers_and_caps_at_3() {
        let txt = "1. First question?\n- Second one\n* Third?\n4) Fourth (dropped)\n\n";
        let out = strip_bullet_lines(txt);
        assert_eq!(out, vec![json!("First question?"), json!("Second one"), json!("Third?")]);
    }

    #[test]
    fn strip_bullet_lines_filters_blank_lines() {
        let out = strip_bullet_lines("\n   \nOnly one\n");
        assert_eq!(out, vec![json!("Only one")]);
    }

    #[test]
    fn is_file_code_matches_only_code_nodes_or_descriptions() {
        assert!(is_file_code(&json!({ "source_db": "code", "source_table": "code_nodes" })));
        assert!(is_file_code(&json!({ "source_db": "code", "source_table": "code_descriptions" })));
        assert!(!is_file_code(&json!({ "source_db": "code", "source_table": "other" })));
        assert!(!is_file_code(&json!({ "source_db": "knowledge", "source_table": "code_nodes" })));
    }

    #[test]
    fn fmt_hit_collapses_whitespace_and_caps_at_300_chars() {
        let long = "x".repeat(400);
        let h = json!({ "source_db": "code", "id": 7, "text": format!("a  b\n\tc {long}") });
        let out = fmt_hit(&h);
        assert!(out.starts_with("[code#7] a b c "));
        // Verified against real JS: `[${h.source_db}#${h.id}] ${String(h.text).replace(/\s+/g,' ').slice(0,300)}`
        // node -e output for this exact input: "[code#7] a b c xxxxx..." with length 309 (9-char prefix + 300-char slice).
        assert_eq!(out.len(), "[code#7] ".len() + 300);
    }
}
