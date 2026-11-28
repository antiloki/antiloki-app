//! qa-acceptance-run.rs — twin of skills/agents/qa/qa-acceptance-run.mjs (db lane, thin consumer
//! of the already-converted `agents_dev_dev_acceptance_run::devAcceptanceRun` twin). NAMES ARE
//! CONTRACT: `qaAcceptanceRun` (default; only export in the JS module). Dispatches the deterministic
//! acceptance verifiers via `devAcceptanceRun` and PERSISTS the outcome: one `qa_runs` row +
//! one `qa_run_results` row per AC item, so QA history is queryable per contract without
//! re-running.
//!
//! BESPOKE-NATIVE (not conform-db): this file composes `devAcceptanceRun`, which itself is
//! bespoke-native (real subprocess/fs/git for several AC kinds — see that twin's module doc). A
//! conform-db fixture would have to freeze that non-determinism too, so this twin is proven
//! instead via `#[cfg(test)]` native tests seeding real `contracts`/`qa_runs`/`qa_run_results`
//! rows and DB-only AC kinds (`sql`, unknown-kind), mirroring `qa-acceptance-run.test.mjs`'s own
//! choice to keep its suite hermetic (no filesystem, no git, no AI).
//!
//! ## What is ported faithfully
//! - `context.db` missing → JS `throw err.contextMissing('qa-acceptance-run', 'db')`: not modeled,
//!   `db: &Connection` is a required, non-nullable reference here, same convention as every
//!   sibling db-lane twin (e.g. `qa-scope-check.rs`, `qa-contract-review.rs`).
//! - `contract_slug` missing/falsy → JS `throw err.required('qa-acceptance-run', 'contract_slug')`
//!   → ported as `Err(...)`.
//! - `verifier = context?.actor ?? 'agent:qa'` — but per the established db-lane convention (see
//!   `pl-session-approve.rs`, `man-card-review.rs`, `keeper-journey-append.rs` module docs), this
//!   bespoke-native lane only ever threads `input` + `db` (no context object carrying `actor`), so
//!   `context.actor` is never reachable here; the sole reachable default is the input-first
//!   precedence over the literal `"agent:qa"` (still exactly matching the JS PRECEDENCE test:
//!   `input.verifier` wins whenever present).
//! - The dispatch: `devAcceptanceRun({ contract_slug }, { db })` → `agents_dev_dev_acceptance_run
//!   ::devAcceptanceRun(&json!({"contract_slug": slug}), db)`, propagated with `?` exactly like the
//!   JS `await devAcceptanceRun(...)` (no try/catch — an error here propagates to the caller).
//! - The `qa_runs` INSERT: `scope_passed` always `NULL` (acceptance-only gate), `acceptance_pass`
//!   = `run.passed`, `acceptance_fail` = `run.failed`, `all_passed` = `run.all_passed ? 1 : 0`,
//!   `notes_md` passed through (default `NULL`). `lastInsertRowid` → `db.last_insert_rowid()`.
//! - The `qa_run_results` INSERT, one row per `run.results[i]`: `ac_index` = `r.index`, `kind` =
//!   `r.kind`, `passed` = `r.passed ? 1 : 0`, `evidence_md` = `r.evidence ? JSON.stringify(r.evidence)
//!   .slice(0, 8000) : null` — the 8000-char cap is applied ONLY at this persistence layer; the
//!   in-memory `run.results` returned to the caller keeps the full, uncapped evidence (pinned by
//!   the JS "evidence is capped ... but carries the REAL serialized content" test).
//! - The return shape: `{qa_run_id, contract_slug, verifier, count, passed, failed, all_passed,
//!   results}` — `results` is the UNCAPPED `run.results` from `devAcceptanceRun`, not the
//!   persisted/capped rows.
// QUALITY: thin composer, every line traceable to the JS source; the only disclosed gap is the
// unreachable context.actor path, same convention as every sibling db-lane twin.

use rusqlite::Connection;
use serde_json::{json, Value};

fn take_chars(s: &str, n: usize) -> String { s.chars().take(n).collect() }

#[allow(non_snake_case)]
pub fn qaAcceptanceRun(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS `if (!db) throw err.contextMissing(...)` — unreachable (db: &Connection is required),
    // same convention as every sibling twin.
    let contract_slug = input.get("contract_slug").and_then(Value::as_str).unwrap_or("").to_string();
    if contract_slug.is_empty() {
        return Err("qa-acceptance-run: contract_slug is required".to_string());
    }
    let verifier = input.get("verifier").and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| "agent:qa".to_string()); // context.actor unreachable in this db-only lane
    let notes_md = input.get("notes_md").filter(|v| !v.is_null()).cloned();

    let run = crate::agents_dev_dev_acceptance_run::devAcceptanceRun(&json!({ "contract_slug": contract_slug }), db)?;

    let passed = run.get("passed").and_then(Value::as_i64).unwrap_or(0);
    let failed = run.get("failed").and_then(Value::as_i64).unwrap_or(0);
    let all_passed = run.get("all_passed").and_then(Value::as_bool).unwrap_or(false);
    let count = run.get("count").and_then(Value::as_i64).unwrap_or(0);

    db.execute(
        "INSERT INTO qa_runs (contract_slug, verifier, scope_passed, acceptance_pass, acceptance_fail, all_passed, notes_md) VALUES (?1, ?2, NULL, ?3, ?4, ?5, ?6)",
        rusqlite::params![contract_slug, verifier, passed, failed, if all_passed { 1 } else { 0 }, notes_md.as_ref().and_then(Value::as_str)],
    ).map_err(|e| e.to_string())?;
    let qa_run_id = db.last_insert_rowid();

    let results: Vec<Value> = run.get("results").and_then(Value::as_array).cloned().unwrap_or_default();
    for r in &results {
        let ac_index = r.get("index").and_then(Value::as_i64).unwrap_or(0);
        let kind = r.get("kind").and_then(Value::as_str).unwrap_or("").to_string();
        let r_passed = r.get("passed").and_then(Value::as_bool).unwrap_or(false);
        let evidence = r.get("evidence").filter(|v| !v.is_null());
        let evidence_md = evidence.map(|e| take_chars(&serde_json::to_string(e).unwrap_or_default(), 8000));
        db.execute(
            "INSERT INTO qa_run_results (qa_run_id, ac_index, kind, passed, evidence_md) VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![qa_run_id, ac_index, kind, if r_passed { 1 } else { 0 }, evidence_md],
        ).map_err(|e| e.to_string())?;
    }

    Ok(json!({
        "qa_run_id": qa_run_id,
        "contract_slug": contract_slug,
        "verifier": verifier,
        "count": count,
        "passed": passed,
        "failed": failed,
        "all_passed": all_passed,
        "results": results,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS laws;").unwrap();
        c.execute_batch(
            "CREATE TABLE contracts (id INTEGER PRIMARY KEY AUTOINCREMENT, slug TEXT UNIQUE, card_slug TEXT, status TEXT, sandbox_path TEXT, worktree_path TEXT, bootstrap_cmd TEXT, files_touched_json TEXT, acceptance_json TEXT, generated_paths_json TEXT, updated_at TEXT);
             CREATE TABLE laws.laws (id INTEGER PRIMARY KEY, slug TEXT, kind TEXT, domain TEXT, statement TEXT, enforcement TEXT, enforcement_hook TEXT, superseded_by INTEGER);
             CREATE TABLE laws.law_violations (id INTEGER PRIMARY KEY, law_id INTEGER, outcome TEXT);
             CREATE TABLE qa_runs (id INTEGER PRIMARY KEY AUTOINCREMENT, contract_slug TEXT NOT NULL, ts TEXT NOT NULL DEFAULT (datetime('now')), verifier TEXT NOT NULL, scope_passed INTEGER, acceptance_pass INTEGER, acceptance_fail INTEGER, all_passed INTEGER NOT NULL, notes_md TEXT);
             CREATE TABLE qa_run_results (id INTEGER PRIMARY KEY AUTOINCREMENT, qa_run_id INTEGER NOT NULL, ac_index INTEGER NOT NULL, kind TEXT NOT NULL, passed INTEGER NOT NULL, evidence_md TEXT);",
        ).unwrap();
        c
    }

    fn seed_contract(c: &Connection, slug: &str, acceptance: &Value) {
        c.execute(
            "INSERT INTO contracts (slug, status, acceptance_json) VALUES (?1, 'tl_approved', ?2)",
            rusqlite::params![slug, acceptance.to_string()],
        ).unwrap();
    }

    #[test]
    fn r45_persists_one_qa_runs_row_and_one_qa_run_results_row_per_ac() {
        let c = db();
        let slug = "QA-ACC-R45";
        let acceptance = json!([
            { "kind": "sql", "query": "SELECT 1 AS x" },
            { "kind": "sql", "query": "SELECT 1 AS c", "expect_value_min": 99, "expect_value_path": "$.c" },
            { "kind": "made_up_kind_xyz" },
        ]);
        seed_contract(&c, slug, &acceptance);

        let run = qaAcceptanceRun(&json!({ "contract_slug": slug, "verifier": "agent:qa-bot", "notes_md": "run notes" }), &c).unwrap();

        assert!(run["qa_run_id"].as_i64().unwrap() > 0);
        assert_eq!(run["contract_slug"], json!(slug));
        assert_eq!(run["verifier"], json!("agent:qa-bot"));
        assert_eq!(run["count"], json!(3));
        assert_eq!(run["passed"], json!(1));
        assert_eq!(run["failed"], json!(2));
        assert_eq!(run["all_passed"], json!(false));
        assert_eq!(run["results"].as_array().unwrap().len(), 3);

        let qa_run_id = run["qa_run_id"].as_i64().unwrap();
        let (acceptance_pass, acceptance_fail, all_passed, verifier, notes_md, scope_passed): (i64, i64, i64, String, Option<String>, Option<i64>) = c.query_row(
            "SELECT acceptance_pass, acceptance_fail, all_passed, verifier, notes_md, scope_passed FROM qa_runs WHERE contract_slug = ?1",
            [slug],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?)),
        ).unwrap();
        assert_eq!(acceptance_pass, 1);
        assert_eq!(acceptance_fail, 2);
        assert_eq!(all_passed, 0);
        assert_eq!(verifier, "agent:qa-bot");
        assert_eq!(notes_md.as_deref(), Some("run notes"));
        assert_eq!(scope_passed, None);

        let count: i64 = c.query_row("SELECT COUNT(*) FROM qa_run_results WHERE qa_run_id = ?1", [qa_run_id], |r| r.get(0)).unwrap();
        assert_eq!(count, 3);

        let mut stmt = c.prepare("SELECT ac_index, kind, passed, evidence_md FROM qa_run_results WHERE qa_run_id = ?1 ORDER BY ac_index").unwrap();
        let rows: Vec<(i64, String, i64, String)> = stmt.query_map([qa_run_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))).unwrap().map(|r| r.unwrap()).collect();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows.iter().map(|r| r.0).collect::<Vec<_>>(), vec![0, 1, 2]);
        assert_eq!(rows.iter().map(|r| r.1.clone()).collect::<Vec<_>>(), vec!["sql", "sql", "made_up_kind_xyz"]);
        assert_eq!(rows.iter().map(|r| r.2).collect::<Vec<_>>(), vec![1, 0, 0]);
        let ev0: Value = serde_json::from_str(&rows[0].3).unwrap();
        assert_eq!(ev0, json!({ "row": { "x": 1 } }));
        let ev1: Value = serde_json::from_str(&rows[1].3).unwrap();
        assert_eq!(ev1, json!({ "value": 1, "min": 99 }));
        let ev2: Value = serde_json::from_str(&rows[2].3).unwrap();
        assert_eq!(ev2["reason"], json!("unknown_kind"));
        assert_eq!(ev2["kind"], json!("made_up_kind_xyz"));
    }

    #[test]
    fn evidence_capped_at_8000_chars_but_carries_real_content() {
        let c = db();
        let slug = "QA-ACC-CAP";
        let big = "z".repeat(9000);
        let acceptance = json!([{ "kind": "sql", "query": format!("SELECT '{big}' AS blob") }]);
        seed_contract(&c, slug, &acceptance);

        let run = qaAcceptanceRun(&json!({ "contract_slug": slug, "verifier": "agent:qa" }), &c).unwrap();
        assert_eq!(run["passed"], json!(1));
        assert_eq!(run["results"][0]["evidence"]["row"]["blob"].as_str().unwrap().len(), 9000);

        let qa_run_id = run["qa_run_id"].as_i64().unwrap();
        let ev: String = c.query_row("SELECT evidence_md FROM qa_run_results WHERE qa_run_id = ?1", [qa_run_id], |r| r.get(0)).unwrap();
        assert_eq!(ev.len(), 8000);
        assert!(ev.starts_with("{\"row\":{\"blob\":\"z"));
        assert!(ev.chars().filter(|&c| c == 'z').count() >= 7000);
    }

    #[test]
    fn all_passing_run_records_all_passed_1_and_fail_0() {
        let c = db();
        let slug = "QA-ACC-GREEN";
        let acceptance = json!([
            { "kind": "sql", "query": "SELECT 1 AS x" },
            { "kind": "sql", "query": "SELECT 42 AS c", "expect_value_min": 10, "expect_value_path": "$.c" },
        ]);
        seed_contract(&c, slug, &acceptance);

        let run = qaAcceptanceRun(&json!({ "contract_slug": slug, "verifier": "agent:qa" }), &c).unwrap();
        assert_eq!(run["all_passed"], json!(true));
        assert_eq!(run["passed"], json!(2));
        assert_eq!(run["failed"], json!(0));

        let qa_run_id = run["qa_run_id"].as_i64().unwrap();
        let (all_passed, acceptance_pass, acceptance_fail): (i64, i64, i64) = c.query_row(
            "SELECT all_passed, acceptance_pass, acceptance_fail FROM qa_runs WHERE contract_slug = ?1", [slug],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        ).unwrap();
        assert_eq!(all_passed, 1);
        assert_eq!(acceptance_pass, 2);
        assert_eq!(acceptance_fail, 0);
        let n: i64 = c.query_row("SELECT COUNT(*) FROM qa_run_results WHERE qa_run_id = ?1", [qa_run_id], |r| r.get(0)).unwrap();
        assert_eq!(n, 2);
    }

    #[test]
    fn verifier_falls_back_to_agent_qa_default_when_absent() {
        // context.actor is unreachable in this db-only lane (see module doc); the only reachable
        // precedence is input.verifier vs the literal default.
        let c = db();
        seed_contract(&c, "QA-VER-DEFAULT", &json!([{ "kind": "sql", "query": "SELECT 1 AS x" }]));
        let r = qaAcceptanceRun(&json!({ "contract_slug": "QA-VER-DEFAULT" }), &c).unwrap();
        assert_eq!(r["verifier"], json!("agent:qa"));
    }

    #[test]
    fn input_verifier_wins_when_present() {
        let c = db();
        seed_contract(&c, "QA-VER-EXPLICIT", &json!([{ "kind": "sql", "query": "SELECT 1 AS x" }]));
        let r = qaAcceptanceRun(&json!({ "contract_slug": "QA-VER-EXPLICIT", "verifier": "agent:qa-explicit" }), &c).unwrap();
        assert_eq!(r["verifier"], json!("agent:qa-explicit"));
        let qa_run_id = r["qa_run_id"].as_i64().unwrap();
        let verifier: String = c.query_row("SELECT verifier FROM qa_runs WHERE id = ?1", [qa_run_id], |row| row.get(0)).unwrap();
        assert_eq!(verifier, "agent:qa-explicit");
    }

    #[test]
    fn guards_missing_contract_slug() {
        let c = db();
        assert_eq!(qaAcceptanceRun(&json!({}), &c).unwrap_err(), "qa-acceptance-run: contract_slug is required");
    }
}
