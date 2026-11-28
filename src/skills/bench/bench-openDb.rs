//! bench-openDb.rs — twin of skills/bench/bench-openDb.mjs. NAMES ARE CONTRACT:
//! `benchOpenDb` (default, standard lane).
//!
//! Opens `iterations` fresh `:memory:` substrate DBs via `openDb` (engine_open_db, already-
//! converted twin — each call ATTACHes all five substrate DBs + runs schema + seed, then the
//! connection is dropped), timing each open, then records 5 stats metrics (avg/p50/p95/min/
//! max, all "_ms") via `benchRecord` (bench_bench_record, already-converted twin) using stats
//! from `benchStats` (bench_bench_stats, already-converted twin).
//!
//! `performance.now()` is frozen per the playbook's frozen-clock rule: every call returns the
//! SAME constant, so `performance.now() - t` is always exactly `0.0` for every sample. The
//! collected stats therefore collapse deterministically to all-zero (count = iterations, the
//! rest 0) in BOTH lanes — same documented exception as bench-skill-latency.rs: the measured
//! duration is only ever logged/returned, never branched on. The fresh `:memory:` DBs opened
//! each iteration are independent of the `db: &Connection` argument (JS: `context.db`) — they
//! are opened, timed, and closed (`fresh.close()`, ignoring errors) without ever touching it.
use rusqlite::Connection;
use serde_json::{json, Value};

#[allow(non_snake_case)]
pub fn benchOpenDb(input: &Value, db: &Connection) -> Result<Value, String> {
    let run_id = input
        .get("run_id")
        .and_then(Value::as_str)
        .map(|s| s.to_string())
        .unwrap_or_else(|| format!("bench-{}", crate::clock::now_ms()));
    let iterations = input.get("iterations").and_then(Value::as_i64).unwrap_or(10);

    let mut samples: Vec<Value> = Vec::new();
    for _ in 0..iterations {
        // JS: `const fresh = openDb(':memory:'); ... try { fresh.close() } catch {}` — the
        // connection is dropped at end of scope here, mirroring the close.
        let _fresh: Connection = crate::engine_open_db::openDb(&json!(":memory:"), &json!({}))?;
        samples.push(json!(0.0)); // frozen performance.now() delta
    }

    let stats = crate::bench_bench_stats::benchStats(&json!({ "samples": samples }), &Value::Null);
    let ctx = json!({ "iterations": iterations, "mode": ":memory:" });

    for (metric, unit) in [
        ("avg", "ms"),
        ("p50", "ms"),
        ("p95", "ms"),
        ("min", "ms"),
        ("max", "ms"),
    ] {
        let value = stats.get(metric).cloned().unwrap_or(Value::Null);
        crate::bench_bench_record::benchRecord(
            &json!({
                "run_id": run_id,
                "bench_name": "openDb",
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
            "ATTACH DATABASE ':memory:' AS brain; \
             CREATE TABLE brain.bench_runs (id INTEGER PRIMARY KEY AUTOINCREMENT, \
               ts TEXT NOT NULL DEFAULT (datetime('now')), run_id TEXT NOT NULL, \
               bench_name TEXT NOT NULL, metric TEXT NOT NULL, value REAL NOT NULL, \
               unit TEXT, context_json TEXT, git_sha TEXT);",
        )
        .unwrap();
        c
    }

    #[test]
    fn records_five_bench_runs_rows_for_the_requested_metrics() {
        let c = db();
        let r = benchOpenDb(&json!({ "run_id": "r1", "iterations": 2 }), &c).unwrap();
        assert_eq!(r["run_id"], json!("r1"));
        assert_eq!(r["iterations"], json!(2));
        assert_eq!(r["stats"]["count"], json!(2));
        assert_eq!(r["stats"]["avg"], json!(0.0));

        let rows = crate::db_json::query_rows(
            &c,
            "SELECT metric, unit, run_id, bench_name FROM brain.bench_runs ORDER BY id",
            &[],
        )
        .unwrap();
        let arr = rows.as_array().unwrap();
        assert_eq!(arr.len(), 5);
        let expect = ["avg_ms", "p50_ms", "p95_ms", "min_ms", "max_ms"];
        for (row, metric) in arr.iter().zip(expect.iter()) {
            assert_eq!(row["metric"], json!(*metric));
            assert_eq!(row["unit"], json!("ms"));
            assert_eq!(row["run_id"], json!("r1"));
            assert_eq!(row["bench_name"], json!("openDb"));
        }
    }

    #[test]
    fn defaults_iterations_to_ten_and_derives_run_id() {
        let c = db();
        let r = benchOpenDb(&json!({}), &c).unwrap();
        assert_eq!(r["run_id"], json!("bench-1767225600000"));
        assert_eq!(r["iterations"], json!(10));
        assert_eq!(r["stats"]["count"], json!(10));
    }
}
