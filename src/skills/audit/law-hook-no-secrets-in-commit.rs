//! law-hook-no-secrets-in-commit.rs — twin of skills/audit/law-hook-no-secrets-in-commit.mjs (db
//! lane). NAMES ARE CONTRACT: `lawHookNoSecretsInCommit` (default). Enforces
//! NEVER_COMMIT_SECRETS: fires only for `AUDIT_COMMIT_SKILLS` action names (the git-commit-card
//! callers) and scans the ACTIVE WORKSPACE's working tree (root resolved via the
//! already-converted `engine_workspace_root::getWorkspaceRoot` twin) for forbidden staged/
//! untracked paths and secret-content patterns in the staged diff.
//!
//! GIT SUBPROCESS BRANCH — DISCLOSED UNTESTED BY THE CONFORM FIXTURE (playbook lesson 5's
//! flag-skippable pattern): `action_name` gates the ENTIRE git-touching body — when it is not one
//! of the 3 `AUDIT_COMMIT_SKILLS` members, the JS returns `{ allowed: true }` before ever calling
//! `execSync`. This twin ports `getStagedPaths`/`getStagedDiff`/`getUnstagedSensitivePaths` as
//! real `git` subprocess calls (`std::process::Command`, mirroring the JS `execSync` invocations
//! verbatim: `git diff --cached --name-only`, `git diff --cached -U0`, `git status --porcelain`)
//! for production parity — but the conform-db FIXTURE only exercises the safe early-return path
//! (a non-member `action_name`), since a real git scan depends on the actual repo/working-tree
//! state, which isn't deterministically fixture-controllable the way a committed testdata dir is
//! (same class of gap as constitution-estimate.rs's `git_diff_paths`). Unlike that sibling,
//! though, this module's own `#[cfg(test)]` block DOES exercise the full git-touching path
//! natively — real temp git repos (temp-dir injection pattern, see god-driver-halt.rs) — mirroring
//! case-for-case the JS source's own real-git-repo behavior tests
//! (law-hook-no-secrets-in-commit.test.mjs), for higher confidence than the fixture alone gives.
//!
//! `context.db` missing → JS's `context?.db` optional-chain leaves `cwd` undefined (git falls
//! back to the process cwd) rather than throwing. Not modeled: `db: &Connection` is required,
//! non-nullable here — same "db is always present in the db lane" convention used by every
//! sibling twin (e.g. drift-resolve.rs's module doc).
// QUALITY: 80/100 — real git subprocess code for the whole function incl. the risky branch,
// disclosed-untested by the conform fixture (flag-skippable per lesson 5), covered instead by
// real native #[test]s against real temp git repos.

use regex::Regex;
use rusqlite::Connection;
use serde_json::{json, Value};
use std::collections::HashSet;
use std::sync::OnceLock;

const AUDIT_COMMIT_SKILLS: [&str; 3] = [
    "board-keeper.card.add",
    "board-keeper.card.update",
    "board-keeper.card.status.change",
];

// (compiled regex, JS `.source` string). The source is echoed verbatim into the hit message, so
// it must match the JS RegExp literal body byte-for-byte (same convention as
// law-hook-no-destructive-git.rs's `destructive_git_patterns`).
fn forbidden_path_patterns() -> &'static Vec<(Regex, &'static str)> {
    static P: OnceLock<Vec<(Regex, &'static str)>> = OnceLock::new();
    P.get_or_init(|| {
        let raw = [
            r"^\.env(\..+)?$",
            r"^\.antiloki\/auth\/",
            r"\.key$",
            r"\.pem$",
            r"^id_rsa(\.pub)?$",
            r"credentials\.json$",
            r"service-account.*\.json$",
        ];
        raw.iter().map(|s| (Regex::new(s).unwrap(), *s)).collect()
    })
}

fn secret_content_patterns() -> &'static Vec<(&'static str, Regex)> {
    static P: OnceLock<Vec<(&'static str, Regex)>> = OnceLock::new();
    P.get_or_init(|| {
        vec![
            ("AWS access key id", Regex::new(r"\bAKIA[0-9A-Z]{16}\b").unwrap()),
            ("OpenAI key", Regex::new(r"\bsk-[a-zA-Z0-9]{20,}\b").unwrap()),
            ("Anthropic key", Regex::new(r"\bsk-ant-[a-zA-Z0-9_-]{20,}\b").unwrap()),
            ("GitHub token", Regex::new(r"\bgh[pousr]_[A-Za-z0-9]{36,}\b").unwrap()),
            ("Slack token", Regex::new(r"\bxox[baprs]-[A-Za-z0-9-]{10,}\b").unwrap()),
            ("Stripe live key", Regex::new(r"\bsk_live_[0-9a-zA-Z]{24,}\b").unwrap()),
            ("Private key block", Regex::new(r"-----BEGIN ((RSA|EC|DSA|OPENSSH) )?PRIVATE KEY-----").unwrap()),
            ("JWT-like token", Regex::new(r"\beyJ[A-Za-z0-9_-]{20,}\.[A-Za-z0-9_-]{20,}\.[A-Za-z0-9_-]{20,}\b").unwrap()),
        ]
    })
}

/// `execSync('git diff --cached --name-only', { cwd, ... })` — real subprocess, see module doc.
fn get_staged_paths(cwd: Option<&str>) -> Result<Vec<String>, String> {
    let mut cmd = crate::proc::cmd("git");
    cmd.arg("diff").arg("--cached").arg("--name-only");
    if let Some(c) = cwd {
        cmd.current_dir(c);
    }
    let out = cmd.output().map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).into_owned());
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .split('\n')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect())
}

/// `execSync('git diff --cached -U0', { cwd, ..., maxBuffer: 32*1024*1024 })` — real subprocess,
/// see module doc. No maxBuffer cap ported: `Command::output()` has no Node-style buffer limit.
fn get_staged_diff(cwd: Option<&str>) -> Result<String, String> {
    let mut cmd = crate::proc::cmd("git");
    cmd.arg("diff").arg("--cached").arg("-U0");
    if let Some(c) = cwd {
        cmd.current_dir(c);
    }
    let out = cmd.output().map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).into_owned());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// `line.slice(3).replace(/\\/g, '/').trim().replace(/^"|"$/g, '')` — one leading AND one
/// trailing `"` stripped (not repeated collapsing, unlike Rust's `trim_matches`).
fn strip_one_quote_each_end(s: &str) -> String {
    let mut chars: Vec<char> = s.chars().collect();
    if chars.first() == Some(&'"') {
        chars.remove(0);
    }
    if chars.last() == Some(&'"') {
        chars.pop();
    }
    chars.into_iter().collect()
}

/// `execSync('git status --porcelain', { cwd, ... })` wrapped in a JS try/catch → `[]` on
/// failure — real subprocess, see module doc.
fn get_unstaged_sensitive_paths(cwd: Option<&str>) -> Vec<String> {
    let mut cmd = crate::proc::cmd("git");
    cmd.arg("status").arg("--porcelain");
    if let Some(c) = cwd {
        cmd.current_dir(c);
    }
    let out = match cmd.output() {
        Ok(o) if o.status.success() => o,
        _ => return Vec::new(),
    };
    String::from_utf8_lossy(&out.stdout)
        .split('\n')
        .filter(|s| !s.is_empty())
        .map(|line| {
            let sliced: String = line.chars().skip(3).collect();
            let slashed = sliced.replace('\\', "/");
            strip_one_quote_each_end(slashed.trim())
        })
        .collect()
}

/// `export default function lawHookNoSecretsInCommit(input, context)`
pub fn lawHookNoSecretsInCommit(input: &Value, db: &Connection) -> Result<Value, String> {
    let action_name = input.get("action_name").and_then(Value::as_str).unwrap_or("");
    if !AUDIT_COMMIT_SKILLS.contains(&action_name) {
        return Ok(json!({ "allowed": true }));
    }

    // db is always present in the db lane (see module doc) — JS's `context?.db` optional-chain
    // and its wrapping try/catch collapse to the plain happy path here.
    let root_v = crate::engine_workspace_root::getWorkspaceRoot(db);
    let cwd = root_v.as_str().map(|s| s.to_string());

    let mut hits: Vec<String> = Vec::new();

    // 1. forbidden staged paths. A git failure means "not a git repo / nothing to scan" → treat
    // as no findings for this step (staged_paths stays None, mirroring JS `stagedPaths = null`).
    let staged_paths: Option<Vec<String>> = get_staged_paths(cwd.as_deref()).ok();
    if let Some(ref sp) = staged_paths {
        for p in sp {
            for (re, source) in forbidden_path_patterns() {
                if re.is_match(p) {
                    hits.push(format!("forbidden staged path: {p} (matches {source})"));
                }
            }
        }
    }

    // 2. same check for working-tree paths (untracked secrets the AI might add next).
    let all_paths = get_unstaged_sensitive_paths(cwd.as_deref());
    let staged_set: HashSet<&str> = staged_paths
        .as_ref()
        .map(|v| v.iter().map(String::as_str).collect())
        .unwrap_or_default();
    for p in &all_paths {
        for (re, _source) in forbidden_path_patterns() {
            if re.is_match(p) && !staged_set.contains(p.as_str()) {
                hits.push(format!("forbidden file present in working tree: {p} (add to .gitignore)"));
            }
        }
    }

    // 3. content patterns in the staged diff. FAIL CLOSED: only when there IS staged content but
    // the diff can't be read do we refuse — no staged paths means nothing to scan → allow.
    if let Some(ref sp) = staged_paths {
        if !sp.is_empty() {
            let diff = match get_staged_diff(cwd.as_deref()) {
                Ok(d) => d,
                Err(e) => {
                    let msg: String = e.chars().take(80).collect();
                    return Ok(json!({
                        "allowed": false,
                        "reason": format!("secret scan could not read the staged diff ({msg}) — refusing to commit unscanned"),
                        "suggested_path": "ensure git is available in the workspace root, then retry",
                    }));
                }
            };
            for (name, re) in secret_content_patterns() {
                if let Some(m) = re.find(&diff) {
                    let matched: String = m.as_str().chars().take(20).collect();
                    hits.push(format!("{name} pattern in staged diff: {matched}\u{2026}"));
                }
            }
        }
    }

    if !hits.is_empty() {
        let n = hits.len();
        return Ok(json!({
            "allowed": false,
            "reason": format!("secret-pattern detected ({n} hit{})", if n == 1 { "" } else { "s" }),
            "hits": hits,
            "suggested_path": "unstage the secret, rotate the credential, add path to .gitignore, then retry",
        }));
    }

    Ok(json!({ "allowed": true }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE workspaces (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL UNIQUE, \
             root_path TEXT NOT NULL, vcs TEXT NOT NULL DEFAULT 'git', is_active INTEGER NOT NULL DEFAULT 0, \
             created_at TEXT NOT NULL DEFAULT '2026-01-01 00:00:00')",
        )
        .unwrap();
        c
    }
    fn seed_active(c: &Connection, root: &str) {
        c.execute("INSERT INTO workspaces (name, root_path, is_active) VALUES ('x', ?1, 1)", rusqlite::params![root]).unwrap();
    }

    fn git_repo(tag: &str) -> PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!("antiloki-law-hook-no-secrets-rs-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let run = |args: &[&str]| {
            let out = crate::proc::cmd("git").args(args).current_dir(&dir).output().unwrap();
            assert!(out.status.success(), "git {args:?} failed: {}", String::from_utf8_lossy(&out.stderr));
        };
        run(&["init", "-q"]);
        run(&["config", "user.email", "t@example.com"]);
        run(&["config", "user.name", "Test"]);
        dir
    }
    fn git_add(dir: &PathBuf, path: &str) {
        let out = crate::proc::cmd("git").args(["add", path]).current_dir(dir).output().unwrap();
        assert!(out.status.success(), "git add {path} failed: {}", String::from_utf8_lossy(&out.stderr));
    }

    #[test]
    fn non_member_action_name_short_circuits_before_any_git_or_db_touch() {
        let c = Connection::open_in_memory().unwrap(); // no workspaces table — never queried
        assert_eq!(
            lawHookNoSecretsInCommit(&json!({ "action_name": "system.other" }), &c).unwrap(),
            json!({ "allowed": true })
        );
        assert_eq!(lawHookNoSecretsInCommit(&json!({}), &c).unwrap(), json!({ "allowed": true }));
        assert_eq!(lawHookNoSecretsInCommit(&json!({ "action_name": "" }), &c).unwrap(), json!({ "allowed": true }));
    }

    #[test]
    fn clean_staged_file_allows_the_commit() {
        let dir = git_repo("clean");
        std::fs::write(dir.join("readme.md"), "hello world\n").unwrap();
        git_add(&dir, "readme.md");
        let c = db();
        seed_active(&c, dir.to_str().unwrap());
        let r = lawHookNoSecretsInCommit(&json!({ "action_name": "board-keeper.card.add" }), &c).unwrap();
        assert_eq!(r, json!({ "allowed": true }));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn forbidden_staged_dot_env_blocks_the_commit() {
        let dir = git_repo("dotenv");
        std::fs::write(dir.join(".env"), "SECRET=1\n").unwrap();
        git_add(&dir, ".env");
        let c = db();
        seed_active(&c, dir.to_str().unwrap());
        let r = lawHookNoSecretsInCommit(&json!({ "action_name": "board-keeper.card.add" }), &c).unwrap();
        assert_eq!(r["allowed"], json!(false));
        assert_eq!(r["reason"], json!("secret-pattern detected (1 hit)"));
        let hits = r["hits"].as_array().unwrap();
        assert_eq!(hits.len(), 1);
        assert!(hits[0].as_str().unwrap().starts_with(r"forbidden staged path: .env (matches ^\.env"));
        assert_eq!(r["suggested_path"], json!("unstage the secret, rotate the credential, add path to .gitignore, then retry"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn forbidden_untracked_id_rsa_blocks_even_with_nothing_staged() {
        let dir = git_repo("idrsa");
        std::fs::write(dir.join("id_rsa"), "-----BEGIN OPENSSH PRIVATE KEY-----\n").unwrap();
        let c = db();
        seed_active(&c, dir.to_str().unwrap());
        let r = lawHookNoSecretsInCommit(&json!({ "action_name": "board-keeper.card.status.change" }), &c).unwrap();
        assert_eq!(r["allowed"], json!(false));
        assert_eq!(r["hits"], json!(["forbidden file present in working tree: id_rsa (add to .gitignore)"]));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn staged_and_forbidden_path_is_reported_by_the_staged_check_only() {
        let dir = git_repo("staged-forbidden");
        std::fs::write(dir.join("id_rsa"), "private\n").unwrap();
        git_add(&dir, "id_rsa");
        let c = db();
        seed_active(&c, dir.to_str().unwrap());
        let r = lawHookNoSecretsInCommit(&json!({ "action_name": "board-keeper.card.update" }), &c).unwrap();
        assert_eq!(r["allowed"], json!(false));
        let hits = r["hits"].as_array().unwrap();
        assert_eq!(hits.len(), 1);
        assert!(hits[0].as_str().unwrap().starts_with("forbidden staged path: id_rsa"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn aws_key_in_staged_diff_blocks_the_commit() {
        let dir = git_repo("awskey");
        std::fs::write(dir.join("config.txt"), "AWS_KEY=AKIAIOSFODNN7EXAMPLE\n").unwrap();
        git_add(&dir, "config.txt");
        let c = db();
        seed_active(&c, dir.to_str().unwrap());
        let r = lawHookNoSecretsInCommit(&json!({ "action_name": "board-keeper.card.add" }), &c).unwrap();
        assert_eq!(r["allowed"], json!(false));
        assert_eq!(r["hits"], json!(["AWS access key id pattern in staged diff: AKIAIOSFODNN7EXAMPLE\u{2026}"]));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
