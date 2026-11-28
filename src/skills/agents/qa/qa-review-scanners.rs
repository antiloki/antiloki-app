//! qa-review-scanners.rs — twin of skills/agents/qa/qa-review-scanners.mjs. NAMES ARE CONTRACT:
//! `qaReviewRun` (default, db lane) + `symbolExistence` / `typeBypass` / `placeholderSentinel` /
//! `securityScan` (named, no-db signature shape `fn(rel, contents)`).
//!
//! MIXED FILE — 3 of the 5 units are pure regex scanners (no I/O at all) and ARE conform-db
//! fixture-tested as ordinary no-db helpers: `typeBypass`, `placeholderSentinel`, `securityScan`.
//! The other 2 — `symbolExistence` and the default `qaReviewRun` — do REAL filesystem reads
//! (`existsSync`/`readFileSync`) resolved against `REPO_ROOT`, which the JS source computes
//! inline as `resolve(dirname(fileURLToPath(import.meta.url)), '..','..','..','..')` — a
//! HARDCODED constant derived from the module's own file location, with NO env override and NOT
//! DB-derived (unlike stack-facts.rs's `workspaces.root_path`). Same precedent as
//! man-audit-drift-spec-vs-impl.rs's `REPO_ROOT` / `checkFilesExist` / `checkAcceptanceMentions`
//! (identical `resolve(HERE,'..','..','..','..')` from a `skills/agents/<x>/` file — same
//! physical directory). This twin mirrors that exactly via `CARGO_MANIFEST_DIR`'s parent
//! (`repo_root()` below, copied verbatim from that precedent) — no env check, no injectable
//! root. `symbolExistence`/`qaReviewRun` are BESPOKE-NATIVE: real correct Rust code, proven by
//! the native tests below (scratch files under THIS checkout's `.antiloki/.test-scratch/`, tag
//! `qa-review-scanners-rs-*`, never colliding with the JS suite's own `__qa_review_fixture__` or
//! other twins' scratch dirs) — NOT fixture/frontier-tested through the two-lane harness.
//!
//! `context.db` missing → JS never throws here (qaReviewRun only touches `context.db` inside an
//! `if (card_slug && context?.db)` best-effort branch); `db: &Connection` stays a required,
//! non-nullable reference (stack-facts.rs / man-audit-drift-spec-vs-impl.rs precedent), so the
//! Rust condition simplifies to just `truthy(card_slug)`.
//!
//! `rel`/`contents` are assumed to always be strings in the domain (files array elements read
//! off disk) — same "realistic-domain simplification" convention as man-audit-drift-spec-vs-impl.rs.
// QUALITY: 80/100 — 4 independent regex-table scanners (grouped-by-rule iteration order
// preserved) + a real-fs orchestrator; card-phase-event best-effort write matches JS try/catch.

use regex::Regex;
use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

fn truthy(v: Option<&Value>) -> bool { match v { None|Some(Value::Null)=>false, Some(Value::Bool(b))=>*b, Some(Value::Number(n))=>n.as_f64().map_or(false,|f|f!=0.0&&!f.is_nan()), Some(Value::String(s))=>!s.is_empty(), Some(_)=>true } }
fn str_qq(v: Option<&Value>) -> String { match v { None|Some(Value::Null)=>String::new(), Some(Value::String(s))=>s.clone(), Some(Value::Bool(b))=>b.to_string(), Some(n@Value::Number(_))=>n.to_string(), Some(o)=>o.to_string() } }
fn tmpl(v: Option<&Value>) -> String { match v { None=>"undefined".into(), Some(Value::Null)=>"null".into(), Some(x)=>str_qq(Some(x)) } }

// JS: `const HERE = dirname(fileURLToPath(import.meta.url)); const REPO_ROOT = resolve(HERE,
// '..','..','..','..');` — HERE is antiloki-core/skills/agents/qa, four levels up is this
// checkout's antiloki-core parent. CARGO_MANIFEST_DIR here is antiloki-core-rust; its parent is
// that same directory on this checkout layout (man-audit-drift-spec-vs-impl.rs precedent).
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("antiloki-core-rust has a parent dir")
        .to_path_buf()
}

// node:path `resolve(base, rel)` — normalizes `/` and `\` uniformly, drops `.` components,
// leaves `..` components in place (the OS resolves them lexically on the later exists()/
// metadata() syscall). Copied verbatim from man-audit-drift-spec-vs-impl.rs precedent.
fn node_join(base: &Path, rel: &str) -> PathBuf {
    let mut p = base.to_path_buf();
    for part in rel.split(['/', '\\']) {
        if part.is_empty() || part == "." {
            continue;
        }
        p.push(part);
    }
    p
}

fn line_of(contents: &str, byte_idx: usize) -> i64 {
    contents[..byte_idx].matches('\n').count() as i64 + 1
}

/// `readSafe(rel)` — not exported (internal helper), real fs read gated on existsSync + isFile.
fn read_safe(rel: &str) -> Option<String> {
    let abs = node_join(&repo_root(), rel);
    if !abs.exists() {
        return None;
    }
    match std::fs::metadata(&abs) {
        Ok(meta) if meta.is_file() => std::fs::read_to_string(&abs).ok(),
        _ => None,
    }
}

// `^\s*import\s+(?:.+?\s+from\s+)?['"]([^'"]+)['"]` with the `m` (multiline) flag — no
// lookahead/lookbehind, ports 1:1.
fn import_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r#"(?m)^\s*import\s+(?:.+?\s+from\s+)?['"]([^'"]+)['"]"#).unwrap())
}

/// `export function symbolExistence(rel, contents)` — no-db SIGNATURE shape, but real fs reads
/// (`existsSync`) against `REPO_ROOT` for every relative import candidate — BESPOKE-NATIVE, see
/// module doc.
pub fn symbolExistence(rel: &Value, contents: &Value) -> Value {
    let mut findings: Vec<Value> = Vec::new();
    let contents_s = match contents.as_str() {
        Some(s) => s,
        None => return Value::Array(findings),
    };
    let rel_s = rel.as_str().unwrap_or("");
    for caps in import_re().captures_iter(contents_s) {
        let m = caps.get(0).unwrap();
        let spec = caps.get(1).unwrap().as_str();
        if !spec.starts_with('.') {
            continue; // package — assume installed
        }
        let importing_abs = node_join(&repo_root(), rel_s);
        let base_dir = importing_abs.parent().map(|p| p.to_path_buf()).unwrap_or(importing_abs);
        let candidates = [
            spec.to_string(),
            format!("{spec}.ts"), format!("{spec}.tsx"), format!("{spec}.mjs"),
            format!("{spec}.js"), format!("{spec}.jsx"),
            format!("{spec}/index.ts"), format!("{spec}/index.mjs"), format!("{spec}/index.js"),
        ];
        let hit = candidates.iter().any(|c| node_join(&base_dir, c).exists());
        if !hit {
            let line = line_of(contents_s, m.start());
            findings.push(json!({
                "severity": "critical",
                "code": "IMPORT_UNRESOLVED",
                "message": format!("'{}' imports '{}' which doesn't resolve", tmpl(Some(rel)), spec),
                "line": line
            }));
        }
    }
    Value::Array(findings)
}

fn type_bypass_rules() -> &'static Vec<(Regex, &'static str, &'static str)> {
    static R: OnceLock<Vec<(Regex, &'static str, &'static str)>> = OnceLock::new();
    R.get_or_init(|| {
        vec![
            (Regex::new(r"\bas\s+any\b").unwrap(), "TYPE_AS_ANY", "as any cast"),
            (Regex::new(r"\bas\s+unknown\s+as\b").unwrap(), "TYPE_DOUBLE_CAST", "as unknown as <T> double cast"),
            (Regex::new(r"//\s*@ts-ignore\b").unwrap(), "TS_IGNORE", "@ts-ignore directive"),
            (Regex::new(r"//\s*@ts-nocheck\b").unwrap(), "TS_NOCHECK", "@ts-nocheck directive"),
            (Regex::new(r"//\s*eslint-disable\b").unwrap(), "ESLINT_DISABLE", "eslint-disable directive"),
        ]
    })
}

/// `export function typeBypass(rel, contents)` — pure regex, no I/O. Iteration is GROUPED BY
/// RULE (all matches of rule 1 across the whole doc, then all of rule 2, ...), not globally
/// sorted by position — ports the JS `for (rule) { while (re.exec) push }` structure exactly.
pub fn typeBypass(rel: &Value, contents: &Value) -> Value {
    let mut findings: Vec<Value> = Vec::new();
    let contents_s = match contents.as_str() {
        Some(s) => s,
        None => return Value::Array(findings),
    };
    for (re, code, msg) in type_bypass_rules() {
        for m in re.find_iter(contents_s) {
            let line = line_of(contents_s, m.start());
            findings.push(json!({
                "severity": "warning",
                "code": code,
                "message": format!("{}:{} {}", tmpl(Some(rel)), line, msg),
                "line": line
            }));
        }
    }
    Value::Array(findings)
}

fn placeholder_rules() -> &'static Vec<(Regex, &'static str, &'static str)> {
    static R: OnceLock<Vec<(Regex, &'static str, &'static str)>> = OnceLock::new();
    R.get_or_init(|| {
        vec![
            (Regex::new(r"\b(?:TODO|FIXME|XXX|HACK)\b[:( ]").unwrap(), "TODO_MARKER", "TODO / FIXME / XXX / HACK marker"),
            (Regex::new(r"(?i)\b(?:CHANGEME|REPLACE_ME|PLACEHOLDER)\b").unwrap(), "PLACEHOLDER_VALUE", "placeholder literal value"),
            (Regex::new(r#"['"`](?:fake-|stub-|dummy-)[^'"`]*['"`]"#).unwrap(), "FAKE_VALUE", "fake-/stub-/dummy- prefixed string literal"),
        ]
    })
}

/// `export function placeholderSentinel(rel, contents)` — pure regex, no I/O. Same grouped-by-
/// rule iteration order as `typeBypass`.
pub fn placeholderSentinel(rel: &Value, contents: &Value) -> Value {
    let mut findings: Vec<Value> = Vec::new();
    let contents_s = match contents.as_str() {
        Some(s) => s,
        None => return Value::Array(findings),
    };
    for (re, code, msg) in placeholder_rules() {
        for m in re.find_iter(contents_s) {
            let line = line_of(contents_s, m.start());
            findings.push(json!({
                "severity": "warning",
                "code": code,
                "message": format!("{}:{} {}", tmpl(Some(rel)), line, msg),
                "line": line
            }));
        }
    }
    Value::Array(findings)
}

fn security_rules() -> &'static Vec<(Regex, &'static str, &'static str, &'static str)> {
    static R: OnceLock<Vec<(Regex, &'static str, &'static str, &'static str)>> = OnceLock::new();
    R.get_or_init(|| {
        vec![
            (Regex::new(r"\b(?:sk-ant-|sk-or-|sk-)[A-Za-z0-9_-]{20,}\b").unwrap(), "SECRET_API_KEY", "critical", "hardcoded API key"),
            (Regex::new(r"\bAKIA[0-9A-Z]{16}\b").unwrap(), "SECRET_AWS_KEY", "critical", "hardcoded AWS access key"),
            (Regex::new(r"\b(?:ghp|gho|ghs)_[A-Za-z0-9_]{20,}\b").unwrap(), "SECRET_GH_TOKEN", "critical", "hardcoded GitHub token"),
            (Regex::new(r#"(?:SELECT|INSERT|UPDATE|DELETE)[^`'"\n]*\$\{[^}]+\}"#).unwrap(), "SQL_CONCAT", "critical", "SQL string interpolation (use parameterized queries)"),
            (Regex::new(r"\beval\s*\(").unwrap(), "EVAL_CALL", "critical", "eval() call"),
            (Regex::new(r"\bnew\s+Function\s*\(").unwrap(), "FUNCTION_CTOR", "warning", "new Function() constructor (often safer than eval, still dynamic)"),
        ]
    })
}

/// `export function securityScan(rel, contents)` — pure regex, no I/O. Same grouped-by-rule
/// iteration order; severity varies per rule (unlike the other two scanners).
pub fn securityScan(rel: &Value, contents: &Value) -> Value {
    let mut findings: Vec<Value> = Vec::new();
    let contents_s = match contents.as_str() {
        Some(s) => s,
        None => return Value::Array(findings),
    };
    for (re, code, sev, msg) in security_rules() {
        for m in re.find_iter(contents_s) {
            let line = line_of(contents_s, m.start());
            findings.push(json!({
                "severity": sev,
                "code": code,
                "message": format!("{}:{} {}", tmpl(Some(rel)), line, msg),
                "line": line
            }));
        }
    }
    Value::Array(findings)
}

/// `export default function qaReviewRun(input, context)` — orchestrator: reads every `files[]`
/// entry off the real filesystem (BESPOKE-NATIVE, see module doc), runs the 4 scanners, and
/// best-effort records a `qa_pass`/`qa_fail` card-phase-event when `card_slug` is truthy (any
/// failure there is swallowed, matching the JS `try {} catch {}`).
pub fn qaReviewRun(input: &Value, db: &Connection) -> Result<Value, String> {
    let files_v: Vec<Value> = match input.get("files").and_then(|v| v.as_array()) {
        Some(a) => a.clone(),
        None => return Err("qa-review-run: files (array) is required".to_string()),
    };
    // `{ card_slug = null, actor = 'agent:qa' } = input ?? {}` — default only on ABSENT key.
    let card_slug_v = input.get("card_slug").cloned().unwrap_or(Value::Null);
    let actor_v = match input.get("actor") {
        None => json!("agent:qa"),
        Some(v) => v.clone(),
    };

    let mut all: Vec<Value> = Vec::new();
    let mut se_count = 0i64;
    let mut tb_count = 0i64;
    let mut ps_count = 0i64;
    let mut sec_count = 0i64;
    let mut files_missing = 0i64;

    for rel_v in &files_v {
        let rel_s = rel_v.as_str().unwrap_or("");
        let contents = match read_safe(rel_s) {
            Some(c) => c,
            None => {
                files_missing += 1;
                continue;
            }
        };
        let contents_v = json!(contents);
        let se = symbolExistence(rel_v, &contents_v);
        let tb = typeBypass(rel_v, &contents_v);
        let ps = placeholderSentinel(rel_v, &contents_v);
        let sec = securityScan(rel_v, &contents_v);
        se_count += se.as_array().map(|a| a.len()).unwrap_or(0) as i64;
        tb_count += tb.as_array().map(|a| a.len()).unwrap_or(0) as i64;
        ps_count += ps.as_array().map(|a| a.len()).unwrap_or(0) as i64;
        sec_count += sec.as_array().map(|a| a.len()).unwrap_or(0) as i64;
        if let Value::Array(a) = se { all.extend(a); }
        if let Value::Array(a) = tb { all.extend(a); }
        if let Value::Array(a) = ps { all.extend(a); }
        if let Value::Array(a) = sec { all.extend(a); }
    }

    let criticals = all.iter().filter(|f| f["severity"] == json!("critical")).count() as i64;
    let warnings = all.iter().filter(|f| f["severity"] == json!("warning")).count() as i64;
    let ok = criticals == 0;
    let files_scanned = files_v.len() as i64 - files_missing;

    // FEAT-AUTO-PHASE-EVENTS: best-effort — `db: &Connection` is always injected in the Rust
    // db lane, so `context?.db` is always truthy; only `card_slug` gates the branch.
    let mut phase_event_id = Value::Null;
    if truthy(Some(&card_slug_v)) {
        let evidence = json!({
            "source": "qa-review-scanners",
            "files_scanned": files_scanned,
            "critical_count": criticals,
            "warning_count": warnings
        });
        let phase = if ok { "qa_pass" } else { "qa_fail" };
        let call_input = json!({ "card_slug": card_slug_v, "phase": phase, "actor": actor_v, "evidence": evidence });
        if let Ok(ev) = crate::entities_card_phase_event_record::cardPhaseEventRecord(&call_input, db) {
            phase_event_id = ev.get("id").cloned().unwrap_or(Value::Null);
        }
    }

    let mut per_scanner = Map::new();
    per_scanner.insert("symbol_existence".into(), json!(se_count));
    per_scanner.insert("type_bypass".into(), json!(tb_count));
    per_scanner.insert("placeholder_sentinel".into(), json!(ps_count));
    per_scanner.insert("security_scan".into(), json!(sec_count));

    let mut out = Map::new();
    out.insert("ok".into(), json!(ok));
    out.insert("files_scanned".into(), json!(files_scanned));
    out.insert("files_missing".into(), json!(files_missing));
    out.insert("finding_count".into(), json!(all.len() as i64));
    out.insert("critical_count".into(), json!(criticals));
    out.insert("warning_count".into(), json!(warnings));
    out.insert("per_scanner".into(), Value::Object(per_scanner));
    out.insert("findings".into(), Value::Array(all));
    out.insert("phase_event_id".into(), phase_event_id);
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn scratch_rel(tag: &str) -> String {
        format!(".antiloki/.test-scratch/qa-review-scanners-rs-{tag}-{}", std::process::id())
    }
    fn scratch_abs(rel: &str) -> PathBuf {
        node_join(&repo_root(), rel)
    }
    fn cleanup(rel: &str) {
        let _ = fs::remove_dir_all(scratch_abs(rel));
    }

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE cards (slug TEXT); \
             CREATE TABLE card_phase_events (id INTEGER PRIMARY KEY, card_slug TEXT, phase TEXT, actor TEXT, evidence_json TEXT)",
        ).unwrap();
        c
    }

    #[test]
    fn type_bypass_groups_by_rule_not_by_position() {
        let src = "const x = foo as any;\n// @ts-ignore\nconst y = bar as unknown as Baz;\n// eslint-disable-next-line\n// @ts-nocheck\n";
        let f = typeBypass(&json!("x.ts"), &json!(src));
        let arr = f.as_array().unwrap();
        let codes: Vec<&str> = arr.iter().map(|x| x["code"].as_str().unwrap()).collect();
        assert_eq!(codes, ["TYPE_AS_ANY", "TYPE_DOUBLE_CAST", "TS_IGNORE", "TS_NOCHECK", "ESLINT_DISABLE"]);
        assert_eq!(arr[0]["line"], json!(1));
        assert_eq!(arr[1]["line"], json!(3));
        assert_eq!(arr[0]["severity"], json!("warning"));
        assert_eq!(typeBypass(&json!("x.ts"), &json!("const z: number = 1;")).as_array().unwrap().len(), 0);
        assert_eq!(typeBypass(&json!("x.ts"), &Value::Null), json!([]));
    }

    #[test]
    fn placeholder_sentinel_catches_todo_placeholder_fake() {
        let src = "// TODO: fix this\nconst apiKey = 'CHANGEME';\nconst user = \"fake-user-id\";\n// FIXME(later): still broken\n";
        let f = placeholderSentinel(&json!("x.ts"), &json!(src));
        let arr = f.as_array().unwrap();
        let codes: Vec<&str> = arr.iter().map(|x| x["code"].as_str().unwrap()).collect();
        assert_eq!(codes, ["TODO_MARKER", "TODO_MARKER", "PLACEHOLDER_VALUE", "FAKE_VALUE"]);
        assert_eq!(arr[0]["line"], json!(1));
        assert_eq!(arr[1]["line"], json!(4));
        let ci = placeholderSentinel(&json!("x.ts"), &json!("let x = 'replace_me';"));
        assert_eq!(ci.as_array().unwrap()[0]["code"], json!("PLACEHOLDER_VALUE"));
    }

    #[test]
    fn security_scan_flags_secrets_sql_eval_and_function_ctor() {
        let src = "const k = 'sk-ant-1234567890abcdefghij1234567890';\nconst sql = `SELECT * FROM users WHERE id = ${userId}`;\neval(userInput);\nconst fn = new Function('a', 'return a+1');\n";
        let f = securityScan(&json!("x.ts"), &json!(src));
        let arr = f.as_array().unwrap();
        let codes: Vec<&str> = arr.iter().map(|x| x["code"].as_str().unwrap()).collect();
        assert_eq!(codes, ["SECRET_API_KEY", "SQL_CONCAT", "EVAL_CALL", "FUNCTION_CTOR"]);
        assert_eq!(arr[0]["severity"], json!("critical"));
        assert_eq!(arr[3]["severity"], json!("warning"));
        let aws = securityScan(&json!("x.ts"), &json!("const a = 'AKIAABCDEFGHIJKLMNOP';\nconst b = 'ghp_abcdefghijklmnopqrstuvwxyz12';"));
        let aws_codes: Vec<&str> = aws.as_array().unwrap().iter().map(|x| x["code"].as_str().unwrap()).collect();
        assert_eq!(aws_codes, ["SECRET_AWS_KEY", "SECRET_GH_TOKEN"]);
        assert_eq!(securityScan(&json!("x.ts"), &json!("const x = 1;")).as_array().unwrap().len(), 0);
    }

    #[test]
    fn symbol_existence_flags_only_unresolved_relative_imports() {
        let f = symbolExistence(&json!("x.ts"), &json!("import fs from 'node:fs';\nimport React from 'react';"));
        assert_eq!(f.as_array().unwrap().len(), 0);

        let rel = scratch_rel("broken");
        fs::create_dir_all(scratch_abs(&rel)).unwrap();
        let src_rel = format!("{rel}/src.ts");
        let f = symbolExistence(&json!(src_rel), &json!("import { foo } from './ghost';"));
        let arr = f.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["code"], json!("IMPORT_UNRESOLVED"));
        assert_eq!(arr[0]["severity"], json!("critical"));
        assert!(arr[0]["message"].as_str().unwrap().contains("doesn't resolve"));
        cleanup(&rel);
    }

    #[test]
    fn symbol_existence_resolves_via_common_extensions() {
        let rel = scratch_rel("resolves");
        fs::create_dir_all(scratch_abs(&rel)).unwrap();
        fs::write(scratch_abs(&format!("{rel}/real.ts")), "export const x = 1;").unwrap();
        let src_rel = format!("{rel}/src.ts");
        let f = symbolExistence(&json!(src_rel), &json!("import { x } from './real';"));
        assert_eq!(f.as_array().unwrap().len(), 0);
        cleanup(&rel);
    }

    #[test]
    fn qa_review_run_aggregates_and_counts_missing() {
        let rel = scratch_rel("aggregate");
        fs::create_dir_all(scratch_abs(&rel)).unwrap();
        fs::write(scratch_abs(&format!("{rel}/a.ts")), "const k = 'sk-ant-1234567890abcdefghij1234567890';\n// TODO: fix this").unwrap();
        fs::write(scratch_abs(&format!("{rel}/b.ts")), "const x = something as any;").unwrap();
        let c = db();
        let r = qaReviewRun(&json!({ "files": [format!("{rel}/a.ts"), format!("{rel}/b.ts"), format!("{rel}/__never__.ts")] }), &c).unwrap();
        assert_eq!(r["files_scanned"], json!(2));
        assert_eq!(r["files_missing"], json!(1));
        assert!(r["critical_count"].as_i64().unwrap() >= 1);
        assert!(r["warning_count"].as_i64().unwrap() >= 2);
        assert_eq!(r["ok"], json!(false));
        assert_eq!(r["phase_event_id"], Value::Null);
        let keys: Vec<&String> = r.as_object().unwrap().keys().collect();
        assert_eq!(keys, vec!["ok", "files_scanned", "files_missing", "finding_count", "critical_count", "warning_count", "per_scanner", "findings", "phase_event_id"]);
        cleanup(&rel);
    }

    #[test]
    fn qa_review_run_missing_files_only_counts_not_crashes() {
        let c = db();
        let r = qaReviewRun(&json!({ "files": ["__never_exists__/foo.ts"] }), &c).unwrap();
        assert_eq!(r["files_scanned"], json!(0));
        assert_eq!(r["files_missing"], json!(1));
        assert_eq!(r["ok"], json!(true));
    }

    #[test]
    fn qa_review_run_clean_file_is_ok_true() {
        let rel = scratch_rel("clean");
        fs::create_dir_all(scratch_abs(&rel)).unwrap();
        fs::write(scratch_abs(&format!("{rel}/clean.ts")), "const x: number = 1;\nexport { x };").unwrap();
        let c = db();
        let r = qaReviewRun(&json!({ "files": [format!("{rel}/clean.ts")] }), &c).unwrap();
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["critical_count"], json!(0));
        cleanup(&rel);
    }

    #[test]
    fn qa_review_run_records_card_phase_event_on_pass_and_fail() {
        let rel = scratch_rel("phase-event");
        fs::create_dir_all(scratch_abs(&rel)).unwrap();
        fs::write(scratch_abs(&format!("{rel}/clean.ts")), "const x = 1;").unwrap();
        fs::write(scratch_abs(&format!("{rel}/bad.ts")), "eval(x);").unwrap();
        let c = db();
        c.execute("INSERT INTO cards (slug) VALUES ('C-1')", []).unwrap();

        let pass = qaReviewRun(&json!({ "files": [format!("{rel}/clean.ts")], "card_slug": "C-1" }), &c).unwrap();
        assert_eq!(pass["ok"], json!(true));
        assert_eq!(pass["phase_event_id"], json!(1));
        let phase: String = c.query_row("SELECT phase FROM card_phase_events WHERE id = 1", [], |r| r.get(0)).unwrap();
        assert_eq!(phase, "qa_pass");

        let fail = qaReviewRun(&json!({ "files": [format!("{rel}/bad.ts")], "card_slug": "C-1", "actor": "agent:dev" }), &c).unwrap();
        assert_eq!(fail["ok"], json!(false));
        assert_eq!(fail["phase_event_id"], json!(2));
        let (phase2, actor2): (String, String) = c.query_row("SELECT phase, actor FROM card_phase_events WHERE id = 2", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert_eq!(phase2, "qa_fail");
        assert_eq!(actor2, "agent:dev");
        cleanup(&rel);
    }

    #[test]
    fn qa_review_run_swallows_card_phase_event_failure_for_unknown_card() {
        let rel = scratch_rel("swallow");
        fs::create_dir_all(scratch_abs(&rel)).unwrap();
        fs::write(scratch_abs(&format!("{rel}/clean.ts")), "const x = 1;").unwrap();
        let c = db();
        let r = qaReviewRun(&json!({ "files": [format!("{rel}/clean.ts")], "card_slug": "GHOST" }), &c).unwrap();
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["phase_event_id"], Value::Null);
        cleanup(&rel);
    }

    #[test]
    fn qa_review_run_missing_files_key_errs() {
        let c = db();
        let err = qaReviewRun(&json!({}), &c).unwrap_err();
        assert!(err.contains("files (array) is required"));
    }
}
