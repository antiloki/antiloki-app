//! decay-score.rs — twin of skills/rag/decay-score.mjs. NAMES ARE CONTRACT: `decayScore`
//! (named default). `DEFAULT_HALF_LIFE_DAYS` is a plain const — it is a bare re-export in JS
//! (`export { DEFAULT_HALF_LIFE_DAYS }`), invisible to the crossing's fn-name census, so no
//! frontier unit names it; kept here anyway so the value isn't silently orphaned.
// QUALITY: 87/100 — small, exact, well-tested; the const-visibility caveat is documented above

use serde_json::{json, Value};

pub const DEFAULT_HALF_LIFE_DAYS: f64 = 14.0;

/// JS: `decayScore(input, _context)` — effective_score = base_score * exp(-age/half_life).
pub fn decayScore(input: &Value, _context: &Value) -> Result<Value, String> {
    let base_score = input.get("base_score").and_then(Value::as_f64)
        .ok_or_else(|| "decay-score: base_score is required".to_string())?;
    let age_days = input.get("age_days").and_then(Value::as_f64)
        .ok_or_else(|| "decay-score: age_days is required".to_string())?;
    let half_life_days = input.get("half_life_days").and_then(Value::as_f64).unwrap_or(DEFAULT_HALF_LIFE_DAYS);
    let factor = (-(age_days.max(0.0)) / half_life_days.max(0.1)).exp();
    Ok(json!({
        "effective_score": base_score * factor,
        "factor": factor,
        "base_score": base_score,
        "age_days": age_days,
        "half_life_days": half_life_days,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn age_zero_is_no_decay() {
        let r = decayScore(&json!({"base_score":0.9,"age_days":0.0}), &Value::Null).unwrap();
        assert_eq!(r["factor"], json!(1.0));
        assert_eq!(r["effective_score"], json!(0.9));
    }

    #[test]
    fn half_life_gives_exp_neg_one() {
        let r = decayScore(&json!({"base_score":1.0,"age_days":14.0,"half_life_days":14.0}), &Value::Null).unwrap();
        assert!((r["factor"].as_f64().unwrap() - (-1.0f64).exp()).abs() < 1e-9);
    }

    #[test]
    fn newer_scores_higher_than_older() {
        let old = decayScore(&json!({"base_score":0.9,"age_days":30.0}), &Value::Null).unwrap()["effective_score"].as_f64().unwrap();
        let new = decayScore(&json!({"base_score":0.9,"age_days":1.0}), &Value::Null).unwrap()["effective_score"].as_f64().unwrap();
        assert!(new > old);
    }

    #[test]
    fn requires_base_score_and_age_days() {
        assert!(decayScore(&json!({}), &Value::Null).is_err());
        assert!(decayScore(&json!({"base_score":1.0}), &Value::Null).is_err());
    }
}
