//! benchmark-status.rs — twin of skills/system/benchmark-status.mjs (db READ lane, cross-db
//! brain.ai_calls). NAMES ARE CONTRACT: `benchmarkStatus` (default). Reads the primary benchmark
//! card (body.kind="benchmark", most recently updated, not archived) + its `<slug>-RUN-<n>` clone
//! history: clean/failed/in-progress counts, a cost roll-up via brain.ai_calls EXISTS-joined
//! through sacred_timeline, and a crude wall-time average over the done runs' created_at->updated_at
//! span.
use rusqlite::Connection;
use serde_json::{json, Map, Value};

// days since 1970-01-01 for a proleptic-Gregorian y/m/d (m in 1..=12) — Howard Hinnant's algorithm.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = (if y >= 0 { y } else { y - 399 }) / 400;
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

// Parse a 'YYYY-MM-DD HH:MM:SS' sqlite datetime cell as a UTC epoch-ms instant — mirrors JS
// `new Date(s.replace(' ','T') + 'Z').getTime()`. Malformed input -> None (JS would yield NaN,
// which the caller's `Math.max(0, end-start)` propagates as NaN; fixtures only feed well-formed
// timestamps, matching the SUCCESS-path-only fixture convention).
fn parse_sql_ts_ms(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    if b.len() < 19 || b[4] != b'-' || b[7] != b'-' || (b[10] != b'T' && b[10] != b' ') || b[13] != b':' || b[16] != b':' {
        return None;
    }
    let num = |lo: usize, hi: usize| s.get(lo..hi)?.parse::<i64>().ok();
    let y = num(0, 4)?;
    let mo = num(5, 7)?;
    let d = num(8, 10)?;
    let hh = num(11, 13)?;
    let mm = num(14, 16)?;
    let ss = num(17, 19)?;
    Some((((days_from_civil(y, mo, d) * 24 + hh) * 60 + mm) * 60 + ss) * 1000)
}

pub fn benchmarkStatus(_input: &Value, db: &Connection) -> Result<Value, String> {
    // JS: if (!db) throw err.contextMissing('benchmark-status','db') — the db lane always injects one.
    let benchmarks = crate::db_json::query_rows(
        db,
        "SELECT slug, title, status, body_json, updated_at FROM cards WHERE body_json LIKE '%\"kind\":\"benchmark\"%' AND status NOT IN ('archived') ORDER BY updated_at DESC LIMIT 5",
        &[],
    )
    .ok()
    .and_then(|v| v.as_array().cloned())
    .unwrap_or_default();

    if benchmarks.is_empty() {
        return Ok(json!({
            "defined": false,
            "message": "No benchmark card defined. File a card with body.kind=\"benchmark\" to set one."
        }));
    }

    let primary = &benchmarks[0];
    let primary_slug = primary.get("slug").cloned().unwrap_or(Value::Null);

    let runs = crate::db_json::query_rows(
        db,
        "SELECT slug, status, updated_at, created_at, body_json FROM cards WHERE slug LIKE ? || '-RUN-%' ORDER BY id DESC LIMIT 20",
        &[primary_slug],
    )
    .ok()
    .and_then(|v| v.as_array().cloned())
    .unwrap_or_default();

    let status_is = |r: &Value, s: &str| r.get("status").and_then(Value::as_str) == Some(s);
    let clean = runs.iter().filter(|r| status_is(r, "done")).count();
    let failed = runs.iter().filter(|r| status_is(r, "archived") || status_is(r, "blocked")).count();
    let in_progress = runs.len() as i64 - clean as i64 - failed as i64;

    let mut total_cost = 0f64;
    let mut avg_cost = 0f64;
    let mut avg_seconds = 0f64;

    if !runs.is_empty() {
        let run_slugs: Vec<Value> = runs.iter().map(|r| r.get("slug").cloned().unwrap_or(Value::Null)).collect();
        let placeholders = run_slugs.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let sql = format!(
            "SELECT SUM(a.cost_usd_actual) AS total FROM brain.ai_calls a WHERE EXISTS ( SELECT 1 FROM sacred_timeline st WHERE st.id = a.parent_event_id AND st.entity_type='card' AND st.entity_id IN ({placeholders}) )"
        );
        if let Ok(cost_rows) = crate::db_json::query_rows(db, &sql, &run_slugs) {
            if let Some(row) = cost_rows.as_array().and_then(|a| a.first()) {
                total_cost = row.get("total").and_then(Value::as_f64).unwrap_or(0.0);
            }
            avg_cost = if clean > 0 { total_cost / clean as f64 } else { 0.0 };
        }

        let done_runs: Vec<&Value> = runs.iter().filter(|r| status_is(r, "done")).collect();
        if !done_runs.is_empty() {
            let mut total_seconds = 0f64;
            for r in &done_runs {
                let created = r.get("created_at").and_then(Value::as_str).unwrap_or("");
                let updated = r.get("updated_at").and_then(Value::as_str).unwrap_or("");
                let start = parse_sql_ts_ms(created).unwrap_or(0) as f64;
                let end = parse_sql_ts_ms(updated).unwrap_or(0) as f64;
                total_seconds += ((end - start) / 1000.0).max(0.0);
            }
            avg_seconds = total_seconds / done_runs.len() as f64;
        }
    }

    let mut benchmark = Map::new();
    benchmark.insert("slug".into(), primary.get("slug").cloned().unwrap_or(Value::Null));
    benchmark.insert("title".into(), primary.get("title").cloned().unwrap_or(Value::Null));
    benchmark.insert("status".into(), primary.get("status").cloned().unwrap_or(Value::Null));

    let mut out = Map::new();
    out.insert("defined".into(), json!(true));
    out.insert("benchmark".into(), Value::Object(benchmark));
    out.insert("runs_total".into(), json!(runs.len() as i64));
    out.insert("runs_clean".into(), json!(clean as i64));
    out.insert("runs_failed".into(), json!(failed as i64));
    out.insert("runs_in_progress".into(), json!(in_progress));
    out.insert("clean_ratio".into(), json!(if !runs.is_empty() { clean as f64 / runs.len() as f64 } else { 0.0 }));
    out.insert("avg_cost_usd".into(), json!(avg_cost));
    out.insert("avg_wall_seconds".into(), json!(avg_seconds));
    out.insert("total_cost_usd".into(), json!(total_cost));

    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "ATTACH DATABASE ':memory:' AS brain; \
             CREATE TABLE cards (id INTEGER PRIMARY KEY AUTOINCREMENT, slug TEXT, title TEXT, status TEXT, body_json TEXT, created_at TEXT, updated_at TEXT); \
             CREATE TABLE sacred_timeline (id INTEGER PRIMARY KEY, entity_type TEXT, entity_id TEXT); \
             CREATE TABLE brain.ai_calls (id INTEGER PRIMARY KEY, parent_event_id INTEGER, cost_usd_actual REAL);",
        )
        .unwrap();
        c
    }
    fn card(c: &Connection, slug: &str, status: &str, body_json: &str, created_at: &str, updated_at: &str, title: Option<&str>) {
        c.execute(
            "INSERT INTO cards (slug, title, status, body_json, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![slug, title, status, body_json, created_at, updated_at],
        )
        .unwrap();
    }
    fn timeline_row(c: &Connection, id: i64, entity_type: &str, entity_id: &str) {
        c.execute("INSERT INTO sacred_timeline (id, entity_type, entity_id) VALUES (?1, ?2, ?3)", rusqlite::params![id, entity_type, entity_id]).unwrap();
    }
    fn ai_call(c: &Connection, id: i64, parent_event_id: i64, cost_usd_actual: f64) {
        c.execute("INSERT INTO brain.ai_calls (id, parent_event_id, cost_usd_actual) VALUES (?1, ?2, ?3)", rusqlite::params![id, parent_event_id, cost_usd_actual]).unwrap();
    }

    #[test]
    fn no_benchmark_card_defined() {
        let c = db();
        card(&c, "OTHER-CARD", "doing", "{\"kind\":\"feature\"}", "2025-12-01 00:00:00", "2025-12-01 00:00:00", Some("Other"));
        let r = benchmarkStatus(&json!({}), &c).unwrap();
        assert_eq!(r, json!({ "defined": false, "message": "No benchmark card defined. File a card with body.kind=\"benchmark\" to set one." }));
    }

    #[test]
    fn benchmark_defined_zero_runs() {
        let c = db();
        card(&c, "BENCH-A", "doing", "{\"kind\":\"benchmark\"}", "2025-12-01 00:00:00", "2025-12-15 00:00:00", Some("Bench A"));
        let r = benchmarkStatus(&json!({}), &c).unwrap();
        assert_eq!(r, json!({
            "defined": true,
            "benchmark": { "slug": "BENCH-A", "title": "Bench A", "status": "doing" },
            "runs_total": 0, "runs_clean": 0, "runs_failed": 0, "runs_in_progress": 0,
            "clean_ratio": 0.0, "avg_cost_usd": 0.0, "avg_wall_seconds": 0.0, "total_cost_usd": 0.0
        }));
    }

    #[test]
    fn mixed_statuses_cost_rollup_and_wall_time() {
        let c = db();
        card(&c, "BENCH-A", "doing", "{\"kind\":\"benchmark\"}", "2025-12-01 00:00:00", "2025-12-25 00:00:00", Some("Bench A"));
        card(&c, "BENCH-A-RUN-1", "done", "{}", "2025-12-20 10:00:00", "2025-12-20 10:05:00", None);
        card(&c, "BENCH-A-RUN-2", "done", "{}", "2025-12-21 10:00:00", "2025-12-21 10:10:00", None);
        card(&c, "BENCH-A-RUN-3", "archived", "{}", "2025-12-22 10:00:00", "2025-12-22 10:02:00", None);
        card(&c, "BENCH-A-RUN-4", "blocked", "{}", "2025-12-23 10:00:00", "2025-12-23 10:01:00", None);
        card(&c, "BENCH-A-RUN-5", "doing", "{}", "2025-12-24 10:00:00", "2025-12-24 10:00:30", None);
        timeline_row(&c, 101, "card", "BENCH-A-RUN-1");
        timeline_row(&c, 102, "card", "BENCH-A-RUN-2");
        timeline_row(&c, 103, "contract", "BENCH-A-RUN-3");
        ai_call(&c, 1, 101, 2.5);
        ai_call(&c, 2, 102, 1.5);
        ai_call(&c, 3, 103, 5.0);
        ai_call(&c, 4, 999, 100.0);

        let r = benchmarkStatus(&json!({}), &c).unwrap();
        assert_eq!(r, json!({
            "defined": true,
            "benchmark": { "slug": "BENCH-A", "title": "Bench A", "status": "doing" },
            "runs_total": 5, "runs_clean": 2, "runs_failed": 2, "runs_in_progress": 1,
            "clean_ratio": 0.4, "avg_cost_usd": 2.0, "avg_wall_seconds": 450.0, "total_cost_usd": 4.0
        }));
    }

    #[test]
    fn archived_excluded_most_recent_wins_primary() {
        let c = db();
        card(&c, "ARCHIVED-BENCH", "archived", "{\"kind\":\"benchmark\"}", "2025-12-01 00:00:00", "2026-01-01 00:00:00", Some("Archived Bench"));
        card(&c, "BENCH-OLD", "done", "{\"kind\":\"benchmark\"}", "2025-10-01 00:00:00", "2025-11-01 00:00:00", Some("Bench Old"));
        card(&c, "BENCH-NEW", "doing", "{\"kind\":\"benchmark\"}", "2025-11-15 00:00:00", "2025-12-01 00:00:00", Some("Bench New"));
        card(&c, "NON-BENCH", "doing", "{\"kind\":\"feature\"}", "2025-12-01 00:00:00", "2025-12-31 00:00:00", Some("Non Bench"));
        let r = benchmarkStatus(&json!({}), &c).unwrap();
        assert_eq!(r["benchmark"], json!({ "slug": "BENCH-NEW", "title": "Bench New", "status": "doing" }));
        assert_eq!(r["runs_total"], json!(0));
    }

    #[test]
    fn zero_clean_null_cost_sum_stays_zero() {
        let c = db();
        card(&c, "BENCH-B", "doing", "{\"kind\":\"benchmark\"}", "2025-12-01 00:00:00", "2025-12-10 00:00:00", Some("Bench B"));
        card(&c, "BENCH-B-RUN-1", "doing", "{}", "2025-12-09 00:00:00", "2025-12-09 00:05:00", None);
        let r = benchmarkStatus(&json!({}), &c).unwrap();
        assert_eq!(r, json!({
            "defined": true,
            "benchmark": { "slug": "BENCH-B", "title": "Bench B", "status": "doing" },
            "runs_total": 1, "runs_clean": 0, "runs_failed": 0, "runs_in_progress": 1,
            "clean_ratio": 0.0, "avg_cost_usd": 0.0, "avg_wall_seconds": 0.0, "total_cost_usd": 0.0
        }));
    }
}
