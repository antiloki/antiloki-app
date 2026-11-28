//! bench-report.rs — twin of skills/bench/bench-report.mjs (db READ lane, cross-db brain.bench_runs).
//! NAMES ARE CONTRACT: `benchReport` (default). Markdown report: latest run (or given run_id)
//! grouped by bench_name, with an optional Δ-vs-previous-run column when compare=true.
use rusqlite::Connection;
use serde_json::{json, Value};
use std::collections::HashMap;

fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}
// JS `${v}` interpolation for the values that appear (string/number/bool/null/absent).
fn tmpl(v: Option<&Value>) -> String {
    match v {
        None => "undefined".into(),
        Some(Value::Null) => "null".into(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(),
        Some(o) => o.to_string(),
    }
}
// JS `fmt(v)`: v==null → em dash; else toFixed at a magnitude-dependent precision.
fn fmt(v: Option<f64>) -> String {
    match v {
        None => "\u{2014}".to_string(),
        Some(x) => {
            let a = x.abs();
            if a >= 1000.0 { format!("{:.0}", x) }
            else if a >= 10.0 { format!("{:.1}", x) }
            else if a >= 1.0 { format!("{:.2}", x) }
            else { format!("{:.3}", x) }
        }
    }
}

pub fn benchReport(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS throws err.contextMissing('bench-report','db') when no db — the db lane always injects one.
    let mut target_run_id = input.get("run_id").cloned().unwrap_or(Value::Null);
    let compare = truthy(input.get("compare"));

    if !truthy(Some(&target_run_id)) {
        let latest = crate::db_json::query_rows(db, "SELECT run_id FROM brain.bench_runs ORDER BY id DESC LIMIT 1", &[])?;
        let row = match latest.as_array().and_then(|a| a.first()) {
            None => return Ok(json!("# Benchmark report\n\n_No bench runs recorded yet._")),
            Some(r) => r,
        };
        target_run_id = row.get("run_id").cloned().unwrap_or(Value::Null);
    }

    let rows_v = crate::db_json::query_rows(
        db,
        "SELECT bench_name, metric, value, unit, git_sha FROM brain.bench_runs WHERE run_id = ? ORDER BY bench_name, metric",
        &[target_run_id.clone()],
    )?;
    let rows = rows_v.as_array().cloned().unwrap_or_default();

    let mut prev_rows: Option<Vec<Value>> = None;
    if compare {
        let prev_run_v = crate::db_json::query_rows(
            db,
            "SELECT DISTINCT run_id FROM brain.bench_runs WHERE run_id != ? ORDER BY id DESC LIMIT 1",
            &[target_run_id.clone()],
        )?;
        if let Some(prev_run) = prev_run_v.as_array().and_then(|a| a.first()) {
            let prev_run_id = prev_run.get("run_id").cloned().unwrap_or(Value::Null);
            let pr = crate::db_json::query_rows(
                db,
                "SELECT bench_name, metric, value FROM brain.bench_runs WHERE run_id = ?",
                &[prev_run_id],
            )?;
            prev_rows = Some(pr.as_array().cloned().unwrap_or_default());
        }
    }

    let mut prev_map: HashMap<String, f64> = HashMap::new();
    if let Some(pr) = &prev_rows {
        for r in pr {
            let key = format!("{}:{}", tmpl(r.get("bench_name")), tmpl(r.get("metric")));
            if let Some(val) = r.get("value").and_then(|v| v.as_f64()) {
                prev_map.insert(key, val);
            }
        }
    }

    let mut out: Vec<String> = Vec::new();
    out.push("# Benchmark report".to_string());
    out.push(String::new());
    out.push(format!("run_id: `{}`", tmpl(Some(&target_run_id))));
    if let Some(first) = rows.first() {
        if truthy(first.get("git_sha")) {
            let sha = first.get("git_sha").and_then(|v| v.as_str()).unwrap_or("");
            let sliced: String = sha.chars().take(12).collect();
            out.push(format!("git_sha: `{sliced}`"));
        }
    }
    out.push(String::new());

    // Group by bench_name, preserving first-seen order (rows already ORDER BY bench_name).
    let mut order: Vec<String> = Vec::new();
    let mut by_bench: HashMap<String, Vec<&Value>> = HashMap::new();
    for r in &rows {
        let name = tmpl(r.get("bench_name"));
        if !by_bench.contains_key(&name) { order.push(name.clone()); }
        by_bench.entry(name).or_default().push(r);
    }

    for name in &order {
        let metrics = &by_bench[name];
        out.push(format!("## {name}"));
        out.push(String::new());
        out.push("| metric | value | unit | Δ vs prev |".to_string());
        out.push("| --- | ---: | --- | ---: |".to_string());
        for m in metrics {
            let bench_name = tmpl(m.get("bench_name"));
            let metric = tmpl(m.get("metric"));
            let key = format!("{bench_name}:{metric}");
            let prev = prev_map.get(&key).copied();
            let mut delta = String::new();
            if let Some(prev_val) = prev {
                let value = m.get("value").and_then(|v| v.as_f64()).unwrap_or(f64::NAN);
                let d = value - prev_val;
                let pct = if prev_val != 0.0 { (d / prev_val) * 100.0 } else { 0.0 };
                let sign = if d > 0.0 { "+" } else { "" };
                delta = format!("{}{} ({}{}%)", sign, fmt(Some(d)), sign, fmt(Some(pct)));
            }
            let value_f = m.get("value").and_then(|v| v.as_f64());
            let unit = m.get("unit").filter(|v| !v.is_null()).map(|v| tmpl(Some(v))).unwrap_or_default();
            out.push(format!("| {metric} | {} | {unit} | {delta} |", fmt(value_f)));
        }
        out.push(String::new());
    }

    Ok(json!(out.join("\n")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "ATTACH DATABASE ':memory:' AS brain; \
             CREATE TABLE brain.bench_runs (id INTEGER PRIMARY KEY AUTOINCREMENT, run_id TEXT NOT NULL, \
               bench_name TEXT NOT NULL, metric TEXT NOT NULL, value REAL NOT NULL, unit TEXT, git_sha TEXT);",
        )
        .unwrap();
        c
    }
    #[allow(clippy::too_many_arguments)]
    fn rec(c: &Connection, run_id: &str, bench: &str, metric: &str, value: f64, unit: Option<&str>, git_sha: Option<&str>) {
        c.execute(
            "INSERT INTO brain.bench_runs (run_id, bench_name, metric, value, unit, git_sha) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![run_id, bench, metric, value, unit, git_sha],
        )
        .unwrap();
    }

    #[test]
    fn no_runs_message() {
        let c = db();
        let r = benchReport(&json!({}), &c).unwrap();
        assert_eq!(r, json!("# Benchmark report\n\n_No bench runs recorded yet._"));
    }

    #[test]
    fn auto_detect_git_sha_and_magnitude_buckets() {
        let c = db();
        rec(&c, "r-only", "card-add", "latency_ms", 5.25, Some("ms"), Some("abcdef1234567890abcdef"));
        rec(&c, "r-only", "card-add", "throughput", 1500.0, None, Some("abcdef1234567890abcdef"));
        rec(&c, "r-only", "db-open", "cold_ms", 50.5, Some("ms"), Some("abcdef1234567890abcdef"));
        rec(&c, "r-only", "db-open", "warm_ms", 0.1234, Some("ms"), Some("abcdef1234567890abcdef"));
        let r = benchReport(&json!({ "compare": true }), &c).unwrap();
        let s = r.as_str().unwrap();
        assert!(s.starts_with("# Benchmark report\n\nrun_id: `r-only`\ngit_sha: `abcdef123456`\n\n"));
        assert!(s.contains("## card-add"));
        assert!(s.contains("## db-open"));
        assert!(s.contains("| latency_ms | 5.25 | ms |  |"));
        assert!(s.contains("| throughput | 1500 |  |  |"));
        assert!(s.contains("| cold_ms | 50.5 | ms |  |"));
        assert!(s.contains("| warm_ms | 0.123 | ms |  |"));
        // compare=true but no OTHER run_id exists → prevMap empty → every delta cell is blank
        assert!(!s.contains('+') && !s.contains('%'));
    }

    #[test]
    fn compare_with_previous_run_sign_and_missing_metric() {
        let c = db();
        rec(&c, "r-prev", "suite-x", "m-zero", 0.0, None, Some("deadbeef00"));
        rec(&c, "r-prev", "suite-x", "m-up", 10.0, Some("ops/s"), Some("deadbeef00"));
        rec(&c, "r-prev", "suite-x", "m-down", 20.0, None, Some("deadbeef00"));
        rec(&c, "r-cur", "suite-x", "m-down", 12.0, None, None);
        rec(&c, "r-cur", "suite-x", "m-new", 7.0, Some("ms"), None);
        rec(&c, "r-cur", "suite-x", "m-up", 15.0, Some("ops/s"), None);
        rec(&c, "r-cur", "suite-x", "m-zero", 3.0, None, None);
        let r = benchReport(&json!({ "run_id": "r-cur", "compare": true }), &c).unwrap();
        let s = r.as_str().unwrap();
        assert!(s.contains("run_id: `r-cur`"));
        assert!(!s.contains("git_sha")); // r-cur rows all have null git_sha
        assert!(s.contains("| m-down | 12.0 |  | -8.00 (-40.0%) |")); // d<0 → no leading '+'
        assert!(s.contains("| m-new | 7.00 | ms |  |")); // no matching previous metric → blank delta
        assert!(s.contains("| m-up | 15.0 | ops/s | +5.00 (+50.0%) |")); // d>0 → '+'
        assert!(s.contains("| m-zero | 3.00 |  | +3.00 (+0.000%) |")); // prev===0 → pct short-circuits to 0
    }

    #[test]
    fn compare_true_no_other_run_and_default_compare_false() {
        let c = db();
        rec(&c, "solo-run", "bench-a", "metric-a", 9.5, Some("s"), Some("cafebabe1234"));
        let r = benchReport(&json!({ "run_id": "solo-run" }), &c).unwrap(); // compare omitted → false
        let s = r.as_str().unwrap();
        assert!(s.contains("| metric-a | 9.50 | s |  |"));
    }

    #[test]
    fn null_input_and_falsy_run_id_fall_back_to_latest() {
        let c = db();
        rec(&c, "auto-run", "bench-z", "metric-z", 2.0, None, Some("1234567890ab"));
        let r = benchReport(&Value::Null, &c).unwrap();
        assert!(r.as_str().unwrap().contains("run_id: `auto-run`"));

        rec(&c, "older-run", "bench-q", "metric-q", 3.3, Some("ms"), Some("aa11bb22cc33"));
        rec(&c, "newer-run", "bench-q", "metric-q", 4.4, Some("ms"), Some("aa11bb22cc33"));
        let r2 = benchReport(&json!({ "run_id": "" }), &c).unwrap(); // falsy run_id → auto-detect latest
        let s2 = r2.as_str().unwrap();
        assert!(s2.contains("run_id: `newer-run`"));
        assert!(s2.contains("| metric-q | 4.40 | ms |  |"));
    }
}
