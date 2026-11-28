//! git-pending-commit.rs — twin of skills/system/git-pending-commit.mjs. NAMES ARE CONTRACT:
//! `gitPendingCommit` (default).
//!
//! Commits ONLY the operator-selected pending code files with an operator-confirmed message —
//! the commit-from-chat resolution path for `COMMIT_CODE_BEFORE_AUDIT_COMMIT`. Safety: only files
//! currently in the drift set (`external_git_commit_card::detectCodeDrift`) are committable; the
//! request is intersected with that set, never `git add -A`.
//!
//! BESPOKE-NATIVE, not conform-db fixture-tested: every call shells out to REAL `git` subprocesses
//! (`rev-parse --show-toplevel`, `add`, `commit -F -`, `rev-parse HEAD`) against a real repo
//! toplevel — same category as `external_git_commit_card::detectCodeDrift`/`isGitIgnored`/
//! `realGitRunner` and `system_git_pending_list::gitPendingList`, its two already-converted
//! siblings in this file family. Proven below via `#[cfg(test)]` against a real temp git repo.
//!
//! Cross-module calls (already-converted twins, reused not reimplemented):
//! `crate::external_git_commit_card::detectCodeDrift` and
//! `crate::engine_workspace_root::getWorkspaceRoot`.
//!
//! `context.db` (JS `context?.db`) → `Option<&Connection>`, same modeling as
//! `system_git_pending_list::gitPendingList`: forwarded to `getWorkspaceRoot` (when present, to
//! pin all git subprocesses to the active workspace root rather than the process's own cwd) and to
//! `detectCodeDrift` (which independently resolves the same root internally).
//!
//! Throw fidelity — every thrown message is `AntilokiError{skill:'git-pending-commit', cause}` →
//! rendered `"git-pending-commit: <cause>"`:
//! - missing/empty `files` → `"files is required"`.
//! - missing/empty `message` → `"message is required"`.
//! - intersection with the drift set is empty → `"none of the requested files are in the current
//!   pending set"`.
//! - `git rev-parse --show-toplevel` fails → `"not a git repo: <stderr.trim() or 'exit <code>'>"`.
//! - any other git subprocess fails → `"git <args[0]> failed: <(stderr||stdout).trim() or 'exit
//!   <code>'>"`.

use rusqlite::Connection;
use serde_json::{json, Value};
use std::collections::HashSet;
use std::io::Write;
use std::process::{Command, Stdio};

struct GitOut {
    status: i32,
    stdout: String,
    stderr: String,
}

fn run_git(args: &[&str], cwd: Option<&str>, input: Option<&str>) -> GitOut {
    let mut cmd = crate::proc::cmd("git");
    cmd.args(args).stdout(Stdio::piped()).stderr(Stdio::piped());
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    cmd.stdin(if input.is_some() { Stdio::piped() } else { Stdio::null() });
    let child = match cmd.spawn() {
        Ok(c) => c,
        Err(_) => return GitOut { status: -1, stdout: String::new(), stderr: String::new() },
    };
    let mut child = child;
    if let Some(data) = input {
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(data.as_bytes());
        }
    }
    match child.wait_with_output() {
        Ok(out) => GitOut {
            status: out.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        },
        Err(_) => GitOut { status: -1, stdout: String::new(), stderr: String::new() },
    }
}

/// `function git(args, repoRoot, opts)`.
fn git(args: &[&str], repo_root: &str, input: Option<&str>) -> Result<String, String> {
    let out = run_git(args, Some(repo_root), input);
    if out.status != 0 {
        let stderr_trim = out.stderr.trim();
        let stdout_trim = out.stdout.trim();
        let msg = if !stderr_trim.is_empty() {
            stderr_trim.to_string()
        } else if !stdout_trim.is_empty() {
            stdout_trim.to_string()
        } else {
            format!("exit {}", out.status)
        };
        return Err(format!("git-pending-commit: git {} failed: {}", args.first().copied().unwrap_or(""), msg));
    }
    Ok(out.stdout.trim().to_string())
}

/// `function repoToplevel(cwd)`.
fn repo_toplevel(cwd: Option<&str>) -> Result<String, String> {
    let out = run_git(&["rev-parse", "--show-toplevel"], cwd, None);
    if out.status != 0 {
        let stderr_trim = out.stderr.trim();
        let msg = if !stderr_trim.is_empty() { stderr_trim.to_string() } else { format!("exit {}", out.status) };
        return Err(format!("git-pending-commit: not a git repo: {msg}"));
    }
    Ok(out.stdout.trim().to_string())
}

/// `export default function gitPendingCommit(input, context)`.
pub fn gitPendingCommit(input: &Value, db: Option<&Connection>) -> Result<Value, String> {
    let requested = match input.get("files").and_then(Value::as_array) {
        Some(a) if !a.is_empty() => a,
        _ => return Err("git-pending-commit: files is required".to_string()),
    };
    let message = input
        .get("message")
        .and_then(Value::as_str)
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    if message.is_empty() {
        return Err("git-pending-commit: message is required".to_string());
    }

    // Run ALL git in the ACTIVE WORKSPACE's repo, not the BE's cwd — otherwise this would commit
    // the substrate's own untracked files. detectCodeDrift is measured in the same frame (same
    // workspace root); a table-less/no-db lookup silently falls back (matches the JS try/catch).
    let ws_root = db
        .map(crate::engine_workspace_root::getWorkspaceRoot)
        .and_then(|v| v.as_str().map(|s| s.to_string()));

    let drift: HashSet<String> = crate::external_git_commit_card::detectCodeDrift(db).into_iter().collect();
    let files: Vec<Value> = requested
        .iter()
        .filter(|f| f.as_str().map(|s| drift.contains(s)).unwrap_or(false))
        .cloned()
        .collect();
    if files.is_empty() {
        return Err("git-pending-commit: none of the requested files are in the current pending set".to_string());
    }

    let root = repo_toplevel(ws_root.as_deref())?;

    let file_strs: Vec<&str> = files.iter().filter_map(Value::as_str).collect();
    let mut add_args: Vec<&str> = vec!["add", "--"];
    add_args.extend(file_strs.iter().copied());
    git(&add_args, &root, None)?;
    git(&["commit", "-F", "-"], &root, Some(&message))?;
    let git_sha = git(&["rev-parse", "HEAD"], &root, None)?;

    Ok(json!({ "committed": files.len() as i64, "files": files, "gitSha": git_sha }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    fn temp_repo(tag: &str) -> PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!("antiloki-git-pending-commit-rs-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let run = |args: &[&str]| {
            let st = crate::proc::cmd("git").args(args).current_dir(&dir).status().unwrap();
            assert!(st.success(), "git {args:?} failed");
        };
        run(&["init", "-q"]);
        run(&["config", "user.email", "test@antiloki.local"]);
        run(&["config", "user.name", "antiloki-test"]);
        fs::write(dir.join("README.md"), "# test\n").unwrap();
        run(&["add", "README.md"]);
        run(&["commit", "-q", "-m", "init"]);
        dir
    }

    /// A db whose ACTIVE WORKSPACE is `dir`.
    ///
    /// Replaces an `in_dir` helper that chdir'd the process under the crate-wide lock. Taking a
    /// lock does not make a chdir safe: process CWD is read by every concurrently running test,
    /// and none of them hold that lock. It cost three unrelated flakes in different files
    /// (code-transform, code-regenerate, source-add) before the pattern was clear — a global
    /// mutated "carefully" is still a global.
    ///
    /// gitPendingCommit already runs all its git in the active workspace's repo when it has a db,
    /// so handing it one removes the need for a chdir entirely.
    fn db_at(dir: &std::path::Path) -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE workspaces (id INTEGER PRIMARY KEY, name TEXT, root_path TEXT, vcs TEXT, is_active INTEGER, created_at TEXT);",
        ).unwrap();
        c.execute(
            "INSERT INTO workspaces (name, root_path, vcs, is_active) VALUES ('fx', ?1, 'git', 1)",
            rusqlite::params![dir.to_string_lossy().into_owned()],
        ).unwrap();
        c
    }

    #[test]
    fn missing_files_errs() {
        let e = gitPendingCommit(&json!({}), None).unwrap_err();
        assert_eq!(e, "git-pending-commit: files is required");
        let e2 = gitPendingCommit(&json!({ "files": [] }), None).unwrap_err();
        assert_eq!(e2, "git-pending-commit: files is required");
    }

    #[test]
    fn missing_message_errs() {
        let e = gitPendingCommit(&json!({ "files": ["a.txt"] }), None).unwrap_err();
        assert_eq!(e, "git-pending-commit: message is required");
        let e2 = gitPendingCommit(&json!({ "files": ["a.txt"], "message": "  " }), None).unwrap_err();
        assert_eq!(e2, "git-pending-commit: message is required");
    }

    #[test]
    fn empty_intersection_with_pending_set_errs() {
        let dir = temp_repo("no-intersect");
        let c = db_at(&dir);
        let e = gitPendingCommit(&json!({ "files": ["nope.txt"], "message": "m" }), Some(&c)).unwrap_err();
        assert_eq!(e, "git-pending-commit: none of the requested files are in the current pending set");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn commits_only_the_requested_pending_files() {
        let dir = temp_repo("commit-subset");
        fs::write(dir.join("wanted.txt"), "hello\n").unwrap();
        fs::write(dir.join("unwanted.txt"), "nope\n").unwrap();

        let c = db_at(&dir);
        let r = gitPendingCommit(
            &json!({ "files": ["wanted.txt", "not-pending.txt"], "message": "add wanted" }),
            Some(&c),
        )
        .unwrap();

        assert_eq!(r["committed"], json!(1));
        assert_eq!(r["files"], json!(["wanted.txt"]));
        let sha = r["gitSha"].as_str().unwrap();
        assert_eq!(sha.len(), 40);
        assert!(sha.chars().all(|c| c.is_ascii_hexdigit()));

        // unwanted.txt is still untracked (not committed); wanted.txt is now tracked+clean.
        let status = crate::proc::cmd("git").args(["status", "--porcelain"]).current_dir(&dir).output().unwrap();
        let status_str = String::from_utf8_lossy(&status.stdout);
        assert!(status_str.contains("unwanted.txt"), "{status_str}");
        // git porcelain prefixes each path with a space; use it as a boundary so the
        // "wanted.txt" check isn't tripped by the "unwanted.txt" substring.
        assert!(!status_str.contains(" wanted.txt"), "{status_str}");

        let log = crate::proc::cmd("git").args(["log", "-1", "--pretty=%s"]).current_dir(&dir).output().unwrap();
        assert_eq!(String::from_utf8_lossy(&log.stdout).trim(), "add wanted");

        let _ = fs::remove_dir_all(&dir);
    }
}
