//! THE CODE NODE (RFC 0011 §11): JSON in, JSON out, Rhai with no clock, no I/O, no network, limits on time and operations.
use rhai::{Engine, EvalAltResult};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::time::{Duration, Instant};

pub const DEFAULT_TIME_MS: u64 = 50;
pub const DEFAULT_OP_LIMIT: u64 = 10_000;
/// A spec may ask for more room, never without bound (a patch can set these).
pub const MAX_TIME_MS: u64 = 5_000;
pub const MAX_OP_LIMIT: u64 = 1_000_000;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Example {
    pub input: Value,
    pub output: Value,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Limits {
    #[serde(default)] pub time_limit_ms: Option<u64>,
    #[serde(default)] pub op_limit: Option<u64>,
}

impl Limits {
    pub fn time_ms(&self) -> u64 { self.time_limit_ms.unwrap_or(DEFAULT_TIME_MS).min(MAX_TIME_MS) }
    pub fn ops(&self) -> u64 { self.op_limit.unwrap_or(DEFAULT_OP_LIMIT).min(MAX_OP_LIMIT) }
}

fn engine(limits: &Limits) -> Engine {
    let mut e = Engine::new();
    e.set_max_expr_depths(64, 32);
    e.set_max_string_size(200_000);
    e.set_max_array_size(10_000);
    e.set_max_map_size(10_000);
    // no ambient authority: no module loading from disk, nothing printed, no clock (the `no_time` feature)
    e.set_module_resolver(rhai::module_resolvers::DummyModuleResolver::new());
    e.on_print(|_| {}); e.on_debug(|_, _, _| {});
    e
}

fn normalize(v: Value) -> Value {
    match v {
        Value::Number(n) => json!(n.as_f64().unwrap_or(0.0)),
        Value::Array(a) => Value::Array(a.into_iter().map(normalize).collect()),
        Value::Object(m) => {
            let mut out = serde_json::Map::new();
            for (k, x) in m {
                out.insert(k, normalize(x));
            }
            Value::Object(out)
        }
        other => other,
    }
}

fn same(a: &Value, b: &Value) -> bool { normalize(a.clone()) == normalize(b.clone()) }

/// Run Rhai on one JSON value. Pure when the engine has no ambient authority.
pub fn run(source: &str, input: &Value, limits: &Limits) -> Result<Value, String> {
    let source = source.trim();
    if source.is_empty() { return Err("the program is empty".into()); }
  let input_dyn = rhai::serde::to_dynamic(input).map_err(|e| format!("the input is not valid for the sandbox: {e}"))?;
    let mut engine = engine(limits);
    let start = Instant::now();
    let max = Duration::from_millis(limits.time_ms());
    let src_c = source.to_string();
    let max_ops = limits.ops();
    engine.on_progress(move |ops| {
        if start.elapsed() > max { Some("the time limit was reached".into()) }
        else if ops >= max_ops { Some("the operation limit was reached".into()) }
        else { None }
    });
    let mut scope = rhai::Scope::new();
    scope.push("input", input_dyn);
    let out = engine.eval_with_scope::<rhai::Dynamic>(&mut scope, &src_c).map_err(|e| match *e { EvalAltResult::ErrorTerminated(why, _) => why.to_string(), other => other.to_string() })?;
    rhai::serde::from_dynamic(&out).map_err(|e| format!("the program did not return JSON: {e}"))
}

/// What is wrong with a code spec before any run, in words.
pub fn check(source: &str, examples: &[Example], limits: &Limits) -> Vec<String> {
    let mut out = vec![];
    if source.trim().is_empty() { out.push("a code node needs a program".into()); }
    if examples.is_empty() { out.push("a code node needs at least one example (input → expected output)".into()); return out; }
    for (i, ex) in examples.iter().enumerate() {
        let label = format!("example {}", i + 1);
        match run(source, &ex.input, limits) {
            Ok(got) if same(&got, &ex.output) => {}
            Ok(got) => out.push(format!("{label}: expected {}, got {}", ex.output, got)),
            Err(e) => out.push(format!("{label}: {e}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const SRC: &str = r#"
        let a = input["a"];
        let b = input["b"];
        #{"sum": a + b}
    "#;

    #[test]
    fn code_runs_json_in_and_json_out() {
        let out = run(SRC, &json!({ "a": 2, "b": 3 }), &Limits::default()).unwrap();
        assert_eq!(out, json!({ "sum": 5 }));
    }

    #[test]
    fn code_without_a_passing_example_is_refused_at_save() {
        let bad = check(SRC, &[], &Limits::default());
        assert!(bad.iter().any(|m| m.contains("at least one example")));
        let wrong = check(SRC, &[Example { input: json!({ "a": 1, "b": 1 }), output: json!({ "sum": 99 }) }], &Limits::default());
        assert!(wrong.iter().any(|m| m.contains("expected")));
    }

    #[test]
    fn code_cannot_read_the_clock_use_randomness_touch_files_or_the_network() {
        // each program must be refused for the RIGHT reason: the name is not there at all
        for (src, what) in [
            ("let t = timestamp(); 1", "timestamp"),
            ("rand()", "rand"),
            (r#"read_file("x")"#, "read_file"),
            (r#"http_get("http://example.com")"#, "http_get"),
            (r#"import "secrets" as s; 1"#, "secrets"),
            ("let t = 1; t.elapsed", "elapsed"),
        ] {
            let err = run(src, &json!({}), &Limits::default()).unwrap_err();
            assert!(err.contains(what), "{src}: {err}");
            assert!(!err.contains("did not return JSON"), "{src} must fail before it returns: {err}");
        }
    }

    #[test]
    fn code_is_stopped_by_the_time_and_operation_limits() {
        let lim = Limits { time_limit_ms: Some(5), op_limit: Some(50_000) };
        let slow = run("while true {}", &json!({}), &lim).unwrap_err();
        assert!(slow.contains("time limit") || slow.contains("operation limit"), "{slow}");
        let lim = Limits { time_limit_ms: Some(5_000), op_limit: Some(100) };
        let busy = run("let x = 0; while x < 1_000_000 { x += 1; } x", &json!({}), &lim).unwrap_err();
        assert!(busy.contains("operation limit") || busy.contains("time limit"), "{busy}");
    }

    #[test]
    fn code_is_deterministic_run_twice_same_answer() {
        let input = json!({ "a": 7, "b": 11 });
        let a = run(SRC, &input, &Limits::default()).unwrap();
        let b = run(SRC, &input, &Limits::default()).unwrap();
        assert_eq!(a, b);
    }
}