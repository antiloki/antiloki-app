//! blueprint-parse.rs — twin of skills/blueprint/blueprint-parse.mjs (db READ lane). NAMES ARE
//! CONTRACT: `blueprintParse` (default export). Runs AI-written stub files through the SAME
//! atlas parser (ast-parse -> dep-resolve/symbol-table-build/binding-resolve/flow-resolve) as
//! atlas-ingest, then diffs the planned nodes against the baseline atlas (origin='atlas' by
//! default) to produce an added/changed/unchanged/removed delta. Pure compute — reads
//! code_nodes, writes nothing.
use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::collections::{HashMap, HashSet};

fn str_qq(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(),
        Some(o) => o.to_string(),
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
fn norm(v: Option<&Value>) -> String {
    str_qq(v).replace('\\', "/")
}

#[allow(non_snake_case)]
pub fn blueprintParse(input: &Value, db: &Connection) -> Result<Value, String> {
    let stubs = match input.get("stub_files").and_then(|v| v.as_array()) {
        Some(a) if !a.is_empty() => a,
        _ => return Err("blueprint-parse: stub_files is required".into()),
    };
    let baseline_origin = match input.get("baseline_origin").filter(|v| !v.is_null()) {
        Some(Value::String(s)) => s.clone(),
        Some(v) => js_string(v),
        None => "atlas".to_string(),
    };

    // ── parse each stub through the language-agnostic atlas parser (same shape) ──
    let mut all_nodes: Vec<Value> = Vec::new();
    let mut all_imports: Vec<Value> = Vec::new();
    let mut all_calls: Vec<Value> = Vec::new();
    let mut file_sources: Vec<(String, String)> = Vec::new(); // for flow-resolve
    let mut errors: Vec<Value> = Vec::new();

    for f in stubs {
        let path = norm(f.get("path"));
        let source = str_qq(f.get("source"));
        if path.is_empty() {
            errors.push(json!({ "path": "(none)", "error": "stub missing path" }));
            continue;
        }
        file_sources.push((path.clone(), source.clone()));

        match crate::codebase_ast_parse::astParse(&json!({ "path": path, "source": source }), db) {
            Ok(Value::Null) => {
                // destructuring {nodes,imports,calls} off null in JS throws a TypeError, caught
                // by the same catch as a real parse error — a bare file node stands in.
                errors.push(json!({ "path": path, "error": "Cannot destructure property 'nodes' of 'null' as it is null." }));
                all_nodes.push(json!({ "id": path, "path": path, "type": "file", "content_hash": crate::codebase_atlas_ingest::hashSource(&source) }));
            }
            Ok(parsed) => {
                let mut nodes: Vec<Value> = parsed.get("nodes").and_then(|v| v.as_array()).cloned().unwrap_or_default();
                let imports: Vec<Value> = parsed.get("imports").and_then(|v| v.as_array()).cloned().unwrap_or_default();
                let calls: Vec<Value> = parsed.get("calls").and_then(|v| v.as_array()).cloned().unwrap_or_default();

                for n in nodes.iter_mut() {
                    if n.get("type").and_then(|v| v.as_str()) == Some("file") {
                        if let Some(obj) = n.as_object_mut() {
                            obj.insert("content_hash".to_string(), json!(crate::codebase_atlas_ingest::hashSource(&source)));
                        }
                        break; // .find() — first match only
                    }
                }
                all_nodes.extend(nodes);
                for imp in &imports {
                    let specifier = imp.get("specifier").cloned().unwrap_or(Value::Null);
                    all_imports.push(json!({ "from_path": path, "specifier": specifier }));
                }
                for c in &calls {
                    let caller = c.get("caller").cloned().unwrap_or(Value::Null);
                    let callee = c.get("callee").cloned().unwrap_or(Value::Null);
                    all_calls.push(json!({ "from_path": path, "caller": caller, "callee": callee }));
                }
            }
            Err(e) => {
                errors.push(json!({ "path": path, "error": e }));
                all_nodes.push(json!({ "id": path, "path": path, "type": "file", "content_hash": crate::codebase_atlas_ingest::hashSource(&source) }));
            }
        }
    }

    // ── resolve edges in the atlas shape. universe = planned files UNION current atlas files ──
    let planned_file_ids: Vec<String> = all_nodes
        .iter()
        .filter(|n| n.get("type").and_then(|v| v.as_str()) == Some("file"))
        .map(|n| js_string(n.get("id").unwrap_or(&Value::Null)))
        .collect();
    let atlas_file_ids: Vec<String> = crate::db_json::query_rows(
        db,
        "SELECT id FROM code_nodes WHERE origin = ? AND type = 'file'",
        &[json!(baseline_origin)],
    )
    .ok()
    .and_then(|r| r.as_array().cloned())
    .unwrap_or_default()
    .iter()
    .filter_map(|r| r.get("id").map(js_string))
    .collect();

    let mut universe: Vec<Value> = Vec::new();
    let mut seen_universe: HashSet<String> = HashSet::new();
    for id in planned_file_ids.iter().chain(atlas_file_ids.iter()) {
        if seen_universe.insert(id.clone()) {
            universe.push(json!(id));
        }
    }

    let dep_result = crate::codebase_dep_resolve::depResolve(
        &json!({ "files": universe, "imports": all_imports, "config": {} }),
        db,
    )?;
    let import_edges: Vec<Value> = dep_result.get("edges").and_then(|v| v.as_array()).cloned().unwrap_or_default();

    let symbols = crate::codebase_symbol_table_build::symbolTableBuild(&json!({ "nodes": all_nodes }), db)?;

    let call_result = crate::codebase_binding_resolve::bindingResolve(
        &json!({ "calls": all_calls, "symbols": symbols, "importEdges": import_edges }),
        db,
    )?;
    let call_edges: Vec<Value> = call_result.get("edges").and_then(|v| v.as_array()).cloned().unwrap_or_default();

    let file_sources_json: Vec<Value> = file_sources.iter().map(|(p, s)| json!({ "path": p, "source": s })).collect();
    let flow_result = crate::codebase_flow_resolve::resolveFlow(&json!(file_sources_json));
    let flow_edges: Vec<Value> = flow_result.as_array().cloned().unwrap_or_default();

    let mut edges: Vec<Value> = Vec::new();
    edges.extend(import_edges);
    edges.extend(call_edges);
    edges.extend(flow_edges);

    // ── delta vs the baseline atlas, keyed by node id ──
    let mut baseline: HashMap<String, (String, String)> = HashMap::new(); // id -> (signature, path)
    let mut baseline_order: Vec<String> = Vec::new();
    if let Ok(rows) = crate::db_json::query_rows(db, "SELECT id, signature, path FROM code_nodes WHERE origin = ?", &[json!(baseline_origin)]) {
        if let Some(arr) = rows.as_array() {
            for r in arr {
                let id = js_string(r.get("id").unwrap_or(&Value::Null));
                let signature = str_qq(r.get("signature"));
                let path = norm(r.get("path"));
                if !baseline.contains_key(&id) {
                    baseline_order.push(id.clone());
                }
                baseline.insert(id, (signature, path));
            }
        }
    }

    let planned_file_paths: HashSet<String> = planned_file_ids.iter().map(|p| p.replace('\\', "/")).collect();
    let planned_ids: HashSet<String> = all_nodes.iter().map(|n| js_string(n.get("id").unwrap_or(&Value::Null))).collect();

    let mut added: Vec<Value> = Vec::new();
    let mut changed: Vec<Value> = Vec::new();
    let mut unchanged: Vec<Value> = Vec::new();
    let mut removed: Vec<Value> = Vec::new();
    let mut by_id: Map<String, Value> = Map::new();

    for n in &all_nodes {
        let id = js_string(n.get("id").unwrap_or(&Value::Null));
        let status = match baseline.get(&id) {
            None => "added",
            Some((base_sig, _)) => {
                let n_sig = str_qq(n.get("signature"));
                if base_sig != &n_sig { "changed" } else { "unchanged" }
            }
        };
        by_id.insert(id.clone(), json!(status));
        match status {
            "added" => added.push(json!(id)),
            "changed" => changed.push(json!(id)),
            _ => unchanged.push(json!(id)),
        }
    }
    // removed: a symbol the baseline has IN a file the plan touches, that the plan no longer declares.
    for id in &baseline_order {
        if planned_ids.contains(id) {
            continue;
        }
        let (_, base_path) = baseline.get(id).unwrap();
        if planned_file_paths.contains(base_path) {
            removed.push(json!(id));
            by_id.insert(id.clone(), json!("removed"));
        }
    }

    let delta = json!({
        "added": added,
        "changed": changed,
        "unchanged": unchanged,
        "removed": removed,
        "by_id": by_id,
        "counts": {
            "added": added.len() as i64,
            "changed": changed.len() as i64,
            "unchanged": unchanged.len() as i64,
            "removed": removed.len() as i64,
        },
    });

    Ok(json!({
        "files": file_sources.len() as i64,
        "nodes": all_nodes,
        "edges": edges,
        "delta": delta,
        "errors": errors,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE code_nodes ( id TEXT PRIMARY KEY, origin TEXT NOT NULL, path TEXT NOT NULL, type TEXT NOT NULL, name TEXT, signature TEXT )",
        )
        .unwrap();
        c
    }

    #[test]
    fn two_new_stubs_cross_link_and_all_added() {
        let c = db();
        let stub_a = "export function addUser(name) {\n  return helper(name);\n}\nfunction helper(x) { return x; }\n";
        let stub_b = "import { addUser } from './a.mjs';\nexport function main() {\n  return addUser('x');\n}\n";
        let r = blueprintParse(
            &json!({ "stub_files": [
                { "path": "a.mjs", "source": stub_a },
                { "path": "b.mjs", "source": stub_b }
            ] }),
            &c,
        )
        .unwrap();
        assert_eq!(r["files"], json!(2));
        assert_eq!(r["delta"]["counts"]["added"], json!(5));
        assert_eq!(r["delta"]["counts"]["changed"], json!(0));
        let edge_types: Vec<String> = r["edges"].as_array().unwrap().iter().map(|e| e["type"].as_str().unwrap().to_string()).collect();
        assert!(edge_types.contains(&"imports".to_string()));
        assert!(edge_types.contains(&"calls".to_string()));
    }

    #[test]
    fn diffs_against_baseline_and_flags_missing_path() {
        let c = db();
        c.execute_batch(
            "INSERT INTO code_nodes (id, origin, path, type, name, signature) VALUES
            ('a.mjs', 'atlas', 'a.mjs', 'file', 'a.mjs', NULL),
            ('a.mjs#addUser', 'atlas', 'a.mjs', 'function', 'addUser', '(name)'),
            ('a.mjs#helper', 'atlas', 'a.mjs', 'function', 'helper', '(x, y)'),
            ('a.mjs#gone', 'atlas', 'a.mjs', 'function', 'gone', '()')",
        )
        .unwrap();
        let stub_a = "export function addUser(name) {\n  return helper(name);\n}\nfunction helper(x) { return x; }\n";
        let r = blueprintParse(
            &json!({ "stub_files": [
                { "path": "a.mjs", "source": stub_a },
                { "path": "", "source": "whatever" }
            ] }),
            &c,
        )
        .unwrap();
        assert_eq!(r["delta"]["by_id"]["a.mjs#helper"], json!("changed"));
        assert_eq!(r["delta"]["by_id"]["a.mjs#addUser"], json!("unchanged"));
        assert_eq!(r["delta"]["by_id"]["a.mjs#gone"], json!("removed"));
        assert_eq!(r["errors"][0]["error"], json!("stub missing path"));
    }

    #[test]
    fn unparseable_stub_records_error_and_bare_file_node() {
        let c = db();
        let r = blueprintParse(&json!({ "stub_files": [ { "path": "weird.xyz", "source": "whatever content" } ] }), &c).unwrap();
        assert_eq!(r["delta"]["by_id"]["weird.xyz"], json!("added"));
        assert_eq!(r["errors"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn requires_stub_files() {
        let c = db();
        assert!(blueprintParse(&json!({}), &c).is_err());
        assert!(blueprintParse(&json!({ "stub_files": [] }), &c).is_err());
    }
}
