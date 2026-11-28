//! bench-laws-overhead.rs — twin of skills/bench/bench-laws-overhead.mjs. NAMES ARE CONTRACT:
//! `benchLawsOverhead` (default).
//!
//! BESPOKE-NATIVE, not conform-db fixture-tested: this skill's ENTIRE PURPOSE is measuring real
//! wall-clock latency of `skillRun` (`performance.now()` deltas around N real dispatches) to
//! compute a "law enforcement overhead" number. The playbook's frozen-clock lane freezes
//! `performance.now()` to a constant specifically so fixtures stay deterministic — which would
//! make every sample here `0` and defeat the whole point of the benchmark. So, like
//! `bench-record.rs` (real `git rev-parse HEAD`) and `driver-schedule.rs`/`driver-state-machine.rs`
//! (real fs probes), this unit is proven by real `#[cfg(test)]` native tests against a real
//! `std::time::Instant` clock and a real (in-memory) `engine_action_run::skillRun` dispatch loop,
//! not a fixture.
//!
//! Cross-module calls (pre-existing Rust twins, per NAMES-ARE-CONTRACT):
//!   - `crate::engine_action_run::skillRun` for every dispatch (warmup + both timed loops).
//!   - `crate::bench_bench_stats::benchStats` for the min/max/avg/percentile reduction.
//!   - `crate::bench_bench_record::benchRecord` for the 4 `brain.bench_runs` inserts.
//!
//! `ANTILOKI_SKIP_LAW_ENFORCEMENT` toggle: ported verbatim (`set_var` before the "without" loop,
//! `remove_var` after) to mirror the JS structure byte-for-byte. NOTE for future readers: JS's
//! own `action-run.mjs` reads this env var to skip its hard-law enforcement loop, but the CURRENT
//! `engine_action_run::skillRun` twin's module doc documents that env-gated branch as NOT ported
//! (the conform-db harness never sets it, so the Rust law-enforcement loop always runs
//! unconditionally) — meaning today, in this crate, the "without" and "with" timed loops both
//! exercise the SAME code path and are expected to converge to near-zero overhead. That is a
//! faithful consequence of the already-established action-run.rs scope decision, not a new gap
//! introduced here; if a future twin threads the skip-flag through `skillRun`, this file's
//! semantics automatically become the real differential without any change on this side.
//!
//! `run_id` default (`bench-${Date.now()}`) uses the REAL wall clock (`SystemTime::now()`), same
//! non-determinism class as `gitSha()` in bench-record.rs — not frozen, by design (bespoke-native).
//!
//! `context.db` missing → JS throws `err.contextMissing`; unreachable here (`db: &Connection` is
//! never absent), same convention as every sibling twin.

use rusqlite::Connection;
use serde_json::{json, Value};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

fn timed(n: i64, db: &Connection, action: &str, skill_input: &Value) -> Result<Vec<f64>, String> {
    let mut samples = Vec::with_capacity(n.max(0) as usize);
    for _ in 0..n {
        let input = json!({ "action": action, "input": skill_input });
        let t = Instant::now();
        crate::engine_action_run::skillRun(&input, db)?;
        samples.push(t.elapsed().as_secs_f64() * 1000.0);
    }
    Ok(samples)
}

#[allow(non_snake_case)]
pub fn benchLawsOverhead(input: &Value, db: &Connection) -> Result<Value, String> {
    let run_id = input
        .get("run_id")
        .and_then(Value::as_str)
        .map(|s| s.to_string())
        .unwrap_or_else(|| format!("bench-{}", now_ms()));
    let iterations = input.get("iterations").and_then(Value::as_i64).unwrap_or(50);
    let action = input
        .get("action")
        .and_then(Value::as_str)
        .unwrap_or("board-keeper.card.list")
        .to_string();
    let skill_input = input.get("skill_input").cloned().unwrap_or_else(|| json!({}));

    // Warmup
    for _ in 0..3 {
        let winput = json!({ "action": action, "input": skill_input });
        crate::engine_action_run::skillRun(&winput, db)?;
    }

    // Without enforcement
    // SAFETY: mirrors JS's `process.env.ANTILOKI_SKIP_LAW_ENFORCEMENT = 'true'` / `delete`.
    unsafe {
        std::env::set_var("ANTILOKI_SKIP_LAW_ENFORCEMENT", "true");
    }
    let without_result = timed(iterations, db, &action, &skill_input);
    unsafe {
        std::env::remove_var("ANTILOKI_SKIP_LAW_ENFORCEMENT");
    }
    let without = without_result?;

    // With enforcement (default — context.agent absent means no permission check,
    // but the law-check loop still runs for hard laws with hooks)
    let with_ = timed(iterations, db, &action, &skill_input)?;

    let w = crate::bench_bench_stats::benchStats(&json!({ "samples": without }), &Value::Null);
    let e = crate::bench_bench_stats::benchStats(&json!({ "samples": with_ }), &Value::Null);
    let w_avg = w.get("avg").and_then(Value::as_f64).unwrap_or(0.0);
    let e_avg = e.get("avg").and_then(Value::as_f64).unwrap_or(0.0);
    let overhead = e_avg - w_avg;
    let overhead_pct = if w_avg > 0.0 { (overhead / w_avg) * 100.0 } else { 0.0 };

    let ctx = json!({ "iterations": iterations, "action": action });

    crate::bench_bench_record::benchRecord(
        &json!({ "run_id": run_id, "bench_name": "laws-overhead", "metric": "without_avg_ms", "value": w_avg, "unit": "ms", "context": ctx }),
        db,
    )?;
    crate::bench_bench_record::benchRecord(
        &json!({ "run_id": run_id, "bench_name": "laws-overhead", "metric": "with_avg_ms", "value": e_avg, "unit": "ms", "context": ctx }),
        db,
    )?;
    crate::bench_bench_record::benchRecord(
        &json!({ "run_id": run_id, "bench_name": "laws-overhead", "metric": "overhead_ms", "value": overhead, "unit": "ms", "context": ctx }),
        db,
    )?;
    crate::bench_bench_record::benchRecord(
        &json!({ "run_id": run_id, "bench_name": "laws-overhead", "metric": "overhead_pct", "value": overhead_pct, "unit": "%", "context": ctx }),
        db,
    )?;

    Ok(json!({
        "run_id": run_id,
        "without_avg_ms": w_avg,
        "with_avg_ms": e_avg,
        "overhead_ms": overhead,
        "overhead_pct": overhead_pct,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    const ACTIONS_SCHEMA: &str = "CREATE TABLE actions (name TEXT UNIQUE, description TEXT, owner_agent TEXT, steps_json TEXT, returns TEXT, on_failure TEXT, kind TEXT)";
    const RUNTIME_EVENTS_SCHEMA: &str = "CREATE TABLE runtime_events (id INTEGER PRIMARY KEY AUTOINCREMENT, ts TEXT NOT NULL DEFAULT '2026-01-01 00:00:00', parent_event_id INTEGER, actor TEXT NOT NULL, agent TEXT, action TEXT, skill TEXT, duration_ms REAL, outcome TEXT NOT NULL CHECK (outcome IN ('success','error','blocked','skipped','in_flight')), error_code TEXT, context_json TEXT, user_goal_id INTEGER, source TEXT)";
    const CARDS_SCHEMA: &str = "CREATE TABLE cards (slug TEXT, status TEXT, score REAL)";
    const LAWS_SCHEMA: &str = "CREATE TABLE laws.laws (id INTEGER PRIMARY KEY, slug TEXT, kind TEXT, domain TEXT, statement TEXT, enforcement TEXT, enforcement_hook TEXT, superseded_by INTEGER)";
    const LAW_VIOLATIONS_SCHEMA: &str = "CREATE TABLE laws.law_violations (id INTEGER PRIMARY KEY, law_id INTEGER NOT NULL, attempted_at TEXT NOT NULL DEFAULT '2026-01-01 00:00:00', attempted_by TEXT NOT NULL, agent_name TEXT, action_name TEXT, context_json TEXT, outcome TEXT NOT NULL, CHECK (outcome IN ('blocked', 'warned', 'escaped', 'allowed_exception')))";
    const BENCH_RUNS_SCHEMA: &str = "CREATE TABLE brain.bench_runs (id INTEGER PRIMARY KEY AUTOINCREMENT, ts TEXT NOT NULL DEFAULT (datetime('now')), run_id TEXT NOT NULL, bench_name TEXT NOT NULL, metric TEXT NOT NULL, value REAL NOT NULL, unit TEXT, context_json TEXT, git_sha TEXT)";

    fn seeded_db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS laws; ATTACH ':memory:' AS brain;").unwrap();
        c.execute_batch(&format!(
            "{ACTIONS_SCHEMA}; {RUNTIME_EVENTS_SCHEMA}; {CARDS_SCHEMA}; {LAWS_SCHEMA}; {LAW_VIOLATIONS_SCHEMA}; {BENCH_RUNS_SCHEMA};
             INSERT INTO cards VALUES ('A','doing',5),('B','backlog',NULL);
             INSERT INTO actions (name, description, owner_agent, steps_json, returns, kind) VALUES
               ('board-keeper.card.list', 'list cards', 'board-keeper', '[{{\"skill\":\"card-list\",\"input\":{{}},\"as\":\"cards\"}}]', '$.results.cards', 'atomic');"
        ))
        .unwrap();
        c
    }

    // SAFETY: ANTILOKI_SKIP_LAW_ENFORCEMENT is process-global; serialize against other files'
    // tests via the shared crate-wide lock (see test_env_lock.rs module doc). This file's own
    // production code also mutates the SAME var internally (see benchLawsOverhead above), so
    // running two of THIS file's tests concurrently would themselves race even under the lock —
    // acquire it around the whole call to keep every test's set/unset pair atomic end-to-end.
    fn run_locked(input: &Value, db: &Connection) -> Result<Value, String> {
        let _guard = crate::test_env_lock::ENV_LOCK.lock().unwrap();
        benchLawsOverhead(input, db)
    }

    #[test]
    fn default_run_measures_five_dispatches_and_records_four_metrics() {
        let c = seeded_db();
        let r = run_locked(&json!({ "iterations": 2 }), &c).unwrap();

        assert!(r["run_id"].as_str().unwrap().starts_with("bench-"));
        assert!(r["without_avg_ms"].as_f64().unwrap() >= 0.0);
        assert!(r["with_avg_ms"].as_f64().unwrap() >= 0.0);
        assert!(r["overhead_ms"].is_number());
        assert!(r["overhead_pct"].is_number());

        // 3 warmup + 2 without + 2 with = 7 total skillRun dispatches. Each dispatch logs a
        // parent runtime_events row (RUNTIME-LOG-V1) plus one row per inner step (1 step here),
        // so 7 dispatches * 2 rows = 14 (verified against real JS action-run.mjs's logFn calls).
        let events = crate::db_json::query_rows(&c, "SELECT * FROM runtime_events", &[]).unwrap();
        assert_eq!(events.as_array().unwrap().len(), 14);

        let runs = crate::db_json::query_rows(&c, "SELECT * FROM brain.bench_runs ORDER BY id", &[]).unwrap();
        let rows = runs.as_array().unwrap();
        assert_eq!(rows.len(), 4);
        let metrics: Vec<&str> = rows.iter().map(|row| row["metric"].as_str().unwrap()).collect();
        assert_eq!(metrics, vec!["without_avg_ms", "with_avg_ms", "overhead_ms", "overhead_pct"]);
        assert!(rows.iter().all(|row| row["bench_name"] == json!("laws-overhead")));
        assert!(rows.iter().all(|row| row["run_id"] == rows[0]["run_id"]));
    }

    #[test]
    fn explicit_run_id_and_action_are_threaded_through() {
        let c = seeded_db();
        let r = run_locked(
            &json!({ "run_id": "my-run", "iterations": 1, "action": "board-keeper.card.list" }),
            &c,
        )
        .unwrap();
        assert_eq!(r["run_id"], json!("my-run"));

        let runs = crate::db_json::query_rows(&c, "SELECT context_json FROM brain.bench_runs", &[]).unwrap();
        let ctx: Value = serde_json::from_str(runs[0]["context_json"].as_str().unwrap()).unwrap();
        assert_eq!(ctx["iterations"], json!(1));
        assert_eq!(ctx["action"], json!("board-keeper.card.list"));
    }

    #[test]
    fn skip_law_enforcement_env_var_is_unset_again_after_the_without_loop() {
        let c = seeded_db();
        let _guard = crate::test_env_lock::ENV_LOCK.lock().unwrap();
        std::env::remove_var("ANTILOKI_SKIP_LAW_ENFORCEMENT");
        benchLawsOverhead(&json!({ "iterations": 1 }), &c).unwrap();
        assert!(std::env::var("ANTILOKI_SKIP_LAW_ENFORCEMENT").is_err());
    }

    #[test]
    fn unknown_action_propagates_the_dispatch_error() {
        let c = seeded_db();
        let err = run_locked(&json!({ "iterations": 1, "action": "nope.action" }), &c).unwrap_err();
        assert_eq!(err, "action-run: action 'nope.action' not found");
    }
}
