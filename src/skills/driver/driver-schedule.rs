//! driver-schedule.rs — twin of skills/driver/driver-schedule.mjs. NAMES ARE CONTRACT:
//! `driverSchedule` (default). Pure-READ scheduler: walks actionable cards in priority/FIFO
//! order and asks driver-state-machine what to do with each.
//!
//! BESPOKE-NATIVE, not conform-db fixture-tested: the single exported `driverSchedule` unit calls
//! `driver_driver_state_machine::driverStateMachine` UNCONDITIONALLY for every non-backed-off
//! candidate card, and that twin is itself bespoke-native (its module doc: the fixture-safe db-read
//! half is inseparably mixed with a real filesystem probe — `workspaceIsGitRepo` /
//! `getWorkspaceRoot(db)` + `existsSync`). Because driver-schedule's own output is downstream of
//! that non-fixture-seedable call on every iteration of its main loop, the whole function inherits
//! the same bespoke-native category (same precedent as driver-state-machine.rs / driver-status.rs /
//! stale-pid-files.rs), proven via `#[cfg(test)]` native tests instead of a conform-db fixture.
//!
//! Frozen-clock lane (applied for project-wide consistency even though this module never runs
//! through the conform-db two-lane comparison): the flat fallback window in `recentFailureCount`
//! (`datetime('now', '-10 minutes')`) is written as `datetime('2026-01-01 00:00:00', '-10 minutes')`.
//!
//! `context.db` is always injected by the db lane in production, so the JS `if (!db) throw
//! err.contextMissing(...)` guard is unreachable here (mirrors every sibling twin's convention).
//!
//! Cross-module calls (pre-existing Rust twin): `crate::driver_driver_state_machine::driverStateMachine`.

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

const BACKOFF_MIN_FAILURES: i64 = 2;
const BACKOFF_WINDOW_MIN: i64 = 10;

fn recent_failure_count(db: &Connection, slug: &str, contract_updated_at: Option<&str>) -> i64 {
    let result = if let Some(cua) = contract_updated_at {
        crate::db_json::query_rows(
            db,
            "SELECT COUNT(*) AS c FROM brain.driver_ticks WHERE card_slug = ? AND outcome = 'failed' AND started_at > ?",
            &[json!(slug), json!(cua)],
        )
    } else {
        crate::db_json::query_rows(
            db,
            &format!(
                "SELECT COUNT(*) AS c FROM brain.driver_ticks WHERE card_slug = ? AND outcome = 'failed' AND started_at >= datetime('2026-01-01 00:00:00', '-{BACKOFF_WINDOW_MIN} minutes')"
            ),
            &[json!(slug)],
        )
    };
    result
        .ok()
        .and_then(|v| v.as_array().and_then(|a| a.first().cloned()))
        .and_then(|r| r.get("c").and_then(Value::as_i64))
        .unwrap_or(0)
}

#[allow(non_snake_case)]
pub fn driverSchedule(input: &Value, db: &Connection) -> Result<Value, String> {
    let lazy = matches!(input.get("lazy"), Some(Value::Bool(true)));

    let cards = crate::db_json::query_rows(
        db,
        "SELECT slug, status, priority, COALESCE(driver_intent, 'auto') AS driver_intent,
          (SELECT updated_at FROM contracts
            WHERE card_slug = cards.slug AND COALESCE(superseded_by, '') = ''
            ORDER BY id DESC LIMIT 1) AS contract_updated_at
        FROM cards
        WHERE status NOT IN ('done', 'blocked', 'archived')
          AND COALESCE(driver_intent, 'auto') NOT IN ('paused', 'stopped')
        ORDER BY
          CASE WHEN COALESCE(driver_intent, 'auto') = 'play' THEN 0 ELSE 1 END,
          CASE WHEN priority IS NULL THEN 1 ELSE 0 END,
          priority DESC,
          created_at ASC,
          id ASC",
        &[],
    )?;
    let cards_arr = cards.as_array().cloned().unwrap_or_default();

    let mut all_candidates: Vec<Value> = Vec::new();
    let mut chosen_card: Option<Value> = None;
    let mut chosen_decision: Option<Value> = None;

    for c in &cards_arr {
        let slug = c.get("slug").cloned().unwrap_or(Value::Null);
        let slug_str = slug.as_str().unwrap_or("").to_string();
        let status = c.get("status").cloned().unwrap_or(Value::Null);
        let contract_updated_at = c.get("contract_updated_at").and_then(|v| v.as_str());

        let failures = recent_failure_count(db, &slug_str, contract_updated_at);
        if failures >= BACKOFF_MIN_FAILURES {
            let mut decision = Map::new();
            decision.insert("matched_rule".into(), json!("per-card-backoff"));
            decision.insert("next_agent".into(), Value::Null);
            decision.insert("next_action".into(), Value::Null);
            decision.insert(
                "reason".into(),
                json!(format!(
                    "card backed off: {failures} failed tick(s) in {BACKOFF_WINDOW_MIN}min — retries automatically after the window clears"
                )),
            );
            decision.insert("terminal".into(), json!(false));

            let mut cand = Map::new();
            cand.insert("slug".into(), slug.clone());
            cand.insert("status".into(), status.clone());
            cand.insert("decision".into(), Value::Object(decision));
            cand.insert("backoff".into(), json!(true));
            all_candidates.push(Value::Object(cand));
            continue;
        }

        let decision = crate::driver_driver_state_machine::driverStateMachine(
            &json!({ "card_slug": slug_str }),
            db,
        )?;

        let mut cand = Map::new();
        cand.insert("slug".into(), slug.clone());
        cand.insert("status".into(), status.clone());
        cand.insert("decision".into(), decision.clone());
        all_candidates.push(Value::Object(cand));

        let terminal = truthy(decision.get("terminal"));
        let next_action_not_null = !matches!(decision.get("next_action"), Some(Value::Null));
        if chosen_card.is_none() && !terminal && next_action_not_null {
            chosen_card = Some(slug);
            chosen_decision = Some(decision);
            if lazy {
                break;
            }
        }
    }

    let walk_truncated =
        lazy && chosen_card.is_some() && all_candidates.len() < cards_arr.len();

    let mut out = Map::new();
    out.insert("chosen_card".into(), chosen_card.unwrap_or(Value::Null));
    out.insert("decision".into(), chosen_decision.unwrap_or(Value::Null));
    out.insert("all_candidates".into(), Value::Array(all_candidates));
    out.insert("walk_truncated".into(), json!(walk_truncated));
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_env_lock::ENV_LOCK;
    // driverSchedule's rule block reads ANTILOKI_PIPELINE_V2, which driver-state-machine's and
    // eval-run-one's tests SET (under this same lock) while running. A reader that takes no lock
    // still observes those writes — guarding only the writer narrows a race without closing it —
    // so every test here holds it too. Observed as `matched_rule` flipping to the legacy branch
    // in a full-suite run while passing in isolation.


    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS brain; ATTACH ':memory:' AS judgments; ATTACH ':memory:' AS code;")
            .unwrap();
        c.execute_batch(
            "CREATE TABLE cards (id INTEGER PRIMARY KEY AUTOINCREMENT, slug TEXT NOT NULL UNIQUE, status TEXT, priority INTEGER, body_json TEXT, driver_intent TEXT NOT NULL DEFAULT 'auto', skip_pl_flag INTEGER NOT NULL DEFAULT 0, auto_merge INTEGER NOT NULL DEFAULT 0, parent_slug TEXT, created_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')));
             CREATE TABLE contracts (id INTEGER PRIMARY KEY AUTOINCREMENT, card_slug TEXT, slug TEXT, status TEXT, tier TEXT, size TEXT, microtasks_json TEXT, current_attempt INTEGER, sandbox_path TEXT, worktree_path TEXT, dev_microtask_progress INTEGER, pl_review_count INTEGER, rejection_reason_md TEXT, failure_kind TEXT, superseded_by TEXT, created_at TEXT, updated_at TEXT, last_qa_consumed_run_id INTEGER, amendments_applied INTEGER);
             CREATE TABLE man_reports (id INTEGER PRIMARY KEY AUTOINCREMENT, contract_slug TEXT, phase TEXT, verdict TEXT);
             CREATE TABLE qa_runs (id INTEGER PRIMARY KEY AUTOINCREMENT, contract_slug TEXT, ts TEXT, all_passed INTEGER, scope_passed INTEGER, acceptance_pass INTEGER, acceptance_fail INTEGER, escalation_reason TEXT, suggested_microtasks_json TEXT, verdict TEXT);
             CREATE TABLE pl_sessions (id INTEGER PRIMARY KEY AUTOINCREMENT, status TEXT, approved_card_slug TEXT);
             CREATE TABLE pl_questions (id INTEGER PRIMARY KEY AUTOINCREMENT, session_id INTEGER, step TEXT);
             CREATE TABLE pl_answers (id INTEGER PRIMARY KEY AUTOINCREMENT, question_id INTEGER);
             CREATE TABLE pl_screenings (card_slug TEXT);
             CREATE TABLE build_gate_runs (id INTEGER PRIMARY KEY AUTOINCREMENT, contract_slug TEXT, ts TEXT, passed INTEGER, skipped INTEGER, build_cmd TEXT, evidence_md TEXT, applied_at TEXT, phase TEXT, snippets_hash TEXT);
             CREATE TABLE test_gate_runs (id INTEGER PRIMARY KEY AUTOINCREMENT, contract_slug TEXT, ts TEXT, passed INTEGER, skipped INTEGER, test_cmd TEXT, phase TEXT, snippets_hash TEXT);
             CREATE TABLE contract_qa_reviews (id INTEGER PRIMARY KEY AUTOINCREMENT, contract_slug TEXT, ts TEXT, passed INTEGER, verdict TEXT, findings_md TEXT, issues_json TEXT, reviewer TEXT, failure_target TEXT, review_mode TEXT);
             CREATE TABLE card_phase_events (id INTEGER PRIMARY KEY AUTOINCREMENT, card_slug TEXT, phase TEXT);
             CREATE TABLE card_review_blueprints (card_slug TEXT, contract_slug TEXT, attempt INTEGER);
             CREATE TABLE swarm_manifests (id INTEGER PRIMARY KEY AUTOINCREMENT, contract_slug TEXT, superseded_by TEXT);
             CREATE TABLE swarm_blocks (id INTEGER PRIMARY KEY AUTOINCREMENT, contract_slug TEXT, block_id TEXT, file TEXT, content TEXT);
             CREATE TABLE blueprints (id INTEGER PRIMARY KEY AUTOINCREMENT, contract_slug TEXT, version INTEGER, superseded_by TEXT, blueprint_json TEXT);
             CREATE TABLE blueprint_snippets (id INTEGER PRIMARY KEY AUTOINCREMENT, blueprint_id INTEGER, block_id TEXT, code TEXT);
             CREATE TABLE cockpit_pull_requests (id INTEGER PRIMARY KEY AUTOINCREMENT, source TEXT, ref_slug TEXT, status TEXT);
             CREATE TABLE workspaces (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL UNIQUE, root_path TEXT NOT NULL, vcs TEXT NOT NULL DEFAULT 'git', is_active INTEGER NOT NULL DEFAULT 0, created_at TEXT);
             CREATE TABLE brain.driver_ticks (id INTEGER PRIMARY KEY AUTOINCREMENT, card_slug TEXT, decided_action TEXT, outcome TEXT, started_at TEXT, error_message TEXT);
             CREATE TABLE brain.brain_decisions (id INTEGER PRIMARY KEY AUTOINCREMENT, session_id TEXT NOT NULL, decided_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')), card_slug TEXT, decision TEXT NOT NULL, rationale TEXT NOT NULL, inputs_json TEXT, fire_id INTEGER, CHECK (decision IN ('fire','wait','rollback','escalate','done','pause','session_start','session_end','goal_set','classify','gate_verdict','rule_match','sandbox_create','review_pass','schedule_and_dispatch','agent_step')));
             CREATE TABLE judgments.judgment_cases (id INTEGER PRIMARY KEY AUTOINCREMENT, card_slug TEXT, scope_kind TEXT, scope_ref TEXT, verdict TEXT, goal_line TEXT, updated_at TEXT, judgment_id INTEGER);
             CREATE TABLE judgments.judgment_items (id INTEGER PRIMARY KEY AUTOINCREMENT, judgment_id INTEGER, statement TEXT, judge_why TEXT, side TEXT, judge_value INTEGER);
             CREATE TABLE code.code_nodes (id INTEGER PRIMARY KEY AUTOINCREMENT, origin TEXT);",
        )
        .unwrap();
        c
    }

    fn insert_card(c: &Connection, slug: &str, status: &str, priority: Option<i64>, created_offset_sec: i64, driver_intent: &str) {
        c.execute(
            "INSERT INTO cards (slug, status, priority, driver_intent, created_at) VALUES (?, ?, ?, ?, datetime('2026-01-01 00:00:00', ?))",
            rusqlite::params![
                slug,
                status,
                priority,
                driver_intent,
                format!("{created_offset_sec} seconds"),
            ],
        )
        .unwrap();
    }

    #[test]
    fn no_actionable_cards_returns_null_chosen() {
        let _env = ENV_LOCK.lock().unwrap();
        let c = db();
        insert_card(&c, "CARD-DONE", "done", Some(5), 0, "auto");
        let r = driverSchedule(&json!({}), &c).unwrap();
        assert_eq!(r["chosen_card"], Value::Null);
        assert_eq!(r["decision"], Value::Null);
        assert_eq!(r["all_candidates"], json!([]));
        assert_eq!(r["walk_truncated"], json!(false));
    }

    #[test]
    fn picks_user_prioritized_backlog_card_with_no_contract() {
        let _env = ENV_LOCK.lock().unwrap();
        // priority=5 + no contract + no PL session: pipelineV2Enabled defaults to true (no
        // env override), and the v2 rule block is checked before the legacy 'pl-start' rule,
        // so 'v2-needs-screening' fires instead — confirmed against the real JS skill.
        let c = db();
        insert_card(&c, "CARD-1", "backlog", Some(5), 0, "auto");
        let r = driverSchedule(&json!({}), &c).unwrap();
        assert_eq!(r["chosen_card"], json!("CARD-1"));
        assert_eq!(r["decision"]["matched_rule"], json!("v2-needs-screening"));
        assert_eq!(r["all_candidates"].as_array().unwrap().len(), 1);
        assert_eq!(r["all_candidates"][0]["slug"], json!("CARD-1"));
    }

    #[test]
    fn play_intent_sorts_above_priority_and_paused_stopped_are_excluded() {
        let _env = ENV_LOCK.lock().unwrap();
        let c = db();
        insert_card(&c, "CARD-LOW", "backlog", Some(5), 0, "auto");
        insert_card(&c, "CARD-PLAY", "backlog", None, 100, "play");
        insert_card(&c, "CARD-PAUSED", "backlog", Some(5), -100, "paused");
        insert_card(&c, "CARD-STOPPED", "backlog", Some(5), -200, "stopped");
        let r = driverSchedule(&json!({}), &c).unwrap();
        let slugs: Vec<&str> = r["all_candidates"]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x["slug"].as_str().unwrap())
            .collect();
        assert_eq!(slugs, vec!["CARD-PLAY", "CARD-LOW"]);
        assert_eq!(r["chosen_card"], json!("CARD-PLAY"));
    }

    #[test]
    fn per_card_backoff_skips_thrashing_card_but_others_still_flow() {
        let _env = ENV_LOCK.lock().unwrap();
        let c = db();
        insert_card(&c, "CARD-BAD", "backlog", Some(5), 0, "auto");
        insert_card(&c, "CARD-GOOD", "backlog", Some(5), 10, "auto");
        c.execute(
            "INSERT INTO brain.driver_ticks (card_slug, outcome, started_at) VALUES ('CARD-BAD', 'failed', '2026-01-01 00:05:00')",
            [],
        )
        .unwrap();
        c.execute(
            "INSERT INTO brain.driver_ticks (card_slug, outcome, started_at) VALUES ('CARD-BAD', 'failed', '2026-01-01 00:06:00')",
            [],
        )
        .unwrap();
        let r = driverSchedule(&json!({}), &c).unwrap();
        assert_eq!(r["all_candidates"][0]["slug"], json!("CARD-BAD"));
        assert_eq!(r["all_candidates"][0]["backoff"], json!(true));
        assert_eq!(r["all_candidates"][0]["decision"]["matched_rule"], json!("per-card-backoff"));
        assert_eq!(r["chosen_card"], json!("CARD-GOOD"));
    }

    #[test]
    fn contract_update_clears_backoff_window() {
        let _env = ENV_LOCK.lock().unwrap();
        let c = db();
        insert_card(&c, "CARD-REWOUND", "backlog", Some(5), 0, "auto");
        c.execute(
            "INSERT INTO contracts (card_slug, slug, status, superseded_by, updated_at) VALUES ('CARD-REWOUND', 'CT-1', 'rejected', '', '2026-01-01 00:10:00')",
            [],
        )
        .unwrap();
        // Failures BEFORE the contract's updated_at must not count toward backoff.
        c.execute(
            "INSERT INTO brain.driver_ticks (card_slug, outcome, started_at) VALUES ('CARD-REWOUND', 'failed', '2026-01-01 00:01:00')",
            [],
        )
        .unwrap();
        c.execute(
            "INSERT INTO brain.driver_ticks (card_slug, outcome, started_at) VALUES ('CARD-REWOUND', 'failed', '2026-01-01 00:02:00')",
            [],
        )
        .unwrap();
        let r = driverSchedule(&json!({}), &c).unwrap();
        assert_eq!(r["all_candidates"][0].get("backoff"), None);
    }

    #[test]
    fn lazy_mode_stops_at_first_actionable_card_and_marks_walk_truncated() {
        let _env = ENV_LOCK.lock().unwrap();
        let c = db();
        insert_card(&c, "CARD-1", "backlog", Some(5), 0, "auto");
        insert_card(&c, "CARD-2", "backlog", Some(5), 10, "auto");
        let r = driverSchedule(&json!({ "lazy": true }), &c).unwrap();
        assert_eq!(r["chosen_card"], json!("CARD-1"));
        assert_eq!(r["all_candidates"].as_array().unwrap().len(), 1);
        assert_eq!(r["walk_truncated"], json!(true));
    }
}
