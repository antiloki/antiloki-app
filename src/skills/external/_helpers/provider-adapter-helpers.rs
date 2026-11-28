//! provider-adapter-helpers.rs — twin of skills/external/_helpers/provider-adapter-helpers.mjs
//! (helper module, no db). NAMES ARE CONTRACT: `substituteTemplate`, `stripJsonFences`,
//! `parseJsonOutputOrText`, `normalizeTokens`, `requireApiKey`, `parseProviderResponse`.
//!
//! `requireApiKey` reads `process.env[env_var]` — twinned via `std::env::var`. `parseProviderResponse`
//! takes a fetch `Response` in JS (status/ok plus an async `.text()` method); since a Response object
//! is not JSON-representable, the twin's `response` arg is the POJO shape a caller supplies AFTER doing
//! the real await + status read: `{ "status": <int>, "ok": <bool, optional — derived from status if
//! absent>, "text": <string, already resolved> }`. Both throwing helpers reproduce AntilokiError's
//! `message` format (`error.mjs`): `${skill}: ${cause}` when skill is truthy, else just `cause`.

use regex::Regex;
use serde_json::{json, Value};
use std::sync::OnceLock;

fn str_qq(v: Option<&Value>) -> String {
    // String(v ?? '')
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(),
        Some(o) => o.to_string(),
    }
}
fn js_string(v: &Value) -> String {
    // String(v) (no ??)
    match v {
        Value::Null => "null".into(),
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        // String(array) falls through to Array.prototype.join(','), which renders
        // null/undefined ELEMENTS as '' (String([1,null,2]) === '1,,2', not '1,null,2') —
        // distinct from js_string(Value::Null) itself, which stays "null" at the top level.
        Value::Array(a) => a.iter().map(js_array_join_element).collect::<Vec<_>>().join(","),
        Value::Object(_) => "[object Object]".into(),
    }
}
// Array.prototype.join's per-element stringification: null/undefined -> '', everything else
// -> js_string (recursing through this same rule for nested arrays, matching JS join semantics).
fn js_array_join_element(v: &Value) -> String {
    match v {
        Value::Null => String::new(),
        other => js_string(other),
    }
}
fn tmpl(v: Option<&Value>) -> String {
    // `${v}`
    match v {
        None => "undefined".into(),
        Some(Value::Null) => "null".into(),
        Some(x) => str_qq(Some(x)),
    }
}
fn nz(v: Option<&Value>) -> Option<&Value> {
    v.filter(|x| !x.is_null())
}
fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}
fn js_number(v: Option<&Value>) -> f64 {
    // Number(v)
    match v {
        None => f64::NAN,
        Some(Value::Null) => 0.0,
        Some(Value::Bool(b)) => {
            if *b {
                1.0
            } else {
                0.0
            }
        }
        Some(Value::Number(n)) => n.as_f64().unwrap_or(f64::NAN),
        Some(Value::String(s)) => {
            let t = s.trim();
            if t.is_empty() {
                0.0
            } else {
                t.parse::<f64>().unwrap_or(f64::NAN)
            }
        }
        // Number(array) coerces via join+parse: [] -> Number('') -> 0, [x] -> Number(x)
        // (single element unwraps), [x, y, ...] -> NaN (multi-element join never parses).
        Some(Value::Array(a)) => match a.as_slice() {
            [] => 0.0,
            [only] => js_number(Some(only)),
            _ => f64::NAN,
        },
        Some(_) => f64::NAN,
    }
}
fn take_chars(s: &str, n: usize) -> String {
    // .slice(0, n) (BMP-equivalent). GAP: JS String.prototype.slice counts UTF-16 code units,
    // while .chars() here counts Unicode scalar values. Equivalent for BMP text (ASCII, Latin,
    // CJK, etc). Diverges only for astral-plane codepoints (emoji, some CJK-ext-B ideographs):
    // each one is a single Rust `char` but 2 UTF-16 units in JS, so a body_preview containing
    // such codepoints near the 200/500-char boundary in parseProviderResponse can truncate at a
    // different byte offset than the .mjs twin. Low-risk (error-preview text only); exact fix
    // would require re-encoding to UTF-16 units before slicing.
    s.chars().take(n).collect()
}
// AntilokiError's message construction (error.mjs): parts = [skill?`${skill}:`, cause] joined by ' '.
fn antiloki_message(skill: Option<&Value>, cause: &str) -> String {
    if truthy(skill) {
        format!("{}: {}", tmpl(skill), cause)
    } else {
        cause.to_string()
    }
}

/// JS: `substituteTemplate(template_md, input_kv)` — replaces every `{{key}}` occurrence.
/// `value == null` (null OR undefined) renders as empty string, never the literal "undefined".
/// GAP: JS `for...of` over a non-nullish, non-iterable `input_kv` (e.g. a plain number or
/// object) throws `TypeError: input_kv is not iterable`. This twin instead treats any non-array
/// input_kv as an empty item list (no substitutions, template returned ~as-is) instead of
/// erroring. Comment-only per reviewer: real call sites always pass an array or omit the arg.
pub fn substituteTemplate(template_md: &Value, input_kv: &Value) -> Value {
    let mut out = str_qq(Some(template_md));
    let items: &[Value] = nz(Some(input_kv)).and_then(|v| v.as_array()).map(|a| a.as_slice()).unwrap_or(&[]);
    for item in items {
        let key = tmpl(item.get("key"));
        let value = item.get("value");
        let replacement = match value {
            None | Some(Value::Null) => String::new(),
            Some(v) => js_string(v),
        };
        let pattern = format!("{{{{{key}}}}}");
        out = out.replace(&pattern, &replacement); // .split(pat).join(repl) == replace-all
    }
    json!(out)
}

fn front_fence_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"(?i)^\s*```(?:json)?\s*").unwrap())
}
fn back_fence_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"(?i)\s*```\s*$").unwrap())
}
// JS: text.replace(/^\s*```(?:json)?\s*/i, '').replace(/\s*```\s*$/i, '').trim()
fn strip_json_fences_str(text: &str) -> String {
    let s = front_fence_re().replace(text, "");
    let s = back_fence_re().replace(&s, "");
    s.trim().to_string()
}

/// JS: `stripJsonFences(text)` — strips a leading ```` ```json ```` / ```` ``` ```` fence and a
/// trailing ```` ``` ````, then trims. Non-string input passes through unchanged.
pub fn stripJsonFences(text: &Value) -> Value {
    match text.as_str() {
        Some(s) => json!(strip_json_fences_str(s)),
        None => text.clone(),
    }
}

/// JS: `parseJsonOutputOrText(resultText)` — parses fenced/bare `{...}`/`[...]` JSON; anything
/// else (not string, no leading brace, or parse failure) returns the ORIGINAL text untouched.
pub fn parseJsonOutputOrText(result_text: &Value) -> Value {
    let s = match result_text.as_str() {
        Some(s) => s,
        None => return result_text.clone(),
    };
    let stripped = strip_json_fences_str(s);
    if stripped.starts_with('{') || stripped.starts_with('[') {
        if let Ok(v) = serde_json::from_str::<Value>(&stripped) {
            return v;
        }
    }
    result_text.clone()
}

/// JS: `normalizeTokens(raw)` — `raw.field ?? ... ?? 0` chains, then `Number(x) || 0`.
/// `!raw || typeof raw !== 'object'` rejects everything except a truthy object/array.
pub fn normalizeTokens(raw: &Value) -> Value {
    let zero = || json!({ "tokens_in": 0, "tokens_out": 0, "tokens_cached": 0 });
    if !matches!(raw, Value::Object(_) | Value::Array(_)) {
        return zero();
    }
    let get = |k: &str| raw.get(k);
    let num_or_zero = |v: Option<&Value>| -> Value {
        let n = js_number(v);
        if n != 0.0 && !n.is_nan() {
            if n.fract() == 0.0 && n.is_finite() {
                json!(n as i64)
            } else {
                json!(n)
            }
        } else {
            json!(0)
        }
    };
    let tokens_in = nz(get("input_tokens")).or_else(|| nz(get("prompt_tokens"))).or_else(|| nz(get("in_tokens"))).or_else(|| nz(get("tokens_in")));
    let tokens_out = nz(get("output_tokens")).or_else(|| nz(get("completion_tokens"))).or_else(|| nz(get("out_tokens"))).or_else(|| nz(get("tokens_out")));
    let cached_from_details = get("prompt_tokens_details").and_then(|d| d.get("cached_tokens"));
    let tokens_cached = nz(cached_from_details)
        .or_else(|| nz(get("cache_read_input_tokens")))
        .or_else(|| nz(get("prompt_cache_hit_tokens")))
        .or_else(|| nz(get("cached_tokens")));
    json!({
        "tokens_in": num_or_zero(tokens_in),
        "tokens_out": num_or_zero(tokens_out),
        "tokens_cached": num_or_zero(tokens_cached),
    })
}

/// JS: `requireApiKey(env_var, provider_name)` — reads `process.env[env_var]`; throws
/// `PERMISSION_DENIED` (AntilokiError, skill=provider_name) when unset (JS `!key` also rejects
/// an empty-string env var, since `''` is falsy).
pub fn requireApiKey(env_var: &Value, provider_name: &Value) -> Result<Value, String> {
    let env_var_name = env_var.as_str().map(|s| s.to_string()).unwrap_or_else(|| js_string(env_var));
    let key = std::env::var(&env_var_name).ok().filter(|s| !s.is_empty());
    match key {
        Some(k) => Ok(json!(k)),
        None => {
            let cause = format!(
                "{} environment variable is not set. Set it to your {} API key and retry.",
                tmpl(Some(env_var)),
                tmpl(Some(provider_name))
            );
            Err(antiloki_message(Some(provider_name), &cause))
        }
    }
}

/// JS: `parseProviderResponse(response, provider_name)` — `response` is a fetch `Response` in
/// JS; the twin takes the resolved POJO shape `{ status, ok?, text }` (see module doc). Maps
/// 401/403 -> PERMISSION_DENIED, 429 -> IO_ERROR, other non-ok -> IO_ERROR, unparseable JSON body
/// -> PARSE_ERROR; success returns the parsed JSON body.
pub fn parseProviderResponse(response: &Value, provider_name: &Value) -> Result<Value, String> {
    let status = response.get("status").and_then(Value::as_i64).unwrap_or(0);
    let ok = response
        .get("ok")
        .and_then(Value::as_bool)
        .unwrap_or((200..300).contains(&status));
    let text = response.get("text").and_then(Value::as_str).unwrap_or("");

    if status == 401 || status == 403 {
        let cause = format!("{} auth failed (status {status}). Check your API key.", tmpl(Some(provider_name)));
        return Err(antiloki_message(Some(provider_name), &cause));
    }
    if status == 429 {
        let cause = format!("{} rate-limited (429). Backoff and retry.", tmpl(Some(provider_name)));
        return Err(antiloki_message(Some(provider_name), &cause));
    }
    if !ok {
        let cause = format!("{} returned {status}: {}", tmpl(Some(provider_name)), take_chars(text, 200));
        return Err(antiloki_message(Some(provider_name), &cause));
    }
    // GAP: the interpolated error text comes from serde_json's Display, which does not match
    // V8's JSON.parse message wording/format (e.g. "expected value at line 1 column 1" vs
    // "Unexpected token o in JSON at position 1"). Comment-only per reviewer: callers match on
    // the PARSE_ERROR code, not this message substring, so the exact wording is inert.
    match serde_json::from_str::<Value>(text) {
        Ok(v) => Ok(v),
        Err(e) => {
            let cause = format!("{} returned unparseable JSON: {e}", tmpl(Some(provider_name)));
            Err(antiloki_message(Some(provider_name), &cause))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn substitute_template_replaces_and_blanks_nullish() {
        let out = substituteTemplate(
            &json!("Hi {{name}}, you are {{role}}"),
            &json!([{ "key": "name", "value": "Atlas" }, { "key": "role", "value": "PL" }]),
        );
        assert_eq!(out, json!("Hi Atlas, you are PL"));
        assert_eq!(substituteTemplate(&json!("val={{x}}"), &json!([{ "key": "x", "value": null }])), json!("val="));
        assert_eq!(substituteTemplate(&json!("{{a}} {{a}} {{a}}"), &json!([{ "key": "a", "value": "x" }])), json!("x x x"));
        // String([1,null,2]) === '1,,2' — array-join renders a null ELEMENT as '', not "null".
        assert_eq!(
            substituteTemplate(&json!("v={{arr}}"), &json!([{ "key": "arr", "value": [1, null, 2] }])),
            json!("v=1,,2")
        );
    }

    #[test]
    fn strip_json_fences_removes_wrapper() {
        assert_eq!(stripJsonFences(&json!("```json\n{\"a\":1}\n```")), json!("{\"a\":1}"));
        assert_eq!(stripJsonFences(&json!("```\n[1,2]\n```")), json!("[1,2]"));
        assert_eq!(stripJsonFences(&json!("{\"a\":1}")), json!("{\"a\":1}"));
    }

    #[test]
    fn parse_json_output_or_text_parses_or_falls_back() {
        assert_eq!(parseJsonOutputOrText(&json!("{\"x\":42}")), json!({ "x": 42 }));
        assert_eq!(parseJsonOutputOrText(&json!("```json\n[1,2,3]\n```")), json!([1, 2, 3]));
        assert_eq!(parseJsonOutputOrText(&json!("plain prose reply")), json!("plain prose reply"));
        assert_eq!(parseJsonOutputOrText(&json!("{not actually json")), json!("{not actually json"));
    }

    #[test]
    fn normalize_tokens_covers_shapes() {
        assert_eq!(
            normalizeTokens(&json!({ "input_tokens": 100, "output_tokens": 25 })),
            json!({ "tokens_in": 100, "tokens_out": 25, "tokens_cached": 0 })
        );
        assert_eq!(
            normalizeTokens(&json!({ "prompt_tokens": 80, "completion_tokens": 20, "prompt_tokens_details": { "cached_tokens": 64 } })),
            json!({ "tokens_in": 80, "tokens_out": 20, "tokens_cached": 64 })
        );
        assert_eq!(normalizeTokens(&json!({})), json!({ "tokens_in": 0, "tokens_out": 0, "tokens_cached": 0 }));
        assert_eq!(normalizeTokens(&Value::Null), json!({ "tokens_in": 0, "tokens_out": 0, "tokens_cached": 0 }));
        // Number([]) === 0, Number([5]) === 5, Number([5,6]) === NaN (join+parse coercion).
        assert_eq!(
            normalizeTokens(&json!({ "input_tokens": [], "output_tokens": [5], "tokens_cached": [5, 6] })),
            json!({ "tokens_in": 0, "tokens_out": 5, "tokens_cached": 0 })
        );
    }

    #[test]
    fn require_api_key_present_and_missing() {
        // SAFETY: test-only env mutation of a process-unique var name; no concurrent readers.
        unsafe {
            std::env::set_var("TEST_PROVIDER_KEY_RS", "sk-test-123");
        }
        assert_eq!(requireApiKey(&json!("TEST_PROVIDER_KEY_RS"), &json!("test")).unwrap(), json!("sk-test-123"));
        unsafe {
            std::env::remove_var("TEST_PROVIDER_KEY_RS");
            std::env::remove_var("NEVER_SET_ME_RS");
        }
        let err = requireApiKey(&json!("NEVER_SET_ME_RS"), &json!("fake-provider")).unwrap_err();
        assert!(err.contains("NEVER_SET_ME_RS"), "{err}");
        assert!(err.to_lowercase().contains("not set"), "{err}");
    }

    #[test]
    fn parse_provider_response_maps_statuses() {
        let auth_err = parseProviderResponse(&json!({ "status": 401, "ok": false, "text": "{\"error\":\"invalid_api_key\"}" }), &json!("fake")).unwrap_err();
        assert!(auth_err.to_lowercase().contains("auth failed"), "{auth_err}");
        let rl_err = parseProviderResponse(&json!({ "status": 429, "ok": false, "text": "{\"error\":\"rate_limited\"}" }), &json!("fake")).unwrap_err();
        assert!(rl_err.to_lowercase().contains("rate-limited"), "{rl_err}");
        let body = parseProviderResponse(&json!({ "status": 200, "ok": true, "text": "{\"result\":\"ok\",\"count\":5}" }), &json!("fake")).unwrap();
        assert_eq!(body, json!({ "result": "ok", "count": 5 }));
    }
}
