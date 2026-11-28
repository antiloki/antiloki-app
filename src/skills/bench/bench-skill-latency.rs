//! bench-skill-latency.rs — twin of skills/bench/bench-skill-latency.mjs. NAMES ARE CONTRACT:
//! `benchSkillLatency` (default, standard lane).
//!
//! Warms up `warmup` times then runs `action` `iterations` times through `skillRun`
//! (engine_action_run, already-converted twin) timing each call, then records 6 metrics
//! (avg/p50/p95/p99/min/max, all "_ms") via `benchRecord` (bench_bench_record, already-converted
//! twin) using stats from `benchStats` (bench_bench_stats, already-converted twin).
//!
//! `performance.now()` is frozen per the playbook's frozen-clock rule: every call returns the
//! SAME constant, so `performance.now() - t` is always exactly `0.0` for every sample. The
//! collected stats therefore collapse deterministically to all-zero (count = iterations, the
//! rest 0) in BOTH lanes — this is the documented "not just a logged/stored value" exception:
//! the measured duration is only ever logged/returned, never branched on.
use rusqlite::Connection;
use serde_json::{json, Value};

#[allow(non_snake_case)]
pub fn benchSkillLatency(input: &Value, db: &Connection) -> Result<Value, String> {
    let run_id = input
        .get("run_id")
        .and_then(Value::as_str)
        .map(|s| s.to_string())
        .unwrap_or_else(|| format!("bench-{}", crate::clock::now_ms()));
    let action = input
        .get("action")
        .and_then(Value::as_str)
        .map(|s| s.to_string())
        .unwrap_or_else(|| "board-keeper.card.list".to_string());
    let iterations = input.get("iterations").and_then(Value::as_i64).unwrap_or(100);
    let skill_input = input.get("skill_input").cloned().unwrap_or_else(|| json!({}));
    let warmup = input.get("warmup").and_then(Value::as_i64).unwrap_or(5);

    let call_input = json!({ "action": action, "input": skill_input });
    for _ in 0..warmup {
        crate::engine_action_run::skillRun(&call_input, db)?;
    }

    let mut samples: Vec<Value> = Vec::new();
    for _ in 0..iterations {
        crate::engine_action_run::skillRun(&call_input, db)?;
        samples.push(json!(0.0)); // frozen performance.now() delta
    }

    let stats = crate::bench_bench_stats::benchStats(&json!({ "samples": samples }), &Value::Null);
    let ctx = json!({ "action": action, "iterations": iterations, "warmup": warmup });

    for (metric, unit) in [
        ("avg", "ms"),
        ("p50", "ms"),
        ("p95", "ms"),
        ("p99", "ms"),
        ("min", "ms"),
        ("max", "ms"),
    ] {
        let value = stats.get(metric).cloned().unwrap_or(Value::Null);
        crate::bench_bench_record::benchRecord(
            &json!({
                "run_id": run_id,
                "bench_name": "action-latency",
                "metric": format!("{metric}_ms"),
                "value": value,
                "unit": unit,
                "context": ctx,
            }),
            db,
        )?;
    }

    Ok(json!({
        "run_id": run_id,
        "action": action,
        "iterations": iterations,
        "stats": stats,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "ATTACH DATABASE ':memory:' AS laws; \
             ATTACH DATABASE ':memory:' AS brain; \
             CREATE TABLE actions (name TEXT UNIQUE, description TEXT, owner_agent TEXT, steps_json TEXT, returns TEXT, on_failure TEXT, kind TEXT); \
             CREATE TABLE runtime_events (id INTEGER PRIMARY KEY AUTOINCREMENT, ts TEXT NOT NULL DEFAULT '2026-01-01 00:00:00', parent_event_id INTEGER, actor TEXT NOT NULL, agent TEXT, action TEXT, skill TEXT, duration_ms REAL, outcome TEXT NOT NULL CHECK (outcome IN ('success','error','blocked','skipped','in_flight')), error_code TEXT, context_json TEXT, user_goal_id INTEGER, source TEXT); \
             CREATE TABLE cards (slug TEXT, status TEXT, score REAL); \
             CREATE TABLE laws.laws (id INTEGER PRIMARY KEY, slug TEXT, kind TEXT, domain TEXT, statement TEXT, enforcement TEXT, enforcement_hook TEXT, superseded_by INTEGER); \
             CREATE TABLE brain.bench_runs (id INTEGER PRIMARY KEY AUTOINCREMENT, ts TEXT NOT NULL DEFAULT (datetime('now')), run_id TEXT NOT NULL, bench_name TEXT NOT NULL, metric TEXT NOT NULL, value REAL NOT NULL, unit TEXT, context_json TEXT, git_sha TEXT); \
             INSERT INTO actions (name, description, owner_agent, steps_json, returns, kind) VALUES \
               ('board-keeper.card.list', 'list cards', 'board-keeper', '[{\"skill\":\"card-list\",\"input\":{},\"as\":\"cards\"}]', '$.results.cards', 'atomic'); \
             INSERT INTO cards VALUES ('A','doing',5),('B','backlog',NULL);",
        )
        .unwrap();
        c
    }

    #[test]
    fn runs_warmup_plus_iterations_and_records_six_metrics() {
        let c = db();
        let r = benchSkillLatency(&json!({ "run_id": "r1", "action": "board-keeper.card.list", "iterations": 2, "warmup": 1 }), &c).unwrap();
        assert_eq!(r["run_id"], json!("r1"));
        assert_eq!(r["action"], json!("board-keeper.card.list"));
        assert_eq!(r["iterations"], json!(2));
        assert_eq!(r["stats"]["count"], json!(2));
        assert_eq!(r["stats"]["avg"], json!(0.0));
        assert_eq!(r["stats"]["p99"], json!(0.0));

        // warmup(1) + iterations(2) = 3 skillRun calls, each 2 runtime_events rows = 6 rows
        let evt_count: i64 = c.query_row("SELECT COUNT(*) FROM runtime_events", [], |row| row.get(0)).unwrap();
        assert_eq!(evt_count, 6);

        let metrics = crate::db_json::query_rows(&c, "SELECT metric FROM brain.bench_runs ORDER BY id", &[]).unwrap();
        let arr = metrics.as_array().unwrap();
        assert_eq!(arr.len(), 6);
        assert_eq!(arr[0]["metric"], json!("avg_ms"));
        assert_eq!(arr[1]["metric"], json!("p50_ms"));
        assert_eq!(arr[5]["metric"], json!("max_ms"));
    }

    #[test]
    fn defaults_fill_in_run_id_action_iterations_warmup() {
        let c = db();
        let r = benchSkillLatency(&json!({ "iterations": 1, "warmup": 0 }), &c).unwrap();
        assert_eq!(r["run_id"], json!("bench-1767225600000"));
        assert_eq!(r["action"], json!("board-keeper.card.list"));
        assert_eq!(r["stats"]["count"], json!(1));
    }

    #[test]
    fn unknown_action_propagates_error() {
        let c = db();
        let err = benchSkillLatency(&json!({ "action": "nope.action", "iterations": 1, "warmup": 0 }), &c).unwrap_err();
        assert!(err.contains("not found"));
    }
}
