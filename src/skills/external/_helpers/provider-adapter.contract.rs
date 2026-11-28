//! provider-adapter.contract.rs — twin of skills/external/_helpers/provider-adapter.contract.mjs
//! (helper module, no db). NAMES ARE CONTRACT: `validateProviderResult`, `assertProviderResult`.
//! `CONTRACT_VERSION` / `PROVIDER_ERROR_CODES` are plain data consts on the JS module, not
//! called by either fn below, so they carry no runtime behavior to twin.

use serde_json::{json, Value};

// JS `${v}` interpolation for the value shapes these two fns actually see (string/number/
// bool/null; object/array only appear inside `errors.join('; ')`, itself always strings).
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

// JS `typeof v` for the JSON-representable subset (no function/symbol/undefined inputs).
fn js_typeof(v: &Value) -> &'static str {
    match v {
        Value::Null | Value::Array(_) | Value::Object(_) => "object",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
    }
}

// JS `'k' in result` — only a plain object ever carries string-keyed props; a JSON array
// (also `typeof === 'object'` in JS) never has an 'output'/'tokens_in'/'tokens_out' key.
fn has_key(v: &Value, k: &str) -> bool {
    matches!(v, Value::Object(m) if m.contains_key(k))
}
fn get_key<'a>(v: &'a Value, k: &str) -> Option<&'a Value> {
    v.as_object().and_then(|m| m.get(k))
}

/// JS: `validateProviderResult(result)` — `{ valid, errors }`. `result == null ||
/// typeof result !== 'object'` rejects string/number/bool/null immediately (arrays pass,
/// same as JS where `typeof [] === 'object'`).
pub fn validateProviderResult(result: &Value) -> Value {
    let mut errors: Vec<Value> = Vec::new();
    if !matches!(result, Value::Object(_) | Value::Array(_)) {
        errors.push(json!("result must be an object"));
        return json!({ "valid": false, "errors": errors });
    }
    if !has_key(result, "output") {
        errors.push(json!("missing 'output' property"));
    }
    // JS also checks `result.output === undefined` here — unreachable via JSON-sourced
    // fixtures (serde_json::Value has no undefined variant to construct that case with).
    for k in ["tokens_in", "tokens_out"] {
        match get_key(result, k) {
            None => errors.push(json!(format!("missing '{k}' property"))),
            Some(v) => {
                let finite_nonneg = matches!(v, Value::Number(n) if n.as_f64().map_or(false, |f| f.is_finite() && f >= 0.0));
                if !finite_nonneg {
                    errors.push(json!(format!(
                        "'{k}' must be a non-negative finite number (got {} {})",
                        js_typeof(v),
                        tmpl(Some(v))
                    )));
                }
            }
        }
    }
    let valid = errors.is_empty();
    json!({ "valid": valid, "errors": errors })
}

/// JS: `assertProviderResult(result, providerName = 'unknown')` — throws on contract
/// violation, else returns undefined. `providerName`'s default only fires for a truly
/// absent argument; conform's helper-arm always supplies a Value (Null when the fixture
/// omits it), so Null is treated as the default here too.
pub fn assertProviderResult(result: &Value, provider_name: &Value) -> Result<Value, String> {
    let v = validateProviderResult(result);
    if v["valid"].as_bool().unwrap_or(false) {
        return Ok(Value::Null);
    }
    let errors = v["errors"].as_array().cloned().unwrap_or_default();
    let joined = errors.iter().map(|e| tmpl(Some(e))).collect::<Vec<_>>().join("; ");
    let name = if provider_name.is_null() { "unknown".to_string() } else { tmpl(Some(provider_name)) };
    Err(format!("provider-adapter contract violation in '{name}': {joined}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_result_has_no_errors() {
        let r = validateProviderResult(&json!({ "output": "hi", "tokens_in": 5, "tokens_out": 3 }));
        assert_eq!(r, json!({ "valid": true, "errors": [] }));
    }

    #[test]
    fn null_output_is_allowed() {
        let r = validateProviderResult(&json!({ "output": null, "tokens_in": 0, "tokens_out": 0 }));
        assert_eq!(r["valid"], json!(true));
    }

    #[test]
    fn non_object_result_rejected() {
        let r = validateProviderResult(&json!("nope"));
        assert_eq!(r, json!({ "valid": false, "errors": ["result must be an object"] }));
    }

    #[test]
    fn missing_and_bad_fields_collect_errors() {
        let r = validateProviderResult(&json!({ "tokens_in": -1, "tokens_out": "3" }));
        assert_eq!(
            r,
            json!({
                "valid": false,
                "errors": [
                    "missing 'output' property",
                    "'tokens_in' must be a non-negative finite number (got number -1)",
                    "'tokens_out' must be a non-negative finite number (got string 3)"
                ]
            })
        );
    }

    #[test]
    fn assert_provider_result_succeeds_silently_on_valid_result() {
        let ok = json!({ "output": {}, "tokens_in": 1, "tokens_out": 1 });
        assert_eq!(assertProviderResult(&ok, &json!("openrouter")).unwrap(), Value::Null);
        assert_eq!(assertProviderResult(&ok, &Value::Null).unwrap(), Value::Null);
    }
}
