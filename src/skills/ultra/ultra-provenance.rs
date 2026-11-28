//! ultra-provenance.rs — twin of skills/ultra/ultra-provenance.mjs (BESPOKE NATIVE — see below).
//! NAMES ARE CONTRACT: `ultraProvenance` (default). ROOT CAUSE AS DATA (docs/ultra-review-plan.html
//! Pillar A: "never stop at 'high complexity'"): for one unit, walk the chain the substrate
//! already records — with ZERO AI: file → its `git log` commits → the card slugs those commits'
//! subjects name → those cards + their latest contract → the decisions recorded around them. A
//! missing link is reported missing, never inferred.
//!
//! This unit is near-identical in shape to `constitution/constitution-provenance.mjs` ("folded in
//! from the retired ultra screen" per that module's own doc comment) — same SLUG_RE, same NOISE
//! set, same three-stage chain, same `context.exec_fn` testability seam. This twin mirrors
//! `constitution_constitution_provenance.rs` field-for-field, renamed for this module.
//!
//! BESPOKE-NATIVE CLASSIFICATION (playbook's flag-skippable rule does NOT apply here): the JS
//! `run(...)` (`execFileSync('git', ['-C', root, 'log', ...])`) call fires on EVERY success path
//! once `context.db` and `input.path` are present — there is no input flag or optional field
//! whose absence steers around it. The JS `context.exec_fn` override exists only as a JS-unit-test
//! mocking seam; the conform-db harness's fixtures are plain JSON and cannot inject a function, so
//! from the harness's perspective this unit unconditionally shells out. Per the crossing
//! playbook's bespoke-native rule: real, correct code below (a genuine `std::process::Command`
//! git-log call, ported field-for-field), proven by `#[cfg(test)]` native tests against a real
//! temp git repo (constitution-provenance.rs / god-driver-halt.rs / stack-facts.rs precedent) —
//! NO conform-db fixture, NO frontier entry, lib.rs carries only the `pub mod` line.
//!
//! `run` swallows ANY execFileSync failure (non-repo path, git absent, nonzero exit, spawn error)
//! into `commits = []` — same try/catch-to-empty-array shape ported here as a match on
//! `Command::output()`.
//!
//! Per-slug card/contract lookups mirror the JS try/catch precisely: a missing card row is a plain
//! skip (`continue`); a query ERROR on either the card OR contract statement is the outer catch —
//! the whole slug produces no entry (not just the contract fields blanked). The `body_json` →
//! `why` extraction has its OWN inner try/catch (JSON.parse failure → `why: null`, does NOT abort
//! the outer per-slug block) — modeled as a separate `Option` chain that never short-circuits the
//! surrounding card push.
//!
//! `context.db` missing / `input.path` empty → JS throws via `err.required(...)`. Not modeled:
//! `db: &Connection` is a required, non-nullable reference here (same "guard-throws are not
//! exercised" convention as every sibling twin); the empty-`path` throw IS reachable with a valid
//! `db`, so it is modeled as `Err(String)` (still not exercised by a test — playbook lesson 9,
//! throws are fragile and this unit carries no fixture anyway).
// QUALITY: 75/100 — real git-log subprocess + real cross-table lookups, bespoke-native (no
// conform-db coverage); proven via committed temp-repo native tests instead.

use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::collections::HashSet;
use std::sync::OnceLock;

fn truthy(v: Option<&Value>) -> bool { match v { None|Some(Value::Null)=>false, Some(Value::Bool(b))=>*b, Some(Value::Number(n))=>n.as_f64().map_or(false,|f|f!=0.0&&!f.is_nan()), Some(Value::String(s))=>!s.is_empty(), Some(_)=>true } }
fn str_qq(v: Option<&Value>) -> String { match v { None|Some(Value::Null)=>String::new(), Some(Value::String(s))=>s.clone(), Some(Value::Bool(b))=>b.to_string(), Some(n@Value::Number(_))=>n.to_string(), Some(o)=>o.to_string() } } // = JS String(v ?? '')
fn js_string(v: &Value) -> String { match v { Value::Null=>"null".into(), Value::String(s)=>s.clone(), Value::Number(n)=>n.to_string(), Value::Bool(b)=>b.to_string(), Value::Array(a)=>a.iter().map(js_string).collect::<Vec<_>>().join(","), Value::Object(_)=>"[object Object]".into() } } // = JS String(v)
fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) } // JS `??` right operand
fn js_number(v: Option<&Value>) -> f64 { match v { None=>f64::NAN, Some(Value::Null)=>0.0, Some(Value::Bool(b))=>if *b {1.0} else {0.0}, Some(Value::Number(n))=>n.as_f64().unwrap_or(f64::NAN), Some(Value::String(s))=>{let t=s.trim(); if t.is_empty(){0.0}else{t.parse().unwrap_or(f64::NAN)}}, Some(_)=>f64::NAN } } // = JS Number(v)

// = JS Number.prototype.toString() for a raw f64 — used to build the `-n <limit>` git CLI arg
// (limit is a computed Math.min/Math.max result, not a JSON-sourced Value::Number).
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

// same order/values as the JS module-level SLUG_RE / NOISE.
fn slug_re() -> &'static regex::Regex {
    static R: OnceLock<regex::Regex> = OnceLock::new();
    R.get_or_init(|| regex::Regex::new(r"\b[A-Z][A-Z0-9]{1,}(?:-[A-Z0-9]+){1,}\b").unwrap())
}
const NOISE: [&str; 12] = ["AI", "API", "BE", "FE", "UI", "DB", "JSON", "HTTP", "SQL", "TODO", "WIP", "CI-CD"];

struct GitCommit { sha: String, author: String, ts: String, subject: String }

/// `execFileSync('git', ['-C', root, 'log', '-n', String(limit), '--format=%h%x1f%an%x1f%aI%x1f%s', '--', path], { cwd: root, ... })`
/// — see module doc: real, unconditional subprocess call. Any spawn/nonzero-exit failure
/// collapses to an empty commit list (JS `try { ... } catch { commits = []; }`).
fn run_git_log(root: &str, path: &str, limit: f64) -> Vec<GitCommit> {
    let limit_str = js_number_to_string(limit);
    let output = crate::proc::cmd("git")
        .args(["-C", root, "log", "-n", &limit_str, "--format=%h%x1f%an%x1f%aI%x1f%s", "--", path])
        .current_dir(root)
        .output();
    let stdout = match output {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).into_owned(),
        _ => return Vec::new(),
    };
    stdout
        .split('\n')
        .filter(|l| !l.is_empty())
        .map(|line| {
            // full split (not splitn) so `subject` picks exactly the 4th field, matching JS's
            // `[sha, author, ts, subject] = line.split('\x1f')` destructure (extra fields, if
            // any, are dropped by both — unreachable in practice since %s is the last format
            // specifier and a real commit subject never contains the \x1f control char).
            let mut parts = line.split('\u{1f}');
            let sha = parts.next().unwrap_or("").to_string();
            let author = parts.next().unwrap_or("").to_string();
            let ts = parts.next().unwrap_or("").to_string();
            let subject_raw = parts.next().unwrap_or("");
            let subject: String = subject_raw.chars().take(200).collect();
            GitCommit { sha, author, ts, subject }
        })
        .collect()
}

/// `export default function ultraProvenance(input, context)`
pub fn ultraProvenance(input: &Value, db: &Connection) -> Result<Value, String> {
    let path = str_qq(nz(input.get("path"))).trim().to_string();
    if path.is_empty() {
        return Err("ultra-provenance: path is required".into());
    }
    let raw_limit = js_number(input.get("limit"));
    let base_limit = if raw_limit.is_nan() || raw_limit == 0.0 { 6.0 } else { raw_limit };
    let limit = base_limit.max(1.0).min(20.0);

    let root_v = crate::engine_workspace_root::getWorkspaceRoot(db);
    let root = root_v.as_str().unwrap_or_default().to_string();

    let commits = run_git_log(&root, &path, limit);

    let mut seen: HashSet<String> = HashSet::new();
    let mut slugs: Vec<String> = Vec::new();
    'commits: for c in &commits {
        for m in slug_re().find_iter(&c.subject) {
            let s = m.as_str().to_string();
            if NOISE.contains(&s.as_str()) || s.chars().count() < 6 { continue; }
            if seen.insert(s.clone()) {
                slugs.push(s);
                if slugs.len() >= 8 { break 'commits; }
            }
        }
    }

    let mut cards: Vec<Value> = Vec::new();
    for slug in &slugs {
        let card = match crate::db_json::query_rows(db, "SELECT slug, title, status, body_json FROM cards WHERE slug = ?", &[json!(slug)]) {
            Ok(v) => v.as_array().and_then(|a| a.first()).cloned(),
            Err(_) => continue, // card/contract tables absent → skip (outer catch)
        };
        let card = match card { Some(c) => c, None => continue };

        // inner try/catch: JSON.parse failure degrades to why=null WITHOUT aborting this slug.
        let body_json_str: String = match card.get("body_json") {
            None | Some(Value::Null) => "{}".to_string(),
            Some(Value::String(s)) => s.clone(),
            Some(other) => js_string(other),
        };
        let why_raw: Option<Value> = serde_json::from_str::<Value>(&body_json_str)
            .ok()
            .and_then(|v| v.get("why").cloned())
            .filter(|v| !v.is_null());
        let why_out: Value = match &why_raw {
            Some(v) if truthy(Some(v)) => json!(js_string(v).chars().take(300).collect::<String>()),
            _ => Value::Null,
        };

        let contract: Value = match crate::db_json::query_rows(db, "SELECT slug, status FROM contracts WHERE card_slug = ? ORDER BY id DESC LIMIT 1", &[json!(slug)]) {
            Ok(v) => v.as_array().and_then(|a| a.first()).cloned().unwrap_or(Value::Null),
            Err(_) => continue, // outer catch: same as the card query failing
        };

        let mut o = Map::new();
        o.insert("slug".into(), card.get("slug").cloned().unwrap_or(Value::Null));
        o.insert("title".into(), card.get("title").cloned().unwrap_or(Value::Null));
        o.insert("status".into(), card.get("status").cloned().unwrap_or(Value::Null));
        o.insert("why".into(), why_out);
        o.insert("contract".into(), contract.get("slug").cloned().unwrap_or(Value::Null));
        o.insert("contract_status".into(), contract.get("status").cloned().unwrap_or(Value::Null));
        cards.push(Value::Object(o));
    }

    let mut decisions: Vec<Value> = Vec::new();
    for slug in slugs.iter().take(4) {
        let like = format!("%{slug}%");
        let rows = crate::db_json::query_rows(
            db,
            "SELECT title, situation, guidance, type FROM memory.memories WHERE type IN ('decision','correction') AND superseded_by IS NULL AND (source_card = ? OR title LIKE ?) ORDER BY id DESC LIMIT 2",
            &[json!(slug), json!(like)],
        );
        if let Ok(v) = rows {
            for m in v.as_array().cloned().unwrap_or_default() {
                let excerpt_src = nz(m.get("guidance")).or_else(|| nz(m.get("situation")));
                let excerpt: String = str_qq(excerpt_src).chars().take(240).collect();
                decisions.push(json!({
                    "slug": slug,
                    "type": m.get("type").cloned().unwrap_or(Value::Null),
                    "title": m.get("title").cloned().unwrap_or(Value::Null),
                    "excerpt": excerpt,
                }));
            }
        } // memory shape differs → skip
    }

    let commits_out: Vec<Value> = commits
        .iter()
        .map(|c| json!({ "sha": c.sha, "author": c.author, "ts": c.ts, "subject": c.subject }))
        .collect();

    let mut out = Map::new();
    out.insert("path".into(), json!(path));
    out.insert("commits".into(), json!(commits_out));
    out.insert("cards".into(), json!(cards));
    out.insert("decisions".into(), json!(decisions));
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use std::process::Command;

    fn temp_repo(tag: &str) -> PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!("antiloki-ultra-provenance-rs-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        run(&dir, &["init", "-q"]);
        run(&dir, &["config", "user.email", "test@example.com"]);
        run(&dir, &["config", "user.name", "Test"]);
        dir
    }

    fn run(dir: &PathBuf, args: &[&str]) {
        let st = Command::new("git").args(args).current_dir(dir).output().unwrap();
        assert!(st.status.success(), "git {:?} failed: {}", args, String::from_utf8_lossy(&st.stderr));
    }

    fn commit_file(dir: &PathBuf, rel: &str, contents: &str, message: &str) {
        let full = dir.join(rel);
        if let Some(parent) = full.parent() { fs::create_dir_all(parent).unwrap(); }
        fs::write(&full, contents).unwrap();
        run(dir, &["add", rel]);
        run(dir, &["commit", "-q", "-m", message]);
    }

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS memory").unwrap();
        c.execute_batch(
            "CREATE TABLE workspaces (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL UNIQUE, \
             root_path TEXT NOT NULL, vcs TEXT NOT NULL DEFAULT 'git', is_active INTEGER NOT NULL DEFAULT 0, \
             created_at TEXT NOT NULL DEFAULT '2026-01-01 00:00:00'); \
             CREATE TABLE cards (id INTEGER PRIMARY KEY AUTOINCREMENT, slug TEXT NOT NULL UNIQUE, title TEXT, status TEXT, body_json TEXT); \
             CREATE TABLE contracts (id INTEGER PRIMARY KEY AUTOINCREMENT, slug TEXT, card_slug TEXT, status TEXT); \
             CREATE TABLE memory.memories (id INTEGER PRIMARY KEY AUTOINCREMENT, type TEXT, title TEXT, situation TEXT, guidance TEXT, source_card TEXT, superseded_by INTEGER)",
        ).unwrap();
        c
    }
    fn seed_active(c: &Connection, root: &str) {
        c.execute("INSERT INTO workspaces (name, root_path, is_active) VALUES ('x', ?1, 1)", rusqlite::params![root]).unwrap();
    }

    #[test]
    fn full_chain_dedupes_filters_noise_and_joins_card_contract_decision() {
        let dir = temp_repo("full-chain");
        commit_file(&dir, "src/x.mjs", "// v1", "chore: setup");
        commit_file(&dir, "src/x.mjs", "// v2", "AB-CD too short slug");
        commit_file(&dir, "src/x.mjs", "// v3", "CI-CD standardization (noise)");
        commit_file(&dir, "src/x.mjs", "// v4", "FEAT-PROVENANCE-DEMO: add feature");
        commit_file(&dir, "src/x.mjs", "// v5", "fix bug for FEAT-PROVENANCE-DEMO again");

        let c = db();
        seed_active(&c, dir.to_str().unwrap());
        c.execute(
            "INSERT INTO cards (slug, title, status, body_json) VALUES ('FEAT-PROVENANCE-DEMO', 'Provenance demo card', 'active', '{\"why\":\"because tests need it\"}')",
            [],
        ).unwrap();
        c.execute("INSERT INTO contracts (slug, card_slug, status) VALUES ('CONTRACT-OLD', 'FEAT-PROVENANCE-DEMO', 'draft')", []).unwrap();
        c.execute("INSERT INTO contracts (slug, card_slug, status) VALUES ('CONTRACT-NEW', 'FEAT-PROVENANCE-DEMO', 'signed')", []).unwrap();
        c.execute(
            "INSERT INTO memory.memories (type, title, situation, guidance, source_card, superseded_by) VALUES ('decision', 'use provenance', 'needed root cause', 'wire it up', 'FEAT-PROVENANCE-DEMO', NULL)",
            [],
        ).unwrap();

        let r = ultraProvenance(&json!({ "path": "src/x.mjs" }), &c).unwrap();
        assert_eq!(r["path"], json!("src/x.mjs"));
        assert_eq!(r["commits"].as_array().unwrap().len(), 5);
        assert_eq!(r["commits"][0]["subject"], json!("fix bug for FEAT-PROVENANCE-DEMO again")); // newest first
        assert_eq!(r["cards"].as_array().unwrap().len(), 1);
        assert_eq!(r["cards"][0]["slug"], json!("FEAT-PROVENANCE-DEMO"));
        assert_eq!(r["cards"][0]["why"], json!("because tests need it"));
        assert_eq!(r["cards"][0]["contract"], json!("CONTRACT-NEW")); // latest by id, not status
        assert_eq!(r["cards"][0]["contract_status"], json!("signed"));
        assert_eq!(r["decisions"].as_array().unwrap().len(), 1);
        assert_eq!(r["decisions"][0]["slug"], json!("FEAT-PROVENANCE-DEMO"));
        assert_eq!(r["decisions"][0]["excerpt"], json!("wire it up")); // guidance wins over situation

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn card_without_why_and_no_contract_row() {
        let dir = temp_repo("no-why");
        commit_file(&dir, "src/y.mjs", "// v1", "FEAT-BARE-CARD: initial");

        let c = db();
        seed_active(&c, dir.to_str().unwrap());
        c.execute("INSERT INTO cards (slug, title, status, body_json) VALUES ('FEAT-BARE-CARD', 'Bare card', 'backlog', NULL)", []).unwrap();

        let r = ultraProvenance(&json!({ "path": "src/y.mjs" }), &c).unwrap();
        assert_eq!(r["cards"][0]["why"], Value::Null);
        assert_eq!(r["cards"][0]["contract"], Value::Null);
        assert_eq!(r["cards"][0]["contract_status"], Value::Null);
        assert_eq!(r["decisions"].as_array().unwrap().len(), 0);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn unmatched_slug_produces_no_card_entry() {
        let dir = temp_repo("no-card");
        commit_file(&dir, "src/z.mjs", "// v1", "FEAT-NO-SUCH-CARD: initial");

        let c = db();
        seed_active(&c, dir.to_str().unwrap());
        let r = ultraProvenance(&json!({ "path": "src/z.mjs" }), &c).unwrap();
        assert_eq!(r["cards"].as_array().unwrap().len(), 0);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn limit_clamps_low_and_high() {
        let dir = temp_repo("limit-clamp");
        for i in 1..=3 {
            commit_file(&dir, "src/w.mjs", &format!("// v{i}"), &format!("commit {i}"));
        }
        let c = db();
        seed_active(&c, dir.to_str().unwrap());

        let r_low = ultraProvenance(&json!({ "path": "src/w.mjs", "limit": -5 }), &c).unwrap();
        assert_eq!(r_low["commits"].as_array().unwrap().len(), 1); // clamped to 1

        let r_high = ultraProvenance(&json!({ "path": "src/w.mjs", "limit": 999 }), &c).unwrap();
        assert_eq!(r_high["commits"].as_array().unwrap().len(), 3); // only 3 commits exist, cap is 20

        let r_default = ultraProvenance(&json!({ "path": "src/w.mjs", "limit": 0 }), &c).unwrap();
        assert_eq!(r_default["commits"].as_array().unwrap().len(), 3); // 0 is falsy -> default 6, still capped by history

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn path_not_in_git_history_returns_empty_commits_no_throw() {
        let dir = temp_repo("no-history-for-path");
        commit_file(&dir, "src/tracked.mjs", "// v1", "unrelated commit");
        let c = db();
        seed_active(&c, dir.to_str().unwrap());
        let r = ultraProvenance(&json!({ "path": "src/never-existed.mjs" }), &c).unwrap();
        assert_eq!(r["commits"], json!([]));
        assert_eq!(r["cards"], json!([]));
        assert_eq!(r["decisions"], json!([]));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn non_git_root_swallows_the_subprocess_failure_into_empty_commits() {
        let mut dir = std::env::temp_dir();
        dir.push(format!("antiloki-ultra-provenance-rs-not-a-repo-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        let c = db();
        seed_active(&c, dir.to_str().unwrap());
        let r = ultraProvenance(&json!({ "path": "anything.mjs" }), &c).unwrap();
        assert_eq!(r["commits"], json!([]));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn empty_path_is_rejected() {
        let c = db();
        seed_active(&c, "C:/tmp/whatever");
        let e = ultraProvenance(&json!({ "path": "  " }), &c).unwrap_err();
        assert!(e.contains("path"), "{e}");
    }
}
