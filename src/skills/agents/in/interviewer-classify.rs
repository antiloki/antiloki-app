//! interviewer-classify.rs — twin of skills/agents/in/interviewer-classify.mjs (no-db, pure).
//! NAMES ARE CONTRACT: `interviewerClassify` (default). Deterministic regex/keyword intent classification.
// QUALITY: 80/100 — ordered regex rule cascade (destructive>autonomous>curate>read>meta>mutate>propose)
// + keyword-stem action-hint suggester, kind-namespaced (autonomous/curate) to filter cross-kind hints.
use regex::Regex;
use rusqlite::Connection;
use serde_json::{json, Value};
use std::sync::OnceLock;

// Kind → action-namespace whitelist. Only autonomous/curate filter; other kinds are unfiltered.
fn kind_namespace(kind: &str, name: &str) -> bool {
    match kind {
        "autonomous" => name.starts_with("system.driver."),
        "curate" => name.starts_with("keeper."),
        _ => true,
    }
}

// kws, action, risk_tier — order is the match/output order (mirrors JS ACTION_HINTS).
const ACTION_HINTS: &[(&[&str], &str, &str)] = &[
    (&["card", "add"], "board-keeper.card.add", "medium"),
    (&["card", "list"], "board-keeper.card.list", "low"),
    (&["card", "status"], "board-keeper.card.status.change", "medium"),
    (&["memor", "add"], "keeper.memory.add", "medium"),
    (&["memor", "list"], "system.memory.list", "low"),
    (&["memor", "similar"], "system.memory.similar", "low"),
    (&["driver", "tick"], "system.driver.tick", "medium"),
    (&["driver", "run"], "system.driver.run", "high"),
    (&["driver", "state"], "system.driver.tick", "low"),
    (&["backlog"], "board-keeper.card.list", "low"),
    (&["consistency"], "system.consistency.scan", "low"),
    (&["anatomy"], "system.anatomy.html", "low"),
    (&["render", "docs"], "system.docs.index", "low"),
    (&["contract", "list"], "contract.list", "low"),
    (&["agent", "list"], "system.agent.find", "low"),
    (&["law", "list"], "system.law.list", "low"),
    (&["drive", "backlog"], "system.driver.run", "high"),
    (&["run", "driver"], "system.driver.run", "high"),
    (&["tick"], "system.driver.tick", "medium"),
    (&["process", "pending"], "system.driver.run", "high"),
    (&["review", "memor"], "keeper.review", "low"),
    (&["consolidate", "memor"], "keeper.review", "low"),
    (&["consolidate", "lesson"], "keeper.review", "low"),
    (&["memory", "hygiene"], "keeper.review", "low"),
    (&["keeper", "review"], "keeper.review", "low"),
];

fn suggest_actions(text: &str, kind: &str) -> Vec<Value> {
    let lowered = text.to_lowercase();
    let matches: Vec<(&str, &str)> = ACTION_HINTS
        .iter()
        .filter(|(kws, _, _)| kws.iter().all(|kw| lowered.contains(kw)))
        .map(|(_, action, risk)| (*action, *risk))
        .collect();
    matches
        .into_iter()
        .filter(|(name, _)| kind_namespace(kind, name))
        .map(|(name, risk)| json!({ "name": name, "risk_tier": risk }))
        .collect()
}

fn destructive_verb_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"(?i)\b(delete|rm|drop|reset|force|purge|wipe|destroy|truncate|nuke)\b").unwrap())
}
fn destructive_no_verify_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"--no-verify\b").unwrap())
}
fn autonomous_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"(?i)\b(drive (the )?backlog|run (the )?driver|tick (once|the driver)|process pending|autonomous loop|fire (a |the )?tick|advance (the )?board)\b").unwrap())
}
fn curate_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"(?i)\b(review (the )?memor|consolidate (today'?s? |the )?lessons|consolidate( the)? memor|memory (review|hygiene)|prune memor|check (for )?stale memor|keeper review)").unwrap())
}
fn read_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"(?i)^(show|list|how many|what is|what are|tell me|display|render|grep|find|where|count|inspect|view)\b").unwrap())
}
fn meta_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"(?i)^(explain|why|what does|how do|teach|describe|tell me about)\b").unwrap())
}
fn mutate_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"(?i)^(add|create|file|update|change|edit|set|move|rename|migrate|insert|register|seed)\b").unwrap())
}
fn propose_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"(?i)^(should we|let'?s|how about|consider|maybe|i think we|what if)\b").unwrap())
}

// Order matters: destructive > autonomous > curate > read > meta > mutate > propose (mirrors JS RULES).
fn classify_kind(t: &str) -> Option<(&'static str, f64, &'static str)> {
    let trimmed = t.trim();
    if destructive_verb_re().is_match(t) || destructive_no_verify_re().is_match(t) {
        return Some(("destructive", 0.95, "destructive verb detected"));
    }
    if autonomous_re().is_match(t) {
        return Some(("autonomous", 0.9, "autonomous-loop intent detected (driver work)"));
    }
    if curate_re().is_match(t) {
        return Some(("curate", 0.9, "memory-curation intent detected (keeper review)"));
    }
    if read_re().is_match(trimmed) {
        return Some(("read", 0.9, "read verb detected"));
    }
    if meta_re().is_match(trimmed) {
        return Some(("meta", 0.85, "meta/explanation verb detected"));
    }
    if mutate_re().is_match(trimmed) {
        return Some(("mutate", 0.85, "mutate verb detected"));
    }
    if propose_re().is_match(trimmed) {
        return Some(("propose", 0.7, "proposal-style phrasing"));
    }
    None
}

pub fn interviewerClassify(input: &Value, _db: &Connection) -> Result<Value, String> {
    let text = match input.get("user_text").and_then(|v| v.as_str()) {
        Some(s) if !s.is_empty() => s,
        _ => return Ok(json!({ "kind": "clarify", "confidence": 0, "reason": "empty input", "suggested_actions": [] })),
    };

    if let Some((kind, confidence, reason)) = classify_kind(text) {
        return Ok(json!({
            "kind": kind,
            "confidence": confidence,
            "reason": reason,
            "suggested_actions": suggest_actions(text, kind),
        }));
    }

    Ok(json!({
        "kind": "clarify",
        "confidence": 0.2,
        "reason": "no rule matched — input is ambiguous; rephrase or be more specific",
        "suggested_actions": [],
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn call(i: Value) -> Value { interviewerClassify(&i, &Connection::open_in_memory().unwrap()).unwrap() }

    #[test]
    fn classifies_every_rule_in_order() {
        assert_eq!(call(json!({})), json!({ "kind": "clarify", "confidence": 0, "reason": "empty input", "suggested_actions": [] }));
        assert_eq!(call(json!({ "user_text": "" })), json!({ "kind": "clarify", "confidence": 0, "reason": "empty input", "suggested_actions": [] }));
        assert_eq!(call(json!({ "user_text": 42 })), json!({ "kind": "clarify", "confidence": 0, "reason": "empty input", "suggested_actions": [] }));

        let no_match = call(json!({ "user_text": "banana smoothie recipe" }));
        assert_eq!(no_match["kind"], json!("clarify"));
        assert_eq!(no_match["confidence"], json!(0.2));
        assert_eq!(no_match["suggested_actions"], json!([]));

        let d = call(json!({ "user_text": "DELETE the stale memory entries" }));
        assert_eq!(d, json!({ "kind": "destructive", "confidence": 0.95, "reason": "destructive verb detected", "suggested_actions": [] }));

        let d2 = call(json!({ "user_text": "git commit --no-verify" }));
        assert_eq!(d2["kind"], json!("destructive"));

        let a = call(json!({ "user_text": "run the driver and check the backlog" }));
        assert_eq!(a["kind"], json!("autonomous"));
        assert_eq!(a["confidence"], json!(0.9));
        assert_eq!(a["suggested_actions"], json!([
            { "name": "system.driver.run", "risk_tier": "high" },
            { "name": "system.driver.run", "risk_tier": "high" },
            { "name": "system.driver.run", "risk_tier": "high" }
        ])); // namespace filter drops the 'backlog' → board-keeper.card.list hint

        let c = call(json!({ "user_text": "review the memory backlog" }));
        assert_eq!(c["kind"], json!("curate"));
        assert_eq!(c["suggested_actions"], json!([{ "name": "keeper.review", "risk_tier": "low" }])); // namespace filter drops board-keeper.card.list

        let r = call(json!({ "user_text": "show me the backlog" }));
        assert_eq!(r["kind"], json!("read"));
        assert_eq!(r["suggested_actions"], json!([{ "name": "board-keeper.card.list", "risk_tier": "low" }]));

        let m = call(json!({ "user_text": "explain how the driver works" }));
        assert_eq!(m["kind"], json!("meta"));
        assert_eq!(m["suggested_actions"], json!([]));

        let mu = call(json!({ "user_text": "add a new card for this feature" }));
        assert_eq!(mu["kind"], json!("mutate"));
        assert_eq!(mu["suggested_actions"], json!([{ "name": "board-keeper.card.add", "risk_tier": "medium" }]));

        let p = call(json!({ "user_text": "should we migrate the schema" }));
        assert_eq!(p["kind"], json!("propose"));
        assert_eq!(p["suggested_actions"], json!([]));
    }
}
