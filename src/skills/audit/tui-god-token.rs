//! tui-god-token.rs — twin of skills/audit/tui-god-token.mjs. UNUSUAL SIGNATURE (not the
//! standard (input, context) shape, not the 2-arg no-db helper shape either): the JS default
//! export takes exactly ONE positional arg, `checkTuiGodToken(ctx)`. NAMES ARE CONTRACT:
//! `checkTuiGodToken` (default), `TOKEN_HASH_FILE` (the resolved-path constant re-export —
//! modeled as the `token_hash_file()` fn, same convention as sibling god-driver-halt.rs /
//! god-driver-resume.rs / driver-halt-check.rs for a roots.mjs-derived path).
//!
//! sha256-compares a plaintext TUI god-mode session token (`ctx.tui_god_token`) against the
//! on-disk hash file the TUI writes on activation (roots.mjs `TOKEN_HASH_FILE` =
//! `resolve(RUN_DIR, 'tui-god-token.sha256')`, `RUN_DIR` = `HOME_DIR/run`). Pure read; no side
//! effects (never writes, mirrors the JS module doc "Pure read; no side effects").
//!
//! HOME_DIR resolution mirrors the already-converted skills/agents/god/god-driver-resume.rs /
//! skills/driver/driver-halt-check.rs precedent (no `dirs`-crate dependency): `ANTILOKI_HOME`
//! env override (plain path join, no cwd-resolve — callers never pass a relative value in
//! practice) else Node `os.homedir()` semantics (POSIX `$HOME`; Windows `%USERPROFILE%` falling
//! back to `%HOMEDRIVE%%HOMEPATH%`).
//!
//! TOKEN_HASH_FILE is a fixed module-level constant (not input-injectable — same class as
//! god-driver-resume.rs's HALT_FILE) — a real fs read against a resolved path, so this is NOT
//! fixture-testable through the normal conform-db harness (no fs-injection lane exists yet).
//! Tested via bespoke #[cfg(test)] native tests using ANTILOKI_HOME env-var + tempdir injection,
//! copying the `with_home()` pattern from god-driver-halt.rs exactly.
//!
//! Function is total (never Err): the real JS never throws out of `checkTuiGodToken` — the one
//! spot that could (`readFileSync` after `existsSync` already confirmed presence) is caught
//! internally by the JS itself and folded into the `{ ok:false, reason: ... }` return shape.
//! VET / disclosed gap: that JS `catch (e) { reason: \`failed to read god-token hash file:
//! ${e.message}\` }` branch requires winning a race (file deleted / permission-denied between
//! the `existsSync` and `readFileSync` calls) that is not practically triggerable in a test, and
//! its message is a Node-runtime-formatted string ("ENOENT: ..., open '<path>'" style) this twin
//! cannot byte-reproduce with `std::io::Error`'s Display — treated as an honestly-uncovered gap,
//! same class as an unreachable guard-throw (gotcha #9). This twin's read-error branch mirrors
//! the JS's `reason` prefix and appends `e.to_string()` for parity of shape, not of exact text.
//!
//! sha256 hex: `hex::encode(Sha256::digest(...))` = Node's
//! `createHash('sha256').update(s).digest('hex')` (both lowercase hex).

use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::path::PathBuf;

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
// RUN_DIR = join(HOME_DIR, 'run'); TOKEN_HASH_FILE = resolve(RUN_DIR, 'tui-god-token.sha256').
// Private: TOKEN_HASH_FILE is a plain const in roots.mjs (not a census-counted function), so
// `pub` here would falsely flag as an extra function with no JS counterpart under names_ok.
fn token_hash_file() -> PathBuf {
    let home_dir = match std::env::var("ANTILOKI_HOME") {
        Ok(v) if !v.is_empty() => PathBuf::from(v),
        _ => homedir().unwrap_or_default().join(".antiloki"),
    };
    home_dir.join("run").join("tui-god-token.sha256")
}

fn sha256_hex(s: &str) -> String {
    hex::encode(Sha256::digest(s.as_bytes()))
}

/// `const plain = ctx?.tui_god_token; if (!plain || typeof plain !== 'string') return {...}` —
/// truthy-AND-typeof-string check, so `""` (empty string, falsy) and any non-string JSON type
/// (number/bool/array/object/null) all fall into the same "no tui_god_token in context" branch.
pub fn checkTuiGodToken(ctx: &Value) -> Value {
    let plain = match ctx.get("tui_god_token").and_then(|v| v.as_str()) {
        Some(s) if !s.is_empty() => s,
        _ => {
            return json!({ "ok": false, "reason": "no tui_god_token in context" });
        }
    };

    let path = token_hash_file();
    if !path.exists() {
        return json!({ "ok": false, "reason": "no active TUI god session (hash file missing)" });
    }

    let stored_raw = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) => {
            return json!({
                "ok": false,
                "reason": format!("failed to read god-token hash file: {e}"),
            });
        }
    };
    let stored = stored_raw.trim();
    if stored.is_empty() {
        return json!({ "ok": false, "reason": "hash file is empty (TUI exited or deactivated)" });
    }

    let computed = sha256_hex(plain);
    if computed != stored {
        return json!({
            "ok": false,
            "reason": "token does not match the active TUI god session",
        });
    }

    json!({ "ok": true, "reason": "token verified against active TUI session" })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::Mutex;

    // SAFETY: ANTILOKI_HOME is process-global; serialize since `cargo test` runs threads in
    // parallel by default, and this same var is read by other converted twins' tests too.
    // shared crate-wide lock (test_env_lock.rs) — a local mutex here would not prevent a race with OTHER files' tests over the same real env var.

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
        dir.push(format!("antiloki-tui-god-token-rs-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn no_token_in_context_missing_key() {
        let r = checkTuiGodToken(&json!({}));
        assert_eq!(r, json!({ "ok": false, "reason": "no tui_god_token in context" }));
    }

    #[test]
    fn no_token_in_context_null_ctx() {
        let r = checkTuiGodToken(&Value::Null);
        assert_eq!(r, json!({ "ok": false, "reason": "no tui_god_token in context" }));
    }

    #[test]
    fn no_token_in_context_empty_string_falsy() {
        let r = checkTuiGodToken(&json!({ "tui_god_token": "" }));
        assert_eq!(r, json!({ "ok": false, "reason": "no tui_god_token in context" }));
    }

    #[test]
    fn no_token_in_context_non_string_value() {
        let r = checkTuiGodToken(&json!({ "tui_god_token": 12345 }));
        assert_eq!(r, json!({ "ok": false, "reason": "no tui_god_token in context" }));
    }

    #[test]
    fn hash_file_missing_no_active_session() {
        let dir = temp_dir("missing");
        let r = with_home(dir.to_str().unwrap(), || {
            checkTuiGodToken(&json!({ "tui_god_token": "plain-token" }))
        });
        assert_eq!(
            r,
            json!({ "ok": false, "reason": "no active TUI god session (hash file missing)" })
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn hash_file_empty_deactivated() {
        let dir = temp_dir("empty");
        let run_dir = dir.join("run");
        fs::create_dir_all(&run_dir).unwrap();
        fs::write(run_dir.join("tui-god-token.sha256"), "").unwrap();
        let r = with_home(dir.to_str().unwrap(), || {
            checkTuiGodToken(&json!({ "tui_god_token": "plain-token" }))
        });
        assert_eq!(
            r,
            json!({ "ok": false, "reason": "hash file is empty (TUI exited or deactivated)" })
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn hash_file_whitespace_only_trims_to_empty() {
        let dir = temp_dir("whitespace");
        let run_dir = dir.join("run");
        fs::create_dir_all(&run_dir).unwrap();
        fs::write(run_dir.join("tui-god-token.sha256"), "   \n").unwrap();
        let r = with_home(dir.to_str().unwrap(), || {
            checkTuiGodToken(&json!({ "tui_god_token": "plain-token" }))
        });
        assert_eq!(
            r,
            json!({ "ok": false, "reason": "hash file is empty (TUI exited or deactivated)" })
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn token_mismatch() {
        let dir = temp_dir("mismatch");
        let run_dir = dir.join("run");
        fs::create_dir_all(&run_dir).unwrap();
        fs::write(run_dir.join("tui-god-token.sha256"), sha256_hex("the-real-token")).unwrap();
        let r = with_home(dir.to_str().unwrap(), || {
            checkTuiGodToken(&json!({ "tui_god_token": "a-different-token" }))
        });
        assert_eq!(
            r,
            json!({ "ok": false, "reason": "token does not match the active TUI god session" })
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn token_match_verified() {
        let dir = temp_dir("match");
        let run_dir = dir.join("run");
        fs::create_dir_all(&run_dir).unwrap();
        fs::write(run_dir.join("tui-god-token.sha256"), sha256_hex("the-real-token")).unwrap();
        let r = with_home(dir.to_str().unwrap(), || {
            checkTuiGodToken(&json!({ "tui_god_token": "the-real-token" }))
        });
        assert_eq!(
            r,
            json!({ "ok": true, "reason": "token verified against active TUI session" })
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn stored_hash_whitespace_trimmed_before_compare() {
        let dir = temp_dir("trim-match");
        let run_dir = dir.join("run");
        fs::create_dir_all(&run_dir).unwrap();
        fs::write(
            run_dir.join("tui-god-token.sha256"),
            format!("{}\n", sha256_hex("token-with-newline")),
        )
        .unwrap();
        let r = with_home(dir.to_str().unwrap(), || {
            checkTuiGodToken(&json!({ "tui_god_token": "token-with-newline" }))
        });
        assert_eq!(
            r,
            json!({ "ok": true, "reason": "token verified against active TUI session" })
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn token_hash_file_resolves_under_home_run() {
        let dir = temp_dir("resolve-path");
        let expected = dir.join("run").join("tui-god-token.sha256");
        let resolved = with_home(dir.to_str().unwrap(), token_hash_file);
        assert_eq!(resolved, expected);
        let _ = fs::remove_dir_all(&dir);
    }
}
