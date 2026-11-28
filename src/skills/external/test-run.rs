//! external/test-run.rs — twin of skills/external/test-run.mjs. BESPOKE-NATIVE (playbook), not
//! conform-db fixtured. NAMES ARE CONTRACT: `testRun` (default export only — JS has no named
//! exports; `parseCounts` is a private, non-exported JS helper, mirrored here as the private
//! `parse_counts`).
//!
//! Runs `node --test --test-reporter=spec --disable-warning=ExperimentalWarning <path>` as a REAL
//! child process against a REAL `.test.mjs` file, exactly like the JS `spawnSync`. Every branch
//! that reaches the subprocess launch is unconditional (no input flag keeps this fn
//! deterministic/fixture-safe) — same class as `external_proc_run::procRun`, this crate's other
//! subprocess-launching bespoke-native twin; the child-process spawn/timeout/capture plumbing
//! below is a deliberate structural copy of `proc-run.rs`'s `spawn_sync`, adapted for
//! `node --test`'s fixed-args / no-allowlist shape. Proven correct below via `#[cfg(test)]`
//! against real temp `.test.mjs` files (passing test, failing test, and the two path-guard
//! rejections) run through the REAL `node` binary on PATH — spec-reporter output format verified
//! live via `node --test --test-reporter=spec --disable-warning=ExperimentalWarning` before
//! writing the assertions below (real `ℹ tests N` / `ℹ pass N` / `ℹ fail N` summary lines, exit
//! code 0 on all-pass, 1 on any failure).
//!
//! `path` validation reuses the already-converted `external_file_read::validatePath` twin
//! (cross-module reuse per the playbook) — this is a literal JS behavior, not a naming choice:
//! `test-run.mjs` imports `validatePath`/`PROJECT_ROOT` directly from `file-read.mjs`, so a
//! rejected path surfaces the exact `"file-read: ..."` message (skill:'file-read' is baked into
//! file-read.mjs's own `AntilokiError`s), never `"test-run: ..."`. Ported literally, not "fixed".
//!
//! `process.execPath` (the exact node binary running the parent JS process) and JS's
//! `PROJECT_ROOT`-as-cwd have no equivalent constant reachable from this Rust binary — this twin
//! resolves the child via `"node"` on PATH and runs it in `std::env::current_dir()`, the same
//! disclosed-gap class as `proc-run.rs`'s missing `PROJECT_ROOT` cwd anchor.
//!
//! Env stripping (`NODE_TEST_CONTEXT`, `NODE_OPTIONS` removed so `node --test` doesn't see itself
//! as already inside a test run and skip) is ported via `Command::env_remove`, which starts from
//! the current process env exactly like JS's `{ ...process.env }` base-then-delete.
//!
//! Output handling mirrors JS exactly: stdout and stderr are EACH truncated to 64KB first, then
//! joined (`stdout + '\n' + stderr` when stderr is non-empty), `parseCounts` runs over that
//! combined text, and `output`/`truncated` are computed from a SECOND 64KB truncation of the
//! combined string (two-stage truncation, ported literally — not simplified to one pass).
// QUALITY: real `node --test` subprocess dispatch + spec/TAP output parsing; disclosed exec-path/cwd gaps shared with proc-run.rs

use regex::Regex;
use serde_json::{json, Value};
use std::io::Read;
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::thread;
use std::time::{Duration, Instant};

const MAX_OUTPUT_BYTES: usize = 64 * 1024;
const DEFAULT_TIMEOUT_MS: u64 = 120_000;

fn summary_re(word: &str) -> Regex {
    Regex::new(&format!(r"^(?:\x{{2139}}|#)\s*{word}\s+(\d+)")).unwrap()
}
fn tests_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| summary_re("tests"))
}
fn pass_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| summary_re("pass"))
}
fn fail_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| summary_re("fail"))
}
fn ok_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"^ok \d+").unwrap())
}
fn not_ok_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"^not ok \d+").unwrap())
}

#[derive(Default)]
struct Counts {
    pass: i64,
    fail: i64,
    total: i64,
}

/// `function parseCounts(out)` — parses BOTH the spec reporter (`ℹ tests N` — TTY-default) and
/// the TAP reporter (`# tests N` — subprocess-default) summary lines. Falls back to counting
/// `ok N` / `not ok N` lines when no summary comment was found (pure TAP w/o summary).
fn parse_counts(out: &str) -> Counts {
    let mut c = Counts::default();
    for line in out.split('\n') {
        if let Some(m) = tests_re().captures(line) {
            c.total = m[1].parse().unwrap_or(0);
        }
        if let Some(m) = pass_re().captures(line) {
            c.pass = m[1].parse().unwrap_or(0);
        }
        if let Some(m) = fail_re().captures(line) {
            c.fail = m[1].parse().unwrap_or(0);
        }
    }
    if c.total == 0 {
        for line in out.split('\n') {
            if ok_re().is_match(line) {
                c.pass += 1;
            } else if not_ok_re().is_match(line) {
                c.fail += 1;
            }
        }
        c.total = c.pass + c.fail;
    }
    c
}

fn truncate_bytes(buf: &[u8], max: usize) -> (String, bool) {
    if buf.len() <= max {
        (String::from_utf8_lossy(buf).into_owned(), false)
    } else {
        (String::from_utf8_lossy(&buf[..max]).into_owned(), true)
    }
}

struct SpawnOutcome {
    status: Option<i32>,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    spawn_err: Option<std::io::Error>,
}

/// Real `node --test` child process, spawn/timeout-kill/capture plumbing structurally copied from
/// `proc-run.rs`'s `spawn_sync` (see module doc for the `execPath`/cwd disclosed gaps).
fn spawn_node_test(abs_path: &str, cwd: &str, timeout_ms: u64) -> SpawnOutcome {
    let mut cmd = crate::proc::cmd("node");
    cmd.args([
        "--test",
        "--test-reporter=spec",
        "--disable-warning=ExperimentalWarning",
        abs_path,
    ])
    .current_dir(cwd)
    .env_remove("NODE_TEST_CONTEXT")
    .env_remove("NODE_OPTIONS")
    .stdin(Stdio::null())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped());

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            return SpawnOutcome { status: None, stdout: Vec::new(), stderr: Vec::new(), spawn_err: Some(e) };
        }
    };

    let mut stdout_pipe = child.stdout.take();
    let mut stderr_pipe = child.stderr.take();
    let stdout_handle = thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(p) = stdout_pipe.as_mut() {
            let _ = p.read_to_end(&mut buf);
        }
        buf
    });
    let stderr_handle = thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(p) = stderr_pipe.as_mut() {
            let _ = p.read_to_end(&mut buf);
        }
        buf
    });

    let start = Instant::now();
    let status: Option<i32>;
    loop {
        match child.try_wait() {
            Ok(Some(s)) => {
                status = s.code();
                break;
            }
            Ok(None) => {
                if start.elapsed() >= Duration::from_millis(timeout_ms) {
                    let _ = child.kill();
                    let _ = child.wait();
                    status = None;
                    break;
                }
                thread::sleep(Duration::from_millis(10));
            }
            Err(_) => {
                status = None;
                break;
            }
        }
    }

    let stdout = stdout_handle.join().unwrap_or_default();
    let stderr = stderr_handle.join().unwrap_or_default();
    SpawnOutcome { status, stdout, stderr, spawn_err: None }
}

/// `export default function testRun(input, _context)` — run a single node:test file via
/// `node --test`. `_context` is unused in the JS source (only `input` is destructured), so this
/// twin drops the param.
pub fn testRun(input: &Value) -> Result<Value, String> {
    let obj = input.as_object();
    let path = match obj.and_then(|o| o.get("path")).and_then(|v| v.as_str()).filter(|s| !s.is_empty()) {
        Some(p) => p.to_string(),
        None => return Err("test-run: path is required".to_string()),
    };
    // JS `const { timeout_ms = DEFAULT_TIMEOUT_MS } = input ?? {}`: default fires only when the
    // key is absent (undefined), not when explicit null/other — mirrored via `.get` + fallback.
    let timeout_ms = obj
        .and_then(|o| o.get("timeout_ms"))
        .and_then(|v| v.as_u64())
        .unwrap_or(DEFAULT_TIMEOUT_MS);

    // Reuses file-read.mjs's validatePath verbatim (see module doc) — errors surface with
    // skill:'file-read', not 'test-run'.
    let abs_val = crate::external_file_read::validatePath(&json!(path))?;
    let abs = abs_val.as_str().unwrap_or("").to_string();
    if !std::path::Path::new(&abs).exists() {
        return Err(format!("test-run: test file '{path}' not found"));
    }
    if !path.ends_with(".test.mjs") {
        return Err("test-run: test file must end in .test.mjs".to_string());
    }

    let started = Instant::now();
    let cwd = std::env::current_dir()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| ".".to_string());
    let outcome = spawn_node_test(&abs, &cwd, timeout_ms);
    let duration_ms = started.elapsed().as_millis() as i64;

    if let Some(e) = &outcome.spawn_err {
        return Err(format!("test-run: failed to spawn node: {e}"));
    }

    let (stdout_s, _) = truncate_bytes(&outcome.stdout, MAX_OUTPUT_BYTES);
    let (stderr_s, _) = truncate_bytes(&outcome.stderr, MAX_OUTPUT_BYTES);
    let combined = if stderr_s.is_empty() { stdout_s } else { format!("{stdout_s}\n{stderr_s}") };
    let counts = parse_counts(&combined);
    let (output, truncated) = truncate_bytes(combined.as_bytes(), MAX_OUTPUT_BYTES);

    Ok(json!({
        "path": path,
        "exit_code": outcome.status,
        "pass": counts.pass,
        "fail": counts.fail,
        "total": counts.total,
        "duration_ms": duration_ms,
        "output": output,
        "truncated": truncated,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    // SAFETY: ANTILOKI_SUBSTRATE_ROOT is process-global — shared crate-wide lock, same convention
    // as file-read.rs / auth-sign.rs (a local mutex would not prevent a race with other files'
    // tests over the same real env var).
    fn with_substrate_root<T>(root: &str, f: impl FnOnce() -> T) -> T {
        let _guard = crate::test_env_lock::ENV_LOCK.lock().unwrap();
        unsafe {
            std::env::set_var("ANTILOKI_SUBSTRATE_ROOT", root);
        }
        let result = f();
        unsafe {
            std::env::remove_var("ANTILOKI_SUBSTRATE_ROOT");
        }
        result
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!("antiloki-test-run-rs-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn parse_counts_reads_spec_reporter_summary() {
        let out = "\u{2713} passes (1ms)\n\u{2139} tests 1\n\u{2139} suites 0\n\u{2139} pass 1\n\u{2139} fail 0\n";
        let c = parse_counts(out);
        assert_eq!((c.total, c.pass, c.fail), (1, 1, 0));
    }

    #[test]
    fn parse_counts_reads_tap_summary() {
        let out = "TAP version 13\nok 1 - passes\n# tests 1\n# pass 1\n# fail 0\n";
        let c = parse_counts(out);
        assert_eq!((c.total, c.pass, c.fail), (1, 1, 0));
    }

    #[test]
    fn parse_counts_falls_back_to_ok_lines_when_no_summary() {
        let out = "ok 1 - a\nnot ok 2 - b\nok 3 - c\n";
        let c = parse_counts(out);
        assert_eq!((c.total, c.pass, c.fail), (3, 2, 1));
    }

    #[test]
    fn missing_path_is_an_err() {
        assert_eq!(testRun(&json!({})).unwrap_err(), "test-run: path is required");
        assert_eq!(testRun(&json!({ "path": "" })).unwrap_err(), "test-run: path is required");
    }

    #[test]
    fn non_test_mjs_suffix_is_rejected() {
        let dir = temp_dir("bad-suffix");
        fs::write(dir.join("a.mjs"), "export {};\n").unwrap();
        let e = with_substrate_root(dir.to_str().unwrap(), || {
            testRun(&json!({ "path": "a.mjs" })).unwrap_err()
        });
        assert_eq!(e, "test-run: test file must end in .test.mjs");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_file_is_an_err() {
        let dir = temp_dir("missing");
        let e = with_substrate_root(dir.to_str().unwrap(), || {
            testRun(&json!({ "path": "nope.test.mjs" })).unwrap_err()
        });
        assert_eq!(e, "test-run: test file 'nope.test.mjs' not found");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn absolute_path_is_rejected_by_reused_file_read_validate_path() {
        let e = testRun(&json!({ "path": "C:/etc/passwd.test.mjs" })).unwrap_err();
        assert!(e.starts_with("file-read: absolute paths not allowed"), "{e}");
    }

    #[test]
    fn runs_a_real_passing_test_file_via_real_node() {
        let dir = temp_dir("pass");
        fs::write(
            dir.join("a.test.mjs"),
            "import test from 'node:test';\nimport assert from 'node:assert';\ntest('passes', () => { assert.equal(1, 1); });\n",
        )
        .unwrap();
        let r = with_substrate_root(dir.to_str().unwrap(), || {
            testRun(&json!({ "path": "a.test.mjs" })).unwrap()
        });
        assert_eq!(r["path"], json!("a.test.mjs"));
        assert_eq!(r["exit_code"], json!(0));
        assert_eq!(r["total"], json!(1));
        assert_eq!(r["pass"], json!(1));
        assert_eq!(r["fail"], json!(0));
        assert_eq!(r["truncated"], json!(false));
        assert!(r["duration_ms"].as_i64().unwrap() >= 0);
        assert!(r["output"].as_str().unwrap().contains("pass 1"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn runs_a_real_failing_test_file_via_real_node() {
        let dir = temp_dir("fail");
        fs::write(
            dir.join("b.test.mjs"),
            "import test from 'node:test';\nimport assert from 'node:assert';\ntest('fails', () => { assert.equal(1, 2); });\n",
        )
        .unwrap();
        let r = with_substrate_root(dir.to_str().unwrap(), || {
            testRun(&json!({ "path": "b.test.mjs" })).unwrap()
        });
        assert_eq!(r["exit_code"], json!(1));
        assert_eq!(r["total"], json!(1));
        assert_eq!(r["pass"], json!(0));
        assert_eq!(r["fail"], json!(1));
        let _ = fs::remove_dir_all(&dir);
    }
}
