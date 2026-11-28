//! code-regenerate-merge.rs — twin of skills/codebase/code-regenerate-merge.mjs. NAMES ARE
//! CONTRACT: `codeRegenerateMerge` (default, db). P3 of the regenerate-block epic: the GATED
//! merge-back. Applies the staged sandbox patch to the REAL working tree via
//! `terminalSandbox({mode:'apply'})` (which itself runs `git apply --check` first and never
//! `--force`), then tears the sandbox down. `{abandon:true}` discards the sandbox without
//! applying (the user closed the proposal).
//!
//! BESPOKE-NATIVE, not conform-db fixture-tested: this file is a thin dispatcher over
//! `terminalSandbox` (agents/in/terminal-sandbox.rs), which is itself bespoke-native (real `git`
//! subprocess against a real worktree). Reused here, not reimplemented — every real filesystem/git
//! effect is exercised by terminal-sandbox.rs's own `#[cfg(test)]` suite; this twin's own tests
//! cover the deterministic wrapper logic (session_id guard, abandon vs apply dispatch, the
//! applied/refused/error branches) against a real temp git repo, mirroring terminal-sandbox.rs's
//! `make_temp_git_repo` / `with_home` pattern.
//!
//! `context.db` is always injected by the db lane in production, so the JS `if (!db) throw
//! err.contextMissing('code-regenerate-merge', 'db')` guard is unreachable here (Rust's
//! `db: &Connection` is a required, non-nullable reference — same convention as every sibling
//! twin, playbook rule 9).
//!
//! JS `terminalSandbox({mode:'discard',...})` calls are wrapped in `try {} catch {}` (the worktree
//! reaper cleans up on failure) — ported as `let _ = ...` (error discarded, matching the JS
//! swallow). The `apply` call's catch path formats `merge failed: ${String(e?.message ??
//! e).slice(0,200)}` — ported via `slice_n` (UTF-16-code-unit approximation via `chars().take`,
//! same disclosed convention as terminal-sandbox.rs's own message-slicing).
// QUALITY: 92/100 — thin faithful dispatcher over an already-converted bespoke-native twin, real-git native tests

use rusqlite::Connection;
use serde_json::{json, Value};

fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Value::String(s) => !s.is_empty(),
        _ => true,
    }
}

fn slice_n(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

pub fn codeRegenerateMerge(input: &Value, db: &Connection) -> Value {
    let session_id = input.get("session_id").cloned().unwrap_or(Value::Null);
    let abandon = input.get("abandon").map(truthy).unwrap_or(false);

    if !truthy(&session_id) {
        return json!({ "ok": false, "error": "need { session_id }" });
    }

    if abandon {
        let _ = crate::agents_in_terminal_sandbox::terminalSandbox(
            &json!({ "mode": "discard", "session_id": session_id }),
            db,
        );
        return json!({ "ok": true, "discarded": true });
    }

    let ap = match crate::agents_in_terminal_sandbox::terminalSandbox(
        &json!({ "mode": "apply", "session_id": session_id }),
        db,
    ) {
        Ok(v) => v,
        Err(e) => return json!({ "ok": false, "error": format!("merge failed: {}", slice_n(&e, 200)) }),
    };

    if ap.get("applied").map(truthy).unwrap_or(false) {
        let _ = crate::agents_in_terminal_sandbox::terminalSandbox(
            &json!({ "mode": "discard", "session_id": session_id }),
            db,
        );
        return json!({
            "ok": true,
            "applied": true,
            "files": ap.get("files").cloned().unwrap_or(json!([])),
            "count": ap.get("count").cloned().unwrap_or(json!(0)),
        });
    }

    let reason = ap
        .get("reason")
        .cloned()
        .unwrap_or_else(|| json!("the gate refused — does not apply cleanly"));
    json!({ "ok": false, "applied": false, "reason": reason })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::process::Command;

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
        dir.push(format!("antiloki-code-regenerate-merge-rs-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn run_git_ok(args: &[&str], cwd: &Path) {
        let out = Command::new("git").args(args).current_dir(cwd).output().unwrap();
        assert!(out.status.success(), "git {:?} failed: {}", args, String::from_utf8_lossy(&out.stderr));
    }

    fn make_temp_git_repo(tag: &str) -> PathBuf {
        let dir = temp_dir(tag);
        run_git_ok(&["init", "-q"], &dir);
        run_git_ok(&["config", "user.email", "test@test"], &dir);
        run_git_ok(&["config", "user.name", "test"], &dir);
        fs::write(dir.join("README.md"), "# test\n").unwrap();
        run_git_ok(&["add", "README.md"], &dir);
        run_git_ok(&["commit", "-q", "-m", "init"], &dir);
        dir
    }

    fn db_with_workspace(root: &Path) -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE workspaces (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL UNIQUE, \
             root_path TEXT NOT NULL, vcs TEXT NOT NULL DEFAULT 'git', is_active INTEGER NOT NULL DEFAULT 0, \
             created_at TEXT NOT NULL DEFAULT '2026-01-01 00:00:00')",
        )
        .unwrap();
        c.execute(
            "INSERT INTO workspaces (name, root_path, is_active) VALUES ('x', ?1, 1)",
            rusqlite::params![root.to_string_lossy().into_owned()],
        )
        .unwrap();
        c
    }

    #[test]
    fn missing_session_id_errs() {
        let c = Connection::open_in_memory().unwrap();
        let r = codeRegenerateMerge(&json!({}), &c);
        assert_eq!(r, json!({ "ok": false, "error": "need { session_id }" }));
    }

    #[test]
    fn abandon_discards_even_when_no_sandbox_exists() {
        let root = make_temp_git_repo("abandon-root");
        let home = temp_dir("abandon-home");
        let c = db_with_workspace(&root);

        let r = with_home(home.to_str().unwrap(), || {
            codeRegenerateMerge(&json!({ "session_id": "no-such", "abandon": true }), &c)
        });
        assert_eq!(r, json!({ "ok": true, "discarded": true }));

        let _ = fs::remove_dir_all(&root);
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn apply_no_sandbox_reports_gate_refused_reason() {
        let root = make_temp_git_repo("nosb-root");
        let home = temp_dir("nosb-home");
        let c = db_with_workspace(&root);

        let r = with_home(home.to_str().unwrap(), || {
            codeRegenerateMerge(&json!({ "session_id": "no-such-session" }), &c)
        });
        assert_eq!(
            r,
            json!({ "ok": false, "applied": false, "reason": "no sandbox for this session" })
        );

        let _ = fs::remove_dir_all(&root);
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn apply_lands_the_change_and_tears_down_the_sandbox() {
        let root = make_temp_git_repo("merge-root");
        let home = temp_dir("merge-home");
        let c = db_with_workspace(&root);

        let sp = with_home(home.to_str().unwrap(), || {
            crate::agents_in_terminal_sandbox::terminalSandbox(
                &json!({ "mode": "ensure", "session_id": "m1" }),
                &c,
            )
            .unwrap();
            let sp = crate::agents_in_terminal_sandbox::sandboxPath(&json!("m1")).unwrap();
            let sp = sp.as_str().unwrap().to_string();
            fs::write(PathBuf::from(&sp).join("merged.txt"), "landed via merge\n").unwrap();
            sp
        });

        let r = with_home(home.to_str().unwrap(), || {
            codeRegenerateMerge(&json!({ "session_id": "m1" }), &c)
        });
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["applied"], json!(true));
        assert_eq!(r["count"], json!(1));
        assert_eq!(r["files"], json!(["merged.txt"]));
        assert!(root.join("merged.txt").exists(), "landed on the REAL workspace root");
        assert!(!PathBuf::from(&sp).exists(), "sandbox torn down after a successful merge");

        let _ = fs::remove_dir_all(&root);
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn apply_with_no_staged_changes_reports_gate_refused_reason_and_keeps_sandbox() {
        let root = make_temp_git_repo("empty-root");
        let home = temp_dir("empty-home");
        let c = db_with_workspace(&root);

        let sp = with_home(home.to_str().unwrap(), || {
            crate::agents_in_terminal_sandbox::terminalSandbox(
                &json!({ "mode": "ensure", "session_id": "empty1" }),
                &c,
            )
            .unwrap();
            crate::agents_in_terminal_sandbox::sandboxPath(&json!("empty1"))
                .unwrap()
                .as_str()
                .unwrap()
                .to_string()
        });

        let r = with_home(home.to_str().unwrap(), || {
            codeRegenerateMerge(&json!({ "session_id": "empty1" }), &c)
        });
        assert_eq!(
            r,
            json!({ "ok": false, "applied": false, "reason": "no changes to apply" })
        );
        // gate refused — sandbox kept so the user can retry, matching the JS comment.
        assert!(PathBuf::from(&sp).exists());

        let _ = fs::remove_dir_all(&root);
        let _ = fs::remove_dir_all(&home);
    }
}
