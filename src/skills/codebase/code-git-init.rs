//! code-git-init.rs — twin of skills/codebase/code-git-init.mjs. NAMES ARE CONTRACT:
//! `codeGitInit` (default).
//!
//! BESPOKE-NATIVE (playbook's "not conditionally avoidable" rule): unlike
//! constitution-estimate.rs's git-diff branch (which fires only when `input.base` is truthy,
//! with a git-free `walk()` default path carrying the fixture), EVERY meaningful success path
//! of this skill unconditionally shells out to `git` — even the "already a repo" outcome
//! requires the `git rev-parse --is-inside-work-tree` probe to succeed. The only git-free return
//! (`{ok:false, error:"no workspace root resolved"}`) fires only when the caller passes an
//! explicit `input.root: ""` (`getWorkspaceRoot` itself always falls back to a truthy
//! SUBSTRATE_ROOT — see workspace-root.rs — so a real caller never reaches it via the ??
//! fallback). Per the playbook: real correct code for every branch, proven by `#[cfg(test)]`
//! native tests against a real temp-dir git repo (god-driver-halt.rs's temp-dir precedent), NO
//! conform-db fixture and NO frontier entry — this unit is exercised only by the native tests
//! below.
//!
//! `context.db` missing → JS throws `err.contextMissing('code-git-init', 'db')`. Not modeled:
//! `db: &Connection` is a required, non-nullable reference here — same "guard-throws are not
//! exercised" convention used by every sibling twin.
//!
//! `input?.root ?? getWorkspaceRoot(db)`: mirrored via `nz` (JS `??`, null/undefined only).
//! `root` is always a string in every real call site; a non-string `input.root` is not a
//! realistic input in this substrate (downstream JS `join()`/`spawnSync(..., {cwd})` would
//! itself misbehave on a non-string cwd) and is treated as the empty string here, same as the
//! "no workspace root resolved" edge above.
//!
//! `try { ... } catch { /* best-effort */ }` around the `.gitignore` write: any fs error is
//! swallowed in JS, never surfaces in the return value — mirrored with `let _ = fs::write(...)`.
//!
//! `(x.stderr || '').slice(0, 200)`: JS UTF-16 slice on an ASCII-range git error string (see
//! playbook lesson 5) — `.chars().take(200)`.
// QUALITY: bespoke-native (real git subprocess on every meaningful branch) — no fixture/frontier
// entry; correctness proven by the native tests below against a real temp-dir git repo.

use rusqlite::Connection;
use serde_json::{json, Value};
use std::path::Path;

fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) } // JS `??` right operand

const GITIGNORE: &str = "node_modules/\ndist/\nbuild/\n.next/\nout/\ncoverage/\n.DS_Store\n*.log\n.env\n.env.local\n";

struct GitResult {
    ok: bool,
    stdout: String,
    stderr: String,
}

// `const git = (args, cwd) => spawnSync('git', args, { cwd, encoding: 'utf8', maxBuffer: ... })`
fn git(args: &[&str], cwd: &str) -> GitResult {
    match crate::proc::cmd("git").args(args).current_dir(cwd).output() {
        Ok(o) => GitResult {
            ok: o.status.success(),
            stdout: String::from_utf8_lossy(&o.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&o.stderr).into_owned(),
        },
        // spawnSync with a missing/unspawnable binary: status stays null (!== 0), stdout/stderr
        // stay unset — same "failed" shape without a stderr message.
        Err(_) => GitResult { ok: false, stdout: String::new(), stderr: String::new() },
    }
}

// JS `(s || '').slice(0, 200)` on an ASCII-range git error string.
fn slice0_200(s: &str) -> String { s.chars().take(200).collect() }

fn resolve_root(input: &Value, db: &Connection) -> String {
    if let Some(v) = nz(input.get("root")) {
        return v.as_str().unwrap_or_default().to_string();
    }
    let root_v = crate::engine_workspace_root::getWorkspaceRoot(db);
    root_v.as_str().unwrap_or_default().to_string()
}

pub fn codeGitInit(input: &Value, db: &Connection) -> Value {
    let root = resolve_root(input, db);
    if root.is_empty() {
        return json!({ "ok": false, "error": "no workspace root resolved" });
    }

    // already a repo → no-op (never re-init / touch existing history)
    let probe = git(&["rev-parse", "--is-inside-work-tree"], &root);
    if probe.ok && probe.stdout.trim() == "true" {
        return json!({ "ok": true, "already": true, "root": root });
    }

    // best-effort .gitignore write — never fails the operation
    let gi = Path::new(&root).join(".gitignore");
    if !gi.exists() {
        let _ = std::fs::write(&gi, GITIGNORE);
    }

    let ini = git(&["init"], &root);
    if !ini.ok {
        return json!({ "ok": false, "root": root, "error": format!("git init failed: {}", slice0_200(&ini.stderr)) });
    }
    let _ = git(&["add", "-A"], &root);
    // pass an identity inline so the commit works even if the workspace has no git user configured
    let commit = git(
        &["-c", "user.name=antiloki", "-c", "user.email=antiloki@local", "commit", "-m", "Initial commit (antiloki)"],
        &root,
    );
    if !commit.ok {
        let src = if !commit.stderr.is_empty() { &commit.stderr } else { &commit.stdout };
        return json!({ "ok": false, "root": root, "error": format!("commit failed: {}", slice0_200(src)) });
    }

    let head_out = git(&["rev-parse", "--short", "HEAD"], &root);
    let head_trim = head_out.stdout.trim();
    let head = if head_trim.is_empty() { Value::Null } else { json!(head_trim) };
    let files_out = git(&["ls-files"], &root);
    let files = files_out.stdout.split('\n').filter(|l| !l.is_empty()).count() as i64;

    json!({ "ok": true, "initialized": true, "root": root, "head": head, "files": files })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn db0() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE workspaces (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL UNIQUE, \
             root_path TEXT NOT NULL, vcs TEXT NOT NULL DEFAULT 'git', is_active INTEGER NOT NULL DEFAULT 0, \
             created_at TEXT NOT NULL DEFAULT '2026-01-01 00:00:00')",
        ).unwrap();
        c
    }

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!("antiloki-code-git-init-rs-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn no_workspace_root_resolved_when_input_root_is_explicit_empty_string() {
        let r = codeGitInit(&json!({ "root": "" }), &db0());
        assert_eq!(r, json!({ "ok": false, "error": "no workspace root resolved" }));
    }

    #[test]
    fn initializes_a_fresh_workspace_then_no_ops_on_the_already_repo_branch() {
        let dir = temp_dir("init-then-already");
        let root = dir.to_string_lossy().into_owned();
        fs::write(dir.join("hello.txt"), "hi").unwrap();

        let r1 = codeGitInit(&json!({ "root": root.clone() }), &db0());
        assert_eq!(r1["ok"], json!(true));
        assert_eq!(r1["initialized"], json!(true));
        assert_eq!(r1["root"], json!(root));
        assert!(r1["head"].as_str().is_some(), "head should be a short sha: {r1}");
        // files: hello.txt + the .gitignore this run wrote
        assert_eq!(r1["files"], json!(2));
        let keys: Vec<&String> = r1.as_object().unwrap().keys().collect();
        assert_eq!(keys, ["ok", "initialized", "root", "head", "files"]);

        assert!(dir.join(".gitignore").exists());
        let gi_contents = fs::read_to_string(dir.join(".gitignore")).unwrap();
        assert_eq!(gi_contents, GITIGNORE);

        // second call on the now-initialized repo → already:true, no re-init/no history touch
        let r2 = codeGitInit(&json!({ "root": root.clone() }), &db0());
        assert_eq!(r2, json!({ "ok": true, "already": true, "root": root }));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn does_not_overwrite_an_existing_gitignore() {
        let dir = temp_dir("keep-existing-gitignore");
        let root = dir.to_string_lossy().into_owned();
        fs::write(dir.join(".gitignore"), "custom-content\n").unwrap();
        fs::write(dir.join("a.txt"), "a").unwrap();

        let r = codeGitInit(&json!({ "root": root.clone() }), &db0());
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["initialized"], json!(true));
        let gi_contents = fs::read_to_string(dir.join(".gitignore")).unwrap();
        assert_eq!(gi_contents, "custom-content\n");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn falls_back_to_workspace_root_from_db_when_input_root_absent() {
        let dir = temp_dir("db-fallback-root");
        let root = dir.to_string_lossy().into_owned();
        fs::write(dir.join("a.txt"), "a").unwrap();

        let c = db0();
        c.execute(
            "INSERT INTO workspaces (name, root_path, vcs, is_active, created_at) VALUES ('x', ?1, 'git', 1, '2026-01-01 00:00:00')",
            rusqlite::params![root.clone()],
        ).unwrap();

        let r = codeGitInit(&json!({}), &c);
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["initialized"], json!(true));
        assert_eq!(r["root"], json!(root));

        let _ = fs::remove_dir_all(&dir);
    }
}
