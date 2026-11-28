//! dev-acceptance-run.rs — twin of skills/agents/dev/dev-acceptance-run.mjs (THIN CONSUMER of
//! `contract_contract_get::contractGet` + `engine_action_run::skillRun`). NAMES ARE CONTRACT:
//! `devAcceptanceRun` (default; only export in the JS module). Dispatches every `acceptance_json`
//! item of a contract to its kind-specific verifier: test/cli (real shell subprocess),
//! file_exists (real fs), sql (real DB read), diff_in_paths (real git diff OR sandbox-vs-original
//! byte compare), no_law_violation (laws.laws / laws.law_violations read), skill_returns (real
//! dispatch through the already-converted `action-run` twin).
//!
//! BESPOKE-NATIVE (not conform-db): a real `std::process::Command` subprocess run (`test`/`cli`
//! kinds), real fs reads (`file_exists`), and real `git diff` reads (`diff_in_paths`, non-sandbox
//! mode) are genuinely non-deterministic/non-portable across the JS/Rust conform lanes — same
//! category+precedent as the sibling `agents_dev_dev_build_gate` twin (whose own module doc
//! discloses the identical real-subprocess gap) and `agents_dev_dev_test_gate`.
//!
//! ## What is ported faithfully
//! - `resolvePathForAc`: worktree_path (full checkout) → resolves everywhere; else sandbox_path +
//!   the relpath being IN `files_touched` → sandbox copy; else the live-tree relpath unchanged.
//! - `runTest` (shared verbatim by `test` AND `cli` kinds, exactly like JS's
//!   `case 'cli': r = runTest({ ...item, cmd: item.cmd }, contract)`): worktree-cwd /
//!   sandbox-cwd / live-tree mode selection, `expect_exit` comparison on nonzero exit, 4000-char
//!   stdout/stderr caps on failure.
//! - `runFileExists`: `path` ?? `target` ?? `file` ?? prose-extracted path (the `PROSE_PATH_RE`
//!   regex over `done_criteria`+`intent`, r32), min_bytes + must_contain checks.
//! - `runSql`: raw `item.query` read (no bind params, matching JS `db.prepare(item.query).get()`),
//!   `expect_value_min`/`expect_value_path` numeric-threshold branch vs plain row-truthiness.
//! - `runDiffInPaths`: sandbox mode (byte-compare each `files_touched` entry, sandbox vs original)
//!   when `contract.sandbox_path` is set, else legacy `git diff --name-only {base}...HEAD`;
//!   `resolveAllowedPaths` (allowed/paths/files_touched field + stringified-JSON-array normalize)
//!   and `isGeneratedPath` (generated_paths prefix exemption) both ported.
//! - `runNoLawViolation`, `runSkillReturns` (real `engine_action_run::skillRun` dispatch + the
//!   3-shape mini-jsonpath: `$`, `$.field`, `$[*].field`).
//! - The CLASS-B CHOKEPOINT (`isMachineRunnable`/`hasProseBody`/`proseCourtJudged`, r32+r45): a
//!   criterion tagged with a machine `kind` but missing the field that kind's runner needs (or a
//!   prose kind `grep`/`behavior`/`behavioral`, or an unknown/invented kind) that STILL carries
//!   prose (`done_criteria`/`intent`) is court-judged (`passed:true, skipped:true`), never crashed
//!   or hard-failed; only a genuinely field-less+prose-less item falls to its runner's own
//!   `malformed_ac_no_path`/`unknown_kind` path.
//! - `contract.bootstrap_cmd` run ONCE (sandbox-only, matching JS which never checks
//!   `worktree_path` here) before any acceptance item, recorded but never thrown on failure.
//! - The final tally shape: `{contract_slug, bootstrap, count, all_passed, passed, failed,
//!   results}, results[i] = {index, kind, passed, evidence, ...}`.
//!
//! ## Deliberately NOT ported (disclosed gaps)
//! - The `try { ... } catch (e) { ... ac_runner_crashed ... }` per-item guard (r30 session-killer
//!   protection): every Rust arm below returns a plain `Value` (no panics on malformed input by
//!   construction — `Option`/`unwrap_or` throughout), so the JS catch branch is structurally
//!   unreachable here and not modeled, same convention as every sibling twin's dropped
//!   `context?.db` guard.
//! - `REPO_ROOT`-relative resolution (`resolve(REPO_ROOT, contract.sandbox_path)` where
//!   `REPO_ROOT` is 4 dirs up from the `.mjs` file): this twin ships in a separate repo with no
//!   equivalent fixed anchor, so a relative `sandbox_path` is joined onto `ANTILOKI_REPO_ROOT`
//!   (env var, default `"."`) — same convention as `agents_dev_dev_build_gate::sandbox_root`. An
//!   absolute `worktree_path` (the common real-world case, EPIC-WORKTREE) bypasses this entirely.
//! - execSync's exact `e.message` text on a `git diff` failure: approximated as the captured
//!   stderr (the observable content JS's `e.message` would itself embed), not exercised beyond a
//!   nonzero-exit smoke test.
// QUALITY: 80/100 — every AC kind's runner ported faithfully incl. the class-B prose chokepoint;
// disclosed gaps are the per-item panic guard (structurally unreachable in Rust) and the
// JS-repo-specific REPO_ROOT anchor (necessarily reinterpreted cross-repo, same as build-gate).

use regex::Regex;
use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

fn truthy(v: Option<&Value>) -> bool { match v { None|Some(Value::Null)=>false, Some(Value::Bool(b))=>*b, Some(Value::Number(n))=>n.as_f64().map_or(false,|f|f!=0.0&&!f.is_nan()), Some(Value::String(s))=>!s.is_empty(), Some(_)=>true } }
fn str_qq(v: Option<&Value>) -> String { match v { None|Some(Value::Null)=>String::new(), Some(Value::String(s))=>s.clone(), Some(Value::Bool(b))=>b.to_string(), Some(n@Value::Number(_))=>n.to_string(), Some(o)=>o.to_string() } }
fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) }
fn take_chars(s: &str, n: usize) -> String { s.chars().take(n).collect() }

fn repo_root() -> String {
    std::env::var("ANTILOKI_REPO_ROOT").unwrap_or_else(|_| ".".to_string())
}

// ── PROSE_PATH_RE (r32) ──────────────────────────────────────────────────────
fn prose_path_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r#"(?:^|[\s'"`(])((?:src|public|app|lib|test|tests|components|pages)/[\w./-]+\.\w+)"#).unwrap())
}
fn extract_path_from_prose(item: &Value) -> Option<String> {
    let dc = str_qq(nz(item.get("done_criteria")));
    let intent = str_qq(nz(item.get("intent")));
    let hay = format!("{dc} {intent}");
    prose_path_re().captures(&hay).map(|c| c[1].to_string())
}

// EPIC-QA-SANDBOX-INTEGRATION: route each path-based AC kind to the sandbox copy when the path is
// in files_touched; otherwise current behavior. `None` mirrors JS `null` (malformed AC).
fn resolve_path_for_ac(relpath: Option<&str>, contract: &Value) -> Option<PathBuf> {
    let rel = relpath?;
    if rel.trim().is_empty() { return None; }
    if let Some(w) = contract.get("worktree_path").and_then(Value::as_str).filter(|s| !s.is_empty()) {
        return Some(Path::new(w).join(rel));
    }
    let sandbox = match contract.get("sandbox_path").and_then(Value::as_str).filter(|s| !s.is_empty()) {
        Some(s) => s,
        None => return Some(PathBuf::from(rel)), // no sandbox → live tree
    };
    let allowed: Vec<String> = contract.get("files_touched").and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect()).unwrap_or_default();
    if !allowed.iter().any(|a| a == rel) { return Some(PathBuf::from(rel)); } // out of scope → live tree
    Some(Path::new(&repo_root()).join(sandbox).join(rel))
}

fn run_output(cmd: &str, cwd: &str) -> (i32, String, String) {
    #[cfg(windows)]
    let mut c = { let mut c = crate::proc::cmd("cmd"); c.arg("/C").arg(cmd); c };
    #[cfg(not(windows))]
    let mut c = { let mut c = crate::proc::cmd("sh"); c.arg("-c").arg(cmd); c };
    c.current_dir(cwd);
    match c.output() {
        Ok(o) => (o.status.code().unwrap_or(1), String::from_utf8_lossy(&o.stdout).into_owned(), String::from_utf8_lossy(&o.stderr).into_owned()),
        Err(e) => (1, String::new(), e.to_string()),
    }
}

/// `function runTest(item, contract)` — shared verbatim by both the `test` and `cli` kinds.
#[allow(non_snake_case)]
fn runTest(item: &Value, contract: &Value) -> Value {
    let using_worktree = truthy(contract.get("worktree_path"));
    let files_touched_nonempty = contract.get("files_touched").and_then(Value::as_array).map(|a| !a.is_empty()).unwrap_or(false);
    let using_sandbox = !using_worktree && truthy(contract.get("sandbox_path")) && files_touched_nonempty;
    let (cwd, mode) = if using_worktree {
        (contract.get("worktree_path").and_then(Value::as_str).unwrap_or("").to_string(), "worktree-cwd")
    } else if using_sandbox {
        (Path::new(&repo_root()).join(contract.get("sandbox_path").and_then(Value::as_str).unwrap_or("")).to_string_lossy().into_owned(), "sandbox-cwd")
    } else {
        (repo_root(), "live-tree")
    };
    let cmd = str_qq(item.get("cmd"));
    let (code, out, err) = run_output(&cmd, &cwd);
    if code == 0 {
        return json!({ "passed": true, "evidence": { "exit": 0, "mode": mode } });
    }
    let expect_exit = item.get("expect_exit").and_then(Value::as_i64).unwrap_or(0);
    json!({
        "passed": expect_exit == code as i64,
        "evidence": { "exit": code, "stdout": take_chars(&out, 4000), "stderr": take_chars(&err, 4000), "mode": mode },
    })
}

/// `function proseCourtJudged(item)` — r32: a prose-shaped criterion is judged by the snippet
/// COURT (which already judged it against the goal-line), never crashed or hard-failed.
#[allow(non_snake_case)]
fn proseCourtJudged(item: &Value) -> Value {
    let val = if nz(item.get("done_criteria")).is_some() { str_qq(nz(item.get("done_criteria"))) } else { str_qq(nz(item.get("intent"))) };
    json!({ "passed": true, "skipped": true, "evidence": { "reason": "prose_criterion_court_judged", "criterion": take_chars(&val, 160) } })
}

const PROSE_KINDS: [&str; 3] = ["grep", "behavior", "behavioral"];

fn has_prose_body(item: &Value) -> bool {
    !str_qq(nz(item.get("done_criteria"))).trim().is_empty() || !str_qq(nz(item.get("intent"))).trim().is_empty()
}

/// CLASS-B CHOKEPOINT: mirrors the JS `MACHINE_FIELD` dispatch table — true only if the item's
/// declared `kind` carries the machine field that kind's runner needs.
fn is_machine_runnable(item: &Value) -> bool {
    let kind = item.get("kind").and_then(Value::as_str).unwrap_or("");
    if PROSE_KINDS.contains(&kind) { return false; }
    match kind {
        "test" | "cli" => truthy(item.get("cmd")),
        "sql" => truthy(item.get("query")),
        "no_law_violation" => truthy(item.get("law_slug")),
        "skill_returns" => truthy(item.get("action")),
        "file_exists" | "file-exists" => {
            let direct = nz(item.get("path")).or_else(|| nz(item.get("target"))).or_else(|| nz(item.get("file")));
            match direct {
                Some(v) => truthy(Some(v)),
                None => extract_path_from_prose(item).is_some(),
            }
        }
        "diff_in_paths" => true, // falls back to contract.files_touched — always runnable
        _ => false, // an unknown/invented kind is never machine-runnable
    }
}

#[allow(non_snake_case)]
fn runFileExists(item: &Value, contract: &Value) -> Value {
    let rel = nz(item.get("path")).and_then(Value::as_str)
        .or_else(|| nz(item.get("target")).and_then(Value::as_str))
        .or_else(|| nz(item.get("file")).and_then(Value::as_str))
        .map(str::to_string)
        .or_else(|| extract_path_from_prose(item));
    let check_path = match resolve_path_for_ac(rel.as_deref(), contract) {
        Some(p) => p,
        None => {
            let keys: Vec<Value> = item.as_object().map(|o| o.keys().map(|k| json!(k)).collect()).unwrap_or_default();
            return json!({ "passed": false, "evidence": { "reason": "malformed_ac_no_path", "item_keys": keys } });
        }
    };
    let resolved = check_path.to_string_lossy().into_owned();
    if !check_path.exists() {
        return json!({ "passed": false, "evidence": { "reason": "missing", "resolved_path": resolved } });
    }
    let meta = match std::fs::metadata(&check_path) {
        Ok(m) => m,
        Err(_) => return json!({ "passed": false, "evidence": { "reason": "missing", "resolved_path": resolved } }),
    };
    let min_bytes = item.get("min_bytes").and_then(Value::as_i64).filter(|n| *n != 0);
    if let Some(mb) = min_bytes {
        if (meta.len() as i64) < mb {
            return json!({ "passed": false, "evidence": { "reason": "too_small", "size": meta.len(), "min": mb, "resolved_path": resolved } });
        }
    }
    if let Some(must) = item.get("must_contain").and_then(Value::as_array).filter(|a| !a.is_empty()) {
        let text = std::fs::read_to_string(&check_path).unwrap_or_default();
        let missing: Vec<Value> = must.iter().filter(|s| !text.contains(s.as_str().unwrap_or(""))).cloned().collect();
        if !missing.is_empty() {
            return json!({ "passed": false, "evidence": { "reason": "missing_tokens", "missing": missing, "resolved_path": resolved } });
        }
    }
    json!({ "passed": true, "evidence": { "size": meta.len(), "resolved_path": resolved } })
}

#[allow(non_snake_case)]
fn runSql(item: &Value, db: &Connection) -> Value {
    let query = str_qq(item.get("query"));
    let rows = match crate::db_json::query_rows(db, &query, &[]) {
        Ok(r) => r,
        Err(e) => return json!({ "passed": false, "evidence": { "error": e } }),
    };
    let row = rows.as_array().and_then(|a| a.first()).cloned();
    let expect_min = item.get("expect_value_min").filter(|v| !v.is_null());
    let expect_path = item.get("expect_value_path").and_then(Value::as_str);
    if let (Some(min), Some(path)) = (expect_min, expect_path) {
        let key = path.strip_prefix("$.").unwrap_or(path);
        let v = row.as_ref().and_then(|r| r.get(key)).cloned().unwrap_or(Value::Null);
        let passed = match (v.as_f64(), min.as_f64()) {
            (Some(vf), Some(mf)) => v.is_number() && vf >= mf,
            _ => false,
        };
        return json!({ "passed": passed, "evidence": { "value": v, "min": min } });
    }
    json!({ "passed": row.is_some(), "evidence": { "row": row.unwrap_or(Value::Null) } })
}

/// `function resolveAllowedPaths(item, contract)` — normalizes `allowed`/`paths`/`files_touched`
/// (possibly a stringified JSON array), falling back to `contract.files_touched`.
#[allow(non_snake_case)]
fn resolveAllowedPaths(item: &Value, contract: &Value) -> Vec<String> {
    let raw = nz(item.get("allowed")).or_else(|| nz(item.get("paths"))).or_else(|| nz(item.get("files_touched")));
    match raw {
        Some(Value::Array(a)) => return a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect(),
        Some(Value::String(s)) => {
            let t = s.trim();
            if t.starts_with('[') && t.ends_with(']') {
                if let Ok(Value::Array(a)) = serde_json::from_str::<Value>(t) {
                    return a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect();
                }
            }
        }
        _ => {}
    }
    contract.get("files_touched").and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
        .unwrap_or_default()
}

#[allow(non_snake_case)]
fn isGeneratedPath(p: &str, contract: &Value) -> bool {
    let generated: Vec<String> = contract.get("generated_paths").and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect()).unwrap_or_default();
    generated.iter().any(|g| p == g || p.starts_with(&if g.ends_with('/') { g.clone() } else { format!("{g}/") }))
}

fn offenders_of(changed: &[String], item: &Value, contract: &Value) -> Vec<String> {
    let allowed = resolveAllowedPaths(item, contract);
    changed.iter()
        .filter(|p| !isGeneratedPath(p, contract) && !allowed.iter().any(|a| *p == a || p.starts_with(&a.replace("/*", "/"))))
        .cloned().collect()
}

#[allow(non_snake_case)]
fn runDiffInPaths(item: &Value, contract: &Value) -> Value {
    // FEAT-QA-DIFF-IN-PATHS-SANDBOX-AWARE: sandbox present → compare sandbox vs originals.
    if let Some(sandbox) = contract.get("sandbox_path").and_then(Value::as_str).filter(|s| !s.is_empty()) {
        let scope: Vec<String> = contract.get("files_touched").and_then(Value::as_array)
            .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect()).unwrap_or_default();
        let mut changed: Vec<String> = Vec::new();
        for relpath in &scope {
            let sb_abs = Path::new(&repo_root()).join(sandbox).join(relpath);
            let orig_abs = Path::new(&repo_root()).join(relpath);
            let sb_exists = sb_abs.exists();
            let orig_exists = orig_abs.exists();
            if !sb_exists && !orig_exists { continue; }
            if !sb_exists || !orig_exists { changed.push(relpath.clone()); continue; }
            let sb_buf = std::fs::read(&sb_abs).unwrap_or_default();
            let orig_buf = std::fs::read(&orig_abs).unwrap_or_default();
            if sb_buf != orig_buf { changed.push(relpath.clone()); }
        }
        let offenders = offenders_of(&changed, item, contract);
        return json!({ "passed": offenders.is_empty(), "evidence": { "mode": "sandbox-vs-original", "changed": changed, "offenders": offenders.into_iter().take(10).collect::<Vec<_>>() } });
    }
    // No sandbox → legacy git-based comparison.
    let base_val = item.get("base").and_then(Value::as_str).unwrap_or("");
    let base = if base_val == "contract.base_sha" || base_val.is_empty() { "HEAD~1".to_string() } else { base_val.to_string() };
    let (code, out, err) = run_output(&format!("git diff --name-only {base}...HEAD"), &repo_root());
    if code != 0 {
        return json!({ "passed": false, "evidence": { "error": err, "mode": "git-diff" } });
    }
    let changed: Vec<String> = out.split('\n').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
    let offenders = offenders_of(&changed, item, contract);
    json!({ "passed": offenders.is_empty(), "evidence": { "mode": "git-diff", "changed": changed, "offenders": offenders.into_iter().take(10).collect::<Vec<_>>() } })
}

#[allow(non_snake_case)]
fn runNoLawViolation(item: &Value, db: &Connection) -> Value {
    let law_slug = str_qq(item.get("law_slug"));
    let law = match crate::db_json::query_rows(db, "SELECT id FROM laws.laws WHERE slug = ?", &[json!(law_slug)]) {
        Ok(rows) => rows.as_array().and_then(|a| a.first()).cloned(),
        Err(e) => return json!({ "passed": false, "evidence": { "error": e } }),
    };
    let law = match law {
        Some(l) => l,
        None => return json!({ "passed": false, "evidence": { "reason": "law_not_found", "slug": law_slug } }),
    };
    let law_id = law.get("id").cloned().unwrap_or(Value::Null);
    match crate::db_json::query_rows(db, "SELECT COUNT(*) AS c FROM laws.law_violations WHERE law_id = ? AND outcome IN ('blocked','escaped')", &[law_id]) {
        Ok(rows) => {
            let count = rows.as_array().and_then(|a| a.first()).and_then(|r| r.get("c")).and_then(Value::as_i64).unwrap_or(0);
            json!({ "passed": count == 0, "evidence": { "violation_count": count } })
        }
        Err(e) => json!({ "passed": false, "evidence": { "error": e } }),
    }
}

#[allow(non_snake_case)]
fn runSkillReturns(item: &Value, db: &Connection) -> Value {
    let call = json!({ "action": item.get("action").cloned().unwrap_or(Value::Null), "input": item.get("input").cloned().unwrap_or(json!({})) });
    let result = match crate::engine_action_run::skillRun(&call, db) {
        Ok(r) => r,
        Err(e) => return json!({ "passed": false, "evidence": { "error": e } }),
    };
    let expect: Map<String, Value> = item.get("expect_jsonpath").and_then(Value::as_object).cloned().unwrap_or_default();
    let ok = expect.iter().all(|(path, expected)| {
        if path == "$" { return &result == expected; }
        if let Some(field) = path.strip_prefix("$.") { return result.get(field) == Some(expected); }
        if let Some(field) = path.strip_prefix("$[*].") {
            return result.as_array().map(|a| a.iter().any(|r| r.get(field) == Some(expected))).unwrap_or(false);
        }
        false
    });
    let preview = take_chars(&serde_json::to_string(&result).unwrap_or_default(), 200);
    json!({ "passed": ok, "evidence": { "result_preview": preview } })
}

#[allow(non_snake_case)]
pub fn devAcceptanceRun(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS `if (!db) throw err.contextMissing(...)` — unreachable (db: &Connection is required),
    // same convention as every sibling twin.
    let slug = match input {
        Value::String(s) => s.clone(),
        _ => input.get("contract_slug").and_then(Value::as_str).unwrap_or("").to_string(),
    };
    if slug.is_empty() {
        return Err("dev-acceptance-run: contract_slug is required".to_string());
    }
    let contract = crate::contract_contract_get::contractGet(&json!(slug.clone()), db)?;
    if contract.is_null() {
        return Err(format!("dev-acceptance-run: contract '{slug}' not found"));
    }

    // EPIC-AUTHORED-VS-GENERATED: run bootstrap_cmd ONCE with cwd=sandbox before any acceptance.
    let mut bootstrap: Value = Value::Null;
    let sandbox_path = contract.get("sandbox_path").and_then(Value::as_str).filter(|s| !s.is_empty());
    let bootstrap_cmd = contract.get("bootstrap_cmd").and_then(Value::as_str).filter(|s| !s.is_empty());
    if let (Some(bc), Some(sp)) = (bootstrap_cmd, sandbox_path) {
        let cwd = Path::new(&repo_root()).join(sp);
        let (code, _out, err) = run_output(bc, &cwd.to_string_lossy());
        bootstrap = if code == 0 {
            json!({ "cmd": bc, "exit": 0, "cwd": sp })
        } else {
            json!({ "cmd": bc, "exit": code, "stderr": take_chars(&err, 300), "cwd": sp })
        };
    }

    let acceptance: Vec<Value> = contract.get("acceptance").and_then(Value::as_array).cloned().unwrap_or_default();
    let mut results: Vec<Value> = Vec::new();
    for (i, item) in acceptance.iter().enumerate() {
        let kind = item.get("kind").cloned().unwrap_or(Value::Null);
        let r = if !is_machine_runnable(item) && has_prose_body(item) {
            proseCourtJudged(item)
        } else {
            match kind.as_str().unwrap_or("") {
                "test" => runTest(item, &contract),
                "cli" => runTest(item, &contract),
                "file_exists" | "file-exists" => runFileExists(item, &contract),
                "sql" => runSql(item, db),
                "grep" | "behavior" | "behavioral" => proseCourtJudged(item),
                "diff_in_paths" => runDiffInPaths(item, &contract),
                "no_law_violation" => runNoLawViolation(item, db),
                "skill_returns" => runSkillReturns(item, db),
                _ => json!({ "passed": false, "evidence": { "reason": "unknown_kind", "kind": kind } }),
            }
        };
        let mut row = Map::new();
        row.insert("index".into(), json!(i));
        row.insert("kind".into(), kind);
        if let Some(o) = r.as_object() {
            for (k, v) in o { row.insert(k.clone(), v.clone()); }
        }
        results.push(Value::Object(row));
    }

    let count = results.len();
    let passed = results.iter().filter(|r| truthy(r.get("passed"))).count();
    let failed = count - passed;
    Ok(json!({
        "contract_slug": slug,
        "bootstrap": bootstrap,
        "count": count,
        "all_passed": failed == 0,
        "passed": passed,
        "failed": failed,
        "results": results,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    const CONTRACTS_SCHEMA: &str = "CREATE TABLE contracts (id INTEGER PRIMARY KEY AUTOINCREMENT, slug TEXT UNIQUE, card_slug TEXT, status TEXT, sandbox_path TEXT, worktree_path TEXT, bootstrap_cmd TEXT, files_touched_json TEXT, acceptance_json TEXT, generated_paths_json TEXT, updated_at TEXT)";
    // skill_returns_kind_dispatches_through_action_run_and_checks_jsonpath is the one test here
    // that goes through the REAL action-run dispatch (crate::engine_action_run::skillRun), which
    // unconditionally runs law-check's law-check.mjs-identical query selecting
    // kind/domain/statement/enforcement/superseded_by — a laws.laws missing those columns makes
    // that query fail with "no such column", which skillRun propagates and runSkillReturns
    // swallows into passed:false (confirmed against real JS: law-check.mjs runs the identical
    // WHERE superseded_by IS NULL AND enforcement = 'hard' ... query against the same table).
    const LAWS_SCHEMA: &str = "CREATE TABLE laws.laws (id INTEGER PRIMARY KEY, slug TEXT, kind TEXT, domain TEXT, statement TEXT, enforcement TEXT, enforcement_hook TEXT, superseded_by INTEGER)";
    const LAW_VIOLATIONS_SCHEMA: &str = "CREATE TABLE laws.law_violations (id INTEGER PRIMARY KEY, law_id INTEGER, outcome TEXT)";

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS laws;").unwrap();
        c.execute_batch(&format!("{CONTRACTS_SCHEMA}; {LAWS_SCHEMA}; {LAW_VIOLATIONS_SCHEMA};")).unwrap();
        c
    }

    fn insert_contract(c: &Connection, slug: &str, sandbox: Option<&str>, worktree: Option<&str>, files_touched: &str, acceptance: &str, bootstrap_cmd: Option<&str>) {
        c.execute(
            "INSERT INTO contracts (slug, sandbox_path, worktree_path, files_touched_json, acceptance_json, bootstrap_cmd) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![slug, sandbox, worktree, files_touched, acceptance, bootstrap_cmd],
        ).unwrap();
    }

    // ANTILOKI_REPO_ROOT is a process-global env var (like ANTILOKI_VIBE_CHAT in
    // cockpit-pane-message.rs) — guard reads/writes with the shared crate-wide lock so this
    // file's own two env-setting tests (and any other test file touching the same var) don't
    // race under parallel `cargo test`.
    fn with_repo_root<T>(v: &str, f: impl FnOnce() -> T) -> T {
        let _guard = crate::test_env_lock::ENV_LOCK.lock().unwrap();
        unsafe { std::env::set_var("ANTILOKI_REPO_ROOT", v) };
        let result = f();
        unsafe { std::env::remove_var("ANTILOKI_REPO_ROOT") };
        result
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!("antiloki-dev-acceptance-run-rs-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn guards_missing_slug_and_not_found() {
        let c = db();
        assert_eq!(devAcceptanceRun(&json!({}), &c).unwrap_err(), "dev-acceptance-run: contract_slug is required");
        assert_eq!(devAcceptanceRun(&json!("NOPE"), &c).unwrap_err(), "dev-acceptance-run: contract 'NOPE' not found");
    }

    #[test]
    fn test_kind_real_subprocess_green_and_red_in_worktree() {
        let dir = temp_dir("test-kind");
        let c = db();
        let acceptance = json!([
            { "kind": "test", "cmd": if cfg!(windows) { "exit 0" } else { "true" } },
            { "kind": "test", "cmd": if cfg!(windows) { "exit 1" } else { "false" } },
        ]);
        insert_contract(&c, "C-TEST", None, Some(dir.to_str().unwrap()), "[]", &acceptance.to_string(), None);
        let r = devAcceptanceRun(&json!("C-TEST"), &c).unwrap();
        assert_eq!(r["count"], json!(2));
        assert_eq!(r["all_passed"], json!(false));
        assert_eq!(r["results"][0]["passed"], json!(true));
        assert_eq!(r["results"][0]["evidence"]["mode"], json!("worktree-cwd"));
        assert_eq!(r["results"][1]["passed"], json!(false));
        assert_eq!(r["results"][1]["evidence"]["exit"], json!(1));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cli_kind_shares_run_test() {
        let dir = temp_dir("cli-kind");
        let c = db();
        let acceptance = json!([{ "kind": "cli", "cmd": if cfg!(windows) { "exit 0" } else { "true" } }]);
        insert_contract(&c, "C-CLI", None, Some(dir.to_str().unwrap()), "[]", &acceptance.to_string(), None);
        let r = devAcceptanceRun(&json!("C-CLI"), &c).unwrap();
        assert_eq!(r["results"][0]["passed"], json!(true));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn file_exists_kind_checks_worktree_path_and_min_bytes_and_must_contain() {
        let dir = temp_dir("file-exists");
        std::fs::write(dir.join("a.txt"), "hello world").unwrap();
        let c = db();
        let acceptance = json!([
            { "kind": "file_exists", "path": "a.txt", "min_bytes": 5, "must_contain": ["hello"] },
            { "kind": "file_exists", "path": "a.txt", "min_bytes": 999 },
            { "kind": "file-exists", "path": "missing.txt" },
        ]);
        insert_contract(&c, "C-FE", None, Some(dir.to_str().unwrap()), "[]", &acceptance.to_string(), None);
        let r = devAcceptanceRun(&json!("C-FE"), &c).unwrap();
        assert_eq!(r["results"][0]["passed"], json!(true));
        assert_eq!(r["results"][1]["passed"], json!(false));
        assert_eq!(r["results"][1]["evidence"]["reason"], json!("too_small"));
        assert_eq!(r["results"][2]["passed"], json!(false));
        assert_eq!(r["results"][2]["evidence"]["reason"], json!("missing"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn file_exists_extracts_path_from_prose_when_no_machine_field() {
        let dir = temp_dir("file-exists-prose");
        std::fs::write(dir.join("index.js"), "x").unwrap();
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("src/index.js"), "x").unwrap();
        let c = db();
        let acceptance = json!([
            { "kind": "file_exists", "done_criteria": "the file src/index.js exists and is wired" },
        ]);
        insert_contract(&c, "C-FE-PROSE", None, Some(dir.to_str().unwrap()), "[]", &acceptance.to_string(), None);
        let r = devAcceptanceRun(&json!("C-FE-PROSE"), &c).unwrap();
        assert_eq!(r["results"][0]["passed"], json!(true));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sql_kind_expect_value_min_and_row_truthiness() {
        let c = db();
        c.execute_batch("CREATE TABLE widgets (n INTEGER); INSERT INTO widgets VALUES (7);").unwrap();
        let acceptance = json!([
            { "kind": "sql", "query": "SELECT n AS c FROM widgets", "expect_value_min": 5, "expect_value_path": "$.c" },
            { "kind": "sql", "query": "SELECT n AS c FROM widgets", "expect_value_min": 100, "expect_value_path": "$.c" },
            { "kind": "sql", "query": "SELECT 1 AS x FROM widgets WHERE n = 999" },
            { "kind": "sql", "query": "SELECT * FROM nope_no_such_table" },
        ]);
        insert_contract(&c, "C-SQL", None, None, "[]", &acceptance.to_string(), None);
        let r = devAcceptanceRun(&json!("C-SQL"), &c).unwrap();
        assert_eq!(r["results"][0]["passed"], json!(true));
        assert_eq!(r["results"][1]["passed"], json!(false));
        assert_eq!(r["results"][2]["passed"], json!(false)); // no row → falsy
        assert_eq!(r["results"][3]["passed"], json!(false));
        assert!(r["results"][3]["evidence"]["error"].is_string());
    }

    #[test]
    fn diff_in_paths_sandbox_mode_offenders_and_generated_exemption() {
        let dir = temp_dir("diff-sandbox");
        std::fs::create_dir_all(dir.join("live/src")).unwrap();
        std::fs::create_dir_all(dir.join("sb/src")).unwrap();
        std::fs::create_dir_all(dir.join("live/dist")).unwrap();
        std::fs::create_dir_all(dir.join("sb/dist")).unwrap();
        std::fs::write(dir.join("live/src/a.js"), "old").unwrap();
        std::fs::write(dir.join("sb/src/a.js"), "new").unwrap(); // changed, allowed
        std::fs::write(dir.join("live/src/b.js"), "old").unwrap();
        std::fs::write(dir.join("sb/src/b.js"), "new").unwrap(); // changed, offender
        std::fs::write(dir.join("live/dist/gen.js"), "old").unwrap();
        std::fs::write(dir.join("sb/dist/gen.js"), "new").unwrap(); // changed but generated → exempt

        let c = db();
        let files_touched = json!(["src/a.js", "src/b.js", "dist/gen.js"]);
        let acceptance = json!([{ "kind": "diff_in_paths", "allowed": ["src/a.js"] }]);
        c.execute(
            "INSERT INTO contracts (slug, sandbox_path, files_touched_json, acceptance_json, generated_paths_json) VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params!["C-DIFF", dir.join("sb").to_str().unwrap(), files_touched.to_string(), acceptance.to_string(), json!(["dist"]).to_string()],
        ).unwrap();
        let r = with_repo_root(dir.join("live").to_str().unwrap(), || devAcceptanceRun(&json!("C-DIFF"), &c).unwrap());
        assert_eq!(r["results"][0]["passed"], json!(false));
        assert_eq!(r["results"][0]["evidence"]["mode"], json!("sandbox-vs-original"));
        assert_eq!(r["results"][0]["evidence"]["offenders"], json!(["src/b.js"]));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn no_law_violation_kind_law_not_found_and_zero_count() {
        let c = db();
        c.execute("INSERT INTO laws.laws (id, slug) VALUES (1, 'GOOD_LAW')", []).unwrap();
        let acceptance = json!([
            { "kind": "no_law_violation", "law_slug": "GOOD_LAW" },
            { "kind": "no_law_violation", "law_slug": "GHOST_LAW" },
        ]);
        insert_contract(&c, "C-LAW", None, None, "[]", &acceptance.to_string(), None);
        let r = devAcceptanceRun(&json!("C-LAW"), &c).unwrap();
        assert_eq!(r["results"][0]["passed"], json!(true));
        assert_eq!(r["results"][1]["passed"], json!(false));
        assert_eq!(r["results"][1]["evidence"]["reason"], json!("law_not_found"));
    }

    #[test]
    fn skill_returns_kind_dispatches_through_action_run_and_checks_jsonpath() {
        let c = db();
        c.execute_batch(
            "CREATE TABLE actions (name TEXT UNIQUE, description TEXT, owner_agent TEXT, steps_json TEXT, returns TEXT, on_failure TEXT, kind TEXT);
             CREATE TABLE runtime_events (id INTEGER PRIMARY KEY AUTOINCREMENT, ts TEXT NOT NULL DEFAULT '2026-01-01 00:00:00', parent_event_id INTEGER, actor TEXT NOT NULL, agent TEXT, action TEXT, skill TEXT, duration_ms REAL, outcome TEXT NOT NULL, error_code TEXT, context_json TEXT, user_goal_id INTEGER, source TEXT);
             CREATE TABLE cards (slug TEXT, status TEXT, score REAL);
             INSERT INTO cards VALUES ('A','doing',5),('B','backlog',NULL);
             INSERT INTO actions (name, steps_json, returns, kind) VALUES
               ('board-keeper.card.list', '[{\"skill\":\"card-list\",\"input\":{},\"as\":\"cards\"}]', '$.results.cards', 'atomic');",
        ).unwrap();
        let acceptance = json!([
            { "kind": "skill_returns", "action": "board-keeper.card.list", "expect_jsonpath": { "$[*].slug": "A" } },
            { "kind": "skill_returns", "action": "no-such-action" },
        ]);
        insert_contract(&c, "C-SKILL", None, None, "[]", &acceptance.to_string(), None);
        let r = devAcceptanceRun(&json!("C-SKILL"), &c).unwrap();
        assert_eq!(r["results"][0]["passed"], json!(true));
        assert_eq!(r["results"][1]["passed"], json!(false));
        assert!(r["results"][1]["evidence"]["error"].as_str().unwrap().contains("not found"));
    }

    #[test]
    fn prose_kind_and_class_b_chokepoint_court_judge_instead_of_crashing() {
        let c = db();
        let acceptance = json!([
            { "kind": "grep", "done_criteria": "the header renders correctly" },
            { "kind": "test", "done_criteria": "should work" }, // machine kind, missing `cmd`, has prose → court-judged
            { "kind": "made_up_kind" }, // no prose, no machine field → unknown_kind
        ]);
        insert_contract(&c, "C-PROSE", None, None, "[]", &acceptance.to_string(), None);
        let r = devAcceptanceRun(&json!("C-PROSE"), &c).unwrap();
        assert_eq!(r["results"][0]["skipped"], json!(true));
        assert_eq!(r["results"][0]["evidence"]["criterion"], json!("the header renders correctly"));
        assert_eq!(r["results"][1]["skipped"], json!(true));
        assert_eq!(r["results"][2]["passed"], json!(false));
        assert_eq!(r["results"][2]["evidence"]["reason"], json!("unknown_kind"));
    }

    #[test]
    fn bootstrap_cmd_runs_once_before_acceptance_and_records_outcome() {
        let dir = temp_dir("bootstrap");
        let c = db();
        insert_contract(&c, "C-BOOT", Some(dir.to_str().unwrap()), None, "[]", "[]", Some(if cfg!(windows) { "exit 0" } else { "true" }));
        // sandbox_path is absolute already, so join(".", abs_path) still resolves to itself on
        // both platforms — Path::join with an absolute rhs replaces the base entirely.
        let r = with_repo_root(".", || devAcceptanceRun(&json!("C-BOOT"), &c).unwrap());
        assert_eq!(r["bootstrap"]["exit"], json!(0));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
