//! to-mcp-markdown.rs — twin of skills/external/to-mcp-markdown.mjs. NAMES ARE CONTRACT:
//! `toMcpMarkdown` (named default) + `isMcpShape`. Pure object building, no DB, no AI.
// QUALITY: 86/100 — faithful shape builder; conditional keys mirror JS spreads (comparator sorts keys)
use serde_json::{json, Value};

/// JS: `toMcpMarkdown(input, _context)` — wraps a markdown string in the MCP response shape.
/// `data` is spread into `_meta.antiloki` only when non-null; `isError` only when true.
pub fn toMcpMarkdown(input: &Value, _context: &Value) -> Result<Value, String> {
    let markdown = match input.get("markdown") {
        Some(Value::String(s)) => s.clone(),
        _ => return Err("to-mcp-markdown: markdown is required".into()),
    };
    let data = input.get("data").filter(|v| !v.is_null());
    let is_error = input.get("isError").and_then(Value::as_bool).unwrap_or(false);

    let mut antiloki = serde_json::Map::new();
    antiloki.insert("version".into(), json!(1));
    antiloki.insert("format".into(), json!("markdown"));
    if let Some(d) = data {
        antiloki.insert("data".into(), d.clone());
    }
    let mut out = serde_json::Map::new();
    out.insert("content".into(), json!([{ "type": "text", "text": markdown }]));
    out.insert("_meta".into(), json!({ "antiloki": antiloki }));
    if is_error {
        out.insert("isError".into(), json!(true));
    }
    Ok(Value::Object(out))
}

/// JS: `isMcpShape(v)` — is `v` already an MCP-shaped response?
pub fn isMcpShape(v: &Value) -> bool {
    let Some(content) = v.get("content").and_then(Value::as_array) else { return false };
    let Some(first) = content.first() else { return false };
    first.get("type").map_or(false, Value::is_string)
        && first.get("text").map_or(false, Value::is_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wraps_markdown_and_optional_keys() {
        let r = toMcpMarkdown(&json!({ "markdown": "hi" }), &Value::Null).unwrap();
        assert_eq!(r["content"][0]["text"], json!("hi"));
        assert_eq!(r["_meta"]["antiloki"]["format"], json!("markdown"));
        assert!(r.get("isError").is_none());
        let e = toMcpMarkdown(&json!({ "markdown": "x", "data": {"a":1}, "isError": true }), &Value::Null).unwrap();
        assert_eq!(e["isError"], json!(true));
        assert_eq!(e["_meta"]["antiloki"]["data"], json!({"a":1}));
        assert!(toMcpMarkdown(&json!({}), &Value::Null).is_err());
    }

    #[test]
    fn detects_mcp_shape() {
        assert!(isMcpShape(&json!({ "content": [{ "type": "text", "text": "x" }] })));
        assert!(!isMcpShape(&json!({ "content": [] })));
        assert!(!isMcpShape(&json!({ "nope": 1 })));
    }
}
