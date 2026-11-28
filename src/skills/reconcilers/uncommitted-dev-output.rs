//! uncommitted-dev-output.rs — twin of skills/reconcilers/uncommitted-dev-output.mjs
//! (reconciler-object lane). NAMES ARE CONTRACT: the units are the default object's METHODS
//! `detect(db)` and `fix(finding, db)`.
//!
//! Drift pattern: the autonomous pipeline lands files in the working tree without `git add
//! + git commit`, which trips the COMMIT_CODE_BEFORE_AUDIT_COMMIT law-hook. `detect()` finds
//! drifted (uncommitted) working-tree files that some recent contract's `files_touched_json`
//! claims to have authored; `fix()` `git add`s + `git commit`s the file, attributing the
//! commit to that contract.
//!
//! BESPOKE-NATIVE (playbook's "not conditionally avoidable" rule, code-git-info.rs /
//! card-commits.rs precedent): `detect()` unconditionally shells out to `git status
//! --porcelain` on every call (there is no input flag that selects a git-free path), and
//! `fix()`'s only non-trivial branch unconditionally shells out to `git add` + `git commit`.
//! Per the playbook: real correct code for every branch, proven by `#[cfg(test)]` native
//! tests against a real temp-dir git repo, NO conform-db fixture and NO frontier entry.
//!
//! `REPO_ROOT = resolve(HERE, '..', '..', '..')` (JS `HERE` = this file's own
//! `skills/reconcilers/` dir via `import.meta.url`) walks up 3 levels — `skills`,
//! `antiloki-core`, then antiloki-core's PARENT directory — with NO env-var override
//! anywhere in the JS (unlike card-commits.mjs's `roots.mjs`-derived `SUBSTRATE_ROOT`, which
//! DOES respect `ANTILOKI_SUBSTRATE_ROOT`). On this checkout layout that parent directory is
//! `CARGO_MANIFEST_DIR`'s parent too (antiloki-core-rust sits alongside antiloki-core), so
//! `repo_root()` below defaults identically to the JS's hardcoded path. `ANTILOKI_SUBSTRATE_ROOT`
//! is read ONLY as a test-injection seam (same env var name card-commits.rs already uses for
//! this same real directory) so native tests can point at an isolated temp git repo instead of
//! mutating the real one — this lever does not exist in the JS and never fires in production
//! (nothing sets that var to redirect THIS module; it just happens to default to the same real
//! path the JS's hardcoded computation lands on).
//!
//! `gitStatus()`: `execSync('git status --porcelain', {cwd, encoding:'utf8'})` inside a
//! try/catch that collapses ANY failure (not a repo, git missing, non-zero exit) to `[]`.
//! Each non-empty TRIMMED line is split on `/\s+/` into `[code, ...rest]`, `path =
//! rest.join(' ')` — ported as `split_whitespace()` on the already-trimmed line (no leading
//! whitespace remains post-trim, so it matches `split(/\s+/)` exactly here).
//!
//! `getContractFilesForPath(db, relPath)`: the WHOLE query is inside one try/catch (table
//! absent etc. → `[]`); each row's `files_touched_json` is `JSON.parse`d in its OWN inner
//! try/catch (`catch { return false }` — a malformed-JSON row is excluded, not a hard
//! failure) and kept only if the parsed value is an array containing `relPath` by strict
//! equality. Only `.slug` is ever read off the surviving rows downstream, so this twin
//! returns `Vec<String>` of slugs directly rather than full row objects.
//!
//! `fix(finding, db)`: JS declares `db` but never reads it in the body — `_db` here for the
//! same reason. `execSync(cmd, {...})` builds ONE shell-interpreted command string
//! (`git add "${path}"`, then `git commit -m "${subject}" -m "${body.replace(/"/g,
//! '\\"')}"` — the `\"` escaping exists only to survive the SHELL's own quote parsing); this
//! twin instead calls `git` via `std::process::Command` with an explicit args array, so the
//! subject/body strings are passed as literal argv entries with no shell layer to escape for
//! (same choice code-git-info.rs / card-commits.rs made). The exact text of a THROWN error's
//! `.message` (`e?.message?.slice(0,200) ?? String(e)`) is not reproducible byte-for-byte
//! across Node's execSync vs Rust's `Command` (different underlying error surfaces) and isn't
//! judged by any fixture here (bespoke-native, no cross-lane comparison) — this twin returns
//! a real, non-empty diagnostic string (stderr, truncated to 200 chars) on failure, matching
//! the JS's *shape* (`{ applied: false, reason: <string> }`) and its trigger conditions
//! exactly, not its exact wording.
//!
//! `body` is a JS template literal containing LITERAL embedded newlines — the source file is
//! CRLF on disk, so those newlines are `\r\n` bytes in the actual runtime string; this twin
//! embeds `\r\n` for fidelity even though no fixture compares it byte-for-byte.
//!
//! `contracts[0] ?? 'pipeline'` / `contracts.join(', ')`: `contracts` here is
//! `finding.detail?.contracts ?? []`, always an array of slug STRINGS in real usage (produced
//! by this module's own `detect()`), so this twin renders it as such; a malformed non-array
//! `detail.contracts` (would throw `TypeError: contracts.join is not a function` in JS,
//! caught by `fix()`'s own try/catch) is not modeled — not a shape `detect()` ever produces.

use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};

fn tmpl(v: Option<&Value>) -> String { match v { None=>"undefined".into(), Some(Value::Null)=>"null".into(), Some(x)=>str_qq(Some(x)) } } // = JS `${v}`
fn str_qq(v: Option<&Value>) -> String { match v { None|Some(Value::Null)=>String::new(), Some(Value::String(s))=>s.clone(), Some(Value::Bool(b))=>b.to_string(), Some(n@Value::Number(_))=>n.to_string(), Some(o)=>o.to_string() } } // = JS String(v ?? '')
fn js_string(v: &Value) -> String { match v { Value::Null=>"null".into(), Value::String(s)=>s.clone(), Value::Number(n)=>n.to_string(), Value::Bool(b)=>b.to_string(), Value::Array(a)=>a.iter().map(js_string).collect::<Vec<_>>().join(","), Value::Object(_)=>"[object Object]".into() } } // = JS String(v)

// See module doc: JS has no env-var lever for REPO_ROOT at all — ANTILOKI_SUBSTRATE_ROOT here
// is a TEST-ONLY injection seam (card-commits.rs precedent, same real target directory).
fn repo_root() -> PathBuf {
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

struct StatusEntry {
    code: String,
    path: String,
}

// `function gitStatus() { try { const out = execSync('git status --porcelain', {cwd, encoding:'utf8'}); return out.split('\n')...; } catch { return []; } }`
fn git_status(cwd: &Path) -> Vec<StatusEntry> {
    let out = match crate::proc::cmd("git").args(["status", "--porcelain"]).current_dir(cwd).output() {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).into_owned(),
        _ => return Vec::new(), // spawn failure, missing git, or non-zero exit (e.g. not a repo)
    };
    out.split('\n')
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .map(|l| {
            let mut parts = l.split_whitespace();
            let code = parts.next().unwrap_or("").to_string();
            let path = parts.collect::<Vec<_>>().join(" ");
            StatusEntry { code, path }
        })
        .collect()
}

// `function getContractFilesForPath(db, relPath)` — returns the matching rows' `.slug` only
// (the only field consumed downstream). Whole-query try/catch → `[]`; per-row JSON.parse
// try/catch → row excluded (not a hard failure).
fn contract_slugs_for_path(db: &Connection, rel_path: &str) -> Vec<String> {
    let pattern = format!("%{rel_path}%");
    let rows = match crate::db_json::query_rows(
        db,
        "SELECT slug, files_touched_json, status, updated_at FROM contracts \
         WHERE status IN ('done','impl','qa') AND files_touched_json LIKE ?1 \
         ORDER BY id DESC LIMIT 10",
        &[json!(pattern)],
    ) {
        Ok(Value::Array(rows)) => rows,
        _ => return Vec::new(),
    };

    rows.into_iter()
        .filter_map(|row| {
            let text = row.get("files_touched_json").and_then(Value::as_str)?;
            let parsed: Value = serde_json::from_str(text).ok()?;
            let is_match = parsed
                .as_array()
                .is_some_and(|a| a.iter().any(|v| v.as_str() == Some(rel_path)));
            if !is_match {
                return None;
            }
            row.get("slug").and_then(Value::as_str).map(|s| s.to_string())
        })
        .collect()
}

/// JS: `async detect(db)`.
pub fn detect(db: &Connection) -> Value {
    let root = repo_root();
    let drifted: Vec<StatusEntry> = git_status(&root)
        .into_iter()
        .filter(|f| {
            if f.path.is_empty() { return false; }
            if f.path.ends_with(".sqlite") { return false; }
            if f.path.starts_with(".antiloki/db/") { return false; }
            if f.path.starts_with(".antiloki/run/") { return false; }
            if f.path.starts_with("node_modules/") { return false; }
            if f.path.contains("/dist/") { return false; }
            true
        })
        .collect();

    let mut findings: Vec<Value> = Vec::new();
    for f in drifted {
        let slugs = contract_slugs_for_path(db, &f.path);
        if slugs.is_empty() {
            continue; // not authored by substrate
        }
        let first_slug = &slugs[0];

        let mut detail = Map::new();
        detail.insert("path".into(), json!(f.path));
        detail.insert("git_code".into(), json!(f.code));
        detail.insert("contracts".into(), json!(slugs));

        let mut finding = Map::new();
        finding.insert("kind".into(), json!("uncommitted-dev-output"));
        finding.insert("severity".into(), json!("human_blocking"));
        finding.insert(
            "message".into(),
            json!(format!(
                "{}: uncommitted (autored by {}); blocks COMMIT_CODE_BEFORE_AUDIT_COMMIT",
                f.path, first_slug
            )),
        );
        finding.insert("contract_slug".into(), json!(first_slug));
        finding.insert("detail".into(), Value::Object(detail));
        finding.insert(
            "suggested_action".into(),
            json!(format!(
                "git add {} && git commit -m \"feat: autonomous pipeline output ({})\"",
                f.path, first_slug
            )),
        );
        findings.push(Value::Object(finding));
    }
    json!(findings)
}

// `function run(args, cwd)` (module-local, not JS-named — mirrors this file's two execSync
// call sites uniformly). Success → Ok(stdout); failure (non-zero exit or spawn error) →
// Err(<truncated diagnostic>), matching the `{applied:false, reason:<string>}` shape JS's
// catch block produces (see module doc: exact wording is not fixture-compared).
fn run_git(args: &[&str], cwd: &Path) -> Result<String, String> {
    match crate::proc::cmd("git").args(args).current_dir(cwd).output() {
        Ok(o) if o.status.success() => Ok(String::from_utf8_lossy(&o.stdout).into_owned()),
        Ok(o) => {
            let stderr = String::from_utf8_lossy(&o.stderr);
            let msg = if stderr.trim().is_empty() {
                format!("git {} failed", args.join(" "))
            } else {
                stderr.trim().to_string()
            };
            Err(msg.chars().take(200).collect())
        }
        Err(e) => Err(e.to_string().chars().take(200).collect()),
    }
}

/// JS: `async fix(finding, db)` — `db` declared but never read in the body.
pub fn fix(finding: &Value, _db: &Connection) -> Value {
    let path = finding
        .get("detail")
        .and_then(|d| d.get("path"))
        .and_then(Value::as_str)
        .filter(|p| !p.is_empty());
    let path = match path {
        Some(p) => p,
        None => return json!({ "applied": false, "reason": "missing path" }),
    };

    let contracts_val: Value = finding
        .get("detail")
        .and_then(|d| d.get("contracts"))
        .filter(|v| !v.is_null())
        .cloned()
        .unwrap_or_else(|| json!([]));
    let contracts_arr: &[Value] = contracts_val.as_array().map(|a| a.as_slice()).unwrap_or(&[]);

    let first_label: String = match contracts_arr.first() {
        None | Some(Value::Null) => "pipeline".to_string(),
        Some(v) => tmpl(Some(v)),
    };
    let joined: String = contracts_arr
        .iter()
        .map(|v| match v {
            Value::Null => String::new(),
            other => js_string(other),
        })
        .collect::<Vec<_>>()
        .join(", ");

    let subject = format!("feat(autonomous): {first_label} produced {path}");
    let body = format!(
        "Auto-committed by reconciler uncommitted-dev-output so the\r\n\
         COMMIT_CODE_BEFORE_AUDIT_COMMIT law-hook stops blocking subsequent\r\n\
         audit-commits. Originating contract(s): {joined}.\r\n\
         \r\n\
         Co-Authored-By: antiloki autonomous pipeline <noreply@anthropic.com>"
    );

    let root = repo_root();
    if let Err(reason) = run_git(&["add", path], &root) {
        return json!({ "applied": false, "reason": reason });
    }
    if let Err(reason) = run_git(&["commit", "-m", &subject, "-m", &body], &root) {
        return json!({ "applied": false, "reason": reason });
    }

    json!({
        "applied": true,
        "changes": { "files_committed": [path], "contracts": contracts_val }
    })
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
        dir.push(format!("antiloki-uncommitted-dev-output-rs-{tag}-{}", std::process::id()));
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
        // A repo with zero commits has no HEAD, which `git commit` handles fine but keeps
        // this setup minimal and explicit for the tests below.
    }

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE contracts (id INTEGER PRIMARY KEY AUTOINCREMENT, slug TEXT, \
             files_touched_json TEXT, status TEXT, updated_at TEXT)",
        )
        .unwrap();
        c
    }

    fn insert_contract(c: &Connection, slug: &str, files: &[&str], status: &str) {
        c.execute(
            "INSERT INTO contracts (slug, files_touched_json, status, updated_at) VALUES (?1, ?2, ?3, '2026-01-01')",
            rusqlite::params![slug, serde_json::to_string(&files).unwrap(), status],
        )
        .unwrap();
    }

    #[test]
    fn detect_returns_empty_when_repo_root_is_not_a_git_repo() {
        // No `git init` here — mirrors this module's real production shape on this checkout
        // (REPO_ROOT = antiloki-core's PARENT dir, which is not itself a git repo).
        let dir = temp_dir("not-a-repo");
        let r = with_substrate_root(dir.to_str().unwrap(), || detect(&db()));
        assert_eq!(r, json!([]));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn detect_ignores_drift_with_no_matching_contract() {
        let dir = temp_dir("no-contract");
        init_repo(&dir);
        fs::write(dir.join("orphan.txt"), "x").unwrap();

        let r = with_substrate_root(dir.to_str().unwrap(), || detect(&db()));
        assert_eq!(r, json!([]));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn detect_filters_excluded_paths_even_with_a_matching_contract() {
        let dir = temp_dir("excluded");
        init_repo(&dir);
        fs::create_dir_all(dir.join("node_modules").join("pkg")).unwrap();
        fs::write(dir.join("node_modules").join("pkg").join("f.js"), "x").unwrap();
        fs::write(dir.join("data.sqlite"), "x").unwrap();
        fs::create_dir_all(dir.join(".antiloki").join("db")).unwrap();
        fs::write(dir.join(".antiloki").join("db").join("state.sqlite3"), "x").unwrap();
        fs::create_dir_all(dir.join("build").join("dist")).unwrap();
        fs::write(dir.join("build").join("dist").join("out.js"), "x").unwrap();

        let c = db();
        // Contracts that would match ANY of the excluded paths, to prove exclusion wins.
        insert_contract(&c, "FEAT-X", &["node_modules/pkg/f.js", "data.sqlite", "build/dist/out.js"], "done");

        let r = with_substrate_root(dir.to_str().unwrap(), || detect(&c));
        assert_eq!(r, json!([]));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn detect_flags_drift_authored_by_a_recent_contract() {
        let dir = temp_dir("flag");
        init_repo(&dir);
        fs::write(dir.join("src.mjs"), "content").unwrap();

        let c = db();
        insert_contract(&c, "FEAT-AUTO-1", &["src.mjs"], "done");

        let r = with_substrate_root(dir.to_str().unwrap(), || detect(&c));
        let arr = r.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["kind"], json!("uncommitted-dev-output"));
        assert_eq!(arr[0]["severity"], json!("human_blocking"));
        assert_eq!(
            arr[0]["message"],
            json!("src.mjs: uncommitted (autored by FEAT-AUTO-1); blocks COMMIT_CODE_BEFORE_AUDIT_COMMIT")
        );
        assert_eq!(arr[0]["contract_slug"], json!("FEAT-AUTO-1"));
        assert_eq!(arr[0]["detail"]["path"], json!("src.mjs"));
        assert_eq!(arr[0]["detail"]["git_code"], json!("??"));
        assert_eq!(arr[0]["detail"]["contracts"], json!(["FEAT-AUTO-1"]));
        assert_eq!(
            arr[0]["suggested_action"],
            json!("git add src.mjs && git commit -m \"feat: autonomous pipeline output (FEAT-AUTO-1)\"")
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn detect_ignores_a_contract_in_a_non_terminal_status() {
        let dir = temp_dir("wrong-status");
        init_repo(&dir);
        fs::write(dir.join("src.mjs"), "content").unwrap();

        let c = db();
        insert_contract(&c, "FEAT-PROPOSED", &["src.mjs"], "proposed");

        let r = with_substrate_root(dir.to_str().unwrap(), || detect(&c));
        assert_eq!(r, json!([]));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn fix_missing_path_is_a_noop_failure() {
        let r = fix(&json!({}), &Connection::open_in_memory().unwrap());
        assert_eq!(r, json!({ "applied": false, "reason": "missing path" }));
        let r2 = fix(&json!({ "detail": {} }), &Connection::open_in_memory().unwrap());
        assert_eq!(r2, json!({ "applied": false, "reason": "missing path" }));
    }

    #[test]
    fn fix_add_failure_outside_a_git_repo_is_reported_as_a_real_failure() {
        let dir = temp_dir("fix-not-a-repo");
        fs::write(dir.join("f.mjs"), "x").unwrap();
        let r = with_substrate_root(dir.to_str().unwrap(), || {
            fix(&json!({ "detail": { "path": "f.mjs", "contracts": ["FEAT-X"] } }), &Connection::open_in_memory().unwrap())
        });
        assert_eq!(r["applied"], json!(false));
        assert!(r["reason"].as_str().is_some_and(|s| !s.is_empty()));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn fix_adds_and_commits_a_drifted_file_attributed_to_the_contract() {
        let dir = temp_dir("fix-success");
        init_repo(&dir);
        fs::write(dir.join("src.mjs"), "content").unwrap();

        let r = with_substrate_root(dir.to_str().unwrap(), || {
            fix(
                &json!({ "detail": { "path": "src.mjs", "contracts": ["FEAT-AUTO-1"] } }),
                &Connection::open_in_memory().unwrap(),
            )
        });
        assert_eq!(
            r,
            json!({ "applied": true, "changes": { "files_committed": ["src.mjs"], "contracts": ["FEAT-AUTO-1"] } })
        );

        // Real assertion the commit landed: git status is now clean.
        let status = git_status(&dir);
        assert!(status.iter().all(|s| s.path != "src.mjs"));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn fix_empty_contracts_falls_back_to_pipeline_label() {
        let dir = temp_dir("fix-no-contracts");
        init_repo(&dir);
        fs::write(dir.join("orphan.mjs"), "x").unwrap();

        let r = with_substrate_root(dir.to_str().unwrap(), || {
            fix(&json!({ "detail": { "path": "orphan.mjs" } }), &Connection::open_in_memory().unwrap())
        });
        assert_eq!(r["applied"], json!(true));
        assert_eq!(r["changes"]["contracts"], json!([]));
        let _ = fs::remove_dir_all(&dir);
    }
}
