//! contract-reads.rs — the BE's contract READ queries, moved into skills. NAMES ARE CONTRACT:
//! `contractsPendingHumanApproval`, `contractsRecent`, `contractDetailFull`.
//!
//! WHY THEY EXIST. antiloki-BE's ContractsService ran these as raw SQL against the substrate.
//! Only a skill touches the databases — see board-reads.rs for the full reasoning.
//!
//! `contractDetailFull` deliberately absorbs the WHOLE of the BE's getContract, not just its
//! first query: the detail view is one contract row plus its latest QA run, its QA-retry depth,
//! and its MAN audit reports. Splitting that across four dispatches would make the BE re-implement
//! the assembly (including the refine_history_json parse-and-drop) and leave four ways to get it
//! subtly wrong. One call, one owner, one shape.
// QUALITY: 87/100 — verbatim SQL port, composed detail assembly, malformed-JSON tolerance
use rusqlite::Connection;
use serde_json::{json, Value};

// The list projection. Kept in one place because both list queries promise the same columns to
// the same FE type, and a column added to one and not the other is a silent shape divergence.
const LIST_COLS: &str = "slug, card_slug, title, status, tier, risk, complexity, size, \
                         signed_by, signed_at, sandbox_path, superseded_by, updated_at";

fn limit_of(input: &Value) -> i64 {
    input.get("limit").and_then(|v| v.as_i64()).filter(|&n| n > 0).unwrap_or(50)
}

/// High-risk contracts a human still has to sign off on. This is a QUEUE — the operator's
/// approval inbox — so it orders by most-recently-touched, not by insertion.
pub fn contractsPendingHumanApproval(input: &Value, db: &Connection) -> Result<Value, String> {
    crate::db_json::query_rows(
        db,
        &format!(
            "SELECT {LIST_COLS} FROM contracts
             WHERE status = 'tl_approved' AND tier = 'high-risk' AND superseded_by IS NULL
             ORDER BY updated_at DESC LIMIT ?"
        ),
        &[json!(limit_of(input))],
    )
}

/// The recent contracts feed. Orders by id DESC (insertion), which is NOT the same as updated_at:
/// this answers "what was created lately", while the approval queue answers "what changed lately".
pub fn contractsRecent(input: &Value, db: &Connection) -> Result<Value, String> {
    crate::db_json::query_rows(
        db,
        &format!(
            "SELECT {LIST_COLS} FROM contracts
             WHERE superseded_by IS NULL
             ORDER BY id DESC LIMIT ?"
        ),
        &[json!(limit_of(input))],
    )
}

fn first_row(db: &Connection, sql: &str, params: &[Value]) -> Result<Option<Value>, String> {
    Ok(crate::db_json::query_rows(db, sql, params)?.as_array().and_then(|a| a.first()).cloned())
}

fn count_of(db: &Connection, sql: &str, params: &[Value]) -> Result<i64, String> {
    Ok(first_row(db, sql, params)?
        .and_then(|r| r.get("n").and_then(|v| v.as_i64()))
        .unwrap_or(0))
}

/// One contract, assembled for the detail view.
pub fn contractDetailFull(input: &Value, db: &Connection) -> Result<Value, String> {
    let slug = input
        .get("slug")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .ok_or("contract-detail-full: slug is required")?;

    let mut row = first_row(
        db,
        "SELECT slug, card_slug, title, status, tier, risk, complexity, size,
                signed_by, signed_at, sandbox_path, superseded_by, updated_at,
                briefing_md, goal_md, tl_briefing_md, microtasks_json,
                files_touched_json, acceptance_json, dev_microtask_progress,
                last_qa_consumed_run_id,
                rejection_reason_md, failure_kind,
                refine_history_json,
                current_attempt, base_sha, created_at
         FROM contracts WHERE slug = ?",
        &[json!(slug)],
    )?
    // "not found" is the wording the BE maps onto a 404; the FE distinguishes a missing contract
    // from a broken server on exactly that.
    .ok_or_else(|| format!("contract-detail-full: contract '{slug}' not found"))?;

    let card_slug = row.get("card_slug").cloned().unwrap_or(Value::Null);

    // FEAT-QA-DEV-FEEDBACK-LOOP: the latest QA run, so the detail view can render it inline.
    // Null when QA has not run yet.
    let latest_qa = match first_row(
        db,
        "SELECT id, ts, all_passed, acceptance_pass, acceptance_fail,
                feedback_md, suggested_microtasks_json
         FROM qa_runs WHERE contract_slug = ? ORDER BY id DESC LIMIT 1",
        &[json!(slug)],
    )? {
        None => Value::Null,
        Some(mut run) => {
            // Only the FAILING criteria: the passing ones do not help explain a failure, and
            // shipping all of them makes the payload grow with the acceptance list.
            let failing = crate::db_json::query_rows(
                db,
                "SELECT ac_index, kind, evidence_md
                 FROM qa_run_results WHERE qa_run_id = ? AND passed = 0 ORDER BY ac_index",
                &[run.get("id").cloned().unwrap_or(Value::Null)],
            )?;
            if let Some(o) = run.as_object_mut() {
                o.insert("failing_acs".into(), failing);
            }
            run
        }
    };

    // QA-fail retry depth: how many times the driver re-authored microtasks under the
    // qa-failed-retry rule, and whether it ultimately escalated (cap reached). This is what lets
    // the FE show the under-cap re-author loop explicitly instead of a silent stall.
    let retries = count_of(
        db,
        "SELECT COUNT(*) AS n FROM brain.driver_ticks
         WHERE card_slug = ? AND decided_action = 'tl.refine.microtasks'
           AND decided_reason LIKE 'contract-impl-qa-failed-retry%'",
        &[card_slug.clone()],
    )?;
    let escalated = count_of(
        db,
        "SELECT COUNT(*) AS n FROM brain.driver_ticks
         WHERE card_slug = ? AND decided_reason LIKE 'contract-impl-qa-failed-escalate%'",
        &[card_slug],
    )? > 0;

    // FEAT-REJECTION-RECOVERY-AND-MAN-AUDIT: the MAN audit reports, newest first.
    let man_reports = crate::db_json::query_rows(
        db,
        "SELECT id, phase, ts, verdict, what_happened_md, lessons_md
         FROM man_reports WHERE contract_slug = ? ORDER BY id DESC",
        &[json!(slug)],
    )?;

    // FEAT-SLICED-RETRIES: re-author provenance, one entry per tl.refine pass. Malformed JSON
    // degrades to an empty list rather than failing the whole detail view — a corrupt history
    // field should not make a contract unopenable.
    let refine_history = row
        .get("refine_history_json")
        .and_then(|v| v.as_str())
        .and_then(|s| serde_json::from_str::<Value>(s).ok())
        .filter(|v| v.is_array())
        .unwrap_or_else(|| json!([]));
    if let Some(o) = row.as_object_mut() {
        // Dropped, not returned: the parsed form is what the FE reads, and shipping both invites
        // a consumer to parse the raw string a second, differently-tolerant way.
        o.remove("refine_history_json");
        o.insert("latest_qa_run".into(), latest_qa);
        o.insert("qa_retry".into(), json!({ "retries": retries, "escalated": escalated }));
        o.insert("refine_history".into(), refine_history);
        o.insert("man_reports".into(), man_reports);
    }
    Ok(row)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS brain;").unwrap();
        c.execute_batch(
            "CREATE TABLE contracts (
               id INTEGER PRIMARY KEY, slug TEXT, card_slug TEXT, title TEXT, status TEXT, tier TEXT,
               risk TEXT, complexity TEXT, size TEXT, signed_by TEXT, signed_at TEXT,
               sandbox_path TEXT, superseded_by TEXT, updated_at TEXT,
               briefing_md TEXT, goal_md TEXT, tl_briefing_md TEXT, microtasks_json TEXT,
               files_touched_json TEXT, acceptance_json TEXT, dev_microtask_progress INTEGER,
               last_qa_consumed_run_id INTEGER, rejection_reason_md TEXT, failure_kind TEXT,
               refine_history_json TEXT, current_attempt INTEGER, base_sha TEXT, created_at TEXT
             );
             CREATE TABLE qa_runs (id INTEGER PRIMARY KEY, contract_slug TEXT, ts TEXT, all_passed INTEGER,
                                   acceptance_pass INTEGER, acceptance_fail INTEGER, feedback_md TEXT,
                                   suggested_microtasks_json TEXT);
             CREATE TABLE qa_run_results (qa_run_id INTEGER, ac_index INTEGER, kind TEXT, evidence_md TEXT, passed INTEGER);
             CREATE TABLE man_reports (id INTEGER PRIMARY KEY, contract_slug TEXT, phase TEXT, ts TEXT,
                                       verdict TEXT, what_happened_md TEXT, lessons_md TEXT);
             CREATE TABLE brain.driver_ticks (card_slug TEXT, decided_action TEXT, decided_reason TEXT);

             INSERT INTO contracts (id, slug, card_slug, title, status, tier, superseded_by, updated_at, refine_history_json)
               VALUES (1,'CT-1','C-1','first','tl_approved','high-risk',NULL,'2026-01-01','[{\"mode\":\"sliced\"}]'),
                      (2,'CT-2','C-2','second','tl_approved','high-risk',NULL,'2026-01-03',NULL),
                      (3,'CT-3','C-3','third','draft','low-risk',NULL,'2026-01-02',NULL),
                      (4,'CT-OLD','C-1','superseded','tl_approved','high-risk','CT-1','2026-01-04',NULL);
             INSERT INTO qa_runs VALUES (1,'CT-1','t1',0,2,1,'try again','[]'),
                                        (2,'CT-1','t2',0,1,2,'still failing','[]');
             INSERT INTO qa_run_results VALUES (2,0,'unit','it blew up',0),(2,1,'unit','fine',1),(2,2,'lint','style',0);
             INSERT INTO man_reports VALUES (1,'CT-1','contract_signed','t1','ok','signed','none'),
                                            (2,'CT-1','qa_passed','t2','fail','qa broke','check the mocks');
             INSERT INTO brain.driver_ticks VALUES
               ('C-1','tl.refine.microtasks','contract-impl-qa-failed-retry #1'),
               ('C-1','tl.refine.microtasks','contract-impl-qa-failed-retry #2'),
               ('C-1','tl.refine.microtasks','some-other-reason'),
               ('C-1','driver.halt','contract-impl-qa-failed-escalate cap reached');",
        )
        .unwrap();
        c
    }

    #[test]
    fn pending_approval_is_high_risk_tl_approved_only() {
        let r = contractsPendingHumanApproval(&json!({}), &db()).unwrap();
        let slugs: Vec<&str> = r.as_array().unwrap().iter().map(|x| x["slug"].as_str().unwrap()).collect();
        // CT-3 is low-risk/draft; CT-OLD is superseded. Neither belongs in an approval queue.
        assert_eq!(slugs, ["CT-2", "CT-1"], "newest updated_at first");
    }

    #[test]
    fn recent_orders_by_INSERTION_not_by_update() {
        // The distinction is real: CT-2 was updated last but CT-3 was created last.
        let r = contractsRecent(&json!({}), &db()).unwrap();
        let slugs: Vec<&str> = r.as_array().unwrap().iter().map(|x| x["slug"].as_str().unwrap()).collect();
        assert_eq!(slugs, ["CT-3", "CT-2", "CT-1"]);
    }

    #[test]
    fn recent_excludes_superseded_and_honours_limit() {
        let c = db();
        assert!(!contractsRecent(&json!({}), &c).unwrap().to_string().contains("CT-OLD"));
        assert_eq!(contractsRecent(&json!({ "limit": 1 }), &c).unwrap().as_array().unwrap().len(), 1);
    }

    #[test]
    fn detail_attaches_the_LATEST_qa_run_with_only_its_FAILING_criteria() {
        let d = contractDetailFull(&json!({ "slug": "CT-1" }), &db()).unwrap();
        assert_eq!(d["latest_qa_run"]["id"], json!(2), "the newest run, not the first");
        let failing = d["latest_qa_run"]["failing_acs"].as_array().unwrap();
        assert_eq!(failing.len(), 2, "the passing criterion is not shipped");
        assert_eq!(failing[0]["ac_index"], json!(0));
        assert_eq!(failing[1]["kind"], json!("lint"));
    }

    #[test]
    fn detail_counts_retries_and_detects_escalation() {
        let d = contractDetailFull(&json!({ "slug": "CT-1" }), &db()).unwrap();
        // Only the retry-reason ticks count — the unrelated tick on the same action must not.
        assert_eq!(d["qa_retry"], json!({ "retries": 2, "escalated": true }));
    }

    #[test]
    fn detail_parses_refine_history_and_DROPS_the_raw_column() {
        let d = contractDetailFull(&json!({ "slug": "CT-1" }), &db()).unwrap();
        assert_eq!(d["refine_history"], json!([{ "mode": "sliced" }]));
        assert!(d.get("refine_history_json").is_none(), "the raw string must not also ship");
    }

    #[test]
    fn MALFORMED_refine_history_degrades_to_empty_rather_than_failing_the_view() {
        // A corrupt history field must not make the contract unopenable.
        let c = db();
        c.execute("UPDATE contracts SET refine_history_json = '{not json' WHERE slug = 'CT-1'", []).unwrap();
        assert_eq!(contractDetailFull(&json!({ "slug": "CT-1" }), &c).unwrap()["refine_history"], json!([]));
    }

    #[test]
    fn a_contract_with_no_qa_run_reports_null_not_an_error() {
        let d = contractDetailFull(&json!({ "slug": "CT-2" }), &db()).unwrap();
        assert_eq!(d["latest_qa_run"], Value::Null);
        assert_eq!(d["qa_retry"], json!({ "retries": 0, "escalated": false }));
        assert_eq!(d["man_reports"], json!([]));
    }

    #[test]
    fn man_reports_come_back_newest_first() {
        let d = contractDetailFull(&json!({ "slug": "CT-1" }), &db()).unwrap();
        let ids: Vec<i64> = d["man_reports"].as_array().unwrap().iter().map(|r| r["id"].as_i64().unwrap()).collect();
        assert_eq!(ids, [2, 1]);
    }

    #[test]
    fn a_missing_contract_says_NOT_FOUND_and_a_missing_slug_is_a_validation_error() {
        let c = db();
        assert!(contractDetailFull(&json!({ "slug": "NOPE" }), &c).unwrap_err().contains("not found"));
        assert!(contractDetailFull(&json!({}), &c).unwrap_err().contains("slug is required"));
    }
}
