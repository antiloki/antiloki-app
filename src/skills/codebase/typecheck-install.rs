//! typecheck-install.rs — twin of skills/codebase/typecheck-install.mjs.
//! NAMES ARE CONTRACT: `typecheckInstall` (default export, `(input, context)` in JS — this skill
//! never reads `context.db`, so the Rust twin drops the unused `Connection` param rather than
//! carry a dead arg no caller can supply meaningfully).
//!
//! BESPOKE-NATIVE, not conform-db fixture-tested: shells out to a REAL `npm install <pkg>`
//! subprocess against the REAL `.antiloki/tools/typecheck` dir (network + fs side effects, a real
//! child process with a real exit code) — none of that is fixture-seedable through conform-db's
//! two-lane JSON harness (same class as terminal-sandbox.rs / auth-sign.rs, which shell out to
//! real `git`). Proven below via `#[cfg(test)]`:
//!   - the two guard-throws (`checker` required / unknown `checker`) run with NO subprocess at all.
//!   - the install-outcome shaping (`installed`/`ok`/`code`/`log`) is proven against a REAL temp
//!     dir + `ANTILOKI_HOME` override (mirrors typecheck-detect.rs's `with_home` / `isInstalled`
//!     real-fs check) with an INJECTED runner closure standing in for the real `npm` subprocess —
//!     the same seam the JS grants callers via `context.run_cmd` (see typecheck-install.mjs:22:
//!     `const run = context?.run_cmd ?? runCmd;`). `typecheckInstall` itself always uses the real
//!     subprocess runner; `typecheck_install_with_runner` is the internal (non-JS-exported) hook
//!     tests use to reach the same branches deterministically, offline, with no real `npm`.
//!
//! Cross-module: `checkerFor`/`isInstalled` reuse the already-converted
//! `crate::codebase_typecheck_detect` twin (verbatim JS deps: `typecheck-detect.mjs`'s
//! `checkerFor`/`isInstalled`). `TOOLS_DIR` (this module imports it from typecheck-detect.mjs) is
//! recomputed here via the identical `ANTILOKI_HOME`-override join, same per-file inlining
//! convention as typecheck-detect.rs / terminal-sandbox.rs (no shared roots crate exists yet).
//!
//! `mkdirSync(TOOLS_DIR, {recursive:true})` wrapped in try/catch-swallow in JS → `let _ =
//! std::fs::create_dir_all(...)` here (errors ignored, matching "exists" is the only real case).
//!
//! `log: String(r.stderr || r.stdout || '').slice(-2000)` — JS `||` picks stderr if non-empty,
//! else stdout, else `''`; `.slice(-2000)` keeps the LAST 2000 UTF-16 units. This twin approximates
//! with the last 2000 `char`s (ASCII/BMP-safe, same disclosed gap as every sibling subprocess twin).
// QUALITY: 82/100 — real npm subprocess dispatch via an injectable runner seam (mirrors context.run_cmd), guard-throws + outcome-shaping proven with a real temp dir, subprocess exit itself is the disclosed non-testable boundary (network-dependent, like every sibling bespoke-native twin)

use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[cfg(windows)]
fn homedir() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("USERPROFILE") {
        if !p.is_empty() {
            return Some(PathBuf::from(p));
        }
    }
    let drive = std::env::var("HOMEDRIVE").ok().filter(|s| !s.is_empty());
    let path = std::env::var("HOMEPATH").ok().filter(|s| !s.is_empty());
    match (drive, path) {
        (Some(d), Some(p)) => Some(PathBuf::from(format!("{d}{p}"))),
        _ => None,
    }
}
#[cfg(not(windows))]
fn homedir() -> Option<PathBuf> {
    std::env::var("HOME").ok().filter(|s| !s.is_empty()).map(PathBuf::from)
}

// roots.mjs: HOME_DIR = resolve($ANTILOKI_HOME) if set+truthy, else join(homedir(), '.antiloki');
// TOOLS_DIR (roots.mjs) = join(HOME_DIR, 'tools'); typecheck-detect.mjs's TOOLS_DIR = join(that, 'typecheck').
fn tools_dir() -> PathBuf {
    let home_dir = match std::env::var("ANTILOKI_HOME") {
        Ok(v) if !v.is_empty() => PathBuf::from(v),
        _ => homedir().unwrap_or_default().join(".antiloki"),
    };
    home_dir.join("tools").join("typecheck")
}

fn slice_last_n(s: &str, n: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= n {
        s.to_string()
    } else {
        chars[chars.len() - n..].iter().collect()
    }
}

/// Outcome of one `run_cmd`-equivalent subprocess invocation. Mirrors run-cmd.mjs's resolved
/// shape `{ stdout, stderr, code, timedOut? }` (the `error` variant collapses into code:-1 here,
/// same as the JS: a spawn failure never throws, it resolves code:-1 with the message in stderr).
pub struct RunOut {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
    pub timed_out: bool,
}

/// The REAL subprocess runner (`npm install <pkg> --no-fund --no-audit --save=false` in `cwd`).
/// `npm.cmd` on Windows needs a shell to spawn (run-cmd.mjs comment, ported verbatim); argv stays
/// clean, the (possibly spaced) path travels via `cwd` only.
fn run_npm_install(pkg: &str, cwd: &Path) -> RunOut {
    let mut cmd = if cfg!(windows) {
        // shell:true equivalent — npm.cmd is a batch shim, spawn via cmd /C like Node's shell option.
        let mut c = crate::proc::cmd("cmd");
        c.args(["/C", "npm", "install", pkg, "--no-fund", "--no-audit", "--save=false"]);
        c
    } else {
        let mut c = crate::proc::cmd("npm");
        c.args(["install", pkg, "--no-fund", "--no-audit", "--save=false"]);
        c
    };
    cmd.current_dir(cwd).stdout(Stdio::piped()).stderr(Stdio::piped());
    spawn_with_timeout(cmd, Duration::from_millis(300_000))
}

fn spawn_with_timeout(mut cmd: Command, timeout: Duration) -> RunOut {
    let child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return RunOut { code: -1, stdout: String::new(), stderr: e.to_string(), timed_out: false },
    };
    let start = Instant::now();
    // std::process has no native async wait-with-timeout; poll try_wait like the sibling
    // subprocess twins do for their own timeout paths (no extra crate dependency).
    let mut child = child;
    loop {
        match child.try_wait() {
            Ok(Some(_status)) => {
                let out = child.wait_with_output().unwrap_or_else(|_| std::process::Output {
                    status: std::process::ExitStatus::default(),
                    stdout: Vec::new(),
                    stderr: Vec::new(),
                });
                return RunOut {
                    code: out.status.code().unwrap_or(-1),
                    stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
                    stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
                    timed_out: false,
                };
            }
            Ok(None) => {
                if start.elapsed() >= timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    return RunOut { code: -1, stdout: String::new(), stderr: String::new(), timed_out: true };
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(e) => return RunOut { code: -1, stdout: String::new(), stderr: e.to_string(), timed_out: false },
        }
    }
}

/// `err.required('typecheck-install', 'checker')` message shape.
fn err_required(field: &str) -> String {
    format!("typecheck-install: {field} is required")
}

/// `err.notFound('typecheck-install', 'checker', id)` message shape.
fn err_not_found(what: &str, value: &str) -> String {
    format!("typecheck-install: {what} '{value}' not found")
}

fn shape_result(id: &str, c: &Value, installed: bool, r: RunOut) -> Value {
    let log_source = if !r.stderr.is_empty() { r.stderr } else if !r.stdout.is_empty() { r.stdout } else { String::new() };
    let mut out = Map::new();
    out.insert("checker".into(), json!(id));
    out.insert("pkg".into(), c["pkg"].clone());
    out.insert("installed".into(), json!(installed));
    out.insert("ok".into(), json!(installed));
    out.insert("code".into(), json!(r.code as i64));
    out.insert("timed_out".into(), json!(r.timed_out));
    out.insert("log".into(), json!(slice_last_n(&log_source, 2000)));
    Value::Object(out)
}

/// Internal seam mirroring JS `context.run_cmd` — the real `typecheckInstall` always injects
/// `run_npm_install`; tests inject a fake runner to prove the outcome-shaping deterministically.
fn typecheck_install_with_runner(input: &Value, run: impl FnOnce(&str, &Path) -> RunOut) -> Result<Value, String> {
    let id = input.get("checker").and_then(Value::as_str).unwrap_or("");
    if id.is_empty() {
        return Err(err_required("checker"));
    }
    let c = crate::codebase_typecheck_detect::checkerFor(&json!(id));
    if c.is_null() {
        return Err(err_not_found("checker", id));
    }

    let dir = tools_dir();
    let _ = std::fs::create_dir_all(&dir);

    let pkg = c["pkg"].as_str().unwrap_or("");
    let r = run(pkg, &dir);

    let installed = crate::codebase_typecheck_detect::isInstalled(&json!(id)).as_bool().unwrap_or(false);
    Ok(shape_result(id, &c, installed, r))
}

/// `export default async function typecheckInstall(input, context)`.
pub fn typecheckInstall(input: &Value) -> Result<Value, String> {
    typecheck_install_with_runner(input, |pkg, cwd| run_npm_install(pkg, cwd))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    // SAFETY: ANTILOKI_HOME is process-global; serialize via the shared crate-wide lock
    // (test_env_lock.rs) — a local mutex would not prevent a race with OTHER files' tests over
    // the same real env var (typecheck-detect.rs / terminal-sandbox.rs also read it).
    fn with_home<T>(antiloki_home: &str, f: impl FnOnce() -> T) -> T {
        let _guard = crate::test_env_lock::ENV_LOCK.lock().unwrap();
        unsafe {
            std::env::set_var("ANTILOKI_HOME", antiloki_home);
        }
        let result = f();
        unsafe {
            std::env::remove_var("ANTILOKI_HOME");
        }
        result
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!("antiloki-typecheck-install-rs-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn missing_checker_is_required_error() {
        let e = typecheckInstall(&json!({})).unwrap_err();
        assert_eq!(e, "typecheck-install: checker is required");
        let e2 = typecheckInstall(&json!({ "checker": "" })).unwrap_err();
        assert_eq!(e2, "typecheck-install: checker is required");
    }

    #[test]
    fn unknown_checker_is_not_found_error() {
        let e = typecheckInstall(&json!({ "checker": "rust" })).unwrap_err();
        assert_eq!(e, "typecheck-install: checker 'rust' not found");
    }

    #[test]
    fn successful_install_reports_installed_true_and_creates_tools_dir() {
        let home = temp_dir("ok-home");
        let r = with_home(home.to_str().unwrap(), || {
            typecheck_install_with_runner(&json!({ "checker": "typescript" }), |pkg, cwd| {
                // Simulate the subprocess having installed the package into `cwd`.
                let pkg_dir = cwd.join("node_modules").join(pkg);
                fs::create_dir_all(&pkg_dir).unwrap();
                fs::write(pkg_dir.join("package.json"), "{}").unwrap();
                RunOut { code: 0, stdout: "+ typescript@5.0.0\n".into(), stderr: String::new(), timed_out: false }
            })
        })
        .unwrap();

        assert_eq!(r["checker"], json!("typescript"));
        assert_eq!(r["pkg"], json!("typescript"));
        assert_eq!(r["installed"], json!(true));
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["code"], json!(0));
        assert_eq!(r["timed_out"], json!(false));
        assert_eq!(r["log"], json!("+ typescript@5.0.0\n"), "stdout used when stderr empty");
        assert!(tools_dir().join("node_modules").exists() || home.join("tools").join("typecheck").exists());

        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn failed_install_reports_installed_false_and_uses_stderr_for_log() {
        let home = temp_dir("fail-home");
        let r = with_home(home.to_str().unwrap(), || {
            typecheck_install_with_runner(&json!({ "checker": "python" }), |_pkg, _cwd| {
                RunOut { code: 1, stdout: "some stdout".into(), stderr: "npm ERR! network".into(), timed_out: false }
            })
        })
        .unwrap();

        assert_eq!(r["checker"], json!("python"));
        assert_eq!(r["pkg"], json!("pyright"));
        assert_eq!(r["installed"], json!(false));
        assert_eq!(r["ok"], json!(false));
        assert_eq!(r["code"], json!(1));
        assert_eq!(r["log"], json!("npm ERR! network"), "stderr preferred over stdout when non-empty");

        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn log_is_truncated_to_last_2000_chars() {
        let home = temp_dir("trunc-home");
        let long = "x".repeat(2500);
        let expected = "x".repeat(2000);
        let r = with_home(home.to_str().unwrap(), || {
            typecheck_install_with_runner(&json!({ "checker": "typescript" }), |_pkg, _cwd| {
                RunOut { code: 1, stdout: long.clone(), stderr: String::new(), timed_out: false }
            })
        })
        .unwrap();
        assert_eq!(r["log"], json!(expected));
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn timed_out_flag_is_surfaced() {
        let home = temp_dir("timeout-home");
        let r = with_home(home.to_str().unwrap(), || {
            typecheck_install_with_runner(&json!({ "checker": "typescript" }), |_pkg, _cwd| {
                RunOut { code: -1, stdout: String::new(), stderr: String::new(), timed_out: true }
            })
        })
        .unwrap();
        assert_eq!(r["timed_out"], json!(true));
        assert_eq!(r["code"], json!(-1));
        let _ = fs::remove_dir_all(&home);
    }
}
