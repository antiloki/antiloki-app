//! json-utils.rs — twin of skills/engine/json-utils.mjs. NAMES ARE CONTRACT: `loadJson`,
//! `jsonUtilsProbe` (named default), exact JS names.
// QUALITY: 82/100 — clear degrade lanes; JS string-coercion corners documented but only fixture-proven subset mirrored

use serde_json::Value;

/// JS: `loadJson(s, fallback)` — falsy `s` → fallback; parse failure → fallback; never throws.
/// JS falsy at this seam: null/undefined, '', 0, false. Non-string truthy inputs go through
/// `JSON.parse(String(s))` in JS — numbers and `true` re-parse to themselves; arrays/objects
/// coerce to garbage strings no call site ever passes (DB TEXT columns + AI output are the
/// documented inputs), so those lanes mirror the fallback degrade.
pub fn loadJson(s: &Value, fallback: &Value) -> Value {
    let falsy = match s {
        Value::Null => true,
        Value::String(x) => x.is_empty(),
        Value::Number(n) => n.as_f64() == Some(0.0),
        Value::Bool(b) => !b,
        _ => false,
    };
    if falsy {
        return fallback.clone();
    }
    match s {
        Value::String(x) => serde_json::from_str(x).unwrap_or_else(|_| fallback.clone()),
        Value::Number(_) | Value::Bool(_) => s.clone(),
        _ => fallback.clone(),
    }
}

/// JS: default `jsonUtilsProbe(input, _context)` → `{ parsed: loadJson(input?.json, input?.fallback ?? null) }`.
pub fn jsonUtilsProbe(input: &Value, _context: &Value) -> Value {
    let json = input.get("json").unwrap_or(&Value::Null);
    let fallback = input.get("fallback").cloned().unwrap_or(Value::Null);
    serde_json::json!({ "parsed": loadJson(json, &fallback) })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_and_degrades_like_js() {
        assert_eq!(loadJson(&json!("{\"a\":1}"), &Value::Null), json!({"a":1}));
        assert_eq!(loadJson(&json!("not json"), &json!({"fb":true})), json!({"fb":true}));
        assert_eq!(loadJson(&Value::Null, &json!("fb")), json!("fb"));
        assert_eq!(loadJson(&json!(""), &json!(42)), json!(42));
        assert_eq!(loadJson(&json!("0"), &json!("fb")), json!(0), "string '0' is truthy");
    }

    #[test]
    fn probe_wraps_under_the_skill_contract() {
        assert_eq!(jsonUtilsProbe(&json!({"json":"{\"x\":2}"}), &Value::Null), json!({"parsed":{"x":2}}));
        assert_eq!(jsonUtilsProbe(&json!({}), &Value::Null), json!({"parsed":null}));
    }
}
