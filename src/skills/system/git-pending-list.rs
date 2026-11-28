//! git-pending-list.rs — twin of skills/system/git-pending-list.mjs. NAMES ARE CONTRACT:
//! `gitPendingList` (default). Read-only: lists uncommitted code files (the "drift" set the
//! COMMIT_CODE_BEFORE_AUDIT_COMMIT law blocks on) by delegating to `detectCodeDrift`.
//!
//! BESPOKE-NATIVE, not conform-db fixture-tested: every call runs a real `git status --porcelain`
//! subprocess via `external_git_commit_card::detectCodeDrift` (no input-flag/optional-field escape
//! hatch keeps it fixture-deterministic) — the same category as that module's own
//! `detectCodeDrift`/`isGitIgnored`/`realGitRunner`, which carry zero frontier/fixture entries by
//! design and are proven only by native `#[cfg(test)]` tests against real temp git repos. This
//! twin is a thin wrapper with no fixture-safe branch at all (unlike `gitCommitCard`'s zero-events
//! early return), so it follows the same no-frontier-entry convention as its dependency.
//!
//! `context.db` (JS `context?.db`) → modeled as `Option<&Connection>`, forwarded verbatim to
//! `detectCodeDrift` (which itself resolves `getWorkspaceRoot(db)` when present, else falls back
//! to the calling process's own cwd).

use rusqlite::Connection;
use serde_json::{json, Value};

/// `export default function gitPendingList(input, context)` — `input` is unused by the JS source
/// (only `context.db` is read), so this twin drops it entirely rather than threading an unused
/// `&Value` through, matching bespoke-native siblings that only model the fields actually read.
pub fn gitPendingList(db: Option<&Connection>) -> Value {
    let files = crate::external_git_commit_card::detectCodeDrift(db);
    let count = files.len() as i64;
    json!({ "files": files, "count": count })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::process::Command;

    fn temp_repo(tag: &str) -> std::path::PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!("antiloki-git-pending-list-rs-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let run = |args: &[&str]| {
            let st = Command::new("git").args(args).current_dir(&dir).status().unwrap();
            assert!(st.success(), "git {args:?} failed");
        };
        run(&["init", "-q"]);
        run(&["config", "user.email", "test@antiloki.local"]);
        run(&["config", "user.name", "antiloki-test"]);
        dir
    }

    #[test]
    fn no_db_never_panics_and_returns_files_count_shape() {
        let r = gitPendingList(None);
        assert!(r["files"].is_array());
        assert_eq!(r["count"], json!(r["files"].as_array().unwrap().len() as i64));
    }

    #[test]
    fn count_matches_files_len_against_a_real_dirty_repo() {
        let _guard = crate::test_env_lock::ENV_LOCK.lock().unwrap();
        let dir = temp_repo("pending");
        fs::write(dir.join("tracked.txt"), "v1\n").unwrap();
        Command::new("git").args(["add", "tracked.txt"]).current_dir(&dir).status().unwrap();
        Command::new("git").args(["commit", "-q", "-m", "init"]).current_dir(&dir).status().unwrap();
        fs::write(dir.join("rogue.mjs"), "export default 1;\n").unwrap(); // untracked, unmanaged

        // A db with an active workspace instead of a chdir. detectCodeDrift already runs git in
        // the workspace root when it has one and only falls back to the process CWD without a db —
        // so passing one removes the need to mutate a PROCESS-GLOBAL that every other concurrently
        // running test also reads. That chdir was unguarded, and an unguarded chdir corrupts
        // neighbours regardless of any lock THIS file takes, since the victims hold no lock.
        let c = rusqlite::Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE workspaces (id INTEGER PRIMARY KEY, name TEXT, root_path TEXT, vcs TEXT, is_active INTEGER, created_at TEXT);",
        ).unwrap();
        c.execute(
            "INSERT INTO workspaces (name, root_path, vcs, is_active) VALUES ('fx', ?1, 'git', 1)",
            rusqlite::params![dir.to_string_lossy().into_owned()],
        ).unwrap();
        let r = gitPendingList(Some(&c));
        let _ = fs::remove_dir_all(&dir);

        assert_eq!(r["files"], json!(["rogue.mjs"]));
        assert_eq!(r["count"], json!(1));
    }
}
