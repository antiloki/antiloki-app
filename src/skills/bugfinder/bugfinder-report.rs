//! bugfinder-report.rs — twin of skills/bugfinder/bugfinder-report.mjs (db READ lane,
//! async-over-sync). NAMES ARE CONTRACT: `bugfinderReport` (default). The Ultra Review screen's
//! data spine: runs, findings, category/severity/source breakdowns, the operator-classification
//! precision ledger (the ONLY promotion path from advisory to gating), and the schedules with
//! their computed due state. Read-only; counted, never vibed. Frozen-clock lane: JS `Date.now()`
//! is pinned to 2026-01-01T00:00:00.000Z (1767225600000 ms).
// QUALITY: 80/100 — five aggregate queries over the ledger + the precision ledger's dynamic-key
// accumulation (real/false-positive/known counted, ratio rounded to 2dp), delegating due-date
// math to the already-converted schedule-math twin.
use rusqlite::Connection;
use serde_json::{json, Map, Value};


// Number.isInteger(v): Some(int) if v is a JSON number with no fractional part; else None.
fn js_integer(v: Option<&Value>) -> Option<i64> {
    match v {
        Some(Value::Number(n)) => {
            let f = n.as_f64()?;
            if f.is_finite() && f.fract() == 0.0 { Some(f as i64) } else { None }
        }
        _ => None,
    }
}

// JS Math.round: nearest integer, ties toward +Infinity.
fn js_round(x: f64) -> f64 { if !x.is_finite() { return x; } let f = x.floor(); if x - f < 0.5 { f } else { f + 1.0 } }

// JS Number stringification: integral f64 -> integer JSON number, else float (gotcha #2).
fn num_val(x: f64) -> Value {
    if x.is_finite() && x.fract() == 0.0 && x.abs() < 9.007_199_254_740_992e15 { json!(x as i64) } else { json!(x) }
}

// days since 1970-01-01 for a proleptic-Gregorian y/m/d (m in 1..=12) — Howard Hinnant's algorithm
// (mirrors the private copy in bugfinder/_helpers/schedule-math.rs; duplicated here since it's not `pub` there).
fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719468;
    let era = (if z >= 0 { z } else { z - 146096 }) / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

// JS `new Date(ms).toISOString().slice(0, 19).replace('T', ' ')` — SQL-format instant (no ms, no Z).
fn sql_from_ms(ms: i64) -> String {
    let day = ms.div_euclid(86_400_000);
    let mut rem = ms.rem_euclid(86_400_000);
    let (y, m, d) = civil_from_days(day);
    let h = rem / 3_600_000; rem %= 3_600_000;
    let mi = rem / 60_000; rem %= 60_000;
    let s = rem / 1000;
    format!("{:04}-{:02}-{:02} {:02}:{:02}:{:02}", y, m, d, h, mi, s)
}

// JSON.parse(s ?? '{}') with a try/catch default (gotcha #6): DB NULL or malformed JSON -> default.
fn parse_json_or(s: Option<&str>, default: Value) -> Value {
    s.and_then(|t| serde_json::from_str(t).ok()).unwrap_or(default)
}

// Object.fromEntries(rows.map(r => [r.k, r.n])) — rows come from a `... AS k, COUNT(*) AS n` query.
fn to_map(rows: &Value) -> Value {
    let mut m = Map::new();
    for r in rows.as_array().cloned().unwrap_or_default() {
        let k = r.get("k").and_then(|v| v.as_str()).unwrap_or("").to_string();
        m.insert(k, r.get("n").cloned().unwrap_or(Value::Null));
    }
    Value::Object(m)
}

pub fn bugfinderReport(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS `const db = context?.db; if (!db) throw err.required(...)` — the db lane always injects
    // a live connection, so that guard is unreachable here (Rust's signature takes `db: &Connection`,
    // never absent).
    crate::bugfinder__helpers_schema::ensureBugfinderSchema(db);

    let days: i64 = match js_integer(input.get("days")) {
        Some(d) if d > 0 => d.min(365),
        _ => 30,
    };
    let since = sql_from_ms(crate::clock::now_ms() - days * 24 * 3_600_000);

    // ── runs (unfiltered by days — LIMIT 40 only) ───────────────────────────
    let runs_raw = crate::db_json::query_rows(
        db,
        "SELECT id, created_at, source, session_id, contract_slug, status, scope_json, totals_json, cost_usd, error FROM judgments.bugfinder_runs ORDER BY id DESC LIMIT 40",
        &[],
    )?;
    const RUN_KEYS: [&str; 8] = ["id", "created_at", "source", "session_id", "contract_slug", "status", "cost_usd", "error"];
    let runs: Vec<Value> = runs_raw.as_array().cloned().unwrap_or_default().iter().map(|r| {
        let scope = parse_json_or(r.get("scope_json").and_then(|v| v.as_str()), json!({}));
        let totals = parse_json_or(r.get("totals_json").and_then(|v| v.as_str()), json!({}));
        let files = scope.get("files").and_then(|f| f.as_array()).map(|a| a.len()).unwrap_or(0);
        let mut o = Map::new();
        for k in RUN_KEYS { o.insert(k.into(), r.get(k).cloned().unwrap_or(Value::Null)); }
        o.insert("files".into(), json!(files as i64));
        o.insert("scope".into(), scope);
        o.insert("totals".into(), totals);
        Value::Object(o)
    }).collect();

    // ── since-windowed aggregates ────────────────────────────────────────────
    let agg_raw = crate::db_json::query_rows(
        db,
        "SELECT COUNT(*) AS runs, COALESCE(SUM(cost_usd), 0) AS cost FROM judgments.bugfinder_runs WHERE created_at >= ?",
        &[json!(since.clone())],
    )?;
    let agg_row = agg_raw.as_array().and_then(|a| a.first().cloned());
    let agg_runs = agg_row.as_ref().and_then(|a| a.get("runs").cloned()).unwrap_or(json!(0));
    let agg_cost = agg_row.as_ref().and_then(|a| a.get("cost").cloned()).unwrap_or(json!(0));

    let kept_dropped_raw = crate::db_json::query_rows(
        db,
        "SELECT f.dropped AS dropped, COUNT(*) AS n FROM judgments.bugfinder_findings f JOIN judgments.bugfinder_runs r ON r.id = f.run_id WHERE r.created_at >= ? GROUP BY f.dropped",
        &[json!(since.clone())],
    )?;
    let kept_dropped: Vec<Value> = kept_dropped_raw.as_array().cloned().unwrap_or_default();

    let by_category_raw = crate::db_json::query_rows(
        db,
        "SELECT f.category AS k, COUNT(*) AS n FROM judgments.bugfinder_findings f JOIN judgments.bugfinder_runs r ON r.id = f.run_id WHERE r.created_at >= ? AND f.dropped = 0 GROUP BY f.category ORDER BY n DESC",
        &[json!(since.clone())],
    )?;
    let by_severity_raw = crate::db_json::query_rows(
        db,
        "SELECT f.severity AS k, COUNT(*) AS n FROM judgments.bugfinder_findings f JOIN judgments.bugfinder_runs r ON r.id = f.run_id WHERE r.created_at >= ? AND f.dropped = 0 GROUP BY f.severity",
        &[json!(since.clone())],
    )?;
    let by_source_raw = crate::db_json::query_rows(
        db,
        "SELECT source AS k, COUNT(*) AS n FROM judgments.bugfinder_runs WHERE created_at >= ? GROUP BY source",
        &[json!(since.clone())],
    )?;

    // ── the precision ledger: operator classifications per category, ALL time (no since filter,
    // no join with runs) — precision = real / (real + false-positive); null until both sides exist.
    let classified_raw = crate::db_json::query_rows(
        db,
        "SELECT category, operator_class, COUNT(*) AS n FROM judgments.bugfinder_findings WHERE dropped = 0 AND operator_class IS NOT NULL GROUP BY category, operator_class",
        &[],
    )?;
    let mut precision_map: Map<String, Value> = Map::new();
    for row in classified_raw.as_array().cloned().unwrap_or_default() {
        let category = row.get("category").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let operator_class = row.get("operator_class").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let n = row.get("n").cloned().unwrap_or(Value::Null);
        let entry = precision_map
            .entry(category)
            .or_insert_with(|| json!({ "real": 0, "false-positive": 0, "known": 0, "precision": null }));
        if let Value::Object(p) = entry { p.insert(operator_class, n); }
    }
    for (_, v) in precision_map.iter_mut() {
        if let Value::Object(p) = v {
            let real = p.get("real").and_then(|x| x.as_f64()).unwrap_or(0.0);
            let fp = p.get("false-positive").and_then(|x| x.as_f64()).unwrap_or(0.0);
            let judged = real + fp;
            let precision_val = if judged > 0.0 { num_val(js_round(real / judged * 100.0) / 100.0) } else { Value::Null };
            p.insert("precision".into(), precision_val);
        }
    }
    let precision = Value::Object(precision_map);

    let kept = kept_dropped.iter()
        .find(|x| x.get("dropped").and_then(|v| v.as_i64()) == Some(0))
        .and_then(|x| x.get("n").cloned())
        .unwrap_or(json!(0));
    let dropped_count = kept_dropped.iter()
        .find(|x| x.get("dropped").and_then(|v| v.as_i64()) == Some(1))
        .and_then(|x| x.get("n").cloned())
        .unwrap_or(json!(0));

    let mut summary = Map::new();
    summary.insert("days".into(), json!(days));
    summary.insert("runs".into(), agg_runs);
    summary.insert("cost_usd".into(), agg_cost);
    summary.insert("kept".into(), kept);
    summary.insert("dropped".into(), dropped_count);
    summary.insert("by_category".into(), to_map(&by_category_raw));
    summary.insert("by_severity".into(), to_map(&by_severity_raw));
    summary.insert("by_source".into(), to_map(&by_source_raw));
    summary.insert("precision".into(), precision);
    let summary_val = Value::Object(summary);

    // ── findings — only when run_id is a proper integer (key omitted otherwise) ─────────────────
    let findings: Option<Value> = match js_integer(input.get("run_id")) {
        Some(run_id) => Some(crate::db_json::query_rows(
            db,
            "SELECT id, wave, category, severity, file, line, detail, evidence, model, verified, dropped, drop_reason, operator_class, created_at FROM judgments.bugfinder_findings WHERE run_id = ? ORDER BY dropped ASC, wave ASC, severity DESC, id ASC",
            &[json!(run_id)],
        )?),
        None => None,
    };

    // ── schedules — due-date math delegated to the already-converted schedule-math twin ─────────
    let schedules_raw = crate::db_json::query_rows(db, "SELECT * FROM judgments.bugfinder_schedules ORDER BY id ASC", &[])?;
    const SCHED_KEYS: [&str; 8] = ["id", "name", "cadence", "hour_utc", "enabled", "last_run_at", "last_run_id", "created_at"];
    let schedules: Vec<Value> = schedules_raw.as_array().cloned().unwrap_or_default().iter().map(|s| {
        let scope = parse_json_or(s.get("scope_json").and_then(|v| v.as_str()), json!({}));
        let mut o = Map::new();
        for k in SCHED_KEYS { o.insert(k.into(), s.get(k).cloned().unwrap_or(Value::Null)); }
        o.insert("scope".into(), scope);
        o.insert("next_due_at".into(), crate::bugfinder__helpers_schedule_math::computeNextDue(s, &Value::Null));
        o.insert("overdue".into(), crate::bugfinder__helpers_schedule_math::isOverdue(s, &Value::Null));
        Value::Object(o)
    }).collect();

    let mut out = Map::new();
    out.insert("summary".into(), summary_val);
    out.insert("runs".into(), Value::Array(runs));
    if let Some(f) = findings { out.insert("findings".into(), f); }
    out.insert("schedules".into(), Value::Array(schedules));
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conn() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS judgments").unwrap();
        let _ = crate::bugfinder__helpers_schema::ensureBugfinderSchema(&c);
        c
    }

    fn insert_run(c: &Connection, id: i64, created_at: &str, source: &str, scope_json: &str, totals_json: &str, cost_usd: f64) {
        c.execute(
            "INSERT INTO judgments.bugfinder_runs (id, created_at, source, session_id, contract_slug, status, scope_json, totals_json, cost_usd, error) VALUES (?,?,?,NULL,NULL,'done',?,?,?,NULL)",
            rusqlite::params![id, created_at, source, scope_json, totals_json, cost_usd],
        ).unwrap();
    }

    fn insert_finding(c: &Connection, id: i64, run_id: i64, wave: i64, category: &str, severity: &str, dropped: i64, operator_class: Option<&str>, created_at: &str) {
        c.execute(
            "INSERT INTO judgments.bugfinder_findings (id, run_id, wave, category, severity, detail, dropped, operator_class, created_at) VALUES (?,?,?,?,?,'d',?,?,?)",
            rusqlite::params![id, run_id, wave, category, severity, dropped, operator_class, created_at],
        ).unwrap();
    }

    #[test]
    fn runs_unfiltered_by_days_files_count_and_malformed_json_fallback() {
        let c = conn();
        insert_run(&c, 1, "2025-12-30 00:00:00", "manual", "{\"files\":[\"a.mjs\",\"b.mjs\"]}", "{\"kept\":1}", 1.5);
        insert_run(&c, 2, "2025-09-01 00:00:00", "manual", "not-json{", "also-bad{", 0.0); // well outside the 30-day default window
        let out = bugfinderReport(&json!({}), &c).unwrap();
        assert_eq!(out["runs"].as_array().unwrap().len(), 2); // LIMIT 40, no date filter
        let r1 = out["runs"].as_array().unwrap().iter().find(|r| r["id"] == json!(1)).unwrap();
        assert_eq!(r1["files"], json!(2));
        assert_eq!(r1["scope"], json!({ "files": ["a.mjs", "b.mjs"] }));
        assert_eq!(r1["totals"], json!({ "kept": 1 }));
        assert!(r1.get("scope_json").is_none());
        assert!(r1.get("totals_json").is_none());
        let r2 = out["runs"].as_array().unwrap().iter().find(|r| r["id"] == json!(2)).unwrap();
        assert_eq!(r2["scope"], json!({}));
        assert_eq!(r2["totals"], json!({}));
        assert_eq!(r2["files"], json!(0));
        assert_eq!(out["summary"]["runs"], json!(1)); // only run 1 is inside the default 30-day window
        assert_eq!(out["summary"]["cost_usd"], json!(1.5));
    }

    #[test]
    fn summary_defaults_precision_ledger_and_findings_ordering() {
        let c = conn();
        insert_run(&c, 1, "2026-01-01 00:00:00", "vibe", "{}", "{}", 0.0);
        insert_finding(&c, 1, 1, 1, "lint", "high", 0, None, "2026-01-01 00:00:00");
        insert_finding(&c, 2, 1, 1, "security", "low", 0, None, "2026-01-01 00:00:00");
        let out = bugfinderReport(&json!({}), &c).unwrap();
        assert_eq!(out["summary"]["kept"], json!(2));
        assert_eq!(out["summary"]["dropped"], json!(0)); // no dropped=1 bucket -> ?? 0
        assert_eq!(out["summary"]["by_category"], json!({ "lint": 1, "security": 1 }));
        assert_eq!(out["summary"]["by_severity"], json!({ "high": 1, "low": 1 }));
        assert_eq!(out["summary"]["by_source"], json!({ "vibe": 1 }));

        // precision ledger: typecheck real=1,false-positive=2 -> round(1/3*100)/100 = 0.33; checking
        // only 'known' -> judged=0 -> null.
        insert_finding(&c, 3, 1, 1, "typecheck", "medium", 0, Some("real"), "2026-01-01 00:00:00");
        insert_finding(&c, 4, 1, 1, "typecheck", "medium", 0, Some("false-positive"), "2026-01-01 00:00:00");
        insert_finding(&c, 5, 1, 1, "typecheck", "medium", 0, Some("false-positive"), "2026-01-01 00:00:00");
        insert_finding(&c, 6, 1, 1, "checking", "medium", 0, Some("known"), "2026-01-01 00:00:00");
        let out = bugfinderReport(&json!({}), &c).unwrap();
        assert_eq!(out["summary"]["precision"]["typecheck"], json!({ "real": 1, "false-positive": 2, "known": 0, "precision": 0.33 }));
        assert_eq!(out["summary"]["precision"]["checking"], json!({ "real": 0, "false-positive": 0, "known": 1, "precision": Value::Null }));

        // findings key: present + ordered when run_id is a proper integer; omitted otherwise.
        insert_finding(&c, 7, 1, 2, "lint", "high", 1, None, "2026-01-01 00:00:00"); // dropped
        let with_run = bugfinderReport(&json!({ "run_id": 1 }), &c).unwrap();
        let ids: Vec<i64> = with_run["findings"].as_array().unwrap().iter().map(|f| f["id"].as_i64().unwrap()).collect();
        // dropped=0 rows first (severity DESC lexical: "medium" > "low" > "high"), then dropped=1.
        assert_eq!(ids, vec![3, 4, 5, 6, 2, 1, 7]);
        let no_run = bugfinderReport(&json!({}), &c).unwrap();
        assert!(no_run.get("findings").is_none());
        let non_integer_run = bugfinderReport(&json!({ "run_id": 1.5 }), &c).unwrap();
        assert!(non_integer_run.get("findings").is_none());
        let not_found = bugfinderReport(&json!({ "run_id": 999 }), &c).unwrap();
        assert_eq!(not_found["findings"], json!([])); // valid integer, no rows -> present but empty
    }

    #[test]
    fn days_default_clamp_and_fallback() {
        let c = conn();
        insert_run(&c, 1, "2026-01-01 00:00:00", "manual", "{}", "{}", 0.0);
        assert_eq!(bugfinderReport(&json!({}), &c).unwrap()["summary"]["days"], json!(30));
        assert_eq!(bugfinderReport(&json!({ "days": 10 }), &c).unwrap()["summary"]["days"], json!(10));
        assert_eq!(bugfinderReport(&json!({ "days": 1000 }), &c).unwrap()["summary"]["days"], json!(365));
        assert_eq!(bugfinderReport(&json!({ "days": 0 }), &c).unwrap()["summary"]["days"], json!(30));
        assert_eq!(bugfinderReport(&json!({ "days": -5 }), &c).unwrap()["summary"]["days"], json!(30));
        assert_eq!(bugfinderReport(&json!({ "days": 2.5 }), &c).unwrap()["summary"]["days"], json!(30));
    }

    #[test]
    fn schedules_scope_parse_and_due_state() {
        let c = conn();
        c.execute(
            "INSERT INTO judgments.bugfinder_schedules (id, name, scope_json, cadence, hour_utc, enabled, last_run_at, last_run_id, created_at) VALUES (1,'Daily Sweep','{\"folder\":\"src\"}','daily',6,1,'2025-12-01 06:00:00',NULL,'2025-11-01 00:00:00')",
            [],
        ).unwrap();
        c.execute(
            "INSERT INTO judgments.bugfinder_schedules (id, name, scope_json, cadence, hour_utc, enabled, last_run_at, last_run_id, created_at) VALUES (2,'Manual Only','bad-json','manual',6,1,NULL,NULL,'2025-12-27 00:00:00')",
            [],
        ).unwrap();
        let out = bugfinderReport(&json!({}), &c).unwrap();
        assert_eq!(out["schedules"].as_array().unwrap().len(), 2);
        let daily = &out["schedules"][0];
        assert_eq!(daily["scope"], json!({ "folder": "src" }));
        assert!(daily.get("scope_json").is_none());
        assert!(daily["next_due_at"].is_string()); // last run well before the frozen instant -> overdue
        assert_eq!(daily["overdue"], json!(true));
        let manual = &out["schedules"][1];
        assert_eq!(manual["scope"], json!({})); // malformed scope_json -> catch -> {}
        assert_eq!(manual["next_due_at"], Value::Null);
        assert_eq!(manual["overdue"], json!(false));
    }
}
