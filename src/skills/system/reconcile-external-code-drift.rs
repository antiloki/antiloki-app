//! reconcile-external-code-drift.rs — twin of skills/system/reconcile-external-code-drift.mjs.
//! NAMES ARE CONTRACT: `reconcileExternalCodeDrift` (default, standard lane), plus the two named
//! exports the JS re-exports "for unit-testing the internal helpers": `isExcluded`,
//! `compareWorkingTreeVsSandbox` (`EXCLUDED_PATTERNS` is plain data, not a unit — census rule).
//!
//! Detects working-tree edits that happened OUTSIDE the substrate (operator hand-edits) and
//! classifies each against the open contracts table: (a) claimed + sandbox matches → no finding,
//! (b) claimed + sandbox differs → 'error' collision, (c) unclaimed → 'warn' operator drift.
//!
//! BESPOKE-NATIVE, default export only (playbook's "not conditionally avoidable" rule,
//! uncommitted-dev-output.rs / code-git-info.rs precedent): `reconcileExternalCodeDrift`
//! unconditionally shells to `git status --porcelain` as the FIRST statement of every call — there
//! is no `input` flag that selects a git-free path (unlike constitution-estimate.rs's
//! `input.base`-gated branch). Per the playbook: real correct code, proven by `#[cfg(test)]` native
//! tests against a real temp-dir git repo, NO conform-db fixture and NO frontier entry for this fn.
//!
//! `compareWorkingTreeVsSandbox` is ALSO bespoke (not fixture-tested), for a different reason: it
//! reads real files relative to the module's fixed `REPO_ROOT` (`resolve(HERE,'..','..','..')`,
//! same computation as uncommitted-dev-output.mjs's `REPO_ROOT` — both live one level under
//! `skills/`), which is a hardcoded path, never fixture-controllable (stack-facts.rs precedent:
//! "a roots.mjs-anchored fixed path... never fixture-controllable per the crossing playbook" —
//! unlike stack-facts's OWN db-derived workspace root, which IS). Proven by native tests below.
//!
//! `isExcluded`, however, is PURE (no db, no fs, no subprocess — just 4 regexes against a string)
//! and IS fixture-tested via the no-db lane; it has its own conform-db fixture + frontier entry.
//!
//! `ANTILOKI_SUBSTRATE_ROOT` env var: a TEST-ONLY injection seam for `repo_root()` (same lever
//! card-commits.rs / uncommitted-dev-output.rs already use for this identical real directory) —
//! it does not exist in the JS and never fires in production; it just lets native tests point both
//! the git subprocess and the fs reads at an isolated temp dir instead of mutating the real repo.
//!
//! `gitStatus()`/`safeGitStatusPorcelain()`: `execSync('git status --porcelain', {cwd, timeout:
//! 3000})` inside a try/catch that collapses ANY failure (not a repo, git missing, non-zero exit,
//! timeout) to `[]`. Each non-empty line: `line.slice(3).split(' -> ').pop().trim()` — a CHAR-based
//! slice (skip the 2-char status code + 1 space), then take the target side of a rename arrow (or
//! the whole remainder when there is none), then trim; a second `filter(Boolean)` drops any line
//! whose resulting path collapses to empty.
//!
//! `pathClaims` (path → claimant list): built from `SELECT ... FROM contracts WHERE status IN
//! (...) AND superseded_by IS NULL`, each row's `files_touched_json` parsed via `safeJsonParse`
//! (whole-row try/catch → `[]` fallback, non-array parse result also skipped) into a per-file claim
//! `{ slug, status, sandbox_path }` (this exact 3-key shape/order is what ends up in a finding's
//! `detail.contract`). Iteration ORDER of this map is never observed (only `.get(path)` lookups
//! happen downstream), so `HashMap` suffices — no port of JS `Map`'s insertion order needed here.
//!
//! `bySev` (summary.by_severity): built via `bySev[f.severity] = (bySev[f.severity] ?? 0) + 1`
//! walking `findings` in order — JS object string-key insertion order is FIRST-OCCURRENCE order,
//! which `serde_json::Map`'s `entry().or_insert()` (preserve_order feature) matches exactly.
//!
//! `ran_at: new Date().toISOString()` → frozen-clock literal `"2026-01-01T00:00:00.000Z"` per the
//! playbook's datetime convention (kept for consistency even though this fn carries no fixture).
//!
//! `context.db` missing → JS throws `err.contextMissing(...)`. Not modeled: `db: &Connection` is a
//! required, non-nullable reference (same "guard-throws are not exercised" convention every sibling
//! twin uses). `input` is entirely unused by the JS body (only `context.db` is read) — `_input`
//! here for the same reason `godDriverHalt`/`stackFacts` use an ignored parameter.
// QUALITY: bespoke-native for 2 of 3 units (unconditional git subprocess / fixed-path fs reads,
// uncommitted-dev-output.rs precedent) — no fixture/frontier entry for those two; `isExcluded` is
// pure and IS fixture-tested. Correctness proven by the native tests below.

use regex::Regex;
use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

const FROZEN_ISO: &str = "2026-01-01T00:00:00.000Z";

fn tmpl(v: Option<&Value>) -> String { match v { None=>"undefined".into(), Some(Value::Null)=>"null".into(), Some(x)=>str_qq(Some(x)) } } // = JS `${v}`
fn str_qq(v: Option<&Value>) -> String { match v { None|Some(Value::Null)=>String::new(), Some(Value::String(s))=>s.clone(), Some(Value::Bool(b))=>b.to_string(), Some(n@Value::Number(_))=>n.to_string(), Some(o)=>o.to_string() } } // = JS String(v ?? '')
fn js_string(v: &Value) -> String { match v { Value::Null=>"null".into(), Value::String(s)=>s.clone(), Value::Number(n)=>n.to_string(), Value::Bool(b)=>b.to_string(), Value::Array(a)=>a.iter().map(js_string).collect::<Vec<_>>().join(","), Value::Object(_)=>"[object Object]".into() } } // = JS String(v)

// `const EXCLUDED_PATTERNS = [ /^\.antiloki\/db\/.*\.sqlite(-wal|-shm)?$/, /^\.antiloki\/run\//,
// /^docs\/contracts\//, /^docs\/proof-runs\// ]` — plain data, not a census unit; transcribed as a
// static regex array used by `isExcluded`.
fn excluded_patterns() -> &'static [Regex; 4] {
    static PATTERNS: OnceLock<[Regex; 4]> = OnceLock::new();
    PATTERNS.get_or_init(|| {
        [
            Regex::new(r"^\.antiloki/db/.*\.sqlite(-wal|-shm)?$").unwrap(),
            Regex::new(r"^\.antiloki/run/").unwrap(),
            Regex::new(r"^docs/contracts/").unwrap(),
            Regex::new(r"^docs/proof-runs/").unwrap(),
        ]
    })
}

/// `function isExcluded(path) { return EXCLUDED_PATTERNS.some((re) => re.test(path)); }` —
/// `RegExp.prototype.test` does an implicit `ToString` on its argument, ported via `js_string`
/// (not `str_qq`, which is the `??`-flavored coercion — no `??` here).
pub fn isExcluded(path: &Value) -> Value {
    let s = js_string(path);
    json!(excluded_patterns().iter().any(|re| re.is_match(&s)))
}

// See module doc: JS has no env-var lever for REPO_ROOT at all — ANTILOKI_SUBSTRATE_ROOT here is a
// TEST-ONLY injection seam (card-commits.rs / uncommitted-dev-output.rs precedent, same real
// target directory: `resolve(HERE, '..', '..', '..')` from a file one level under `skills/`).
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

// `function safeGitStatusPorcelain() { try { const out = execSync('git status --porcelain',
// {cwd, encoding:'utf8', timeout:3000}); return out.split('\n').filter(Boolean).map((line) => {
// const path = line.slice(3).split(' -> ').pop().trim(); return path; }).filter(Boolean); }
// catch { return []; } }`
fn safe_git_status_porcelain(cwd: &Path) -> Vec<String> {
    let out = match crate::proc::cmd("git").args(["status", "--porcelain"]).current_dir(cwd).output() {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).into_owned(),
        _ => return Vec::new(), // spawn failure, missing git, or non-zero exit (e.g. not a repo)
    };
    out.split('\n')
        .filter(|l| !l.is_empty())
        .filter_map(|line| {
            let rest: String = line.chars().skip(3).collect();
            let path = rest.split(" -> ").last().unwrap_or("").trim().to_string();
            if path.is_empty() { None } else { Some(path) }
        })
        .collect()
}

// `function safeJsonParse(s, fallback = null) { try { return JSON.parse(s ?? ''); } catch {
// return fallback; } }` — always called here as `safeJsonParse(c.files_touched_json, [])`
// (playbook gotcha #6). A SQL NULL column collapses to `Value::Null`, whose `.as_str()` is
// `None` → fallback, matching JS's `null ?? '' → JSON.parse('') → throw → catch → fallback`.
fn safe_json_parse(s: Option<&Value>, fallback: Value) -> Value {
    match s.and_then(Value::as_str) {
        Some(text) => serde_json::from_str(text).unwrap_or(fallback),
        None => fallback,
    }
}

// `function compareWorkingTreeVsSandbox(relPath, sandboxPath)` — see module doc: real fs reads
// relative to the fixed `repo_root()`, not fixture-controllable. `!sandboxPath` is a JS falsy
// check (null/undefined/''); `sandbox_path: None` here covers all three.
fn compare_working_tree_vs_sandbox_at(root: &Path, rel_path: &str, sandbox_path: Option<&str>) -> &'static str {
    let Some(sandbox) = sandbox_path else { return "no_sandbox" };
    let wt_abs = root.join(rel_path);
    let sb_abs = root.join(sandbox).join(rel_path);
    if !sb_abs.exists() { return "no_sandbox"; }
    if !wt_abs.exists() { return "missing_wt"; }
    match (std::fs::read(&wt_abs), std::fs::read(&sb_abs)) {
        (Ok(a), Ok(b)) => if a == b { "match" } else { "differ" },
        _ => "error",
    }
}

/// `function compareWorkingTreeVsSandbox(relPath, sandboxPath)` — public form matching the JS
/// export's exact arity/name (NAMES ARE CONTRACT). `relPath` non-string: JS's `resolve(REPO_ROOT,
/// relPath)` would throw a `TypeError` caught by the surrounding try/catch → `'error'`; not a
/// realistic caller shape (relPath always comes from `safeGitStatusPorcelain()`'s own strings), so
/// mirrored here as an `'error'` result rather than a panic. NOT fixture-tested (see module doc).
pub fn compareWorkingTreeVsSandbox(rel_path: &Value, sandbox_path: &Value) -> Value {
    let Some(rel) = rel_path.as_str() else { return json!("error") };
    let sandbox = sandbox_path.as_str().filter(|s| !s.is_empty());
    json!(compare_working_tree_vs_sandbox_at(&repo_root(), rel, sandbox))
}

/// `export default function reconcileExternalCodeDrift(input, context)`. See module doc:
/// BESPOKE-NATIVE (unconditional `git status --porcelain` shell-out) — no fixture/frontier entry.
pub fn reconcileExternalCodeDrift(_input: &Value, db: &Connection) -> Value {
    let root = repo_root();
    let changed_paths: Vec<String> = safe_git_status_porcelain(&root)
        .into_iter()
        .filter(|p| !isExcluded(&json!(p)).as_bool().unwrap_or(false))
        .collect();

    if changed_paths.is_empty() {
        return json!({
            "ran_at": FROZEN_ISO,
            "findings": [],
            "summary": { "total": 0, "by_severity": {} },
        });
    }

    let rows = crate::db_json::query_rows(
        db,
        "SELECT slug, status, sandbox_path, files_touched_json FROM contracts \
         WHERE status IN ('proposed', 'pl_ratified', 'tl_approved', 'impl', 'qa') \
           AND superseded_by IS NULL",
        &[],
    )
    .unwrap_or(json!([]));
    let rows_arr = rows.as_array().cloned().unwrap_or_default();

    let mut path_claims: HashMap<String, Vec<Value>> = HashMap::new();
    for c in &rows_arr {
        let files = safe_json_parse(c.get("files_touched_json"), json!([]));
        let Some(files_arr) = files.as_array() else { continue };
        let slug = c.get("slug").cloned().unwrap_or(Value::Null);
        let status = c.get("status").cloned().unwrap_or(Value::Null);
        let sandbox_path = c.get("sandbox_path").cloned().unwrap_or(Value::Null);
        for f in files_arr {
            let Some(fpath) = f.as_str() else { continue };
            let mut claim = Map::new();
            claim.insert("slug".into(), slug.clone());
            claim.insert("status".into(), status.clone());
            claim.insert("sandbox_path".into(), sandbox_path.clone());
            path_claims.entry(fpath.to_string()).or_default().push(Value::Object(claim));
        }
    }

    let mut findings: Vec<Value> = Vec::new();
    for path in &changed_paths {
        let claimants = path_claims.get(path).cloned().unwrap_or_default();
        if claimants.is_empty() {
            let mut detail = Map::new();
            detail.insert("path".into(), json!(path));
            detail.insert("claimants".into(), json!([]));
            findings.push(json!({
                "kind": "external_code_drift_outside_contracts",
                "severity": "warn",
                "message": format!(
                    "File '{path}' was modified outside any active contract's scope. The substrate has no pending work on this file — operator edit acknowledged."
                ),
                "detail": Value::Object(detail),
                "suggested_action": "If the edit is intentional, dismiss. If you want it tracked, file a card with files_touched_proposed including this path.",
            }));
            continue;
        }
        for claim in &claimants {
            let slug = claim.get("slug").cloned().unwrap_or(Value::Null);
            let status = claim.get("status").cloned().unwrap_or(Value::Null);
            let sandbox_path_str = claim.get("sandbox_path").and_then(Value::as_str).filter(|s| !s.is_empty());
            let status_kind = compare_working_tree_vs_sandbox_at(&root, path, sandbox_path_str);
            match status_kind {
                "match" | "missing_wt" => continue,
                "no_sandbox" => {
                    let mut detail = Map::new();
                    detail.insert("path".into(), json!(path));
                    detail.insert("contract".into(), claim.clone());
                    findings.push(json!({
                        "kind": "external_code_drift_before_sandbox",
                        "severity": "warn",
                        "message": format!(
                            "File '{path}' is claimed by contract '{}' ({}) but no sandbox exists yet. Operator edit before DEV picked it up — substrate will see this once sandbox-create runs.",
                            tmpl(Some(&slug)), tmpl(Some(&status))
                        ),
                        "contract_slug": slug,
                        "detail": Value::Object(detail),
                        "suggested_action": "Either commit the operator change first, or wait for DEV to write to the sandbox and reconcile then.",
                    }));
                }
                "differ" => {
                    let mut detail = Map::new();
                    detail.insert("path".into(), json!(path));
                    detail.insert("contract".into(), claim.clone());
                    findings.push(json!({
                        "kind": "external_code_drift_collision",
                        "severity": "error",
                        "message": format!(
                            "File '{path}' has DIFFERENT content in working tree vs sandbox of active contract '{}' ({}). Operator edit and DEV's sandbox will collide on next promote.",
                            tmpl(Some(&slug)), tmpl(Some(&status))
                        ),
                        "contract_slug": slug,
                        "detail": Value::Object(detail),
                        "suggested_action": "Inspect both versions. Either: (1) reject the contract via contract.admin.reject to discard sandbox, (2) accept DEV's sandbox via qa.sandbox.promote (overwrites operator edit), or (3) merge manually before next dispatch.",
                    }));
                }
                _ => {} // 'error' status (read failure) — silently skip, matches JS comment
            }
        }
    }

    let mut by_sev: Map<String, Value> = Map::new();
    for f in &findings {
        let sev = f.get("severity").and_then(Value::as_str).unwrap_or("").to_string();
        let n = by_sev.get(&sev).and_then(Value::as_i64).unwrap_or(0) + 1;
        by_sev.insert(sev, json!(n));
    }

    json!({
        "ran_at": FROZEN_ISO,
        "findings": findings,
        "summary": { "total": findings.len() as i64, "by_severity": Value::Object(by_sev) },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    // SAFETY: ANTILOKI_SUBSTRATE_ROOT is a process-global env var read by several twins' tests
    // across the crate — shared crate-wide lock (test_env_lock.rs); a local mutex would not
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
        dir.push(format!("antiloki-reconcile-external-code-drift-rs-{tag}-{}", std::process::id()));
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

    // `git status --porcelain` (both lanes, verbatim) collapses a BRAND-NEW, entirely-untracked
    // directory to just its own path (e.g. `?? sandboxes/` instead of `?? sandboxes/s1/src.mjs`)
    // — real git behavior neither lane controls. Tests that write into a subdirectory and expect
    // per-file porcelain output must first commit a placeholder there so the directory is already
    // tracked, matching how these paths behave in a real, lived-in substrate checkout.
    fn commit_baseline(dir: &Path, placeholder_subdirs: &[&str]) {
        for sub in placeholder_subdirs {
            let d = dir.join(sub);
            fs::create_dir_all(&d).unwrap();
            fs::write(d.join(".gitkeep"), "").unwrap();
        }
        git(&["add", "-A"], dir);
        git(&["commit", "-q", "-m", "baseline"], dir);
    }

    // compareWorkingTreeVsSandbox reads BOTH copies via direct fs reads, never via git status —
    // the whole algorithm assumes a contract's sandbox_path is never part of THIS repo's own
    // tracked working tree (in production it's a separate git worktree, so it never appears in
    // REPO_ROOT's `git status --porcelain` output at all). A test that writes real files into
    // "sandboxes/s1/" as a plain subdirectory of the SAME repo violates that assumption and would
    // make the sandbox copy itself show up as a second, spurious changedPath. Gitignoring it
    // reproduces the real isolation a separate worktree gives for free.
    fn gitignore_sandboxes(dir: &Path) {
        fs::write(dir.join(".gitignore"), "/sandboxes/\n").unwrap();
        git(&["add", "-A"], dir);
        git(&["commit", "-q", "-m", "baseline"], dir);
    }

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE contracts (id INTEGER PRIMARY KEY AUTOINCREMENT, slug TEXT, status TEXT, \
             sandbox_path TEXT, files_touched_json TEXT, superseded_by INTEGER)",
        )
        .unwrap();
        c
    }

    fn insert_contract(c: &Connection, slug: &str, status: &str, sandbox_path: Option<&str>, files: &[&str]) {
        c.execute(
            "INSERT INTO contracts (slug, status, sandbox_path, files_touched_json) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![slug, status, sandbox_path, serde_json::to_string(&files).unwrap()],
        )
        .unwrap();
    }

    #[test]
    fn is_excluded_matches_db_dir_run_dir_docs_and_lets_source_through() {
        assert_eq!(isExcluded(&json!(".antiloki/db/state.sqlite")), json!(true));
        assert_eq!(isExcluded(&json!(".antiloki/db/memory.sqlite-wal")), json!(true));
        assert_eq!(isExcluded(&json!(".antiloki/run/halt")), json!(true));
        assert_eq!(isExcluded(&json!("docs/contracts/X.html")), json!(true));
        assert_eq!(isExcluded(&json!("docs/proof-runs/1.json")), json!(true));
        assert_eq!(isExcluded(&json!("FE-TUI/src/App.tsx")), json!(false));
        assert_eq!(isExcluded(&json!(".antiloki/skills/system/reconcile.mjs")), json!(false));
    }

    #[test]
    fn reconcile_returns_empty_shape_when_repo_root_is_not_a_git_repo() {
        let dir = temp_dir("not-a-repo");
        let r = with_substrate_root(dir.to_str().unwrap(), || reconcileExternalCodeDrift(&json!({}), &db()));
        assert_eq!(r, json!({ "ran_at": FROZEN_ISO, "findings": [], "summary": { "total": 0, "by_severity": {} } }));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn reconcile_flags_unclaimed_drift_as_warn_outside_contracts() {
        let dir = temp_dir("outside-contracts");
        init_repo(&dir);
        fs::write(dir.join("orphan.mjs"), "x").unwrap();

        let c = db();
        let r = with_substrate_root(dir.to_str().unwrap(), || reconcileExternalCodeDrift(&json!({}), &c));
        let findings = r["findings"].as_array().unwrap();
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0]["kind"], json!("external_code_drift_outside_contracts"));
        assert_eq!(findings[0]["severity"], json!("warn"));
        assert_eq!(findings[0]["detail"]["path"], json!("orphan.mjs"));
        assert_eq!(findings[0]["detail"]["claimants"], json!([]));
        assert_eq!(r["summary"]["total"], json!(1));
        assert_eq!(r["summary"]["by_severity"]["warn"], json!(1));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn reconcile_excludes_db_run_and_docs_paths_even_when_unclaimed() {
        let dir = temp_dir("excluded");
        init_repo(&dir);
        commit_baseline(&dir, &[".antiloki/run", "docs/contracts"]);
        fs::write(dir.join(".antiloki").join("run").join("halt"), "x").unwrap();
        fs::write(dir.join("docs").join("contracts").join("X.html"), "x").unwrap();

        let c = db();
        let r = with_substrate_root(dir.to_str().unwrap(), || reconcileExternalCodeDrift(&json!({}), &c));
        assert_eq!(r["findings"], json!([]));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn reconcile_claimed_with_no_sandbox_yet_is_warn_before_sandbox() {
        let dir = temp_dir("before-sandbox");
        init_repo(&dir);
        fs::write(dir.join("src.mjs"), "content").unwrap();

        let c = db();
        insert_contract(&c, "FEAT-X", "impl", None, &["src.mjs"]);
        let r = with_substrate_root(dir.to_str().unwrap(), || reconcileExternalCodeDrift(&json!({}), &c));
        let findings = r["findings"].as_array().unwrap();
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0]["kind"], json!("external_code_drift_before_sandbox"));
        assert_eq!(findings[0]["severity"], json!("warn"));
        assert_eq!(findings[0]["contract_slug"], json!("FEAT-X"));
        assert_eq!(
            findings[0]["detail"]["contract"],
            json!({ "slug": "FEAT-X", "status": "impl", "sandbox_path": null })
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn reconcile_claimed_with_matching_sandbox_produces_no_finding() {
        let dir = temp_dir("match");
        init_repo(&dir);
        gitignore_sandboxes(&dir);
        fs::create_dir_all(dir.join("sandboxes").join("s1")).unwrap();
        fs::write(dir.join("src.mjs"), "same content").unwrap();
        fs::write(dir.join("sandboxes").join("s1").join("src.mjs"), "same content").unwrap();

        let c = db();
        insert_contract(&c, "FEAT-Y", "qa", Some("sandboxes/s1"), &["src.mjs"]);
        let r = with_substrate_root(dir.to_str().unwrap(), || reconcileExternalCodeDrift(&json!({}), &c));
        assert_eq!(r["findings"], json!([]));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn reconcile_claimed_with_differing_sandbox_is_error_collision() {
        let dir = temp_dir("collision");
        init_repo(&dir);
        gitignore_sandboxes(&dir);
        fs::create_dir_all(dir.join("sandboxes").join("s1")).unwrap();
        fs::write(dir.join("src.mjs"), "operator version").unwrap();
        fs::write(dir.join("sandboxes").join("s1").join("src.mjs"), "dev version").unwrap();

        let c = db();
        insert_contract(&c, "FEAT-Z", "qa", Some("sandboxes/s1"), &["src.mjs"]);
        let r = with_substrate_root(dir.to_str().unwrap(), || reconcileExternalCodeDrift(&json!({}), &c));
        let findings = r["findings"].as_array().unwrap();
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0]["kind"], json!("external_code_drift_collision"));
        assert_eq!(findings[0]["severity"], json!("error"));
        assert_eq!(findings[0]["contract_slug"], json!("FEAT-Z"));
        assert_eq!(r["summary"]["by_severity"]["error"], json!(1));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn reconcile_superseded_contract_never_claims_a_path() {
        let dir = temp_dir("superseded");
        init_repo(&dir);
        fs::write(dir.join("src.mjs"), "content").unwrap();

        let c = db();
        c.execute(
            "INSERT INTO contracts (slug, status, sandbox_path, files_touched_json, superseded_by) \
             VALUES ('FEAT-OLD', 'impl', NULL, '[\"src.mjs\"]', 99)",
            [],
        )
        .unwrap();
        let r = with_substrate_root(dir.to_str().unwrap(), || reconcileExternalCodeDrift(&json!({}), &c));
        let findings = r["findings"].as_array().unwrap();
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0]["kind"], json!("external_code_drift_outside_contracts"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn compare_working_tree_vs_sandbox_no_sandbox_when_sandbox_path_missing() {
        assert_eq!(compareWorkingTreeVsSandbox(&json!("a.txt"), &Value::Null), json!("no_sandbox"));
        assert_eq!(compareWorkingTreeVsSandbox(&json!("a.txt"), &json!("")), json!("no_sandbox"));
    }

    #[test]
    fn compare_working_tree_vs_sandbox_missing_wt_when_only_sandbox_copy_exists() {
        let dir = temp_dir("cmp-missing-wt");
        fs::create_dir_all(dir.join("sb")).unwrap();
        fs::write(dir.join("sb").join("f.txt"), "x").unwrap();
        let r = with_substrate_root(dir.to_str().unwrap(), || {
            compareWorkingTreeVsSandbox(&json!("f.txt"), &json!("sb"))
        });
        assert_eq!(r, json!("missing_wt"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn compare_working_tree_vs_sandbox_match_and_differ() {
        let dir = temp_dir("cmp-match-differ");
        fs::write(dir.join("same.txt"), "x").unwrap();
        fs::create_dir_all(dir.join("sb")).unwrap();
        fs::write(dir.join("sb").join("same.txt"), "x").unwrap();
        fs::write(dir.join("diff.txt"), "a").unwrap();
        fs::write(dir.join("sb").join("diff.txt"), "b").unwrap();

        let (m, d) = with_substrate_root(dir.to_str().unwrap(), || {
            (
                compareWorkingTreeVsSandbox(&json!("same.txt"), &json!("sb")),
                compareWorkingTreeVsSandbox(&json!("diff.txt"), &json!("sb")),
            )
        });
        assert_eq!(m, json!("match"));
        assert_eq!(d, json!("differ"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn compare_working_tree_vs_sandbox_non_string_rel_path_is_error() {
        assert_eq!(compareWorkingTreeVsSandbox(&json!(42), &json!("sb")), json!("error"));
    }
}
