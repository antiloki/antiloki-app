//! bandit-pick.rs — twin of skills/ai/bandit-pick.mjs (db READ, cross-db brain.*). NAMES ARE
//! CONTRACT: `banditPick` (default). Empirical top-1 for cooking-OFF: reorder providers best-first
//! by Laplace-smoothed clean-rate ((ok+1)/(attempts+2)) from brain.ai_calls history; never-seen
//! models ride right after the leader; null = keep the configured slot order.
// QUALITY: 84/100 — smoothed ranking + quality-flag demotion + 30-day window + fresh-after-leader
use rusqlite::Connection;
use serde_json::Value;

fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}

// QUALITY FLAGS count as bad (r24): a 'success' whose output a guard rejected must not rank as a
// win. JS says datetime('now', '-30 days'); crate::clock decides whether 'now' is the wall clock
// (production) or the pinned instant (tests / ANTILOKI_FROZEN_CLOCK), so this is a format! rather
// than a plain const.
fn stats_sql() -> String {
    format!("
      SELECT a.model_id,
             SUM(CASE WHEN a.outcome = 'success'
                       AND COALESCE(a.outcome_class, '') NOT IN ('degenerate', 'truncated', 'validation_failed')
                      THEN 1 ELSE 0 END) AS ok,
             SUM(CASE WHEN a.outcome != 'success'
                       OR COALESCE(a.outcome_class, '') IN ('degenerate', 'truncated', 'validation_failed')
                      THEN 1 ELSE 0 END) AS bad
      FROM brain.ai_calls a
      JOIN brain.prompt_templates p ON p.id = a.prompt_template_id
      WHERE p.name = ? AND a.tier = ? AND a.model_id IS NOT NULL AND a.cached = 0
        AND a.ts >= datetime({NOW}, '-30 days')
      GROUP BY a.model_id
    ",
        NOW = crate::clock::now_sql_inner()
    )
}

pub fn banditPick(input: &Value, db: &Connection) -> Result<Value, String> {
    let prompt_name = input.get("prompt_name");
    let tier = input.get("tier");
    let providers = input.get("providers").and_then(|v| v.as_array());
    if !truthy(prompt_name) || !truthy(tier) || providers.map_or(true, |p| p.len() < 2) {
        return Ok(Value::Null);
    }
    if std::env::var("ANTILOKI_BANDIT").map_or(false, |v| v == "off") {
        return Ok(Value::Null);
    }
    let providers = providers.unwrap();

    let stats = match crate::db_json::query_rows(
        db,
        &stats_sql(),
        &[prompt_name.cloned().unwrap_or(Value::Null), tier.cloned().unwrap_or(Value::Null)],
    ) {
        Ok(v) => v.as_array().cloned().unwrap_or_default(),
        Err(_) => return Ok(Value::Null), // JS try/catch → null (e.g. brain.* tables absent)
    };

    let counts = |r: &Value| -> (i64, i64) {
        (
            r.get("ok").and_then(|v| v.as_i64()).unwrap_or(0),
            r.get("bad").and_then(|v| v.as_i64()).unwrap_or(0),
        )
    };
    let total: i64 = stats.iter().map(|r| { let (ok, bad) = counts(r); ok + bad }).sum();
    if total == 0 {
        return Ok(Value::Null); // no history — the configured slot order stands
    }

    // GROUP BY model_id → keys are unique; a.model_id IS NOT NULL → keys are never null.
    let rate: Vec<(Value, f64)> = stats
        .iter()
        .map(|r| {
            let (ok, bad) = counts(r);
            let attempts = ok + bad;
            (r.get("model_id").cloned().unwrap_or(Value::Null), (ok + 1) as f64 / (attempts + 2) as f64)
        })
        .collect();
    let rate_of = |p: &Value| -> Option<f64> {
        let mid = p.get("model_id")?; // missing model_id → Map.has(undefined) → false → fresh
        rate.iter().find(|(k, _)| k == mid).map(|(_, v)| *v)
    };

    let mut known: Vec<Value> = providers.iter().filter(|p| rate_of(p).is_some()).cloned().collect();
    let fresh: Vec<Value> = providers.iter().filter(|p| rate_of(p).is_none()).cloned().collect();
    // JS .sort((a,b) => rate(b) - rate(a)): descending, stable on ties (rates are always finite).
    known.sort_by(|a, b| {
        rate_of(b).unwrap().partial_cmp(&rate_of(a).unwrap()).unwrap_or(std::cmp::Ordering::Equal)
    });

    let mut out: Vec<Value> = Vec::new();
    out.extend(known.iter().take(1).cloned());
    out.extend(fresh);
    out.extend(known.into_iter().skip(1));
    Ok(Value::Array(out))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "ATTACH DATABASE ':memory:' AS brain; \
             CREATE TABLE brain.prompt_templates (id INTEGER PRIMARY KEY, name TEXT NOT NULL); \
             CREATE TABLE brain.ai_calls (id INTEGER PRIMARY KEY, prompt_template_id INTEGER, tier TEXT, \
               model_id TEXT, cached INTEGER, outcome TEXT, outcome_class TEXT, ts TEXT); \
             INSERT INTO brain.prompt_templates (id, name) VALUES (1, 'bandit.test');",
        )
        .unwrap();
        c
    }
    fn rec(c: &Connection, model: &str, outcome: &str, class: Option<&str>, cached: i64, ts: &str) {
        c.execute(
            "INSERT INTO brain.ai_calls (prompt_template_id, tier, model_id, cached, outcome, outcome_class, ts) \
             VALUES (1, 'mid', ?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![model, cached, outcome, class, ts],
        )
        .unwrap();
    }
    fn providers() -> Value {
        json!([
            { "provider": "openrouter", "model_id": "model-a" },
            { "provider": "openrouter", "model_id": "model-b" },
            { "provider": "openrouter", "model_id": "model-c" }
        ])
    }
    fn pick(c: &Connection, providers: Value) -> Value {
        banditPick(&json!({ "prompt_name": "bandit.test", "tier": "mid", "providers": providers }), c).unwrap()
    }

    #[test]
    fn no_history_and_guards_return_null() {
        let c = db();
        assert_eq!(pick(&c, providers()), Value::Null); // no history — slot order stands
        rec(&c, "model-a", "success", None, 0, "2025-12-20 10:00:00");
        let single = json!([{ "provider": "openrouter", "model_id": "model-a" }]);
        assert_eq!(pick(&c, single), Value::Null); // < 2 providers
        assert_eq!(banditPick(&json!({ "tier": "mid", "providers": providers() }), &c).unwrap(), Value::Null);
        assert_eq!(banditPick(&Value::Null, &c).unwrap(), Value::Null);
    }

    #[test]
    fn best_clean_rate_leads_fresh_rides_after_leader() {
        let c = db();
        for i in 0..3 { rec(&c, "model-a", "success", None, 0, &format!("2025-12-20 10:00:0{i}")) }
        rec(&c, "model-b", "success", None, 0, "2025-12-20 11:00:00");
        for i in 0..3 { rec(&c, "model-b", "error", None, 0, &format!("2025-12-20 12:00:0{i}")) }
        let out = pick(&c, providers());
        let ids: Vec<&str> = out.as_array().unwrap().iter().map(|p| p["model_id"].as_str().unwrap()).collect();
        assert_eq!(ids, ["model-a", "model-c", "model-b"]); // fresh c before the proven loser b
    }

    #[test]
    fn quality_flagged_success_counts_as_bad() {
        let c = db();
        for i in 0..2 { rec(&c, "model-a", "success", None, 0, &format!("2025-12-20 10:00:0{i}")) }
        rec(&c, "model-b", "success", Some("degenerate"), 0, "2025-12-20 11:00:00");
        rec(&c, "model-b", "success", Some("truncated"), 0, "2025-12-20 11:00:01");
        let out = pick(&c, providers());
        assert_eq!(out[0]["model_id"], json!("model-a"));
        assert_eq!(out[2]["model_id"], json!("model-b"));
    }

    #[test]
    fn window_and_cached_rows_are_excluded() {
        let c = db();
        // b: 5 stale wins (before 2025-12-02 cutoff) + 1 cached win — all invisible; 1 fresh error.
        for i in 0..5 { rec(&c, "model-b", "success", None, 0, &format!("2025-10-01 10:00:0{i}")) }
        rec(&c, "model-b", "success", None, 1, "2025-12-20 09:00:00");
        rec(&c, "model-b", "error", None, 0, "2025-12-20 09:30:00");
        rec(&c, "model-a", "success", None, 0, "2025-12-20 10:00:00");
        let out = pick(&c, providers());
        let ids: Vec<&str> = out.as_array().unwrap().iter().map(|p| p["model_id"].as_str().unwrap()).collect();
        assert_eq!(ids, ["model-a", "model-c", "model-b"]);
    }
}
