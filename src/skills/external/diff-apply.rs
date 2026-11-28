//! diff-apply.rs — twin of skills/external/diff-apply.mjs (db lane + real git subprocess). NAMES
//! ARE CONTRACT: `diffApply` (default), `realGitApply`, `isInScope` (`ALLOWED_SCOPES` is plain
//! data, not a function — not a census unit, transcribed as the `ALLOWED_SCOPES` const below).
//!
//! MIXED module, same class as run-cmd.rs / excalidraw-emit.rs: `isInScope` is a pure deterministic
//! string check and IS fixture-tested as an ordinary `sig:"no-db"` helper (fixture + frontier entry
//! + dispatch arm). `diffApply` and `realGitApply` are BESPOKE NATIVE — no conform-db fixture, no
//! frontier entry, no dispatch arm; proven only by the real `#[cfg(test)]` tests below.
//!
//! WHY bespoke-native, not the flag-skippable pattern (playbook lesson 5 / card-run-preflight.rs /
//! constitution-estimate.rs's git-diff branch): those units gate the subprocess behind an input
//! flag or optional field whose ABSENCE keeps the SUCCESS path deterministic and fixture-safe.
//! `diffApply` has no such flag — its JS body is `const gitApply = context.gitApply ||
//! realGitApply; ... gitApply(row.diff_text)`, and `context.gitApply` is a TEST-ONLY injection
//! seam (see diff-apply.test.mjs — every call stubs it): production callers (dev.diff.apply, the
//! conform-db JS lane's plain `{db}` context) never populate it, so EVERY reachable success path
//! unconditionally shells out to real `git apply`. There is no JSON-fixture-expressible way to
//! reach `status:'applied'` without a real git subprocess touching a real git working tree — same
//! reasoning as run-cmd.rs's `runCmd`. `realGitApply` itself IS that unconditional subprocess call,
//! so it inherits the same classification.
//!
//! `isInScope` — `file_path.replace(/\\/g, '/')` assumes a string arg (JS would throw a
//! TypeError on `.replace` for a non-string); real callers only ever pass a `pending_diffs.file_path`
//! TEXT column value or a literal string (see diff-apply.test.mjs), so this twin narrows to
//! `.as_str().unwrap_or_default()` rather than replaying a throw no caller exercises (same
//! "guard-throws are not exercised" convention every sibling twin uses).
//!
//! `realGitApply` — JS: dry-run `git apply --check -` first; a --check failure is caught and
//! re-thrown as a friendly `AntilokiError` (`diff does not apply cleanly: <first 3 stderr lines>`).
//! If --check passes, the REAL `git apply -` runs UNCAUGHT inside `realGitApply` — its failure
//! propagates as the raw subprocess error, not wrapped, matching the JS's asymmetry (only the
//! dry-run's catch block builds a friendly message). Neither git invocation passes a `cwd` option
//! in the JS (`execSync('git apply --check -', {input, stdio, encoding})` — no `cwd` key), so both
//! lanes run relative to the ACTUAL process cwd in production. `real_git_apply_in`/`diff_apply_impl`
//! below thread an `Option<&Path>` override purely as a TEST seam (temp git repos, guarded per-test
//! by using unique dirs — no process-global mutation, unlike ANTILOKI_HOME-style twins) — the
//! public `diffApply`/`realGitApply` entry points always pass `None`, so production behavior is
//! byte-for-byte the JS's no-cwd-override call.
//!
//! `diffApply` — SELECT the row, guard status='approved', guard `isInScope(file_path)`, snapshot
//! sha256 before (if the file exists), run git apply, on failure UPDATE the row to
//! status='superseded' with `decision_reason` appended (`e.message.slice(0,200)` — capped to 200
//! chars) and re-throw, on success snapshot sha256 after and UPDATE status='applied' +
//! `applied_at` (frozen-clock `datetime('2026-01-01 00:00:00')`) + `applied_sha`. `context.db`
//! missing → JS `throw err.contextMissing('diff-apply','db')`; unreachable here (`db: &Connection`
//! is a required, non-nullable reference, same convention every sibling db-lane twin uses).
// QUALITY: bespoke-native (diffApply, realGitApply) proven by real git-repo native tests;
// isInScope is pure/deterministic and fixture-tested normally.

use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}
fn str_qq(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(),
        Some(o) => o.to_string(),
    }
}
fn tmpl(v: Option<&Value>) -> String {
    match v {
        None => "undefined".into(),
        Some(Value::Null) => "null".into(),
        Some(x) => str_qq(Some(x)),
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(bytes))
}

// Same set of allowed scopes as the dev.file.write.* actions hardcode (JS module doc: "kept in
// sync manually for now").
const ALLOWED_SCOPES: [&str; 9] = [
    ".antiloki/skills/",
    "bin/",
    "docs/",
    ".mcp.json",
    "package.json",
    "tsconfig.json",
    ".npmrc",
    ".editorconfig",
    ".gitattributes",
];

/// `function isInScope(file_path) { const norm = file_path.replace(/\\/g, '/'); return
/// ALLOWED_SCOPES.some((scope) => norm.startsWith(scope)); }`
fn is_in_scope_str(file_path: &str) -> bool {
    let norm = file_path.replace('\\', "/");
    ALLOWED_SCOPES.iter().any(|scope| norm.starts_with(scope))
}

/// `export function isInScope(file_path)` — pure, no-db, fixture-tested normally.
pub fn isInScope(file_path: &Value) -> Value {
    json!(is_in_scope_str(file_path.as_str().unwrap_or_default()))
}

/// One `git apply [args] -` invocation, feeding `diff_text` on stdin exactly like JS's
/// `execSync(cmd, { input: diff_text, stdio: ['pipe','pipe','pipe'], encoding: 'utf8' })`.
/// `dir` is the test-only cwd-override seam (see module doc) — `None` in production, matching the
/// JS's lack of a `cwd` option. On failure, returns the captured stderr text (utf8-lossy).
fn run_git_apply(dir: Option<&Path>, args: &[&str], diff_text: &str) -> Result<(), String> {
    let mut cmd = crate::proc::cmd("git");
    cmd.args(args);
    if let Some(d) = dir {
        cmd.current_dir(d);
    }
    cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd.spawn().map_err(|e| e.to_string())?;
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(diff_text.as_bytes());
    }
    let output = child.wait_with_output().map_err(|e| e.to_string())?;
    if output.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).into_owned())
    }
}

/// `function realGitApply(diff_text)` — see module doc: real subprocess, bespoke-native. `dir` is
/// the test-only seam threaded through from `diff_apply_impl`; the public `realGitApply` below
/// always passes `None`.
fn real_git_apply_in(dir: Option<&Path>, diff_text: &str) -> Result<(), String> {
    if let Err(stderr) = run_git_apply(dir, &["apply", "--check", "-"], diff_text) {
        let first3: String = stderr.split('\n').take(3).collect::<Vec<_>>().join(" ").trim().to_string();
        let msg = if first3.is_empty() { stderr } else { first3 };
        return Err(format!("diff does not apply cleanly: {msg}"));
    }
    // The real apply runs UNCAUGHT in JS (no try/catch around this second execSync) — its
    // failure propagates as the raw subprocess stderr, not wrapped in a friendly prefix.
    run_git_apply(dir, &["apply", "-"], diff_text)
}

/// `export { realGitApply }` — see module doc: bespoke-native, no fixture/frontier/dispatch arm.
pub fn realGitApply(diff_text: &Value) -> Result<Value, String> {
    real_git_apply_in(None, diff_text.as_str().unwrap_or_default()).map(|_| Value::Null)
}

/// Shared implementation behind `diffApply`; `git_dir` is the test-only cwd-override seam (see
/// module doc) applied to BOTH the sha256 file reads and the git subprocess, since JS resolves
/// `row.file_path` (always repo-relative, per `ALLOWED_SCOPES`) against whatever cwd is active —
/// the same cwd the git subprocess itself runs in. `None` (the public entry point) reproduces the
/// JS exactly: everything resolves against the real process cwd.
fn diff_apply_impl(input: &Value, db: &Connection, git_dir: Option<&Path>) -> Result<Value, String> {
    let diff_id = input.get("diff_id").cloned().filter(|v| truthy(Some(v)));
    let diff_id = match diff_id {
        Some(v) => v,
        None => return Err("diff-apply: diff_id is required".into()),
    };
    let applied_by = input.get("applied_by").cloned().filter(|v| truthy(Some(v)));
    let applied_by = match applied_by {
        Some(v) => v,
        None => return Err("diff-apply: applied_by is required".into()),
    };

    let rows = crate::db_json::query_rows(db, "SELECT * FROM pending_diffs WHERE id = ?", &[diff_id.clone()])?;
    let row = match rows.as_array().and_then(|a| a.first()).cloned() {
        Some(r) => r,
        None => return Err(format!("diff-apply: pending_diff {} not found", tmpl(Some(&diff_id)))),
    };

    let status = row.get("status").and_then(Value::as_str).unwrap_or("");
    if status != "approved" {
        return Err(format!(
            "diff-apply: pending_diff {} is in status='{}'; only 'approved' diffs may be applied",
            tmpl(Some(&diff_id)),
            status
        ));
    }

    let file_path = row.get("file_path").and_then(Value::as_str).unwrap_or("").to_string();
    if !is_in_scope_str(&file_path) {
        return Err(format!(
            "diff-apply: file_path '{file_path}' not under any substrate-managed scope; refusing to apply"
        ));
    }

    let full_path: PathBuf = match git_dir {
        Some(d) => d.join(&file_path),
        None => PathBuf::from(&file_path),
    };

    // Snapshot before.
    let before_sha: Option<String> = if full_path.exists() {
        Some(sha256_hex(&std::fs::read(&full_path).map_err(|e| e.to_string())?))
    } else {
        None
    };

    let diff_text = row.get("diff_text").and_then(Value::as_str).unwrap_or("");
    if let Err(e) = real_git_apply_in(git_dir, diff_text) {
        // Mark the row superseded with the failure reason so the next proposal supersedes it
        // cleanly. `e.message.slice(0, 200)` — JS String.slice on UTF-16 units; this twin's fixture
        // scope is ASCII-only (guard/error-path text), so `.chars().take(200)` matches.
        // JS: this UPDATE is UNGUARDED inside the catch block — if it itself throws, that
        // exception propagates instead of the AntilokiError below. `?` mirrors that (a write
        // failure here returns early with the write's own error, not the apply-failed message).
        let capped: String = e.chars().take(200).collect();
        crate::db_json::run_write(
            db,
            "UPDATE pending_diffs\n      SET status = 'superseded',\n          decision_reason = COALESCE(decision_reason, '') || ' [apply-failed: ' || ? || ']'\n      WHERE id = ?",
            &[json!(capped), diff_id.clone()],
        )?;
        return Err(format!("diff-apply: apply failed for diff {}: {}", tmpl(Some(&diff_id)), e));
    }

    // Snapshot after.
    let after_sha: Option<String> = if full_path.exists() {
        Some(sha256_hex(&std::fs::read(&full_path).map_err(|e| e.to_string())?))
    } else {
        None
    };

    crate::db_json::run_write(
        db,
        "UPDATE pending_diffs\n    SET status = 'applied', applied_at = datetime('2026-01-01 00:00:00'), applied_sha = ?\n    WHERE id = ?",
        &[after_sha.clone().map_or(Value::Null, |s| json!(s)), diff_id.clone()],
    )?;

    let mut out = Map::new();
    out.insert("id".into(), diff_id);
    out.insert("file_path".into(), json!(file_path));
    out.insert("status".into(), json!("applied"));
    out.insert("applied_by".into(), applied_by);
    out.insert("before_sha256".into(), before_sha.map_or(Value::Null, |s| json!(s)));
    out.insert("after_sha256".into(), after_sha.map_or(Value::Null, |s| json!(s)));
    out.insert("card_slug".into(), row.get("card_slug").cloned().unwrap_or(Value::Null));
    Ok(Value::Object(out))
}

/// `export default function diffApply(input, context)` — see module doc: bespoke-native, no
/// fixture/frontier/dispatch arm.
pub fn diffApply(input: &Value, db: &Connection) -> Result<Value, String> {
    diff_apply_impl(input, db, None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    const SCHEMA: &str = "CREATE TABLE pending_diffs ( id INTEGER PRIMARY KEY, card_slug TEXT NOT NULL, file_path TEXT NOT NULL, diff_text TEXT NOT NULL, proposed_by TEXT NOT NULL, status TEXT NOT NULL DEFAULT 'pending' CHECK (status IN ('pending','approved','rejected','applied','superseded')), proposed_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')), decided_at TEXT, decided_by TEXT, decision_reason TEXT, applied_at TEXT, applied_sha TEXT )";

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(SCHEMA).unwrap();
        c
    }

    fn insert_diff(c: &Connection, id: i64, file_path: &str, diff_text: &str, status: &str) {
        c.execute(
            "INSERT INTO pending_diffs (id, card_slug, file_path, diff_text, proposed_by, status) VALUES (?1, 'CARD-X', ?2, ?3, 'ai:test', ?4)",
            rusqlite::params![id, file_path, diff_text, status],
        ).unwrap();
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!("antiloki-diff-apply-rs-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A real git repo with `bin/sample.mjs` containing "old\n", committed.
    fn fresh_repo(tag: &str) -> PathBuf {
        let dir = temp_dir(tag);
        let d = dir.to_str().unwrap();
        crate::proc::cmd("git").args(["init"]).current_dir(d).output().unwrap();
        crate::proc::cmd("git").args(["config", "user.email", "test@antiloki.local"]).current_dir(d).output().unwrap();
        crate::proc::cmd("git").args(["config", "user.name", "antiloki-test"]).current_dir(d).output().unwrap();
        // Pin line-ending handling regardless of the host's global core.autocrlf (Windows
        // commonly defaults to true, which would silently rewrite our LF fixtures to CRLF on
        // checkout/apply and break the byte-exact assertions below).
        crate::proc::cmd("git").args(["config", "core.autocrlf", "false"]).current_dir(d).output().unwrap();
        fs::create_dir_all(dir.join("bin")).unwrap();
        fs::write(dir.join("bin/sample.mjs"), "old\n").unwrap();
        crate::proc::cmd("git").args(["add", "-A"]).current_dir(d).output().unwrap();
        crate::proc::cmd("git").args(["commit", "-m", "initial"]).current_dir(d).output().unwrap();
        dir
    }

    const VALID_DIFF: &str = "--- a/bin/sample.mjs\n+++ b/bin/sample.mjs\n@@ -1 +1 @@\n-old\n+new\n";
    const CONFLICTING_DIFF: &str = "--- a/bin/sample.mjs\n+++ b/bin/sample.mjs\n@@ -1 +1 @@\n-does-not-match\n+new\n";

    #[test]
    fn is_in_scope_covers_all_scopes_and_backslash_normalization() {
        assert_eq!(isInScope(&json!(".antiloki/skills/foo.mjs")), json!(true));
        assert_eq!(isInScope(&json!("bin/antiloki.mjs")), json!(true));
        assert_eq!(isInScope(&json!("bin\\antiloki.mjs")), json!(true));
        assert_eq!(isInScope(&json!("node_modules/junk.js")), json!(false));
        assert_eq!(isInScope(&json!("src/secret.mjs")), json!(false));
    }

    #[test]
    fn success_path_applies_diff_updates_row_and_returns_shas() {
        let dir = fresh_repo("success");
        let c = db();
        insert_diff(&c, 1, "bin/sample.mjs", VALID_DIFF, "approved");

        let r = diff_apply_impl(&json!({ "diff_id": 1, "applied_by": "human:dani" }), &c, Some(&dir)).unwrap();

        assert_eq!(r["status"], json!("applied"));
        assert_eq!(r["applied_by"], json!("human:dani"));
        assert_eq!(r["file_path"], json!("bin/sample.mjs"));
        assert_eq!(r["card_slug"], json!("CARD-X"));
        assert_eq!(r["before_sha256"], json!(sha256_hex(b"old\n")));
        let after = fs::read(dir.join("bin/sample.mjs")).unwrap();
        assert_eq!(after, b"new\n");
        assert_eq!(r["after_sha256"], json!(sha256_hex(&after)));

        let row = crate::db_json::query_rows(&c, "SELECT status, applied_at, applied_sha FROM pending_diffs WHERE id = 1", &[]).unwrap();
        assert_eq!(row[0]["status"], json!("applied"));
        assert_eq!(row[0]["applied_at"], json!("2026-01-01 00:00:00"));
        assert_eq!(row[0]["applied_sha"], json!(sha256_hex(&after)));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn success_path_no_prior_file_leaves_before_sha_null() {
        let dir = fresh_repo("no-prior-file");
        let c = db();
        let diff = "--- /dev/null\n+++ b/bin/new-file.mjs\n@@ -0,0 +1 @@\n+hello\n";
        insert_diff(&c, 1, "bin/new-file.mjs", diff, "approved");
        let r = diff_apply_impl(&json!({ "diff_id": 1, "applied_by": "u" }), &c, Some(&dir)).unwrap();
        assert_eq!(r["before_sha256"], Value::Null);
        assert_eq!(r["after_sha256"], json!(sha256_hex(b"hello\n")));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn apply_failure_marks_row_superseded_and_preserves_reason() {
        let dir = fresh_repo("failure");
        let c = db();
        insert_diff(&c, 1, "bin/sample.mjs", CONFLICTING_DIFF, "approved");

        let err = diff_apply_impl(&json!({ "diff_id": 1, "applied_by": "u" }), &c, Some(&dir)).unwrap_err();
        assert!(err.contains("apply failed for diff"), "{err}");

        let row = crate::db_json::query_rows(&c, "SELECT status, decision_reason FROM pending_diffs WHERE id = 1", &[]).unwrap();
        assert_eq!(row[0]["status"], json!("superseded"));
        assert!(row[0]["decision_reason"].as_str().unwrap().contains("apply-failed"), "{:?}", row[0]["decision_reason"]);

        // file untouched by the failed apply
        let content = fs::read(dir.join("bin/sample.mjs")).unwrap();
        assert_eq!(content, b"old\n");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn cannot_reapply_an_already_applied_row() {
        let dir = fresh_repo("reapply");
        let c = db();
        insert_diff(&c, 1, "bin/sample.mjs", VALID_DIFF, "applied");
        let err = diff_apply_impl(&json!({ "diff_id": 1, "applied_by": "u" }), &c, Some(&dir)).unwrap_err();
        assert!(err.contains("only 'approved'"), "{err}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn not_found_and_out_of_scope_and_missing_fields() {
        let dir = fresh_repo("guards");
        let c = db();
        insert_diff(&c, 1, "src/escape.mjs", VALID_DIFF, "approved");

        assert!(diff_apply_impl(&json!({}), &c, Some(&dir)).unwrap_err().contains("diff_id is required"));
        assert!(diff_apply_impl(&json!({ "diff_id": 1 }), &c, Some(&dir)).unwrap_err().contains("applied_by is required"));
        assert!(diff_apply_impl(&json!({ "diff_id": 9999, "applied_by": "u" }), &c, Some(&dir)).unwrap_err().contains("not found"));
        assert!(diff_apply_impl(&json!({ "diff_id": 1, "applied_by": "u" }), &c, Some(&dir)).unwrap_err().contains("not under any substrate-managed scope"));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn real_git_apply_check_failure_is_wrapped_friendly() {
        // No cwd override needed: a malformed patch fails --check regardless of working directory.
        let e = realGitApply(&json!("this is not a valid unified diff at all")).unwrap_err();
        assert!(e.contains("diff does not apply cleanly"), "{e}");
    }

    #[test]
    fn diff_apply_default_entry_point_matches_impl_with_none() {
        // Exercises the public `diffApply(input, db)` entry point directly (git_dir=None): a
        // missing-field guard fires before any subprocess runs, so this is safe from any cwd.
        let c = db();
        let e = diffApply(&json!({}), &c).unwrap_err();
        assert_eq!(e, "diff-apply: diff_id is required");
    }
}
