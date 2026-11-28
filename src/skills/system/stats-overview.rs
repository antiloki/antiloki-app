//! stats-overview.rs — twin of skills/system/stats-overview.mjs (db READ, cross-db
//! brain.*/memory.*). NAMES ARE CONTRACT: `statsOverview` (default export, async in JS — awaited
//! over sync SQLite reads per the async-default-export playbook rule, so the twin is an ordinary
//! sync fn). The substrate report card: one read-only pass over brain.ai_calls (30d totals,
//! outcome_class breakdown, paid-retry burn, per-tier split), cards-by-status + shipped count,
//! heimdall tagging coverage, memory.error_events recurrence grouping, a 14-day brain.ai_calls
//! series for the sparkline, and deterministic prose insights derived from those numbers (no AI).
//!
//! Frozen clock: JS uses `datetime('now','-30 days')` / `datetime('now','-14 days')`; the conform
//! JS lane pins 'now' to 2026-01-01 00:00:00, so this twin writes the frozen literal in both
//! spots. The JS `if (!db) throw err.contextMissing(...)` guard is unreachable in the db lane (a
//! connection is always injected), so it is not transcribed.
// QUALITY: 80/100 — 8 aggregate queries + 6 deterministic insight sentences derived from them;
// the only sharp edge is SQLite ROUND()/SUM() always returning REAL, collapsed via sql_num so
// whole-valued numbers render "5" not "5.0" like JS, both in the output object and in the prose.

use rusqlite::Connection;
use serde_json::{json, Map, Value};

fn now_ts() -> String { crate::clock::now_ts_nodb() }

fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) } // JS `??` right operand
fn str_qq(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(),
        Some(o) => o.to_string(),
    }
} // = JS String(v ?? '')
fn tmpl(v: Option<&Value>) -> String { match v { None => "undefined".into(), Some(Value::Null) => "null".into(), Some(x) => str_qq(Some(x)) } } // = JS `${v}`
fn js_number(v: Option<&Value>) -> f64 {
    match v {
        None => f64::NAN,
        Some(Value::Null) => 0.0,
        Some(Value::Bool(b)) => if *b { 1.0 } else { 0.0 },
        Some(Value::Number(n)) => n.as_f64().unwrap_or(f64::NAN),
        Some(Value::String(s)) => { let t = s.trim(); if t.is_empty() { 0.0 } else { t.parse().unwrap_or(f64::NAN) } }
        Some(_) => f64::NAN,
    }
}
// JS Math.round: nearest integer, ties toward +Infinity.
fn js_round(x: f64) -> f64 { let f = x.floor(); if x - f >= 0.5 { f + 1.0 } else { f } }

// SQLite ROUND()/SUM() over a REAL column always return REAL, but JS never distinguishes 5 from
// 5.0 — collapse a whole-valued number to an integer JSON representation (matching JS output,
// both in the returned object and when interpolated into an insight sentence via `tmpl`), keep
// NULL as null (an aggregate over zero matching rows), and keep genuine fractionals as f64.
fn sql_num(v: Option<&Value>) -> Value {
    match v {
        None | Some(Value::Null) => Value::Null,
        Some(x) => {
            let f = js_number(Some(x));
            if f.is_finite() && f.fract() == 0.0 && f.abs() < 9e15 { json!(f as i64) } else { json!(f) }
        }
    }
}

// `const q = (sql, ...args) => { try { return db.prepare(sql).all(...args); } catch { return []; } }`
fn q(db: &Connection, sql: &str) -> Vec<Value> {
    match crate::db_json::query_rows(db, sql, &[]) {
        Ok(v) => v.as_array().cloned().unwrap_or_default(),
        Err(_) => Vec::new(),
    }
}
// `const one = (sql, ...args) => { try { return db.prepare(sql).get(...args) ?? {}; } catch { return {}; } }`
fn one(db: &Connection, sql: &str) -> Value {
    match crate::db_json::query_rows(db, sql, &[]) {
        Ok(v) => v.as_array().and_then(|a| a.first().cloned()).unwrap_or_else(|| json!({})),
        Err(_) => json!({}),
    }
}
// `row.key ?? 0` for a COUNT(*)-shaped aggregate, cast to i64 (COUNT/SUM(CASE) results are
// always integer-typed once a row exists).
fn count_or0(row: &Value, key: &str) -> i64 {
    match nz(row.get(key)) { Some(v) => js_number(Some(v)) as i64, None => 0 }
}
// Replace column `col` in every row object with sql_num(value) — the row came from a query whose
// `col` is a ROUND()/REAL SQL result, so this fixes its JSON rendering to match JS. `Map::insert`
// on an already-present key keeps its position (JS object property order is preserved).
fn round_col(rows: Vec<Value>, col: &str) -> Vec<Value> {
    rows.into_iter()
        .map(|r| match r {
            Value::Object(mut m) => {
                if let Some(v) = m.get(col).cloned() { m.insert(col.to_string(), sql_num(Some(&v))); }
                Value::Object(m)
            }
            other => other,
        })
        .collect()
}

pub fn statsOverview(_input: &Value, db: &Connection) -> Result<Value, String> {
    let now = now_ts();
    // ── AI usage (30d): the engine room. `cost_usd` is METERED spend only: a `cli` row carries the
    // engine's API-equivalent cost for reference, but the subscription already paid for it.
    let mut ai_tot = one(db, &format!(
        "SELECT COUNT(*) AS calls, SUM(cached) AS cached, \
         ROUND(SUM(CASE WHEN provider = 'cli' THEN 0 ELSE COALESCE(cost_usd_actual,0) END),4) AS cost_usd, \
         SUM(COALESCE(tokens_in,0)) AS tokens_in, SUM(COALESCE(tokens_out,0)) AS tokens_out, \
         ROUND(AVG(duration_ms)) AS avg_ms \
         FROM brain.ai_calls WHERE ts >= datetime('{now}','-30 days')"
    ));
    if let Value::Object(m) = &mut ai_tot {
        if let Some(v) = m.get("cost_usd").cloned() { m.insert("cost_usd".to_string(), sql_num(Some(&v))); }
        if let Some(v) = m.get("avg_ms").cloned() { m.insert("avg_ms".to_string(), sql_num(Some(&v))); }
    }
    let ai_outcomes = q(db, &format!(
        "SELECT COALESCE(outcome_class,'(unclassified)') AS outcome_class, COUNT(*) AS n \
         FROM brain.ai_calls WHERE ts >= datetime('{now}','-30 days') AND cached = 0 \
         GROUP BY outcome_class ORDER BY n DESC"
    ));
    let retries = one(db, &format!(
        "SELECT COUNT(*) AS attempts_beyond_first FROM brain.ai_call_attempts \
         WHERE attempt_no > 1 AND ts >= datetime('{now}','-30 days')"
    ));
    let by_tier = round_col(
        q(db, &format!(
            "SELECT tier, COUNT(*) AS n, ROUND(SUM(COALESCE(cost_usd_actual,0)),4) AS cost_usd \
             FROM brain.ai_calls WHERE ts >= datetime('{now}','-30 days') AND cached = 0 GROUP BY tier ORDER BY n DESC"
        )),
        "cost_usd",
    );
    // BY ENGINE (operator 2026-09-19: "statistics should measure the claude, codex and openrouter AIs we
    // set as default"): which provider/model answered, how often, how fast, how much of it was metered
    // (a cli row is a subscription — never $/call; only openrouter rows carry a cost).
    let by_engine = round_col(
        q(db, &format!(
            "SELECT COALESCE(provider,'?') AS provider, COALESCE(model_id,'?') AS model_id, COUNT(*) AS n, \
                    ROUND(AVG(duration_ms)) AS avg_ms, ROUND(SUM(COALESCE(cost_usd_actual,0)),4) AS cost_usd, \
                    SUM(CASE WHEN outcome = 'success' THEN 1 ELSE 0 END) AS ok_n, \
                    CASE WHEN provider = 'cli' THEN 'subscription' WHEN provider IN ('ollama','local') THEN 'local' WHEN provider = 'openrouter' THEN 'metered' ELSE 'other' END AS billing \
             FROM brain.ai_calls WHERE ts >= datetime('{now}','-30 days') AND cached = 0 GROUP BY provider, model_id ORDER BY n DESC"
        )),
        "cost_usd",
    );

    // ── the board: what all this AI actually produced ──
    let cards_by_status = q(db, "SELECT status, COUNT(*) AS n FROM cards GROUP BY status ORDER BY n DESC");
    let shipped_30d = one(db, &format!(
        "SELECT COUNT(*) AS n FROM cards WHERE status = 'shipped' AND updated_at >= datetime('{now}','-30 days')"
    ));

    // ── tagging coverage: how much of the traffic has a quality verdict ──
    let taggable_row = one(db,
        "SELECT COUNT(*) AS n FROM brain.ai_calls a JOIN brain.heimdall_castings c ON c.ai_call_id = a.id \
         WHERE a.cached = 0 AND c.template_base != 'heimdall.ai.judge'"
    );
    let taggable = count_or0(&taggable_row, "n");
    let human_tags = count_or0(&one(db, "SELECT COUNT(*) AS n FROM brain.heimdall_human_tags"), "n");
    let ai_tags = count_or0(&one(db, "SELECT COUNT(*) AS n FROM brain.heimdall_ai_tags WHERE superseded_by_human = 0"), "n");
    let prompt_tags = count_or0(&one(db, "SELECT COUNT(*) AS n FROM brain.heimdall_prompt_tags"), "n");
    let evidence = count_or0(&one(db, "SELECT COUNT(*) AS n FROM brain.heimdall_evidence"), "n");

    // ── recorded errors, grouped by recurrence pattern ──
    let error_patterns = q(db,
        "SELECT COALESCE(pattern_key, error_code, '(untyped)') AS pattern, COUNT(*) AS n, \
         SUM(CASE WHEN recovery_outcome IS NOT NULL THEN 1 ELSE 0 END) AS recovered, MAX(ts) AS last_ts \
         FROM memory.error_events GROUP BY pattern ORDER BY n DESC LIMIT 10"
    );

    // ── 14-day series for the sparkline ──
    let days = round_col(
        q(db, &format!(
            "SELECT substr(ts,1,10) AS day, COUNT(*) AS calls, \
             SUM(CASE WHEN outcome_class IS NOT NULL AND outcome_class NOT IN ('clean','cached') THEN 1 ELSE 0 END) AS failed, \
             ROUND(SUM(COALESCE(cost_usd_actual,0)),4) AS cost \
             FROM brain.ai_calls WHERE ts >= datetime('{now}','-14 days') AND cached = 0 \
             GROUP BY day ORDER BY day"
        )),
        "cost",
    );

    // ── deterministic insights: sentences the numbers already prove ──
    let mut insights: Vec<Value> = Vec::new();
    let calls30 = count_or0(&ai_tot, "calls");
    let failed_n: i64 = ai_outcomes
        .iter()
        .filter(|o| {
            let oc = o.get("outcome_class").and_then(|v| v.as_str()).unwrap_or("");
            !["clean", "cached", "(unclassified)"].contains(&oc)
        })
        .map(|o| o.get("n").and_then(|v| v.as_i64()).unwrap_or(0))
        .sum();
    let cached_n = count_or0(&ai_tot, "cached");
    let live_n = (calls30 - cached_n).max(1);
    let cost_usd_v = nz(ai_tot.get("cost_usd")).cloned().unwrap_or(json!(0));
    let pct_free = js_round((cached_n as f64 / calls30.max(1) as f64) * 100.0) as i64;
    let sub_n: i64 = by_engine.iter().filter(|e| e.get("billing") == Some(&json!("subscription"))).map(|e| e.get("n").and_then(Value::as_i64).unwrap_or(0)).sum();
    insights.push(json!(format!(
        "{} AI calls in 30d ({} served from cache — {}% free){}; metered spend ${}.",
        calls30, cached_n, pct_free,
        if sub_n > 0 { format!(", {sub_n} on your subscriptions") } else { String::new() },
        tmpl(Some(&cost_usd_v))
    )));

    if failed_n > 0 {
        let pct2 = js_round((failed_n as f64 / live_n as f64) * 100.0) as i64;
        let worst_outcome = ai_outcomes.iter().find(|o| {
            let oc = o.get("outcome_class").and_then(|v| v.as_str()).unwrap_or("");
            !["clean", "cached", "(unclassified)"].contains(&oc)
        });
        let worst_str = match worst_outcome.and_then(|o| o.get("outcome_class")) {
            None => "undefined".to_string(),
            Some(v) => tmpl(Some(v)),
        };
        insights.push(json!(format!(
            "{}/{} live calls ({}%) hit a failure class — the top one is '{}'.",
            failed_n, live_n, pct2, worst_str
        )));
    }

    let attempts_beyond_first = count_or0(&retries, "attempts_beyond_first");
    if attempts_beyond_first > 0 {
        insights.push(json!(format!(
            "{} paid retry attempts in 30d — every one is a prompt or model defect worth a tag.",
            attempts_beyond_first
        )));
    }

    if taggable > 0 {
        let pct3 = js_round(((human_tags + ai_tags) as f64 / taggable as f64) * 100.0) as i64;
        insights.push(json!(format!(
            "quality verdicts cover {}/{} taggable calls ({}%) — {} human · {} AI.",
            human_tags + ai_tags, taggable, pct3, human_tags, ai_tags
        )));
    }

    if let Some(worst_pattern) = error_patterns.first() {
        let n = worst_pattern.get("n").and_then(|v| v.as_i64()).unwrap_or(0);
        if n > 1 {
            insights.push(json!(format!(
                "the most recurrent error pattern is '{}' ({}×, {} recovered).",
                tmpl(worst_pattern.get("pattern")), n, tmpl(worst_pattern.get("recovered"))
            )));
        }
    }

    if prompt_tags == 0 {
        insights.push(json!("no prompt-design tags yet — the PROMPTS tab on /tagging is untouched territory."));
    }

    // ── assemble the report ──
    let mut ai_obj = match &ai_tot { Value::Object(m) => m.clone(), _ => Map::new() };
    ai_obj.insert("outcomes".to_string(), json!(ai_outcomes));
    ai_obj.insert("retries_beyond_first".to_string(), nz(retries.get("attempts_beyond_first")).cloned().unwrap_or(json!(0)));
    ai_obj.insert("by_tier".to_string(), json!(by_tier));
    ai_obj.insert("by_engine".to_string(), json!(by_engine));

    let mut out = Map::new();
    out.insert("ok".to_string(), json!(true));
    out.insert("ai".to_string(), Value::Object(ai_obj));
    out.insert("cards".to_string(), json!({
        "by_status": cards_by_status,
        "shipped_30d": nz(shipped_30d.get("n")).cloned().unwrap_or(json!(0)),
    }));
    out.insert("tagging".to_string(), json!({
        "taggable": taggable,
        "human_tags": human_tags,
        "ai_tags": ai_tags,
        "prompt_tags": prompt_tags,
        "evidence": evidence,
    }));
    out.insert("errors".to_string(), json!({ "patterns": error_patterns }));
    out.insert("days".to_string(), json!(days));
    out.insert("insights".to_string(), json!(insights));
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "ATTACH ':memory:' AS brain; ATTACH ':memory:' AS memory; \
             CREATE TABLE brain.ai_calls (id INTEGER PRIMARY KEY, ts TEXT, cost_usd_actual REAL, tokens_in INTEGER, tokens_out INTEGER, duration_ms REAL, cached INTEGER, outcome_class TEXT, tier TEXT, provider TEXT, model_id TEXT, outcome TEXT); \
             CREATE TABLE brain.ai_call_attempts (id INTEGER PRIMARY KEY, ai_call_id INTEGER, attempt_no INTEGER, ts TEXT); \
             CREATE TABLE brain.heimdall_castings (id INTEGER PRIMARY KEY, ai_call_id INTEGER, template_base TEXT); \
             CREATE TABLE brain.heimdall_human_tags (id INTEGER PRIMARY KEY); \
             CREATE TABLE brain.heimdall_ai_tags (id INTEGER PRIMARY KEY, superseded_by_human INTEGER); \
             CREATE TABLE brain.heimdall_prompt_tags (id INTEGER PRIMARY KEY); \
             CREATE TABLE brain.heimdall_evidence (id INTEGER PRIMARY KEY); \
             CREATE TABLE memory.error_events (id INTEGER PRIMARY KEY, ts TEXT, pattern_key TEXT, error_code TEXT, recovery_outcome TEXT); \
             CREATE TABLE cards (slug TEXT PRIMARY KEY, status TEXT, updated_at TEXT);",
        )
        .unwrap();
        c
    }

    #[allow(clippy::too_many_arguments)]
    fn call(c: &Connection, id: i64, ts: &str, cost: f64, tin: i64, tout: i64, ms: i64, cached: i64, oc: Option<&str>, tier: Option<&str>) {
        c.execute(
            "INSERT INTO brain.ai_calls (id, ts, cost_usd_actual, tokens_in, tokens_out, duration_ms, cached, outcome_class, tier) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
            rusqlite::params![id, ts, cost, tin, tout, ms, cached, oc, tier],
        )
        .unwrap();
    }

    #[test]
    fn report_card_aggregates_tagging_errors_days_and_insights() {
        let c = db();
        // 30d window anchored to the frozen now_ts(); -40d falls outside both the 30d and 14d windows.
        call(&c, 1, "2025-12-27 00:00:00", 1.0, 1000, 200, 100, 0, Some("clean"), Some("mid"));
        call(&c, 2, "2025-12-27 00:00:00", 2.0, 500, 100, 200, 0, Some("schema_invalid"), Some("high"));
        call(&c, 3, "2025-12-27 00:00:00", 0.5, 200, 50, 150, 1, Some("clean"), Some("mid"));
        call(&c, 4, "2025-11-22 00:00:00", 100.0, 999, 999, 999, 0, Some("clean"), Some("low"));
        call(&c, 5, "2025-12-30 00:00:00", 1.5, 300, 60, 130, 0, None, Some("mid"));

        c.execute_batch(
            "INSERT INTO brain.ai_call_attempts (id, ai_call_id, attempt_no, ts) VALUES (1,1,1,'2025-12-27 00:00:00'); \
             INSERT INTO brain.ai_call_attempts (id, ai_call_id, attempt_no, ts) VALUES (2,1,2,'2025-12-27 00:00:00'); \
             INSERT INTO brain.ai_call_attempts (id, ai_call_id, attempt_no, ts) VALUES (3,2,2,'2025-11-22 00:00:00'); \
             INSERT INTO brain.ai_call_attempts (id, ai_call_id, attempt_no, ts) VALUES (4,2,3,'2025-12-27 00:00:00'); \
             INSERT INTO brain.heimdall_castings (id, ai_call_id, template_base) VALUES (1,1,'heimdall.tag.assist'); \
             INSERT INTO brain.heimdall_castings (id, ai_call_id, template_base) VALUES (2,2,'heimdall.ai.judge'); \
             INSERT INTO brain.heimdall_castings (id, ai_call_id, template_base) VALUES (3,3,'heimdall.tag.assist'); \
             INSERT INTO brain.heimdall_castings (id, ai_call_id, template_base) VALUES (4,5,'heimdall.tag.assist'); \
             INSERT INTO brain.heimdall_human_tags (id) VALUES (1); \
             INSERT INTO brain.heimdall_ai_tags (id, superseded_by_human) VALUES (1,0); \
             INSERT INTO brain.heimdall_ai_tags (id, superseded_by_human) VALUES (2,0); \
             INSERT INTO brain.heimdall_ai_tags (id, superseded_by_human) VALUES (3,1); \
             INSERT INTO brain.heimdall_evidence (id) VALUES (1); \
             INSERT INTO brain.heimdall_evidence (id) VALUES (2); \
             INSERT INTO brain.heimdall_evidence (id) VALUES (3); \
             INSERT INTO memory.error_events (id, ts, pattern_key, error_code, recovery_outcome) VALUES (1,'2025-12-27 00:00:00','card_not_found',NULL,'ok'); \
             INSERT INTO memory.error_events (id, ts, pattern_key, error_code, recovery_outcome) VALUES (2,'2025-12-27 00:00:00','card_not_found',NULL,NULL); \
             INSERT INTO memory.error_events (id, ts, pattern_key, error_code, recovery_outcome) VALUES (3,'2025-12-27 00:00:00','card_not_found',NULL,'ok'); \
             INSERT INTO memory.error_events (id, ts, pattern_key, error_code, recovery_outcome) VALUES (4,'2025-12-27 00:00:00',NULL,'TIMEOUT',NULL); \
             INSERT INTO memory.error_events (id, ts, pattern_key, error_code, recovery_outcome) VALUES (5,'2025-12-27 00:00:00',NULL,NULL,NULL); \
             INSERT INTO cards (slug, status, updated_at) VALUES ('A','shipped','2025-12-27 00:00:00'); \
             INSERT INTO cards (slug, status, updated_at) VALUES ('B','shipped','2025-11-22 00:00:00'); \
             INSERT INTO cards (slug, status, updated_at) VALUES ('C','doing','2025-12-27 00:00:00'); \
             INSERT INTO cards (slug, status, updated_at) VALUES ('D','backlog','2025-12-27 00:00:00');",
        )
        .unwrap();

        let r = statsOverview(&json!({}), &c).unwrap();
        assert_eq!(r["ok"], json!(true));

        assert_eq!(r["ai"]["calls"], json!(4));
        assert_eq!(r["ai"]["cached"], json!(1));
        assert_eq!(r["ai"]["cost_usd"], json!(5));
        assert_eq!(r["ai"]["tokens_in"], json!(2000));
        assert_eq!(r["ai"]["tokens_out"], json!(410));
        assert_eq!(r["ai"]["avg_ms"], json!(145));
        assert_eq!(r["ai"]["retries_beyond_first"], json!(2));
        assert_eq!(r["ai"]["outcomes"].as_array().unwrap().iter().map(|o| o["n"].as_i64().unwrap()).sum::<i64>(), 3);
        assert!(r["ai"]["by_tier"].as_array().unwrap().iter().any(|t| t["tier"] == json!("mid")));
        assert!(!r["ai"]["by_tier"].as_array().unwrap().iter().any(|t| t["tier"] == json!("low")));

        assert_eq!(
            r["cards"]["by_status"].as_array().unwrap().iter().find(|c| c["status"] == json!("shipped")).unwrap(),
            &json!({ "status": "shipped", "n": 2 })
        );
        assert_eq!(r["cards"]["shipped_30d"], json!(1));

        assert_eq!(r["tagging"], json!({ "taggable": 2, "human_tags": 1, "ai_tags": 2, "prompt_tags": 0, "evidence": 3 }));

        let worst = &r["errors"]["patterns"][0];
        assert_eq!(worst["pattern"], json!("card_not_found"));
        assert_eq!(worst["n"], json!(3));
        assert_eq!(worst["recovered"], json!(2));

        assert_eq!(r["days"].as_array().unwrap().iter().map(|d| d["calls"].as_i64().unwrap()).sum::<i64>(), 3);

        let insights: Vec<String> = r["insights"].as_array().unwrap().iter().map(|s| s.as_str().unwrap().to_string()).collect();
        assert_eq!(insights[0], "4 AI calls in 30d (1 served from cache — 25% free); metered spend $5.");
        assert!(insights.iter().any(|s| s == "1/3 live calls (33%) hit a failure class — the top one is 'schema_invalid'."));
        assert!(insights.iter().any(|s| s == "2 paid retry attempts in 30d — every one is a prompt or model defect worth a tag."));
        assert!(insights.iter().any(|s| s == "quality verdicts cover 3/2 taggable calls (150%) — 1 human · 2 AI."));
        assert!(insights.iter().any(|s| s == "the most recurrent error pattern is 'card_not_found' (3×, 2 recovered)."));
        assert!(insights.iter().any(|s| s == "no prompt-design tags yet — the PROMPTS tab on /tagging is untouched territory."));
    }

    #[test]
    fn empty_tables_null_safe_defaults() {
        let c = db();
        let r = statsOverview(&json!({}), &c).unwrap();
        assert_eq!(r["ai"]["calls"], json!(0));
        assert_eq!(r["ai"]["cached"], Value::Null);
        assert_eq!(r["ai"]["cost_usd"], Value::Null);
        assert_eq!(r["ai"]["avg_ms"], Value::Null);
        assert_eq!(r["ai"]["outcomes"], json!([]));
        assert_eq!(r["ai"]["retries_beyond_first"], json!(0));
        assert_eq!(r["cards"], json!({ "by_status": [], "shipped_30d": 0 }));
        assert_eq!(r["tagging"], json!({ "taggable": 0, "human_tags": 0, "ai_tags": 0, "prompt_tags": 0, "evidence": 0 }));
        assert_eq!(r["errors"], json!({ "patterns": [] }));
        assert_eq!(r["days"], json!([]));
        assert_eq!(
            r["insights"],
            json!([
                "0 AI calls in 30d (0 served from cache — 0% free); metered spend $0.",
                "no prompt-design tags yet — the PROMPTS tab on /tagging is untouched territory.",
            ])
        );
    }

    #[test]
    fn worst_pattern_single_occurrence_and_taggable_zero_skip_their_insights() {
        let c = db();
        call(&c, 1, "2025-12-27 00:00:00", 3.0, 100, 20, 100, 0, Some("clean"), Some("mid"));
        c.execute(
            "INSERT INTO memory.error_events (id, ts, pattern_key, recovery_outcome) VALUES (1,'2025-12-27 00:00:00','only-once',NULL)",
            [],
        )
        .unwrap();
        c.execute("INSERT INTO brain.heimdall_prompt_tags (id) VALUES (1)", []).unwrap();

        let r = statsOverview(&json!({}), &c).unwrap();
        let insights: Vec<String> = r["insights"].as_array().unwrap().iter().map(|s| s.as_str().unwrap().to_string()).collect();
        // failedN=0 (only 'clean'), retries=0, taggable=0, worstPattern.n=1 (not >1), prompt_tags=1
        // (not 0) — every conditional insight is skipped, leaving only the always-on first line.
        assert_eq!(insights, vec!["1 AI calls in 30d (0 served from cache — 0% free); metered spend $3.".to_string()]);
        assert_eq!(r["errors"]["patterns"][0]["n"], json!(1));
    }

    #[test]
    fn a_subscription_row_counts_as_a_call_but_never_as_metered_spend() {
        let c = db();
        call(&c, 1, "2026-01-10 10:00:00", 0.0, 10, 5, 100, 0, Some("clean"), Some("mid"));
        c.execute("UPDATE brain.ai_calls SET provider = 'cli', model_id = 'claude-code:opus', cost_usd_actual = 0.2 WHERE id = 1", []).unwrap();
        call(&c, 2, "2026-01-10 11:00:00", 3.0, 10, 5, 100, 0, Some("clean"), Some("mid"));
        c.execute("UPDATE brain.ai_calls SET provider = 'openrouter' WHERE id = 2", []).unwrap();
        let r = statsOverview(&json!({ "now": "2026-01-11 00:00:00" }), &c).unwrap();
        let insights: Vec<String> = r["insights"].as_array().unwrap().iter().map(|v| v.as_str().unwrap().to_string()).collect();
        assert_eq!(insights[0], "2 AI calls in 30d (0 served from cache — 0% free), 1 on your subscriptions; metered spend $3.");
        assert_eq!(r["ai"]["cost_usd"].as_f64(), Some(3.0));
    }
}
