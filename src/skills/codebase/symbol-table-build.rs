//! symbol-table-build.rs — twin of skills/codebase/symbol-table-build.mjs (db lane; db unused). NAMES
//! ARE CONTRACT: `symbolTableBuild` (named default). Index named defs → byName + defsByFile.
// QUALITY: 84/100 — fold over nodes; first-def-per-file wins; serde Map has no prototype-leak (JS uses null-proto)
use rusqlite::Connection;
use serde_json::{json, Map, Value};

const DEF_TYPES: [&str; 5] = ["function", "class", "method", "component", "hook"];

pub fn symbolTableBuild(input: &Value, _db: &Connection) -> Result<Value, String> {
    let empty = vec![];
    let nodes = input.get("nodes").and_then(|v| v.as_array()).unwrap_or(&empty);
    let mut by_name: Map<String, Value> = Map::new();
    let mut defs_by_file: Map<String, Value> = Map::new();

    for n in nodes {
        let ty = n.get("type").and_then(|v| v.as_str()).unwrap_or("");
        if !DEF_TYPES.contains(&ty) { continue; }
        // !n.name → skip (falsy); n.id == null → skip
        let name = match n.get("name") { Some(Value::String(s)) if !s.is_empty() => s.clone(), Some(v) if !v.is_null() && v.as_str() != Some("") => v.to_string(), _ => continue };
        let id = match n.get("id") { Some(v) if !v.is_null() => match v { Value::String(s) => s.clone(), o => o.to_string() }, _ => continue };

        by_name.entry(name.clone()).or_insert_with(|| json!([])).as_array_mut().unwrap().push(json!(id));

        let path = match n.get("path") { Some(v) if !v.is_null() => match v { Value::String(s) => s.clone(), o => o.to_string() }, _ => String::new() };
        let file_defs = defs_by_file.entry(path).or_insert_with(|| json!({})).as_object_mut().unwrap();
        if !file_defs.contains_key(&name) { file_defs.insert(name, json!(id)); } // first def wins
    }
    Ok(json!({ "byName": by_name, "defsByFile": defs_by_file }))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn indexes_defs() {
        let c = Connection::open_in_memory().unwrap();
        let r = symbolTableBuild(&json!({ "nodes": [
            { "id": "a.mjs", "path": "a.mjs", "type": "file", "name": "a.mjs" },
            { "id": "a.mjs#foo", "path": "a.mjs", "type": "function", "name": "foo" },
            { "id": "b.mjs#foo", "path": "b.mjs", "type": "function", "name": "foo" },
            { "id": "a.mjs#k", "path": "a.mjs", "type": "const", "name": "k" },
            { "id": "a.mjs#toString", "path": "a.mjs", "type": "method", "name": "toString" }
        ] }), &c).unwrap();
        assert_eq!(r["byName"]["foo"], json!(["a.mjs#foo", "b.mjs#foo"]));
        assert_eq!(r["byName"].get("k"), None); // const skipped
        assert_eq!(r["defsByFile"]["a.mjs"]["toString"], json!("a.mjs#toString")); // builtin-named indexed
        // first def wins
        let d = symbolTableBuild(&json!({ "nodes": [
            { "id": "x#1", "path": "x", "type": "function", "name": "dup" },
            { "id": "x#2", "path": "x", "type": "function", "name": "dup" }
        ] }), &c).unwrap();
        assert_eq!(d["defsByFile"]["x"]["dup"], json!("x#1"));
        assert_eq!(d["byName"]["dup"], json!(["x#1", "x#2"]));
    }
}
