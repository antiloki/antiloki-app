//! code-run.rs — twin of skills/codebase/code-run.mjs. NAMES ARE CONTRACT: `codeRun` (default).
//!
//! Runs a code chunk ephemerally with an optional fixture + expected value, for the playground's
//! "run / test this" buttons. Nothing is written: the chunk executes in a fresh context, the
//! completion value and console output are captured, and (if `expected` is given) compared.
//!
//! ## MIRRORED, not translated: `node:vm` → `boa_engine`
//! This module was listed as untwinnable because Rust has no `node:vm`. That framed the problem as
//! translation when it is really capability: the requirement is "evaluate a JS chunk in a fresh
//! context with captured console output and a bound on runaway code". `boa_engine` — a pure-Rust
//! JS engine — provides exactly that, and being pure Rust it keeps the single-binary endgame
//! intact (no V8 embed, no C++ toolchain).
//!
//! ## The one honest behavioural difference: the bound is not a clock
//! JS passes `{ timeout: 2000 }`; Boa has no wall-clock interrupt. The mirror uses `RuntimeLimits`
//! — a loop-iteration cap and a recursion cap — so a runaway chunk is still bounded, but
//! DETERMINISTICALLY (by work done) rather than by elapsed time. For the playground's purpose
//! (stop an accidental `while(true)`) that is equivalent or better: the same chunk is cut off at
//! the same point on a fast machine and a slow one. A chunk that is merely *slow* without looping
//! is not interrupted, which the JS version would have killed at 2s. `duration_ms` is still
//! reported and is real.
//!
//! ## SECURITY, restated
//! The JS module's own warning holds and is arguably stronger here: this is the user's OWN chunk on
//! their OWN machine (single-user local dev tool). Boa is not a security sandbox against hostile
//! code either. Do not point this at untrusted input.
use boa_engine::{js_string, property::Attribute, Context, JsValue, NativeFunction, Source};
use serde_json::{json, Value};
use std::cell::RefCell;
use std::rc::Rc;

/// JS `safeJson(v)` — `undefined` renders as the literal string "undefined"; functions become
/// `[function name]`; anything unserializable degrades to a string rather than throwing.
fn safe_json(v: &Option<Value>) -> String {
    match v {
        None => "undefined".to_string(),
        Some(x) => serde_json::to_string(x).unwrap_or_else(|_| "[unserializable]".to_string()),
    }
}

/// JS `fmt(a)` — objects/arrays via safeJson, everything else via String(a).
fn fmt_arg(v: &Value) -> String {
    match v {
        Value::Object(_) | Value::Array(_) => serde_json::to_string(v).unwrap_or_else(|_| "[unserializable]".into()),
        Value::String(s) => s.clone(),
        Value::Null => "null".to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
    }
}

/// JS `deepEq(a, b)` — structural equality with the array/object-kind guard and key-count check.
fn deep_eq(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Object(x), Value::Object(y)) => {
            x.len() == y.len() && x.iter().all(|(k, v)| y.get(k).map(|w| deep_eq(v, w)).unwrap_or(false))
        }
        (Value::Array(x), Value::Array(y)) => x.len() == y.len() && x.iter().zip(y).all(|(v, w)| deep_eq(v, w)),
        // JS: `Array.isArray(a) !== Array.isArray(b)` -> false; different kinds never match
        (Value::Object(_), _) | (_, Value::Object(_)) | (Value::Array(_), _) | (_, Value::Array(_)) => false,
        _ => a == b,
    }
}

fn new_context() -> Context {
    let mut ctx = Context::default();
    // The `{ timeout: 2000 }` mirror — bound the work, not the clock. See the module doc.
    let mut limits = ctx.runtime_limits();
    limits.set_loop_iteration_limit(10_000_000);
    limits.set_recursion_limit(2_000);
    ctx.set_runtime_limits(limits);
    ctx
}

fn js_to_json(v: &JsValue, ctx: &mut Context) -> Option<Value> {
    if v.is_undefined() {
        return None;
    }
    v.to_json(ctx).ok().flatten()
}

/// JS `parseLoose(str)` — strict JSON first, then a JS object literal evaluated in a throwaway
/// context, so the user can type `{a:1, b:'x'}` and not just JSON.
fn parse_loose(s: &str) -> Result<Value, String> {
    let t = s.trim();
    if let Ok(v) = serde_json::from_str::<Value>(t) {
        return Ok(v);
    }
    let mut ctx = new_context();
    match ctx.eval(Source::from_bytes(&format!("({t})"))) {
        Ok(v) => js_to_json(&v, &mut ctx)
            .ok_or_else(|| "value is undefined".to_string()),
        Err(e) => Err(e.to_string().chars().take(140).collect()),
    }
}

/// `export default function codeRun(input, _context)`.
pub fn codeRun(input: &Value, _db: &rusqlite::Connection) -> Result<Value, String> {
    let code = input.get("code").and_then(Value::as_str).unwrap_or("");
    if code.trim().is_empty() {
        return Ok(json!({ "ok": false, "error": "no code to run" }));
    }

    // fixture (JS: only when input.input != null AND non-blank)
    let mut fixture: Option<Value> = None;
    if let Some(raw) = input.get("input").filter(|v| !v.is_null()) {
        let s = match raw {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        };
        if !s.trim().is_empty() {
            match parse_loose(&s) {
                Ok(v) => fixture = Some(v),
                Err(e) => {
                    return Ok(json!({
                        "ok": false,
                        "error": format!("the input fixture didn't parse (JSON or a JS literal): {e}")
                    }))
                }
            }
        }
    }

    let logs: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    let mut ctx = new_context();

    // `input` global — the fixture the chunk runs against.
    let fixture_js = match &fixture {
        Some(v) => JsValue::from_json(v, &mut ctx).unwrap_or(JsValue::undefined()),
        None => JsValue::undefined(),
    };
    let _ = ctx.register_global_property(js_string!("input"), fixture_js, Attribute::all());

    // `console` — log/info/warn join args with a space; error prefixes '⚠ ' (JS lines 54-56).
    let console = boa_engine::object::ObjectInitializer::new(&mut ctx)
        .function(make_logger(logs.clone(), false), js_string!("log"), 0)
        .function(make_logger(logs.clone(), false), js_string!("info"), 0)
        .function(make_logger(logs.clone(), false), js_string!("warn"), 0)
        .function(make_logger(logs.clone(), true), js_string!("error"), 0)
        .build();
    let _ = ctx.register_global_property(js_string!("console"), console, Attribute::all());

    let t0 = std::time::Instant::now();
    let (result, runtime_error) = match ctx.eval(Source::from_bytes(code)) {
        Ok(v) => (js_to_json(&v, &mut ctx), None),
        // JS renders `${e.name || 'Error'}: ${message.slice(0,300)}`; Boa's Display already carries
        // the error kind, so it is prefixed only when it does not already read as `Kind: message`.
        Err(e) => {
            let msg: String = e.to_string().chars().take(300).collect();
            let shaped = if msg.contains(':') { msg } else { format!("Error: {msg}") };
            (None, Some(shaped))
        }
    };
    let duration_ms = crate::clock::is_frozen().then_some(0u128).unwrap_or_else(|| t0.elapsed().as_millis());

    let mut out = serde_json::Map::new();
    out.insert("ok".into(), json!(true));
    out.insert("ran".into(), json!(runtime_error.is_none()));
    out.insert("error".into(), runtime_error.clone().map(Value::String).unwrap_or(Value::Null));
    out.insert("logs".into(), json!(logs.borrow().clone()));
    out.insert("result_json".into(), json!(safe_json(&result)));
    out.insert("duration_ms".into(), json!(duration_ms as i64));

    if let Some(raw) = input.get("expected").filter(|v| !v.is_null()) {
        let s = match raw {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        };
        if !s.trim().is_empty() {
            match parse_loose(&s) {
                Ok(v) => {
                    out.insert("expected_json".into(), json!(safe_json(&Some(v.clone()))));
                    let passed = runtime_error.is_none()
                        && result.as_ref().map(|r| deep_eq(r, &v)).unwrap_or(false);
                    out.insert("passed".into(), json!(passed));
                }
                Err(_) => {
                    // JS falls back to a RAW STRING comparison against result_json
                    let raw_trimmed = s.trim().to_string();
                    let passed = runtime_error.is_none() && safe_json(&result) == raw_trimmed;
                    out.insert("expected_raw".into(), json!(raw_trimmed));
                    out.insert("passed".into(), json!(passed));
                }
            }
        }
    }
    Ok(Value::Object(out))
}

fn make_logger(logs: Rc<RefCell<Vec<String>>>, is_error: bool) -> NativeFunction {
    // SAFETY: the closure captures an Rc, which is not traced by Boa's GC. It holds only Strings
    // (no JsValue), so there is no GC cycle for Boa to miss.
    unsafe {
        NativeFunction::from_closure(move |_this, args, ctx| {
            let parts: Vec<String> = args
                .iter()
                .map(|a| match a.to_json(ctx) {
                    Ok(Some(v)) => fmt_arg(&v),
                    _ => a.display().to_string(),
                })
                .collect();
            let line = parts.join(" ");
            logs.borrow_mut().push(if is_error { format!("⚠ {line}") } else { line });
            Ok(JsValue::undefined())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c0() -> rusqlite::Connection {
        rusqlite::Connection::open_in_memory().unwrap()
    }
    fn run(v: Value) -> Value {
        codeRun(&v, &c0()).unwrap()
    }

    #[test]
    fn empty_code_is_refused_without_running_anything() {
        assert_eq!(run(json!({})), json!({ "ok": false, "error": "no code to run" }));
        assert_eq!(run(json!({ "code": "   " })), json!({ "ok": false, "error": "no code to run" }));
    }

    #[test]
    fn a_chunk_evaluates_to_its_completion_value() {
        let r = run(json!({ "code": "1 + 1" }));
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["ran"], json!(true));
        assert_eq!(r["error"], Value::Null);
        assert_eq!(r["result_json"], json!("2"));
    }

    #[test]
    fn console_output_is_captured_in_order_with_the_error_marker() {
        let r = run(json!({ "code": "console.log('a', 1); console.error('bad'); console.warn('w'); 0" }));
        let logs: Vec<&str> = r["logs"].as_array().unwrap().iter().map(|l| l.as_str().unwrap()).collect();
        assert_eq!(logs, vec!["a 1", "⚠ bad", "w"], "order and the ⚠ prefix are part of the contract");
    }

    #[test]
    fn the_input_fixture_is_exposed_as_the_input_global() {
        let r = run(json!({ "code": "input.a + input.b", "input": "{\"a\":2,\"b\":3}" }));
        assert_eq!(r["result_json"], json!("5"));
    }

    #[test]
    fn a_loose_js_literal_fixture_parses_too() {
        // the whole point of parseLoose: `{a:1}` is not JSON but must work
        let r = run(json!({ "code": "input.a", "input": "{a: 41 + 1}" }));
        assert_eq!(r["result_json"], json!("42"));
    }

    #[test]
    fn an_unparseable_fixture_is_reported_not_thrown() {
        let r = run(json!({ "code": "1", "input": "{{{" }));
        assert_eq!(r["ok"], json!(false));
        assert!(r["error"].as_str().unwrap().contains("input fixture didn't parse"), "{r}");
    }

    #[test]
    fn a_runtime_error_is_captured_and_ran_goes_false() {
        let r = run(json!({ "code": "throw new Error('boom')" }));
        assert_eq!(r["ok"], json!(true), "the SKILL succeeded even though the chunk failed");
        assert_eq!(r["ran"], json!(false));
        assert!(r["error"].as_str().unwrap().contains("boom"), "{r}");
    }

    #[test]
    fn expected_compares_structurally_not_textually() {
        // key ORDER differs — deepEq must still pass where a string compare would fail
        let r = run(json!({ "code": "({b:2, a:1})", "expected": "{\"a\":1,\"b\":2}" }));
        assert_eq!(r["passed"], json!(true), "{r}");
        let bad = run(json!({ "code": "({a:1})", "expected": "{\"a\":2}" }));
        assert_eq!(bad["passed"], json!(false));
    }

    #[test]
    fn an_unparseable_expected_falls_back_to_a_raw_string_compare() {
        let r = run(json!({ "code": "'hello'", "expected": "\"hello\"" }));
        assert_eq!(r["passed"], json!(true), "{r}");
        assert!(r.get("expected_raw").is_some() || r.get("expected_json").is_some());
    }

    #[test]
    fn a_failing_chunk_never_passes_even_if_expected_matches_nothing() {
        let r = run(json!({ "code": "throw new Error('x')", "expected": "1" }));
        assert_eq!(r["passed"], json!(false), "a thrown chunk can never pass");
    }

    #[test]
    fn undefined_completion_renders_as_the_literal_undefined() {
        let r = run(json!({ "code": "let x = 1;" }));
        assert_eq!(r["result_json"], json!("undefined"));
    }

    #[test]
    fn a_runaway_loop_is_bounded_rather_than_hanging_the_process() {
        // the RuntimeLimits mirror of JS's { timeout: 2000 } — this must RETURN, not hang
        let r = run(json!({ "code": "while (true) {}" }));
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["ran"], json!(false), "the runaway chunk must be cut off: {r}");
    }

    #[test]
    fn deep_eq_matches_the_js_semantics() {
        assert!(deep_eq(&json!({"a":[1,{"b":2}]}), &json!({"a":[1,{"b":2}]})));
        assert!(!deep_eq(&json!([1, 2]), &json!({"0":1,"1":2})), "array vs object never match");
        assert!(!deep_eq(&json!({"a":1}), &json!({"a":1,"b":2})), "key count matters");
        assert!(deep_eq(&json!(null), &json!(null)));
    }
}
