//! qa-scope-check.rs — twin of skills/agents/qa/qa-scope-check.mjs (db lane, thin consumer of the
//! already-converted `contract_contract_get::contractGet` twin). NAMES ARE CONTRACT:
//! `qaScopeCheck` (default). Verifies the diff between `contract.base_sha` and HEAD (plus
//! working-tree dirt) lies entirely within `contract.files_touched` (+ generated-path prefixes).
//!
//! r37 SAFE-PATH GUARD: when the contract has neither `worktree_path` nor `sandbox_path` set, the
//! scope diff is UNJUDGEABLE (no build env to diff inside) and the skill returns
//! `passed:true` with a `no_build_env` reason INSTEAD of ever shelling out — this is the one
//! success path that does NOT touch git, and it is the ONLY path this unit's conform-db fixture
//! exercises. `cwd` in JS is checked via a FALSY test (`if (!cwd)`), not a nullish one — an empty
//! string in `worktree_path`/`sandbox_path` (non-null but falsy) ALSO takes this safe path. Ported
//! as `!truthy(cwd_val)`, not an `Option::is_none()` check — see `qaScopeCheck` below.
//!
//! GIT-DIFF/STATUS BRANCH — DISCLOSED UNTESTED BY CONFORM-DB FIXTURE (playbook lesson 5's
//! flag-skippable pattern, same as constitution-estimate.rs's git-diff branch): once `cwd` is
//! truthy, the JS unconditionally runs `git status --porcelain` (and `git diff --name-only
//! <base>...HEAD` when `contract.base_sha` is truthy) via `execSync` inside `cwd`. This twin ports
//! BOTH as real `std::process::Command` subprocess calls (`git_diff_names` / `git_status_porcelain`)
//! for production parity — real code, not a stub — but no conform-db fixture case sets
//! `worktree_path`/`sandbox_path` truthy: a real git diff/status depends on actual on-disk repo
//! state, which isn't expressible as static fixture JSON the way a committed testdata dir is (the
//! `readFileSync`-through-a-fixture pattern constitution-estimate.rs uses doesn't apply here because
//! the observed OUTPUT is git's own free-form diff/status text, not file content the fixture
//! controls). Because this branch carries the file's core logic, it is instead proven for real via
//! `#[cfg(test)]` native tests against REAL temp git repos (see god-driver-halt.rs / stack-facts.rs /
//! git-manager-branch-init.rs for the same temp-dir injection convention) — these tests mirror the
//! JS `.test.mjs`'s own real-git-repo scenarios (r37 range-diff-vs-dirt union, no-base_sha skip,
//! generated-defaults filtering, in-scope prefix matching, sandbox_path fallback, git-failure shape).
//!
//! `execSync('git diff --name-only ${base}...HEAD')` is ONE shell-interpreted string in JS (so a
//! `base` containing whitespace could split into extra shell tokens); this twin passes the range as
//! a single `Command` arg (no shell) — unreachable in practice (git ref/sha values don't contain
//! spaces) and irrelevant since the branch carries no fixture coverage either way (same disclosed
//! divergence constitution-estimate.rs's git_diff_paths documents).
//!
//! `line.slice(3).trim().replace(/^"|"$/g,'')` (porcelain status-line parsing): the JS regex removes
//! AT MOST one leading `"` and AT MOST one trailing `"` (each anchor matches once against the
//! ORIGINAL string, not iteratively) — ported as two independent, sequential `starts_with`/`pop`
//! checks on the (already `.slice(3)`'d + trimmed) string, which produces the identical result for
//! every length (verified down to 0/1/2-char edge cases in the module's own reasoning, not just the
//! common case).
//!
//! `context.db` missing → JS throws `err.contextMissing('qa-scope-check','db')`. Not modeled:
//! `db: &Connection` is a required, non-nullable reference here, same convention every sibling
//! db-lane twin uses. `contract_slug` missing/falsy → JS `err.required(...)` → ported as `Err`, not
//! fixture-tested (playbook gotcha #9 — only the success path is pinned).
// QUALITY: disclosed-untested git subprocess branch (flag-skippable per lesson 5), proven instead
// via real-git-repo native tests; conform-db fixture covers only the no_build_env safe path.

use rusqlite::Connection;
use serde_json::{json, Value};

fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}
fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) } // JS `??` right operand

const GENERATED_DEFAULTS: [&str; 4] = ["package-lock.json", "yarn.lock", "pnpm-lock.yaml", "node_modules/"];

/// `function git(args, cwd)` analog: real subprocess, stdin ignored (mirrors execSync's
/// `stdio: ['ignore','pipe','pipe']`). Non-zero exit / spawn failure → Err (execSync throws on
/// either).
fn exec_git(mut cmd: std::process::Command) -> Result<String, String> {
    cmd.stdin(std::process::Stdio::null());
    let output = cmd.output().map_err(|e| format!("failed to spawn git: {e}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let detail = if !stderr.trim().is_empty() { stderr.trim() } else { stdout.trim() };
        return Err(format!("git exited with {}: {}", output.status, detail));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// `execSync(\`git diff --name-only ${base}...HEAD\`, { cwd, ... })`
fn git_diff_names(cwd: &str, base: &str) -> Result<String, String> {
    let mut cmd = crate::proc::cmd("git");
    cmd.arg("diff").arg("--name-only").arg(format!("{base}...HEAD")).current_dir(cwd);
    exec_git(cmd)
}

/// `execSync('git status --porcelain', { cwd, ... })`
fn git_status_porcelain(cwd: &str) -> Result<String, String> {
    let mut cmd = crate::proc::cmd("git");
    cmd.arg("status").arg("--porcelain").current_dir(cwd);
    exec_git(cmd)
}

/// `line.slice(3).trim().replace(/^"|"$/g,'').split(' -> ').pop()` — see module doc for the
/// at-most-once quote-strip reasoning.
fn parse_porcelain_line(line: &str) -> String {
    let rest: String = line.chars().skip(3).collect();
    let mut p = rest.trim().to_string();
    if p.starts_with('"') { p.remove(0); }
    if p.ends_with('"') { p.pop(); }
    p.rsplit(" -> ").next().unwrap_or("").to_string()
}

/// `export default function qaScopeCheck(input, context)`
pub fn qaScopeCheck(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS: slug = typeof input === 'string' ? input : input?.contract_slug
    let slug = match input {
        Value::String(s) => s.clone(),
        _ => input.get("contract_slug").and_then(Value::as_str).unwrap_or("").to_string(),
    };
    if slug.is_empty() {
        return Err("qa-scope-check: contract_slug is required".into());
    }

    let contract = crate::contract_contract_get::contractGet(&json!(slug), db)?;
    if contract.is_null() {
        return Err(format!("qa-scope-check: contract '{slug}' not found"));
    }

    // JS: `const allowed = contract.files_touched ?? [];`
    let allowed: Vec<Value> = nz(contract.get("files_touched")).and_then(Value::as_array).cloned().unwrap_or_default();
    // JS: `Array.isArray(contract.generated_paths) ? contract.generated_paths : []`
    let mut generated: Vec<Value> = contract.get("generated_paths").and_then(Value::as_array).cloned().unwrap_or_default();
    // JS: `contract.base_sha || 'HEAD~1'` — falsy fallback (empty string ALSO falls back).
    let base_sha_v = contract.get("base_sha");
    let base = if truthy(base_sha_v) { base_sha_v.and_then(Value::as_str).unwrap_or("HEAD~1").to_string() } else { "HEAD~1".to_string() };

    // JS: `const cwd = contract.worktree_path ?? contract.sandbox_path ?? null; if (!cwd) {...}`
    let cwd_val = nz(contract.get("worktree_path")).or_else(|| nz(contract.get("sandbox_path")));
    if !truthy(cwd_val) {
        return Ok(json!({
            "contract_slug": slug,
            "base_sha": base,
            "declared_files_touched": allowed,
            "generated_paths": generated,
            "changed_files": [],
            "offenders": [],
            "passed": true,
            "evidence": { "reason": "no_build_env — scope diff skipped (would have judged the wrong repo)" },
        }));
    }
    let cwd = cwd_val.and_then(Value::as_str).unwrap_or("").to_string();

    let mut changed: Vec<String> = Vec::new();
    let git_result: Result<(), String> = (|| -> Result<(), String> {
        if truthy(base_sha_v) {
            let base_raw = base_sha_v.and_then(Value::as_str).unwrap_or("");
            let out = git_diff_names(&cwd, base_raw)?;
            changed = out.split('\n').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
        }
        let dirt = git_status_porcelain(&cwd)?;
        for line in dirt.split('\n') {
            let p = parse_porcelain_line(line);
            if !p.is_empty() {
                changed.push(p.replace('\\', "/"));
            }
        }
        let mut seen = std::collections::HashSet::new();
        changed.retain(|p| seen.insert(p.clone()));
        Ok(())
    })();

    if let Err(e) = git_result {
        return Ok(json!({ "passed": false, "evidence": { "reason": "git_diff_failed", "error": e } }));
    }

    for g in GENERATED_DEFAULTS {
        if !generated.iter().any(|v| v.as_str() == Some(g)) {
            generated.push(json!(g));
        }
    }

    let offenders: Vec<String> = {
        let allowed_strs: Vec<&str> = allowed.iter().map(|v| v.as_str().unwrap_or("")).collect();
        let generated_strs: Vec<&str> = generated.iter().map(|v| v.as_str().unwrap_or("")).collect();
        let in_scope = |p: &str| -> bool {
            allowed_strs.iter().any(|a| p == *a || (a.ends_with('/') && p.starts_with(a)))
                || generated_strs.iter().any(|g| {
                    if p == *g { return true; }
                    let prefix = if g.ends_with('/') { g.to_string() } else { format!("{g}/") };
                    p.starts_with(&prefix)
                })
        };
        changed.iter().filter(|p| !in_scope(p)).cloned().collect()
    };
    let passed = offenders.is_empty();

    Ok(json!({
        "contract_slug": slug,
        "base_sha": base,
        "declared_files_touched": allowed,
        "generated_paths": generated,
        "changed_files": changed,
        "offenders": offenders,
        "passed": passed,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::{Path, PathBuf};

    fn run_git(args: &[&str], cwd: &Path) -> String {
        let o = crate::proc::cmd("git").args(args).current_dir(cwd).output().unwrap();
        assert!(o.status.success(), "git {:?} failed: {}", args, String::from_utf8_lossy(&o.stderr));
        String::from_utf8_lossy(&o.stdout).into_owned()
    }

    fn make_repo(tag: &str) -> PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!("antiloki-qa-scope-check-rs-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        run_git(&["init", "-q"], &dir);
        run_git(&["config", "user.email", "t@t.t"], &dir);
        run_git(&["config", "user.name", "t"], &dir);
        run_git(&["config", "commit.gpgsign", "false"], &dir);
        dir
    }

    fn write_file(dir: &Path, rel: &str, content: &str) {
        let abs = dir.join(rel);
        fs::create_dir_all(abs.parent().unwrap()).unwrap();
        fs::write(abs, content).unwrap();
    }

    fn commit_all(dir: &Path, msg: &str) -> String {
        run_git(&["add", "-A"], dir);
        run_git(&["commit", "-q", "-m", msg], dir);
        run_git(&["rev-parse", "HEAD"], dir).trim().to_string()
    }

    fn seeded_db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE contracts (slug TEXT, title TEXT, files_touched_json TEXT, \
             generated_paths_json TEXT, base_sha TEXT, worktree_path TEXT, sandbox_path TEXT)",
        )
        .unwrap();
        c
    }

    fn seed(c: &Connection, slug: &str, files: &str, base_sha: Option<&str>, worktree: Option<&str>, sandbox: Option<&str>, generated: Option<&str>) {
        c.execute(
            "INSERT INTO contracts (slug, title, files_touched_json, generated_paths_json, base_sha, worktree_path, sandbox_path) VALUES (?1, 't', ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![slug, files, generated, base_sha, worktree, sandbox],
        )
        .unwrap();
    }

    #[test]
    fn missing_contract_slug_and_not_found() {
        let c = seeded_db();
        assert_eq!(qaScopeCheck(&json!({}), &c).unwrap_err(), "qa-scope-check: contract_slug is required");
        assert_eq!(qaScopeCheck(&json!({ "contract_slug": "GHOST" }), &c).unwrap_err(), "qa-scope-check: contract 'GHOST' not found");
    }

    #[test]
    fn no_build_env_passes_with_reason_and_skips_generated_defaults() {
        let c = seeded_db();
        seed(&c, "NO-ENV", "[\"src/a.js\"]", None, None, None, None);
        let r = qaScopeCheck(&json!({ "contract_slug": "NO-ENV" }), &c).unwrap();
        assert_eq!(r["passed"], json!(true));
        assert_eq!(r["base_sha"], json!("HEAD~1"));
        assert_eq!(r["declared_files_touched"], json!(["src/a.js"]));
        assert_eq!(r["generated_paths"], json!([]));
        assert_eq!(r["changed_files"], json!([]));
        assert_eq!(r["offenders"], json!([]));
        assert!(r["evidence"]["reason"].as_str().unwrap().contains("no_build_env"));
    }

    #[test]
    fn falsy_empty_string_worktree_path_also_takes_safe_path() {
        let c = seeded_db();
        seed(&c, "EMPTY-WT", "[]", Some(""), Some(""), None, None);
        let r = qaScopeCheck(&json!({ "contract_slug": "EMPTY-WT" }), &c).unwrap();
        assert_eq!(r["passed"], json!(true));
        assert_eq!(r["base_sha"], json!("HEAD~1")); // empty base_sha also falls back
        assert!(r["evidence"]["reason"].as_str().unwrap().contains("no_build_env"));
    }

    #[test]
    fn r37_diff_runs_inside_worktree_path_union_range_and_dirt() {
        let c = seeded_db();
        let dir = make_repo("union");
        write_file(&dir, "a.js", "x");
        let base = commit_all(&dir, "base");
        write_file(&dir, "committed-rogue.js", "x");
        commit_all(&dir, "after-base");
        write_file(&dir, "dirt-rogue.js", "x");

        seed(&c, "UNION", "[\"a.js\"]", Some(&base), Some(dir.to_string_lossy().as_ref()), None, None);
        let r = qaScopeCheck(&json!({ "contract_slug": "UNION" }), &c).unwrap();
        let mut changed: Vec<String> = r["changed_files"].as_array().unwrap().iter().map(|v| v.as_str().unwrap().to_string()).collect();
        changed.sort();
        assert_eq!(changed, vec!["committed-rogue.js", "dirt-rogue.js"]);
        let mut offenders: Vec<String> = r["offenders"].as_array().unwrap().iter().map(|v| v.as_str().unwrap().to_string()).collect();
        offenders.sort();
        assert_eq!(offenders, vec!["committed-rogue.js", "dirt-rogue.js"]);
        assert_eq!(r["passed"], json!(false));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn no_base_sha_skips_range_diff_only_dirt_judged() {
        let c = seeded_db();
        let dir = make_repo("nobase");
        write_file(&dir, "baseline.js", "x");
        commit_all(&dir, "baseline");
        write_file(&dir, "operator-prebranch.js", "x");
        commit_all(&dir, "operator base commit");
        write_file(&dir, "authored.js", "x");

        seed(&c, "NOBASE", "[\"authored.js\"]", None, Some(dir.to_string_lossy().as_ref()), None, None);
        let r = qaScopeCheck(&json!({ "contract_slug": "NOBASE" }), &c).unwrap();
        assert_eq!(r["changed_files"], json!(["authored.js"]));
        assert_eq!(r["offenders"], json!([]));
        assert_eq!(r["passed"], json!(true));
        assert_eq!(r["base_sha"], json!("HEAD~1"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn generated_defaults_filter_lockfiles_and_node_modules() {
        let c = seeded_db();
        let dir = make_repo("gen");
        write_file(&dir, "src/index.js", "x");
        let base = commit_all(&dir, "base");
        write_file(&dir, "package-lock.json", "x");
        write_file(&dir, "yarn.lock", "x");
        write_file(&dir, "pnpm-lock.yaml", "x");
        write_file(&dir, "node_modules/pkg/index.js", "x");
        write_file(&dir, "dist/bundle.js", "x");
        write_file(&dir, "src/rogue.js", "x");

        seed(&c, "GEN", "[\"src/index.js\"]", Some(&base), Some(dir.to_string_lossy().as_ref()), None, Some("[\"dist/\"]"));
        let r = qaScopeCheck(&json!({ "contract_slug": "GEN" }), &c).unwrap();
        assert_eq!(
            r["generated_paths"],
            json!(["dist/", "package-lock.json", "yarn.lock", "pnpm-lock.yaml", "node_modules/"])
        );
        assert_eq!(r["offenders"], json!(["src/rogue.js"]));
        assert_eq!(r["passed"], json!(false));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn in_scope_exact_and_trailing_slash_prefix_matching() {
        let c = seeded_db();
        let dir = make_repo("match");
        write_file(&dir, "base.txt", "x");
        write_file(&dir, "src/index.js", "x");
        let base = commit_all(&dir, "base");
        write_file(&dir, "exact.js", "x");
        write_file(&dir, "src/feature.js", "x");
        write_file(&dir, "rogue.js", "x");

        seed(&c, "MATCH", "[\"exact.js\",\"src/\"]", Some(&base), Some(dir.to_string_lossy().as_ref()), None, None);
        let r = qaScopeCheck(&json!({ "contract_slug": "MATCH" }), &c).unwrap();
        assert_eq!(r["offenders"], json!(["rogue.js"]));
        assert_eq!(r["passed"], json!(false));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn falls_back_to_sandbox_path_when_worktree_path_absent() {
        let c = seeded_db();
        let dir = make_repo("sandbox");
        write_file(&dir, "x.js", "x");
        let base = commit_all(&dir, "base");
        write_file(&dir, "rogue.js", "x");

        seed(&c, "SB", "[\"x.js\"]", Some(&base), None, Some(dir.to_string_lossy().as_ref()), None);
        let r = qaScopeCheck(&json!({ "contract_slug": "SB" }), &c).unwrap();
        assert_eq!(r["offenders"], json!(["rogue.js"]));
        assert_eq!(r["passed"], json!(false));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn git_failure_returns_minimal_failure_shape() {
        let c = seeded_db();
        let dir = make_repo("badsha");
        write_file(&dir, "x.js", "x");
        commit_all(&dir, "base");

        seed(&c, "BADSHA", "[]", Some("no-such-ref-xyz"), Some(dir.to_string_lossy().as_ref()), None, None);
        let r = qaScopeCheck(&json!({ "contract_slug": "BADSHA" }), &c).unwrap();
        assert_eq!(r["passed"], json!(false));
        assert_eq!(r["evidence"]["reason"], json!("git_diff_failed"));
        assert!(r["evidence"]["error"].is_string());
        assert!(r.get("changed_files").is_none(), "failure path returns the minimal shape");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn string_input_variant_resolves_slug() {
        let c = seeded_db();
        seed(&c, "STR", "[]", None, None, None, None);
        let r = qaScopeCheck(&json!("STR"), &c).unwrap();
        assert_eq!(r["contract_slug"], json!("STR"));
        assert_eq!(r["passed"], json!(true));
    }
}
