//! tokens-count.rs — twin of skills/system/tokens-count.mjs. NAMES ARE CONTRACT:
//! `estimateTokens`, `tokensCount` (named default). ~4 chars/token estimate, ceiling.
//! JS `.length` counts UTF-16 code units, not codepoints — mirrored via encode_utf16().
//! String coercion covers null/string/number/bool (the only types any real caller passes);
//! arrays/objects are out of documented domain and coerce to empty rather than guess JS's
//! `[object Object]`/comma-joined array stringification (same stance as json-utils.rs).
// QUALITY: 82/100 — UTF-16 code-unit counting is exact; array/object coercion is a documented
// narrowing rather than full JS String() fidelity

use serde_json::Value;

fn js_string_coerce(v: &Value) -> String {
    match v {
        Value::Null => String::new(),
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        _ => String::new(),
    }
}

pub fn estimateTokens(text: &Value) -> i64 {
    let len = js_string_coerce(text).encode_utf16().count() as i64;
    if len == 0 { 0 } else { ((len as f64) / 4.0).ceil() as i64 }
}

pub fn tokensCount(input: &Value) -> Result<Value, String> {
    if let Some(texts) = input.get("texts").and_then(Value::as_array) {
        let mut sources = Vec::new();
        let mut total = 0i64;
        for t in texts {
            let label = t.get("label").cloned().unwrap_or(Value::Null);
            let tokens = estimateTokens(t.get("text").unwrap_or(&Value::Null));
            total += tokens;
            sources.push(serde_json::json!({ "label": label, "tokens": tokens }));
        }
        return Ok(serde_json::json!({ "ok": true, "sources": sources, "total_tokens": total }));
    }
    if let Some(text) = input.get("text").filter(|v| !v.is_null()) {
        return Ok(serde_json::json!({ "ok": true, "tokens": estimateTokens(text) }));
    }
    Err("tokens-count: text|texts is required".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn estimate_is_quarter_chars_ceiling() {
        assert_eq!(estimateTokens(&json!("")), 0);
        assert_eq!(estimateTokens(&Value::Null), 0);
        assert_eq!(estimateTokens(&json!("a")), 1);
        assert_eq!(estimateTokens(&json!("abcd")), 1);
        assert_eq!(estimateTokens(&json!("abcde")), 2);
        assert_eq!(estimateTokens(&json!("hello world")), 3);
    }

    #[test]
    fn single_text_lane() {
        assert_eq!(tokensCount(&json!({"text":"hello world"})).unwrap(), json!({"ok":true,"tokens":3}));
    }

    #[test]
    fn texts_lane_sums_totals() {
        let r = tokensCount(&json!({"texts":[{"label":"a","text":"hello"},{"label":"b","text":"world!!"}]})).unwrap();
        assert_eq!(r["total_tokens"], json!(4));
        assert_eq!(r["sources"], json!([{"label":"a","tokens":2},{"label":"b","tokens":2}]));
    }

    #[test]
    fn requires_text_or_texts() {
        assert!(tokensCount(&json!({})).is_err());
    }
}
