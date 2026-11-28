//! model-scoreboard.rs — twin of skills/driver/model-scoreboard.mjs (db READ, cross-db brain.*).
//! NAMES ARE CONTRACT: `modelScoreboard` (default). The EMPIRICAL side of the model center:
//! op 'scoreboard' ranks (prompt template, model) by Laplace-smoothed clean rate over
//! brain.ai_calls (quality flags r24: degenerate/truncated 'successes' count against);
//! op 'draws' shows draw fairness per (tier, model) — failovers made loud. Pure reads.
// QUALITY: 84/100 — frozen-clock window, template#id fallback, HAVING>=2, smoothed clean_rate
use rusqlite::Connection;
use serde_json::{json, Map, Value};

fn js_number(v: Option<&Value>) -> f64 { match v { None=>f64::NAN, Some(Value::Null)=>0.0, Some(Value::Bool(b))=>if *b {1.0} else {0.0}, Some(Value::Number(n))=>n.as_f64().unwrap_or(f64::NAN), Some(Value::String(s))=>{let t=s.trim(); if t.is_empty(){0.0}else{t.parse().unwrap_or(f64::NAN)}}, Some(_)=>f64::NAN } } // = JS Number(v)

// JS number output: whole values render as integers ("30"), fractionals stay f64 ("7.5").
fn js_num(n: f64) -> Value {
    if n.is_finite() && n.fract() == 0.0 && n.abs() < 9e15 { json!(n as i64) } else { json!(n) }
}

pub fn modelScoreboard(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS throws err.contextMissing('model-scoreboard', 'db') when no db — the db lane always
    // injects one, so that guard cannot fire here.
    let op = if input.get("op").and_then(|v| v.as_str()) == Some("draws") { "draws" } else { "scoreboard" };
    let days_n = js_number(input.get("days"));
    let days = if days_n.is_finite() && days_n > 0.0 { days_n } else { 30.0 };
    // JS: `datetime('now', '-${Math.floor(days)} days')` — the frozen-clock lane pins 'now'.
    let since = format!("datetime({}, '-{} days')", crate::clock::now_sql_inner(), days.floor() as i64);

    if op == "draws" {
        let rows = crate::db_json::query_rows(db, &format!("
      SELECT tier, model_id,
             COUNT(*)                                                          AS calls,
             SUM(CASE WHEN outcome = 'success' AND cached = 0 THEN 1 ELSE 0 END) AS served,
             SUM(CASE WHEN cached = 1 THEN 1 ELSE 0 END)                       AS cached_hits,
             SUM(CASE WHEN outcome != 'success' THEN 1 ELSE 0 END)             AS failovers,
             ROUND(SUM(COALESCE(cost_usd_actual, 0)), 4)                       AS spend_usd
      FROM brain.ai_calls
      WHERE ts >= {since} AND model_id IS NOT NULL
      GROUP BY tier, model_id
      ORDER BY tier, calls DESC
    "), &[])?;
        let mut out = Map::new();
        out.insert("ok".into(), json!(true));
        out.insert("op".into(), json!(op));
        out.insert("days".into(), js_num(days));
        out.insert("rows".into(), rows);
        return Ok(Value::Object(out));
    }

    // QUALITY FLAGS (r24): a transport 'success' whose OUTPUT was rejected by a guard —
    // degenerate (prose-as-code) or truncated (cut at the token cap) — is a QUALITY failure.
    let rows_v = crate::db_json::query_rows(db, &format!("
    SELECT COALESCE(p.name, 'template#' || a.prompt_template_id) AS template,
           a.tier, a.model_id,
           COUNT(*)                                                            AS calls,
           SUM(CASE WHEN a.outcome = 'success' AND a.cached = 0
                     AND COALESCE(a.outcome_class, '') NOT IN ('degenerate', 'truncated', 'validation_failed')
                    THEN 1 ELSE 0 END)                                         AS fresh_ok,
           SUM(CASE WHEN a.cached = 1 THEN 1 ELSE 0 END)                       AS cached_hits,
           SUM(CASE WHEN a.outcome_class = 'validation_failed' OR a.outcome = 'validation_failed' THEN 1 ELSE 0 END) AS validation_fails,
           SUM(CASE WHEN a.outcome = 'success' AND COALESCE(a.outcome_class, '') IN ('degenerate', 'truncated') THEN 1 ELSE 0 END) AS quality_flags,
           SUM(CASE WHEN a.outcome != 'success' THEN 1 ELSE 0 END)             AS errors,
           ROUND(AVG(CASE WHEN a.cached = 0 THEN a.duration_ms END), 0)        AS avg_ms,
           ROUND(SUM(COALESCE(a.cost_usd_actual, 0)), 4)                       AS spend_usd
    FROM brain.ai_calls a
    LEFT JOIN brain.prompt_templates p ON p.id = a.prompt_template_id
    WHERE a.ts >= {since} AND a.model_id IS NOT NULL
    GROUP BY template, a.tier, a.model_id
    HAVING calls >= 2
    ORDER BY template, calls DESC
  "), &[])?;

    // Laplace-smoothed clean rate: (ok + 1) / (attempts + 2). JS `+(x).toFixed(3)`:
    // scaled round-half-away (positive domain) then integer-collapse via js_num.
    let mut rows = rows_v.as_array().cloned().unwrap_or_default();
    for r in rows.iter_mut() {
        if let Some(obj) = r.as_object_mut() {
            let fresh_ok = obj.get("fresh_ok").and_then(|v| v.as_i64()).unwrap_or(0);
            let errors = obj.get("errors").and_then(|v| v.as_i64()).unwrap_or(0);
            let quality = obj.get("quality_flags").and_then(|v| v.as_i64()).unwrap_or(0);
            let attempts = fresh_ok + errors + quality;
            let clean = if attempts > 0 {
                let x = (fresh_ok + 1) as f64 / (attempts + 2) as f64;
                js_num((x * 1000.0).round() / 1000.0)
            } else {
                Value::Null
            };
            obj.insert("clean_rate".into(), clean); // new key appends after spend_usd, like JS
        }
    }
    let mut out = Map::new();
    out.insert("ok".into(), json!(true));
    out.insert("op".into(), json!(op));
    out.insert("days".into(), js_num(days));
    out.insert("rows".into(), Value::Array(rows));
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "ATTACH DATABASE ':memory:' AS brain; \
             CREATE TABLE brain.prompt_templates (id INTEGER PRIMARY KEY, name TEXT); \
             CREATE TABLE brain.ai_calls (id INTEGER PRIMARY KEY, prompt_template_id INTEGER, tier TEXT, \
               model_id TEXT, cached INTEGER, outcome TEXT, outcome_class TEXT, ts TEXT, \
               duration_ms INTEGER, cost_usd_actual REAL); \
             INSERT INTO brain.prompt_templates (id, name) VALUES (1, 'card.discuss');",
        )
        .unwrap();
        c
    }
    #[allow(clippy::too_many_arguments)]
    fn rec(c: &Connection, tpl: Option<i64>, tier: &str, model: Option<&str>, cached: i64,
           outcome: &str, class: Option<&str>, ts: &str, dur: i64, cost: Option<f64>) {
        c.execute(
            "INSERT INTO brain.ai_calls (prompt_template_id, tier, model_id, cached, outcome, outcome_class, ts, duration_ms, cost_usd_actual) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            rusqlite::params![tpl, tier, model, cached, outcome, class, ts, dur, cost],
        )
        .unwrap();
    }

    #[test]
    fn scoreboard_smoothed_clean_rate_and_quality_flags() {
        let c = db();
        let a = Some("model-a");
        rec(&c, Some(1), "mid", a, 0, "success", None, "2025-12-20 10:00:00", 100, Some(0.001));
        rec(&c, Some(1), "mid", a, 0, "success", None, "2025-12-20 10:00:01", 110, Some(0.001));
        rec(&c, Some(1), "mid", a, 1, "success", None, "2025-12-20 10:00:02", 50, None);
        rec(&c, Some(1), "mid", a, 0, "error", None, "2025-12-20 10:00:03", 120, Some(0.001));
        rec(&c, Some(1), "mid", a, 0, "success", Some("degenerate"), "2025-12-20 10:00:04", 130, Some(0.001));
        rec(&c, Some(1), "mid", a, 0, "validation_failed", None, "2025-12-20 10:00:05", 140, Some(0.001));
        rec(&c, Some(1), "mid", a, 0, "success", Some("validation_failed"), "2025-12-20 10:00:06", 150, Some(0.001));
        // single-call group — HAVING calls >= 2 hides it
        rec(&c, Some(1), "mid", Some("model-b"), 0, "success", None, "2025-12-20 11:00:00", 90, Some(0.002));
        // no matching template row → 'template#99'
        rec(&c, Some(99), "high", Some("model-x"), 0, "success", None, "2025-12-21 09:00:00", 200, Some(0.003));
        rec(&c, Some(99), "high", Some("model-x"), 0, "success", None, "2025-12-21 09:00:01", 220, Some(0.003));
        // cached-only group — attempts 0 → clean_rate null, avg_ms null
        rec(&c, Some(1), "mid", Some("model-c"), 1, "success", None, "2025-12-23 08:00:00", 10, None);
        rec(&c, Some(1), "mid", Some("model-c"), 1, "success", None, "2025-12-23 08:00:01", 12, None);
        // excluded: null model_id, out-of-window ts
        rec(&c, Some(1), "mid", None, 0, "success", None, "2025-12-20 12:00:00", 99, Some(0.9));
        rec(&c, Some(1), "mid", a, 0, "success", None, "2025-10-01 12:00:00", 99, Some(0.9));

        let r = modelScoreboard(&json!({}), &c).unwrap();
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["op"], json!("scoreboard"));
        assert_eq!(r["days"], json!(30));
        let rows = r["rows"].as_array().unwrap();
        assert!(rows.iter().all(|x| x["model_id"] != json!("model-b"))); // HAVING filtered
        let ma = rows.iter().find(|x| x["model_id"] == json!("model-a")).unwrap();
        assert_eq!(ma["template"], json!("card.discuss"));
        assert_eq!(ma["calls"], json!(7));
        assert_eq!(ma["fresh_ok"], json!(2));
        assert_eq!(ma["cached_hits"], json!(1));
        assert_eq!(ma["validation_fails"], json!(2));
        assert_eq!(ma["quality_flags"], json!(1));
        assert_eq!(ma["errors"], json!(2));
        assert_eq!(ma["avg_ms"], json!(125.0)); // ROUND(AVG(100..150), 0) over the 6 fresh rows
        assert_eq!(ma["clean_rate"], json!(0.429)); // (2+1)/(5+2) = 3/7 → toFixed(3)
        let mx = rows.iter().find(|x| x["model_id"] == json!("model-x")).unwrap();
        assert_eq!(mx["template"], json!("template#99"));
        assert_eq!(mx["clean_rate"], json!(0.75)); // (2+1)/(2+2) — trailing zeros collapse
        let mc = rows.iter().find(|x| x["model_id"] == json!("model-c")).unwrap();
        assert_eq!(mc["clean_rate"], Value::Null);
        assert_eq!(mc["avg_ms"], Value::Null);
        assert!((mc["spend_usd"].as_f64().unwrap()).abs() < 1e-12);
        // ORDER BY template ASC: card.discuss group before template#99
        let ti: Vec<&str> = rows.iter().map(|x| x["template"].as_str().unwrap()).collect();
        assert!(ti.iter().position(|t| *t == "card.discuss").unwrap() < ti.iter().position(|t| *t == "template#99").unwrap());
    }

    #[test]
    fn draws_fairness_per_tier_model() {
        let c = db();
        rec(&c, Some(1), "mid", Some("model-a"), 0, "success", None, "2025-12-28 10:00:00", 100, Some(0.01));
        rec(&c, Some(1), "mid", Some("model-a"), 0, "success", None, "2025-12-28 10:00:01", 100, Some(0.02));
        rec(&c, Some(1), "mid", Some("model-a"), 0, "error", None, "2025-12-29 10:00:00", 100, Some(0.005));
        rec(&c, Some(1), "mid", Some("model-b"), 1, "success", None, "2025-12-28 11:00:00", 5, None);
        rec(&c, Some(1), "low", Some("model-c"), 0, "success", None, "2025-12-30 09:00:00", 40, Some(0.5));
        // excluded: outside the 7-day window; null model_id
        rec(&c, Some(1), "mid", Some("model-a"), 0, "success", None, "2025-12-10 10:00:00", 100, Some(9.9));
        rec(&c, Some(1), "mid", None, 0, "success", None, "2025-12-28 12:00:00", 100, Some(9.9));

        let r = modelScoreboard(&json!({ "op": "draws", "days": 7 }), &c).unwrap();
        assert_eq!(r["op"], json!("draws"));
        assert_eq!(r["days"], json!(7));
        let rows = r["rows"].as_array().unwrap();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0]["tier"], json!("low")); // tier ASC, then calls DESC
        assert_eq!(rows[1]["model_id"], json!("model-a"));
        assert_eq!(rows[1]["calls"], json!(3));
        assert_eq!(rows[1]["served"], json!(2));
        assert_eq!(rows[1]["failovers"], json!(1));
        assert!((rows[1]["spend_usd"].as_f64().unwrap() - 0.035).abs() < 1e-12);
        assert_eq!(rows[2]["model_id"], json!("model-b")); // no HAVING in draws: 1 call stays
        assert_eq!(rows[2]["served"], json!(0));
        assert_eq!(rows[2]["cached_hits"], json!(1));
    }

    #[test]
    fn days_coercion_window_and_rendering() {
        let c = db();
        // inside 45d, outside 30d (frozen now = 2026-01-01)
        rec(&c, Some(1), "mid", Some("model-a"), 0, "success", None, "2025-11-20 10:00:00", 100, Some(0.001));
        rec(&c, Some(1), "mid", Some("model-a"), 0, "success", None, "2025-11-20 10:00:01", 102, Some(0.001));
        let d30 = modelScoreboard(&json!({}), &c).unwrap();
        assert_eq!(d30["rows"], json!([])); // default 30 hides them
        let d45 = modelScoreboard(&json!({ "days": "45" }), &c).unwrap();
        assert_eq!(d45["days"], json!(45)); // Number("45") — coerced, rendered integer
        assert_eq!(d45["rows"].as_array().unwrap().len(), 1);
        let frac = modelScoreboard(&json!({ "days": 7.5 }), &c).unwrap();
        assert_eq!(frac["days"], json!(7.5)); // un-floored in the output, floored in the SQL
        assert_eq!(frac["rows"], json!([]));
        let bad = modelScoreboard(&json!({ "days": -3, "op": "bogus" }), &c).unwrap();
        assert_eq!(bad["days"], json!(30)); // invalid days → default; unknown op → scoreboard
        assert_eq!(bad["op"], json!("scoreboard"));
        let none = modelScoreboard(&Value::Null, &c).unwrap();
        assert_eq!(none["days"], json!(30));
    }
}
