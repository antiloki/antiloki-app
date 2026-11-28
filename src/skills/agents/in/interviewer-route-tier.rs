//! interviewer-route-tier.rs — twin of skills/agents/in/interviewer-route-tier.mjs (no-db, pure).
//! NAMES ARE CONTRACT: `interviewerRouteTier` (default). Deterministic tier/temperature routing.
// QUALITY: keyword/regex heuristics over user_text -> {tier, temperature, reason, signals}.
use regex::Regex;
use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::collections::HashSet;
use std::sync::OnceLock;

const CONCEPT_KEYWORDS: &[&str] = &[
    "card", "memory", "memories", "contract", "driver", "agent", "action",
    "skill", "law", "consistency", "sandbox", "tick", "session", "narrative",
    "rag", "consolidate", "audit", "sacred", "timeline", "embedding",
    "gate", "dispatch", "keeper", "interviewer", "epic", "feature",
];

const PLANNING_VERBS: &[&str] = &[
    "decompose", "plan", "design", "architect", "compare", "tradeoff",
    "should we", "figure out", "help me think", "walk through",
    "break down", "how would we", "what would it take",
];

const REASONING_MARKERS: &[&str] = &[
    "because", "so that", "given that", "considering", "across",
    "in terms of", "relative to", "compared to", "depends on",
    "as opposed to", "instead of", "why", "why does", "why is",
];

const COMPLEXITY_MARKERS: &[&str] = &[
    "contradicts", "narrative", "multi-step", "cross-cutting",
    "refactor", "migration", "decomposition", "epic",
];

const EXPLAIN_VERBS: &[&str] = &[
    "explain", "describe", "teach", "tell me about", "what does",
    "what is", "how does", "how do", "overview",
];

const READ_VERBS: &[&str] = &[
    "list", "show", "render", "view", "display", "grep", "find",
    "where", "count", "inspect", "how many",
];

fn greeting_res() -> &'static Vec<Regex> {
    static R: OnceLock<Vec<Regex>> = OnceLock::new();
    R.get_or_init(|| {
        vec![
            Regex::new(r"(?i)^(hi|hello|hey|yo|sup|good (morning|afternoon|evening))\b").unwrap(),
            Regex::new(r"(?i)^thanks?\b").unwrap(),
            Regex::new(r"(?i)^ok\b").unwrap(),
            Regex::new(r"(?i)^cool\b").unwrap(),
            Regex::new(r"(?i)^nice\b").unwrap(),
        ]
    })
}

fn count_occurrences(text: &str, list: &[&str]) -> i64 {
    let t = text.to_lowercase();
    list.iter().filter(|item| t.contains(**item)).count() as i64
}

fn distinct_concepts(text: &str) -> i64 {
    let t = text.to_lowercase();
    let hit: HashSet<&&str> = CONCEPT_KEYWORDS.iter().filter(|k| t.contains(**k)).collect();
    hit.len() as i64
}

fn is_greeting(text: &str) -> bool {
    let t = text.trim();
    greeting_res().iter().any(|re| re.is_match(t))
}

fn has_read_verb(text: &str) -> bool {
    let t = text.trim().to_lowercase();
    READ_VERBS.iter().any(|v| t.starts_with(v) || t.contains(&format!(" {v} ")))
}

fn has_explain_verb(text: &str) -> bool {
    let t = text.trim().to_lowercase();
    EXPLAIN_VERBS.iter().any(|v| t.starts_with(v) || t.contains(&format!(" {v} ")))
}

fn has_planning_verb(text: &str) -> bool {
    let t = text.to_lowercase();
    PLANNING_VERBS.iter().any(|v| t.contains(v))
}

pub fn interviewerRouteTier(input: &Value, _db: &Connection) -> Result<Value, String> {
    let user_text = match input.get("user_text") {
        Some(Value::String(s)) if !s.is_empty() => s.clone(),
        _ => return Err("interviewer-route-tier: user_text is required".into()),
    };

    let t = user_text.trim().to_string();
    let chars = t.chars().count() as i64;
    let words = t.split_whitespace().count() as i64;
    let concepts = distinct_concepts(&t);
    let planning = has_planning_verb(&t);
    let reasoning_hits = count_occurrences(&t, REASONING_MARKERS);
    let complexity_hits = count_occurrences(&t, COMPLEXITY_MARKERS);
    let greeting = is_greeting(&t);
    let read_verb = has_read_verb(&t);
    let explain_verb = has_explain_verb(&t);

    let mut signals = Map::new();
    signals.insert("chars".into(), json!(chars));
    signals.insert("words".into(), json!(words));
    signals.insert("concepts".into(), json!(concepts));
    signals.insert("planning_verbs".into(), json!(planning));
    signals.insert("reasoning_markers".into(), json!(reasoning_hits));
    signals.insert("complexity_markers".into(), json!(complexity_hits));
    signals.insert("greeting".into(), json!(greeting));
    signals.insert("read_verb".into(), json!(read_verb));
    signals.insert("explain_verb".into(), json!(explain_verb));
    let signals = Value::Object(signals);

    // HIGH: explicit planning verb, OR any complexity marker, OR 2+ reasoning
    // markers with no explain verb (see JS comment for the "explain wins" rule).
    if planning || complexity_hits >= 1 || (reasoning_hits >= 2 && !explain_verb) {
        let reason = if planning {
            "planning verb detected (decompose/plan/design)".to_string()
        } else if complexity_hits >= 1 {
            "substrate-complexity marker present".to_string()
        } else {
            format!("{reasoning_hits} reasoning markers signal deep thinking")
        };
        return Ok(json!({ "tier": "high", "temperature": "cold", "reason": reason, "signals": signals }));
    }

    // MID: explain/educate verbs, 2+ concepts, one reasoning marker, or long input.
    if explain_verb || concepts >= 2 || reasoning_hits == 1 || words >= 25 {
        let reason = if explain_verb {
            "explain/educate verb — narrative reply expected".to_string()
        } else if concepts >= 2 {
            format!("{concepts} concepts present — moderate context")
        } else if reasoning_hits == 1 {
            "one reasoning marker — needs reframing".to_string()
        } else {
            format!("{words} words — longer input warrants mid tier")
        };
        return Ok(json!({ "tier": "mid", "temperature": "warm", "reason": reason, "signals": signals }));
    }

    // LOW: short, single-concept, read verb, greeting.
    let reason = if greeting {
        "greeting — cheap onboarding response".to_string()
    } else if read_verb {
        "read verb — terse listing reply".to_string()
    } else if chars < 50 {
        "short input — simple intent".to_string()
    } else {
        "default low-cognition path".to_string()
    };
    Ok(json!({ "tier": "low", "temperature": "cold", "reason": reason, "signals": signals }))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn call(i: Value) -> Value {
        interviewerRouteTier(&i, &Connection::open_in_memory().unwrap()).unwrap()
    }

    #[test]
    fn requires_user_text() {
        let db = Connection::open_in_memory().unwrap();
        assert!(interviewerRouteTier(&json!({}), &db).is_err());
        assert!(interviewerRouteTier(&json!({ "user_text": "" }), &db).is_err());
        assert!(interviewerRouteTier(&json!({ "user_text": 42 }), &db).is_err());
    }

    #[test]
    fn greeting_is_low_cold() {
        let r = call(json!({ "user_text": "hi" }));
        assert_eq!(r["tier"], json!("low"));
        assert_eq!(r["temperature"], json!("cold"));
        assert!(r["reason"].as_str().unwrap().contains("greeting"));
        assert_eq!(r["signals"]["greeting"], json!(true));
    }

    #[test]
    fn read_verb_is_low() {
        let r = call(json!({ "user_text": "list cards" }));
        assert_eq!(r["tier"], json!("low"));
    }

    #[test]
    fn explain_verb_is_mid_warm() {
        let r = call(json!({ "user_text": "explain how the driver works" }));
        assert_eq!(r["tier"], json!("mid"));
        assert_eq!(r["temperature"], json!("warm"));
        assert_eq!(r["signals"]["explain_verb"], json!(true));
    }

    #[test]
    fn planning_verb_is_high_cold() {
        let r = call(json!({ "user_text": "help me decompose what we need for multi-user sessions" }));
        assert_eq!(r["tier"], json!("high"));
        assert_eq!(r["temperature"], json!("cold"));
        assert!(r["reason"].as_str().unwrap().contains("planning"));
    }

    #[test]
    fn explain_verb_wins_over_concept_count() {
        let r = call(json!({ "user_text": "what is the relationship between contracts cards and the driver" }));
        assert_eq!(r["tier"], json!("mid"));
    }

    #[test]
    fn reasoning_markers_push_high() {
        let r = call(json!({ "user_text": "why does the sandbox tamper-check fire so often considering that we already have the audit chain" }));
        assert_eq!(r["tier"], json!("high"));
    }

    #[test]
    fn complexity_marker_is_high() {
        let r = call(json!({ "user_text": "consolidate a narrative from recent lessons" }));
        assert_eq!(r["tier"], json!("high"));
    }

    #[test]
    fn thanks_ok_cool_nice_are_low() {
        for t in ["thanks", "ok", "cool", "nice"] {
            let r = call(json!({ "user_text": t }));
            assert_eq!(r["tier"], json!("low"), "'{}' should be low", t);
        }
    }

    #[test]
    fn signals_exposed() {
        let r = call(json!({ "user_text": "decompose a refactor across multiple cards" }));
        assert!(r["signals"].is_object());
        assert_eq!(r["signals"]["planning_verbs"], json!(true));
        assert!(r["signals"]["concepts"].as_i64().unwrap() >= 1);
        assert!(r["signals"]["complexity_markers"].as_i64().unwrap() >= 1);
    }
}
