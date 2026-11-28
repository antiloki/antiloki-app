//! codebase/test-suite-run.rs — twin of skills/codebase/test-suite-run.mjs (default export
//! `async function testRun(input, _context)`, no-db). NAMES ARE CONTRACT: `testRun` (default),
//! `detectTestCmd`, `parseCounts` (named exports).
//!
//! BESPOKE-NATIVE (no conform-db fixture/frontier entry), same precedent as `lint-eslint.rs` /
//! `_helpers/run-cmd.rs`'s `runCmd`: `testRun` spawns a REAL child process (the project's own
//! detected test runner) over the REAL workspace filesystem — not a value the conform-db two-lane
//! harness can seed/replay. Proven instead by real `#[cfg(test)]` tests:
//!   - `detectTestCmd` (pure fs read + JSON parse) is exercised directly against real temp dirs
//!     with committed package.json fixtures (vitest / jest / npm-script / node:test fallback /
//!     missing package.json) — deterministic, no subprocess needed.
//!   - `parseCounts` (pure string parsing) is exercised directly against literal reporter output
//!     strings for both the node:test and vitest branches, plus the neither-matches fallback.
//!   - `testRun` itself is exercised end-to-end against a REAL temp dir containing a trivial
//!     `node:test` file (no package.json → `detectTestCmd` falls back to `node --test`, `shell:
//!     false`, matching the JS default path exactly) — a real `node --test` subprocess runs,
//!     `runCmd` (twin already converted) captures its real stdout, and `parseCounts` parses the
//!     real node:test spec-reporter output. This is the same "real subprocess over a real fixture
//!     tree, no mocking" style as `lint-eslint.rs`'s `not_installed_when_no_eslint_anywhere`.
//!
//! `runCmd` is reused verbatim from the already-converted `codebase__helpers_run_cmd::runCmd`
//! twin (module `crate::codebase__helpers_run_cmd`) — not re-implemented here, per the crossing
//! playbook's "reuse already-converted twins" rule.
//!
//! `context` (JS `_context`, unused — only `context.db` is documented in the header comment and
//! never actually read by the function body) has no Rust parameter here, mirroring
//! `lint-eslint.rs`'s db-first-but-unused-fields convention: this unit does no db reads/writes at
//! all (JS body never touches `context`), so `testRun` below takes no `db: &Connection` parameter
//! — the ONE deliberate deviation from the `(input, db)` two-arg convention used elsewhere, because
//! there is nothing db-shaped to thread through; see module doc precedent in `run-cmd.rs` (a
//! no-db helper) for the same shape.
// QUALITY: 80/100 — real subprocess (bespoke-native, no fixture) reusing the converted runCmd twin; detectTestCmd/parseCounts pure and directly fixture-tested.

use serde_json::{json, Map, Value};
use std::path::Path;

/// `export function detectTestCmd(cwd)` — reads `{cwd}/package.json`, picks vitest > jest >
/// npm-script `test` > node:test fallback. JS wraps the whole body in `try/catch`, collapsing
/// ANY failure (missing file, malformed JSON) to the node:test fallback — mirrored here by
/// treating a failed read/parse identically to a missing package.json.
pub fn detectTestCmd(cwd: &str) -> Value {
    let pj_path = Path::new(cwd).join("package.json");
    let pj: Option<Value> = std::fs::read_to_string(&pj_path)
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok());

    if let Some(pj) = pj {
        let deps = pj.get("dependencies").and_then(Value::as_object);
        let dev_deps = pj.get("devDependencies").and_then(Value::as_object);
        let has_dep = |name: &str| -> bool {
            deps.map_or(false, |m| m.contains_key(name)) || dev_deps.map_or(false, |m| m.contains_key(name))
        };
        if has_dep("vitest") {
            return json!({ "cmd": "npx", "args": ["vitest", "run"], "framework": "vitest", "shell": true });
        }
        if has_dep("jest") {
            return json!({ "cmd": "npx", "args": ["jest"], "framework": "jest", "shell": true });
        }
        let has_test_script = pj
            .get("scripts")
            .and_then(Value::as_object)
            .map_or(false, |s| s.get("test").map_or(false, |v| !v.is_null()));
        if has_test_script {
            return json!({ "cmd": "npm", "args": ["test"], "framework": "npm-script", "shell": true });
        }
    }
    json!({ "cmd": "node", "args": ["--test"], "framework": "node-test", "shell": false })
}

/// `export function parseCounts(out)` — best-effort parse of node:test (spec/TAP) or vitest
/// reporter output. `String(out ?? '')` — JS `??` only substitutes on null/undefined; ported as
/// treating a null input as empty, else stringifying (all real call sites pass a JS string).
pub fn parseCounts(out: &str) -> Value {
    let text = out;
    // node:test spec reporter ("ℹ pass 12") + TAP ("# pass 12")
    if regex_tests_line(text) {
        return json!({
            "tests": capture_int(text, &["ℹ", "#"], "tests"),
            "pass": capture_int(text, &["ℹ", "#"], "pass"),
            "fail": capture_int(text, &["ℹ", "#"], "fail"),
        });
    }
    let vp = capture_after_tests_word(text, "passed");
    let vf = capture_after_tests_word(text, "failed");
    if vp.is_some() || vf.is_some() {
        let p = vp.unwrap_or(0);
        let f = vf.unwrap_or(0);
        return json!({ "pass": p, "fail": f, "tests": p + f });
    }
    json!({ "tests": Value::Null, "pass": Value::Null, "fail": Value::Null })
}

// `/(?:ℹ|#)\s*tests?\s+\d+/` — a line starting with ℹ or # (any leading text before it doesn't
// matter since JS `test()` isn't anchored), followed by optional whitespace, "test" or "tests",
// whitespace, then digits.
fn regex_tests_line(text: &str) -> bool {
    find_after_marker(text, "tests").is_some() || find_after_marker(text, "test").is_some()
}

// Finds `(?:ℹ|#)\s*{word}\s+(\d+)` and returns the matched digit run as &str, else None.
fn find_after_marker(text: &str, word: &str) -> Option<String> {
    for (idx, _) in text.match_indices(|c| c == 'ℹ' || c == '#') {
        let rest = &text[idx + text[idx..].chars().next().unwrap().len_utf8()..];
        let trimmed = skip_ws(rest);
        let after_word = if word == "tests" {
            trimmed.strip_prefix("tests").or_else(|| trimmed.strip_prefix("test"))
        } else {
            trimmed.strip_prefix(word)
        };
        if let Some(rest2) = after_word {
            let rest2b = skip_ws_required(rest2)?;
            let digits = take_digits(rest2b);
            if !digits.is_empty() {
                return Some(digits);
            }
        }
    }
    None
}

fn skip_ws(s: &str) -> &str {
    s.trim_start_matches(|c: char| c.is_whitespace())
}

// `\s+` requires at least one whitespace char.
fn skip_ws_required(s: &str) -> Option<&str> {
    let trimmed = s.trim_start_matches(|c: char| c.is_whitespace());
    if trimmed.len() == s.len() {
        None
    } else {
        Some(trimmed)
    }
}

fn take_digits(s: &str) -> String {
    s.chars().take_while(|c| c.is_ascii_digit()).collect()
}

fn capture_int(text: &str, markers: &[&str], word: &str) -> Value {
    for m in markers {
        for (idx, _) in text.match_indices(m) {
            let rest = &text[idx + m.len()..];
            let trimmed = skip_ws(rest);
            let after_word = if word == "tests" {
                trimmed.strip_prefix("tests").or_else(|| trimmed.strip_prefix("test"))
            } else {
                trimmed.strip_prefix(word)
            };
            if let Some(rest2) = after_word {
                if let Some(rest3) = skip_ws_required(rest2) {
                    let digits = take_digits(rest3);
                    if !digits.is_empty() {
                        return json!(digits.parse::<i64>().unwrap_or(0));
                    }
                }
            }
        }
    }
    Value::Null
}

// `/Tests\s+[^\n]*?(\d+)\s+passed/` (and `failed`) — "Tests", whitespace, then lazily the FIRST
// digit run followed by whitespace+word, anywhere on the same line (no newline crossing).
fn capture_after_tests_word(text: &str, word: &str) -> Option<i64> {
    for (idx, _) in text.match_indices("Tests") {
        let rest = &text[idx + 5..];
        let rest = skip_ws_required(rest)?;
        let line_end = rest.find('\n').unwrap_or(rest.len());
        let line = &rest[..line_end];
        // Lazy match: scan left-to-right for the first digit run immediately followed (after
        // \s+) by `word`.
        let bytes = line.as_bytes();
        let mut i = 0usize;
        while i < bytes.len() {
            if bytes[i].is_ascii_digit() {
                let start = i;
                while i < bytes.len() && bytes[i].is_ascii_digit() {
                    i += 1;
                }
                let digits = &line[start..i];
                if let Some(after) = skip_ws_required(&line[i..]) {
                    if after.starts_with(word) {
                        return digits.parse::<i64>().ok();
                    }
                }
            } else {
                i += 1;
            }
        }
    }
    None
}

/// `export default async function testRun(input, _context)` — see module doc: real subprocess,
/// bespoke-native (no fixture/frontier entry). `input.cwd ?? input.root ?? process.cwd()`;
/// `process.cwd()` is mirrored via `std::env::current_dir()`.
pub fn testRun(input: &Value) -> Value {
    let cwd = input
        .get("cwd")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .or_else(|| input.get("root").and_then(Value::as_str).filter(|s| !s.is_empty()))
        .map(str::to_string)
        .unwrap_or_else(|| {
            std::env::current_dir()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default()
        });

    let det = detectTestCmd(&cwd);
    let det_cmd = det.get("cmd").and_then(Value::as_str).unwrap_or_default().to_string();
    let det_args: Vec<String> = det
        .get("args")
        .and_then(Value::as_array)
        .map(|a| a.iter().map(|v| v.as_str().unwrap_or_default().to_string()).collect())
        .unwrap_or_default();
    let det_framework = det.get("framework").and_then(Value::as_str).unwrap_or_default().to_string();
    let det_shell = matches!(det.get("shell"), Some(Value::Bool(true)));

    let mut args = det_args.clone();
    if let Some(path) = input.get("path").and_then(Value::as_str).filter(|s| !s.is_empty()) {
        args.push(path.to_string());
    }

    let timeout_ms = input.get("timeout_ms").and_then(Value::as_u64).unwrap_or(300000);
    let args_json: Vec<Value> = args.iter().map(|a| json!(a)).collect();
    let opts = json!({ "cwd": cwd, "timeoutMs": timeout_ms, "shell": det_shell });
    let r = crate::codebase__helpers_run_cmd::runCmd(&json!(det_cmd), &Value::Array(args_json), &opts);

    let stdout = r.get("stdout").and_then(Value::as_str).unwrap_or_default();
    let stderr = r.get("stderr").and_then(Value::as_str).unwrap_or_default();
    let blob = if stderr.is_empty() { stdout.to_string() } else { format!("{stdout}\n{stderr}") };
    let has_error = matches!(r.get("error"), Some(Value::Bool(true)));
    let timed_out = matches!(r.get("timedOut"), Some(Value::Bool(true)));
    let code = r.get("code").and_then(Value::as_i64).unwrap_or(0);
    let passed = !has_error && !timed_out && code == 0;

    let counts = parseCounts(&blob);
    let command = format!("{det_cmd} {}", args.join(" "));

    let tail: String = {
        let bytes = blob.as_bytes();
        if bytes.len() <= 8000 {
            blob.clone()
        } else {
            // JS `.slice(-8000)` is UTF-16-code-unit based; approximate via a UTF-8-safe
            // byte-boundary trim from the tail (no real call site exercises non-ASCII output at
            // exactly this boundary).
            let start = bytes.len() - 8000;
            let mut s = start;
            while s < bytes.len() && !blob.is_char_boundary(s) {
                s += 1;
            }
            blob[s..].to_string()
        }
    };

    let mut o = Map::new();
    o.insert("ok".into(), json!(true));
    o.insert("passed".into(), json!(passed));
    o.insert("framework".into(), json!(det_framework));
    o.insert("command".into(), json!(command));
    o.insert("code".into(), json!(code));
    o.insert("timed_out".into(), json!(timed_out));
    if let Value::Object(c) = counts {
        for (k, v) in c {
            o.insert(k, v);
        }
    }
    o.insert("output_tail".into(), json!(tail));
    Value::Object(o)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    fn temp_dir(tag: &str) -> PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!("antiloki-test-suite-run-rs-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn detect_test_cmd_prefers_vitest_over_jest() {
        let dir = temp_dir("vitest");
        fs::write(
            dir.join("package.json"),
            r#"{"devDependencies":{"vitest":"^1.0.0","jest":"^29.0.0"}}"#,
        )
        .unwrap();
        let d = detectTestCmd(dir.to_str().unwrap());
        assert_eq!(d["framework"], json!("vitest"));
        assert_eq!(d["cmd"], json!("npx"));
        assert_eq!(d["args"], json!(["vitest", "run"]));
        assert_eq!(d["shell"], json!(true));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn detect_test_cmd_falls_back_to_jest() {
        let dir = temp_dir("jest");
        fs::write(dir.join("package.json"), r#"{"dependencies":{"jest":"^29.0.0"}}"#).unwrap();
        let d = detectTestCmd(dir.to_str().unwrap());
        assert_eq!(d["framework"], json!("jest"));
        assert_eq!(d["args"], json!(["jest"]));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn detect_test_cmd_falls_back_to_npm_script() {
        let dir = temp_dir("npm-script");
        fs::write(dir.join("package.json"), r#"{"scripts":{"test":"node run-tests.js"}}"#).unwrap();
        let d = detectTestCmd(dir.to_str().unwrap());
        assert_eq!(d["framework"], json!("npm-script"));
        assert_eq!(d["cmd"], json!("npm"));
        assert_eq!(d["args"], json!(["test"]));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn detect_test_cmd_falls_back_to_node_test_when_no_package_json() {
        let dir = temp_dir("no-pj");
        let d = detectTestCmd(dir.to_str().unwrap());
        assert_eq!(d["framework"], json!("node-test"));
        assert_eq!(d["cmd"], json!("node"));
        assert_eq!(d["args"], json!(["--test"]));
        assert_eq!(d["shell"], json!(false));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn detect_test_cmd_falls_back_when_package_json_malformed() {
        let dir = temp_dir("malformed");
        fs::write(dir.join("package.json"), "not json{{{").unwrap();
        let d = detectTestCmd(dir.to_str().unwrap());
        assert_eq!(d["framework"], json!("node-test"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn parse_counts_node_test_spec_reporter() {
        let out = "ℹ tests 12\nℹ pass 10\nℹ fail 2\n";
        let c = parseCounts(out);
        assert_eq!(c["tests"], json!(12));
        assert_eq!(c["pass"], json!(10));
        assert_eq!(c["fail"], json!(2));
    }

    #[test]
    fn parse_counts_node_test_tap_reporter() {
        let out = "# tests 5\n# pass 5\n# fail 0\n";
        let c = parseCounts(out);
        assert_eq!(c["tests"], json!(5));
        assert_eq!(c["pass"], json!(5));
        assert_eq!(c["fail"], json!(0));
    }

    #[test]
    fn parse_counts_vitest_passed_only() {
        let out = "Tests  2 passed (2)\n";
        let c = parseCounts(out);
        assert_eq!(c["pass"], json!(2));
        assert_eq!(c["fail"], json!(0));
        assert_eq!(c["tests"], json!(2));
    }

    #[test]
    fn parse_counts_vitest_mixed() {
        let out = "Tests  1 failed | 1 passed (2)\n";
        let c = parseCounts(out);
        assert_eq!(c["fail"], json!(1));
        assert_eq!(c["pass"], json!(1));
        assert_eq!(c["tests"], json!(2));
    }

    #[test]
    fn parse_counts_no_match_returns_nulls() {
        let c = parseCounts("nothing recognizable here");
        assert_eq!(c["tests"], Value::Null);
        assert_eq!(c["pass"], Value::Null);
        assert_eq!(c["fail"], Value::Null);
    }

    #[test]
    fn test_run_real_node_test_subprocess_passes() {
        let dir = temp_dir("run-pass");
        fs::write(
            dir.join("sample.test.js"),
            "const test = require('node:test');\nconst assert = require('node:assert');\ntest('adds', () => { assert.strictEqual(1 + 1, 2); });\n",
        )
        .unwrap();
        let r = testRun(&json!({ "cwd": dir.to_str().unwrap(), "path": "sample.test.js" }));
        assert_eq!(r["framework"], json!("node-test"));
        assert!(r["command"].as_str().unwrap().starts_with("node --test"));
        assert_eq!(r["code"], json!(0));
        assert_eq!(r["passed"], json!(true));
        assert_eq!(r["timed_out"], json!(false));
        assert!(r["output_tail"].as_str().unwrap().len() > 0);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_run_real_node_test_subprocess_reports_failure() {
        let dir = temp_dir("run-fail");
        fs::write(
            dir.join("sample.test.js"),
            "const test = require('node:test');\nconst assert = require('node:assert');\ntest('fails', () => { assert.strictEqual(1, 2); });\n",
        )
        .unwrap();
        let r = testRun(&json!({ "cwd": dir.to_str().unwrap(), "path": "sample.test.js" }));
        assert_eq!(r["passed"], json!(false));
        assert_ne!(r["code"], json!(0));
        let _ = fs::remove_dir_all(&dir);
    }
}
