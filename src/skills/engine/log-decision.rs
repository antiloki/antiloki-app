//! log-decision.rs — twin of skills/engine/log-decision.mjs (db WRITE lane, delegates to
//! entities/brain-decision-record). NAMES ARE CONTRACT: `logDecision` (default). Thin wrapper:
//! auto-resolves an adhoc session_id ('adhoc:<agent>:<day>') when none is given, and swallows
//! ANY underlying failure (returns null — logging must never break the caller's primary path).
// FIX-BRAIN-DECISIONS-DEDUP: an unchanged rule_match for the SAME card is a no-op write (the driver
// re-logs an identical heartbeat every tick otherwise). Mirrors the JS module-scope
// `lastRuleMatchRationale` Map via a process-scoped static — both conform lanes run ONE process per
// fixture (JS: a single module import reused across all `cases`; Rust: this static lives for the one
// `cargo run` invocation), so the baseline persists across cases identically both sides. The baseline
// is recorded only AFTER a successful write, so a failed insert never suppresses the next attempt.
use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}
// JS `${v}` interpolation — used for the adhoc session_id's `${agent}` slot.
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
fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) }

const ADHOC_PREFIX: &str = "adhoc:";
// Frozen-clock literal: the JS `new Date().toISOString().slice(0, 10)` under the conform harness's
// frozen clock (2026-01-01T00:00:00.000Z) always yields this literal.
fn today() -> String { crate::clock::now_iso_nodb()[..10].to_string() }

fn adhoc_session_id(agent: Option<&Value>) -> String {
    let day = today();
    format!("{ADHOC_PREFIX}{}:{day}", tmpl(agent))
}

// Canonical JSON text stands in for JS SameValueZero equality on the string/number/bool/null values
// this skill ever passes as a Map key/value (card_slug, rationale are always primitives in practice).
fn canon(v: &Value) -> String { serde_json::to_string(v).unwrap_or_default() }

fn dedup_map() -> &'static Mutex<HashMap<String, String>> {
    static MAP: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
    MAP.get_or_init(|| Mutex::new(HashMap::new()))
}

pub fn logDecision(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS `const { db } = context ?? {}; if (!db) return null;` — the db lane always injects a
    // Connection, so that guard is unreachable here (mirrors brain-decision-record's same note).
    let obj = input.as_object();
    let get = |k: &str| obj.and_then(|o| o.get(k));

    let agent = get("agent").cloned().unwrap_or(Value::Null);
    let decision = get("decision").cloned().unwrap_or(Value::Null);
    let rationale = get("rationale").cloned().unwrap_or(Value::Null);
    // JS destructure defaults (= null) trigger only on undefined; JSON has no undefined, so absent
    // key and explicit null both land as null here — identical either way.
    let card_slug = get("card_slug").cloned().unwrap_or(Value::Null);
    let inputs = get("inputs").cloned().unwrap_or(Value::Null);
    let session_id_raw = get("session_id").cloned().unwrap_or(Value::Null);
    let fire_id = get("fire_id").cloned().unwrap_or(Value::Null);

    if !truthy(Some(&agent)) || !truthy(Some(&decision)) || !truthy(Some(&rationale)) {
        return Ok(Value::Null);
    }

    let is_rule_match = decision.as_str() == Some("rule_match");
    let card_slug_truthy = truthy(Some(&card_slug));

    // Heartbeat dedup: an unchanged rule_match for the same card is a no-op write.
    if is_rule_match && card_slug_truthy {
        let key = canon(&card_slug);
        let map = dedup_map().lock().unwrap();
        if map.get(&key) == Some(&canon(&rationale)) {
            return Ok(Value::Null);
        }
    }

    let sid = nz(Some(&session_id_raw)).cloned().unwrap_or_else(|| json!(adhoc_session_id(Some(&agent))));

    let mut sub_input = Map::new();
    sub_input.insert("session_id".into(), sid);
    sub_input.insert("decision".into(), decision.clone());
    sub_input.insert("rationale".into(), rationale.clone());
    sub_input.insert("card_slug".into(), card_slug.clone());
    sub_input.insert("inputs".into(), inputs);
    sub_input.insert("fire_id".into(), fire_id);

    match crate::entities_brain_decision_record::brainDecisionRecord(&Value::Object(sub_input), db) {
        Ok(result) => {
            // Record the baseline only after a successful write, so a failed insert doesn't
            // suppress the next attempt at the same decision.
            if is_rule_match && card_slug_truthy {
                dedup_map().lock().unwrap().insert(canon(&card_slug), canon(&rationale));
            }
            Ok(result)
        }
        // Don't propagate logging failures. The caller's primary work matters more.
        Err(_) => Ok(Value::Null),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCHEMA_DECISIONS: &str = "CREATE TABLE brain.brain_decisions ( id INTEGER PRIMARY KEY AUTOINCREMENT, session_id TEXT NOT NULL, decided_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')), card_slug TEXT, decision TEXT NOT NULL, rationale TEXT NOT NULL, inputs_json TEXT, fire_id INTEGER, CHECK (decision IN ('fire','wait','rollback','escalate','done','pause','session_start','session_end','goal_set','classify','gate_verdict','rule_match','sandbox_create','review_pass','schedule_and_dispatch','agent_step')) )";

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS brain").unwrap();
        c.execute_batch(SCHEMA_DECISIONS).unwrap();
        c
    }

    // One test function: the dedup map is a process-scoped static, so keeping every scenario
    // sequential (rather than splitting across #[test] fns that cargo test may run concurrently)
    // avoids cross-scenario races on shared state.
    #[test]
    fn logs_writes_guards_and_dedupes_rule_match_heartbeats() {
        // Missing required fields never write.
        let c = db();
        assert_eq!(logDecision(&json!({}), &c).unwrap(), Value::Null);
        assert_eq!(logDecision(&json!({ "agent": "x" }), &c).unwrap(), Value::Null);
        assert_eq!(logDecision(&json!({ "agent": "x", "decision": "classify" }), &c).unwrap(), Value::Null);

        // Successful write auto-resolves an adhoc session_id from the frozen clock.
        let c = db();
        let r = logDecision(
            &json!({ "agent": "in", "decision": "classify", "rationale": "kind=meta", "inputs": { "foo": 1 } }),
            &c,
        )
        .unwrap();
        assert_eq!(r["decision"], json!("classify"));
        assert_eq!(r["session_id"], json!("adhoc:in:2026-01-01"));
        assert_eq!(r["inputs_json"], json!("{\"foo\":1}"));

        // Explicit session_id is honored verbatim (no adhoc fallback).
        let c = db();
        let r = logDecision(
            &json!({ "agent": "driver:auto", "decision": "schedule_and_dispatch", "rationale": "x", "session_id": "sess-xyz" }),
            &c,
        )
        .unwrap();
        assert_eq!(r["session_id"], json!("sess-xyz"));

        // An invalid decision kind makes brain-decision-record throw; log-decision swallows it.
        let c = db();
        assert_eq!(
            logDecision(&json!({ "agent": "x", "decision": "bogus_kind", "rationale": "r" }), &c).unwrap(),
            Value::Null
        );

        // rule_match heartbeat dedup, keyed per card_slug.
        let c = db();
        let base = json!({ "agent": "state-machine", "decision": "rule_match", "card_slug": "DEDUP-CARD-1", "rationale": "rule-x: unchanged reason" });
        assert!(logDecision(&base, &c).unwrap().is_object(), "first rule_match writes (establishes baseline)");
        assert_eq!(logDecision(&base, &c).unwrap(), Value::Null, "identical rule_match for the same card is skipped");
        let mut changed = base.clone();
        changed["rationale"] = json!("rule-y: a different decision");
        assert!(logDecision(&changed, &c).unwrap().is_object(), "a CHANGED rationale writes again");

        // rule_match WITHOUT a card_slug is never deduped.
        let no_card = json!({ "agent": "state-machine", "decision": "rule_match", "rationale": "no-card-rationale" });
        assert!(logDecision(&no_card, &c).unwrap().is_object());
        assert!(logDecision(&no_card, &c).unwrap().is_object(), "card-less rule_match always writes");

        // non-rule_match decisions are never deduped, even with identical card_slug + rationale.
        let dispatch = json!({ "agent": "driver:auto", "decision": "schedule_and_dispatch", "card_slug": "DEDUP-CARD-2", "rationale": "same dispatch reason" });
        assert!(logDecision(&dispatch, &c).unwrap().is_object());
        assert!(logDecision(&dispatch, &c).unwrap().is_object(), "schedule_and_dispatch always writes");

        // dedup key is per-card: same rationale on different cards both write.
        let a = json!({ "agent": "state-machine", "decision": "rule_match", "card_slug": "DEDUP-A", "rationale": "shared-reason" });
        let b = json!({ "agent": "state-machine", "decision": "rule_match", "card_slug": "DEDUP-B", "rationale": "shared-reason" });
        assert!(logDecision(&a, &c).unwrap().is_object());
        assert!(logDecision(&b, &c).unwrap().is_object());

        // A failed write must NOT record the dedup baseline — the next identical attempt still writes.
        let bad = json!({ "agent": "x", "decision": "rule_match", "card_slug": "DEDUP-FAIL", "rationale": "r1", "fire_id": "not-an-int" });
        assert_eq!(
            logDecision(&bad, &c).unwrap(),
            Value::Null,
            "invalid fire_id makes the underlying write throw"
        );
        let retry = json!({ "agent": "x", "decision": "rule_match", "card_slug": "DEDUP-FAIL", "rationale": "r1" });
        assert!(
            logDecision(&retry, &c).unwrap().is_object(),
            "no baseline was recorded after the failed attempt, so this still writes"
        );

        // Falsy card_slug ("") bypasses dedup entirely, same as a missing card_slug.
        let empty_slug = json!({ "agent": "state-machine", "decision": "rule_match", "card_slug": "", "rationale": "empty-slug-rationale" });
        assert!(logDecision(&empty_slug, &c).unwrap().is_object());
        assert!(
            logDecision(&empty_slug, &c).unwrap().is_object(),
            "empty-string card_slug is falsy — never deduped"
        );
    }
}
