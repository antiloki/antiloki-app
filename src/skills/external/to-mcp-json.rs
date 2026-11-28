//! to-mcp-json.rs — twin of skills/external/to-mcp-json.mjs. NAMES ARE CONTRACT:
//! `toMcpJson` (default). Wraps structured `data` as pretty JSON in content[0] AND _meta.
// QUALITY: pure transform (no db) — JSON.stringify(data,null,2) == serde_json::to_string_pretty;
// optional hint prefix + conditional top-level isError key; `data` required (undefined throws, null ok).
use rusqlite::Connection;
use serde_json::{json, Map, Value};

// JS truthy test — drives `hint ? ...` and `isError ? ...`.
fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}
// JS `${v}` interpolation (hint is truthy here → string/number/bool).
fn tmpl(v: Option<&Value>) -> String {
    match v {
        None => "undefined".into(),
        Some(Value::Null) => "null".into(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(),
        Some(o) => o.to_string(),
    }
}

/// JS: `toMcpJson(input, _context)` — structured `data` pretty-printed as JSON, fenced, in
/// content[0].text AND mirrored under `_meta.antiloki`. `data` is REQUIRED (undefined throws;
/// null passes through). A truthy `hint` prefixes the fenced block; `isError` adds the top-level
/// flag only when truthy.
pub fn toMcpJson(input: &Value, _db: &Connection) -> Result<Value, String> {
    let data = match input.get("data") {
        None => return Err("to-mcp-json: data is required".into()),
        Some(d) => d,
    };

    let pretty = serde_json::to_string_pretty(data).map_err(|e| e.to_string())?;
    let text = if truthy(input.get("hint")) {
        format!("{}\n\n```json\n{}\n```", tmpl(input.get("hint")), pretty)
    } else {
        format!("```json\n{}\n```", pretty)
    };

    let mut antiloki = Map::new();
    antiloki.insert("version".into(), json!(1));
    antiloki.insert("format".into(), json!("json"));
    antiloki.insert("data".into(), data.clone());

    let mut out = Map::new();
    out.insert("content".into(), json!([{ "type": "text", "text": text }]));
    out.insert("_meta".into(), json!({ "antiloki": antiloki }));
    if truthy(input.get("isError")) {
        out.insert("isError".into(), json!(true));
    }
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn call(i: Value) -> Result<Value, String> {
        toMcpJson(&i, &Connection::open_in_memory().unwrap())
    }

    #[test]
    fn wraps_json_meta_and_optional_keys() {
        // fenced pretty JSON + mirrored _meta; no isError when absent
        let r = call(json!({ "data": { "a": 1 } })).unwrap();
        assert_eq!(r["content"][0]["type"], json!("text"));
        assert_eq!(r["content"][0]["text"], json!("```json\n{\n  \"a\": 1\n}\n```"));
        assert_eq!(r["_meta"]["antiloki"]["version"], json!(1));
        assert_eq!(r["_meta"]["antiloki"]["format"], json!("json"));
        assert_eq!(r["_meta"]["antiloki"]["data"], json!({ "a": 1 }));
        assert!(r.get("isError").is_none());

        // truthy hint prefixes the block
        let h = call(json!({ "data": { "k": 1 }, "hint": "title" })).unwrap();
        assert_eq!(h["content"][0]["text"], json!("title\n\n```json\n{\n  \"k\": 1\n}\n```"));

        // null hint → no prefix (falsy)
        let hn = call(json!({ "data": { "k": 1 }, "hint": Value::Null })).unwrap();
        assert_eq!(hn["content"][0]["text"], json!("```json\n{\n  \"k\": 1\n}\n```"));

        // isError truthy → top-level flag; falsy → omitted
        let e = call(json!({ "data": { "ok": false }, "isError": true })).unwrap();
        assert_eq!(e["isError"], json!(true));
        let f = call(json!({ "data": "hello", "isError": false })).unwrap();
        assert!(f.get("isError").is_none());
        assert_eq!(f["content"][0]["text"], json!("```json\n\"hello\"\n```"));

        // data=null is allowed (only undefined throws); missing data throws
        let n = call(json!({ "data": Value::Null })).unwrap();
        assert_eq!(n["content"][0]["text"], json!("```json\nnull\n```"));
        assert_eq!(n["_meta"]["antiloki"]["data"], Value::Null);
        assert!(call(json!({})).is_err());
    }
}
