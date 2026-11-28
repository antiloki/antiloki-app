//! codebase/lint-install.rs — twin of skills/codebase/lint-install.mjs (default export, db-first
//! sig `async function lintInstall(input, context)`). NAMES ARE CONTRACT: `lintInstall`.
//!
//! BESPOKE-NATIVE (no conform-db fixture/frontier entry), same precedent as `lint-eslint.rs`
//! and `_helpers/run-cmd.rs`'s `runCmd`: the real work is a REAL `npm install <pkg>` subprocess
//! against `.antiloki/tools/lint` on the REAL filesystem — network + package-manager side
//! effects the conform-db two-lane harness cannot seed/replay. `context.run_cmd` DI has no Rust
//! analogue (same disclosed scope as `lint-eslint.rs`/`lint-detect.rs`'s `isInstalled`) — this
//! twin always uses the real `runCmd`.
//!
//! Cross-module calls (pre-existing Rust twins): `crate::codebase_lint_detect::linterFor` /
//! `isInstalled` / `npmInstalled`, `crate::codebase__helpers_run_cmd::runCmd`.
//! `TOOLS_DIR` (lint-detect.mjs's plain `export const`, not a function) is duplicated here as the
//! private `tools_dir()` fn, same convention as `lint-eslint.rs`'s own duplicate of the same
//! helper (that fn isn't `pub` on `lint-detect.rs`, so it can't be imported cross-module).
//!
//! Guard/throw fidelity (`err.required`/`err.notFound`, `skills/engine/error.mjs`):
//! - missing `input.linter` (and `input.checker` fallback, JS `??` nullish-then-falsy check) →
//!   `"lint-install: linter is required"`.
//! - unknown linter id → `"lint-install: linter '<id>' not found"` (JS template literal —
//!   `${id}` coerces via `String()`, mirrored by `js_string`).
//!
//! `String(r.stderr || r.stdout || '').slice(-2000)`: JS `||` picks the first TRUTHY operand
//! (empty string is falsy) — mirrored by `first_nonempty`; `.slice(-2000)` keeps the last 2000
//! UTF-16 units, approximated here as the last 2000 `char`s (disclosed, matches this crate's
//! existing slice-approximation convention elsewhere).
// QUALITY: 78/100 — real subprocess + real fs mkdir (bespoke-native, no fixture); slice(-2000) is
// a char- not UTF-16-unit precise port (disclosed, matches existing convention).

use rusqlite::Connection;
use serde_json::{json, Map, Value};

fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) } // JS `??` right operand

fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Value::String(s) => !s.is_empty(),
        _ => true,
    }
}

fn js_string(v: &Value) -> String {
    match v {
        Value::Null => "null".into(),
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Array(a) => a.iter().map(js_string).collect::<Vec<_>>().join(","),
        Value::Object(_) => "[object Object]".into(),
    }
}

#[cfg(windows)]
fn homedir() -> Option<std::path::PathBuf> {
    if let Ok(p) = std::env::var("USERPROFILE") {
        if !p.is_empty() { return Some(std::path::PathBuf::from(p)); }
    }
    let drive = std::env::var("HOMEDRIVE").ok().filter(|s| !s.is_empty());
    let path = std::env::var("HOMEPATH").ok().filter(|s| !s.is_empty());
    match (drive, path) {
        (Some(d), Some(p)) => Some(std::path::PathBuf::from(format!("{d}{p}"))),
        _ => None,
    }
}

#[cfg(not(windows))]
fn homedir() -> Option<std::path::PathBuf> {
    std::env::var("HOME").ok().filter(|s| !s.is_empty()).map(std::path::PathBuf::from)
}

// lint-detect.mjs TOOLS_DIR — duplicated private helper (see module doc).
fn tools_dir() -> std::path::PathBuf {
    let home_dir = match std::env::var("ANTILOKI_HOME") {
        Ok(v) if !v.is_empty() => std::path::PathBuf::from(v),
        _ => homedir().unwrap_or_default().join(".antiloki"),
    };
    home_dir.join("tools").join("lint")
}

// = JS `String(r.stderr || r.stdout || '').slice(-2000)` — see module doc.
fn last_2000_chars(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let len = chars.len();
    let start = if len > 2000 { len - 2000 } else { 0 };
    chars[start..].iter().collect()
}

/// `export default async function lintInstall(input, context)` — see module doc.
pub fn lintInstall(input: &Value, _db: &Connection) -> Result<Value, String> {
    let id_v = nz(input.as_object().and_then(|o| o.get("linter")))
        .or_else(|| nz(input.as_object().and_then(|o| o.get("checker"))))
        .cloned()
        .unwrap_or(Value::Null);
    if !truthy(&id_v) {
        return Err("lint-install: linter is required".to_string());
    }

    let l = crate::codebase_lint_detect::linterFor(&id_v);
    if l.is_null() {
        return Err(format!("lint-install: linter '{}' not found", js_string(&id_v)));
    }

    let via = l.get("via").and_then(Value::as_str).unwrap_or_default().to_string();
    if via != "npm" {
        let installed = crate::codebase_lint_detect::isInstalled(&id_v);
        let bin = l.get("bin").and_then(Value::as_str).unwrap_or_default();
        let mut o = Map::new();
        o.insert("linter".into(), id_v.clone());
        o.insert("ok".into(), json!(false));
        o.insert("via".into(), json!(via));
        o.insert("installed".into(), installed);
        o.insert(
            "reason".into(),
            json!(format!(
                "{} ships as a standalone binary — install it on your PATH (e.g. `pip install {}`); antiloki detects it there.",
                js_string(&id_v),
                bin
            )),
        );
        return Ok(Value::Object(o));
    }

    let dir = tools_dir();
    let _ = std::fs::create_dir_all(&dir); // JS: try { mkdirSync(..., {recursive:true}) } catch {}
    let npm_cmd = if cfg!(windows) { "npm.cmd" } else { "npm" };
    let pkg = l.get("pkg").and_then(Value::as_str).unwrap_or_default().to_string();

    let r = crate::codebase__helpers_run_cmd::runCmd(
        &json!(npm_cmd),
        &json!(["install", pkg, "--no-fund", "--no-audit", "--save=false"]),
        &json!({ "cwd": dir.to_string_lossy(), "shell": cfg!(windows), "timeoutMs": 300000 }),
    );

    let installed = crate::codebase_lint_detect::npmInstalled(&id_v);
    let code = r.get("code").cloned().unwrap_or(json!(0));
    let timed_out = truthy(r.get("timedOut").unwrap_or(&Value::Null));
    let stderr = r.get("stderr").and_then(Value::as_str).unwrap_or_default();
    let stdout = r.get("stdout").and_then(Value::as_str).unwrap_or_default();
    let raw_log = if !stderr.is_empty() { stderr } else if !stdout.is_empty() { stdout } else { "" };

    let mut o = Map::new();
    o.insert("linter".into(), id_v);
    o.insert("pkg".into(), json!(pkg));
    o.insert("installed".into(), installed.clone());
    o.insert("ok".into(), installed);
    o.insert("code".into(), code);
    o.insert("timed_out".into(), json!(timed_out));
    o.insert("log".into(), json!(last_2000_chars(raw_log)));
    Ok(Value::Object(o))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn db0() -> Connection {
        Connection::open_in_memory().unwrap()
    }

    #[test]
    fn missing_linter_and_checker_returns_exact_required_message() {
        let e = lintInstall(&json!({}), &db0()).unwrap_err();
        assert_eq!(e, "lint-install: linter is required");
    }

    #[test]
    fn falsy_linter_falls_back_to_checker_then_required() {
        // JS `??` only skips null/undefined; `input.linter = null` falls through to `checker`.
        let e = lintInstall(&json!({ "linter": null }), &db0()).unwrap_err();
        assert_eq!(e, "lint-install: linter is required");
        let e2 = lintInstall(&json!({ "checker": "" }), &db0()).unwrap_err();
        assert_eq!(e2, "lint-install: linter is required");
    }

    #[test]
    fn checker_field_used_when_linter_absent() {
        // 'ruff' hits the PATH-tool branch (no subprocess install needed) — proves the checker
        // fallback resolves the same as a direct `linter` field would.
        let r = lintInstall(&json!({ "checker": "ruff" }), &db0()).unwrap();
        assert_eq!(r["linter"], json!("ruff"));
        assert_eq!(r["via"], json!("path"));
    }

    #[test]
    fn unknown_linter_returns_exact_not_found_message() {
        let e = lintInstall(&json!({ "linter": "nope" }), &db0()).unwrap_err();
        assert_eq!(e, "lint-install: linter 'nope' not found");
    }

    #[test]
    fn path_via_linter_returns_ok_false_with_reason_and_no_subprocess() {
        let r = lintInstall(&json!({ "linter": "ruff" }), &db0()).unwrap();
        assert_eq!(r["linter"], json!("ruff"));
        assert_eq!(r["ok"], json!(false));
        assert_eq!(r["via"], json!("path"));
        assert!(matches!(r["installed"], Value::Bool(_)));
        let reason = r["reason"].as_str().unwrap();
        assert!(reason.contains("standalone binary"));
        assert!(reason.contains("pip install ruff"));
        let keys: Vec<&String> = r.as_object().unwrap().keys().collect();
        assert_eq!(keys, ["linter", "ok", "via", "installed", "reason"]);
    }

    #[test]
    fn npm_via_linter_with_bogus_npm_binary_reports_error_shape() {
        // Real subprocess path (npm on PATH is not guaranteed in CI): swap ANTILOKI_HOME so
        // TOOLS_DIR is a throwaway temp dir, then call with an id whose pkg exists but let the
        // real `npm`/`npm.cmd` run — if npm isn't installed, runCmd's spawn-error branch still
        // produces the full `{ok,code,log}` shape deterministically (installed stays false).
        let _guard = crate::test_env_lock::ENV_LOCK.lock().unwrap();
        let mut dir = std::env::temp_dir();
        dir.push(format!("antiloki-lint-install-rs-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        unsafe { std::env::set_var("ANTILOKI_HOME", dir.to_str().unwrap()); }

        let r = lintInstall(&json!({ "linter": "eslint" }), &db0()).unwrap();

        unsafe { std::env::remove_var("ANTILOKI_HOME"); }
        let _ = fs::remove_dir_all(&dir);

        assert_eq!(r["linter"], json!("eslint"));
        assert_eq!(r["pkg"], json!("eslint"));
        let keys: Vec<&String> = r.as_object().unwrap().keys().collect();
        assert_eq!(keys, ["linter", "pkg", "installed", "ok", "code", "timed_out", "log"]);
        assert!(r["code"].is_i64() || r["code"].is_u64());
        assert!(matches!(r["installed"], Value::Bool(_)));
        assert!(matches!(r["timed_out"], Value::Bool(_)));
        assert!(r["log"].is_string());
    }
}
