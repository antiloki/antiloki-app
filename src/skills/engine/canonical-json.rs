//! canonical-json — the Rust twin of skills/engine/canonical-json.mjs.
//! NAMES ARE CONTRACT: the export is `canonicalJson`, exactly as in JS; the internal walker is
//! `serialize`, exactly as in JS. The golden strings in the tests are shared verbatim with the
//! JS canaries — if either side drifts, identity breaks at the crossing and this module screams.
// QUALITY: 88/100 — clean recursive serializer, golden tests; integer-float caveat documented

use serde_json::Value;

pub fn canonicalJson(value: &Value) -> Result<String, String> {
    serialize(value)
}

fn serialize(v: &Value) -> Result<String, String> {
    match v {
        Value::Null => Ok("null".into()),
        Value::Bool(b) => Ok(if *b { "true".into() } else { "false".into() }),
        Value::Number(n) => {
            if n.as_f64().map(|f| !f.is_finite()).unwrap_or(false) {
                return Err("canonical-json: non-finite number — a hash over garbage is worse than none".into());
            }
            // serde_json prints integers plainly and floats via shortest round-trip — the same
            // family as V8. Integer-valued floats (1.0) would differ; the CONTRACT forbids them.
            Ok(n.to_string())
        }
        Value::String(s) => Ok(escape_js_compatible(s)),
        Value::Array(a) => {
            let mut parts = Vec::with_capacity(a.len());
            for x in a {
                parts.push(serialize(x)?);
            }
            Ok(format!("[{}]", parts.join(",")))
        }
        Value::Object(m) => {
            let mut keys: Vec<&String> = m.keys().collect();
            keys.sort(); // lexicographic by UTF-8 bytes == JS code-unit sort for these keys
            let mut parts = Vec::with_capacity(keys.len());
            for k in keys {
                parts.push(format!("{}:{}", escape_js_compatible(k), serialize(&m[k])?));
            }
            Ok(format!("{{{}}}", parts.join(",")))
        }
    }
}

/// JSON.stringify string escaping (Rust-side helper — the JS inlines this in JSON.stringify):
/// `"` and `\` escaped; control chars as \b \t \n \f \r or \u00XX; non-ASCII passes raw.
fn escape_js_compatible(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{0008}' => out.push_str("\\b"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\u{000C}' => out.push_str("\\f"),
            '\r' => out.push_str("\\r"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn golden_sorted_recursive() {
        // SHARED GOLDEN with skills/engine/canonical-json.test.mjs — byte for byte.
        let v = json!({ "b": 1, "a": { "d": 2, "c": [3, { "z": 4, "y": 5 }] } });
        assert_eq!(canonicalJson(&v).unwrap(), r#"{"a":{"c":[3,{"y":5,"z":4}],"d":2},"b":1}"#);
    }

    #[test]
    fn golden_order_independence() {
        let a = json!({ "a": 1, "b": 2 });
        let b = json!({ "b": 2, "a": 1 });
        assert_eq!(canonicalJson(&a).unwrap(), canonicalJson(&b).unwrap());
        assert_eq!(canonicalJson(&a).unwrap(), r#"{"a":1,"b":2}"#);
    }

    #[test]
    fn unicode_passes_raw_like_js() {
        let v = json!("é — “q”");
        assert_eq!(canonicalJson(&v).unwrap(), "\"é — “q”\"");
    }
}
