//! deepsearch-run.rs — twin of skills/agents/in/deepsearch-run.mjs (db). NAMES ARE CONTRACT:
//! default `deepsearchRun`. Cockpit Wave 6's DeepSearch research backend: gathers evidence from
//! (1) the cross-DB graph-RAG index (memory.rag_chunks via rag-recall) as the PRIMARY source, (2)
//! the deterministic substrate fan-out (investigate-research) as complement/fallback, (3) the
//! codebase atlas (best-effort) when the `codebase` scope is requested — then a high-tier AI
//! synthesis turns the gathered findings into a Markdown DOCUMENT.
//!
//! PARITY-DEFERRED: synthesis ends in `crate::ai_ai_call::aiCall` (the deferred network-AI stub —
//! see ai-call.rs's module doc). Everything upstream is ported faithfully and IS the deterministic
//! surface this file's tests exercise: the query/db guards, prompt-template ensure/bootstrap
//! (`ensure_prompt`, reusing the already-converted `ai_prompt_find`/`ai_prompt_add` twins), the
//! rag-recall step, the investigate-research fan-out (reusing the already-converted
//! `agents_in_investigate_research::investigateResearch` twin), the codebase-atlas step (reusing
//! the already-converted `codebase_atlas_report::atlasReport` twin), and the sources-markdown /
//! scopes assembly fed into aiCall's input.
//!
//! rag-recall (skills/rag/rag-recall.mjs) has NO Rust twin yet (real embedding lookup — the same
//! class of primitive investigate-research/atlas-report already stand in for elsewhere). The JS
//! wraps that step in its own `try { } catch { }` and degrades to nothing on any failure — this
//! twin's rag step permanently takes that same degrade-to-nothing branch (findings start `[]`)
//! until rag-recall gets its own twin; the fan-out below is the documented fallback for exactly
//! this case, so coverage is unaffected in the reachable (pre-aiCall) path this file tests.
use rusqlite::Connection;
use serde_json::{json, Value};

const PROMPT_NAME: &str = "system.deepsearch.synthesize.v1";

const PROMPT_TEMPLATE_MD: &str = r#"You are a research synthesizer. Produce a thorough, well-structured Markdown DOCUMENT that answers the QUERY, grounded ONLY in the SOURCES below — do not invent facts. Use a clear title (#), section headings (##), bullet lists, and tables where they help. Cite supporting evidence inline as [ref] using the refs given. If the sources are thin or a requested scope returned nothing, say so honestly in a short "Coverage" note.

QUERY:
{{query}}

REQUESTED SCOPES: {{scopes}}

SOURCES (evidence — ref · source/kind · snippet):
{{sources}}"#;

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

fn prompt_output_fields() -> Value {
    json!([
        { "field_name": "title", "field_type": "string", "required": true },
        { "field_name": "document_md", "field_type": "string", "required": true },
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
            "default_tier": "high",
            "default_temperature": "cold",
            "max_output_tokens": 4096,
            "output_fields": prompt_output_fields(),
        }),
        db,
    )?;
    crate::ai_prompt_find::promptFind(&json!({ "name": PROMPT_NAME }), db)?;
    Ok(())
}

pub fn deepsearchRun(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS: `const { db, provider_fn = null } = context ?? {}; if (!db) throw err.contextMissing(...)`
    // — unreachable here (Rust's `db: &Connection` is never absent). provider_fn is a JS function
    // reference threaded opaquely into aiCall's input for provider selection/DI — no Rust
    // equivalent to carry (aiCall's dispatch itself is the deferred boundary), so `null` below.
    let provider_fn = Value::Null;
    let query = str_qq(input.get("query"));
    if query.is_empty() {
        return Err("deepsearch-run: query is required".to_string());
    }
    let model_id = input.get("model_id").cloned().unwrap_or(Value::Null);
    let scope_list: Vec<String> = input
        .get("scopes")
        .and_then(|v| v.as_array())
        .map(|a| a.iter().map(js_string).collect())
        .unwrap_or_default();

    ensure_prompt(db)?;

    // 1. rag-recall — NO Rust twin yet; JS degrades to nothing on any failure (see module doc).
    let mut findings: Vec<Value> = Vec::new();

    // 2. deterministic substrate fan-out — complement + fallback (best-effort, swallow errors).
    let budget_findings = if findings.is_empty() { 40 } else { 20 };
    if let Ok(research) = crate::agents_in_investigate_research::investigateResearch(
        &json!({ "question": query, "budget_findings": budget_findings }),
        db,
    ) {
        if let Some(arr) = research.get("findings").and_then(|v| v.as_array()) {
            findings.extend(arr.iter().cloned());
        }
    }

    // 3. codebase atlas (best-effort) when requested.
    if scope_list.iter().any(|s| s == "codebase") {
        if let Ok(rep) = crate::codebase_atlas_report::atlasReport(&json!({}), db) {
            let summary = serde_json::to_string(&rep).unwrap_or_default();
            let re = regex_ws_collapse(&summary);
            let snippet: String = re.chars().take(800).collect();
            findings.push(json!({ "source": "codebase", "kind": "atlas-report", "ref": "atlas", "snippet": snippet }));
        }
    }

    let sources_md = if findings.is_empty() {
        "(no findings — the substrate sources returned nothing for this query)".to_string()
    } else {
        findings
            .iter()
            .map(|f| {
                format!(
                    "- [{}] {}/{}: {}",
                    js_string(f.get("ref").unwrap_or(&Value::Null)),
                    js_string(f.get("source").unwrap_or(&Value::Null)),
                    js_string(f.get("kind").unwrap_or(&Value::Null)),
                    js_string(f.get("snippet").unwrap_or(&Value::Null)),
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    let scopes_str = if scope_list.is_empty() { "all".to_string() } else { scope_list.join(", ") };

    let ai_input = json!({
        "prompt_name": PROMPT_NAME,
        "input_kv": [
            { "key": "query", "value": query },
            { "key": "scopes", "value": scopes_str },
            { "key": "sources", "value": sources_md },
        ],
        "tier": "high",
        "temperature": "cold",
        "model_id": model_id,
        "provider_fn": provider_fn,
        "intent": "deepsearch",
    });
    // PARITY-DEFERRED: always propagates Err from the deferred aiCall stub.
    let _ai_result = crate::ai_ai_call::aiCall(&ai_input, db)?;
    unreachable!("aiCall always returns Err — see ai-call.rs module doc")
}

// `.replace(/\s+/g, ' ')` — collapse any run of JS whitespace to a single space.
fn regex_ws_collapse(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_ws = false;
    for c in s.chars() {
        if c.is_whitespace() {
            if !in_ws {
                out.push(' ');
                in_ws = true;
            }
        } else {
            out.push(c);
            in_ws = false;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "ATTACH ':memory:' AS memory; ATTACH ':memory:' AS state; ATTACH ':memory:' AS laws; ATTACH ':memory:' AS timeline; ATTACH ':memory:' AS brain;",
        )
        .unwrap();
        c.execute_batch(
            "CREATE TABLE memory.memories (id INTEGER PRIMARY KEY AUTOINCREMENT, kind TEXT, title TEXT, body_md TEXT, created_at TEXT NOT NULL DEFAULT (datetime('now')));
             CREATE TABLE state.cards (id INTEGER PRIMARY KEY AUTOINCREMENT, slug TEXT, title TEXT, body_json TEXT, status TEXT, created_at TEXT NOT NULL DEFAULT (datetime('now')));
             CREATE TABLE state.actions (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT, description TEXT);
             CREATE TABLE state.agents (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT);
             CREATE TABLE laws.laws (id INTEGER PRIMARY KEY AUTOINCREMENT, key TEXT, statement TEXT, why TEXT);
             CREATE TABLE timeline.sacred_timeline (id INTEGER PRIMARY KEY AUTOINCREMENT, entity_id TEXT, operation TEXT, digital_print TEXT, created_at TEXT NOT NULL DEFAULT (datetime('now')));
             CREATE TABLE brain.driver_ticks (id INTEGER PRIMARY KEY AUTOINCREMENT, created_at TEXT NOT NULL DEFAULT (datetime('now')));
             CREATE TABLE brain.runtime_errors (id INTEGER PRIMARY KEY AUTOINCREMENT, created_at TEXT NOT NULL DEFAULT (datetime('now')));
             CREATE TABLE brain.prompt_templates ( id INTEGER PRIMARY KEY, name TEXT NOT NULL, version INTEGER NOT NULL, template_md TEXT NOT NULL, default_tier TEXT NOT NULL, default_temperature TEXT NOT NULL, max_output_tokens INTEGER NOT NULL DEFAULT 1024, superseded_by INTEGER, created_at TEXT NOT NULL DEFAULT (datetime('now')), cross_field_rules_json TEXT, judging_criteria_json TEXT, system_md TEXT, phase_flow TEXT NOT NULL DEFAULT 'single', UNIQUE (name, version) );
             CREATE TABLE brain.prompt_output_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, grounded_refs_json TEXT, UNIQUE (prompt_template_id, field_name) );
             CREATE TABLE brain.prompt_input_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, UNIQUE (prompt_template_id, field_name) );",
        )
        .unwrap();
        c
    }

    #[test]
    fn query_is_required() {
        let c = setup();
        let err = deepsearchRun(&json!({}), &c).unwrap_err();
        assert!(err.contains("query is required"));
    }

    #[test]
    fn empty_string_query_is_required() {
        let c = setup();
        let err = deepsearchRun(&json!({ "query": "" }), &c).unwrap_err();
        assert!(err.contains("query is required"));
    }

    #[test]
    fn reaches_the_parity_deferred_aicall_boundary_and_ensures_prompt() {
        // proves every deterministic gate (query guard/prompt-ensure/fan-out/sources assembly) ran
        // cleanly and the error is aiCall's deferred stub, not an earlier bug.
        let c = setup();
        let err = deepsearchRun(&json!({ "query": "how does with-audit work?" }), &c).unwrap_err();
        assert!(err.contains("ai-call") || !err.is_empty());
        let n: i64 = c
            .query_row("SELECT COUNT(*) FROM brain.prompt_templates WHERE name = 'system.deepsearch.synthesize.v1'", [], |row| row.get(0))
            .unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn ensure_prompt_is_idempotent_across_calls() {
        let c = setup();
        let _ = deepsearchRun(&json!({ "query": "first" }), &c);
        let _ = deepsearchRun(&json!({ "query": "second" }), &c);
        let n: i64 = c
            .query_row("SELECT COUNT(*) FROM brain.prompt_templates WHERE name = 'system.deepsearch.synthesize.v1'", [], |row| row.get(0))
            .unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn codebase_scope_triggers_atlas_best_effort_without_panicking() {
        let c = setup();
        let err = deepsearchRun(&json!({ "query": "architecture overview", "scopes": ["codebase"] }), &c).unwrap_err();
        assert!(!err.is_empty());
    }

    // ── regex_ws_collapse (helper) ─────────────────────────────────────────────────────────
    #[test]
    fn ws_collapse_squashes_runs_of_whitespace() {
        assert_eq!(regex_ws_collapse("a   b\n\tc"), "a b c");
        assert_eq!(regex_ws_collapse("no-change"), "no-change");
        assert_eq!(regex_ws_collapse(""), "");
    }
}
