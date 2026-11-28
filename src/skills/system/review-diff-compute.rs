//! review-diff-compute.rs — twin of skills/system/review-diff-compute.mjs. NAMES ARE CONTRACT:
//! `reviewDiffCompute` (default), `parseUnifiedDiff` (named export).
//!
//! BESPOKE-NATIVE (playbook precedent: card-commits.rs / auth-sign.rs): every meaningful branch
//! shells out to a real `git` subprocess (`git diff --no-index` for sandbox mode, `git diff
//! base..head` for git mode) against a real repo on disk, and sandbox mode also touches real
//! files via `std::path::Path::exists`. No conform-db fixture, no frontier entry — correctness
//! is proven below via `#[cfg(test)]` against a real temp-dir git repo (mirrors card-commits.rs).
//!
//! `parseUnifiedDiff` itself IS pure/deterministic (string in, structured Value out) — it is
//! given its own native tests below exercising the parser directly, in addition to the
//! subprocess-driven end-to-end tests of `reviewDiffCompute`.
//!
//! Dependencies reused (both already-converted twins, per the crossing playbook's
//! cross-module-call convention):
//! - `crate::contract_contract_get::contractGet` (twin of skills/contract/contract-get.mjs)
//! - `crate::system_card_commits::cardCommits` (twin of skills/system/card-commits.mjs)
//!
//! REPO_ROOT: JS computes `resolve(HERE, '..', '..', '..')` from this file's own location
//! (skills/system/review-diff-compute.mjs → 3 levels up = antiloki-core's parent, i.e. the
//! substrate root). Rust has no equivalent "this source file's directory" at runtime, so this
//! twin reuses the `ANTILOKI_SUBSTRATE_ROOT`-env-override-else-`CARGO_MANIFEST_DIR`-parent
//! pattern established by card-commits.rs / sandbox-resolve-path.rs for the same substrate root.
//!
//! `input?.contract_slug` missing/empty → JS `throw err.required('review-diff-compute',
//! 'contract_slug')` → `"review-diff-compute: contract_slug is required"`. Contract not found →
//! JS `throw new AntilokiError({code:NOT_FOUND, skill:'review-diff-compute', cause:"contract
//! '<slug>' not found"})` → message `"review-diff-compute: contract '<slug>' not found"`. Both
//! modeled as `Err(String)` (error.mjs's `skill: cause` message-join convention).
//!
//! `exec_fn` (JS's injectable git runner, used by the JS unit tests to avoid a live repo) has no
//! twin equivalent here — this twin always shells out to the real `git` binary, matching the
//! bespoke-native contract (real subprocess proven by real native tests, not a fixture).
// QUALITY: bespoke-native (real git subprocess + real fs on every meaningful branch) — no
// fixture/frontier entry; correctness proven by the native tests below against a real temp-dir
// git repo.

use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};

// roots.mjs: SUBSTRATE_ROOT = resolve($ANTILOKI_SUBSTRATE_ROOT) if set+truthy, else the
// substrate root anchored at this crate's parent dir on this checkout layout (card-commits.rs /
// sandbox-resolve-path.rs precedent).
fn substrate_root() -> PathBuf {
    if let Ok(v) = std::env::var("ANTILOKI_SUBSTRATE_ROOT") {
        if !v.is_empty() {
            return PathBuf::from(v);
        }
    }
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("antiloki-core-rust has a parent dir")
        .to_path_buf()
}

// `try { return execFileSync('git', args, {...}); } catch (e) { return typeof e?.stdout ===
// 'string' ? e.stdout : ''; }` — `git diff` exits non-zero (1) when there ARE differences, which
// is not a real failure here, so ALWAYS return whatever stdout was produced. `Command::output()`
// (unlike execFileSync) does not throw/Err on a non-zero exit — it only errors on a real spawn
// failure (e.g. git missing), matching the JS catch-and-recover shape.
fn run_git(args: &[String], cwd: &Path) -> String {
    match crate::proc::cmd("git").args(args).current_dir(cwd).output() {
        Ok(o) => String::from_utf8_lossy(&o.stdout).into_owned(),
        Err(_) => String::new(),
    }
}

/// `export function parseUnifiedDiff(text)` — parses `git diff` unified output into structured
/// per-file hunks. Tracks the NEW-side line number for context/added lines; removed lines carry
/// `new_line: null`.
pub fn parseUnifiedDiff(text: &str) -> Value {
    let mut files: Vec<Map<String, Value>> = Vec::new();
    if text.is_empty() {
        return Value::Array(vec![]);
    }
    let lines: Vec<&str> = text.split('\n').collect();
    let mut cur_idx: Option<usize> = None;
    let mut new_line: i64 = 0;

    for line in &lines {
        if let Some(rest) = line.strip_prefix("diff --git ") {
            // "diff --git a/x b/y" — prefer the b/ path (new side): find " b/" then take the
            // rest of the line (JS regex ` b\/(.+)$`).
            let path = match line.find(" b/") {
                Some(i) => line[i + 3..].to_string(),
                None => rest.to_string(),
            };
            let mut m = Map::new();
            m.insert("path".into(), json!(path));
            m.insert("status".into(), json!("modified"));
            m.insert("hunks".into(), json!([]));
            files.push(m);
            cur_idx = Some(files.len() - 1);
            continue;
        }
        let Some(ci) = cur_idx else { continue };
        let cur = &mut files[ci];

        if line.starts_with("new file") {
            cur.insert("status".into(), json!("added"));
            continue;
        }
        if line.starts_with("deleted file") {
            cur.insert("status".into(), json!("deleted"));
            continue;
        }
        if line.starts_with("rename ") {
            cur.insert("status".into(), json!("renamed"));
            continue;
        }
        if let Some(rest) = line.strip_prefix("+++ ") {
            let p = rest.trim();
            if p != "/dev/null" {
                let stripped = p.strip_prefix("b/").unwrap_or(p);
                // `.replace(/^"?(.*?)"?$/, '$1')` — strip at most one leading and one trailing
                // double-quote if present.
                let stripped = stripped.strip_prefix('"').unwrap_or(stripped);
                let stripped = stripped.strip_suffix('"').unwrap_or(stripped);
                cur.insert("path".into(), json!(stripped));
            }
            continue;
        }
        if let Some(rest) = line.strip_prefix("--- ") {
            if rest.trim() == "/dev/null" {
                cur.insert("status".into(), json!("added"));
            }
            continue;
        }
        if line.starts_with("index ") || line.starts_with("similarity ") {
            continue;
        }

        if let Some(new_start) = parse_hunk_header(line) {
            new_line = new_start;
            // `line.slice(line.indexOf('@@', 2) + 2).trim()` — find the SECOND `@@` (search
            // starting at index 2, past the first), take everything after it, trimmed.
            let header = match line.get(2..).and_then(|tail| tail.find("@@")) {
                Some(off) => line[2 + off + 2..].trim().to_string(),
                None => String::new(),
            };
            let hunk = json!({ "old_start": 0, "new_start": new_start, "lines": [], "header": header });
            if let Some(Value::Array(hunks)) = cur.get_mut("hunks") {
                hunks.push(hunk);
            }
            continue;
        }

        let Some(Value::Array(hunks)) = cur.get_mut("hunks") else { continue };
        let Some(Value::Object(hunk)) = hunks.last_mut() else { continue };
        let Some(Value::Array(hlines)) = hunk.get_mut("lines") else { continue };
        let sign = line.chars().next();
        match sign {
            Some('+') => {
                hlines.push(json!({ "sign": "+", "content": &line[1..], "new_line": new_line }));
                new_line += 1;
            }
            Some('-') => {
                hlines.push(json!({ "sign": "-", "content": &line[1..], "new_line": Value::Null }));
            }
            Some(' ') => {
                hlines.push(json!({ "sign": " ", "content": &line[1..], "new_line": new_line }));
                new_line += 1;
            }
            _ => {} // '\' (No newline at end of file) and anything else: ignore.
        }
    }

    Value::Array(files.into_iter().map(Value::Object).collect())
}

// `/^@@ -\d+(?:,\d+)? \+(\d+)(?:,\d+)? @@/` — returns the captured NEW-side start line.
fn parse_hunk_header(line: &str) -> Option<i64> {
    let rest = line.strip_prefix("@@ -")?;
    let after_old = rest.find(' ')?;
    let plus_part = &rest[after_old + 1..];
    let plus_part = plus_part.strip_prefix('+')?;
    let digits_end = plus_part.find(|c: char| !c.is_ascii_digit()).unwrap_or(plus_part.len());
    if digits_end == 0 {
        return None;
    }
    let tail = &plus_part[digits_end..];
    // must be followed by (,\d+)? then " @@"
    let after_comma = if let Some(t) = tail.strip_prefix(',') {
        let ce = t.find(|c: char| !c.is_ascii_digit()).unwrap_or(t.len());
        if ce == 0 { return None; }
        &t[ce..]
    } else {
        tail
    };
    if !after_comma.starts_with(" @@") {
        return None;
    }
    plus_part[..digits_end].parse::<i64>().ok()
}

fn sandbox_diff(root: &Path, sandbox_path: &str, files_touched: &[String]) -> Vec<Value> {
    let mut out = Vec::new();
    for rel in files_touched {
        let orig = root.join(rel);
        let sb = root.join(sandbox_path).join(rel);
        if !sb.exists() {
            continue; // sandbox never wrote this file
        }
        let old_arg = if orig.exists() {
            orig.to_string_lossy().into_owned()
        } else {
            "/dev/null".to_string()
        };
        let patch = run_git(
            &[
                "diff".into(),
                "--no-index".into(),
                "--".into(),
                old_arg,
                sb.to_string_lossy().into_owned(),
            ],
            root,
        );
        if let Value::Array(arr) = parseUnifiedDiff(&patch) {
            for mut f in arr {
                if let Value::Object(ref mut m) = f {
                    m.insert("path".into(), json!(rel));
                }
                out.push(f);
            }
        }
    }
    out
}

/// `export default async function reviewDiffCompute(input, context)` — async ignored per the
/// crossing playbook (await over sync SQLite reads + a sync subprocess call resolves to plain
/// sync Rust).
pub fn reviewDiffCompute(input: &Value, db: &Connection) -> Result<Value, String> {
    let contract_slug = match input {
        Value::String(s) => Some(s.clone()),
        Value::Object(_) => input.get("contract_slug").and_then(Value::as_str).map(String::from),
        _ => None,
    };
    let contract_slug = match contract_slug.filter(|s| !s.is_empty()) {
        Some(s) => s,
        None => return Err("review-diff-compute: contract_slug is required".into()),
    };

    let contract = crate::contract_contract_get::contractGet(&json!(contract_slug), db)?;
    if contract.is_null() {
        return Err(format!("review-diff-compute: contract '{contract_slug}' not found"));
    }

    let files_touched: Vec<String> = contract
        .get("files_touched")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
        .unwrap_or_default();

    let root = substrate_root();
    let sandbox_path = contract
        .get("sandbox_path")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty());
    let sandbox_usable = sandbox_path.map_or(false, |sp| root.join(sp).exists()) && !files_touched.is_empty();

    if sandbox_usable {
        let sp = sandbox_path.unwrap();
        let base_sha = contract.get("base_sha").cloned().unwrap_or(Value::Null);
        return Ok(json!({
            "diff_source": "sandbox",
            "base_sha": base_sha,
            "head_sha": Value::Null,
            "files": sandbox_diff(&root, sp, &files_touched),
        }));
    }

    // git mode — resolve head_sha from the card's latest commit.
    let mut head_sha = input.get("head_sha").and_then(Value::as_str).map(String::from);
    if head_sha.is_none() {
        if let Some(card_slug) = contract.get("card_slug").and_then(Value::as_str) {
            if let Ok(commits) =
                crate::system_card_commits::cardCommits(&json!({ "slug": card_slug, "limit": 1 }), db)
            {
                head_sha = commits
                    .as_array()
                    .and_then(|a| a.first())
                    .and_then(|c| c.get("sha"))
                    .and_then(Value::as_str)
                    .map(String::from);
            }
        }
    }
    let head_sha = head_sha.unwrap_or_else(|| "HEAD".to_string());
    let base_sha = input
        .get("base_sha")
        .and_then(Value::as_str)
        .map(String::from)
        .or_else(|| contract.get("base_sha").and_then(Value::as_str).map(String::from))
        .unwrap_or_else(|| format!("{head_sha}~1"));

    let mut args = vec!["diff".to_string(), format!("{base_sha}..{head_sha}"), "--".to_string()];
    for f in &files_touched {
        args.push(f.clone());
    }
    let diff_args = if args.len() > 3 {
        args
    } else {
        vec!["diff".to_string(), format!("{base_sha}..{head_sha}")]
    };
    let patch = run_git(&diff_args, &root);

    Ok(json!({
        "diff_source": "git",
        "base_sha": base_sha,
        "head_sha": head_sha,
        "files": parseUnifiedDiff(&patch),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    // SAFETY: ANTILOKI_SUBSTRATE_ROOT is process-global; shared crate-wide lock, same convention
    // as card-commits.rs / auth-sign.rs (never a local mutex — it wouldn't stop a race with
    // OTHER files' tests over the same real env var).
    fn with_substrate_root<T>(root: &str, f: impl FnOnce() -> T) -> T {
        let _guard = crate::test_env_lock::ENV_LOCK.lock().unwrap();
        unsafe {
            std::env::set_var("ANTILOKI_SUBSTRATE_ROOT", root);
        }
        let result = f();
        unsafe {
            std::env::remove_var("ANTILOKI_SUBSTRATE_ROOT");
        }
        result
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!("antiloki-review-diff-compute-rs-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn git(args: &[&str], cwd: &Path) {
        let status = crate::proc::cmd("git").args(args).current_dir(cwd).status().unwrap();
        assert!(status.success(), "git {args:?} failed in {cwd:?}");
    }

    fn init_repo(dir: &Path) {
        git(&["init", "-q"], dir);
        git(&["config", "user.email", "test@example.com"], dir);
        git(&["config", "user.name", "Test"], dir);
    }

    fn commit_file(dir: &Path, file: &str, contents: &str, message: &str) {
        if let Some(parent) = Path::new(file).parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(dir.join(parent)).unwrap();
            }
        }
        fs::write(dir.join(file), contents).unwrap();
        git(&["add", "-A"], dir);
        git(&["commit", "-m", message], dir);
    }

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE contracts (slug TEXT, card_slug TEXT, base_sha TEXT, sandbox_path TEXT,
                files_touched_json TEXT, acceptance_json TEXT, naming_json TEXT, edge_cases_json TEXT,
                must_obey_json TEXT, out_of_scope_json TEXT, reference_artifacts_json TEXT, generated_paths_json TEXT);",
        )
        .unwrap();
        c
    }

    // --- parseUnifiedDiff (pure parser) --------------------------------------------------

    #[test]
    fn parse_unified_diff_empty_text_returns_empty_array() {
        assert_eq!(parseUnifiedDiff(""), json!([]));
    }

    #[test]
    fn parse_unified_diff_tracks_new_line_numbers_and_signs() {
        let text = "diff --git a/foo.txt b/foo.txt\nindex 111..222 100644\n--- a/foo.txt\n+++ b/foo.txt\n@@ -1,3 +1,4 @@ header text\n unchanged\n-removed\n+added1\n+added2\n";
        let r = parseUnifiedDiff(text);
        let files = r.as_array().unwrap();
        assert_eq!(files.len(), 1);
        let f = &files[0];
        assert_eq!(f["path"], json!("foo.txt"));
        assert_eq!(f["status"], json!("modified"));
        let hunks = f["hunks"].as_array().unwrap();
        assert_eq!(hunks.len(), 1);
        let h = &hunks[0];
        assert_eq!(h["new_start"], json!(1));
        assert_eq!(h["header"], json!("header text"));
        let lines = h["lines"].as_array().unwrap();
        assert_eq!(lines.len(), 4);
        assert_eq!(lines[0], json!({ "sign": " ", "content": "unchanged", "new_line": 1 }));
        assert_eq!(lines[1], json!({ "sign": "-", "content": "removed", "new_line": Value::Null }));
        assert_eq!(lines[2], json!({ "sign": "+", "content": "added1", "new_line": 2 }));
        assert_eq!(lines[3], json!({ "sign": "+", "content": "added2", "new_line": 3 }));
    }

    #[test]
    fn parse_unified_diff_new_file_and_deleted_file_status() {
        let added = "diff --git a/x b/x\nnew file mode 100644\nindex 0000000..111\n--- /dev/null\n+++ b/x\n@@ -0,0 +1,1 @@\n+hi\n";
        let r = parseUnifiedDiff(added);
        assert_eq!(r[0]["status"], json!("added"));
        assert_eq!(r[0]["path"], json!("x"));

        let deleted = "diff --git a/y b/y\ndeleted file mode 100644\nindex 111..0000000\n--- a/y\n+++ /dev/null\n@@ -1,1 +0,0 @@\n-bye\n";
        let r2 = parseUnifiedDiff(deleted);
        assert_eq!(r2[0]["status"], json!("deleted"));
    }

    #[test]
    fn parse_unified_diff_multiple_files() {
        let text = "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -1,1 +1,1 @@\n-old\n+new\ndiff --git a/b.txt b/b.txt\n--- a/b.txt\n+++ b/b.txt\n@@ -1,1 +1,1 @@\n-x\n+y\n";
        let r = parseUnifiedDiff(text);
        let files = r.as_array().unwrap();
        assert_eq!(files.len(), 2);
        assert_eq!(files[0]["path"], json!("a.txt"));
        assert_eq!(files[1]["path"], json!("b.txt"));
    }

    // --- reviewDiffCompute (real subprocess + fs, git mode) ------------------------------

    #[test]
    fn missing_contract_slug_returns_exact_error() {
        let e = reviewDiffCompute(&json!({}), &db()).unwrap_err();
        assert_eq!(e, "review-diff-compute: contract_slug is required");
    }

    #[test]
    fn contract_not_found_returns_exact_error() {
        let e = reviewDiffCompute(&json!({ "contract_slug": "GHOST" }), &db()).unwrap_err();
        assert_eq!(e, "review-diff-compute: contract 'GHOST' not found");
    }

    #[test]
    fn contract_slug_as_bare_string_input_works() {
        let e = reviewDiffCompute(&json!("GHOST"), &db()).unwrap_err();
        assert_eq!(e, "review-diff-compute: contract 'GHOST' not found");
    }

    #[test]
    fn git_mode_diffs_two_commits_for_files_touched() {
        let dir = temp_dir("git-mode");
        init_repo(&dir);
        commit_file(&dir, "src/foo.txt", "line1\nline2\n", "base commit");
        let base_out = crate::proc::cmd("git").args(["rev-parse", "HEAD"]).current_dir(&dir).output().unwrap();
        let base_sha = String::from_utf8_lossy(&base_out.stdout).trim().to_string();
        commit_file(&dir, "src/foo.txt", "line1\nline2-changed\n", "head commit");
        let head_out = crate::proc::cmd("git").args(["rev-parse", "HEAD"]).current_dir(&dir).output().unwrap();
        let head_sha = String::from_utf8_lossy(&head_out.stdout).trim().to_string();

        let conn = db();
        conn.execute(
            "INSERT INTO contracts (slug, base_sha, files_touched_json, sandbox_path) VALUES ('C1', ?1, '[\"src/foo.txt\"]', NULL)",
            [&base_sha],
        )
        .unwrap();

        let root = dir.to_string_lossy().into_owned();
        let r = with_substrate_root(&root, || {
            reviewDiffCompute(&json!({ "contract_slug": "C1", "head_sha": head_sha }), &conn).unwrap()
        });
        assert_eq!(r["diff_source"], json!("git"));
        assert_eq!(r["base_sha"], json!(base_sha));
        assert_eq!(r["head_sha"], json!(head_sha));
        let files = r["files"].as_array().unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0]["path"], json!("src/foo.txt"));
        assert_eq!(files[0]["status"], json!("modified"));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn sandbox_mode_diffs_live_file_against_sandbox_copy() {
        let dir = temp_dir("sandbox-mode");
        init_repo(&dir);
        commit_file(&dir, "src/bar.txt", "original\n", "base commit");

        let sandbox_dir = dir.join(".sandbox");
        fs::create_dir_all(sandbox_dir.join("src")).unwrap();
        fs::write(sandbox_dir.join("src/bar.txt"), "changed in sandbox\n").unwrap();

        let conn = db();
        conn.execute(
            "INSERT INTO contracts (slug, sandbox_path, files_touched_json) VALUES ('C2', '.sandbox', '[\"src/bar.txt\"]')",
            [],
        )
        .unwrap();

        let root = dir.to_string_lossy().into_owned();
        let r = with_substrate_root(&root, || {
            reviewDiffCompute(&json!({ "contract_slug": "C2" }), &conn).unwrap()
        });
        assert_eq!(r["diff_source"], json!("sandbox"));
        assert_eq!(r["head_sha"], Value::Null);
        let files = r["files"].as_array().unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0]["path"], json!("src/bar.txt"));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn sandbox_mode_skips_files_the_sandbox_never_wrote() {
        let dir = temp_dir("sandbox-skip");
        init_repo(&dir);
        commit_file(&dir, "src/base_only.txt", "1\n", "base commit");
        let base_out = crate::proc::cmd("git").args(["rev-parse", "HEAD"]).current_dir(&dir).output().unwrap();
        let base_sha = String::from_utf8_lossy(&base_out.stdout).trim().to_string();

        // sandbox_path points at a dir that exists but never wrote the touched file — sandbox
        // is "usable" (dir exists) yet sandbox_diff finds nothing for this one file.
        let sandbox_dir = dir.join(".sandbox");
        fs::create_dir_all(&sandbox_dir).unwrap();

        let conn = db();
        conn.execute(
            "INSERT INTO contracts (slug, base_sha, sandbox_path, files_touched_json) VALUES ('C3', ?1, '.sandbox', '[\"src/base_only.txt\"]')",
            [&base_sha],
        )
        .unwrap();

        let root = dir.to_string_lossy().into_owned();
        let r = with_substrate_root(&root, || {
            reviewDiffCompute(&json!({ "contract_slug": "C3" }), &conn).unwrap()
        });
        assert_eq!(r["diff_source"], json!("sandbox"));
        assert_eq!(r["files"].as_array().unwrap().len(), 0);

        let _ = fs::remove_dir_all(&dir);
    }
}
