//! driver-queue.rs — twin of skills/system/driver-queue.mjs. NAMES ARE CONTRACT:
//! `driverQueue` (default). Read-only dispatch queue: wraps driver-schedule (the SAME pure read
//! the driver loop uses to pick the next card) WITHOUT ticking or dispatching, and shapes its
//! all_candidates into an ordered queue with a one-line reason per non-chosen card.
//!
//! BESPOKE-NATIVE, not conform-db fixture-tested: this module's single exported `driverQueue`
//! unit calls `driver_driver_schedule::driverSchedule` UNCONDITIONALLY, and that twin is itself
//! bespoke-native (its module doc: driver-schedule calls driver-state-machine on every
//! non-backed-off candidate, which mixes a real filesystem probe into an otherwise fixture-safe
//! db read). Because driver-queue's own output is entirely downstream of that non-fixture-seedable
//! call, it inherits the same bespoke-native category (same precedent as driver-schedule.rs /
//! driver-state-machine.rs / driver-status.rs / stale-pid-files.rs), proven via `#[cfg(test)]`
//! native tests instead of a conform-db fixture.
//!
//! `context.db` is always injected by the db lane in production, so the JS `if (!db) throw
//! err.contextMissing(...)` guard is unreachable here (mirrors every sibling twin's convention).
//!
//! Cross-module calls (pre-existing Rust twin): `crate::driver_driver_schedule::driverSchedule`.

use rusqlite::Connection;
use serde_json::{json, Map, Value};

fn nz(v: Option<&Value>) -> Option<&Value> {
    v.filter(|x| !x.is_null())
}

fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}

#[allow(non_snake_case)]
pub fn driverQueue(input: &Value, db: &Connection) -> Result<Value, String> {
    let schedule = crate::driver_driver_schedule::driverSchedule(input, db)?;
    let chosen_card = schedule.get("chosen_card").cloned().unwrap_or(Value::Null);
    let all_candidates = schedule
        .get("all_candidates")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    let mut title_stmt = db
        .prepare("SELECT title FROM cards WHERE slug = ?")
        .map_err(|e| e.to_string())?;

    let mut queue: Vec<Value> = Vec::with_capacity(all_candidates.len());
    for (i, c) in all_candidates.iter().enumerate() {
        let empty_decision = Value::Object(Map::new());
        let d = c.get("decision").filter(|v| !v.is_null()).unwrap_or(&empty_decision);
        let slug = c.get("slug").cloned().unwrap_or(Value::Null);
        let slug_str = slug.as_str().unwrap_or("");

        let title: Value = title_stmt
            .query_row(rusqlite::params![slug_str], |row| {
                row.get::<_, Option<String>>(0)
            })
            .ok()
            .flatten()
            .map(Value::String)
            .unwrap_or(Value::Null);

        let terminal = truthy(d.get("terminal"));
        let next_action_not_null = !matches!(d.get("next_action"), None | Some(Value::Null));
        let backoff = truthy(c.get("backoff"));
        let dispatchable = !terminal && next_action_not_null && !backoff;

        let is_next = matches!((&slug, &chosen_card), (Value::String(a), Value::String(b)) if a == b);

        let mut q = Map::new();
        q.insert("position".into(), json!(i as i64));
        q.insert("slug".into(), slug.clone());
        q.insert("title".into(), title.clone());
        q.insert("status".into(), c.get("status").cloned().unwrap_or(Value::Null));
        q.insert("is_next".into(), json!(is_next));
        q.insert("dispatchable".into(), json!(dispatchable));
        q.insert("backoff".into(), json!(backoff));
        q.insert("terminal".into(), json!(terminal));
        q.insert("next_agent".into(), nz(d.get("next_agent")).cloned().unwrap_or(Value::Null));
        q.insert("next_action".into(), nz(d.get("next_action")).cloned().unwrap_or(Value::Null));
        q.insert("reason".into(), nz(d.get("reason")).cloned().unwrap_or(Value::Null));
        queue.push(Value::Object(q));
    }

    let next_entry = queue.iter().find(|q| truthy(q.get("is_next")));
    let next: Value = match next_entry {
        Some(q) => {
            let mut n = Map::new();
            n.insert("slug".into(), q.get("slug").cloned().unwrap_or(Value::Null));
            n.insert("title".into(), q.get("title").cloned().unwrap_or(Value::Null));
            n.insert("agent".into(), q.get("next_agent").cloned().unwrap_or(Value::Null));
            n.insert("action".into(), q.get("next_action").cloned().unwrap_or(Value::Null));
            n.insert("reason".into(), q.get("reason").cloned().unwrap_or(Value::Null));
            Value::Object(n)
        }
        None => Value::Null,
    };

    let total = queue.len() as i64;
    let dispatchable_count = queue.iter().filter(|q| truthy(q.get("dispatchable"))).count() as i64;
    let blocked_count = total - dispatchable_count;

    let mut counts = Map::new();
    counts.insert("total".into(), json!(total));
    counts.insert("dispatchable".into(), json!(dispatchable_count));
    counts.insert("blocked".into(), json!(blocked_count));

    let mut out = Map::new();
    out.insert("next_card".into(), chosen_card);
    out.insert("next".into(), next);
    out.insert("queue".into(), Value::Array(queue));
    out.insert("counts".into(), Value::Object(counts));
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS brain; ATTACH ':memory:' AS judgments; ATTACH ':memory:' AS code;")
            .unwrap();
        c.execute_batch(
            "CREATE TABLE cards (id INTEGER PRIMARY KEY AUTOINCREMENT, slug TEXT NOT NULL UNIQUE, title TEXT, status TEXT, priority INTEGER, body_json TEXT, driver_intent TEXT NOT NULL DEFAULT 'auto', skip_pl_flag INTEGER NOT NULL DEFAULT 0, auto_merge INTEGER NOT NULL DEFAULT 0, parent_slug TEXT, created_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')));
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

    fn insert_card(c: &Connection, slug: &str, title: &str, status: &str, priority: Option<i64>, created_offset_sec: i64, driver_intent: &str) {
        c.execute(
            "INSERT INTO cards (slug, title, status, priority, driver_intent, created_at) VALUES (?, ?, ?, ?, ?, datetime('2026-01-01 00:00:00', ?))",
            rusqlite::params![
                slug,
                title,
                status,
                priority,
                driver_intent,
                format!("{created_offset_sec} seconds"),
            ],
        )
        .unwrap();
    }

    #[test]
    fn empty_board_returns_empty_queue() {
        let c = db();
        let r = driverQueue(&json!({}), &c).unwrap();
        assert_eq!(r["next_card"], Value::Null);
        assert_eq!(r["next"], Value::Null);
        assert_eq!(r["queue"], json!([]));
        assert_eq!(r["counts"], json!({"total": 0, "dispatchable": 0, "blocked": 0}));
    }

    #[test]
    fn actionable_card_is_next_with_title_and_dispatchable_true() {
        let c = db();
        insert_card(&c, "CARD-1", "First Card", "backlog", Some(5), 0, "auto");
        let r = driverQueue(&json!({}), &c).unwrap();
        assert_eq!(r["next_card"], json!("CARD-1"));
        assert_eq!(r["next"]["slug"], json!("CARD-1"));
        assert_eq!(r["next"]["title"], json!("First Card"));
        assert_eq!(r["queue"][0]["position"], json!(0));
        assert_eq!(r["queue"][0]["is_next"], json!(true));
        assert_eq!(r["queue"][0]["dispatchable"], json!(true));
        assert_eq!(r["counts"], json!({"total": 1, "dispatchable": 1, "blocked": 0}));
    }

    #[test]
    fn backed_off_card_is_blocked_and_not_next() {
        let c = db();
        insert_card(&c, "CARD-BAD", "Bad Card", "backlog", Some(5), 0, "auto");
        insert_card(&c, "CARD-GOOD", "Good Card", "backlog", Some(5), 10, "auto");
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
        let r = driverQueue(&json!({}), &c).unwrap();
        assert_eq!(r["next_card"], json!("CARD-GOOD"));
        assert_eq!(r["queue"][0]["slug"], json!("CARD-BAD"));
        assert_eq!(r["queue"][0]["backoff"], json!(true));
        assert_eq!(r["queue"][0]["dispatchable"], json!(false));
        assert_eq!(r["queue"][0]["is_next"], json!(false));
        assert_eq!(r["queue"][0]["reason"].as_str().unwrap().contains("backed off"), true);
        assert_eq!(r["counts"], json!({"total": 2, "dispatchable": 1, "blocked": 1}));
    }

    #[test]
    fn card_with_no_title_row_reports_null_title() {
        let c = db();
        // insert directly without title, bypassing insert_card's NOT NULL title arg
        c.execute(
            "INSERT INTO cards (slug, status, priority, driver_intent, created_at) VALUES ('CARD-NOTITLE', 'backlog', 5, 'auto', datetime('2026-01-01 00:00:00'))",
            [],
        )
        .unwrap();
        let r = driverQueue(&json!({}), &c).unwrap();
        assert_eq!(r["queue"][0]["title"], Value::Null);
    }
}
