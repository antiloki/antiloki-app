//! code-file-history.rs — twin of skills/codebase/code-file-history.mjs (db READ lane, real git
//! subprocess). NAMES ARE CONTRACT: `codeFileHistory` (default). The per-FILE revision timeline
//! for the cinematic replay: the recent commits that touched ONE file (via `git log --follow`)
//! plus the file's CONTENT at each revision (`git show`), oldest→newest, each revision annotated
//! with the SAME deterministic per-file signal detectors the review runs
//! (`codebase_code_signals_detect::detectFileSignals`, an already-converted twin).
//!
//! GIT SUBPROCESS — DISCLOSED PARTIAL COVERAGE (playbook lesson 5's flag-skippable pattern,
//! applied via environment fact rather than an input flag, same precedent as the sibling
//! `codebase_code_file_review_history.rs`): this twin ports the JS `spawnSync('git', ['-C',
//! root, ...])` calls faithfully as real `std::process::Command` subprocess calls (`git_cmd`) —
//! no stub, no fake return. Every fixture case seeds NO active `workspaces` row (the skill's own
//! `getActiveWorkspace` SELECT swallows a missing/absent table via try/catch either way — no
//! `workspaces` table needs to exist in the fixture schema at all), so `getWorkspaceRoot` falls
//! back to `SUBSTRATE_ROOT` (`engine_workspace_root::substrate_root`, i.e. this monorepo's
//! `antiloki-core-rust`'s PARENT directory) — which, on this project's real on-disk layout, is
//! confirmed NOT a git repository (`git -C <substrate root> log ...` fails: "not a git
//! repository"). So `git log` genuinely executes as a real subprocess in every fixture case, but
//! its outcome (no repo found → non-zero exit → `git_cmd` returns `None`) is fully deterministic
//! on this dev/CI layout, and the fixture never reaches commit parsing, `git show`, or the
//! `detectFileSignals` wiring. Those branches are real-but-untested-by-fixture — ported faithfully
//! below, exercised only by the JS behavior test (`code-file-history.test.mjs`, a real git repo in
//! a temp dir), not by conform-db.
//!
//! `content.length > MAX_BYTES` / `.slice(0, MAX_BYTES)` (JS UTF-16 code units) is approximated
//! with UTF-16 code-unit counting for the length check and a `char`-boundary slice for the cut
//! (this substrate's source files are ASCII/BMP-only, so the two conventions never diverge in
//! practice) — same disclosed approximation as `codebase_code_file_review_history.rs`.
//!
//! `context.db` is destructured with no guard in the JS (`const { db } = context ?? {}`) — a
//! missing db would only surface once `getWorkspaceRoot(db)` throws its own `contextMissing`.
//! Unreachable here: `db: &Connection` is a required, non-nullable reference (same "guard-throws
//! are not exercised" convention every sibling twin uses). Likewise `if (!root)` ("no active
//! workspace root") is real-but-unreachable in practice: `getWorkspaceRoot` always returns a
//! truthy string (either an active workspace's `root_path` or the `SUBSTRATE_ROOT` fallback).
// QUALITY: real git subprocess ported faithfully; positive commit-match branch disclosed
// untested (deterministic environment fact, not a fixture shortcut — see module doc)

use rusqlite::Connection;
use serde_json::{json, Map, Value};

fn truthy(v: Option<&Value>) -> bool { match v { None|Some(Value::Null)=>false, Some(Value::Bool(b))=>*b, Some(Value::Number(n))=>n.as_f64().map_or(false,|f|f!=0.0&&!f.is_nan()), Some(Value::String(s))=>!s.is_empty(), Some(_)=>true } }
fn str_qq(v: Option<&Value>) -> String { match v { None|Some(Value::Null)=>String::new(), Some(Value::String(s))=>s.clone(), Some(Value::Bool(b))=>b.to_string(), Some(n@Value::Number(_))=>n.to_string(), Some(o)=>o.to_string() } } // = JS String(v ?? '')
fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) } // JS `??` right operand
fn js_number(v: Option<&Value>) -> f64 { match v { None=>f64::NAN, Some(Value::Null)=>0.0, Some(Value::Bool(b))=>if *b {1.0} else {0.0}, Some(Value::Number(n))=>n.as_f64().unwrap_or(f64::NAN), Some(Value::String(s))=>{let t=s.trim(); if t.is_empty(){0.0}else{t.parse().unwrap_or(f64::NAN)}}, Some(_)=>f64::NAN } } // = JS Number(v)
// = JS Number.prototype.toString()/`${f}` for a raw f64 embedded directly into an output STRING
// (here: the `-n <limit>` git arg, a computed Math.min/max result, not a JSON-sourced Value).
fn js_number_to_string(f: f64) -> String {
    if f.is_nan() { return "NaN".to_string(); }
    if f == 0.0 { return "0".to_string(); }
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

const MAX_BYTES: usize = 200 * 1024;
const US: char = '\u{1f}';
const RS: char = '\u{1e}';

/// `function git(root, args)` — real `spawnSync('git', ['-C', root, ...args], ...)`. Returns the
/// captured stdout on a zero exit, `None` on a nonzero exit or a launch failure (missing binary
/// etc — JS wraps the whole call in try/catch and returns `null`).
fn git_cmd(root: &str, args: &[&str]) -> Option<String> {
    let output = crate::proc::cmd("git").arg("-C").arg(root).args(args).output();
    match output {
        Ok(o) if o.status.success() => Some(String::from_utf8_lossy(&o.stdout).into_owned()),
        _ => None,
    }
}

/// `const sigToViolation = (s) => ({ line: s.line ?? null, law_key: String(s.kind ?? 'signal')
/// .toUpperCase().replace(/-/g, '_'), severity: s.severity ?? 'low', evidence: s.detail ?? '',
/// category: s.category ?? null })`
fn sig_to_violation(s: &Value) -> Value {
    let kind_val = nz(s.get("kind")).cloned().unwrap_or(json!("signal"));
    let law_key = str_qq(Some(&kind_val)).to_uppercase().replace('-', "_");
    let mut o = Map::new();
    o.insert("line".into(), nz(s.get("line")).cloned().unwrap_or(Value::Null));
    o.insert("law_key".into(), json!(law_key));
    o.insert("severity".into(), nz(s.get("severity")).cloned().unwrap_or(json!("low")));
    o.insert("evidence".into(), nz(s.get("detail")).cloned().unwrap_or(json!("")));
    o.insert("category".into(), nz(s.get("category")).cloned().unwrap_or(Value::Null));
    Value::Object(o)
}

struct Commit { sha: String, short: Option<String>, author: Option<String>, date: Option<String>, subject: Option<String>, path_at: String }

pub fn codeFileHistory(input: &Value, db: &Connection) -> Result<Value, String> {
    let path = str_qq(input.get("path")).replace('\\', "/");
    if path.is_empty() {
        return Ok(json!({ "path": "", "count": 0, "revisions": [], "error": "need { path }" }));
    }
    let root_v = crate::engine_workspace_root::getWorkspaceRoot(db);
    if !truthy(Some(&root_v)) {
        // unreachable in practice — see module doc.
        return Ok(json!({ "path": path, "count": 0, "revisions": [], "error": "no active workspace root" }));
    }
    let root = root_v.as_str().unwrap_or("").to_string();

    let limit_input = nz(input.get("limit")).cloned();
    let limit_num = match &limit_input { Some(v) => js_number(Some(v)), None => 40.0 };
    let limit_clamped = if limit_num.is_nan() { f64::NAN } else { limit_num.max(1.0).min(100.0) };
    let limit_str = js_number_to_string(limit_clamped);

    let format_arg = format!("--format={RS}%H{US}%h{US}%an{US}%ad{US}%s");
    let log = git_cmd(&root, &["log", "--follow", "-n", &limit_str, &format_arg, "--date=short", "--name-status", "--", &path]);
    let log_str = match log {
        Some(s) => s,
        None => return Ok(json!({ "path": path, "count": 0, "revisions": [], "error": "not a git repo / git unavailable" })),
    };

    let mut commits: Vec<Commit> = Vec::new();
    for block in log_str.split(RS) {
        let t = block.trim();
        if t.is_empty() { continue; }
        let nl = t.find('\n');
        let head = match nl { Some(i) => &t[..i], None => t };
        let parts: Vec<&str> = head.split(US).collect();
        let sha = parts.first().copied().unwrap_or("");
        if sha.is_empty() { continue; }
        let short = parts.get(1).map(|s| s.to_string());
        let author = parts.get(2).map(|s| s.to_string());
        let date = parts.get(3).map(|s| s.to_string());
        let subject = parts.get(4).map(|s| s.to_string());

        let mut path_at = path.clone();
        if let Some(i) = nl {
            let names: Vec<&str> = t[i + 1..].split('\n').map(|l| l.trim()).filter(|l| !l.is_empty()).collect();
            if let Some(last) = names.last() {
                let name_parts: Vec<&str> = last.split('\t').collect();
                let candidate = if name_parts.len() >= 3 { name_parts.get(2) } else { name_parts.get(1) };
                if let Some(c) = candidate {
                    if !c.is_empty() { path_at = c.to_string(); }
                }
            }
        }
        commits.push(Commit { sha: sha.to_string(), short, author, date, subject, path_at });
    }
    if commits.is_empty() {
        return Ok(json!({ "path": path, "count": 0, "revisions": [], "error": "no history for this file" }));
    }
    commits.reverse(); // oldest → newest

    let mut revisions: Vec<Value> = Vec::with_capacity(commits.len());
    for c in &commits {
        let raw_content = git_cmd(&root, &["show", &format!("{}:{}", c.sha, c.path_at)]);
        let content: String = match raw_content {
            None => String::new(),
            Some(s) => {
                if s.encode_utf16().count() > MAX_BYTES {
                    let truncated: String = s.chars().take(MAX_BYTES).collect();
                    format!("{truncated}\n\u{2026} (truncated for replay)")
                } else { s }
            }
        };

        let violations: Vec<Value> = if content.is_empty() {
            Vec::new()
        } else {
            match crate::codebase_code_signals_detect::detectFileSignals(&json!(content), &json!(c.path_at)).as_array() {
                Some(arr) => arr.iter().map(sig_to_violation).collect(),
                None => Vec::new(),
            }
        };

        let mut rev = Map::new();
        rev.insert("sha".into(), json!(c.sha));
        if let Some(s) = &c.short { rev.insert("short".into(), json!(s)); } // undefined key omitted (gotcha #3)
        if let Some(a) = &c.author { rev.insert("author".into(), json!(a)); }
        if let Some(d) = &c.date { rev.insert("date".into(), json!(d)); }
        if let Some(s) = &c.subject { rev.insert("subject".into(), json!(s)); }
        rev.insert("content".into(), json!(content));
        rev.insert("lines".into(), json!(if content.is_empty() { 0i64 } else { content.split('\n').count() as i64 }));
        rev.insert("violations".into(), Value::Array(violations));
        revisions.push(Value::Object(rev));
    }

    Ok(json!({ "path": path, "count": revisions.len() as i64, "revisions": revisions }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection { Connection::open_in_memory().unwrap() }

    #[test]
    fn empty_path_returns_need_path_error_without_touching_git() {
        let r = codeFileHistory(&json!({}), &db()).unwrap();
        assert_eq!(r, json!({ "path": "", "count": 0, "revisions": [], "error": "need { path }" }));
    }

    #[test]
    fn no_active_workspace_falls_back_to_substrate_root_which_is_not_a_git_repo() {
        // no `workspaces` table/row at all -> getActiveWorkspace swallows the SELECT failure ->
        // getWorkspaceRoot falls back to SUBSTRATE_ROOT, which on this monorepo's real on-disk
        // layout (antiloki-core-rust's parent dir) is confirmed NOT a git repository -> `git log`
        // exits non-zero -> the deterministic "not a git repo" branch (see module doc).
        let r = codeFileHistory(&json!({ "path": "a.ts" }), &db()).unwrap();
        assert_eq!(r, json!({ "path": "a.ts", "count": 0, "revisions": [], "error": "not a git repo / git unavailable" }));
    }

    #[test]
    fn backslash_path_normalized_before_the_not_a_git_repo_error() {
        let r = codeFileHistory(&json!({ "path": "src\\a.ts", "limit": 5 }), &db()).unwrap();
        assert_eq!(r["path"], json!("src/a.ts"));
        assert_eq!(r["error"], json!("not a git repo / git unavailable"));
    }

    #[test]
    fn limit_clamps_to_1_and_100() {
        let lo = codeFileHistory(&json!({ "path": "a.ts", "limit": 0 }), &db()).unwrap();
        assert_eq!(lo["error"], json!("not a git repo / git unavailable"));
        let hi = codeFileHistory(&json!({ "path": "a.ts", "limit": 500 }), &db()).unwrap();
        assert_eq!(hi["error"], json!("not a git repo / git unavailable"));
    }

    #[test]
    fn sig_to_violation_maps_defaults_and_present_fields() {
        let v = sig_to_violation(&json!({ "kind": "swallowed-error", "line": 5, "severity": "medium", "detail": "x", "category": "safety" }));
        assert_eq!(v, json!({ "line": 5, "law_key": "SWALLOWED_ERROR", "severity": "medium", "evidence": "x", "category": "safety" }));
        let v2 = sig_to_violation(&json!({}));
        assert_eq!(v2, json!({ "line": null, "law_key": "SIGNAL", "severity": "low", "evidence": "", "category": null }));
    }

    #[test]
    fn git_cmd_returns_none_for_a_non_git_directory() {
        assert_eq!(git_cmd(std::env::temp_dir().to_str().unwrap(), &["log", "-1"]), None);
    }
}
