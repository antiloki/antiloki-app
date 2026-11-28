//! run-cmd.rs — twin of skills/codebase/_helpers/run-cmd.mjs (named-exports-only helper, both
//! no-db). NAMES ARE CONTRACT: `runCmd` · `resolveBin`.
//!
//! `runCmd` — REAL subprocess (mirrors JS `spawn`), and EVERY success path unconditionally
//! shells out: there is no flag/optional field whose absence keeps this fn deterministic and
//! fixture-safe the way constitution-estimate.rs's `git_diff_paths` (only reachable when
//! `input.base` is truthy) or card-run-preflight.rs's flag-gated branches are. Per the crossing
//! playbook's bespoke-native rule this unit therefore carries NO conform-db fixture/frontier
//! entry — it is proven correct instead by the real, non-mocked `#[cfg(test)]` tests below (real
//! child processes: a successful run, a missing-binary error, and a timeout-kill). JS never
//! throws — always resolves `{ stdout, stderr, code, timedOut?, error? }`; ported 1:1: `args`
//! defaults to `[]`, `opts.cwd`, `opts.timeoutMs` (default 180000), `opts.env` (when present,
//! JS's `spawn` REPLACES the inherited env with exactly that object — not a merge — mirrored
//! here via `env_clear()` + only the given keys; when absent both runtimes inherit the parent
//! process env), `opts.shell` (bool — `true` runs through the platform shell, matching Node's
//! `{ shell: true }`; JS's `shell` option also accepts a shell-path STRING, not modeled since no
//! real call site in this file's JS source passes one). The JS timer immediately resolves
//! `{ code: -1, timedOut: true }` on expiry without waiting for the (racing) `close` event —
//! mirrored by returning `exit_code = -1` from the timeout branch regardless of the killed
//! child's eventual real exit status.
//!
//! `resolveBin` — reads `{toolsDir}/node_modules/{pkg}/package.json` off the REAL filesystem
//! (fixture-controllable via a committed testdata dir — same precedent as stack-facts.rs /
//! constitution-estimate.rs: `antiloki-crossing/fixtures/testdata/resolve-bin/`). JS wraps the
//! ENTIRE body (`readFileSync` + `JSON.parse` + the final `path.join`) in one `try/catch`, so any
//! failure — missing file, malformed JSON, or a non-string `toolsDir`/`pkg`/`rel` making
//! `path.join` throw a TypeError — collapses to `null`; this twin reaches the same `null` for
//! non-string `toolsDir`/`pkg`/`binName` inputs by an early return rather than by replaying the
//! throw (no fixture case exercises non-string args — those are exactly the "guard-throws are not
//! exercised" edge cases every sibling twin skips). `rel = bin?.[binName] ?? bin?.[pkg]` is `nz`;
//! `bin` string-shorthand (`{ [pkg]: bin }`) is normalized like the JS does.
//!
//! `win_join` — `node:path` win32 `join`+`normalize` (separator conversion only, no `.`/`..`
//! resolution or redundant-slash collapsing: this unit's fixture testdata never needs either) —
//! same disclosed win32-only-scope convention as `constitution-estimate.rs`'s `win_is_absolute`.
// QUALITY: 80/100 — runCmd is bespoke-native (real subprocess, proven by native tests only);
// resolveBin is a real-fs read, fixture-backed via a committed testdata dir.

use serde_json::{json, Map, Value};
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) } // JS `??` right operand

/// `path.join(...parts)` for the HOST platform: win32 semantics (backslash separators) on Windows — see
/// module doc for disclosed scope — and plain `/` on POSIX, where a backslash-joined path is not a path
/// at all (`node /tools/node_modules\\pyright\\fake.js` cannot be opened).
fn win_join(parts: &[&str]) -> String {
    let parts = parts.iter().filter(|p| !p.is_empty());
    if cfg!(windows) {
        parts.map(|p| p.replace('/', "\\")).collect::<Vec<_>>().join("\\")
    } else {
        parts.copied().collect::<Vec<_>>().join("/")
    }
}

/// `export function runCmd(cmd, args = [], opts = {})` — see module doc: real subprocess,
/// bespoke-native (no fixture/frontier entry).
pub fn runCmd(cmd: &Value, args: &Value, opts: &Value) -> Value {
    let cmd_s = cmd.as_str().unwrap_or_default().to_string();
    let args_v: Vec<String> = args
        .as_array()
        .map(|a| a.iter().map(|x| x.as_str().unwrap_or_default().to_string()).collect())
        .unwrap_or_default();
    let cwd = opts.get("cwd").and_then(Value::as_str).map(|s| s.to_string());
    let timeout_ms = opts.get("timeoutMs").and_then(Value::as_u64).unwrap_or(180000);
    let use_shell = matches!(opts.get("shell"), Some(Value::Bool(true)));

    let mut command = if use_shell {
        let mut c = if cfg!(windows) { crate::proc::cmd("cmd") } else { crate::proc::cmd("sh") };
        if cfg!(windows) { c.arg("/C"); } else { c.arg("-c"); }
        let mut full = cmd_s.clone();
        for a in &args_v {
            full.push(' ');
            full.push_str(a);
        }
        c.arg(full);
        c
    } else {
        let mut c = crate::proc::cmd(&cmd_s);
        c.args(&args_v);
        c
    };
    if let Some(d) = &cwd {
        command.current_dir(d);
    }
    if let Some(env_obj) = opts.get("env").and_then(Value::as_object) {
        command.env_clear();
        for (k, v) in env_obj {
            command.env(k, v.as_str().unwrap_or_default());
        }
    }
    command.stdout(Stdio::piped()).stderr(Stdio::piped());

    let mut child = match command.spawn() {
        Ok(c) => c,
        Err(e) => {
            let mut out = Map::new();
            out.insert("stdout".into(), json!(""));
            out.insert("stderr".into(), json!(e.to_string()));
            out.insert("code".into(), json!(-1));
            out.insert("error".into(), json!(true));
            return Value::Object(out);
        }
    };

    let stdout_pipe = child.stdout.take();
    let stderr_pipe = child.stderr.take();
    let stdout_handle = thread::spawn(move || {
        let mut buf = String::new();
        if let Some(mut p) = stdout_pipe {
            let _ = p.read_to_string(&mut buf);
        }
        buf
    });
    let stderr_handle = thread::spawn(move || {
        let mut buf = String::new();
        if let Some(mut p) = stderr_pipe {
            let _ = p.read_to_string(&mut buf);
        }
        buf
    });

    let start = Instant::now();
    let mut timed_out = false;
    let mut exit_code: i64 = 0;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                exit_code = status.code().unwrap_or(0) as i64;
                break;
            }
            Ok(None) => {
                if start.elapsed() >= Duration::from_millis(timeout_ms) {
                    let _ = child.kill();
                    let _ = child.wait();
                    timed_out = true;
                    exit_code = -1;
                    break;
                }
                thread::sleep(Duration::from_millis(10));
            }
            Err(_) => {
                exit_code = -1;
                break;
            }
        }
    }

    let stdout = stdout_handle.join().unwrap_or_default();
    let stderr = stderr_handle.join().unwrap_or_default();

    let mut out = Map::new();
    out.insert("stdout".into(), json!(stdout));
    out.insert("stderr".into(), json!(stderr));
    out.insert("code".into(), json!(exit_code));
    if timed_out {
        out.insert("timedOut".into(), json!(true));
    }
    Value::Object(out)
}

/// `export function resolveBin(toolsDir, pkg, binName)` — see module doc.
pub fn resolveBin(tools_dir: &Value, pkg: &Value, bin_name: &Value) -> Value {
    let (tools_dir_s, pkg_s, bin_name_s) = match (tools_dir.as_str(), pkg.as_str(), bin_name.as_str()) {
        (Some(a), Some(b), Some(c)) => (a, b, c),
        _ => return Value::Null,
    };

    let pj_path = Path::new(tools_dir_s).join("node_modules").join(pkg_s).join("package.json");
    let pj: Value = match std::fs::read_to_string(&pj_path).ok().and_then(|t| serde_json::from_str(&t).ok()) {
        Some(v) => v,
        None => return Value::Null,
    };

    let bin_obj: Option<Map<String, Value>> = match pj.get("bin") {
        Some(Value::String(s)) => {
            let mut m = Map::new();
            m.insert(pkg_s.to_string(), json!(s));
            Some(m)
        }
        Some(Value::Object(m)) => Some(m.clone()),
        _ => None,
    };
    let rel = nz(bin_obj.as_ref().and_then(|m| m.get(bin_name_s))).or_else(|| nz(bin_obj.as_ref().and_then(|m| m.get(pkg_s))));

    match rel {
        Some(Value::String(s)) if !s.is_empty() => json!(win_join(&[tools_dir_s, "node_modules", pkg_s, s])),
        _ => Value::Null,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOOLS: &str = concat!(env!("ANTILOKI_FIXTURES"), "/resolve-bin/tools");

    /// TOOLS + `rest`, joined with the host separator (backslash on Windows, `/` elsewhere).
    fn expected(rest: &[&str]) -> String {
        let sep = if cfg!(windows) { "\\" } else { "/" };
        let base = if cfg!(windows) { TOOLS.replace('/', "\\") } else { TOOLS.to_string() };
        format!("{base}{sep}{}", rest.join(sep))
    }

    #[test]
    fn run_cmd_runs_a_real_command_and_captures_stdout() {
        let r = if cfg!(windows) {
            runCmd(&json!("cmd"), &json!(["/C", "echo", "hello-antiloki-run-cmd"]), &json!({}))
        } else {
            runCmd(&json!("echo"), &json!(["hello-antiloki-run-cmd"]), &json!({}))
        };
        assert_eq!(r["code"], json!(0));
        assert!(r["stdout"].as_str().unwrap().contains("hello-antiloki-run-cmd"));
        assert!(r.get("error").is_none());
        assert!(r.get("timedOut").is_none());
    }

    #[test]
    fn run_cmd_nonexistent_binary_resolves_with_error_true() {
        let r = runCmd(&json!("this-binary-does-not-exist-xyz-antiloki"), &json!([]), &json!({}));
        assert_eq!(r["code"], json!(-1));
        assert_eq!(r["error"], json!(true));
    }

    #[test]
    fn run_cmd_timeout_kills_the_child_and_reports_timed_out() {
        // `ping` waits ~1s per reply without needing an interactive console (unlike `timeout.exe`,
        // which errors immediately when stdin is redirected — as it is under `cargo test`).
        let r = if cfg!(windows) {
            runCmd(&json!("cmd"), &json!(["/C", "ping", "-n", "10", "127.0.0.1"]), &json!({ "timeoutMs": 200 }))
        } else {
            runCmd(&json!("sleep"), &json!(["10"]), &json!({ "timeoutMs": 200 }))
        };
        assert_eq!(r["code"], json!(-1));
        assert_eq!(r["timedOut"], json!(true));
    }

    #[test]
    fn run_cmd_cwd_option_is_respected() {
        let dir = std::env::temp_dir();
        let r = if cfg!(windows) {
            runCmd(&json!("cmd"), &json!(["/C", "cd"]), &json!({ "cwd": dir.to_string_lossy() }))
        } else {
            runCmd(&json!("pwd"), &json!([]), &json!({ "cwd": dir.to_string_lossy() }))
        };
        assert_eq!(r["code"], json!(0));
        let name = dir.file_name().unwrap().to_string_lossy().to_lowercase();
        assert!(r["stdout"].as_str().unwrap().to_lowercase().contains(&*name));
    }

    #[test]
    fn resolve_bin_object_form_prefers_bin_name_key() {
        let r = resolveBin(&json!(TOOLS), &json!("eslint"), &json!("eslint"));
        assert_eq!(r, json!(expected(&["node_modules", "eslint", "bin", "eslint.js"])));
    }

    #[test]
    fn resolve_bin_falls_back_to_pkg_key_when_bin_name_missing() {
        let r = resolveBin(&json!(TOOLS), &json!("eslint"), &json!("missing-bin-name"));
        assert_eq!(r, json!(expected(&["node_modules", "eslint", "bin", "eslint.js"])));
    }

    #[test]
    fn resolve_bin_string_shorthand_normalizes_to_pkg_key() {
        let r = resolveBin(&json!(TOOLS), &json!("prettier"), &json!("anything"));
        assert_eq!(r, json!(expected(&["node_modules", "prettier", "bin", "prettier.cjs"])));
    }

    #[test]
    fn resolve_bin_no_bin_field_returns_null() {
        let r = resolveBin(&json!(TOOLS), &json!("noop"), &json!("x"));
        assert_eq!(r, Value::Null);
    }

    #[test]
    fn resolve_bin_missing_package_returns_null() {
        let r = resolveBin(&json!(TOOLS), &json!("does-not-exist"), &json!("x"));
        assert_eq!(r, Value::Null);
    }
}
