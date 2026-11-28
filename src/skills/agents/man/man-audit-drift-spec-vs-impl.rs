//! man-audit-drift-spec-vs-impl.rs — twin of skills/agents/man/man-audit-drift-spec-vs-impl.mjs.
//! NAMES ARE CONTRACT: `manAuditDriftSpecVsImpl` (default, db lane), `checkFilesExist` /
//! `checkAcceptanceMentions` (named, no-db helpers — both do real fs reads, see below).
//!
//! BESPOKE-NATIVE — NOT fixture-testable through the conform-db two-lane harness. All three
//! functions resolve paths against `REPO_ROOT`, which the JS source computes inline as
//! `resolve(dirname(fileURLToPath(import.meta.url)), '..','..','..','..')` — a HARDCODED
//! constant derived from the module's own file location, with NO env override and NOT
//! DB-derived (unlike stack-facts.rs's `workspaces.root_path`). Same precedent as
//! qa-sandbox-reset.rs's `REPO_ROOT` (`resolve(HERE,'..','..','..')` from `skills/contract/`,
//! one `..` shallower — both land on the same physical directory, the parent of `antiloki-core`).
//! This twin mirrors that exactly via `CARGO_MANIFEST_DIR`'s parent (`repo_root()` below, copied
//! verbatim from qa-sandbox-reset.rs) — no env check, no injectable root — so, like that twin,
//! the native tests below write real scratch files under THIS checkout's
//! `.antiloki/.test-scratch/`, using dedicated `man-audit-drift-spec-vs-impl-rs-*` names that
//! never collide with the JS suite's own `__drift_fixture__` scratch dir or other twins' tests.
//! Only reads (`existsSync`/`statSync`/`readFileSync`) — no writes/deletes — so there is no
//! destructive-replay hazard, but the hardcoded root still rules out a conform-db fixture per the
//! playbook's VET criteria.
//!
//! `context.db` missing → JS throws `err.contextMissing(...)`; unreachable here since
//! `db: &Connection` is a required, non-nullable reference (stack-facts.rs / qa-sandbox-reset.rs
//! precedent). `status_min` enum-validation guard IS modeled (own input validation, not a context
//! guard) as a plain `Err(String)`, matching atlas-build.rs's "must be one of" convention.
//!
//! `checkFilesExist`/`checkAcceptanceMentions` assume (realistic-domain simplification, same
//! convention as qa-sandbox-reset.rs's slug handling) that `filesTouched` is always a JSON array
//! of strings (or null/absent, defaulting to empty) — `parse_json_field` already guarantees this
//! for the values `manAuditDriftSpecVsImpl` passes them; the exported helpers degrade to an empty
//! iteration for any non-array input rather than reproducing JS's `for...of` over a raw string's
//! UTF-16 code units.

use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};

fn truthy(v: Option<&Value>) -> bool { match v { None|Some(Value::Null)=>false, Some(Value::Bool(b))=>*b, Some(Value::Number(n))=>n.as_f64().map_or(false,|f|f!=0.0&&!f.is_nan()), Some(Value::String(s))=>!s.is_empty(), Some(_)=>true } }
fn str_qq(v: Option<&Value>) -> String { match v { None|Some(Value::Null)=>String::new(), Some(Value::String(s))=>s.clone(), Some(Value::Bool(b))=>b.to_string(), Some(n@Value::Number(_))=>n.to_string(), Some(o)=>o.to_string() } }
fn tmpl(v: Option<&Value>) -> String { match v { None=>"undefined".into(), Some(Value::Null)=>"null".into(), Some(x)=>str_qq(Some(x)) } }

// JS: `const HERE = dirname(fileURLToPath(import.meta.url)); const REPO_ROOT = resolve(HERE,
// '..','..','..','..');` — HERE is antiloki-core/skills/agents/man, four levels up is this
// checkout's antiloki-core parent. CARGO_MANIFEST_DIR here is antiloki-core-rust; its parent is
// that same directory on this checkout layout (qa-sandbox-reset.rs precedent). Deliberately NO
// env override — see module doc.
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("antiloki-core-rust has a parent dir")
        .to_path_buf()
}

// node:path `resolve(base, rel)` — normalizes `/` and `\` uniformly and drops `.` components.
// Copied verbatim from qa-sandbox-reset.rs / sandbox-resolve-path.rs precedent.
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

fn finding(severity: &str, code: &str, message: String) -> Value {
    let mut m = Map::new();
    m.insert("severity".into(), json!(severity));
    m.insert("code".into(), json!(code));
    m.insert("message".into(), json!(message));
    Value::Object(m)
}

fn as_slice(v: &Value) -> &[Value] {
    v.as_array().map(|a| a.as_slice()).unwrap_or(&[])
}

/// `export function checkFilesExist(filesTouched)` — no-db, real fs reads (existsSync/statSync).
pub fn checkFilesExist(files_touched: &Value) -> Value {
    let mut findings: Vec<Value> = Vec::new();
    for rel_v in as_slice(files_touched) {
        let rel = rel_v.as_str().unwrap_or("");
        let abs = node_join(&repo_root(), rel);
        if !abs.exists() {
            findings.push(finding(
                "critical",
                "FILE_MISSING",
                format!("contract declares '{rel}' but it doesn't exist on disk"),
            ));
            continue;
        }
        if let Ok(st) = std::fs::metadata(&abs) {
            if st.len() == 0 {
                findings.push(finding("warning", "FILE_EMPTY", format!("'{rel}' exists but is empty")));
            }
        } // else: permissions etc — skip, matches JS catch{}
    }
    Value::Array(findings)
}

const STOP: &[&str] = &[
    "should", "must", "will", "have", "when", "then", "that", "these", "those", "their", "them", "this",
    "with", "from", "into", "onto", "about", "after", "before", "where", "what", "which", "while",
];

// JS `/[^a-z0-9\s_.-]/g` replaced with ' ' — applied AFTER .toLowerCase(), so only ASCII a-z ever
// need to survive here (non-ASCII, already-lowercased input just falls through to the else arm).
fn sanitize_for_tokens(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_lowercase() || c.is_ascii_digit() || c.is_whitespace() || c == '_' || c == '.' || c == '-' {
                c
            } else {
                ' '
            }
        })
        .collect()
}

/// `export function checkAcceptanceMentions(filesTouched, acceptance)` — no-db, real fs reads
/// (readFileSync) to build a text corpus, then a salient-token heuristic per acceptance item.
pub fn checkAcceptanceMentions(files_touched: &Value, acceptance: &Value) -> Value {
    let acc_items = as_slice(acceptance);
    if acc_items.is_empty() {
        return Value::Array(Vec::new());
    }

    let mut corpus = String::new();
    for rel_v in as_slice(files_touched) {
        let rel = rel_v.as_str().unwrap_or("");
        let abs = node_join(&repo_root(), rel);
        if !abs.exists() {
            continue;
        }
        if let Ok(text) = std::fs::read_to_string(&abs) {
            corpus.push_str(&text);
            corpus.push('\n');
        } // else: skip, matches JS catch{}
    }
    let lower_corpus = corpus.to_lowercase();

    let mut findings: Vec<Value> = Vec::new();
    for (i, ac_v) in acc_items.iter().enumerate() {
        let ac = str_qq(Some(ac_v)); // = JS String(acceptance[i] ?? '')
        if ac.is_empty() {
            continue;
        }
        let sanitized = sanitize_for_tokens(&ac.to_lowercase());
        let tokens: Vec<&str> = sanitized
            .split_whitespace()
            .filter(|w| w.chars().count() > 4 && !STOP.contains(w))
            .collect();
        if tokens.is_empty() {
            continue;
        }
        let mentioned = tokens.iter().filter(|t| lower_corpus.contains(**t)).count();
        let ratio = mentioned as f64 / tokens.len() as f64;
        if ratio < 0.34 {
            let snippet: String = ac.chars().take(80).collect(); // JS ac.slice(0, 80)
            findings.push(finding(
                "warning",
                "ACCEPTANCE_UNMET_HEURISTIC",
                format!(
                    "acceptance[{i}] \"{snippet}\u{2026}\" — only {mentioned}/{} salient tokens found in touched files",
                    tokens.len()
                ),
            ));
        }
    }
    Value::Array(findings)
}

// JS: `try { const v = JSON.parse(s ?? 'null'); return v ?? fallback; } catch { return fallback; }`
// — note this local helper applies its OWN `?? fallback` after parsing (unlike the generic
// JSON.parse(s ?? '[]') pattern elsewhere in the codebase): a parsed `null` (whether from a
// null/missing column or a literal "null" JSON string) falls back too, not just a parse error.
fn parse_json_field(s: Option<&Value>, fallback: &Value) -> Value {
    let text = match s {
        None | Some(Value::Null) => "null".to_string(),
        Some(Value::String(t)) => t.clone(),
        Some(other) => other.to_string(), // realistic-domain: TEXT columns are String/Null only
    };
    match serde_json::from_str::<Value>(&text) {
        Ok(Value::Null) => fallback.clone(),
        Ok(v) => v,
        Err(_) => fallback.clone(),
    }
}

const STATUS_ORDER: &[&str] = &["proposed", "pl_ratified", "tl_approved", "human_approved", "impl", "qa", "done"];

/// `export default function manAuditDriftSpecVsImpl(input, context)`.
pub fn manAuditDriftSpecVsImpl(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS: `const { db } = context ?? {}; if (!db) throw err.contextMissing(...)` — unreachable in
    // the db lane (a connection is always injected), mirrors stack-facts.rs / qa-sandbox-reset.rs.

    // JS: `const { contract_slug = null, status_min = 'pl_ratified' } = input ?? {};` — the
    // default only applies when the key is ABSENT (JSON has no `undefined`); an explicit `null`
    // stays null.
    let contract_slug = input.get("contract_slug").cloned().unwrap_or(Value::Null);
    let status_min_v = match input.get("status_min") {
        None => json!("pl_ratified"),
        Some(v) => v.clone(),
    };

    // JS `statusOrder.indexOf(status_min)`: strict-equality lookup — only a String value that
    // matches one of the 7 statuses returns a real index; anything else (null, number, object,
    // an unrecognized string) is -1.
    let min_idx = status_min_v.as_str().and_then(|s| STATUS_ORDER.iter().position(|x| *x == s));
    let min_idx = match min_idx {
        Some(i) => i,
        None => {
            return Err(format!( // guard-throw, not fixture-exercised (bespoke-native anyway)
                "man-audit-drift-spec-vs-impl: status_min must be one of: {} (got '{}')",
                STATUS_ORDER.join(", "),
                tmpl(Some(&status_min_v))
            ));
        }
    };

    let contracts: Value = if truthy(Some(&contract_slug)) {
        crate::db_json::query_rows(db, "SELECT * FROM contracts WHERE slug = ?", &[contract_slug.clone()])?
    } else {
        let statuses = &STATUS_ORDER[min_idx..];
        let placeholders = statuses.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let sql = format!("SELECT * FROM contracts WHERE status IN ({placeholders}) ORDER BY id DESC");
        let params: Vec<Value> = statuses.iter().map(|s| json!(*s)).collect();
        crate::db_json::query_rows(db, &sql, &params)?
    };

    let mut results: Vec<Value> = Vec::new();
    for c in contracts.as_array().cloned().unwrap_or_default() {
        let files = parse_json_field(c.get("files_touched_json"), &json!([]));
        let acceptance = parse_json_field(c.get("acceptance_json"), &json!([]));

        let mut findings: Vec<Value> = Vec::new();
        if let Value::Array(a) = checkFilesExist(&files) {
            findings.extend(a);
        }
        if let Value::Array(a) = checkAcceptanceMentions(&files, &acceptance) {
            findings.extend(a);
        }

        let critical_count = findings.iter().filter(|f| f["severity"] == json!("critical")).count();
        let acceptance_count = acceptance.as_array().map(|a| a.len()).unwrap_or(0);

        let mut r = Map::new();
        r.insert("contract_slug".into(), c.get("slug").cloned().unwrap_or(Value::Null));
        r.insert("status".into(), c.get("status").cloned().unwrap_or(Value::Null));
        r.insert("files_touched".into(), files);
        r.insert("acceptance_count".into(), json!(acceptance_count as i64));
        r.insert("finding_count".into(), json!(findings.len() as i64));
        r.insert("critical_count".into(), json!(critical_count as i64));
        r.insert("findings".into(), Value::Array(findings));
        results.push(Value::Object(r));
    }

    let total_critical: i64 = results.iter().map(|r| r["critical_count"].as_i64().unwrap_or(0)).sum();
    let contracts_with_drift = results.iter().filter(|r| r["finding_count"].as_i64().unwrap_or(0) > 0).count();
    let overall_health = if total_critical == 0 {
        "green"
    } else if total_critical < 3 {
        "yellow"
    } else {
        "red"
    };

    let mut out = Map::new();
    out.insert("contracts_scanned".into(), json!(results.len() as i64));
    out.insert("contracts_with_drift".into(), json!(contracts_with_drift as i64));
    out.insert("total_critical".into(), json!(total_critical));
    out.insert("overall_health".into(), json!(overall_health));
    out.insert("results".into(), Value::Array(results));
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn scratch_rel(tag: &str) -> String {
        format!(".antiloki/.test-scratch/man-audit-drift-spec-vs-impl-rs-{tag}-{}", std::process::id())
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
            "CREATE TABLE contracts (id INTEGER PRIMARY KEY AUTOINCREMENT, slug TEXT, title TEXT, status TEXT, \
             files_touched_json TEXT, acceptance_json TEXT)",
        )
        .unwrap();
        c
    }
    fn seed(c: &Connection, slug: &str, status: &str, files_touched_json: &str, acceptance_json: &str) {
        c.execute(
            "INSERT INTO contracts (slug, title, status, files_touched_json, acceptance_json) \
             VALUES (?1, 't', ?2, ?3, ?4)",
            rusqlite::params![slug, status, files_touched_json, acceptance_json],
        )
        .unwrap();
    }

    #[test]
    fn check_files_exist_missing_file_is_critical() {
        let rel = scratch_rel("missing");
        let r = checkFilesExist(&json!([format!("{rel}/ghost.ts")]));
        let arr = r.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["code"], json!("FILE_MISSING"));
        assert_eq!(arr[0]["severity"], json!("critical"));
        assert!(arr[0]["message"].as_str().unwrap().contains("doesn't exist on disk"));
    }

    #[test]
    fn check_files_exist_empty_file_is_warning() {
        let rel = scratch_rel("empty");
        fs::create_dir_all(scratch_abs(&rel)).unwrap();
        let file_rel = format!("{rel}/empty.ts");
        fs::write(scratch_abs(&file_rel), "").unwrap();
        let r = checkFilesExist(&json!([file_rel]));
        let arr = r.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["code"], json!("FILE_EMPTY"));
        assert_eq!(arr[0]["severity"], json!("warning"));
        cleanup(&rel);
    }

    #[test]
    fn check_files_exist_present_and_nonempty_has_no_findings() {
        let rel = scratch_rel("real");
        fs::create_dir_all(scratch_abs(&rel)).unwrap();
        let file_rel = format!("{rel}/real.ts");
        fs::write(scratch_abs(&file_rel), "export const x = 1;").unwrap();
        let r = checkFilesExist(&json!([file_rel]));
        assert_eq!(r.as_array().unwrap().len(), 0);
        cleanup(&rel);
    }

    #[test]
    fn check_acceptance_mentions_reflected_has_no_findings() {
        let rel = scratch_rel("ac-hit");
        fs::create_dir_all(scratch_abs(&rel)).unwrap();
        let file_rel = format!("{rel}/src.ts");
        fs::write(
            scratch_abs(&file_rel),
            "export function authenticateUser(userId, password) { return \"validated\"; }",
        )
        .unwrap();
        let r = checkAcceptanceMentions(
            &json!([file_rel]),
            &json!(["authenticate user with userId and validate password"]),
        );
        assert_eq!(r.as_array().unwrap().len(), 0);
        cleanup(&rel);
    }

    #[test]
    fn check_acceptance_mentions_unreflected_is_warning() {
        let rel = scratch_rel("ac-miss");
        fs::create_dir_all(scratch_abs(&rel)).unwrap();
        let file_rel = format!("{rel}/src.ts");
        fs::write(scratch_abs(&file_rel), "export const x = 1;").unwrap();
        let r = checkAcceptanceMentions(
            &json!([file_rel]),
            &json!(["payment processor handles stripe webhooks with idempotent retry"]),
        );
        let arr = r.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["code"], json!("ACCEPTANCE_UNMET_HEURISTIC"));
        assert_eq!(arr[0]["severity"], json!("warning"));
        cleanup(&rel);
    }

    #[test]
    fn check_acceptance_mentions_empty_acceptance_short_circuits() {
        assert_eq!(checkAcceptanceMentions(&json!([]), &json!([])), json!([]));
        assert_eq!(checkAcceptanceMentions(&json!(null), &json!(null)), json!([]));
    }

    #[test]
    fn scans_pl_ratified_and_above_by_default() {
        let c = db();
        let rel = scratch_rel("scan-default");
        fs::create_dir_all(scratch_abs(&rel)).unwrap();
        let file_rel = format!("{rel}/real.ts");
        fs::write(scratch_abs(&file_rel), "export const x = 1;").unwrap();
        seed(&c, "CON-1", "pl_ratified", &json!([file_rel]).to_string(), "[]");
        seed(&c, "CON-2", "proposed", &json!([format!("{rel}/ghost.ts")]).to_string(), "[]");

        let r = manAuditDriftSpecVsImpl(&json!({}), &c).unwrap();
        assert_eq!(r["contracts_scanned"], json!(1));
        assert_eq!(r["results"][0]["contract_slug"], json!("CON-1"));
        assert_eq!(r["results"][0]["critical_count"], json!(0));
        assert_eq!(r["overall_health"], json!("green"));
        let keys: Vec<&String> = r.as_object().unwrap().keys().collect();
        assert_eq!(keys, vec!["contracts_scanned", "contracts_with_drift", "total_critical", "overall_health", "results"]);
        cleanup(&rel);
    }

    #[test]
    fn contract_pointing_at_missing_files_is_red_health() {
        let c = db();
        let rel = scratch_rel("scan-red");
        let files = json!([
            format!("{rel}/a.ts"), format!("{rel}/b.ts"), format!("{rel}/c.ts"), format!("{rel}/d.ts")
        ]);
        seed(&c, "CON-X", "pl_ratified", &files.to_string(), "[]");

        let r = manAuditDriftSpecVsImpl(&json!({ "contract_slug": "CON-X" }), &c).unwrap();
        assert_eq!(r["contracts_scanned"], json!(1));
        assert_eq!(r["results"][0]["critical_count"], json!(4));
        assert_eq!(r["overall_health"], json!("red"));
    }

    #[test]
    fn invalid_status_min_errs() {
        let c = db();
        let err = manAuditDriftSpecVsImpl(&json!({ "status_min": "bogus" }), &c).unwrap_err();
        assert!(err.contains("status_min must be one of"));
        assert!(err.contains("got 'bogus'"));
    }

    #[test]
    fn contract_slug_lookup_ignores_status_min() {
        let c = db();
        seed(&c, "CON-PROPOSED", "proposed", "[]", "[]");
        let r = manAuditDriftSpecVsImpl(&json!({ "contract_slug": "CON-PROPOSED" }), &c).unwrap();
        assert_eq!(r["contracts_scanned"], json!(1));
        assert_eq!(r["results"][0]["status"], json!("proposed"));
    }

    #[test]
    fn no_contracts_is_green_with_zero_scanned() {
        let c = db();
        let r = manAuditDriftSpecVsImpl(&json!({}), &c).unwrap();
        assert_eq!(r["contracts_scanned"], json!(0));
        assert_eq!(r["contracts_with_drift"], json!(0));
        assert_eq!(r["total_critical"], json!(0));
        assert_eq!(r["overall_health"], json!("green"));
        assert_eq!(r["results"], json!([]));
    }
}
