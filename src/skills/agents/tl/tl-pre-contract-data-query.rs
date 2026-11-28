//! tl-pre-contract-data-query.rs — twin of skills/agents/tl/tl-pre-contract-data-query.mjs
//! (db READ lane + REAL filesystem reads). NAMES ARE CONTRACT: `tlPreContractDataQuery`
//! (default). TL inspects live state before signing a contract: file existence under the repo
//! root, prior contract overlaps (`contracts` table), and correction/lesson memories mentioning
//! touched-file path tokens (`memory.memories`, attached db).
//!
//! REAL FS SIDE EFFECT — NOT CONFORM-DB FIXTURABLE: `existsSync`/`statSync` resolve every
//! `files_touched` entry against `REPO_ROOT` (four levels up from the module's own file — the
//! same workspace root housing antiloki-core / antiloki-crossing / antiloki-core-rust as
//! siblings, the identical target compute-complexity-risk.rs reaches with three '..' from
//! skills/entities). This is a HARDCODED constant, not DB-derived (unlike stack-facts.rs's
//! workspace-root lane), so the conform-db harness has no seam to seed real files on disk.
//! Verified instead via a bespoke #[cfg(test)] Rust-native test that writes real temp files
//! under the resolved repo root and cleans them up (mirrors the compute-complexity-risk.rs /
//! god-driver-halt.rs precedents for real-fs-anchored twins). The `contracts` +
//! `memory.memories` reads (real db, ordinary cross-db lane) are exercised in the SAME native
//! tests via an in-memory db with `ATTACH DATABASE ':memory:' AS memory`. See the manifest:
//! testing_lane = "bespoke-native" — no frontier/fixture entries for this file (single exported
//! fn, whole file is fs-anchored).
//!
//! `context.db` missing → JS throws `err.contextMissing(...)` — unreachable here (Rust's
//! `db: &Connection` is a required, non-nullable reference), same "not modeled" convention as
//! every sibling twin. `draft_contract` missing/falsy IS a reachable guard-throw (JS
//! `err.required('tl-pre-contract-data-query', 'draft_contract')` → message
//! `"tl-pre-contract-data-query: draft_contract is required"`); ported as `Err(String)` per
//! gotcha #9 but not fixture-exercised (guard-throws are success-path-only per the playbook).
// QUALITY: 79/100 — file-existence/directory conflicts (silent-skip on stat error, matching the
// JS try/catch), prior-contract file overlap (raw overlap values kept, not string-coerced),
// correction/lesson memory token hits (duplicate tokens preserved, matching JS flatMap), empty-
// acceptance sanity conflict, criticals-gated advice string. `contract_slug` key OMITTED when
// `draft_contract.slug` is absent (JS `undefined` → JSON.stringify drops the key), kept as
// explicit `null` when present-but-null.

use rusqlite::Connection;
use serde_json::{json, Map, Value};

fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}
fn str_qq(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(),
        Some(o) => o.to_string(),
    }
}
fn tmpl(v: Option<&Value>) -> String {
    match v {
        None => "undefined".into(),
        Some(Value::Null) => "null".into(),
        Some(x) => str_qq(Some(x)),
    }
}

// JS: HERE = dirname(this module) = antiloki-core/skills/agents/tl. REPO_ROOT =
// resolve(HERE, '..','..','..','..') — four '..' lands on the workspace root (identical target
// compute-complexity-risk.rs reaches with three '..' from skills/entities). CARGO_MANIFEST_DIR
// here is antiloki-core-rust; its parent is that same root on this checkout layout.
fn repo_root() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("antiloki-core-rust has a parent dir")
        .to_path_buf()
}

pub fn tlPreContractDataQuery(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS: if (!db) throw err.contextMissing(...) — unreachable, db: &Connection is non-nullable.
    let draft_contract_v = input.get("draft_contract");
    if !truthy(draft_contract_v) {
        return Err("tl-pre-contract-data-query: draft_contract is required".into());
    }
    let draft_contract = draft_contract_v.unwrap();
    let slug = draft_contract.get("slug").cloned(); // None ↔ JS `slug === undefined` (key absent)
    let files_touched: Vec<Value> = draft_contract.get("files_touched").and_then(Value::as_array).cloned().unwrap_or_default();
    let acceptance: Vec<Value> = draft_contract.get("acceptance").and_then(Value::as_array).cloned().unwrap_or_default();

    let mut conflicts: Vec<Value> = Vec::new();
    let mut assumptions: Vec<Value> = Vec::new();

    // 1. File existence
    let root = repo_root();
    for f_v in &files_touched {
        let f = f_v.as_str().unwrap_or(""); // contract: files_touched entries are strings
        let abs = root.join(f);
        if !abs.exists() {
            conflicts.push(json!({
                "severity": "warning", "code": "NEW_FILE_OK",
                "message": format!("'{f}' doesn't exist yet — DEV will create it. (warning, not blocking)"),
            }));
        } else if let Ok(meta) = std::fs::metadata(&abs) {
            // JS statSync() wrapped in try/catch — permission errors etc are silently skipped.
            if meta.is_dir() {
                conflicts.push(json!({
                    "severity": "critical", "code": "PATH_IS_DIRECTORY",
                    "message": format!("'{f}' is a directory, not a file — contract.files_touched must list files"),
                }));
            }
        }
    }

    // 2. Prior contracts that touched the same files
    if !files_touched.is_empty() {
        let slug_param = match &slug { Some(v) if !v.is_null() => v.clone(), _ => json!("") }; // JS `slug ?? ''`
        let rows = crate::db_json::query_rows(
            db,
            "SELECT slug, files_touched_json, status FROM contracts WHERE slug != ? AND status NOT IN ('rejected','superseded')",
            &[slug_param],
        )?;
        for c in rows.as_array().cloned().unwrap_or_default() {
            let prev_files: Vec<Value> = c
                .get("files_touched_json")
                .and_then(Value::as_str)
                .and_then(|s| serde_json::from_str::<Value>(s).ok())
                .and_then(|v| v.as_array().cloned())
                .unwrap_or_default(); // JS `JSON.parse(c.files_touched_json ?? '[]')` w/ catch → []
            let overlap: Vec<Value> = files_touched.iter().filter(|f| prev_files.contains(f)).cloned().collect();
            if !overlap.is_empty() {
                let c_slug = c.get("slug").cloned().unwrap_or(Value::Null);
                let c_status = c.get("status").cloned().unwrap_or(Value::Null);
                conflicts.push(json!({
                    "severity": "warning", "code": "PRIOR_CONTRACT_OVERLAP",
                    "message": format!(
                        "contract '{}' ({}) already touches {} of the same file(s): {}",
                        tmpl(Some(&c_slug)), tmpl(Some(&c_status)), overlap.len(),
                        overlap.iter().take(3).map(|v| str_qq(Some(v))).collect::<Vec<_>>().join(", "),
                    ),
                    "context": { "other_contract": c_slug, "overlap": overlap },
                }));
            }
        }
    }

    // 3. Memories of type='correction'/'lesson' that mention touched-file path tokens
    let mut words: Vec<String> = Vec::new();
    for f_v in &files_touched {
        let f = f_v.as_str().unwrap_or("");
        for w in f.split(|c| c == '/' || c == '\\') {
            if w.chars().count() > 3 {
                words.push(w.to_string());
            }
        }
    }
    if !words.is_empty() {
        let rows = crate::db_json::query_rows(
            db,
            "SELECT id, title, guidance FROM memory.memories WHERE type IN ('correction','lesson') AND COALESCE(superseded_by,'') = '' LIMIT 200",
            &[],
        )?;
        for m in rows.as_array().cloned().unwrap_or_default() {
            let corpus = format!("{} {}", tmpl(m.get("title")), str_qq(m.get("guidance"))).to_lowercase();
            let hits: Vec<String> = words.iter().filter(|w| corpus.contains(&w.to_lowercase())).cloned().collect();
            if !hits.is_empty() {
                assumptions.push(json!({
                    "memory_id": m.get("id").cloned().unwrap_or(Value::Null),
                    "title": m.get("title").cloned().unwrap_or(Value::Null),
                    "relevance": format!(
                        "mentions {} token(s) from files_touched: {}",
                        hits.len(),
                        hits.iter().take(3).cloned().collect::<Vec<_>>().join(", "),
                    ),
                }));
            }
        }
    }

    // 4. Acceptance criteria sanity
    if acceptance.is_empty() {
        conflicts.push(json!({
            "severity": "warning", "code": "NO_ACCEPTANCE",
            "message": "contract has empty acceptance criteria — TL should require at least 1 testable AC",
        }));
    }

    let criticals = conflicts.iter().filter(|c| c.get("severity").and_then(Value::as_str) == Some("critical")).count();
    let advice = if criticals == 0 {
        if assumptions.is_empty() {
            "No conflicts found. TL may proceed with sign.".to_string()
        } else {
            format!(
                "No conflicts but {} relevant memory/lesson(s) — TL should review before signing.",
                assumptions.len()
            )
        }
    } else {
        format!("Refuse to sign: {criticals} critical conflict(s).")
    };

    let mut out = Map::new();
    out.insert("ok".into(), json!(criticals == 0));
    if let Some(s) = slug {
        out.insert("contract_slug".into(), s); // absent draft_contract.slug → JS `undefined` → key OMITTED (handled by not entering this branch)
    }
    out.insert("files_scanned".into(), json!(files_touched.len() as i64));
    out.insert("conflict_count".into(), json!(conflicts.len() as i64));
    out.insert("critical_count".into(), json!(criticals as i64));
    out.insert("conflicts".into(), Value::Array(conflicts));
    out.insert("assumptions_to_check".into(), Value::Array(assumptions));
    out.insert("advice".into(), json!(advice));
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn db0() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "ATTACH DATABASE ':memory:' AS memory;
             CREATE TABLE contracts (id INTEGER PRIMARY KEY AUTOINCREMENT, slug TEXT NOT NULL UNIQUE, files_touched_json TEXT NOT NULL DEFAULT '[]', status TEXT NOT NULL DEFAULT 'proposed');
             CREATE TABLE memory.memories (id INTEGER PRIMARY KEY AUTOINCREMENT, type TEXT NOT NULL, title TEXT NOT NULL, guidance TEXT NOT NULL, superseded_by INTEGER);",
        )
        .unwrap();
        c
    }

    fn temp_subdir(tag: &str) -> std::path::PathBuf {
        let dir = repo_root().join(format!(".tmp-tlpcdq-rs-test-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn missing_draft_contract_errs() {
        let c = db0();
        let err = tlPreContractDataQuery(&json!({}), &c).unwrap_err();
        assert_eq!(err, "tl-pre-contract-data-query: draft_contract is required");
    }

    #[test]
    fn empty_draft_contract_defaults_and_no_acceptance_conflict() {
        let c = db0();
        let r = tlPreContractDataQuery(&json!({ "draft_contract": {} }), &c).unwrap();
        assert_eq!(r["ok"], json!(true));
        assert!(r.get("contract_slug").is_none()); // slug absent → key OMITTED
        assert_eq!(r["files_scanned"], json!(0));
        assert_eq!(r["conflict_count"], json!(1));
        assert_eq!(r["critical_count"], json!(0));
        assert_eq!(r["conflicts"][0]["code"], json!("NO_ACCEPTANCE"));
        assert_eq!(r["assumptions_to_check"], json!([]));
        assert_eq!(r["advice"], json!("No conflicts found. TL may proceed with sign."));
    }

    #[test]
    fn null_slug_is_kept_as_explicit_null() {
        let c = db0();
        let r = tlPreContractDataQuery(&json!({ "draft_contract": { "slug": null, "acceptance": [{"ac": 1}] } }), &c).unwrap();
        assert!(r.get("contract_slug").is_some());
        assert_eq!(r["contract_slug"], Value::Null);
    }

    #[test]
    fn new_file_and_directory_conflicts_real_fs() {
        let c = db0();
        let dir = temp_subdir("fs");
        fs::write(dir.join("real.txt"), "hi").unwrap();
        fs::create_dir_all(dir.join("adir")).unwrap();
        let rel_dir = dir.file_name().unwrap().to_str().unwrap().to_string();
        let r = tlPreContractDataQuery(
            &json!({ "draft_contract": {
                "slug": "C-NEW",
                "files_touched": [format!("{rel_dir}/real.txt"), format!("{rel_dir}/missing.txt"), format!("{rel_dir}/adir")],
                "acceptance": [{"ac": 1}],
            } }),
            &c,
        )
        .unwrap();
        let conflicts = r["conflicts"].as_array().unwrap();
        assert_eq!(conflicts.len(), 2); // real.txt exists+is-file → no conflict
        assert!(conflicts.iter().any(|x| x["code"] == json!("NEW_FILE_OK") && x["message"].as_str().unwrap().contains("missing.txt")));
        assert!(conflicts.iter().any(|x| x["code"] == json!("PATH_IS_DIRECTORY") && x["severity"] == json!("critical")));
        assert_eq!(r["ok"], json!(false)); // a critical conflict present
        assert_eq!(r["critical_count"], json!(1));
        assert!(r["advice"].as_str().unwrap().starts_with("Refuse to sign: 1 critical conflict(s)."));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn prior_contract_overlap_excludes_self_rejected_and_superseded() {
        let c = db0();
        c.execute_batch(
            "INSERT INTO contracts (slug, files_touched_json, status) VALUES
                ('OTHER-A', '[\"src/a.ts\",\"src/b.ts\"]', 'proposed'),
                ('OTHER-B', '[\"src/a.ts\"]', 'rejected'),
                ('OTHER-C', '[\"src/a.ts\"]', 'superseded'),
                ('SELF', '[\"src/a.ts\"]', 'proposed');",
        )
        .unwrap();
        let r = tlPreContractDataQuery(
            &json!({ "draft_contract": { "slug": "SELF", "files_touched": ["src/a.ts", "src/z.ts"], "acceptance": [{"ac": 1}] } }),
            &c,
        )
        .unwrap();
        let overlaps: Vec<&Value> = r["conflicts"].as_array().unwrap().iter().filter(|x| x["code"] == json!("PRIOR_CONTRACT_OVERLAP")).collect();
        assert_eq!(overlaps.len(), 1); // only OTHER-A: self excluded by slug!=, rejected/superseded excluded by status
        assert_eq!(overlaps[0]["context"]["other_contract"], json!("OTHER-A"));
        assert_eq!(overlaps[0]["context"]["overlap"], json!(["src/a.ts"]));
        assert!(overlaps[0]["message"].as_str().unwrap().contains("contract 'OTHER-A' (proposed) already touches 1 of the same file(s): src/a.ts"));
    }

    #[test]
    fn memory_hits_filtered_by_type_and_superseded_and_short_tokens_ignored() {
        let c = db0();
        c.execute_batch(
            "INSERT INTO memory.memories (type, title, guidance, superseded_by) VALUES
                ('correction', 'auth bug', 'watch out for login.ts edge case', NULL),
                ('lesson', 'unrelated', 'nothing matching here', NULL),
                ('correction', 'stale', 'mentions login.ts but superseded', 3),
                ('decision', 'wrong type', 'login.ts mentioned but wrong type', NULL);",
        )
        .unwrap();
        // files_touched: "src/auth/login.ts" -> tokens ["src","auth","login.ts"] (all len>3)
        let r = tlPreContractDataQuery(
            &json!({ "draft_contract": { "slug": "S1", "files_touched": ["src/auth/login.ts"], "acceptance": [{"ac": 1}] } }),
            &c,
        )
        .unwrap();
        let assumptions = r["assumptions_to_check"].as_array().unwrap();
        assert_eq!(assumptions.len(), 1);
        assert_eq!(assumptions[0]["title"], json!("auth bug"));
        assert!(assumptions[0]["relevance"].as_str().unwrap().contains("login.ts"));
    }

    #[test]
    fn short_path_tokens_produce_no_words_and_skip_memory_query() {
        let c = db0();
        c.execute_batch("INSERT INTO memory.memories (type, title, guidance) VALUES ('correction', 'x', 'y');").unwrap();
        let r = tlPreContractDataQuery(
            &json!({ "draft_contract": { "slug": "S2", "files_touched": ["a/b.ts"], "acceptance": [{"ac": 1}] } }),
            &c,
        )
        .unwrap();
        // "a" and "b.ts" both have length <= 3 -> no words -> memory query skipped entirely
        assert_eq!(r["assumptions_to_check"], json!([]));
    }

    #[test]
    fn nonempty_acceptance_has_no_no_acceptance_conflict() {
        let c = db0();
        let r = tlPreContractDataQuery(&json!({ "draft_contract": { "acceptance": [{"ac": 1}] } }), &c).unwrap();
        assert!(r["conflicts"].as_array().unwrap().iter().all(|x| x["code"] != json!("NO_ACCEPTANCE")));
    }
}
