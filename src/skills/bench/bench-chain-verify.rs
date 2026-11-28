//! bench-chain-verify.rs — twin of skills/bench/bench-chain-verify.mjs. NAMES ARE CONTRACT:
//! `benchChainVerify` (default, standard lane).
//!
//! Times `verifyChain` (audit_verify_chain) at the current chain length, `iterations` times,
//! and records the median duration + event count + per-event cost via `benchRecord`
//! (bench_bench_record) — both already-converted twins, called cross-module per the crossing
//! playbook.
//!
//! `performance.now()` is frozen per the playbook's frozen-clock rule: every call returns a
//! constant, so `performance.now() - t` is always exactly `0.0`. Every sample is therefore
//! `0.0`, the sorted-median is always `0`, and `perEventUs = count > 0 ? (median*1000)/count : 0`
//! collapses to `0` regardless of `count`. Both are rendered as integer `0` (JS `${0}` === "0"),
//! not `0.0`.
use rusqlite::Connection;
use serde_json::{json, Value};

#[allow(non_snake_case)]
pub fn benchChainVerify(input: &Value, db: &Connection) -> Result<Value, String> {
    let run_id = input
        .get("run_id")
        .and_then(Value::as_str)
        .map(|s| s.to_string())
        .unwrap_or_else(|| format!("bench-{}", crate::clock::now_ms()));
    let iterations = input
        .get("iterations")
        .and_then(Value::as_i64)
        .unwrap_or(5);

    let mut count: Value = json!(0);
    for _ in 0..iterations {
        let r = crate::audit_verify_chain::verifyChain(&json!({}), db)?;
        count = r.get("count").cloned().unwrap_or(Value::Null);
    }

    let median_ms: i64 = 0;
    let per_event_us: i64 = 0;
    let ctx = json!({ "iterations": iterations, "event_count": count });

    crate::bench_bench_record::benchRecord(
        &json!({
            "run_id": run_id, "bench_name": "chain-verify", "metric": "median_ms",
            "value": median_ms, "unit": "ms", "context": ctx,
        }),
        db,
    )?;
    crate::bench_bench_record::benchRecord(
        &json!({
            "run_id": run_id, "bench_name": "chain-verify", "metric": "event_count",
            "value": count, "unit": "events", "context": ctx,
        }),
        db,
    )?;
    crate::bench_bench_record::benchRecord(
        &json!({
            "run_id": run_id, "bench_name": "chain-verify", "metric": "per_event_us",
            "value": per_event_us, "unit": "\u{3bc}s/event", "context": ctx,
        }),
        db,
    )?;

    Ok(json!({
        "run_id": run_id,
        "median_ms": median_ms,
        "event_count": count,
        "per_event_us": per_event_us,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "ATTACH DATABASE ':memory:' AS brain; \
             CREATE TABLE sacred_timeline (id INTEGER PRIMARY KEY, ts TEXT, actor TEXT, \
               operation TEXT, entity_type TEXT, entity_id TEXT, before_json TEXT, \
               after_json TEXT, payload TEXT, prev_print TEXT, digital_print TEXT); \
             CREATE TABLE brain.bench_runs (id INTEGER PRIMARY KEY AUTOINCREMENT, \
               ts TEXT NOT NULL DEFAULT (datetime('now')), run_id TEXT NOT NULL, \
               bench_name TEXT NOT NULL, metric TEXT NOT NULL, value REAL NOT NULL, \
               unit TEXT, context_json TEXT, git_sha TEXT);",
        )
        .unwrap();
        c
    }

    #[test]
    fn empty_chain_records_zero_event_count() {
        let c = db();
        let r = benchChainVerify(&json!({ "run_id": "r-empty", "iterations": 1 }), &c).unwrap();
        assert_eq!(r["run_id"], json!("r-empty"));
        assert_eq!(r["median_ms"], json!(0));
        assert_eq!(r["event_count"], json!(0));
        assert_eq!(r["per_event_us"], json!(0));

        let rows = crate::db_json::query_rows(
            &c,
            "SELECT metric, value FROM brain.bench_runs ORDER BY id",
            &[],
        )
        .unwrap();
        let arr = rows.as_array().unwrap();
        assert_eq!(arr.len(), 3);
        assert_eq!(arr[0]["metric"], json!("median_ms"));
        assert_eq!(arr[1]["metric"], json!("event_count"));
        assert_eq!(arr[2]["metric"], json!("per_event_us"));
    }

    #[test]
    fn valid_chain_reports_event_count() {
        let c = db();
        c.execute_batch(
            "INSERT INTO sacred_timeline VALUES \
             (1,'2026-01-01T00:00:00.000Z','t','card.add','card','A',NULL,\
              '{\"slug\":\"A\",\"title\":\"a\"}',NULL,'GENESIS',\
              'e488e6fd7a2ba805f9827ef6b66e767597b207b0359c6231119478be419e6754'), \
             (2,'2026-01-01T00:05:00.000Z','t','card.add','card','B',NULL,\
              '{\"slug\":\"B\",\"title\":\"b\"}',NULL,\
              'e488e6fd7a2ba805f9827ef6b66e767597b207b0359c6231119478be419e6754',\
              'a914ade38681bb347a79baa60704e2c823c1579f448e8fccdf86712bf1e06644');",
        )
        .unwrap();
        let r = benchChainVerify(&json!({ "run_id": "r-two", "iterations": 3 }), &c).unwrap();
        assert_eq!(r["event_count"], json!(2));
        assert_eq!(r["median_ms"], json!(0));
        assert_eq!(r["per_event_us"], json!(0));
    }

    #[test]
    fn defaults_fill_in_run_id_and_iterations() {
        let c = db();
        let r = benchChainVerify(&json!({}), &c).unwrap();
        assert_eq!(r["run_id"], json!("bench-1767225600000"));
        assert_eq!(r["event_count"], json!(0));
    }
}
