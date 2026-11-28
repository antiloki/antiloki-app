//! proc-run.rs — twin of skills/external/proc-run.mjs. NAMES ARE CONTRACT: `isAllowlisted`
//! (named), `procRun` (default).
//!
//! `isAllowlisted` — pure allowlist predicate, no subprocess, no db: a normal no-db helper unit
//! (fixture + frontier entry).
//!
//! `procRun` — BESPOKE-NATIVE (playbook's "not conditionally avoidable" rule, run-cmd.rs /
//! code-git-info.rs precedent): this skill's entire purpose is running a real subprocess — every
//! success path unconditionally `spawnSync`s the allowlisted command, and there is no
//! input flag whose absence keeps the fn deterministic/fixture-safe (unlike
//! constitution-estimate.rs's `input.base`-gated git-diff branch). Per the crossing playbook this
//! unit carries NO conform-db fixture/frontier entry — it is proven correct by the real,
//! non-mocked `#[cfg(test)]` tests below (real child processes: a successful allowlisted run, a
//! permission-denied rejection, a missing-`command` validation error, and a timeout-kill via the
//! internal `spawn_sync` helper called directly — none of the allowlisted commands are slow
//! enough to exercise a real timeout through the public `procRun` entry point itself).
//!
//! JS never returns a structured error object — it `throw`s an `AntilokiError` whose `.message`
//! is `${skill}: ${cause}` (see `err.required`/the local `reject` helper in the JS source); ported
//! as `Result::Err(String)` carrying that same rendered message (not exercised by any fixture, so
//! exact wording fidelity is a best-effort courtesy, not a proof obligation).
//!
//! DISCLOSED GAP — `cwd`: JS pins the child's cwd to `PROJECT_ROOT`, a fixed path computed once
//! from `import.meta.url` (three parents above `skills/external/proc-run.mjs`, i.e. the
//! antiloki-core repo root) — independent of the caller's own process cwd. This twin has no
//! equivalent fixed anchor inside the antiloki-core-rust repo, so it runs the child in the
//! CALLING process's current directory (`std::env::current_dir()`) instead. Harmless for this
//! fn's bespoke-native status (no fixture ever compares the two), but a real divergence if this
//! twin is ever wired into a production dispatcher expecting antiloki-core's cwd semantics.
//!
//! `windowsHide: true` (Node hides the console window spawned for a GUI subsystem binary on
//! Windows) has no direct `std::process::Command` equivalent short of `CREATE_NO_WINDOW` via
//! `CommandExt::creation_flags` — omitted (none of the allowlisted commands are GUI binaries, and
//! this fn carries no fixture coverage to enforce it).
// QUALITY: isAllowlisted is a normal no-db helper (fixture-backed); procRun is bespoke-native
// (real subprocess on every success path, run-cmd.rs precedent) — no fixture/frontier entry,
// correctness proven by the native tests below.

use serde_json::{json, Value};
use std::io::Read;
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::thread;
use std::time::{Duration, Instant};

// ── standard helpers (playbook, copied verbatim) ────────────────────────────
fn truthy(v: Option<&Value>) -> bool { match v { None|Some(Value::Null)=>false, Some(Value::Bool(b))=>*b, Some(Value::Number(n))=>n.as_f64().map_or(false,|f|f!=0.0&&!f.is_nan()), Some(Value::String(s))=>!s.is_empty(), Some(_)=>true } }
fn str_qq(v: Option<&Value>) -> String { match v { None|Some(Value::Null)=>String::new(), Some(Value::String(s))=>s.clone(), Some(Value::Bool(b))=>b.to_string(), Some(n@Value::Number(_))=>n.to_string(), Some(o)=>o.to_string() } } // = JS String(v ?? '')
fn tmpl(v: Option<&Value>) -> String { match v { None=>"undefined".into(), Some(Value::Null)=>"null".into(), Some(x)=>str_qq(Some(x)) } } // = JS `${v}`
fn js_number(v: Option<&Value>) -> f64 { match v { None=>f64::NAN, Some(Value::Null)=>0.0, Some(Value::Bool(b))=>if *b {1.0} else {0.0}, Some(Value::Number(n))=>n.as_f64().unwrap_or(f64::NAN), Some(Value::String(s))=>{let t=s.trim(); if t.is_empty(){0.0}else{t.parse().unwrap_or(f64::NAN)}}, Some(_)=>f64::NAN } } // = JS Number(v)

const MAX_OUTPUT_BYTES: usize = 64 * 1024;
const DEFAULT_TIMEOUT_MS: f64 = 30_000.0;
const MAX_TIMEOUT_MS: f64 = 300_000.0;

// ── isAllowlisted regexes ────────────────────────────────────────────────
fn control_char_re() -> &'static regex::Regex {
    static R: OnceLock<regex::Regex> = OnceLock::new();
    R.get_or_init(|| regex::Regex::new(r"[\n\r\x00`]").unwrap())
}
fn neg_num_re() -> &'static regex::Regex {
    static R: OnceLock<regex::Regex> = OnceLock::new();
    R.get_or_init(|| regex::Regex::new(r"^-\d{1,3}$").unwrap())
}
fn rev_like_re() -> &'static regex::Regex {
    static R: OnceLock<regex::Regex> = OnceLock::new();
    R.get_or_init(|| regex::Regex::new(r"^[A-Za-z0-9_./][A-Za-z0-9_./-]{0,79}$").unwrap())
}
fn ls_files_glob_re() -> &'static regex::Regex {
    static R: OnceLock<regex::Regex> = OnceLock::new();
    R.get_or_init(|| regex::Regex::new(r"^[A-Za-z0-9_./*?-]{1,200}$").unwrap())
}
fn config_key_re() -> &'static regex::Regex {
    static R: OnceLock<regex::Regex> = OnceLock::new();
    R.get_or_init(|| regex::Regex::new(r"^[A-Za-z0-9_.-]{1,80}$").unwrap())
}

/// `git`'s per-subcommand allowlist. `args` is the FULL args array (sub at index 0), matching the
/// JS `args` array scope inside the `if (command === 'git')` branch.
fn git_allowlisted(args: &[&str]) -> bool {
    if args.is_empty() { return false; }
    let sub = args[0];
    match sub {
        "status" => args[1..].iter().all(|a| *a == "--short" || *a == "-s" || *a == "--porcelain"),
        "log" => {
            for (i, a) in args.iter().enumerate().skip(1) {
                if *a == "--oneline" { continue; }
                if neg_num_re().is_match(a) && a[1..].parse::<i64>().map_or(false, |n| n <= 200) { continue; }
                if i == args.len() - 1 && rev_like_re().is_match(a) { continue; }
                return false;
            }
            true
        }
        "diff" | "show" => {
            for (i, a) in args.iter().enumerate().skip(1) {
                if *a == "--stat" || *a == "--name-only" || *a == "--cached" { continue; }
                if i == args.len() - 1 && rev_like_re().is_match(a) { continue; }
                return false;
            }
            true
        }
        "branch" => args[1..].iter().all(|a| *a == "-a" || *a == "-r" || *a == "--show-current"),
        "rev-parse" => {
            const OK: [&str; 4] = ["HEAD", "--abbrev-ref", "--short", "--short=8"];
            args[1..].iter().all(|a| OK.contains(a) || rev_like_re().is_match(a))
        }
        "ls-files" => {
            if args.len() <= 1 { return true; }
            if args.len() == 2 { return ls_files_glob_re().is_match(args[1]); }
            false
        }
        "config" => args.len() == 3 && args[1] == "--get" && config_key_re().is_match(args[2]),
        _ => false,
    }
}

/// `export function isAllowlisted(command, args)`
pub fn isAllowlisted(command: &Value, args: &Value) -> Value {
    let cmd = match command.as_str().filter(|s| !s.is_empty()) { Some(s) => s, None => return json!(false) };
    let arr = match args.as_array() { Some(a) => a, None => return json!(false) };
    if arr.len() > 12 { return json!(false); }
    let mut str_args: Vec<&str> = Vec::with_capacity(arr.len());
    for a in arr {
        let s = match a.as_str() { Some(s) => s, None => return json!(false) };
        if s.chars().count() > 256 { return json!(false); }
        if control_char_re().is_match(s) { return json!(false); }
        str_args.push(s);
    }

    let allowed = match cmd {
        "node" => str_args.len() == 1 && (str_args[0] == "--version" || str_args[0] == "-v"),
        "npm" => {
            (str_args.len() == 1 && (str_args[0] == "--version" || str_args[0] == "-v"))
                || (str_args.len() == 2 && str_args[0] == "list" && str_args[1] == "--depth=0")
        }
        "git" => git_allowlisted(&str_args),
        _ => false,
    };
    json!(allowed)
}

fn truncate(buf: &[u8]) -> (String, bool) {
    if buf.len() <= MAX_OUTPUT_BYTES {
        (String::from_utf8_lossy(buf).into_owned(), false)
    } else {
        let head = String::from_utf8_lossy(&buf[..MAX_OUTPUT_BYTES]).into_owned();
        (format!("{head}\n... [truncated {} bytes]", buf.len() - MAX_OUTPUT_BYTES), true)
    }
}

struct SpawnOutcome {
    status: Option<i32>,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    timed_out: bool,
    spawn_err: Option<std::io::Error>,
}

/// Real spawn + timeout-kill + stdout/stderr capture — mirrors JS
/// `spawnSync(command, args, { cwd, timeout, encoding:'buffer', shell:false, windowsHide:true })`.
/// See module doc for the disclosed `cwd`/`windowsHide` gaps.
fn spawn_sync(command: &str, args: &[String], cwd: &str, timeout_ms: u64) -> SpawnOutcome {
    let mut cmd = crate::proc::cmd(command);
    cmd.args(args).current_dir(cwd).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            return SpawnOutcome { status: None, stdout: Vec::new(), stderr: Vec::new(), timed_out: false, spawn_err: Some(e) };
        }
    };

    let mut stdout_pipe = child.stdout.take();
    let mut stderr_pipe = child.stderr.take();
    let stdout_handle = thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(p) = stdout_pipe.as_mut() { let _ = p.read_to_end(&mut buf); }
        buf
    });
    let stderr_handle = thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(p) = stderr_pipe.as_mut() { let _ = p.read_to_end(&mut buf); }
        buf
    });

    let start = Instant::now();
    let mut timed_out = false;
    let status: Option<i32>;
    loop {
        match child.try_wait() {
            Ok(Some(s)) => { status = s.code(); break; }
            Ok(None) => {
                if start.elapsed() >= Duration::from_millis(timeout_ms) {
                    let _ = child.kill();
                    let _ = child.wait();
                    timed_out = true;
                    status = None;
                    break;
                }
                thread::sleep(Duration::from_millis(10));
            }
            Err(_) => { status = None; break; }
        }
    }

    let stdout = stdout_handle.join().unwrap_or_default();
    let stderr = stderr_handle.join().unwrap_or_default();
    SpawnOutcome { status, stdout, stderr, timed_out, spawn_err: None }
}

/// `export default function procRun(input, _context)` — see module doc: bespoke-native, no
/// fixture/frontier entry. `_context` is unused in the JS source, so this twin drops the param.
pub fn procRun(input: &Value) -> Result<Value, String> {
    let obj = input.as_object();
    let command_v = obj.and_then(|o| o.get("command")).cloned().unwrap_or(Value::Null);
    if !truthy(Some(&command_v)) {
        return Err("proc-run: command is required".to_string());
    }
    // JS `const { args = [] } = input ?? {}`: the `[]` default fires ONLY when the key is absent
    // (undefined), NOT when it's explicit null.
    let args_v = match obj.and_then(|o| o.get("args")) {
        Some(v) => v.clone(),
        None => json!([]),
    };

    if !matches!(isAllowlisted(&command_v, &args_v), Value::Bool(true)) {
        let args_display = serde_json::to_string(&args_v).unwrap_or_else(|_| "null".into());
        return Err(format!("proc-run: not allowlisted: command={} args={args_display}", tmpl(Some(&command_v))));
    }
    // isAllowlisted only returns true for a string `command` and an array of string `args` —
    // both unwraps below are safe past this gate.
    let command = command_v.as_str().unwrap();
    let args_strs: Vec<String> = args_v.as_array().unwrap().iter().map(|a| a.as_str().unwrap().to_string()).collect();

    let timeout_raw = obj.and_then(|o| o.get("timeout_ms"));
    let n = js_number(timeout_raw);
    let base = if n.is_nan() || n == 0.0 { DEFAULT_TIMEOUT_MS } else { n };
    let t = base.max(1000.0).min(MAX_TIMEOUT_MS) as u64;

    let cwd = std::env::current_dir().map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|_| ".".to_string());

    let started = Instant::now();
    let outcome = spawn_sync(command, &args_strs, &cwd, t);
    let duration_ms = started.elapsed().as_millis() as i64;

    if let Some(e) = &outcome.spawn_err {
        if e.kind() == std::io::ErrorKind::NotFound {
            return Err(format!("proc-run: executable '{command}' not found on host PATH"));
        }
        return Err(format!("proc-run: spawn failed: {e}"));
    }

    let (stdout_text, stdout_trunc) = truncate(&outcome.stdout);
    let (stderr_text, stderr_trunc) = truncate(&outcome.stderr);

    Ok(json!({
        "command": command,
        "args": args_v,
        "exit_code": outcome.status,
        "stdout": stdout_text,
        "stderr": stderr_text,
        "duration_ms": duration_ms,
        "timed_out": outcome.timed_out,
        "truncated": stdout_trunc || stderr_trunc,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── isAllowlisted ────────────────────────────────────────────────
    #[test]
    fn allowlist_node_version_forms() {
        assert_eq!(isAllowlisted(&json!("node"), &json!(["--version"])), json!(true));
        assert_eq!(isAllowlisted(&json!("node"), &json!(["-v"])), json!(true));
        assert_eq!(isAllowlisted(&json!("node"), &json!(["script.mjs"])), json!(false));
        assert_eq!(isAllowlisted(&json!("node"), &json!(["-e", "console.log(1)"])), json!(false));
    }

    #[test]
    fn allowlist_npm_forms() {
        assert_eq!(isAllowlisted(&json!("npm"), &json!(["--version"])), json!(true));
        assert_eq!(isAllowlisted(&json!("npm"), &json!(["list", "--depth=0"])), json!(true));
        assert_eq!(isAllowlisted(&json!("npm"), &json!(["install"])), json!(false));
        assert_eq!(isAllowlisted(&json!("npm"), &json!(["run", "whatever"])), json!(false));
    }

    #[test]
    fn allowlist_git_status_and_log() {
        assert_eq!(isAllowlisted(&json!("git"), &json!(["status"])), json!(true));
        assert_eq!(isAllowlisted(&json!("git"), &json!(["status", "--short"])), json!(true));
        assert_eq!(isAllowlisted(&json!("git"), &json!(["log", "--oneline"])), json!(true));
        assert_eq!(isAllowlisted(&json!("git"), &json!(["log", "-10", "--oneline"])), json!(true));
        assert_eq!(isAllowlisted(&json!("git"), &json!(["log", "-9999"])), json!(false));
    }

    #[test]
    fn allowlist_git_diff_show_and_mutations() {
        assert_eq!(isAllowlisted(&json!("git"), &json!(["diff", "--stat"])), json!(true));
        assert_eq!(isAllowlisted(&json!("git"), &json!(["show", "HEAD"])), json!(true));
        assert_eq!(isAllowlisted(&json!("git"), &json!(["commit", "-m", "x"])), json!(false));
        assert_eq!(isAllowlisted(&json!("git"), &json!(["push"])), json!(false));
        assert_eq!(isAllowlisted(&json!("git"), &json!(["checkout", "main"])), json!(false));
    }

    #[test]
    fn allowlist_git_config_get_only() {
        assert_eq!(isAllowlisted(&json!("git"), &json!(["config", "--get", "user.email"])), json!(true));
        assert_eq!(isAllowlisted(&json!("git"), &json!(["config", "--set", "k", "v"])), json!(false));
    }

    #[test]
    fn allowlist_unknown_and_control_chars() {
        assert_eq!(isAllowlisted(&json!("rm"), &json!(["-rf", "/"])), json!(false));
        assert_eq!(isAllowlisted(&json!("git"), &json!(["status\n"])), json!(false));
        assert_eq!(isAllowlisted(&json!("git"), &json!(["status`whoami`"])), json!(false));
    }

    #[test]
    fn allowlist_oversize_argv_and_long_arg_rejected() {
        let huge: Vec<Value> = (0..20).map(|_| json!("a")).collect();
        assert_eq!(isAllowlisted(&json!("git"), &Value::Array(huge)), json!(false));
        assert_eq!(isAllowlisted(&json!("git"), &json!(["status", "a".repeat(257)])), json!(false));
    }

    // ── procRun ──────────────────────────────────────────────────────
    #[test]
    fn proc_run_node_version_returns_expected_shape() {
        let r = procRun(&json!({ "command": "node", "args": ["--version"] })).unwrap();
        assert_eq!(r["command"], json!("node"));
        assert_eq!(r["exit_code"], json!(0));
        assert!(r["stdout"].as_str().unwrap().starts_with('v'));
        assert_eq!(r["timed_out"], json!(false));
        assert_eq!(r["truncated"], json!(false));
        assert!(r["duration_ms"].as_i64().unwrap() >= 0);
    }

    #[test]
    fn proc_run_rejected_command_is_an_err() {
        let e = procRun(&json!({ "command": "rm", "args": ["-rf", "/"] })).unwrap_err();
        assert!(e.contains("not allowlisted"), "{e}");
    }

    #[test]
    fn proc_run_missing_command_is_an_err() {
        let e = procRun(&json!({})).unwrap_err();
        assert_eq!(e, "proc-run: command is required");
    }

    #[test]
    fn proc_run_git_status_in_this_repo_exits_zero() {
        let r = procRun(&json!({ "command": "git", "args": ["status", "--short"] })).unwrap();
        assert_eq!(r["exit_code"], json!(0));
    }

    #[test]
    fn spawn_sync_timeout_kills_the_child_and_reports_timed_out() {
        // Exercises the internal timeout-kill path directly (none of the allowlisted commands run
        // long enough to hit a timeout through the public procRun entry point).
        let cwd = std::env::current_dir().unwrap().to_string_lossy().into_owned();
        let (prog, args): (&str, Vec<String>) = if cfg!(windows) {
            ("cmd", ["/C", "ping", "-n", "10", "127.0.0.1"].iter().map(|s| s.to_string()).collect())
        } else {
            ("sleep", vec!["10".to_string()])
        };
        let outcome = spawn_sync(prog, &args, &cwd, 200);
        assert!(outcome.timed_out);
        assert_eq!(outcome.status, None);
    }

    #[test]
    fn spawn_sync_missing_binary_reports_spawn_err() {
        let cwd = std::env::current_dir().unwrap().to_string_lossy().into_owned();
        let outcome = spawn_sync("this-binary-does-not-exist-xyz-antiloki", &[], &cwd, 5000);
        assert!(outcome.spawn_err.is_some());
        assert_eq!(outcome.spawn_err.unwrap().kind(), std::io::ErrorKind::NotFound);
    }
}
