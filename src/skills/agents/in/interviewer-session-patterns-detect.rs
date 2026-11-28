//! interviewer-session-patterns-detect.rs — twin of skills/agents/in/interviewer-session-patterns-detect.mjs
//! (db READ lane). NAMES ARE CONTRACT: `interviewerSessionPatternsDetect` (default). Deterministic
//! pattern-mining of an interviewer session's turns → candidate memories (no AI). Four detectors:
//! REPEATED_TOPIC · PREFERENCE_SIGNAL · REPEATED_CANCEL · SUCCESSFUL_REPEAT.
use rusqlite::Connection;
use regex::Regex;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::OnceLock;

const CONCEPT_STEMS: [&str; 16] = [
    "card", "memory", "memor", "narrative", "consolidate", "contract",
    "driver", "agent", "action", "workflow", "session", "keeper",
    "gate", "sandbox", "rag", "audit",
];

// PREFERENCE_PATTERNS — /i flag → (?i). No lookaround/backrefs, so the `regex` crate ports them 1:1.
fn preference_patterns() -> &'static Vec<Regex> {
    static R: OnceLock<Vec<Regex>> = OnceLock::new();
    R.get_or_init(|| {
        vec![
            Regex::new(r"(?i)\b(i\s+)?prefer\s+(.{3,80})").unwrap(),
            Regex::new(r"(?i)\balways\s+(.{3,80})").unwrap(),
            Regex::new(r"(?i)\bi\s+want\s+(.{3,80})").unwrap(),
            Regex::new(r"(?i)\bplease\s+(do\s+)?(.{3,80})").unwrap(),
            Regex::new(r"(?i)\bdon'?t\s+(.{3,80})").unwrap(),
        ]
    })
}
fn ws_re() -> &'static Regex { static R: OnceLock<Regex> = OnceLock::new(); R.get_or_init(|| Regex::new(r"\s+").unwrap()) }

fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}
// JS `${v}` interpolation (string/number/bool/null/absent).
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
// JS String(v) (no ?? '') — used by detectSuccessfulRepeats' String(t.user_text) and as a Map-key coercer.
fn js_string(v: &Value) -> String {
    match v {
        Value::Null => "null".into(),
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Array(a) => a.iter().map(js_string).collect::<Vec<_>>().join(","),
        Value::Object(_) => "[object Object]".into(),
    }
}
fn take_chars(s: &str, n: usize) -> String { s.chars().take(n).collect() } // JS .slice(0,n) (BMP-equivalent)
// (text ?? '') — null/absent → "" ; string → itself.
fn nullish_str(v: Option<&Value>) -> String {
    match v { Some(Value::String(s)) => s.clone(), _ => String::new() }
}
// JS Map key coercion for p.action (string identity; other types via String()).
fn action_key(v: &Value) -> String {
    match v { Value::String(s) => s.clone(), other => js_string(other) }
}

// tokenize(text) = (text ?? '').toLowerCase().split(/[^a-z0-9_-]+/).filter(Boolean)
fn tokenize(v: Option<&Value>) -> Vec<String> {
    nullish_str(v)
        .to_lowercase()
        .split(|c: char| !(c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-'))
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect()
}

fn detect_repeated_topics(turns: &[Value]) -> Vec<Value> {
    let mut order: Vec<String> = Vec::new();
    let mut counts: HashMap<String, i64> = HashMap::new();
    for t in turns {
        let toks = tokenize(t.get("user_text"));
        for stem in CONCEPT_STEMS {
            if toks.iter().any(|tok| tok.contains(stem)) {
                if !counts.contains_key(stem) { order.push(stem.to_string()); }
                *counts.entry(stem.to_string()).or_insert(0) += 1;
            }
        }
    }
    let mut out = Vec::new();
    for stem in &order {
        let n = counts[stem];
        if n >= 3 {
            let confidence = (0.6_f64 + 0.1 * (n as f64 - 3.0)).min(0.9);
            out.push(json!({
                "kind": "REPEATED_TOPIC",
                "type": "lesson",
                "title": format!("Recurring topic: {}", stem),
                "guidance": format!("Within a single interviewer session, the user touched \"{}\" {} times. Likely a sustained focus area worth remembering across sessions.", stem, n),
                "confidence": confidence,
                "evidence": { "stem": stem, "count": n },
            }));
        }
    }
    out
}

fn detect_preference_signals(turns: &[Value]) -> Vec<Value> {
    let mut out = Vec::new();
    for t in turns {
        let ut_val = t.get("user_text").cloned().unwrap_or(Value::Null);
        let ut = nullish_str(Some(&ut_val)); // JS: t.user_text.match(...) — success path is always a string
        for pat in preference_patterns() {
            if let Some(m) = pat.find(&ut) {
                let collapsed = ws_re().replace_all(m.as_str().trim(), " ").into_owned();
                let fragment = take_chars(&collapsed, 140);
                out.push(json!({
                    "kind": "PREFERENCE_SIGNAL",
                    "type": "preference",
                    "title": format!("User preference: {}", take_chars(&fragment, 60)),
                    "guidance": format!("Verbatim from user input: \"{}\". Apply this preference in future interviewer turns.", fragment),
                    "confidence": 0.85,
                    "evidence": { "user_text": ut_val, "matched": fragment },
                }));
                break; // one per turn
            }
        }
    }
    out
}

fn detect_repeated_cancellations(turns: &[Value]) -> Vec<Value> {
    let mut order: Vec<Value> = Vec::new();
    let mut counts: HashMap<String, i64> = HashMap::new();
    for t in turns {
        if truthy(t.get("confirmed")) { continue; }
        let plan: Vec<Value> = match t.get("proposed_actions_json") {
            None | Some(Value::Null) => Vec::new(), // ?? '[]' → JSON.parse('[]') → []
            Some(Value::String(s)) => serde_json::from_str::<Value>(s).ok()
                .and_then(|v| v.as_array().cloned()).unwrap_or_default(), // catch {} → keep []
            Some(_) => Vec::new(),
        };
        for p in &plan {
            let rt = p.get("risk_tier");
            let is_mutate = truthy(rt) && rt.and_then(|v| v.as_str()) != Some("low");
            if !is_mutate { continue; }
            let action_val = p.get("action").cloned().unwrap_or(Value::Null);
            let key = action_key(&action_val);
            if !counts.contains_key(&key) { order.push(action_val); }
            *counts.entry(key).or_insert(0) += 1;
        }
    }
    let mut out = Vec::new();
    for action_val in &order {
        let n = counts[&action_key(action_val)];
        if n >= 2 {
            let confidence = (0.7_f64 + 0.1 * (n as f64 - 2.0)).min(0.9);
            out.push(json!({
                "kind": "REPEATED_CANCEL",
                "type": "preference",
                "title": format!("User repeatedly cancels {}", tmpl(Some(action_val))),
                "guidance": format!("In this session, the proposed action \"{}\" was cancelled {} times. Surface a confirmation summary or ask for tweaks before proposing it again.", tmpl(Some(action_val)), n),
                "confidence": confidence,
                "evidence": { "action": action_val, "count": n },
            }));
        }
    }
    out
}

fn detect_successful_repeats(turns: &[Value]) -> Vec<Value> {
    let mut order: Vec<String> = Vec::new();
    let mut counts: HashMap<String, i64> = HashMap::new();
    for t in turns {
        if !truthy(t.get("dispatched_action_ids_json")) { continue; }
        let ut = js_string(t.get("user_text").unwrap_or(&Value::Null));
        let key = take_chars(&ut.trim().to_lowercase(), 200);
        if !counts.contains_key(&key) { order.push(key.clone()); }
        *counts.entry(key).or_insert(0) += 1;
    }
    let mut out = Vec::new();
    for text in &order {
        let n = counts[text];
        if n >= 2 {
            let confidence = (0.7_f64 + 0.1 * (n as f64 - 2.0)).min(0.9);
            out.push(json!({
                "kind": "SUCCESSFUL_REPEAT",
                "type": "lesson",
                "title": format!("Repeated request resolved the same way ({}\u{00D7})", n),
                "guidance": format!("User asked \"{}\" {} times in this session; each time the same dispatch resolved it. Use the same routing on similar inputs.", take_chars(text, 80), n),
                "confidence": confidence,
                "evidence": { "user_text": text, "count": n },
            }));
        }
    }
    out
}

pub fn interviewerSessionPatternsDetect(input: &Value, db: &Connection) -> Result<Value, String> {
    let session_id = input.get("session_id");
    if !truthy(session_id) {
        return Err("interviewer-session-patterns-detect: session_id is required".into());
    }
    let turns_v = crate::db_json::query_rows(
        db,
        "SELECT user_text, classified_kind, proposed_actions_json, confirmed, dispatched_action_ids_json \
         FROM interviewer_turns WHERE session_id = ? ORDER BY turn_index",
        &[session_id.cloned().unwrap_or(Value::Null)],
    )?;
    let turns = turns_v.as_array().cloned().unwrap_or_default();

    if turns.len() < 5 {
        return Ok(json!({ "candidates": [], "reason": "session too short (<5 turns) to consolidate" }));
    }

    let mut candidates: Vec<Value> = Vec::new();
    candidates.extend(detect_repeated_topics(&turns));
    candidates.extend(detect_preference_signals(&turns));
    candidates.extend(detect_repeated_cancellations(&turns));
    candidates.extend(detect_successful_repeats(&turns));

    Ok(json!({ "candidates": candidates, "turn_count": turns.len() }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seed(rows: &[(&str, i64, &str, Option<&str>, i64, Option<&str>)]) -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE interviewer_turns (session_id TEXT, turn_index INTEGER, user_text TEXT, \
             classified_kind TEXT, proposed_actions_json TEXT, confirmed INTEGER, dispatched_action_ids_json TEXT)",
        ).unwrap();
        for (sid, idx, ut, paj, conf, disp) in rows {
            c.execute(
                "INSERT INTO interviewer_turns (session_id, turn_index, user_text, classified_kind, proposed_actions_json, confirmed, dispatched_action_ids_json) VALUES (?1,?2,?3,NULL,?4,?5,?6)",
                rusqlite::params![sid, idx, ut, paj, conf, disp],
            ).unwrap();
        }
        c
    }

    #[test]
    fn rich_session_fires_all_four() {
        let c = seed(&[
            ("S1", 1, "I prefer working on the card board", Some("[{\"action\":\"card.add\",\"risk_tier\":\"medium\"}]"), 0, None),
            ("S1", 2, "add another card to the board", Some("[{\"action\":\"card.add\",\"risk_tier\":\"medium\"}]"), 0, None),
            ("S1", 3, "show me the card list", None, 1, Some("[\"a1\"]")),
            ("S1", 4, "show me the card list", None, 1, Some("[\"a2\"]")),
            ("S1", 5, "always keep the memory clean", None, 1, None),
            ("S1", 6, "what about the agent workflow", None, 1, None),
        ]);
        let r = interviewerSessionPatternsDetect(&json!({ "session_id": "S1" }), &c).unwrap();
        assert_eq!(r["turn_count"], json!(6));
        let cands = r["candidates"].as_array().unwrap();
        let kinds: Vec<&str> = cands.iter().map(|c| c["kind"].as_str().unwrap()).collect();
        assert_eq!(kinds, vec!["REPEATED_TOPIC", "PREFERENCE_SIGNAL", "PREFERENCE_SIGNAL", "REPEATED_CANCEL", "SUCCESSFUL_REPEAT"]);
        assert_eq!(cands[0]["evidence"], json!({ "stem": "card", "count": 4 }));
        assert_eq!(cands[0]["title"], json!("Recurring topic: card"));
        assert_eq!(cands[1]["evidence"]["matched"], json!("I prefer working on the card board"));
        assert_eq!(cands[1]["confidence"], json!(0.85));
        assert_eq!(cands[2]["evidence"]["matched"], json!("always keep the memory clean"));
        assert_eq!(cands[3]["evidence"], json!({ "action": "card.add", "count": 2 }));
        assert_eq!(cands[4]["evidence"], json!({ "user_text": "show me the card list", "count": 2 }));
        assert_eq!(cands[4]["title"], json!("Repeated request resolved the same way (2\u{00D7})"));
    }

    #[test]
    fn short_session_short_circuits() {
        let c = seed(&[
            ("S2", 1, "hi", None, 1, None),
            ("S2", 2, "there", None, 1, None),
            ("S2", 3, "friend", None, 1, None),
        ]);
        let r = interviewerSessionPatternsDetect(&json!({ "session_id": "S2" }), &c).unwrap();
        assert_eq!(r, json!({ "candidates": [], "reason": "session too short (<5 turns) to consolidate" }));
    }

    #[test]
    fn subthreshold_yields_empty_with_count() {
        let c = seed(&[
            ("S4", 1, "lets do something", Some("[{\"action\":\"safe.read\",\"risk_tier\":\"low\"}]"), 0, None),
            ("S4", 2, "do a thing now", Some("[{\"action\":\"card.add\",\"risk_tier\":\"high\"}]"), 1, None),
            ("S4", 3, "unique request one", None, 1, Some("[\"x1\"]")),
            ("S4", 4, "another unique two", None, 1, Some("[\"x2\"]")),
            ("S4", 5, "final words here", None, 1, None),
        ]);
        let r = interviewerSessionPatternsDetect(&json!({ "session_id": "S4" }), &c).unwrap();
        assert_eq!(r, json!({ "candidates": [], "turn_count": 5 }));
    }
}
