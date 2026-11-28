//! atlas-build.rs — twin of skills/codebase/atlas-build.mjs (db WRITE lane). NAMES ARE CONTRACT:
//! `atlasBuild` (default). Validate-then-write a batch of code-graph Blocks (code_nodes) + Bindings
//! (code_edges): every node.type must be in NODE_TYPES, every edge.type in EDGE_TYPES, and every edge
//! endpoint must resolve to a node id already in this batch or already stored — checked BEFORE any
//! write so a bad batch throws and leaves the graph clean (see code-schema.mjs for the table shapes).
use rusqlite::Connection;
use serde_json::{json, Value};
use std::collections::HashSet;

// Transcribed verbatim from skills/codebase/code-schema.mjs (data consts, not a skill call).
const NODE_TYPES: [&str; 11] = [
    "file", "function", "class", "method", "component", "hook",
    "condition", "loop", "import", "type", "const",
];
const EDGE_TYPES: [&str; 8] = [
    "imports", "calls", "uses", "renders", "dataflow", "extends", "implements", "dispatches",
];

/// The SQL predicate for "an import that couples at runtime". `kind` (code_edges) says what an
/// import edge means: `value` (NULL on rows written before the column existed), `dynamic`
/// (`import('./x')`), `reexport` (`export … from`) all execute; `type` (`import type`) is erased
/// by the compiler and is documentation, not coupling. Every reader that measures fan-in,
/// fan-out, blast radius, orphans, cycles or test coverage filters with this, so the one place
/// the rule lives is here.
pub const RUNTIME_IMPORTS: &str = "type = 'imports' AND COALESCE(kind, 'value') <> 'type'";

fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}
fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) } // JS `??` right operand
// = JS String(v) (no ??): absent -> "undefined", null -> "null", else the value's string form.
fn js_string(v: Option<&Value>) -> String {
    match v {
        None => "undefined".into(),
        Some(Value::Null) => "null".into(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(),
        Some(o) => o.to_string(),
    }
}
// = JS `${v}` template interpolation (used only inside guard-throw messages, not fixture-exercised).
fn tmpl(v: Option<&Value>) -> String { js_string(v) }

pub fn atlasBuild(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS: const { db } = context ?? {}; if (!db) throw err.contextMissing('atlas-build', 'db') —
    // the db lane always injects a live connection, so that guard is unreachable here.
    let origin_v = input.get("origin");
    if !truthy(origin_v) {
        return Err("atlas-build: origin is required".into()); // guard-throw, not fixture-exercised
    }
    let origin = origin_v.unwrap().clone();

    let empty: Vec<Value> = vec![];
    let nodes: &Vec<Value> = input.get("nodes").and_then(|v| v.as_array()).unwrap_or(&empty);
    let edges: &Vec<Value> = input.get("edges").and_then(|v| v.as_array()).unwrap_or(&empty);

    // ── validate everything BEFORE any write (clean-on-throw) ──
    let mut batch_ids: HashSet<String> = HashSet::new();
    for n in nodes {
        let id_v = n.get("id");
        if !truthy(id_v) {
            return Err("atlas-build: node.id is required".into()); // guard-throw, not fixture-exercised
        }
        let ty = n.get("type").and_then(|v| v.as_str());
        if !ty.map_or(false, |t| NODE_TYPES.contains(&t)) {
            return Err(format!( // guard-throw, not fixture-exercised
                "atlas-build: node.type must be one of: {} (got '{}')",
                NODE_TYPES.join(", "), tmpl(n.get("type"))
            ));
        }
        batch_ids.insert(js_string(id_v));
    }
    let existing_rows = crate::db_json::query_rows(db, "SELECT id FROM code_nodes", &[])?;
    let existing: HashSet<String> = existing_rows
        .as_array()
        .map(|a| a.iter().map(|r| js_string(r.get("id"))).collect())
        .unwrap_or_default();
    let resolvable = |id: Option<&Value>| -> bool {
        let s = js_string(id);
        batch_ids.contains(&s) || existing.contains(&s)
    };
    for e in edges {
        let ty = e.get("type").and_then(|v| v.as_str());
        if !ty.map_or(false, |t| EDGE_TYPES.contains(&t)) {
            return Err(format!( // guard-throw, not fixture-exercised
                "atlas-build: edge.type must be one of: {} (got '{}')",
                EDGE_TYPES.join(", "), tmpl(e.get("type"))
            ));
        }
        if !resolvable(e.get("from_node")) {
            return Err(format!("atlas-build: edge.from_node '{}' not found", tmpl(e.get("from_node")))); // guard-throw
        }
        if !resolvable(e.get("to_node")) {
            return Err(format!("atlas-build: edge.to_node '{}' not found", tmpl(e.get("to_node")))); // guard-throw
        }
    }

    // ── write (validation passed, so this can't half-apply on bad input) ──
    for n in nodes {
        let id_v = n.get("id");
        let span = match nz(n.get("span")) {
            Some(sp) => json!(serde_json::to_string(sp).map_err(|e| e.to_string())?),
            None => nz(n.get("span_json")).cloned().unwrap_or(Value::Null),
        };
        crate::db_json::run_write(
            db,
            "\n    INSERT OR REPLACE INTO code_nodes\n      (id, origin, path, type, name, signature, intent, parent_id, span_json, content_hash, version, exported)\n    VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)\n  ",
            &[
                json!(js_string(id_v)),
                origin.clone(),
                nz(n.get("path")).cloned().unwrap_or(json!(js_string(id_v))),
                n.get("type").cloned().unwrap_or(Value::Null),
                nz(n.get("name")).cloned().unwrap_or(Value::Null),
                nz(n.get("signature")).cloned().unwrap_or(Value::Null),
                nz(n.get("intent")).cloned().unwrap_or(Value::Null),
                nz(n.get("parent_id")).cloned().unwrap_or(Value::Null),
                span,
                nz(n.get("content_hash")).cloned().unwrap_or(Value::Null),
                nz(n.get("version")).cloned().unwrap_or(json!(1)),
                json!(if truthy(n.get("exported")) { 1 } else { 0 }),
            ],
        )?;
    }
    for e in edges {
        crate::db_json::run_write(
            db,
            "\n    INSERT INTO code_edges (origin, from_node, to_node, type, output, input, kind)\n    VALUES (?, ?, ?, ?, ?, ?, ?)\n  ",
            &[
                origin.clone(),
                json!(js_string(e.get("from_node"))),
                json!(js_string(e.get("to_node"))),
                e.get("type").cloned().unwrap_or(Value::Null),
                nz(e.get("output")).cloned().unwrap_or(Value::Null),
                nz(e.get("input")).cloned().unwrap_or(Value::Null),
                nz(e.get("kind")).cloned().unwrap_or(Value::Null),
            ],
        )?;
    }

    Ok(json!({
        "origin": origin,
        "nodes_written": nodes.len() as i64,
        "edges_written": edges.len() as i64,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCHEMA: &str = "
        CREATE TABLE code_nodes (
          id TEXT PRIMARY KEY, origin TEXT, path TEXT, type TEXT, name TEXT, signature TEXT,
          intent TEXT, parent_id TEXT, span_json TEXT, content_hash TEXT, version INTEGER, exported INTEGER
        );
        CREATE TABLE code_edges (
          id INTEGER PRIMARY KEY AUTOINCREMENT, origin TEXT, from_node TEXT, to_node TEXT,
          type TEXT, output TEXT, input TEXT
        , kind TEXT);
    ";

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(SCHEMA).unwrap();
        c
    }

    #[test]
    fn writes_nodes_with_defaults_and_edges() {
        let c = db();
        let r = atlasBuild(
            &json!({
                "origin": "atlas",
                "nodes": [
                    { "id": "n1", "type": "file" },
                    { "id": "n2", "type": "function", "path": "src/x.js", "name": "x", "signature": "()",
                      "intent": "does x", "parent_id": "n1", "span": { "start_line": 1, "end_line": 5 },
                      "content_hash": "abc", "version": 2, "exported": true }
                ],
                "edges": [{ "from_node": "n1", "to_node": "n2", "type": "imports", "output": "o", "input": "i" }]
            }),
            &c,
        )
        .unwrap();
        assert_eq!(r, json!({ "origin": "atlas", "nodes_written": 2, "edges_written": 1 }));

        let n1 = crate::db_json::query_rows(&c, "SELECT path, exported, version FROM code_nodes WHERE id='n1'", &[]).unwrap();
        assert_eq!(n1[0], json!({ "path": "n1", "exported": 0, "version": 1 })); // path ?? String(id)

        let n2 = crate::db_json::query_rows(&c, "SELECT span_json, exported, version FROM code_nodes WHERE id='n2'", &[]).unwrap();
        assert_eq!(n2[0]["span_json"], json!("{\"start_line\":1,\"end_line\":5}"));
        assert_eq!(n2[0]["exported"], json!(1));
        assert_eq!(n2[0]["version"], json!(2));

        let e = crate::db_json::query_rows(&c, "SELECT from_node, to_node, type, output, input FROM code_edges", &[]).unwrap();
        assert_eq!(e[0], json!({ "from_node": "n1", "to_node": "n2", "type": "imports", "output": "o", "input": "i" }));
    }

    #[test]
    fn span_json_fallback_used_verbatim_when_span_absent() {
        let c = db();
        atlasBuild(&json!({ "origin": "atlas", "nodes": [{ "id": "n1", "type": "file", "span_json": "{\"pre\":true}" }], "edges": [] }), &c).unwrap();
        let span: String = c.query_row("SELECT span_json FROM code_nodes WHERE id='n1'", [], |r| r.get(0)).unwrap();
        assert_eq!(span, "{\"pre\":true}");
    }

    #[test]
    fn insert_or_replace_same_id_twice_in_one_batch_last_write_wins() {
        let c = db();
        atlasBuild(&json!({ "origin": "atlas", "nodes": [
            { "id": "d1", "type": "file", "name": "first" },
            { "id": "d1", "type": "file", "name": "second" }
        ], "edges": [] }), &c).unwrap();
        let count: i64 = c.query_row("SELECT COUNT(*) FROM code_nodes", [], |r| r.get(0)).unwrap();
        assert_eq!(count, 1);
        let name: String = c.query_row("SELECT name FROM code_nodes WHERE id='d1'", [], |r| r.get(0)).unwrap();
        assert_eq!(name, "second");
    }

    #[test]
    fn edge_resolves_against_a_node_already_stored_not_in_this_batch() {
        let c = db();
        c.execute("INSERT INTO code_nodes (id, origin, path, type, version, exported) VALUES ('ext1','atlas','ext.js','file',1,0)", []).unwrap();
        let r = atlasBuild(&json!({ "origin": "atlas", "nodes": [{ "id": "n1", "type": "file" }],
            "edges": [{ "from_node": "n1", "to_node": "ext1", "type": "calls" }] }), &c).unwrap();
        assert_eq!(r, json!({ "origin": "atlas", "nodes_written": 1, "edges_written": 1 }));
    }

    #[test]
    fn empty_nodes_and_edges_is_a_no_op_success() {
        let c = db();
        let r = atlasBuild(&json!({ "origin": "atlas", "nodes": [], "edges": [] }), &c).unwrap();
        assert_eq!(r, json!({ "origin": "atlas", "nodes_written": 0, "edges_written": 0 }));
    }

    #[test]
    fn unknown_node_type_errors() {
        let c = db();
        assert!(atlasBuild(&json!({ "origin": "atlas", "nodes": [{ "id": "n1", "type": "bogus" }], "edges": [] }), &c).is_err());
    }
}
