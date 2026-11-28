//! code-decorators-doc.rs — twin of skills/codebase/code-decorators-doc.mjs (db lane). NAMES ARE
//! CONTRACT: `codeDecoratorsDoc` (named default). Grouped decorators by file + role/kind tallies.
// QUALITY: 84/100 — by-path grouping (insertion order via preserve_order) + many JSON-column parses
use rusqlite::Connection;
use serde_json::{json, Map, Value};

fn j(v: Option<&Value>, dflt: Value) -> Value {
    // JS: J(s,d) = try JSON.parse(s) catch d. A NULL column → JSON.parse(null)==="null"→null (no
    // throw), so it yields null, NOT the default. Only a parse error (bad string) falls to d.
    match v {
        Some(Value::Null) => Value::Null,
        Some(Value::String(s)) => serde_json::from_str::<Value>(s).unwrap_or(dflt),
        _ => dflt,
    }
}

pub fn codeDecoratorsDoc(input: &Value, db: &Connection) -> Result<Value, String> {
    let origin = input.get("origin").and_then(|v| v.as_str()).unwrap_or("atlas");
    let rows = match crate::db_json::query_rows(db, "SELECT * FROM code_decorators WHERE origin = ? ORDER BY path, line", &[json!(origin)]) {
        Ok(r) => r,
        Err(_) => return Ok(json!({ "ok": true, "found": false, "origin": origin, "files": [],
            "by_role": {}, "by_kind": {}, "totals": { "files": 0, "blocks": 0 },
            "error": "code_decorators not present — restart the BE, then rebuild" })),
    };
    let rarr = rows.as_array().unwrap();
    let mut by_path: Map<String, Value> = Map::new();   // preserve_order → insertion order == JS Map
    let mut by_role: Map<String, Value> = Map::new();
    let mut by_kind: Map<String, Value> = Map::new();
    let get = |o: &Map<String, Value>, k: &str| o.get(k).cloned().unwrap_or(Value::Null);
    for r in rarr {
        let o = r.as_object().unwrap();
        let d = json!({
            "name": get(o, "name"), "kind": get(o, "kind"), "role": get(o, "role"), "line": get(o, "line"),
            "end_line": get(o, "end_line"), "signature": get(o, "signature"),
            "inputs": j(o.get("inputs_json"), json!([])), "output": j(o.get("output_json"), Value::Null),
            "effects": j(o.get("effects_json"), json!([])), "calls": j(o.get("calls_json"), json!([])),
            "uses": j(o.get("uses_json"), json!([])), "complexity": get(o, "complexity"),
            "flags": j(o.get("flags_json"), json!([])), "summary": get(o, "summary"), "steps": j(o.get("steps_json"), json!([])),
        });
        let path = o.get("path").and_then(|v| v.as_str()).unwrap_or("").to_string();
        by_path.entry(path).or_insert_with(|| json!([])).as_array_mut().unwrap().push(d);
        for (map, key) in [(&mut by_role, "role"), (&mut by_kind, "kind")] {
            let k = o.get(key).and_then(|v| v.as_str()).unwrap_or("").to_string();
            let cur = map.get(&k).and_then(|v| v.as_i64()).unwrap_or(0);
            map.insert(k, json!(cur + 1));
        }
    }
    let files: Vec<Value> = by_path.iter().map(|(p, decs)| json!({ "path": p, "decorators": decs })).collect();
    Ok(json!({
        "ok": true, "found": !rarr.is_empty(), "origin": origin, "files": files,
        "by_role": by_role, "by_kind": by_kind,
        "totals": { "files": files.len(), "blocks": rarr.len() },
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn groups_by_file_and_tallies() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE code_decorators (origin TEXT, path TEXT, line INTEGER, name TEXT, kind TEXT, role TEXT, end_line INTEGER, signature TEXT, inputs_json TEXT, output_json TEXT, effects_json TEXT, calls_json TEXT, uses_json TEXT, complexity INTEGER, flags_json TEXT, summary TEXT, steps_json TEXT);
            INSERT INTO code_decorators (origin,path,line,name,kind,role,inputs_json) VALUES ('atlas','a.js',1,'f','fn','helper','[\"x\"]'),('atlas','a.js',2,'g','fn','helper',NULL);").unwrap();
        let r = codeDecoratorsDoc(&json!({}), &c).unwrap();
        assert_eq!(r["found"], json!(true));
        assert_eq!(r["files"][0]["decorators"].as_array().unwrap().len(), 2);
        assert_eq!(r["by_role"]["helper"], json!(2));
        assert_eq!(r["files"][0]["decorators"][0]["inputs"], json!(["x"]));
        assert_eq!(r["files"][0]["decorators"][1]["inputs"], Value::Null); // null column → null
    }
}
