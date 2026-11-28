//! notebook-generate.rs — twin of skills/notebooks/notebook-generate.mjs (db lane, cross-db
//! brain.* + rag.* via reused prompt/ai-call/rag-recall twins). NAMES ARE CONTRACT: default
//! `notebookGenerate`. EPIC-NOTEBOOKS: one-click grounded artifacts for a notebook's scope
//! (Overview, Walkthrough, Open-questions, Onboarding, Glossary, Risk report, ...). Each is
//! synthesized from the scoped corpus (in-scope code + RAG hits) via the AI chokepoint and stored
//! as an `artifact` item.
//!
//! `context.db` missing → JS throws `err.contextMissing('notebook-generate', 'db')`. Unreachable
//! here: `db: &Connection` is a required, non-nullable reference (same convention as every
//! sibling db-lane twin, e.g. notebook-quiz.rs). `context.provider_fn` (a JS function reference
//! for provider selection/DI) has no Rust equivalent to carry — threaded as `null` into the
//! constructed aiCall input, same as notebook-quiz.rs's `provider_fn`.
//!
//! PARITY-DEFERRED (two independent boundaries, both handled exactly as the JS does):
//! 1. `ragRecall` (`crate::rag_rag_recall::ragRecall`) bottoms out at the deferred embed call. JS
//!    wraps its own `await ragRecall(...)` in a dedicated `try { ... } catch { hits = []; }` —
//!    NOT the same try/catch as the AI wave below. Mirrored here with its own `match`, same
//!    pattern as smart-context-bundle.rs's ragRecall call: `Err(_) => hits stays empty`.
//! 2. The AI call is dispatched through `crate::ai_ai_call::aiCall`, the deferred network-AI stub
//!    — it always returns `Err`. The JS wraps `ensurePrompt` + the `aiCall` await + the
//!    output-field lookup in one `try { } catch (e) { return { ok:false, error: ... } }` — a
//!    failed/absent provider degrades GRACEFULLY, it never throws. Mirrored here as one closure
//!    whose `Err` short-circuits to the catch branch, same shape as notebook-quiz.rs.
//!
//! Everything else — the KINDS directive table, id/kind/focus parsing, notebook lookup, corpus
//! resolution (reusing `notebooks_notebook_corpus::resolveCorpus`), the rag-hits code/knowledge
//! split + formatting, `ensurePrompt` (reusing `ai_prompt_find`/`ai_prompt_add`), and the final
//! citations + insertItem — is the real, fully ported deterministic surface.
// QUALITY: db-read/write lane (fully ported, fixture-reachable up to both deferred boundaries) +
// 0 unreachable pure helpers (no `sanitize`-style JSON-shaping needed here — the AI's raw
// artifact_md string is used as-is, same as the JS).

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
fn falsy_num(n: f64) -> bool { n == 0.0 || n.is_nan() }
// `.replace(/\s+/g, ' ')` — collapse whitespace runs (no trim).
fn ws_collapse(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

// ── KINDS: `label`/`directive` pairs, `overview` is the fallback for an unknown kind ───────────
fn kind_spec(kind: &str) -> (&'static str, &'static str) {
    match kind {
        "overview" => ("Overview", "A concise briefing of this scope: what it is, its main parts, how they fit together, and the key entry points. 4–8 short sections."),
        "walkthrough" => ("Walkthrough", "A numbered, step-by-step walkthrough of how this code works end-to-end, following the primary flow. At each step name the file(s) involved and what happens."),
        "questions" => ("Open questions", "The open questions, risks, unknowns, and gaps in this scope, as a flat markdown bullet list. Each bullet = ONE actionable question or risk that could become a work item. No preamble."),
        "onboarding" => ("Onboarding guide", "An onboarding guide for a new engineer on this scope: where to start, the core concepts, the main flows, and the gotchas. Practical and ordered."),
        "glossary" => ("Glossary", "A glossary of the domain + technical terms used in this scope. Format: **term** — short grounded definition with a [path] where it lives."),
        "risks" => ("Risk report", "A risk report: what could break, fragile spots, missing tests, and blast considerations across this scope. Severity-ordered bullets with [citations]."),
        "faq" => ("FAQ", "A FAQ: the 8–12 most useful questions about this scope, each with a short grounded answer + [citation]."),
        "timeline" => ("Timeline", "A chronological timeline of how this scope evolved, from the git + decisions context. One bullet per milestone, with dates where known."),
        "briefing" => ("Briefing doc", "An executive briefing: the goal, the current state, the key decisions, the risks, and the open work — tight and skimmable."),
        "table" => ("Info table", "A single markdown TABLE organizing the key entities of this scope (e.g. file | role | key functions | depends-on). 6–20 grounded rows."),
        "slides" => ("Slides", "A slide deck as markdown — each slide separated by a `---` line, starting with `## <slide title>` then 3–5 concise bullets. 6–10 slides telling this scope's story."),
        "eli5" => ("ELI5", "Explain this scope simply, as if to a smart beginner — plain language, a concrete analogy, no jargon without defining it."),
        "adr" => ("ADR", "An Architecture Decision Record for the main design choice in this scope: Context · Decision · Consequences · Alternatives considered. Grounded."),
        "critique" => ("Critique", "A red-team critique: the weakest points, risks, smells, and what a senior reviewer would flag here. Specific, with [citations]; no praise padding."),
        "runbook" => ("Runbook", "An operational runbook for this scope: how to run it, common failures + fixes, and the key signals to watch."),
        "testplan" => ("Test plan → stubs", "A test plan for this scope: first the cases to cover (happy path · edge cases · error paths) as a checklist, then CONCRETE test STUBS as a fenced code block per file, written in the project's test style/framework (infer it from the sources). Mark gaps in the current coverage."),
        _ => kind_spec("overview"),
    }
}

const PROMPT: &str = "notebook.generate.v2";
const SYSTEM: &str = "You are a precise research assistant for a software workspace. Produce the requested artifact grounded ONLY in the provided in-scope code + indexed knowledge. Cite code as [path] and knowledge as [db#id]. Be concrete and structured; never invent files or behavior not present in the context.";
const TEMPLATE: &str = "NOTEBOOK SCOPE: {{scope_title}}
ARTIFACT: {{kind}}
DIRECTIVE: {{directive}}

=== CODE IN SCOPE ===
{{code_context}}

=== INDEXED KNOWLEDGE ===
{{rag_context}}

Return the artifact as clean markdown. Use headings and lists. Include [citations] inline where a claim rests on a specific file or source. If the scope is empty, say what is missing instead of inventing content.";

fn prompt_output_fields() -> Value {
    json!([{ "field_name": "artifact_md", "field_type": "string", "required": true }])
}

// `function ensurePrompt(db)`: try promptFind, ignore any error → null; if missing, promptAdd.
fn ensure_prompt(db: &Connection) -> Result<(), String> {
    let found = crate::ai_prompt_find::promptFind(&json!({ "name": PROMPT }), db).is_ok();
    if found {
        return Ok(());
    }
    crate::ai_prompt_add::promptAdd(
        &json!({
            "name": PROMPT,
            "template_md": TEMPLATE,
            "system_md": SYSTEM,
            "default_tier": "mid",
            "default_temperature": "cold",
            "max_output_tokens": 2400,
            "phase_flow": "single_only",
            "output_fields": prompt_output_fields(),
        }),
        db,
    )?;
    Ok(())
}

// `const fmt = (h) => \`[${h.source_db}#${h.id}] ${String(h.text).replace(/\s+/g, ' ').slice(0, 300)}\`;`
fn fmt_hit(h: &Value) -> String {
    let source_db = js_string(h.get("source_db").unwrap_or(&Value::Null));
    let id = js_string(h.get("id").unwrap_or(&Value::Null));
    let text: String = ws_collapse(&js_string(h.get("text").unwrap_or(&Value::Null))).chars().take(300).collect();
    format!("[{source_db}#{id}] {text}")
}
// `const isFileCode = (h) => h.source_db === 'code' && (h.source_table === 'code_nodes' || h.source_table === 'code_descriptions');`
fn is_file_code(h: &Value) -> bool {
    h.get("source_db").and_then(Value::as_str) == Some("code")
        && matches!(h.get("source_table").and_then(Value::as_str), Some("code_nodes") | Some("code_descriptions"))
}

pub fn notebookGenerate(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS: `const db = context?.db; if (!db) throw err.contextMissing(...)` — unreachable here
    // (Rust's `db: &Connection` is never absent). `context.provider_fn` has no Rust carrier — see
    // module doc; passed as `null` into the constructed aiCall input below.
    let provider_fn = Value::Null;
    crate::notebooks_notebook_schema::ensureNotebookSchema(db, &json!("nb."));

    let id_num = js_number(input.get("id"));
    if falsy_num(id_num) {
        return Err("notebook-generate: id is required".to_string());
    }
    let id_val = json!(id_num);

    let kind = if truthy(input.get("kind")) { js_string(input.get("kind").unwrap()) } else { "overview".to_string() };
    let (label, base_directive) = kind_spec(&kind);
    let focus = js_string(input.get("focus").unwrap_or(&Value::Null)).trim().to_string();
    let focus_capped: String = focus.chars().take(600).collect();
    let directive = if !focus.is_empty() {
        format!("{base_directive}\n\nOPERATOR INTENT — steer the artifact to this: {focus_capped}")
    } else {
        base_directive.to_string()
    };

    let nb = crate::notebooks_notebook_store::getNotebook(db, &id_val);
    if nb.is_null() {
        return Ok(json!({ "ok": false, "error": format!("notebook {} not found", id_num as i64) }));
    }

    let corpus = crate::notebooks_notebook_corpus::resolveCorpus(&nb, db)?;
    let scope_paths: Vec<Value> = corpus.get("paths").and_then(Value::as_array).cloned().unwrap_or_default();

    // ── ragRecall: its OWN try/catch in the JS, distinct from the AI wave's try/catch below.
    let nb_title = nb.get("title").cloned().unwrap_or(Value::Null);
    let query = format!("{} {}", js_string(&nb_title), label);
    let rag_input = json!({
        "query": query,
        "k": 10,
        "scope_paths": if scope_paths.is_empty() { Value::Null } else { json!(scope_paths) },
    });
    let hits: Vec<Value> = match crate::rag_rag_recall::ragRecall(&rag_input, &Value::Null, db) {
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
    let rag_context = if rag_parts.is_empty() { "(no indexed matches.)".to_string() } else { rag_parts.join("\n\n") };

    let blocks: Vec<Value> = corpus.get("blocks").and_then(Value::as_array).cloned().unwrap_or_default();
    let code_context = if blocks.is_empty() {
        "(no sources — set a scope or add sources.)".to_string()
    } else {
        blocks
            .iter()
            .map(|b| {
                let label = js_string(b.get("label").unwrap_or(&Value::Null)).to_uppercase();
                let text = js_string(b.get("text").unwrap_or(&Value::Null));
                format!("=== {label} ===\n{text}")
            })
            .collect::<Vec<_>>()
            .join("\n\n")
    };

    // ── the AI wave: PARITY-DEFERRED from here. JS wraps ensurePrompt + the aiCall await + the
    // output-field lookup in one try/catch — a failed/absent provider degrades GRACEFULLY, it
    // never throws. Mirrored here as one closure whose Err short-circuits to the catch branch
    // below (same shape as notebook-quiz.rs); everything inside is real, ported logic — the
    // success path is unreachable in THIS build only because `crate::ai_ai_call::aiCall` always
    // returns `Err` (the deferred stub), never because it was skipped in the port.
    let wave: Result<(Value, Value), String> = (|| {
        ensure_prompt(db)?;
        let tier = if truthy(input.get("tier")) { input.get("tier").cloned().unwrap() } else { json!("high") };
        let model_id = input.get("model_id").filter(|v| !v.is_null()).cloned().unwrap_or(Value::Null);
        let code_context_capped: String = code_context.chars().take(18000).collect();
        let rag_context_capped: String = rag_context.chars().take(8000).collect();
        let ai_input = json!({
            "prompt_name": PROMPT,
            "tier": tier,
            "model_id": model_id,
            "provider_fn": provider_fn,
            "input_kv": [
                { "key": "scope_title", "value": nb_title },
                { "key": "kind", "value": label },
                { "key": "directive", "value": directive },
                { "key": "code_context", "value": code_context_capped },
                { "key": "rag_context", "value": rag_context_capped },
            ],
        });
        let res = crate::ai_ai_call::aiCall(&ai_input, db)?;
        let artifact = res
            .get("output_kv")
            .and_then(Value::as_array)
            .and_then(|arr| {
                arr.iter().find(|o| {
                    o.get("field_name").and_then(Value::as_str) == Some("artifact_md")
                        && o.get("parent_output_id").map_or(true, |v| v.is_null())
                })
            })
            .and_then(|o| o.get("value_text"))
            .cloned()
            .unwrap_or(json!(""));
        let ai_m = crate::notebooks_notebook_store::aiMeta(db, &res);
        Ok((artifact, ai_m))
    })();

    let (artifact_v, ai_m) = match wave {
        Ok(v) => v,
        Err(e) => {
            return Ok(json!({ "ok": false, "error": format!("notebook-generate AI failed: {e} — check the OpenRouter provider/model.") }));
        }
    };
    let artifact = if js_string(&artifact_v).trim().is_empty() {
        "_(the model returned an empty artifact.)_".to_string()
    } else {
        js_string(&artifact_v)
    };

    let nodes: Vec<Value> = corpus.get("nodes").and_then(Value::as_array).cloned().unwrap_or_default();
    let citations: Vec<Value> = nodes
        .into_iter()
        .take(24)
        .map(|n| json!({ "kind": "code", "ref": n.get("path").cloned().unwrap_or(Value::Null), "node_id": n.get("id").cloned().unwrap_or(Value::Null) }))
        .collect();

    let mut meta = ai_m.as_object().cloned().unwrap_or_default();
    meta.insert("artifact_kind".into(), json!(kind));
    meta.insert("citations".into(), json!(citations));
    let item = crate::notebooks_notebook_store::insertItem(
        db,
        &id_val,
        &json!({ "kind": "artifact", "title": label, "body_md": artifact, "meta": Value::Object(meta), "pinned": 0 }),
    );
    Ok(json!({ "ok": true, "item": item }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS nb; ATTACH ':memory:' AS brain; ATTACH ':memory:' AS knowledge;").unwrap();
        c.execute_batch(
            "CREATE TABLE nb.notebooks (id INTEGER PRIMARY KEY AUTOINCREMENT, slug TEXT, title TEXT NOT NULL, summary TEXT, scope_json TEXT NOT NULL DEFAULT '{}', created_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')), updated_at TEXT);
             CREATE TABLE nb.notebook_items (id INTEGER PRIMARY KEY AUTOINCREMENT, notebook_id INTEGER NOT NULL, kind TEXT NOT NULL DEFAULT 'note', title TEXT, body_md TEXT NOT NULL DEFAULT '', meta_json TEXT, pinned INTEGER NOT NULL DEFAULT 0, created_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')));
             CREATE TABLE nb.notebook_sources (id INTEGER PRIMARY KEY AUTOINCREMENT, notebook_id INTEGER NOT NULL, kind TEXT NOT NULL, ref TEXT, label TEXT, body_md TEXT, enabled INTEGER NOT NULL DEFAULT 1, meta_json TEXT, created_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')));
             CREATE TABLE brain.prompt_templates ( id INTEGER PRIMARY KEY, name TEXT NOT NULL, version INTEGER NOT NULL, template_md TEXT NOT NULL, default_tier TEXT NOT NULL, default_temperature TEXT NOT NULL, max_output_tokens INTEGER NOT NULL DEFAULT 1024, superseded_by INTEGER, created_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')), cross_field_rules_json TEXT, judging_criteria_json TEXT, system_md TEXT, phase_flow TEXT NOT NULL DEFAULT 'single' );
             CREATE TABLE brain.prompt_output_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, grounded_refs_json TEXT, UNIQUE (prompt_template_id, field_name) );
             CREATE TABLE brain.prompt_input_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, UNIQUE (prompt_template_id, field_name) );
             CREATE TABLE code_nodes (id TEXT, origin TEXT, path TEXT, type TEXT, name TEXT, signature TEXT, intent TEXT, parent_id TEXT);
             CREATE TABLE code_edges (origin TEXT, from_node TEXT, to_node TEXT, type TEXT, kind TEXT);",
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
        let e = notebookGenerate(&json!({}), &c).unwrap_err();
        assert_eq!(e, "notebook-generate: id is required");
        let e2 = notebookGenerate(&json!({ "id": 0 }), &c).unwrap_err();
        assert_eq!(e2, "notebook-generate: id is required");
    }

    #[test]
    fn unknown_notebook_returns_ok_false() {
        let c = setup();
        let r = notebookGenerate(&json!({ "id": 999 }), &c).unwrap();
        assert_eq!(r, json!({ "ok": false, "error": "notebook 999 not found" }));
    }

    #[test]
    fn unknown_kind_falls_back_to_overview_spec() {
        assert_eq!(kind_spec("not-a-real-kind"), kind_spec("overview"));
        assert_eq!(kind_spec("walkthrough").0, "Walkthrough");
    }

    #[test]
    fn fmt_hit_collapses_whitespace_and_caps_at_300() {
        let h = json!({ "source_db": "code", "id": 7, "text": "a   b\nc" });
        assert_eq!(fmt_hit(&h), "[code#7] a b c");
        let long = json!({ "source_db": "knowledge", "id": 1, "text": "x".repeat(400) });
        assert_eq!(fmt_hit(&long).chars().count(), "[knowledge#1] ".len() + 300);
    }

    #[test]
    fn is_file_code_matches_only_code_db_with_node_or_description_table() {
        assert!(is_file_code(&json!({ "source_db": "code", "source_table": "code_nodes" })));
        assert!(is_file_code(&json!({ "source_db": "code", "source_table": "code_descriptions" })));
        assert!(!is_file_code(&json!({ "source_db": "code", "source_table": "code_edges" })));
        assert!(!is_file_code(&json!({ "source_db": "knowledge", "source_table": "code_nodes" })));
    }

    #[test]
    fn reaches_the_parity_deferred_aicall_boundary_and_degrades_gracefully() {
        // proves every deterministic gate (id guard, kind resolution, notebook lookup, corpus
        // resolve, ragRecall's own try/catch, prompt ensure) ran cleanly before the deferred
        // aiCall boundary; the deferred stub's Err is caught (never propagated), matching the JS
        // try/catch degrade-gracefully contract. ragRecall's own embed-Err is ALSO swallowed
        // separately (hits stays empty) — proven by reaching this far at all.
        let c = setup();
        let nid = a_notebook(&c, "My Notebook");
        let out = notebookGenerate(&json!({ "id": nid, "kind": "walkthrough", "focus": "the auth flow" }), &c).unwrap();
        assert_eq!(out["ok"], json!(false));
        assert!(out["error"].as_str().unwrap().starts_with("notebook-generate AI failed:"));
        // the prompt WAS ensured (ensure_prompt ran) before the deferred call.
        let n: i64 = c.query_row("SELECT COUNT(*) FROM brain.prompt_templates WHERE name = 'notebook.generate.v2'", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1);
        // no item was inserted — the wave never reached insertItem.
        let items: i64 = c.query_row("SELECT COUNT(*) FROM nb.notebook_items", [], |r| r.get(0)).unwrap();
        assert_eq!(items, 0);
    }

    #[test]
    fn default_kind_is_overview_when_omitted() {
        let c = setup();
        let nid = a_notebook(&c, "N");
        let out = notebookGenerate(&json!({ "id": nid }), &c).unwrap();
        // still degrades at the deferred boundary, but got there via the overview default path.
        assert_eq!(out["ok"], json!(false));
    }
}
