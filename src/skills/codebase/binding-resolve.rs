//! binding-resolve.rs — twin of skills/codebase/binding-resolve.mjs (db lane; db unused). NAMES ARE
//! CONTRACT: `bindingResolve` (named default). Resolve symbol-level calls → 'calls' edges.
// QUALITY: 83/100 — same-file-then-imported-file scoped lookup, own-property (no proto leak), edge dedup
use rusqlite::Connection;
use serde_json::{json, Value};
use std::collections::HashSet;

fn js_str(v: &Value) -> String { match v { Value::String(s) => s.clone(), o => o.to_string() } }
// own-property lookup (serde Map.get is own-only, so a callee named like a builtin never leaks)
fn lookup(dbf: &Value, path: &str, name: &str) -> Value {
    dbf.get(path).and_then(|o| o.as_object()).and_then(|o| o.get(name)).cloned().unwrap_or(Value::Null)
}

pub fn bindingResolve(input: &Value, _db: &Connection) -> Result<Value, String> {
    let empty = vec![];
    let calls = input.get("calls").and_then(|v| v.as_array()).unwrap_or(&empty);
    let defs_by_file = match input.get("symbols").and_then(|s| s.get("defsByFile")).filter(|v| v.is_object()) {
        Some(v) => v.clone(), None => return Err("binding-resolve: symbols.defsByFile is required".into()),
    };
    let import_edges = input.get("importEdges").and_then(|v| v.as_array()).unwrap_or(&empty);

    // file → [imported file, ...]
    let mut imports_by_file: std::collections::HashMap<String, Vec<String>> = std::collections::HashMap::new();
    for e in import_edges {
        let (from, to) = (e.get("from_node"), e.get("to_node"));
        if from.map(|v| v.is_null()).unwrap_or(true) || to.map(|v| v.is_null()).unwrap_or(true) { continue; }
        imports_by_file.entry(js_str(from.unwrap())).or_default().push(js_str(to.unwrap()));
    }

    let mut edges: Vec<Value> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for call in calls {
        let from_path_v = call.get("from_path");
        let callee_v = call.get("callee");
        if from_path_v.map(|v| v.is_null()).unwrap_or(true) || callee_v.map(|v| v.is_null()).unwrap_or(true) { continue; }
        let from_path = js_str(from_path_v.unwrap());
        let callee = js_str(callee_v.unwrap());

        let caller = call.get("caller");
        let caller_id = match caller {
            Some(c) if !c.is_null() && js_str(c) != "" => format!("{from_path}#{}", js_str(c)),
            _ => from_path.clone(),
        };

        let mut target = lookup(&defs_by_file, &from_path, &callee);
        if target.is_null() {
            for imported in imports_by_file.get(&from_path).cloned().unwrap_or_default() {
                let hit = lookup(&defs_by_file, &imported, &callee);
                if !hit.is_null() { target = hit; break; }
            }
        }
        let target_id = match target.as_str() { Some(s) => s.to_string(), None => continue }; // non-string → drop

        // a JSX usage (`<Child/>`, parse-time `via: "jsx"`) is a `renders` edge — same resolution,
        // different meaning: composition rather than invocation.
        let edge_type = match call.get("via").and_then(Value::as_str) {
            Some("jsx") => "renders",
            // a definition passed by name (`useReducer(storeReducer)`, `memo(Inner)`) — a `uses`
            // edge, but only when the name resolves to a definition; a local variable never does
            Some("ref") => "uses",
            _ => "calls",
        };
        let key = format!("{caller_id} {target_id} {edge_type}");
        if !seen.insert(key) { continue; }
        edges.push(json!({ "from_node": caller_id, "to_node": target_id, "type": edge_type }));
    }

    Ok(json!({ "edges": edges, "total": calls.len(), "resolved": edges.len() }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jsx_usage_resolves_to_a_renders_edge() {
        let db = Connection::open_in_memory().unwrap();
        let r = bindingResolve(&json!({
            "calls": [
                { "from_path": "list.tsx", "caller": "List", "callee": "Row", "via": "jsx" },
                { "from_path": "list.tsx", "caller": "List", "callee": "format" }
            ],
            "symbols": { "defsByFile": { "list.tsx": { "Row": "list.tsx#Row", "format": "list.tsx#format" } } },
            "importEdges": []
        }), &db).unwrap();
        let edges = r["edges"].as_array().unwrap();
        assert_eq!(edges[0], json!({ "from_node": "list.tsx#List", "to_node": "list.tsx#Row", "type": "renders" }));
        assert_eq!(edges[1], json!({ "from_node": "list.tsx#List", "to_node": "list.tsx#format", "type": "calls" }));
    }

    #[test]
    fn a_definition_passed_by_name_is_a_uses_edge_and_a_local_is_nothing() {
        let db = Connection::open_in_memory().unwrap();
        let r = bindingResolve(&json!({
            "calls": [
                { "from_path": "s.tsx", "caller": "Provider", "callee": "reducer", "via": "ref" },
                { "from_path": "s.tsx", "caller": "Provider", "callee": "init", "via": "ref" }
            ],
            "symbols": { "defsByFile": { "s.tsx": { "reducer": "s.tsx#reducer", "Provider": "s.tsx#Provider" } } },
            "importEdges": []
        }), &db).unwrap();
        assert_eq!(r["edges"], json!([{ "from_node": "s.tsx#Provider", "to_node": "s.tsx#reducer", "type": "uses" }]));
    }
    #[test]
    fn resolves_calls() {
        let c = Connection::open_in_memory().unwrap();
        let symbols = json!({ "defsByFile": { "a.mjs": { "caller": "a.mjs#caller", "localHelper": "a.mjs#localHelper" }, "b.mjs": { "remote": "b.mjs#remote" } } });
        let r = bindingResolve(&json!({
            "calls": [
                { "from_path": "a.mjs", "caller": "caller", "callee": "localHelper" },
                { "from_path": "a.mjs", "caller": "caller", "callee": "remote" },
                { "from_path": "a.mjs", "caller": "caller", "callee": "console" }
            ],
            "symbols": symbols,
            "importEdges": [{ "from_node": "a.mjs", "to_node": "b.mjs", "type": "imports" }]
        }), &c).unwrap();
        assert_eq!(r["total"], json!(3));
        assert_eq!(r["resolved"], json!(2));
        assert_eq!(r["edges"][0]["to_node"], json!("a.mjs#localHelper"));
        assert_eq!(r["edges"][1]["to_node"], json!("b.mjs#remote"));
        assert!(bindingResolve(&json!({ "calls": [] }), &c).is_err()); // no symbols.defsByFile
    }
}
