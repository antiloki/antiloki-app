//! review-contract.rs — twin of skills/swarm/review-contract.mjs (db, cross-db judgments.*
//! via swarm_review). NAMES ARE CONTRACT: default `reviewContract`.
//!
//! Runs the adversarial REVIEW SWARM (`swarm_review::review`) over every authored block of a
//! contract's latest (non-superseded) swarm manifest, aggregates the per-file verdicts, and
//! records ONE advisory `contract_qa_reviews` row (review_mode='review-swarm').
//!
//! PARITY-DEFERRED: `review()` itself ends in the deferred `crate::ai_ai_call::aiCall` stub
//! (always `Err` — see review.rs's module doc). This twin's JS source has NO try/catch around
//! `await review(...)`, so the `?` here propagates that Err straight out of `reviewContract`,
//! exactly mirroring the JS's unguarded await. The reachable deterministic surface — the
//! contract_slug guard, the manifest/blocks lookup, and the empty-blocks aggregate path (which
//! never reaches `review()` at all) — is ported byte-for-byte and is what this file's own tests
//! exercise, matching book-run.rs / review.rs's convention.
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
fn str_qq(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(),
        Some(o) => o.to_string(),
    }
}
fn nz(v: Option<&Value>) -> Option<&Value> {
    v.filter(|x| !x.is_null())
}
fn query_one(db: &Connection, sql: &str, params: &[Value]) -> Result<Option<Value>, String> {
    let rows = crate::db_json::query_rows(db, sql, params)?;
    Ok(rows.as_array().and_then(|a| a.first()).cloned())
}

/// JS: `export default async function reviewContract(input, context)`. `context` folds into
/// `db` (required, non-nullable per Rust convention); `provider_fn` (JS function-reference DI)
/// has no Rust equivalent and is passed through to `swarm_review::review` as `null`, matching
/// that module's own convention.
pub fn reviewContract(input: &Value, db: &Connection) -> Result<Value, String> {
    let slug_v = input.get("contract_slug").cloned().unwrap_or(Value::Null);
    if !truthy(Some(&slug_v)) {
        return Err("review-contract: contract_slug is required".into());
    }
    let slug = str_qq(Some(&slug_v));

    let man = query_one(
        db,
        "SELECT id FROM swarm_manifests WHERE contract_slug = ? AND superseded_by IS NULL ORDER BY version DESC, id DESC LIMIT 1",
        &[json!(slug)],
    )?;
    let blocks: Vec<Value> = match &man {
        Some(m) => crate::db_json::query_rows(
            db,
            "SELECT block_id, file, content FROM swarm_blocks WHERE manifest_id = ? ORDER BY id",
            &[m.get("id").cloned().unwrap_or(Value::Null)],
        )?
        .as_array()
        .cloned()
        .unwrap_or_default(),
        None => vec![],
    };

    let mut per_file: Vec<Value> = Vec::with_capacity(blocks.len());
    for a in &blocks {
        let file = nz(a.get("file")).map(|v| str_qq(Some(v))).unwrap_or_default();
        let content = nz(a.get("content")).map(|v| str_qq(Some(v))).unwrap_or_default();
        let review_input = json!({
            "contract_slug": slug, "scope_kind": "microtask", "scope_ref": file,
            "snippets": [{ "path": file, "content": content }],
        });
        // PARITY-DEFERRED: propagates review()'s deferred aiCall Err, mirroring the JS's
        // unguarded `await review(...)` (no try/catch in the source).
        let r = crate::swarm_review::review(&review_input, db)?;
        per_file.push(json!({
            "file": file,
            "judgment_id": r.get("judgment_id").cloned().unwrap_or(Value::Null),
            "verdict": r.get("verdict").cloned().unwrap_or(Value::Null),
        }));
    }

    let all_pass = !per_file.is_empty()
        && per_file.iter().all(|p| p.get("verdict").and_then(|v| v.as_str()) == Some("pass"));
    let failed: Vec<&Value> = per_file
        .iter()
        .filter(|p| p.get("verdict").and_then(|v| v.as_str()) != Some("pass"))
        .collect();
    let findings = if per_file.is_empty() {
        "Review swarm: no authored blocks to review.".to_string()
    } else if all_pass {
        format!("Review swarm (advisory): all {} file(s) pass.", per_file.len())
    } else {
        let list = failed
            .iter()
            .map(|f| format!("{} ({})", str_qq(f.get("file")), str_qq(f.get("verdict"))))
            .collect::<Vec<_>>()
            .join(", ");
        format!("Review swarm (advisory): {}/{} file(s) flagged — {}.", failed.len(), per_file.len(), list)
    };

    let reviewer = nz(input.get("reviewer")).map(|v| str_qq(Some(v))).unwrap_or_else(|| "driver:auto".to_string());
    let failed_json = json!(failed.iter().map(|f| (*f).clone()).collect::<Vec<_>>());
    crate::db_json::run_write(
        db,
        "INSERT INTO contract_qa_reviews (contract_slug, passed, verdict, findings_md, issues_json, reviewer, failure_target, review_mode)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        &[
            json!(slug),
            json!(all_pass),
            json!(if all_pass { "pass" } else { "fail" }),
            json!(findings),
            json!(serde_json::to_string(&failed_json).unwrap_or_default()),
            json!(reviewer),
            json!("microtasks"),
            json!("review-swarm"),
        ],
    )?;

    let failed_files: Vec<Value> = failed.iter().map(|f| f.get("file").cloned().unwrap_or(Value::Null)).collect();
    Ok(json!({
        "contract_slug": slug,
        "files": per_file.len() as i64,
        "all_pass": all_pass,
        "failed": failed_files,
        "advisory": true,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conn() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("
            CREATE TABLE contracts (id INTEGER PRIMARY KEY, slug TEXT, card_slug TEXT, acceptance_json TEXT, microtasks_json TEXT, files_touched_json TEXT);
            CREATE TABLE cards (id INTEGER PRIMARY KEY, slug TEXT, title TEXT, status TEXT, body_json TEXT, acceptance_json TEXT);
            CREATE TABLE swarm_manifests (id INTEGER PRIMARY KEY AUTOINCREMENT, contract_slug TEXT NOT NULL, version INTEGER NOT NULL DEFAULT 1, goal TEXT, manifest_json TEXT NOT NULL DEFAULT '{}', lint_passed INTEGER NOT NULL DEFAULT 0, lint_json TEXT NOT NULL DEFAULT '{}', created_by TEXT NOT NULL DEFAULT 'unknown', created_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')), superseded_by INTEGER);
            CREATE TABLE swarm_blocks (id INTEGER PRIMARY KEY AUTOINCREMENT, contract_slug TEXT NOT NULL, manifest_id INTEGER, block_id TEXT NOT NULL, file TEXT NOT NULL, content TEXT, authored_by TEXT, compose_passed INTEGER, compose_json TEXT, authored_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')));
            CREATE TABLE contract_qa_reviews (id INTEGER PRIMARY KEY AUTOINCREMENT, contract_slug TEXT NOT NULL, passed INTEGER NOT NULL, verdict TEXT NOT NULL, findings_md TEXT, issues_json TEXT, reviewer TEXT, failure_target TEXT NOT NULL DEFAULT 'microtasks', review_mode TEXT);
            CREATE TABLE blueprints (id INTEGER PRIMARY KEY, contract_slug TEXT, version INTEGER, lint_passed INTEGER, blueprint_json TEXT, superseded_by INTEGER);
            CREATE TABLE pl_screenings (id INTEGER PRIMARY KEY, card_slug TEXT, screening_json TEXT);
        ").unwrap();
        c.execute_batch("ATTACH ':memory:' AS brain; ATTACH ':memory:' AS judgments; ATTACH ':memory:' AS knowledge;").unwrap();
        c.execute_batch("
            CREATE TABLE brain.prompt_templates ( id INTEGER PRIMARY KEY, name TEXT NOT NULL, version INTEGER NOT NULL DEFAULT 1, template_md TEXT NOT NULL, default_tier TEXT NOT NULL, default_temperature TEXT NOT NULL, max_output_tokens INTEGER NOT NULL DEFAULT 1024, superseded_by INTEGER, created_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')), cross_field_rules_json TEXT, judging_criteria_json TEXT, system_md TEXT, phase_flow TEXT NOT NULL DEFAULT 'single', UNIQUE (name, version) );
            CREATE TABLE brain.prompt_output_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL DEFAULT 0, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, grounded_refs_json TEXT, UNIQUE (prompt_template_id, field_name) );
            CREATE TABLE brain.prompt_input_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL DEFAULT 0, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, UNIQUE (prompt_template_id, field_name) );
        ").unwrap();
        c.execute_batch("
            CREATE TABLE judgments.judgment_cases (judgment_id TEXT PRIMARY KEY, card_slug TEXT NOT NULL, scope_kind TEXT NOT NULL, scope_ref TEXT NOT NULL DEFAULT '', goal_line TEXT, n_items INTEGER, defender_trues INTEGER, prosecutor_trues INTEGER, defender_rate REAL, prosecutor_rate REAL, defender_accuracy REAL, prosecutor_accuracy REAL, verdict TEXT, created_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')), updated_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')), snippets_hash TEXT);
            CREATE TABLE judgments.judgment_items (id INTEGER PRIMARY KEY AUTOINCREMENT, judgment_id TEXT NOT NULL, side TEXT NOT NULL, item_id TEXT NOT NULL, statement TEXT NOT NULL DEFAULT '', links_json TEXT NOT NULL DEFAULT '[]', grounded INTEGER NOT NULL DEFAULT 1, judge_value INTEGER, judge_accuracy REAL, judge_why TEXT, created_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')), UNIQUE (judgment_id, side, item_id));
            CREATE TABLE judgments.judgment_calls (id INTEGER PRIMARY KEY AUTOINCREMENT, judgment_id TEXT NOT NULL, role TEXT NOT NULL, tier TEXT, prompt_md TEXT, response_md TEXT, ai_call_id INTEGER, created_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')));
            CREATE TABLE judgments.court_evidence (judgment_id TEXT PRIMARY KEY, contract_slug TEXT, scope_kind TEXT, scope_ref TEXT, corpus_json TEXT, evidence_md TEXT, n_blocks INTEGER, n_acceptance INTEGER, has_card INTEGER, has_blueprint INTEGER, has_screening INTEGER, created_at TEXT DEFAULT '2026-01-01 00:00:00');
        ").unwrap();
        c
    }

    #[test]
    fn missing_contract_slug_is_required() {
        let c = conn();
        let err = reviewContract(&json!({}), &c).unwrap_err();
        assert!(err.contains("contract_slug is required"));
    }

    #[test]
    fn no_blocks_records_an_empty_advisory_verdict_without_calling_review() {
        let c = conn();
        c.execute("INSERT INTO cards (slug, title, status) VALUES ('card-e','e','doing')", []).unwrap();
        c.execute(
            "INSERT INTO contracts (slug, card_slug, acceptance_json, microtasks_json) VALUES ('ct-e','card-e','[]','[]')",
            [],
        )
        .unwrap();
        let r = reviewContract(&json!({ "contract_slug": "ct-e" }), &c).unwrap();
        assert_eq!(r["files"], json!(0));
        assert_eq!(r["all_pass"], json!(false));
        assert_eq!(r["advisory"], json!(true));
        let v = query_one(&c, "SELECT passed, review_mode, findings_md FROM contract_qa_reviews WHERE contract_slug='ct-e'", &[]).unwrap().unwrap();
        assert_eq!(v["review_mode"], json!("review-swarm"));
        assert_eq!(v["passed"], json!(0));
        assert_eq!(v["findings_md"], json!("Review swarm: no authored blocks to review."));
    }

    #[test]
    fn no_manifest_at_all_also_records_empty_advisory_verdict() {
        let c = conn();
        c.execute("INSERT INTO cards (slug, title, status) VALUES ('card-n','n','doing')", []).unwrap();
        c.execute(
            "INSERT INTO contracts (slug, card_slug, acceptance_json, microtasks_json) VALUES ('ct-n','card-n','[]','[]')",
            [],
        )
        .unwrap();
        let r = reviewContract(&json!({ "contract_slug": "ct-n" }), &c).unwrap();
        assert_eq!(r["files"], json!(0));
        assert_eq!(r["all_pass"], json!(false));
    }

    #[test]
    fn blocks_present_propagates_deferred_ai_call_err_from_review() {
        let c = conn();
        c.execute("INSERT INTO cards (slug, title, status, body_json) VALUES ('card-rc','rc','doing','{\"kind\":\"feature\",\"size\":\"S\",\"why\":\"x\"}')", []).unwrap();
        c.execute(
            "INSERT INTO contracts (slug, card_slug, acceptance_json, microtasks_json) VALUES ('ct-rc','card-rc','[{\"kind\":\"file_exists\",\"path\":\"a.js\"}]','[]')",
            [],
        )
        .unwrap();
        c.execute("INSERT INTO swarm_manifests (contract_slug, version, goal, manifest_json) VALUES ('ct-rc',1,'g','{}')", []).unwrap();
        let mid = c.last_insert_rowid();
        c.execute(
            "INSERT INTO swarm_blocks (contract_slug, manifest_id, block_id, file, content) VALUES ('ct-rc', ?, 'a', 'a.js', 'export const a = 1')",
            rusqlite::params![mid],
        )
        .unwrap();
        let err = reviewContract(&json!({ "contract_slug": "ct-rc" }), &c).unwrap_err();
        assert!(err.contains("ai-call"), "unexpected error: {err}");
        // no contract_qa_reviews row was written — the loop erred out before aggregation.
        let n = query_one(&c, "SELECT count(*) n FROM contract_qa_reviews", &[]).unwrap().unwrap();
        assert_eq!(n["n"], json!(0));
    }
}
