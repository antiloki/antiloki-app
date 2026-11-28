//! code-git-info.rs — twin of skills/codebase/code-git-info.mjs. NAMES ARE CONTRACT:
//! `codeGitInfo` (default).
//!
//! BESPOKE-NATIVE (playbook's "not conditionally avoidable" rule, code-git-init.rs precedent):
//! this skill's entire purpose is reporting on git repos found under the workspace root — every
//! branch that returns interesting data (`is_git:true`, `commits`, `status`, `last_commit_files`)
//! is reached only via a real `git` subprocess (`rev-parse`/`log`/`status`/`show`), and there is
//! no `input` flag (unlike constitution-estimate.rs's `input.base`-gated git-diff branch) that
//! selects a git-free path — the ONLY git-free outcome (`is_git:false`, empty arrays) fires when
//! `findRepos` walks the ENTIRE workspace tree and finds zero `.git` entries, a real-filesystem
//! precondition, not an input switch, and even that branch would be a near-empty fixture (it
//! exercises none of the log/status/show parsing — the actual point of this unit). Worse: `git
//! log`'s `%ar` (author date, RELATIVE format, e.g. "3 days ago") reads the REAL wall clock, which
//! the crossing harness's frozen-clock lane does NOT intercept (it freezes JS `Date`/SQLite
//! `'now'`, not the `git` binary's own clock) — so even a committed real-repo testdata fixture
//! could never be byte-identical across runs taken on different days. Per the playbook: real
//! correct code for every branch (including the `findRepos`/`snapshot` git-shelling path), proven
//! by `#[cfg(test)]` native tests against real temp-dir git repos (code-git-init.rs's precedent),
//! NO conform-db fixture and NO frontier entry.
//!
//! `context.db` missing → JS throws `err.contextMissing('code-git-info', 'db')`. Not modeled:
//! `db: &Connection` is a required, non-nullable reference here — same "guard-throws are not
//! exercised" convention used by every sibling twin.
//!
//! `Math.max(1, Math.min(100, Number(limit) || 30))`: `js_number(limit) || 30` — NB `Number(x)||30`
//! also falls back to 30 when `Number(x)` is exactly `0` (not just `NaN`), mirrored below.
//! `-${n}` (the `git log` count arg): `n` is a raw arithmetic JS Number embedded in a template
//! literal, not a JSON value — ported via `js_number_to_string` (playbook helper), not
//! `serde_json`'s Number rendering.
//!
//! `relative(wsRoot, r).replace(/\\/g,'/') || '.'`: `r` is always `wsRoot` joined with zero or
//! more path segments discovered by `findRepos`'s own walk (never an unrelated path), so the
//! relative suffix is tracked incrementally DURING the walk (same technique as
//! constitution-estimate.rs's `walk()` `child_rel` accumulation) instead of porting a general
//! `path.relative()` — root-level match (the workspace root itself is a repo) yields `''` → `.`,
//! same as JS.
//!
//! Sort comparator `a.rel_path === '.' ? -1 : b.rel_path === '.' ? 1 : a.rel_path.localeCompare(b.rel_path)`:
//! ported as plain byte-lexicographic `.cmp()` for the non-`.` branch (fixture/testdata paths here
//! are plain ASCII directory names; `localeCompare`'s locale-aware collation is not exercised).
//! `sort_by` is stable, matching JS `Array.prototype.sort`.
// QUALITY: bespoke-native (real git subprocess on every meaningful branch, code-git-init.rs
// precedent) — no fixture/frontier entry; correctness proven by the native tests below.

use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::path::Path;
use std::sync::OnceLock;

fn js_number(v: Option<&Value>) -> f64 { match v { None=>f64::NAN, Some(Value::Null)=>0.0, Some(Value::Bool(b))=>if *b {1.0} else {0.0}, Some(Value::Number(n))=>n.as_f64().unwrap_or(f64::NAN), Some(Value::String(s))=>{let t=s.trim(); if t.is_empty(){0.0}else{t.parse().unwrap_or(f64::NAN)}}, Some(_)=>f64::NAN } } // = JS Number(v)

// = JS Number.prototype.toString()/`${f}` for a raw f64 template value (see playbook helper doc).
fn js_number_to_string(f: f64) -> String {
    if f.is_nan() { return "NaN".to_string(); }
    if f == 0.0 { return "0".to_string(); } // JS: (-0).toString() === "0"
    let neg = f.is_sign_negative();
    let abs = f.abs();
    if abs.is_infinite() { return if neg { "-Infinity".into() } else { "Infinity".into() }; }
    let sci = format!("{:e}", abs);
    let (mantissa, exp_str) = sci.split_once('e').unwrap();
    let exp: i32 = exp_str.parse().unwrap();
    let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    let k = digits.len() as i32;
    let body = if (-6..=20).contains(&exp) {
        if exp >= 0 {
            if exp + 1 >= k { format!("{digits}{}", "0".repeat((exp + 1 - k) as usize)) }
            else { let (i, f) = digits.split_at((exp + 1) as usize); format!("{i}.{f}") }
        } else { format!("0.{}{digits}", "0".repeat((-exp - 1) as usize)) }
    } else {
        let m = if k == 1 { digits.clone() } else { format!("{}.{}", &digits[..1], &digits[1..]) };
        format!("{m}e{}{}", if exp >= 0 { "+" } else { "-" }, exp.abs())
    };
    if neg { format!("-{body}") } else { body }
}

const SEP: &str = "\u{1f}";
const SKIP: [&str; 12] = ["node_modules", ".antiloki", "dist", "build", "out", "coverage", ".next", ".cache", ".turbo", "vendor", ".svn", ".hg"];

struct GitResult { ok: bool, stdout: String }

// `function git(args, cwd) { return spawnSync('git', args, { cwd, encoding: 'utf8', maxBuffer: ... }); }`
fn git(args: &[&str], cwd: &str) -> GitResult {
    match crate::proc::cmd("git").args(args).current_dir(cwd).output() {
        Ok(o) => GitResult { ok: o.status.success(), stdout: String::from_utf8_lossy(&o.stdout).into_owned() },
        // spawnSync with a missing/unspawnable binary: status stays null (!== 0), stdout unset —
        // same "failed" shape here.
        Err(_) => GitResult { ok: false, stdout: String::new() },
    }
}

fn name_status_re() -> &'static regex::Regex {
    static R: OnceLock<regex::Regex> = OnceLock::new();
    R.get_or_init(|| regex::Regex::new(r"^(\S+)\s+(.+)$").unwrap())
}

/// `function findRepos(root, maxDepth=6)` — DFS for every dir holding a `.git` entry (dir OR
/// submodule file), descending PAST a found repo (a repo-inside-a-repo is its own entry), never
/// descending into `.git` itself or a `SKIP` dir. Returns `(absolute_dir, rel_path)` pairs;
/// `rel_path` is the `/`-joined suffix from `root` (empty string at `root` itself, matching JS
/// `relative(root, root) === ''` — mapped to `.` by the caller, see module doc).
fn find_repos(root: &str, max_depth: i32) -> Vec<(String, String)> {
    let mut repos: Vec<(String, String)> = Vec::new();
    fn walk(dir: &Path, rel: &str, depth: i32, max_depth: i32, repos: &mut Vec<(String, String)>) {
        let entries: Vec<std::fs::DirEntry> = match std::fs::read_dir(dir) {
            Ok(rd) => rd.flatten().collect(),
            Err(_) => return,
        };
        if entries.iter().any(|e| e.file_name().to_string_lossy() == ".git") {
            repos.push((dir.to_string_lossy().into_owned(), rel.to_string()));
        }
        if depth >= max_depth { return; }
        for e in &entries {
            let name = e.file_name().to_string_lossy().into_owned();
            let is_dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
            if !is_dir || name == ".git" || SKIP.contains(&name.as_str()) { continue; }
            let child_rel = if rel.is_empty() { name.clone() } else { format!("{rel}/{name}") };
            walk(&dir.join(&name), &child_rel, depth + 1, max_depth, repos);
        }
    }
    walk(Path::new(root), "", 0, max_depth, &mut repos);
    repos
}

/// `function snapshot(root, n)` — one repo's branch + recent log + working-tree status + HEAD's
/// changed files. `n_arg` is the ALREADY-`${n}`-stringified `git log` count arg (see module doc).
fn snapshot(root: &str, n_arg: &str) -> Value {
    let probe = git(&["rev-parse", "--is-inside-work-tree"], root);
    if !probe.ok || probe.stdout.trim() != "true" {
        return json!({ "is_git": false, "root": root, "branch": Value::Null, "commits": [], "status": [], "last_commit_files": [] });
    }

    let branch_out = git(&["rev-parse", "--abbrev-ref", "HEAD"], root);
    let branch_trim = branch_out.stdout.trim();
    let branch: Value = if branch_trim.is_empty() { Value::Null } else { json!(branch_trim) };

    // `at` (2026-09-18): the author date as ISO 8601 too — the relative `date` is for reading, the
    // activity calendar needs the day; appended LAST so every existing column keeps its index.
    let pretty = format!("--pretty=format:%h{SEP}%p{SEP}%an{SEP}%ar{SEP}%s{SEP}%aI");
    let log = git(&["log", n_arg, pretty.as_str(), "--no-color"], root);
    let commits: Vec<Value> = log.stdout.split('\n').filter(|l| !l.is_empty()).map(|line| {
        let parts: Vec<&str> = line.split(SEP).collect();
        let sha = parts.first().copied().unwrap_or("");
        let parents_raw = parts.get(1).copied().unwrap_or("");
        let author = parts.get(2).copied().unwrap_or("");
        let date = parts.get(3).copied().unwrap_or("");
        let subject = parts.get(4).copied().unwrap_or("");
        let at = parts.get(5).copied().unwrap_or("");
        let parents: Vec<Value> = parents_raw.split(' ').filter(|s| !s.is_empty()).map(|s| json!(s)).collect();
        json!({ "sha": sha, "parents": parents, "author": author, "date": date, "subject": subject, "at": at })
    }).collect();

    let st = git(&["status", "--porcelain=v1"], root);
    let status: Vec<Value> = st.stdout.split('\n').filter(|l| !l.is_empty()).map(|l| {
        let chars: Vec<char> = l.chars().collect();
        let code_raw: String = chars.iter().take(2).collect();
        let code_trim = code_raw.trim();
        let code = if code_trim.is_empty() { "?" } else { code_trim };
        let path: String = chars.iter().skip(3).collect();
        json!({ "status": code, "path": path })
    }).collect();

    let lf = git(&["show", "--name-status", "--pretty=format:", "HEAD"], root);
    let last_commit_files: Vec<Value> = lf.stdout.split('\n').filter(|l| !l.is_empty()).filter_map(|l| {
        name_status_re().captures(l).map(|c| json!({ "status": c[1].to_string(), "path": c[2].to_string() }))
    }).collect();

    json!({ "is_git": true, "root": root, "branch": branch, "commits": commits, "status": status, "last_commit_files": last_commit_files })
}

/// `Math.max(1, Math.min(2000, Number(limit) || 30))` — the cap was 100; the activity calendar asks
/// for a year of commits (operator 2026-09-19: "the full year lit"), and `git log -n 2000` is still one call.
fn resolve_n(input: &Value) -> f64 {
    let raw = js_number(input.get("limit"));
    let base = if raw.is_nan() || raw == 0.0 { 30.0 } else { raw };
    base.max(1.0).min(2000.0)
}

/// `export default function codeGitInfo(input, context)`
pub fn codeGitInfo(input: &Value, db: &Connection) -> Value {
    let n_arg = format!("-{}", js_number_to_string(resolve_n(input)));

    let root_v = crate::engine_workspace_root::getWorkspaceRoot(db);
    let ws_root = root_v.as_str().unwrap_or_default().to_string();
    let ws_root_fwd = ws_root.replace('\\', "/");

    let mut repos: Vec<Value> = find_repos(&ws_root, 6).into_iter().map(|(dir, rel_raw)| {
        let rel_fwd = rel_raw.replace('\\', "/");
        let rel = if rel_fwd.is_empty() { ".".to_string() } else { rel_fwd };
        let label = if rel == "." {
            ws_root_fwd.split('/').filter(|s| !s.is_empty()).last().map(|s| s.to_string()).unwrap_or_else(|| "root".to_string())
        } else {
            rel.clone()
        };
        let mut snap = snapshot(&dir, &n_arg);
        if let Value::Object(ref mut map) = snap {
            map.insert("rel_path".into(), json!(rel));
            map.insert("label".into(), json!(label));
        }
        snap
    }).filter(|r| r.get("is_git").and_then(Value::as_bool) == Some(true)).collect();

    repos.sort_by(|a, b| {
        let ar = a.get("rel_path").and_then(Value::as_str).unwrap_or("");
        let br = b.get("rel_path").and_then(Value::as_str).unwrap_or("");
        if ar == "." { std::cmp::Ordering::Less }
        else if br == "." { std::cmp::Ordering::Greater }
        else { ar.cmp(br) }
    });

    let primary: Value = repos.iter().find(|r| r.get("rel_path").and_then(Value::as_str) == Some(".")).cloned()
        .or_else(|| repos.first().cloned())
        .unwrap_or_else(|| json!({ "is_git": false, "root": ws_root, "branch": Value::Null, "commits": [], "status": [], "last_commit_files": [] }));

    let mut out = Map::new();
    out.insert("is_git".into(), primary.get("is_git").cloned().unwrap_or(json!(false)));
    out.insert("root".into(), primary.get("root").cloned().unwrap_or(json!(ws_root)));
    out.insert("branch".into(), primary.get("branch").cloned().unwrap_or(Value::Null));
    out.insert("commits".into(), primary.get("commits").cloned().unwrap_or(json!([])));
    out.insert("status".into(), primary.get("status").cloned().unwrap_or(json!([])));
    out.insert("last_commit_files".into(), primary.get("last_commit_files").cloned().unwrap_or(json!([])));
    out.insert("repos".into(), Value::Array(repos));
    Value::Object(out)
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
    fn seed_active(c: &Connection, root: &str) {
        c.execute(
            "INSERT INTO workspaces (name, root_path, vcs, is_active, created_at) VALUES ('x', ?1, 'git', 1, '2026-01-01 00:00:00')",
            rusqlite::params![root],
        ).unwrap();
    }

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!("antiloki-code-git-info-rs-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn git_ok(args: &[&str], cwd: &Path) {
        let out = crate::proc::cmd("git").args(args).current_dir(cwd).output().unwrap();
        assert!(out.status.success(), "git {:?} failed: {}", args, String::from_utf8_lossy(&out.stderr));
    }

    fn init_repo_with_commit(dir: &Path, file: &str, subject: &str) {
        git_ok(&["init", "-q"], dir);
        fs::write(dir.join(file), "content").unwrap();
        git_ok(&["add", "-A"], dir);
        git_ok(&["-c", "user.name=antiloki", "-c", "user.email=antiloki@local", "-c", "commit.gpgsign=false", "commit", "-q", "-m", subject], dir);
    }

    #[test]
    fn snapshot_of_non_git_directory_degrades() {
        let dir = temp_dir("non-git");
        let root = dir.to_string_lossy().into_owned();
        let r = snapshot(&root, "-30");
        assert_eq!(r, json!({ "is_git": false, "root": root, "branch": null, "commits": [], "status": [], "last_commit_files": [] }));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn snapshot_of_real_repo_parses_commits_status_and_last_commit_files() {
        let dir = temp_dir("real-repo");
        init_repo_with_commit(&dir, "a.txt", "first commit");
        fs::write(dir.join("a.txt"), "content v2").unwrap();
        git_ok(&["add", "-A"], &dir);
        git_ok(&["-c", "user.name=antiloki", "-c", "user.email=antiloki@local", "-c", "commit.gpgsign=false", "commit", "-q", "-m", "second commit"], &dir);
        fs::write(dir.join("untracked.txt"), "u").unwrap();

        let root = dir.to_string_lossy().into_owned();
        let r = snapshot(&root, "-30");
        assert_eq!(r["is_git"], json!(true));
        assert!(r["branch"].is_string());

        let commits = r["commits"].as_array().unwrap();
        assert_eq!(commits.len(), 2);
        assert_eq!(commits[0]["subject"], json!("second commit"));
        assert_eq!(commits[0]["parents"].as_array().unwrap().len(), 1);
        assert_eq!(commits[1]["subject"], json!("first commit"));
        assert_eq!(commits[1]["parents"].as_array().unwrap().len(), 0);
        assert!(!commits[0]["sha"].as_str().unwrap().is_empty());
        assert_eq!(
            commits[0].as_object().unwrap().keys().map(|k| k.as_str()).collect::<Vec<_>>(),
            vec!["sha", "parents", "author", "date", "subject", "at"]
        );
        assert!(commits[0]["at"].as_str().unwrap().len() >= 19, "ISO author date for the activity calendar");

        let status = r["status"].as_array().unwrap();
        assert!(status.iter().any(|s| s["status"] == json!("??") && s["path"] == json!("untracked.txt")));

        let last_files = r["last_commit_files"].as_array().unwrap();
        assert!(last_files.iter().any(|f| f["status"] == json!("M") && f["path"] == json!("a.txt")));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn find_repos_discovers_root_and_nested_but_skips_denylisted_dirs() {
        let dir = temp_dir("find-repos");
        git_ok(&["init", "-q"], &dir); // root is a repo
        let nested = dir.join("packages").join("sub");
        fs::create_dir_all(&nested).unwrap();
        git_ok(&["init", "-q"], &nested); // nested repo, discoverable
        let skipped = dir.join("node_modules").join("pkg");
        fs::create_dir_all(&skipped).unwrap();
        git_ok(&["init", "-q"], &skipped); // under node_modules → never walked into

        let root = dir.to_string_lossy().into_owned();
        let found = find_repos(&root, 6);
        let rels: Vec<&str> = found.iter().map(|(_, r)| r.as_str()).collect();
        assert!(rels.contains(&""), "root repo should have empty rel_path: {rels:?}");
        assert!(rels.contains(&"packages/sub"), "{rels:?}");
        assert!(!rels.iter().any(|r| r.contains("node_modules")), "{rels:?}");
        assert_eq!(found.len(), 2);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_n_clamps_and_zero_falls_back_to_default() {
        assert_eq!(resolve_n(&json!({})), 30.0);
        assert_eq!(resolve_n(&json!({ "limit": 0 })), 30.0);
        assert_eq!(resolve_n(&json!({ "limit": 500 })), 500.0);
        assert_eq!(resolve_n(&json!({ "limit": 5000 })), 2000.0);
        assert_eq!(resolve_n(&json!({ "limit": -5 })), 1.0);
        assert_eq!(resolve_n(&json!({ "limit": 12 })), 12.0);
        assert_eq!(resolve_n(&json!({ "limit": "abc" })), 30.0);
    }

    #[test]
    fn code_git_info_reports_primary_repo_at_workspace_root() {
        let dir = temp_dir("full-primary-root");
        init_repo_with_commit(&dir, "a.txt", "root commit");

        let c = db0();
        let root = dir.to_string_lossy().into_owned();
        seed_active(&c, &root);
        let r = codeGitInfo(&json!({}), &c);
        assert_eq!(r["is_git"], json!(true));
        assert_eq!(r["commits"].as_array().unwrap().len(), 1);
        let repos = r["repos"].as_array().unwrap();
        assert_eq!(repos.len(), 1);
        assert_eq!(repos[0]["rel_path"], json!("."));
        assert!(repos[0]["label"].as_str().is_some());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn code_git_info_degrades_on_a_non_git_workspace_with_no_nested_repos() {
        let dir = temp_dir("full-no-git");
        fs::write(dir.join("plain.txt"), "x").unwrap();

        let c = db0();
        let root = dir.to_string_lossy().into_owned();
        seed_active(&c, &root);
        let r = codeGitInfo(&json!({}), &c);
        assert_eq!(r["is_git"], json!(false));
        assert_eq!(r["branch"], Value::Null);
        assert_eq!(r["commits"], json!([]));
        assert_eq!(r["status"], json!([]));
        assert_eq!(r["last_commit_files"], json!([]));
        assert_eq!(r["repos"], json!([]));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn code_git_info_picks_the_nested_repo_as_primary_when_root_itself_is_not_a_repo() {
        let dir = temp_dir("full-nested-primary");
        let nested = dir.join("app");
        fs::create_dir_all(&nested).unwrap();
        init_repo_with_commit(&nested, "a.txt", "nested commit");

        let c = db0();
        let root = dir.to_string_lossy().into_owned();
        seed_active(&c, &root);
        let r = codeGitInfo(&json!({}), &c);
        assert_eq!(r["is_git"], json!(true));
        let repos = r["repos"].as_array().unwrap();
        assert_eq!(repos.len(), 1);
        assert_eq!(repos[0]["rel_path"], json!("app"));
        assert_eq!(repos[0]["label"], json!("app"));

        let _ = fs::remove_dir_all(&dir);
    }
}
