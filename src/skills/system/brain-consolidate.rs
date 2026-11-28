//! brain-consolidate.rs — twin of skills/system/brain-consolidate.mjs (cross-db WRITE lane;
//! ATTACH memory + brain). NAMES ARE CONTRACT: `brainConsolidate` (default). Digests a
//! non-overlapping window of brain.ai_calls (older than `older_than_days`, newer than the last
//! consolidation's period_end) into one memory.memory_consolidation row: overview/analysis/
//! keypoints/learnings/fails/journey. Returns {consolidated:false} when the window is empty
//! (idempotent — consecutive runs never re-digest the same calls).
use rusqlite::Connection;
use serde_json::{json, Map, Value};

const DEFAULT_OLDER_THAN_DAYS: i64 = 30;
// The conform-db JS lane freezes the clock to this instant; the Rust twin has no clock hook so
// the frozen literal is written in place of `datetime('now')` by hand (see playbook).

fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) } // JS `??` right operand
fn str_qq(v: Option<&Value>) -> String {
    // String(v ?? '') — also IS Array.prototype.join's per-element stringify (null/undefined → "").
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(),
        Some(o) => o.to_string(),
    }
}
// JS `${v}` interpolation (string/number/bool/null/absent).
fn tmpl(v: Option<&Value>) -> String {
    match v { None => "undefined".into(), Some(Value::Null) => "null".into(), Some(x) => str_qq(Some(x)) }
}
// = JS Number(v)
fn js_number(v: Option<&Value>) -> f64 {
    match v {
        None => f64::NAN,
        Some(Value::Null) => 0.0,
        Some(Value::Bool(b)) => if *b { 1.0 } else { 0.0 },
        Some(Value::Number(n)) => n.as_f64().unwrap_or(f64::NAN),
        Some(Value::String(s)) => { let t = s.trim(); if t.is_empty() { 0.0 } else { t.parse().unwrap_or(f64::NAN) } },
        Some(_) => f64::NAN,
    }
}
// Same numeric value, as a serde_json Value: integer-valued → Number::Integer (JS/JSON.stringify
// prints "30", never "30.0"); genuinely fractional → f64 passed through unchanged.
fn js_num_value(f: f64) -> Value { if f.is_finite() && f == f.trunc() && f.abs() < 1e15 { json!(f as i64) } else { json!(f) } }

fn as_f64(v: Option<&Value>) -> f64 { v.and_then(|x| x.as_f64()).unwrap_or(0.0) }
fn as_i64(v: Option<&Value>) -> i64 { v.and_then(|x| x.as_i64()).unwrap_or(0) }

// mdTable(header, rows): rows are stringified with array-join semantics (str_qq per cell — null/
// undefined become "", NOT the string "null"/"undefined").
fn md_table(header: &[&str], rows: &[Vec<Value>]) -> String {
    if rows.is_empty() { return "_none_".into(); }
    let mut lines = vec![format!("| {} |", header.join(" | "))];
    lines.push(format!("|{}|", header.iter().map(|_| "---").collect::<Vec<_>>().join("|")));
    for r in rows {
        let cells: Vec<String> = r.iter().map(|v| str_qq(Some(v))).collect();
        lines.push(format!("| {} |", cells.join(" | ")));
    }
    lines.join("\n")
}

pub fn brainConsolidate(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS throws err.contextMissing('brain-consolidate','db') when no db — the db lane always
    // injects a live Connection, so this guard is unreachable in Rust and not represented.
    let older_than_days_raw = nz(input.get("older_than_days")).cloned().unwrap_or(json!(DEFAULT_OLDER_THAN_DAYS));
    let older_than_days_f = js_number(Some(&older_than_days_raw));
    let older_than_days_v = js_num_value(older_than_days_f);

    let last_rows = crate::db_json::query_rows(
        db,
        "SELECT period_end FROM memory.memory_consolidation WHERE source = 'ai_calls' ORDER BY period_end DESC LIMIT 1",
        &[],
    )?;
    let last_row = last_rows.as_array().and_then(|a| a.first()).cloned();
    let last_period_end_v: Option<Value> = last_row.as_ref().and_then(|r| r.get("period_end")).cloned();
    let floor = nz(last_period_end_v.as_ref()).cloned().unwrap_or(json!("0000-01-01"));

    let totals_sql = &format!("SELECT COUNT(*) AS n, MIN(ts) AS first_ts, MAX(ts) AS last_ts, \
         COALESCE(SUM(cost_usd_actual), 0) AS cost, COALESCE(SUM(tokens_in), 0) AS tin, \
         COALESCE(SUM(tokens_out), 0) AS tout, \
         SUM(CASE WHEN outcome = 'success' THEN 1 ELSE 0 END) AS ok_n, \
         SUM(CASE WHEN cached = 1 THEN 1 ELSE 0 END) AS cached_n \
         FROM brain.ai_calls WHERE ts < datetime({}, '-' || ? || ' days') AND ts > ?", crate::clock::now_sql_inner());
    let totals_rows = crate::db_json::query_rows(db, totals_sql, &[older_than_days_v.clone(), floor.clone()])?;
    let totals = totals_rows.as_array().and_then(|a| a.first()).cloned().unwrap_or_else(|| json!({}));
    let n = as_i64(totals.get("n"));

    if n == 0 {
        let mut out = Map::new();
        out.insert("consolidated".into(), json!(false));
        out.insert("reason".into(), json!(format!("no un-consolidated ai_calls older than {}d", tmpl(Some(&older_than_days_v)))));
        out.insert("last_period_end".into(), nz(last_period_end_v.as_ref()).cloned().unwrap_or(Value::Null));
        return Ok(Value::Object(out));
    }
    let period_start = totals.get("first_ts").cloned().unwrap_or(Value::Null);
    let period_end = totals.get("last_ts").cloned().unwrap_or(Value::Null);
    let ok_n = as_i64(totals.get("ok_n"));
    let cached_n = as_i64(totals.get("cached_n"));
    let cost = as_f64(totals.get("cost"));
    let tin = as_i64(totals.get("tin"));
    let tout = as_i64(totals.get("tout"));
    let fail_total = n - ok_n;

    let by_prompt = crate::db_json::query_rows(
        db,
        "SELECT pt.name AS prompt, c.tier, COUNT(*) AS calls, \
         COALESCE(SUM(c.cost_usd_actual), 0) AS cost, ROUND(AVG(c.duration_ms)) AS avg_ms, \
         SUM(CASE WHEN c.outcome != 'success' THEN 1 ELSE 0 END) AS failed \
         FROM brain.ai_calls c JOIN brain.prompt_templates pt ON pt.id = c.prompt_template_id \
         WHERE c.ts >= ? AND c.ts <= ? GROUP BY pt.name, c.tier ORDER BY cost DESC LIMIT 40",
        &[period_start.clone(), period_end.clone()],
    )?.as_array().cloned().unwrap_or_default();

    let fail_groups = crate::db_json::query_rows(
        db,
        "SELECT pt.name AS prompt, c.outcome, COUNT(*) AS n, MAX(c.ts) AS last_seen \
         FROM brain.ai_calls c JOIN brain.prompt_templates pt ON pt.id = c.prompt_template_id \
         WHERE c.ts >= ? AND c.ts <= ? AND c.outcome != 'success' \
         GROUP BY pt.name, c.outcome ORDER BY n DESC LIMIT 20",
        &[period_start.clone(), period_end.clone()],
    )?.as_array().cloned().unwrap_or_default();

    let learnings = crate::db_json::query_rows(
        db,
        "SELECT type, title FROM memory.memories WHERE created_at >= ? AND created_at <= ? \
         AND type IN ('correction', 'lesson', 'decision') ORDER BY id ASC LIMIT 50",
        &[period_start.clone(), period_end.clone()],
    )?.as_array().cloned().unwrap_or_default();

    // driver_ticks shape drift on old DBs — journey degrades (mirrors the JS try/catch). Not
    // fixtured: forcing the query to fail would require a schema that omits the columns the
    // happy-path cases need, and fixtures share one schema across all cases (see playbook).
    let journey_rows: Vec<Value> = match crate::db_json::query_rows(
        db,
        "SELECT card_slug, COUNT(*) AS ticks, \
         SUM(CASE WHEN outcome = 'fired' THEN 1 ELSE 0 END) AS fired, \
         SUM(CASE WHEN outcome = 'failed' THEN 1 ELSE 0 END) AS failed, \
         COALESCE(SUM(cost_usd), 0) AS cost \
         FROM brain.driver_ticks WHERE started_at >= ? AND started_at <= ? AND card_slug IS NOT NULL \
         GROUP BY card_slug ORDER BY ticks DESC LIMIT 40",
        &[period_start.clone(), period_end.clone()],
    ) {
        Ok(v) => v.as_array().cloned().unwrap_or_default(),
        Err(_) => Vec::new(),
    };

    let overview_line = format!(
        "{} AI calls ({} ok, {} failed, {} cache hits) \u{b7} ${:.4} \u{b7} {} tokens in / {} out. \
         {} distinct prompt/tier mixes; {} cards driven; {} memories recorded while the window was live.",
        n, ok_n, fail_total, cached_n, cost, tin, tout, by_prompt.len(), journey_rows.len(), learnings.len()
    );
    let overview_md = format!(
        "## AI-interaction window {} \u{2192} {}\n\n{}",
        tmpl(Some(&period_start)), tmpl(Some(&period_end)), overview_line
    );

    let analysis_rows: Vec<Vec<Value>> = by_prompt.iter().map(|r| {
        let cost_f = as_f64(r.get("cost"));
        let avg_ms = r.get("avg_ms").cloned().unwrap_or(Value::Null);
        let avg_display = if avg_ms.is_null() { json!("\u{2014}") } else { js_num_value(as_f64(Some(&avg_ms))) };
        vec![
            r.get("prompt").cloned().unwrap_or(Value::Null),
            r.get("tier").cloned().unwrap_or(Value::Null),
            r.get("calls").cloned().unwrap_or(Value::Null),
            json!(format!("{:.4}", cost_f)),
            avg_display,
            r.get("failed").cloned().unwrap_or(Value::Null),
        ]
    }).collect();
    let analysis_md = format!(
        "### Prompt mix (by cost)\n{}",
        md_table(&["prompt", "tier", "calls", "cost $", "avg ms", "failed"], &analysis_rows)
    );

    let fail_rows: Vec<Vec<Value>> = fail_groups.iter().map(|r| vec![
        r.get("prompt").cloned().unwrap_or(Value::Null),
        r.get("outcome").cloned().unwrap_or(Value::Null),
        r.get("n").cloned().unwrap_or(Value::Null),
        r.get("last_seen").cloned().unwrap_or(Value::Null),
    ]).collect();
    let fails_md = format!("### Failures\n{}", md_table(&["prompt", "outcome", "count", "last seen"], &fail_rows));

    let learnings_md = if learnings.is_empty() {
        "### Memories recorded in-window\n_none_".to_string()
    } else {
        let lines: Vec<String> = learnings.iter()
            .map(|l| format!("- **{}** \u{2014} {}", tmpl(l.get("type")), tmpl(l.get("title"))))
            .collect();
        format!("### Memories recorded in-window\n{}", lines.join("\n"))
    };

    let journey_out_rows: Vec<Vec<Value>> = journey_rows.iter().map(|r| vec![
        r.get("card_slug").cloned().unwrap_or(Value::Null),
        r.get("ticks").cloned().unwrap_or(Value::Null),
        r.get("fired").cloned().unwrap_or(Value::Null),
        r.get("failed").cloned().unwrap_or(Value::Null),
        json!(format!("{:.4}", as_f64(r.get("cost")))),
    ]).collect();
    let journey_md = format!(
        "### Cards driven in-window\n{}",
        md_table(&["card", "ticks", "fired", "failed", "cost $"], &journey_out_rows)
    );

    let mut keypoints: Vec<Value> = Vec::new();
    for r in by_prompt.iter().take(10) {
        let cost_f = as_f64(r.get("cost"));
        let text = format!(
            "{} ({}): {} calls, ${:.4}, {} failed",
            tmpl(r.get("prompt")), tmpl(r.get("tier")), tmpl(r.get("calls")), cost_f, tmpl(r.get("failed"))
        );
        keypoints.push(json!({ "kind": "prompt", "text": text, "weight": js_num_value(cost_f) }));
    }
    for r in fail_groups.iter().take(10) {
        let text = format!("{} \u{2192} {} \u{d7}{}", tmpl(r.get("prompt")), tmpl(r.get("outcome")), tmpl(r.get("n")));
        keypoints.push(json!({ "kind": "fail", "text": text, "weight": r.get("n").cloned().unwrap_or(Value::Null) }));
    }
    for r in journey_rows.iter().take(10) {
        let text = format!(
            "{}: {} ticks ({} fired / {} failed)",
            tmpl(r.get("card_slug")), tmpl(r.get("ticks")), tmpl(r.get("fired")), tmpl(r.get("failed"))
        );
        keypoints.push(json!({ "kind": "card", "text": text, "weight": r.get("ticks").cloned().unwrap_or(Value::Null) }));
    }
    for l in learnings.iter().take(10) {
        let text = format!("{}: {}", tmpl(l.get("type")), tmpl(l.get("title")));
        keypoints.push(json!({ "kind": "learning", "text": text, "weight": json!(1) }));
    }

    let keypoints_json = serde_json::to_string(&Value::Array(keypoints.clone())).map_err(|e| e.to_string())?;
    let mut meta = Map::new();
    meta.insert("older_than_days".into(), older_than_days_v.clone());
    meta.insert("prompt_mixes".into(), json!(by_prompt.len() as i64));
    meta.insert("fail_total".into(), json!(fail_total));
    let meta_json = serde_json::to_string(&Value::Object(meta)).map_err(|e| e.to_string())?;

    let (_, last_id) = crate::db_json::run_write(
        db,
        "INSERT INTO memory.memory_consolidation \
         (source, period_start, period_end, calls_count, cost_usd_total, \
          tokens_in_total, tokens_out_total, overview_md, analysis_md, \
          keypoints_json, learnings_md, fails_md, journey_md, meta_json) \
         VALUES ('ai_calls', ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        &[
            period_start.clone(), period_end.clone(), json!(n), js_num_value(cost), json!(tin), json!(tout),
            json!(overview_md), json!(analysis_md), json!(keypoints_json),
            json!(learnings_md), json!(fails_md), json!(journey_md), json!(meta_json),
        ],
    )?;

    let mut out = Map::new();
    out.insert("consolidated".into(), json!(true));
    out.insert("consolidation_id".into(), json!(last_id));
    out.insert("period_start".into(), period_start);
    out.insert("period_end".into(), period_end);
    out.insert("calls_count".into(), json!(n));
    out.insert("cost_usd_total".into(), js_num_value(cost));
    out.insert("keypoints".into(), json!(keypoints.len() as i64));
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCHEMA: &str = "
        CREATE TABLE brain.prompt_templates (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT);
        CREATE TABLE brain.ai_calls (
          id INTEGER PRIMARY KEY AUTOINCREMENT, ts TEXT NOT NULL, prompt_template_id INTEGER,
          tier TEXT, cost_usd_actual REAL, tokens_in INTEGER, tokens_out INTEGER,
          outcome TEXT, cached INTEGER, duration_ms REAL
        );
        CREATE TABLE brain.driver_ticks (
          id INTEGER PRIMARY KEY AUTOINCREMENT, card_slug TEXT, started_at TEXT,
          outcome TEXT, cost_usd REAL
        );
        CREATE TABLE memory.memories (
          id INTEGER PRIMARY KEY AUTOINCREMENT, type TEXT NOT NULL, title TEXT NOT NULL,
          situation TEXT NOT NULL DEFAULT '', guidance TEXT NOT NULL DEFAULT '',
          created_at TEXT NOT NULL, created_by TEXT NOT NULL DEFAULT 'seed'
        );
        CREATE TABLE memory.memory_consolidation (
          id INTEGER PRIMARY KEY AUTOINCREMENT,
          created_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')),
          source TEXT NOT NULL DEFAULT 'ai_calls',
          period_start TEXT NOT NULL, period_end TEXT NOT NULL, calls_count INTEGER NOT NULL,
          cost_usd_total REAL, tokens_in_total INTEGER, tokens_out_total INTEGER,
          overview_md TEXT NOT NULL, analysis_md TEXT, keypoints_json TEXT,
          learnings_md TEXT, fails_md TEXT, journey_md TEXT, meta_json TEXT, pruned_at TEXT
        );
    ";

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS memory; ATTACH ':memory:' AS brain;").unwrap();
        c.execute_batch(SCHEMA).unwrap();
        c
    }
    fn tpl(c: &Connection, name: &str) -> i64 {
        c.execute("INSERT INTO brain.prompt_templates (name) VALUES (?1)", rusqlite::params![name]).unwrap();
        c.last_insert_rowid()
    }
    #[allow(clippy::too_many_arguments)]
    fn call(c: &Connection, tpl: i64, tier: &str, ts: &str, cost: f64, tin: i64, tout: i64, outcome: &str, cached: i64, dur: Option<f64>) {
        c.execute(
            "INSERT INTO brain.ai_calls (ts, prompt_template_id, tier, cost_usd_actual, tokens_in, tokens_out, outcome, cached, duration_ms) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
            rusqlite::params![ts, tpl, tier, cost, tin, tout, outcome, cached, dur],
        ).unwrap();
    }
    fn tick(c: &Connection, card: Option<&str>, started_at: &str, outcome: &str, cost: f64) {
        c.execute(
            "INSERT INTO brain.driver_ticks (card_slug, started_at, outcome, cost_usd) VALUES (?1,?2,?3,?4)",
            rusqlite::params![card, started_at, outcome, cost],
        ).unwrap();
    }
    fn memory(c: &Connection, kind: &str, title: &str, created_at: &str) {
        c.execute(
            "INSERT INTO memory.memories (type, title, created_at) VALUES (?1,?2,?3)",
            rusqlite::params![kind, title, created_at],
        ).unwrap();
    }

    #[test]
    fn empty_window_reports_no_prior_consolidation() {
        let c = db();
        let r = brainConsolidate(&json!({ "older_than_days": 7 }), &c).unwrap();
        assert_eq!(r["consolidated"], json!(false));
        assert_eq!(r["last_period_end"], Value::Null);
        assert!(r["reason"].as_str().unwrap().contains("older than 7d"));
    }

    #[test]
    fn happy_path_digests_window_and_writes_row() {
        let c = db();
        let a = tpl(&c, "promptA");
        let b = tpl(&c, "promptB");
        call(&c, a, "mid", "2025-12-01 10:00:00", 0.5, 100, 50, "success", 0, Some(200.0));
        call(&c, a, "mid", "2025-12-05 10:00:00", 0.25, 50, 25, "success", 1, Some(100.0));
        call(&c, b, "high", "2025-12-10 08:00:00", 1.0, 200, 100, "error", 0, Some(300.0));
        call(&c, a, "low", "2025-12-04 00:00:00", 0.1, 10, 5, "success", 0, None); // null avg_ms group
        call(&c, a, "mid", "2025-12-26 00:00:00", 5.0, 999, 999, "success", 0, Some(1.0)); // outside window

        memory(&c, "lesson", "Lesson A", "2025-12-02 00:00:00");
        memory(&c, "preference", "Pref B", "2025-12-03 00:00:00"); // wrong type — excluded
        memory(&c, "decision", "Decision C", "2025-12-09 00:00:00");
        memory(&c, "lesson", "Too early", "2025-11-01 00:00:00"); // before period_start — excluded

        tick(&c, Some("CARD-A"), "2025-12-02 00:00:00", "fired", 0.1);
        tick(&c, Some("CARD-A"), "2025-12-03 00:00:00", "failed", 0.05);
        tick(&c, Some("CARD-B"), "2025-12-08 00:00:00", "fired", 0.2);
        tick(&c, None, "2025-12-02 00:00:00", "fired", 1.0); // null card_slug — excluded

        let r = brainConsolidate(&json!({ "older_than_days": 7 }), &c).unwrap();
        assert_eq!(r["consolidated"], json!(true));
        assert_eq!(r["period_start"], json!("2025-12-01 10:00:00"));
        assert_eq!(r["period_end"], json!("2025-12-10 08:00:00"));
        assert_eq!(r["calls_count"], json!(4));
        assert!((r["cost_usd_total"].as_f64().unwrap() - 1.85).abs() < 1e-9);
        assert_eq!(r["keypoints"], json!(8));

        let row = crate::db_json::query_rows(&c, "SELECT * FROM memory.memory_consolidation WHERE id = ?", &[r["consolidation_id"].clone()]).unwrap();
        let row = &row[0];
        assert!(row["overview_md"].as_str().unwrap().contains("## AI-interaction window 2025-12-01 10:00:00 \u{2192} 2025-12-10 08:00:00"));
        assert!(row["overview_md"].as_str().unwrap().contains("4 AI calls (3 ok, 1 failed, 1 cache hits)"));
        assert!(row["analysis_md"].as_str().unwrap().contains("| promptB | high | 1 | 1.0000 | 300 | 1 |"));
        assert!(row["analysis_md"].as_str().unwrap().contains("| promptA | low | 1 | 0.1000 | \u{2014} | 0 |"));
        assert!(row["fails_md"].as_str().unwrap().contains("| promptB | error | 1 |"));
        assert!(row["learnings_md"].as_str().unwrap().contains("- **lesson** \u{2014} Lesson A"));
        assert!(!row["learnings_md"].as_str().unwrap().contains("Pref B"));
        assert!(row["journey_md"].as_str().unwrap().contains("| CARD-A | 2 | 1 | 1 | 0.1500 |"));

        let keypoints: Value = serde_json::from_str(row["keypoints_json"].as_str().unwrap()).unwrap();
        assert_eq!(keypoints.as_array().unwrap().len(), 8);
        let meta: Value = serde_json::from_str(row["meta_json"].as_str().unwrap()).unwrap();
        assert_eq!(meta, json!({ "older_than_days": 7, "prompt_mixes": 3, "fail_total": 1 }));
    }

    #[test]
    fn falls_back_to_none_when_fails_learnings_journey_are_empty() {
        let c = db();
        let t = tpl(&c, "onlyPrompt");
        call(&c, t, "low", "2025-12-05 00:00:00", 0.3, 30, 15, "success", 0, Some(120.0));
        let r = brainConsolidate(&json!({ "older_than_days": 7 }), &c).unwrap();
        let row = crate::db_json::query_rows(&c, "SELECT * FROM memory.memory_consolidation WHERE id = ?", &[r["consolidation_id"].clone()]).unwrap();
        let row = &row[0];
        assert!(row["fails_md"].as_str().unwrap().contains("### Failures\n_none_"));
        assert!(row["learnings_md"].as_str().unwrap().contains("### Memories recorded in-window\n_none_"));
        assert!(row["journey_md"].as_str().unwrap().contains("### Cards driven in-window\n_none_"));
    }

    #[test]
    fn floor_excludes_already_consolidated_calls_and_ignores_other_sources() {
        let c = db();
        c.execute(
            "INSERT INTO memory.memory_consolidation (period_start, period_end, calls_count, overview_md, source) VALUES ('2025-08-01 00:00:00','2025-09-01 00:00:00', 2, 'r1', 'ai_calls')",
            [],
        ).unwrap();
        c.execute(
            "INSERT INTO memory.memory_consolidation (period_start, period_end, calls_count, overview_md, source) VALUES ('2025-10-01 00:00:00','2025-11-01 00:00:00', 3, 'r2', 'ai_calls')",
            [],
        ).unwrap();
        c.execute(
            "INSERT INTO memory.memory_consolidation (period_start, period_end, calls_count, overview_md, source) VALUES ('2025-12-01 00:00:00','2025-12-20 00:00:00', 99, 'ignored', 'other_source')",
            [],
        ).unwrap();
        let t = tpl(&c, "promptA");
        call(&c, t, "mid", "2025-10-15 00:00:00", 2.0, 20, 10, "success", 0, Some(50.0)); // before floor — excluded
        call(&c, t, "mid", "2025-11-15 00:00:00", 0.4, 40, 20, "success", 0, Some(80.0)); // included
        call(&c, t, "mid", "2025-12-15 00:00:00", 9.0, 90, 90, "success", 0, Some(9.0)); // too new — excluded

        let r = brainConsolidate(&json!({}), &c).unwrap(); // default older_than_days = 30
        assert_eq!(r["consolidated"], json!(true));
        assert_eq!(r["calls_count"], json!(1));
        assert_eq!(r["period_start"], json!("2025-11-15 00:00:00"));
        assert_eq!(r["period_end"], json!("2025-11-15 00:00:00"));
    }
}
