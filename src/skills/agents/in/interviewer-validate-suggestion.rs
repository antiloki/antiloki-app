//! interviewer-validate-suggestion.rs — twin of skills/agents/in/interviewer-validate-suggestion.mjs
//! (db lane; delegates to engine/log-decision on drop). NAMES ARE CONTRACT:
//! `interviewerValidateSuggestion` (default). Deterministic floor under the AI: a
//! suggested_action_name must exist in state.actions, and (unless delegated_to_agent is falsy
//! or 'in') delegated_to_agent must either own it or hold an active agent_actions grant.
//! Dropped suggestions are logged to brain.brain_decisions via log-decision — a fire-and-forget
//! call whose result the JS site discards, so this twin does too.
use rusqlite::Connection;
use serde_json::{json, Map, Value};

fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}
// JS `${v}` interpolation for the reason strings.
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

// JS destructuring default (`{ k = default } = input`) fires ONLY when the key is absent (JS
// `undefined`) — an explicit `null` in the input is NOT replaced by the default. JSON has no
// undefined, so "key absent" is the trigger here; "key present as null" passes null through
// unchanged (this matters: the null/default distinction is visible in the logged inputs_json).
fn default_on_missing(obj: Option<&Map<String, Value>>, key: &str, default: Value) -> Value {
    match obj.and_then(|o| o.get(key)) {
        None => default,
        Some(v) => v.clone(),
    }
}

pub fn interviewerValidateSuggestion(input: &Value, db: &Connection) -> Result<Value, String> {
    let obj = input.as_object();

    // suggested_action_name's JS default is `null`, so absent-vs-explicit-null both land Null.
    let suggested_action_name = obj.and_then(|o| o.get("suggested_action_name")).cloned().unwrap_or(Value::Null);
    let delegated_to_agent = default_on_missing(obj, "delegated_to_agent", json!("in"));
    let suggested_args = default_on_missing(obj, "suggested_args", json!({}));
    let user_text = default_on_missing(obj, "user_text", json!(""));

    // No suggestion → nothing to validate; pass-through.
    if !truthy(Some(&suggested_action_name)) {
        return Ok(json!({ "valid": true, "dropped": false, "reason": Value::Null }));
    }

    // ── Check 1: action exists ────────────────────────────────────────
    let rows = crate::db_json::query_rows(
        db,
        "SELECT name, owner_agent FROM actions WHERE name = ? LIMIT 1",
        &[suggested_action_name.clone()],
    )?;
    let action_row = rows.as_array().and_then(|a| a.first()).cloned();

    let action_row = match action_row {
        None => {
            let reason = format!("unknown action '{}' — not in state.actions", tmpl(Some(&suggested_action_name)));
            let payload = json!({
                "agent": "in",
                "decision": "gate_verdict",
                "rationale": format!("dropped invalid AI suggestion: {}", reason),
                "inputs": {
                    "user_text": user_text,
                    "suggested_action_name": suggested_action_name,
                    "delegated_to_agent": delegated_to_agent,
                    "suggested_args": suggested_args,
                },
            });
            let _ = crate::engine_log_decision::logDecision(&payload, db);
            return Ok(json!({ "valid": false, "dropped": true, "reason": reason }));
        }
        Some(row) => row,
    };

    // ── Check 2: delegated_to_agent owns or is granted the action ────
    if truthy(Some(&delegated_to_agent)) && delegated_to_agent != json!("in") {
        let owner_agent = action_row.get("owner_agent").cloned().unwrap_or(Value::Null);
        if owner_agent != delegated_to_agent {
            let grant_rows = crate::db_json::query_rows(
                db,
                "SELECT 1 AS ok FROM agent_actions WHERE agent_name=? AND action_name=? AND revoked_at IS NULL LIMIT 1",
                &[delegated_to_agent.clone(), suggested_action_name.clone()],
            )?;
            let has_grant = grant_rows.as_array().map_or(false, |a| !a.is_empty());
            if !has_grant {
                let reason = format!(
                    "agent '{}' does not own (or have grant on) action '{}' (owner={})",
                    tmpl(Some(&delegated_to_agent)),
                    tmpl(Some(&suggested_action_name)),
                    tmpl(Some(&owner_agent))
                );
                let payload = json!({
                    "agent": "in",
                    "decision": "gate_verdict",
                    "rationale": format!("dropped invalid AI suggestion: {}", reason),
                    "inputs": {
                        "user_text": user_text,
                        "suggested_action_name": suggested_action_name,
                        "delegated_to_agent": delegated_to_agent,
                        "owner_agent": owner_agent,
                    },
                });
                let _ = crate::engine_log_decision::logDecision(&payload, db);
                let mut out = Map::new();
                out.insert("valid".into(), json!(false));
                out.insert("dropped".into(), json!(true));
                out.insert("reason".into(), json!(reason));
                out.insert("corrected_agent".into(), owner_agent);
                return Ok(Value::Object(out));
            }
        }
    }

    Ok(json!({ "valid": true, "dropped": false, "reason": Value::Null }))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCHEMA_DECISIONS: &str = "CREATE TABLE brain.brain_decisions ( id INTEGER PRIMARY KEY AUTOINCREMENT, session_id TEXT NOT NULL, decided_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')), card_slug TEXT, decision TEXT NOT NULL, rationale TEXT NOT NULL, inputs_json TEXT, fire_id INTEGER, CHECK (decision IN ('fire','wait','rollback','escalate','done','pause','session_start','session_end','goal_set','classify','gate_verdict','rule_match','sandbox_create','review_pass','schedule_and_dispatch','agent_step')) )";

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS brain").unwrap();
        c.execute_batch(SCHEMA_DECISIONS).unwrap();
        c.execute_batch(
            "CREATE TABLE actions (name TEXT PRIMARY KEY, owner_agent TEXT); \
             CREATE TABLE agent_actions (agent_name TEXT, action_name TEXT, revoked_at TEXT);",
        )
        .unwrap();
        c
    }

    #[test]
    fn validates_suggestions() {
        let c = db();

        // No suggestion → pass-through.
        assert_eq!(
            interviewerValidateSuggestion(&json!({}), &c).unwrap(),
            json!({ "valid": true, "dropped": false, "reason": null })
        );
        // Falsy suggested_action_name ("") → same pass-through.
        assert_eq!(
            interviewerValidateSuggestion(&json!({ "suggested_action_name": "" }), &c).unwrap(),
            json!({ "valid": true, "dropped": false, "reason": null })
        );

        // Unknown action → dropped + logged to brain.brain_decisions.
        let r = interviewerValidateSuggestion(
            &json!({ "suggested_action_name": "system.does-not-exist", "delegated_to_agent": "system", "user_text": "do the thing" }),
            &c,
        )
        .unwrap();
        assert_eq!(r["valid"], json!(false));
        assert_eq!(r["dropped"], json!(true));
        assert!(r["reason"].as_str().unwrap().contains("unknown action"));
        let row: (String, String) = c
            .query_row(
                "SELECT decision, rationale FROM brain.brain_decisions WHERE decision='gate_verdict' ORDER BY id DESC LIMIT 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(row.0, "gate_verdict");
        assert!(row.1.contains("dropped invalid AI suggestion"));
        assert!(row.1.contains("unknown action"));

        // Explicit null delegated_to_agent is NOT replaced by the 'in' default — it stays null
        // in the logged inputs_json (default only fires on an ABSENT key).
        interviewerValidateSuggestion(
            &json!({ "suggested_action_name": "another.missing.action", "delegated_to_agent": null }),
            &c,
        )
        .unwrap();
        let inputs_json: String = c
            .query_row(
                "SELECT inputs_json FROM brain.brain_decisions WHERE decision='gate_verdict' ORDER BY id DESC LIMIT 1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let parsed: Value = serde_json::from_str(&inputs_json).unwrap();
        assert_eq!(parsed["delegated_to_agent"], Value::Null);
        assert_eq!(parsed["suggested_action_name"], json!("another.missing.action"));

        // Seed real actions + grants.
        c.execute_batch(
            "INSERT INTO actions (name, owner_agent) VALUES \
               ('board-keeper.card.list', 'board-keeper'), \
               ('board-keeper.card.add', 'board-keeper'), \
               ('board-keeper.card.delete', 'board-keeper'), \
               ('system.memory.list', 'system'), \
               ('keeper.memory.add', 'keeper'); \
             INSERT INTO agent_actions (agent_name, action_name, revoked_at) VALUES \
               ('man', 'board-keeper.card.add', NULL), \
               ('git-manager', 'board-keeper.card.delete', '2025-12-01 00:00:00');",
        )
        .unwrap();

        // delegated_to_agent defaults to 'in' when absent → valid regardless of owner.
        assert_eq!(
            interviewerValidateSuggestion(&json!({ "suggested_action_name": "board-keeper.card.list" }), &c).unwrap(),
            json!({ "valid": true, "dropped": false, "reason": null })
        );
        // Explicit 'in' → same bypass.
        assert_eq!(
            interviewerValidateSuggestion(&json!({ "suggested_action_name": "system.memory.list", "delegated_to_agent": "in" }), &c).unwrap(),
            json!({ "valid": true, "dropped": false, "reason": null })
        );
        // delegated_to_agent === owner_agent → valid.
        assert_eq!(
            interviewerValidateSuggestion(&json!({ "suggested_action_name": "board-keeper.card.list", "delegated_to_agent": "board-keeper" }), &c).unwrap(),
            json!({ "valid": true, "dropped": false, "reason": null })
        );
        // Falsy delegated_to_agent ("") skips the ownership check entirely → valid.
        assert_eq!(
            interviewerValidateSuggestion(&json!({ "suggested_action_name": "board-keeper.card.list", "delegated_to_agent": "" }), &c).unwrap(),
            json!({ "valid": true, "dropped": false, "reason": null })
        );
        // Different agent WITH an active grant → valid.
        assert_eq!(
            interviewerValidateSuggestion(&json!({ "suggested_action_name": "board-keeper.card.add", "delegated_to_agent": "man" }), &c).unwrap(),
            json!({ "valid": true, "dropped": false, "reason": null })
        );
        // Different agent whose grant is REVOKED → dropped + corrected_agent.
        let revoked = interviewerValidateSuggestion(
            &json!({ "suggested_action_name": "board-keeper.card.delete", "delegated_to_agent": "git-manager" }),
            &c,
        )
        .unwrap();
        assert_eq!(revoked["valid"], json!(false));
        assert_eq!(revoked["dropped"], json!(true));
        assert_eq!(revoked["corrected_agent"], json!("board-keeper"));
        assert!(revoked["reason"].as_str().unwrap().contains("does not own"));
        // Different agent with NO grant row at all → dropped + corrected_agent.
        let none = interviewerValidateSuggestion(
            &json!({ "suggested_action_name": "keeper.memory.add", "delegated_to_agent": "git-manager", "user_text": "add a memory" }),
            &c,
        )
        .unwrap();
        assert_eq!(none["valid"], json!(false));
        assert_eq!(none["corrected_agent"], json!("keeper"));
    }
}
