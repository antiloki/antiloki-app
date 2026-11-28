//! atlas-get.rs — twin of skills/codebase/atlas-get.mjs (db READ lane). NAMES ARE CONTRACT:
//! `atlasGet` (default export). Reads the atlas (or a blueprint partition) as {nodes, edges} for
//! a consumer to render: the FE graph, an AI/RAG slice, or another skill. See docs/codebase-atlas-epic.md.
//!
//! Opt-in `with_tokens` branch (real fs `statSync`, best-effort): reads each returned file's
//! on-disk byte size via the already-converted `engine_workspace_root::resolveWorkspacePath`
//! twin + `std::fs::metadata`, mirroring the JS's per-file `try {} catch {}` ("file gone from
//! disk" → silently skipped) and the outer `try {} catch {}` around the DISTINCT-path query
//! (query failure → empty file_tokens, matching the JS fallback). `with_tokens` defaults to
//! false/absent (JS: `=== true || === 1`, strict), so this genuinely-non-portable real-fs branch
//! is SKIPPABLE via the input flag (playbook lesson 5). The unit test below still exercises the
//! branch once, pointed at a workspace-relative path that does not exist on disk, to prove the
//! degrade-safely path (no `tokens` stamped, no panic, `total_tokens`/`file_count` stay 0) —
//! never a real repo file (that would make the test host-dependent).
// QUALITY: db-read lane w/ opt-in best-effort fs-stat sidecar (degrade-safely tested, not the happy-path stat)

use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::collections::HashSet;

fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}
// = JS String(v) (no ??): used only on an already-truthy value here (input.path).
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
fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) } // JS `??` right operand
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

pub fn atlasGet(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS: const { db } = context ?? {}; if (!db) throw err.contextMissing('atlas-get', 'db') —
    // the db lane always injects a live connection, so that guard is unreachable here (gotcha 9).
    let origin = nz(input.get("origin")).cloned().unwrap_or(json!("atlas"));

    // Math.min(Math.max(Number(input?.limit ?? 2000), 1), 10000) — success-path only (gotcha 9):
    // Rust's f64::max/min ignore NaN where JS's Math.max/min propagate it; not exercised by any
    // valid fixture input, so left as the natural Rust idiom rather than hand-rolling NaN parity.
    let limit_num = nz(input.get("limit")).map(|v| js_number(Some(v))).unwrap_or(2000.0);
    let limit: i64 = limit_num.max(1.0).min(10000.0) as i64;

    // input?.path ? String(input.path).replace(/\\/g, '/') : null — truthy gate, then String(v).
    let path: Option<String> = if truthy(input.get("path")) {
        Some(js_string(input.get("path").unwrap()).replace('\\', "/"))
    } else {
        None
    };

    // input?.with_tokens === true || input?.with_tokens === 1 (strict equality, not truthy).
    let with_tokens = match input.get("with_tokens") {
        Some(Value::Bool(true)) => true,
        Some(Value::Number(n)) => n.as_f64() == Some(1.0),
        _ => false,
    };

    // Optional `types: ["file", ...]` (2026-09-19): callers that only want the file tree (scope
    // rails, checkbox trees) filter server-side instead of pulling every symbol (≈4 MB for a
    // 6000-node atlas — the FE's 5 s budget aborted it and the rail read "atlas empty").
    let types: Vec<String> = input
        .get("types")
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|t| t.as_str().map(|s| s.to_string())).filter(|s| !s.is_empty()).collect())
        .unwrap_or_default();
    let type_clause = if types.is_empty() {
        String::new()
    } else {
        format!(" AND type IN ({})", types.iter().map(|_| "?").collect::<Vec<_>>().join(", "))
    };
    let type_params: Vec<Value> = types.iter().map(|t| json!(t)).collect();

    let mut nodes = if let Some(p) = &path {
        let mut params = vec![origin.clone(), json!(p)];
        params.extend(type_params.iter().cloned());
        params.push(json!(limit));
        crate::db_json::query_rows(
            db,
            &format!(
                "SELECT id, origin, path, type, name, signature, intent, parent_id, became, from_blueprint \
                 FROM code_nodes WHERE origin = ? AND path = ?{type_clause} ORDER BY type, name LIMIT ?"
            ),
            &params,
        )?
    } else if types.is_empty() {
        // THE WHOLE ATLAS = EVERY FILE + A BOUNDED SAMPLE OF THE REST (found 2026-09-29: a workspace with 26,941 nodes, 20,348 of them functions, showed only its first two
        // folders in X-ray). The old query took the first `limit` nodes of ANY kind in path order, so the symbols of the alphabetically first folders used the whole budget and
        // every later folder's files never left the database. The file tree needs every file (a few thousand rows at most); the budget applies to the symbols.
        let cols = "id, origin, path, type, name, signature, intent, parent_id, became, from_blueprint";
        let files = crate::db_json::query_rows(db, &format!("SELECT {cols} FROM code_nodes WHERE origin = ? AND type = 'file' ORDER BY path LIMIT 30000"), &[origin.clone()])?;
        let rest = crate::db_json::query_rows(db, &format!("SELECT {cols} FROM code_nodes WHERE origin = ? AND type != 'file' ORDER BY path, type, name LIMIT ?"), &[origin.clone(), json!(limit)])?;
        let mut all: Vec<Value> = files.as_array().cloned().unwrap_or_default();
        all.extend(rest.as_array().cloned().unwrap_or_default());
        let key = |n: &Value| (n.get("path").map(js_string).unwrap_or_default(), n.get("type").map(js_string).unwrap_or_default(), n.get("name").map(js_string).unwrap_or_default());
        all.sort_by(|a, b| key(a).cmp(&key(b)));
        Value::Array(all)
    } else {
        let mut params = vec![origin.clone()];
        params.extend(type_params.iter().cloned());
        params.push(json!(limit));
        crate::db_json::query_rows(
            db,
            &format!(
                "SELECT id, origin, path, type, name, signature, intent, parent_id, became, from_blueprint \
                 FROM code_nodes WHERE origin = ?{type_clause} ORDER BY path, type, name LIMIT ?"
            ),
            &params,
        )?
    };

    // Edges among the returned nodes (so the FE never gets a dangling endpoint).
    let ids: HashSet<String> = nodes
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|n| n.get("id").map(js_string))
        .collect();

    // JS binds `limit * 6` as a raw (possibly-fractional) number and lets SQLite truncate it at
    // LIMIT-evaluation time — NOT `limit` pre-truncated then multiplied. For an integer input
    // these coincide, but for a fractional one (e.g. 5.3) JS's int(5.3*6)=31 diverges from a
    // truncate-then-multiply int(5.3)*6=30. Multiplying in f64 before the single truncation below
    // reproduces JS's evaluation order.
    let all_edges = crate::db_json::query_rows(
        db,
        "SELECT from_node, to_node, type, kind FROM code_edges WHERE origin = ? ORDER BY CASE type WHEN 'imports' THEN 0 ELSE 1 END LIMIT ?",
        &[origin.clone(), json!((limit_num.max(1.0).min(10000.0) * 6.0) as i64)],
    )?;
    let edges: Vec<Value> = all_edges
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| {
            let f = e.get("from_node").map(js_string).unwrap_or_default();
            let t = e.get("to_node").map(js_string).unwrap_or_default();
            if path.is_some() { ids.contains(&f) || ids.contains(&t) } else { ids.contains(&f) && ids.contains(&t) }
        })
        .cloned()
        .collect();

    let node_count = nodes.as_array().unwrap().len();
    let edge_count = edges.len();

    // Opt-in per-file token estimate (best-effort real fs statSync — see module doc).
    let mut file_tokens: Map<String, Value> = Map::new();
    let mut total_tokens: i64 = 0;
    let mut file_count: i64 = 0;
    if with_tokens {
        let files: Vec<String> = if let Some(p) = &path {
            vec![p.clone()]
        } else {
            match crate::db_json::query_rows(
                db,
                "SELECT DISTINCT path FROM code_nodes WHERE origin = ? AND type = 'file' AND path IS NOT NULL",
                &[origin.clone()],
            ) {
                Ok(rows) => rows
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter_map(|r| r.get("path").and_then(|v| v.as_str()).map(|s| s.to_string()))
                    .collect(),
                Err(_) => Vec::new(), // outer try/catch: the DISTINCT-path query failed
            }
        };
        for f in &files {
            let stat = crate::engine_workspace_root::resolveWorkspacePath(db, &json!(f))
                .ok()
                .and_then(|r| r.as_str().and_then(|rp| std::fs::metadata(rp).ok()));
            if let Some(meta) = stat {
                let t = ((meta.len() as f64) / 4.0).ceil() as i64;
                file_tokens.insert(f.clone(), json!(t));
                total_tokens += t;
                file_count += 1;
            } // else: escapes workspace root / not on disk → skipped (JS's per-file catch)
        }
        if let Some(arr) = nodes.as_array_mut() {
            for n in arr.iter_mut() {
                let is_file = n.get("type").and_then(|v| v.as_str()) == Some("file");
                if !is_file { continue; }
                let node_path = n.get("path").and_then(|v| v.as_str()).map(|s| s.to_string());
                if let Some(np) = node_path {
                    if let Some(t) = file_tokens.get(&np).cloned() {
                        if let Value::Object(o) = n { o.insert("tokens".into(), t); }
                    }
                }
            }
        }
    }

    let mut out = Map::new();
    out.insert("origin".into(), origin);
    out.insert("nodes".into(), nodes);
    out.insert("edges".into(), json!(edges));
    out.insert("node_count".into(), json!(node_count as i64));
    out.insert("edge_count".into(), json!(edge_count as i64));
    if with_tokens {
        out.insert("file_tokens".into(), Value::Object(file_tokens));
        out.insert("total_tokens".into(), json!(total_tokens));
        out.insert("file_count".into(), json!(file_count));
    }
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCHEMA: &str = "
        CREATE TABLE code_nodes (
          id TEXT PRIMARY KEY, origin TEXT, path TEXT, type TEXT, name TEXT, signature TEXT,
          intent TEXT, parent_id TEXT, span_json TEXT, content_hash TEXT, version INTEGER,
          exported INTEGER, became TEXT, from_blueprint TEXT
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

    fn seed(c: &Connection) {
        crate::codebase_atlas_build::atlasBuild(
            &json!({
                "origin": "atlas",
                "nodes": [
                    { "id": "Button.tsx", "path": "Button.tsx", "type": "file" },
                    { "id": "Button.tsx#Button", "path": "Button.tsx", "type": "function", "name": "Button", "parent_id": "Button.tsx" },
                    { "id": "App.tsx", "path": "App.tsx", "type": "file" },
                    { "id": "App.tsx#App", "path": "App.tsx", "type": "function", "name": "App", "parent_id": "App.tsx" }
                ],
                "edges": [
                    { "from_node": "App.tsx", "to_node": "Button.tsx", "type": "imports" },
                    { "from_node": "App.tsx#App", "to_node": "Button.tsx#Button", "type": "calls" }
                ]
            }),
            c,
        )
        .unwrap();
    }

    #[test]
    fn returns_the_whole_atlas_as_nodes_and_edges() {
        let c = db();
        seed(&c);
        let r = atlasGet(&json!({ "origin": "atlas" }), &c).unwrap();
        assert_eq!(r["node_count"], json!(4));
        assert_eq!(r["edge_count"], json!(2));
        let edges = r["edges"].as_array().unwrap();
        assert!(edges.iter().any(|e| e["type"] == json!("imports")));
        assert!(edges.iter().any(|e| e["type"] == json!("calls")));
        let ids: HashSet<String> = r["nodes"].as_array().unwrap().iter().map(|n| n["id"].as_str().unwrap().to_string()).collect();
        assert!(edges.iter().all(|e| ids.contains(e["from_node"].as_str().unwrap()) && ids.contains(e["to_node"].as_str().unwrap())));
    }

    #[test]
    fn path_filter_restricts_to_one_file_plus_its_touching_edges() {
        let c = db();
        seed(&c);
        let r = atlasGet(&json!({ "origin": "atlas", "path": "App.tsx" }), &c).unwrap();
        assert!(r["nodes"].as_array().unwrap().iter().all(|n| n["path"] == json!("App.tsx")));
        assert_eq!(r["node_count"], json!(2)); // file + App fn
        assert!(r["edges"].as_array().unwrap().len() >= 1); // the App→Button edges touch App.tsx nodes
    }

    #[test]
    fn empty_origin_returns_empty_graph_not_an_error() {
        let c = db();
        let r = atlasGet(&json!({ "origin": "blueprint:nope@1" }), &c).unwrap();
        assert_eq!(
            r,
            json!({ "origin": "blueprint:nope@1", "nodes": [], "edges": [], "node_count": 0, "edge_count": 0 })
        );
    }

    /// THE TREE MUST NOT DEPEND ON HOW MANY FUNCTIONS THE FIRST FOLDERS HAVE (operator 2026-09-29: X-ray showed two folders of a repository with many).
    #[test]
    fn every_file_is_returned_however_many_symbols_come_first_in_path_order() {
        let c = db();
        c.execute_batch("DELETE FROM code_nodes; DELETE FROM code_edges;").unwrap();
        // 60 functions in `a/` (they would use the whole budget of 50), and files in `b/` and `z/`
        for i in 0..60 { c.execute("INSERT INTO code_nodes (id, origin, path, type, name) VALUES (?1, 'atlas', 'a/x.ts', 'function', ?2)", rusqlite::params![format!("f{i}"), format!("fn{i:02}")]).unwrap(); }
        for p in ["a/x.ts", "b/y.ts", "z/deep/w.ts"] { c.execute("INSERT INTO code_nodes (id, origin, path, type, name) VALUES (?1, 'atlas', ?1, 'file', ?1)", rusqlite::params![p]).unwrap(); }
        let r = atlasGet(&json!({ "limit": 50 }), &c).unwrap();
        let files: Vec<String> = r["nodes"].as_array().unwrap().iter().filter(|n| n["type"] == json!("file")).map(|n| n["path"].as_str().unwrap().to_string()).collect();
        assert_eq!(files, vec!["a/x.ts", "b/y.ts", "z/deep/w.ts"], "every file, in path order");
        let symbols = r["nodes"].as_array().unwrap().iter().filter(|n| n["type"] != json!("file")).count();
        assert_eq!(symbols, 50, "the budget applies to the symbols");
        assert_eq!(r["node_count"], json!(53));
    }

    #[test]
    fn types_filter_returns_only_the_requested_node_kinds() {
        let c = db();
        c.execute_batch(
            "INSERT INTO code_nodes (id, origin, path, type, name) VALUES
               ('a.ts', 'atlas', 'a.ts', 'file', 'a.ts'),
               ('a.ts#f', 'atlas', 'a.ts', 'function', 'f'),
               ('b.ts', 'atlas', 'b.ts', 'file', 'b.ts');",
        ).unwrap();
        let r = atlasGet(&json!({ "origin": "atlas", "types": ["file"] }), &c).unwrap();
        let kinds: Vec<&str> = r["nodes"].as_array().unwrap().iter().map(|n| n["type"].as_str().unwrap()).collect();
        assert_eq!(kinds, vec!["file", "file"]);
        assert_eq!(r["node_count"], json!(2));
        let all = atlasGet(&json!({ "origin": "atlas", "types": [] }), &c).unwrap();
        assert_eq!(all["node_count"], json!(3), "an empty list means no filter");
    }

    #[test]
    fn with_tokens_degrades_safely_when_the_file_is_not_on_disk() {
        let c = db();
        seed(&c);
        let r = atlasGet(&json!({ "origin": "atlas", "path": "Button.tsx", "with_tokens": true }), &c).unwrap();
        // resolveWorkspacePath resolves fine (no traversal), but the file doesn't exist on the
        // test host's filesystem — statSync throws, the per-file catch swallows it (lesson 5).
        assert_eq!(r["file_tokens"], json!({}));
        assert_eq!(r["total_tokens"], json!(0));
        assert_eq!(r["file_count"], json!(0));
        assert!(r["nodes"].as_array().unwrap().iter().all(|n| n.get("tokens").is_none()));
    }

    #[test]
    fn with_tokens_strict_equality_ignores_truthy_non_1_2_values() {
        let c = db();
        seed(&c);
        let r = atlasGet(&json!({ "origin": "atlas", "with_tokens": 2 }), &c).unwrap();
        assert!(r.get("file_tokens").is_none());
    }
}
