//! card-commits.rs — twin of skills/system/card-commits.mjs. NAMES ARE CONTRACT: `cardCommits`
//! (default). Two deduped git-log probes for a card slug: probe 1 matches commit MESSAGES
//! (`git log --grep=<safe>`), probe 2 matches CHANGED PATHS containing the slug as a substring
//! (`git log -- "*<safe>*"`). Read-only.
//!
//! BESPOKE-NATIVE (playbook's "not conditionally avoidable" rule, code-git-init.rs precedent):
//! the ONLY git-free success path is `safe === ''` — the sanitized slug ends up empty (e.g. an
//! all-lowercase or all-punctuation input, since the JS regex `/[^A-Z0-9_-]/g` is
//! case-SENSITIVE and strips lowercase letters too). Real callers always pass an UPPERCASE card
//! slug (`FEAT-...`/`FIX-...`/etc — see CARD_BODY_MUST_MATCH_SCHEMA's canonical slugs), so that
//! branch is a degenerate edge case, not the skill's actual purpose — EVERY meaningful success
//! path shells out to `git log` twice, unconditionally. Per the playbook: real correct code for
//! every branch, proven by `#[cfg(test)]` native tests against a real temp-dir git repo, NO
//! conform-db fixture and NO frontier entry.
//!
//! `execSync(cmd, {...})` in JS builds ONE shell-interpreted command string (`--grep="${safe}"`,
//! `--format="${FORMAT}"`) — this twin instead calls `git` via `std::process::Command` with an
//! explicit args array (same semantic result once the shell's own quote-stripping is accounted
//! for: `--grep="X"` / `--format="Y"` become the literal single tokens `--grep=X` /
//! `--format=Y` after the shell consumes the quotes), avoiding a real shell-injection surface —
//! same choice code-git-init.rs made for its `spawnSync`-based git calls, applied here to an
//! `execSync`-based JS source instead.
//!
//! `slug` missing/falsy → JS `throw err.required('card-commits', 'slug')` →
//! `"card-commits: slug is required"`. Modeled as `Err(String)`.
//!
//! `Math.min(Math.max(1, Number(limit) || 50), 200)`: real callers always pass an integer
//! `limit` (or omit it), so this twin truncates a fractional result to `i64` before using it as
//! the git `-n` count / row cap — a disclosed narrowing (the JS would keep the literal
//! fractional value in the `-n <cap>` shell token, which `git` would itself reject as an
//! invalid `-n` argument in practice, so no real caller reaches that shape either).
//!
//! `rows.sort((a, b) => (b.ts ?? '').localeCompare(a.ts ?? ''))`: `ts` is always a defined `%aI`
//! (strict ISO-8601 author date) string here, never null/undefined, so the `?? ''` fallback
//! never fires. Modeled as a plain descending byte-string compare (`bt.cmp(at)`) — ISO-8601
//! timestamps are all-ASCII digits/`-`/`:`/`T`/`Z`/`+`, whose default-locale `localeCompare`
//! ordering coincides with ordinal byte order (gotcha #8's numeric-desc convention, applied to
//! a string field here).
// QUALITY: bespoke-native (real git subprocess on every meaningful branch) — no fixture/frontier
// entry; correctness proven by the native tests below against a real temp-dir git repo.

use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

fn js_string(v: &Value) -> String { match v { Value::Null=>"null".into(), Value::String(s)=>s.clone(), Value::Number(n)=>n.to_string(), Value::Bool(b)=>b.to_string(), Value::Array(a)=>a.iter().map(js_string).collect::<Vec<_>>().join(","), Value::Object(_)=>"[object Object]".into() } } // = JS String(v)
fn js_number(v: Option<&Value>) -> f64 { match v { None=>f64::NAN, Some(Value::Null)=>0.0, Some(Value::Bool(b))=>if *b {1.0} else {0.0}, Some(Value::Number(n))=>n.as_f64().unwrap_or(f64::NAN), Some(Value::String(s))=>{let t=s.trim(); if t.is_empty(){0.0}else{t.parse().unwrap_or(f64::NAN)}}, Some(_)=>f64::NAN } } // = JS Number(v)
fn truthy(v: Option<&Value>) -> bool { match v { None|Some(Value::Null)=>false, Some(Value::Bool(b))=>*b, Some(Value::Number(n))=>n.as_f64().map_or(false,|f|f!=0.0&&!f.is_nan()), Some(Value::String(s))=>!s.is_empty(), Some(_)=>true } }

// `%H%x1f%h%x1f%an%x1f%aI%x1f%s` — \x1f (unit separator) is safer than spaces/pipes inside
// subjects; written here as the literal Rust char escape for the same byte.
const FORMAT: &str = "%H\x1f%h\x1f%an\x1f%aI\x1f%s";

// roots.mjs: SUBSTRATE_ROOT = resolve($ANTILOKI_SUBSTRATE_ROOT) if set+truthy, else
// resolve(ENGINE_ROOT, '..') anchored at roots.mjs's own file location (antiloki-core's parent).
// CARGO_MANIFEST_DIR here is antiloki-core-rust; its parent is that same SUBSTRATE_ROOT on this
// checkout layout (auth-init.rs / file-read.rs / sandbox-resolve-path.rs precedent).
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

// `function run(cmd) { try { return execSync(cmd, {...}); } catch { return ''; } }` — any spawn
// failure or non-zero exit collapses to ''.
fn run_git(args: &[String], cwd: &Path) -> String {
    match crate::proc::cmd("git").args(args).current_dir(cwd).output() {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).into_owned(),
        _ => String::new(),
    }
}

/// `export default function cardCommits(input, _context)`
pub fn cardCommits(input: &Value, _db: &Connection) -> Result<Value, String> {
    let slug = match input.get("slug").filter(|v| truthy(Some(v))) {
        Some(v) => v,
        None => return Err("card-commits: slug is required".into()),
    };
    // `String(slug).replace(/[^A-Z0-9_-]/g, '')` — case-SENSITIVE: lowercase letters are
    // stripped too, same as digits/underscore/dash being kept and everything else dropped.
    let safe: String = js_string(slug)
        .chars()
        .filter(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || *c == '_' || *c == '-')
        .collect();
    if safe.is_empty() {
        return Ok(Value::Array(vec![]));
    }

    let n = js_number(input.get("limit"));
    let base = if n != 0.0 && !n.is_nan() { n } else { 50.0 };
    let cap_i: i64 = base.max(1.0).min(200.0) as i64;
    let cap_s = cap_i.to_string();

    let root = substrate_root();

    // Probe 1: commit message matches the slug.
    let out1 = run_git(
        &[
            "log".into(),
            "--all".into(),
            "--no-merges".into(),
            format!("--grep={safe}"),
            "-n".into(),
            cap_s.clone(),
            format!("--format={FORMAT}"),
        ],
        &root,
    );
    // Probe 2: any commit that touched a path containing the slug.
    let out2 = run_git(
        &[
            "log".into(),
            "--all".into(),
            "--no-merges".into(),
            "-n".into(),
            cap_s,
            format!("--format={FORMAT}"),
            "--".into(),
            format!("*{safe}*"),
        ],
        &root,
    );

    let mut seen: HashSet<String> = HashSet::new();
    let mut rows: Vec<Value> = Vec::new();
    'outer: for block in [&out1, &out2] {
        for line in block.split('\n') {
            if line.trim().is_empty() {
                continue;
            }
            let parts: Vec<&str> = line.split('\u{1f}').collect();
            if parts.len() < 5 {
                continue;
            }
            let sha = parts[0];
            if !seen.insert(sha.to_string()) {
                continue;
            }
            let mut row = Map::new();
            row.insert("sha".into(), json!(sha));
            row.insert("short_sha".into(), json!(parts[1]));
            row.insert("author_name".into(), json!(parts[2]));
            row.insert("ts".into(), json!(parts[3]));
            row.insert("subject".into(), json!(parts[4]));
            rows.push(Value::Object(row));
            if rows.len() as i64 >= cap_i {
                break 'outer;
            }
        }
    }

    // Most recent first.
    rows.sort_by(|a, b| {
        let at = a.get("ts").and_then(Value::as_str).unwrap_or("");
        let bt = b.get("ts").and_then(Value::as_str).unwrap_or("");
        bt.cmp(at)
    });

    Ok(Value::Array(rows))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    // SAFETY: ANTILOKI_SUBSTRATE_ROOT is a process-global env var read by many twins' tests
    // across the crate — shared crate-wide lock (test_env_lock.rs), a local mutex would not
    // prevent a race with OTHER files' tests over the same real env var.
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
        dir.push(format!("antiloki-card-commits-rs-{tag}-{}", std::process::id()));
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

    fn commit(dir: &Path, file: &str, contents: &str, message: &str, date: &str) {
        if let Some(parent) = Path::new(file).parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(dir.join(parent)).unwrap();
            }
        }
        fs::write(dir.join(file), contents).unwrap();
        git(&["add", "-A"], dir);
        let status = crate::proc::cmd("git")
            .args(["commit", "-m", message])
            .current_dir(dir)
            .env("GIT_AUTHOR_DATE", date)
            .env("GIT_COMMITTER_DATE", date)
            .status()
            .unwrap();
        assert!(status.success(), "commit '{message}' failed");
    }

    fn db() -> Connection {
        Connection::open_in_memory().unwrap()
    }

    #[test]
    fn missing_slug_throws() {
        assert_eq!(cardCommits(&json!({}), &db()), Err("card-commits: slug is required".to_string()));
        assert_eq!(cardCommits(&json!({ "slug": "" }), &db()), Err("card-commits: slug is required".to_string()));
    }

    #[test]
    fn slug_that_sanitizes_to_empty_returns_empty_array_without_touching_git() {
        // lowercase + punctuation only: every char is stripped by the [^A-Z0-9_-] filter.
        let r = cardCommits(&json!({ "slug": "abc!!!" }), &db()).unwrap();
        assert_eq!(r, json!([]));
    }

    #[test]
    fn message_and_path_probes_dedup_and_sort_most_recent_first() {
        let dir = temp_dir("probes");
        init_repo(&dir);
        commit(&dir, "a.txt", "1", "FEAT-CARD-TEST-1: initial commit", "2020-01-01T00:00:01+00:00");
        // matches via PATH probe only (slug appears in a path component, not the message)
        commit(&dir, "src/FEAT-CARD-TEST-1/module.txt", "2", "unrelated path-only commit", "2020-01-02T00:00:01+00:00");
        // matches via MESSAGE probe only
        commit(&dir, "b.txt", "3", "touches FEAT-CARD-TEST-1 in prose", "2020-01-03T00:00:01+00:00");
        // does not match at all
        commit(&dir, "c.txt", "4", "OTHER-CARD unrelated", "2020-01-04T00:00:01+00:00");

        let root = dir.to_string_lossy().into_owned();
        let r = with_substrate_root(&root, || cardCommits(&json!({ "slug": "FEAT-CARD-TEST-1" }), &db()).unwrap());
        let arr = r.as_array().unwrap();
        assert_eq!(arr.len(), 3, "the 3 matching commits, deduped, the non-matching 4th excluded: {r}");

        let subjects: Vec<&str> = arr.iter().map(|row| row["subject"].as_str().unwrap()).collect();
        // most recent first
        assert_eq!(
            subjects,
            vec!["touches FEAT-CARD-TEST-1 in prose", "unrelated path-only commit", "FEAT-CARD-TEST-1: initial commit"]
        );

        let keys: Vec<&String> = arr[0].as_object().unwrap().keys().collect();
        assert_eq!(keys, ["sha", "short_sha", "author_name", "ts", "subject"]);
        assert_eq!(arr[0]["author_name"], json!("Test"));
        let sha = arr[0]["sha"].as_str().unwrap();
        let short = arr[0]["short_sha"].as_str().unwrap();
        assert_eq!(sha.len(), 40);
        assert!(sha.starts_with(short), "short_sha {short} should prefix sha {sha}");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn limit_zero_falls_back_to_default_50_since_zero_is_falsy() {
        let dir = temp_dir("limit-zero");
        init_repo(&dir);
        commit(&dir, "a.txt", "1", "FEAT-CLAMP-1 oldest", "2020-01-01T00:00:01+00:00");
        commit(&dir, "b.txt", "2", "FEAT-CLAMP-1 newest", "2020-01-02T00:00:01+00:00");

        let root = dir.to_string_lossy().into_owned();
        // JS: Number(0) || 50 === 50 (0 is falsy) — cap=50 never binds against 2 real matches.
        let r = with_substrate_root(&root, || cardCommits(&json!({ "slug": "FEAT-CLAMP-1", "limit": 0 }), &db()).unwrap());
        let arr = r.as_array().unwrap();
        assert_eq!(arr.len(), 2);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn negative_limit_is_truthy_then_clamps_up_to_one_row() {
        let dir = temp_dir("limit-negative");
        init_repo(&dir);
        commit(&dir, "a.txt", "1", "FEAT-CLAMP-1 oldest", "2020-01-01T00:00:01+00:00");
        commit(&dir, "b.txt", "2", "FEAT-CLAMP-1 newest", "2020-01-02T00:00:01+00:00");

        let root = dir.to_string_lossy().into_owned();
        // JS: Number(-5) || 50 === -5 (truthy, nonzero) then Math.max(1, -5) === 1.
        let r = with_substrate_root(&root, || cardCommits(&json!({ "slug": "FEAT-CLAMP-1", "limit": -5 }), &db()).unwrap());
        let arr = r.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["subject"], json!("FEAT-CLAMP-1 newest"));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn no_matching_commits_returns_empty_array() {
        let dir = temp_dir("no-match");
        init_repo(&dir);
        commit(&dir, "a.txt", "1", "OTHER-CARD unrelated", "2020-01-01T00:00:01+00:00");

        let root = dir.to_string_lossy().into_owned();
        let r = with_substrate_root(&root, || cardCommits(&json!({ "slug": "FEAT-NOPE" }), &db()).unwrap());
        assert_eq!(r, json!([]));

        let _ = fs::remove_dir_all(&dir);
    }
}
