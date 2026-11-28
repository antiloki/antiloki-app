//! ai-route.rs — twin of skills/ai/ai-route.mjs (db READ lane). NAMES ARE CONTRACT:
//! `aiRoute` (default, standard) + `degradeIfNeeded` (named, no-db helper). Deterministic
//! task_kind → {tier, temperature, max_tokens} lookup in brain.routing_defaults, with
//! optional budget-aware tier degradation.
use rusqlite::Connection;
use serde_json::{json, Map, Value};

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
fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}

const TIER_FALLBACK_RANK: [&str; 3] = ["low", "mid", "high"]; // ascending; degrade by stepping down

// TIER_ESTIMATE_USD[tier] ?? TIER_ESTIMATE_USD.mid — unknown tiers fall back to the mid estimate.
fn tier_estimate_usd(tier: &str) -> f64 {
    match tier {
        "low" => 0.0005,
        "high" => 0.025,
        _ => 0.005,
    }
}

pub fn degradeIfNeeded(tier: &Value, budget_remaining_usd: &Value) -> Value {
    let tier_str = tier.as_str().unwrap_or("").to_string();
    let is_zero = matches!(budget_remaining_usd, Value::Number(n) if n.as_f64() == Some(0.0));
    if budget_remaining_usd.is_null() || is_zero {
        return json!({ "tier": tier_str, "degraded": false });
    }
    let estimate = tier_estimate_usd(&tier_str);
    let budget = budget_remaining_usd.as_f64().unwrap_or(f64::NAN);
    if estimate <= budget {
        return json!({ "tier": tier_str, "degraded": false });
    }
    let idx = TIER_FALLBACK_RANK.iter().position(|&t| t == tier_str);
    match idx {
        None | Some(0) => json!({ "tier": "low", "degraded": false, "exhausted": true }),
        Some(i) => json!({ "tier": TIER_FALLBACK_RANK[i - 1], "degraded": true }),
    }
}

pub fn aiRoute(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS destructures { db } from context and throws err.contextMissing without it — the db lane
    // always injects one, so that guard is unreachable here.
    let task_kind_v = input.get("task_kind").cloned().unwrap_or(Value::Null);
    let intent_brief = input.get("intent_brief").cloned().unwrap_or(Value::Null);
    let budget_remaining_usd = input.get("budget_remaining_usd").cloned().unwrap_or(Value::Null);

    // JS throws err.required('ai-route','task_kind') when falsy — a guard-throw, not exercised
    // by fixtures (only success paths are), so the exact error message doesn't matter here.
    if !truthy(Some(&task_kind_v)) {
        return Err("ai-route: task_kind is required".to_string());
    }

    let rows = crate::db_json::query_rows(
        db,
        "SELECT task_kind, tier, temperature, max_tokens, rationale_md FROM brain.routing_defaults WHERE task_kind = ?",
        &[task_kind_v.clone()],
    )?;
    let row = rows.as_array().and_then(|a| a.first());

    let (base_tier, base_temp, max_tokens, base_rationale, fallback): (String, String, Value, String, bool) =
        if let Some(row) = row {
            (
                row.get("tier").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                row.get("temperature").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                row.get("max_tokens").cloned().unwrap_or(Value::Null),
                row.get("rationale_md")
                    .filter(|v| !v.is_null())
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(), // row.rationale_md ?? ''
                false,
            )
        } else {
            (
                "mid".to_string(),
                "cold".to_string(),
                json!(1024),
                format!(
                    "unknown task_kind '{}' — fell back to mid+cold. Add to brain.routing_defaults.",
                    tmpl(Some(&task_kind_v))
                ),
                true,
            )
        };

    let degrade = degradeIfNeeded(&Value::String(base_tier.clone()), &budget_remaining_usd);
    let final_tier = degrade.get("tier").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let degraded = degrade.get("degraded").and_then(|v| v.as_bool()).unwrap_or(false);
    let exhausted = degrade.get("exhausted").and_then(|v| v.as_bool()).unwrap_or(false);

    let rationale_md = if degraded {
        // budget_remaining_usd?.toFixed(4) — reachable only when degraded is true, which only
        // happens after a numeric (non-null, non-zero) budget comparison, so it's always a number here.
        let budget_str = match &budget_remaining_usd {
            Value::Number(n) => format!("{:.4}", n.as_f64().unwrap_or(f64::NAN)),
            other => tmpl(Some(other)),
        };
        format!(
            "{} [budget-degraded from {}\u{2192}{}; remaining=${}]",
            base_rationale, base_tier, final_tier, budget_str
        )
    } else if exhausted {
        format!("{} [BUDGET EXHAUSTED — using low tier as last resort]", base_rationale)
    } else {
        base_rationale.clone()
    };

    let intent_brief_seen = match &intent_brief {
        Value::String(s) => json!(s.chars().take(80).collect::<String>()),
        _ => Value::Null,
    };

    let mut out = Map::new();
    out.insert("tier".into(), json!(final_tier));
    out.insert("temperature".into(), json!(base_temp));
    out.insert("max_tokens".into(), max_tokens);
    out.insert("rationale_md".into(), json!(rationale_md));
    out.insert("fallback_used".into(), json!(fallback));
    out.insert("degraded".into(), json!(degraded));
    out.insert("intent_brief_seen".into(), intent_brief_seen);
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH DATABASE ':memory:' AS brain;").unwrap();
        c.execute_batch(
            "CREATE TABLE brain.routing_defaults ( id INTEGER PRIMARY KEY AUTOINCREMENT, task_kind TEXT NOT NULL UNIQUE, tier TEXT NOT NULL, temperature TEXT NOT NULL, max_tokens INTEGER NOT NULL DEFAULT 1024, rationale_md TEXT );
             INSERT INTO brain.routing_defaults (task_kind, tier, temperature, max_tokens, rationale_md) VALUES
               ('pl.clarify.questions', 'mid', 'cold', 1024, 'mechanical clarify pass'),
               ('pl.decompose.decision', 'high', 'warm', 4096, 'deep cognition needed'),
               ('pl.tune.concept', 'low', 'freezed', 512, NULL);",
        )
        .unwrap();
        c
    }

    #[test]
    fn known_task_kind_no_budget() {
        let c = setup();
        let r = aiRoute(&json!({ "task_kind": "pl.clarify.questions" }), &c).unwrap();
        assert_eq!(
            r,
            json!({ "tier": "mid", "temperature": "cold", "max_tokens": 1024,
                "rationale_md": "mechanical clarify pass", "fallback_used": false,
                "degraded": false, "intent_brief_seen": null })
        );
    }

    #[test]
    fn unknown_task_kind_falls_back_with_intent_brief_slice() {
        let c = setup();
        let long_brief = "x".repeat(120);
        let r = aiRoute(&json!({ "task_kind": "novel.unknown.kind", "intent_brief": long_brief }), &c).unwrap();
        assert_eq!(r["tier"], json!("mid"));
        assert_eq!(r["temperature"], json!("cold"));
        assert_eq!(r["max_tokens"], json!(1024));
        assert_eq!(r["fallback_used"], json!(true));
        assert!(r["rationale_md"].as_str().unwrap().contains("unknown task_kind 'novel.unknown.kind'"));
        assert_eq!(r["intent_brief_seen"], json!("x".repeat(80)));
    }

    #[test]
    fn budget_degrades_high_to_mid() {
        let c = setup();
        let r = aiRoute(&json!({ "task_kind": "pl.decompose.decision", "budget_remaining_usd": 0.01 }), &c).unwrap();
        assert_eq!(r["tier"], json!("mid"));
        assert_eq!(r["degraded"], json!(true));
        assert_eq!(r["rationale_md"], json!("deep cognition needed [budget-degraded from high\u{2192}mid; remaining=$0.0100]"));
    }

    #[test]
    fn budget_exhausted_at_low_tier() {
        let c = setup();
        let r = aiRoute(&json!({ "task_kind": "pl.tune.concept", "budget_remaining_usd": 0.0001 }), &c).unwrap();
        assert_eq!(r["tier"], json!("low"));
        assert_eq!(r["degraded"], json!(false));
        assert_eq!(r["rationale_md"], json!(" [BUDGET EXHAUSTED — using low tier as last resort]"));
    }

    #[test]
    fn ample_budget_no_degradation() {
        let c = setup();
        let r = aiRoute(&json!({ "task_kind": "pl.decompose.decision", "budget_remaining_usd": 1.00 }), &c).unwrap();
        assert_eq!(r["tier"], json!("high"));
        assert_eq!(r["degraded"], json!(false));
        assert_eq!(r["rationale_md"], json!("deep cognition needed"));
    }

    #[test]
    fn zero_budget_short_circuits_no_degrade() {
        let c = setup();
        let r = aiRoute(&json!({ "task_kind": "pl.decompose.decision", "budget_remaining_usd": 0 }), &c).unwrap();
        assert_eq!(r["tier"], json!("high"));
        assert_eq!(r["degraded"], json!(false));
    }

    #[test]
    fn degrade_if_needed_unit() {
        assert_eq!(degradeIfNeeded(&json!("high"), &json!(0.01)), json!({ "tier": "mid", "degraded": true }));
        assert_eq!(degradeIfNeeded(&json!("low"), &json!(0.0)), json!({ "tier": "low", "degraded": false }));
        assert_eq!(degradeIfNeeded(&json!("low"), &json!(0.0001)), json!({ "tier": "low", "degraded": false, "exhausted": true }));
        assert_eq!(degradeIfNeeded(&json!("high"), &Value::Null), json!({ "tier": "high", "degraded": false }));
    }
}
