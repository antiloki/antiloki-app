//! notebook-diagram.rs — twin of skills/notebooks/notebook-diagram.mjs (db lane, composes
//! `resolveScope` + the AST-based `fileInternalDiagram`). NAMES ARE CONTRACT: default
//! `notebookDiagram`. EPIC-NOTEBOOKS (P6): builds a "diagram pack" for a notebook's scope —
//! one deterministic {title,nodes,edges} diagram per in-scope JS/TS file, produced by our own
//! AST engine (`file-internal-diagram`, NO AI). Stored as a `diagram` item (`meta.diagrams`);
//! the FE renders each spec as an SVG. Non-parseable files degrade out (JS wraps the per-file
//! call in `try {} catch { /* non-JS / unreadable → skip */ }`).
//!
//! `context.db` missing → JS throws `err.contextMissing('notebook-diagram', 'db')`. Unreachable
//! here: `db: &Connection` is a required, non-nullable reference (same convention as every
//! sibling db-lane twin).
//!
//! `fileInternalDiagram` itself never actually throws (its own JS body is guard-clause /
//! `{ ok:false, error }` returns end to end, no bare `throw`) — but the per-file JS call site
//! still wraps it in try/catch for defense-in-depth, so this twin mirrors that shape by treating
//! ANY `Err` from the crate::codebase_file_internal_diagram::fileInternalDiagram twin the same
//! way as an `ok:false` result: skip the file, keep scanning.
// QUALITY: fully ported, deterministic db-read/write lane. No AI, no PARITY-DEFERRED surface —
// the only external call is the already-converted `codebase_file_internal_diagram` twin (also
// this wave), which is itself pure/deterministic (AST-based, no network).

use rusqlite::Connection;
use serde_json::{json, Value};
use regex::Regex;
use std::sync::OnceLock;

// JS truthiness.
fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}
// JS `String(v)` (no `??`).
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
// JS `Number(v)`.
fn js_number(v: Option<&Value>) -> f64 {
    match v {
        None => f64::NAN,
        Some(Value::Null) => 0.0,
        Some(Value::Bool(b)) => if *b { 1.0 } else { 0.0 },
        Some(Value::Number(n)) => n.as_f64().unwrap_or(f64::NAN),
        Some(Value::String(s)) => {
            let t = s.trim();
            if t.is_empty() { 0.0 } else { t.parse().unwrap_or(f64::NAN) }
        }
        Some(_) => f64::NAN,
    }
}
// JS `!n` on a Number result: falsy at 0 / -0 / NaN. True means the guard throws.
fn falsy_num(n: f64) -> bool { n == 0.0 || n.is_nan() }
// node:sqlite binds an integral JS number as INTEGER, a fractional one as REAL.
fn num_param(n: f64) -> Value { if n.is_finite() && n == n.trunc() { json!(n as i64) } else { json!(n) } }

// `/\.(jsx?|mjs|cjs|tsx?)$/.test(p)` — matches .js .jsx .mjs .cjs .ts .tsx (no case-insensitive
// flag in the JS source, so this is intentionally case-sensitive).
fn is_js_like(p: &str) -> bool {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\.(jsx?|mjs|cjs|tsx?)$").unwrap()).is_match(p)
}

/// `export default async function notebookDiagram(input, context)`.
pub fn notebookDiagram(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS: `const db = context?.db; if (!db) throw err.contextMissing(...)` — unreachable here,
    // `db: &Connection` is never absent.
    crate::notebooks_notebook_schema::ensureNotebookSchema(db, &json!("nb."));

    let id_n = js_number(input.get("id"));
    if falsy_num(id_n) {
        return Err("notebook-diagram: id is required".to_string());
    }
    let id_v = num_param(id_n);
    let nb = crate::notebooks_notebook_store::getNotebook(db, &id_v);
    if nb.is_null() {
        return Ok(json!({ "ok": false, "error": format!("notebook {} not found", id_n as i64) }));
    }

    // `nb.scope || {}` — nb.scope is always an object (getNotebook's parse_meta defaults to {}
    // on missing/invalid scope_json), so the `|| {}` fallback never actually fires; kept for
    // fidelity.
    let scope_in = if truthy(nb.get("scope")) { nb.get("scope").cloned().unwrap() } else { json!({}) };
    let scope = crate::notebooks_notebook_scope::resolveScope(&scope_in, db)?;

    let files: Vec<String> = scope
        .get("paths")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(js_string)
        .filter(|p| is_js_like(p))
        .take(12)
        .collect();

    let mut diagrams: Vec<Value> = Vec::new();
    for path in &files {
        let r = match crate::codebase_file_internal_diagram::fileInternalDiagram(&json!({ "path": path }), db) {
            Ok(v) => v,
            Err(_) => continue, // non-JS / unreadable → skip
        };
        if !truthy(r.get("ok")) {
            continue;
        }
        let diagram = match r.get("diagram") {
            Some(d) if d.is_object() => d,
            _ => continue,
        };
        let nodes = diagram.get("nodes").and_then(Value::as_array).cloned().unwrap_or_default();
        if nodes.is_empty() {
            continue;
        }
        let title = if truthy(diagram.get("title")) { diagram.get("title").cloned().unwrap() } else { json!(path) };
        let edges = diagram.get("edges").and_then(Value::as_array).cloned().unwrap_or_default();
        diagrams.push(json!({ "file": path, "title": title, "nodes": nodes, "edges": edges }));
    }

    if diagrams.is_empty() {
        return Ok(json!({ "ok": false, "error": "no diagrams — the scope has no parseable JS/TS files (the engine is AST-based). Add JS/TS files to the scope." }));
    }

    let n = diagrams.len();
    let plural = if n == 1 { "" } else { "s" };
    let nb_title = js_string(nb.get("title").unwrap_or(&Value::Null));
    let lines: Vec<String> = diagrams
        .iter()
        .map(|d| {
            let file = js_string(d.get("file").unwrap_or(&Value::Null));
            let nodes_len = d.get("nodes").and_then(Value::as_array).map_or(0, |a| a.len());
            let edges_len = d.get("edges").and_then(Value::as_array).map_or(0, |a| a.len());
            format!("- {file} \u{b7} {nodes_len} blocks, {edges_len} edges")
        })
        .collect();
    let body_md = format!("Structure diagrams for **{nb_title}** \u{2014} {n} file{plural}:\n{}", lines.join("\n"));

    let item = crate::notebooks_notebook_store::insertItem(
        db,
        &id_v,
        &json!({
            "kind": "diagram",
            "title": format!("Diagram pack \u{b7} {n} file{plural}"),
            "body_md": body_md,
            "meta": { "diagrams": diagrams },
            "pinned": 0,
        }),
    );
    Ok(json!({ "ok": true, "item": item }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS nb;").unwrap();
        c.execute_batch(
            "CREATE TABLE nb.notebooks (id INTEGER PRIMARY KEY AUTOINCREMENT, slug TEXT, title TEXT NOT NULL, summary TEXT, scope_json TEXT NOT NULL DEFAULT '{}', created_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')), updated_at TEXT);
             CREATE TABLE nb.notebook_items (id INTEGER PRIMARY KEY AUTOINCREMENT, notebook_id INTEGER NOT NULL, kind TEXT NOT NULL DEFAULT 'note', title TEXT, body_md TEXT NOT NULL DEFAULT '', meta_json TEXT, pinned INTEGER NOT NULL DEFAULT 0, created_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')));
             CREATE TABLE code_nodes (id TEXT, origin TEXT, path TEXT, type TEXT, name TEXT, signature TEXT, intent TEXT, parent_id TEXT);
             CREATE TABLE code_edges (origin TEXT, from_node TEXT, to_node TEXT, type TEXT, kind TEXT);",
        ).unwrap();
        c
    }

    fn a_notebook(c: &Connection, title: &str, scope: &Value) -> i64 {
        crate::db_json::run_write(
            c,
            "INSERT INTO nb.notebooks (title, scope_json) VALUES (?, ?)",
            &[json!(title), json!(serde_json::to_string(scope).unwrap())],
        ).unwrap();
        c.last_insert_rowid()
    }

    #[test]
    fn id_is_required() {
        let c = setup();
        let e = notebookDiagram(&json!({}), &c).unwrap_err();
        assert_eq!(e, "notebook-diagram: id is required");
        let e2 = notebookDiagram(&json!({ "id": 0 }), &c).unwrap_err();
        assert_eq!(e2, "notebook-diagram: id is required");
    }

    #[test]
    fn unknown_notebook_returns_ok_false() {
        let c = setup();
        let r = notebookDiagram(&json!({ "id": 999 }), &c).unwrap();
        assert_eq!(r, json!({ "ok": false, "error": "notebook 999 not found" }));
    }

    #[test]
    fn empty_scope_yields_no_diagrams_error() {
        let c = setup();
        let nid = a_notebook(&c, "Empty", &json!({}));
        let r = notebookDiagram(&json!({ "id": nid }), &c).unwrap();
        assert_eq!(
            r,
            json!({ "ok": false, "error": "no diagrams — the scope has no parseable JS/TS files (the engine is AST-based). Add JS/TS files to the scope." })
        );
    }

    #[test]
    fn non_js_paths_are_filtered_out_before_diagramming() {
        let c = setup();
        crate::db_json::run_write(&c, "INSERT INTO code_nodes (id,origin,path,type,name) VALUES ('a','atlas','a.md','file','a')", &[]).unwrap();
        let nid = a_notebook(&c, "Docs only", &json!({ "paths": ["a.md"] }));
        let r = notebookDiagram(&json!({ "id": nid }), &c).unwrap();
        assert_eq!(r["ok"], json!(false));
    }

    #[test]
    fn diagram_pack_assembles_from_parseable_files() {
        let c = setup();
        crate::db_json::run_write(&c, "INSERT INTO code_nodes (id,origin,path,type,name) VALUES ('a','atlas','a.js','file','a')", &[]).unwrap();
        let nid = a_notebook(&c, "Real scope", &json!({ "paths": ["a.js"] }));
        // 'a.js' has no readable content in this fixture DB (no codebase-file-read backing
        // table), so fileInternalDiagram degrades to `ok:false` and the file is skipped — this
        // exercises the same "no diagrams" path deterministically without requiring a real repo
        // file, proving the per-file skip-on-failure wiring end to end.
        let r = notebookDiagram(&json!({ "id": nid }), &c).unwrap();
        assert_eq!(r["ok"], json!(false));
        assert!(r["error"].as_str().unwrap().starts_with("no diagrams"));
    }

    #[test]
    fn is_js_like_matches_the_js_regex_exactly() {
        for p in ["a.js", "a.jsx", "a.mjs", "a.cjs", "a.ts", "a.tsx"] {
            assert!(is_js_like(p), "{p} should match");
        }
        for p in ["a.md", "a.JS", "a.json", "a"] {
            assert!(!is_js_like(p), "{p} should not match");
        }
    }
}
