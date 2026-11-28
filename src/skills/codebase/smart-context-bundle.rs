//! smart-context-bundle.rs — twin of skills/codebase/smart-context-bundle.mjs. NAMES ARE CONTRACT:
//! `wholeCodebaseQ`, `isCodeQuestion`, `pathsFromHits` (named exports) + `smartContextBundle`
//! (default). Deterministic relevance-slicing of pre-computed code docs for a question: resolve
//! scope (name-index exact hits + RAG-ranked seeds), gather dense docs (`codeDocsBuild`, reused
//! from the already-converted `codebase_code_docs_build` twin), cap by an approximate token budget.
//!
//! `context.db` missing → JS throws `err.contextMissing('smart-context-bundle', 'db')`.
//! Unreachable here: `db: &Connection` is a required, non-nullable reference (same convention as
//! every sibling db-lane twin, e.g. `code-docs-overview.rs`).
//!
//! PARITY-DEFERRED boundary: `ragRecall` (`crate::rag_rag_recall::ragRecall`) bottoms out at the
//! real 768-dim ONNX embedding call, out of scope for this wave. JS wraps the `await ragRecall(...)`
//! in try/catch and silently degrades to the name-index-only seed set on ANY failure ("RAG optional
//! — degrade to the name index + the whole-codebase overview") — ported exactly: the `Err(_)` arm
//! is a no-op, mirroring the empty catch block. Everything else — the name-index scoring, the
//! seed selection/sort, scope classification, doc-page gathering, and the token-budget capping
//! loop — is fully deterministic and is ported + native-tested here, seeding `code_decorators`
//! rows directly (bypassing embed entirely, since name-index matches alone can drive `explicitFile`
//! and `scope` classification without ever calling `ragRecall`).
// QUALITY: 85/100 — deterministic assembly fully ported and tested; only the RAG seed-augmentation
// call is parity-deferred (embed boundary), matching JS's own degrade-on-failure semantics exactly.

use regex::Regex;
use rusqlite::Connection;
use serde_json::{json, Value};
use std::sync::LazyLock;

// ── JS coercion helpers (playbook) ──────────────────────────────────────────────────────────
fn js_float(f: f64) -> String {
    if f.is_nan() { return "NaN".into(); }
    if f == 0.0 { return "0".into(); }
    if f.is_infinite() { return if f > 0.0 { "Infinity".into() } else { "-Infinity".into() }; }
    format!("{}", f)
}
fn num_str(n: &serde_json::Number) -> String {
    if let Some(i) = n.as_i64() { i.to_string() }
    else if let Some(u) = n.as_u64() { u.to_string() }
    else { js_float(n.as_f64().unwrap_or(f64::NAN)) }
}
fn js_string(v: &Value) -> String {
    match v {
        Value::Null => "null".into(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => num_str(n),
        Value::String(s) => s.clone(),
        Value::Array(a) => a.iter().map(js_string).collect::<Vec<_>>().join(","),
        Value::Object(_) => "[object Object]".into(),
    }
}
fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) }
fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
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
} // = JS Number(v)

// ── regexes (compiled once) ─────────────────────────────────────────────────────────────────
static WHOLE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(the code|codebase|the project|architecture|overall|in general|health|quality|technical debt|overview|structure|smells?|what'?s? (wrong|not great|not good|bad)|improvements?)\b").unwrap()
});
static CODE_INTENT_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(code|codebase|file|function|class|module|component|hook|method|import|dependenc|architecture|refactor|bug|test|coverage|complexity|maintainab|smell|implementation|how (is|does|are)\b[^?]*\b(implement|work|built|structured|organi[sz]ed))\b").unwrap()
});
static EXT_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)\.[a-z]{1,5}$").unwrap());

// = `String(v ?? '')` (nullish coalescing — only null/undefined fall back)
fn nullish_str(v: &Value) -> String { match nz(Some(v)) { Some(x) => js_string(x), None => String::new() } }
// = `String(v || '')` (logical-OR — any falsy value, incl. 0/''/false, falls back)
fn or_str(v: &Value) -> String { if truthy(Some(v)) { js_string(v) } else { String::new() } }
// = JS `wholeCodebaseQ(question)`
pub fn wholeCodebaseQ(question: &Value) -> bool { WHOLE_RE.is_match(&or_str(question)) }
// = JS `isCodeQuestion(question)`
pub fn isCodeQuestion(question: &Value) -> bool {
    wholeCodebaseQ(question) || CODE_INTENT_RE.is_match(&or_str(question))
}
fn approx_tok(s: &str) -> i64 { ((s.chars().count() as f64) / 4.0).ceil() as i64 } // = APPROX_TOK

/// = JS `pathsFromHits(hits)`. Pure — RAG hits → `{path, score}` pairs (code chunks only).
pub fn pathsFromHits(hits: &Value) -> Vec<(String, f64)> {
    let mut out = Vec::new();
    let Some(arr) = hits.as_array() else { return out };
    for h in arr {
        if h.get("source_db").and_then(Value::as_str) != Some("code") { continue; }
        let raw = match nz(h.get("source_row")) { Some(v) => js_string(v), None => String::new() };
        let p = raw.split('@').next().unwrap_or("").split('#').next().unwrap_or("").to_string();
        if !p.is_empty() && p.contains('/') && EXT_RE.is_match(&p) {
            out.push((p, js_number(h.get("score"))));
        }
    }
    out
}

// ── best-effort chat-trace insert (mirrors JS's try/catch, silently swallows errors) ──────────
fn to_sql_scalar(v: &Value) -> rusqlite::types::Value {
    use rusqlite::types::Value as SqlValue;
    match v {
        Value::Null => SqlValue::Null,
        Value::Bool(b) => SqlValue::Integer(if *b { 1 } else { 0 }),
        Value::Number(n) => match n.as_i64() { Some(i) => SqlValue::Integer(i), None => SqlValue::Real(n.as_f64().unwrap_or(0.0)) },
        Value::String(s) => SqlValue::Text(s.clone()),
        _ => SqlValue::Text(js_string(v)),
    }
}
fn trace(db: &Connection, trace_turn: Option<&Value>, skill: &str, detail: Option<Value>) {
    let Some(tt) = nz(trace_turn) else { return };
    let detail_json = detail.map(|d| serde_json::to_string(&d).unwrap_or_default());
    let _ = db.execute(
        "INSERT INTO runtime_events (actor, agent, skill, outcome, user_goal_id, source, context_json) VALUES ('interviewer', 'in', ?1, 'success', ?2, 'chat', ?3)",
        rusqlite::params![skill, to_sql_scalar(tt), detail_json],
    );
}

// ordered score map: insertion-ordered like a JS Map, ties preserved via Rust's stable sort.
fn bump(order: &mut Vec<String>, score: &mut std::collections::HashMap<String, f64>, pages: &Value, p: &str, n: f64) {
    if p.is_empty() { return; }
    if !truthy(pages.get(p)) { return; }
    if !score.contains_key(p) { order.push(p.to_string()); }
    *score.entry(p.to_string()).or_insert(0.0) += n;
}

pub fn smartContextBundle(input: &Value, db: &Connection) -> Result<Value, String> {
    let question = nullish_str(input.get("question").unwrap_or(&Value::Null)).trim().to_string();
    if question.is_empty() { return Err("smart-context-bundle: question is required".to_string()); }
    let origin = nz(input.get("origin")).cloned().unwrap_or(json!("atlas"));
    let budget = match nz(input.get("token_budget")) { Some(v) => js_number(Some(v)), None => 20000.0 };
    let trace_turn = nz(input.get("trace_turn_id")).cloned();

    let site = crate::codebase_code_docs_build::codeDocsBuild(&json!({ "build": true, "origin": origin.clone() }), db)?;
    if !truthy(site.get("found")) {
        return Ok(json!({ "ok": true, "scope": "none", "bundle_md": "_No codebase documentation yet \u{2014} rebuild the atlas (\u{21bb})._", "sources": [], "tokens": 0, "dropped": 0 }));
    }
    let empty_pages = json!({});
    let pages = site.get("pages").unwrap_or(&empty_pages);

    // 1. RESOLVE — exact name-index match + semantic RAG (RAG best-effort, degrades silently).
    let ql = question.to_lowercase();
    let mut score_order: Vec<String> = Vec::new();
    let mut score: std::collections::HashMap<String, f64> = std::collections::HashMap::new();
    if let Some(idx) = site.get("index").and_then(Value::as_array) {
        for e in idx {
            let path = e.get("path").cloned().unwrap_or(Value::Null);
            let path_s = js_string(&path);
            let base = path_s.rsplit('/').next().unwrap_or("").to_lowercase();
            if base.chars().count() >= 5 && ql.contains(&base) { bump(&mut score_order, &mut score, pages, &path_s, 3.0); }
            let nm = js_string(e.get("name").unwrap_or(&Value::Null)).to_lowercase();
            if nm.chars().count() >= 5 && ql.contains(&nm) { bump(&mut score_order, &mut score, pages, &path_s, 2.0); }
        }
    }
    let explicit_file = score.values().any(|v| *v >= 3.0);

    match crate::rag_rag_recall::ragRecall(&json!({ "query": question, "k": 10 }), &Value::Null, db) {
        Ok(r) => {
            let hits = r.get("hits").cloned().unwrap_or(json!([]));
            let n_hits = hits.as_array().map_or(0, |a| a.len());
            for (path, s) in pathsFromHits(&hits) { bump(&mut score_order, &mut score, pages, &path, s); }
            let q_trunc: String = question.chars().take(80).collect();
            trace(db, trace_turn.as_ref(), "rag-recall", Some(json!({ "phase": "search", "hits": n_hits, "query": q_trunc })));
        }
        Err(_) => { /* RAG optional — degrade to the name index + the whole-codebase overview */ }
    }

    let mut entries: Vec<(String, f64)> = score_order.iter().map(|p| (p.clone(), score[p])).collect();
    entries.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    let seeds: Vec<String> = entries.into_iter().take(4).map(|(p, _)| p).collect();

    // 2. CLASSIFY SCOPE.
    let scope = if explicit_file { "file" }
        else if wholeCodebaseQ(&json!(question)) || seeds.is_empty() { "whole-codebase" }
        else { "file" };

    // 3. GATHER dense docs (no raw code).
    let mut parts: Vec<String> = Vec::new();
    let mut sources: Vec<Value> = Vec::new();
    let add_page = |page: &str, kind: &str, header: &str, parts: &mut Vec<String>, sources: &mut Vec<Value>| -> Result<(), String> {
        let pg = crate::codebase_code_docs_build::codeDocsBuild(&json!({ "page": page, "format": "md", "origin": origin.clone() }), db)?;
        if let Some(md) = pg.get("markdown").filter(|v| truthy(Some(*v))) {
            parts.push(format!("{}\n\n{}", header, js_string(md)));
            sources.push(json!({ "kind": kind, "path": page }));
        }
        Ok(())
    };
    if scope == "whole-codebase" {
        add_page("dir:", "overview", "# Codebase overview \u{2014} digest \u{00B7} health \u{00B7} smells \u{00B7} worst files (deterministic)", &mut parts, &mut sources)?;
    } else {
        for p in &seeds { add_page(p, "file", &format!("# {}", p), &mut parts, &mut sources)?; }
    }
    let files_trace: Vec<Value> = sources.iter().take(6).map(|s| s.get("path").cloned().unwrap_or(Value::Null)).collect();
    trace(db, trace_turn.as_ref(), "docs-gather", Some(json!({ "phase": "read", "scope": scope, "pages": sources.len(), "files": files_trace })));

    // 4. CAP by token budget (strongest first; log what's dropped — no silent truncation).
    let mut tokens = 0i64;
    let mut dropped = 0i64;
    let mut kept: Vec<String> = Vec::new();
    for part in parts {
        let t = approx_tok(&part);
        if !kept.is_empty() && (tokens + t) as f64 > budget { dropped += 1; continue; }
        tokens += t;
        kept.push(part);
    }

    let mut bundle_md = kept.join("\n\n---\n\n");
    let mut out = json!({ "ok": true, "scope": scope, "bundle_md": "", "sources": sources, "tokens": tokens, "dropped": dropped });
    // WHERE THIS INTEL STOPS (2026-10-01): a caller that hands the bundle to an agent asks for `coverage`, and the bundle then ends with the
    // files the question touches that the index does not cover (or covers stale), and the folders it never looks in. An intel-only agent
    // used to inherit those blind spots as if there were none - it missed the Rust port of a message because `crates/` was excluded.
    if truthy(input.get("coverage")) {
        let root = crate::engine_workspace_root::getWorkspaceRoot(db);
        let root = js_string(&root);
        if std::path::Path::new(&root).is_dir() {
            let origin_s = js_string(&origin);
            let files: Vec<String> = sources.iter().filter(|s| s.get("kind") == Some(&json!("file"))).filter_map(|s| s.get("path").and_then(Value::as_str).map(String::from)).collect();
            let rep = crate::codebase_coverage_guard::check(db, std::path::Path::new(&root), &origin_s, &question, &files);
            let notice = crate::codebase_coverage_guard::notice_md(&rep);
            bundle_md = if bundle_md.is_empty() { notice.clone() } else { format!("{bundle_md}\n\n---\n\n{notice}") };
            out["tokens"] = json!(tokens + approx_tok(&notice));
            out["coverage"] = crate::codebase_coverage_guard::report_json(&rep);
        }
    }
    out["bundle_md"] = json!(bundle_md);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE code_decorators (path TEXT, name TEXT, kind TEXT, role TEXT, line INTEGER, end_line INTEGER, signature TEXT, summary TEXT, inputs_json TEXT, output_json TEXT, effects_json TEXT, calls_json TEXT, uses_json TEXT, complexity INTEGER, flags_json TEXT, steps_json TEXT, origin TEXT);
             CREATE TABLE runtime_events (id INTEGER PRIMARY KEY AUTOINCREMENT, ts TEXT NOT NULL DEFAULT '2026-01-01 00:00:00', parent_event_id INTEGER, actor TEXT NOT NULL, agent TEXT, action TEXT, skill TEXT, duration_ms REAL, outcome TEXT NOT NULL, error_code TEXT, context_json TEXT, user_goal_id INTEGER, source TEXT);",
        ).unwrap();
        c
    }
    fn seed_file(c: &Connection, path: &str, name: &str) {
        c.execute(
            "INSERT INTO code_decorators (path, name, kind, role, line, end_line, signature, summary, inputs_json, output_json, effects_json, calls_json, uses_json, complexity, flags_json, steps_json, origin) VALUES (?1, ?2, 'function', 'logic', 1, 5, ?3, 'does a thing', '[]', 'null', '[]', '[]', '[]', 1, '[\"exported\"]', '[]', 'atlas')",
            rusqlite::params![path, name, format!("{}()", name)],
        ).unwrap();
    }

    /// The bundle an agent is handed ends with where the index stops (2026-10-01) - only when the caller asks, so the interviewer's chat answers are unchanged.
    #[test]
    fn a_bundle_for_an_agent_ends_with_the_files_the_index_does_not_cover() {
        let root = std::env::temp_dir().join(format!("antiloki-bundle-cov-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::create_dir_all(root.join("port")).unwrap();
        std::fs::write(root.join("src/auth-login.mjs"), "export function authLogin() { return 'weird_marker_string'; }\n").unwrap();
        std::fs::write(root.join("port/login.py"), "def auth_login():\n    return 'weird_marker_string'\n").unwrap();
        let _ = crate::proc::cmd("git").args(["init", "-q"]).current_dir(&root).output();
        let _ = crate::proc::cmd("git").args(["add", "-A"]).current_dir(&root).output();
        let c = setup();
        seed_file(&c, "src/auth-login.mjs", "authLogin");
        c.execute_batch(&format!(
            "CREATE TABLE workspaces (id INTEGER PRIMARY KEY, name TEXT, root_path TEXT, vcs TEXT, is_active INTEGER, created_at TEXT); \
             INSERT INTO workspaces VALUES (1,'w','{}','git',1,'2026-01-01'); \
             CREATE TABLE code_nodes (id TEXT, origin TEXT, name TEXT, path TEXT, type TEXT, content_hash TEXT); \
             INSERT INTO code_nodes VALUES ('src/auth-login.mjs','atlas','auth-login.mjs','src/auth-login.mjs','file','stale');",
            root.to_string_lossy().replace('\'', "''"))).unwrap();
        let q = "how does auth-login.mjs work? it returns 'weird_marker_string' and \"weird_marker_string\"";

        let plain = smartContextBundle(&json!({ "question": q }), &c).unwrap();
        assert!(plain.get("coverage").is_none() && !plain["bundle_md"].as_str().unwrap().contains("Coverage of this intel"), "off unless asked");

        let out = smartContextBundle(&json!({ "question": q, "coverage": true }), &c).unwrap();
        let md = out["bundle_md"].as_str().unwrap();
        assert!(md.contains("src/auth-login.mjs"), "the bundle is still the bundle");
        assert!(md.contains("## Coverage of this intel - read before you trust it"), "{md}");
        assert!(md.contains("- NOT INDEXED: `port/login.py` - not in the index"), "{md}");
        assert!(md.contains("- CHANGED SINCE THE INDEX: `src/auth-login.mjs`"), "{md}");
        assert_eq!(out["coverage"]["gaps"].as_array().unwrap().len(), 2);
        assert!(out["tokens"].as_i64().unwrap() > plain["tokens"].as_i64().unwrap());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn wholeCodebaseQ_matches_broad_terms_only() {
        assert!(wholeCodebaseQ(&json!("what's the overall code quality?")));
        assert!(wholeCodebaseQ(&json!("any technical debt in here?")));
        assert!(!wholeCodebaseQ(&json!("does auth.rs export login?")));
    }

    #[test]
    fn isCodeQuestion_covers_whole_and_narrow_code_intent() {
        assert!(isCodeQuestion(&json!("what's the codebase health?")));
        assert!(isCodeQuestion(&json!("how does the login function work?")));
        assert!(!isCodeQuestion(&json!("what did we decide about pricing?")));
    }

    #[test]
    fn pathsFromHits_filters_to_code_chunks_with_extension() {
        let hits = json!([
            { "source_db": "code", "source_row": "src/foo.rs@atlas", "score": 0.9 },
            { "source_db": "code", "source_row": "src/bar.rs#Symbol", "score": 0.5 },
            { "source_db": "memory", "source_row": "src/baz.rs", "score": 0.7 },
            { "source_db": "code", "source_row": "noext", "score": 0.3 },
            { "source_db": "code", "source_row": "no/slash", "score": 0.3 },
        ]);
        let out = pathsFromHits(&hits);
        assert_eq!(out, vec![("src/foo.rs".to_string(), 0.9), ("src/bar.rs".to_string(), 0.5)]);
    }

    #[test]
    fn no_documentation_yet_returns_none_scope() {
        let c = setup();
        let out = smartContextBundle(&json!({ "question": "how does auth work?" }), &c).unwrap();
        assert_eq!(out["scope"], json!("none"));
        assert_eq!(out["sources"], json!([]));
    }

    #[test]
    fn blank_question_is_rejected() {
        let c = setup();
        let err = smartContextBundle(&json!({ "question": "   " }), &c).unwrap_err();
        assert_eq!(err, "smart-context-bundle: question is required");
    }

    #[test]
    fn explicit_basename_match_scopes_to_file_and_bundles_its_doc() {
        let c = setup();
        seed_file(&c, "src/auth-login.mjs", "authLogin");
        let out = smartContextBundle(&json!({ "question": "how does auth-login.mjs work?" }), &c).unwrap();
        assert_eq!(out["scope"], json!("file"));
        let sources = out["sources"].as_array().unwrap();
        assert_eq!(sources.len(), 1);
        assert_eq!(sources[0]["path"], json!("src/auth-login.mjs"));
        assert!(out["bundle_md"].as_str().unwrap().contains("src/auth-login.mjs"));
    }

    #[test]
    fn whole_codebase_question_with_no_seeds_falls_back_to_overview() {
        let c = setup();
        seed_file(&c, "src/whatever.mjs", "doThing");
        let out = smartContextBundle(&json!({ "question": "what's the overall architecture?" }), &c).unwrap();
        assert_eq!(out["scope"], json!("whole-codebase"));
        let sources = out["sources"].as_array().unwrap();
        assert_eq!(sources.len(), 1);
        assert_eq!(sources[0]["kind"], json!("overview"));
        assert_eq!(sources[0]["path"], json!("dir:"));
    }

    #[test]
    fn narrow_question_with_no_name_match_and_no_rag_falls_back_to_whole_codebase() {
        // no explicit-file hit, ragRecall unreachable (deferred embed) → seeds stay empty →
        // scope falls back to whole-codebase even though the question isn't a WHOLE_RE match.
        let c = setup();
        seed_file(&c, "src/something-else.mjs", "otherThing");
        let out = smartContextBundle(&json!({ "question": "does it support retries?" }), &c).unwrap();
        assert_eq!(out["scope"], json!("whole-codebase"));
    }

    #[test]
    fn token_budget_caps_and_reports_dropped_without_silent_truncation() {
        let c = setup();
        seed_file(&c, "src/one.mjs", "one");
        seed_file(&c, "src/two.mjs", "two");
        seed_file(&c, "src/three.mjs", "three");
        let out = smartContextBundle(&json!({ "question": "how do one.mjs two.mjs three.mjs work?", "token_budget": 1 }), &c).unwrap();
        // first kept part always lands regardless of budget (kept.length gate mirrors JS: `if (kept.length && ...)`);
        // remaining sources over budget are dropped and counted, never silently merged away.
        let sources = out["sources"].as_array().unwrap();
        assert_eq!(sources.len(), 3);
        assert!(out["dropped"].as_i64().unwrap() >= 1);
        assert!(out["tokens"].as_i64().unwrap() >= 1);
    }

    #[test]
    fn trace_turn_id_writes_runtime_events_when_present() {
        let c = setup();
        seed_file(&c, "src/foo.mjs", "foo");
        let out = smartContextBundle(&json!({ "question": "how does foo.mjs work?", "trace_turn_id": 42 }), &c).unwrap();
        assert_eq!(out["scope"], json!("file"));
        let n: i64 = c.query_row("SELECT COUNT(*) FROM runtime_events WHERE skill = 'docs-gather'", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1);
        let uid: i64 = c.query_row("SELECT user_goal_id FROM runtime_events WHERE skill = 'docs-gather'", [], |r| r.get(0)).unwrap();
        assert_eq!(uid, 42);
    }

    #[test]
    fn no_trace_turn_id_writes_nothing() {
        let c = setup();
        seed_file(&c, "src/foo.mjs", "foo");
        smartContextBundle(&json!({ "question": "how does foo.mjs work?" }), &c).unwrap();
        let n: i64 = c.query_row("SELECT COUNT(*) FROM runtime_events", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 0);
    }
}
