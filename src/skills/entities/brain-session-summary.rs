//! brain-session-summary.rs — twin of skills/entities/brain-session-summary.mjs (db READ lane,
//! attached `brain` db). NAMES ARE CONTRACT: `brainSessionSummary` (default). Read-only aggregate
//! of a session: fire totals + decision count + distinct cards/agents, plus a derived `status`
//! ('active' until a `session_end` decision lands, then that decision's inputs_json.status).
use rusqlite::Connection;
use serde_json::{json, Map, Value};

// JS truthiness (the `!session_id` guard AND the `status || 'completed'` fallback).
fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}
// JS `??` right operand: nz(a).or(nz(b)).
fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) }

pub fn brainSessionSummary(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS throws err.contextMissing('brain-session-summary','db') when no db — the db lane always
    // injects one, so that guard is unreachable here.
    let obj = input.as_object();
    let get = |k: &str| obj.and_then(|o| o.get(k));
    let session_id = get("session_id").cloned().unwrap_or(Value::Null);
    if !truthy(Some(&session_id)) {
        return Err("brain-session-summary: session_id is required".into());
    }

    let fires_rows = crate::db_json::query_rows(
        db,
        "SELECT COUNT(*) AS fire_count,\n            COALESCE(SUM(cost_usd), 0)   AS total_cost_usd,\n            COALESCE(SUM(tokens_in), 0)  AS total_tokens_in,\n            COALESCE(SUM(tokens_out), 0) AS total_tokens_out\n       FROM brain.brain_fires\n      WHERE session_id = ?",
        &[session_id.clone()],
    )?;
    let fires = fires_rows.as_array().and_then(|a| a.first()).cloned().unwrap_or(Value::Null);

    let decision_count_rows = crate::db_json::query_rows(
        db,
        "SELECT COUNT(*) AS c FROM brain.brain_decisions WHERE session_id = ?",
        &[session_id.clone()],
    )?;
    let decision_count = decision_count_rows
        .as_array()
        .and_then(|a| a.first())
        .and_then(|r| r.get("c"))
        .cloned()
        .unwrap_or(Value::Null);

    let distinct_cards_rows = crate::db_json::query_rows(
        db,
        "SELECT DISTINCT card_slug FROM brain.brain_fires WHERE session_id = ? AND card_slug IS NOT NULL",
        &[session_id.clone()],
    )?;
    let distinct_cards: Vec<Value> = distinct_cards_rows
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|r| r.get("card_slug").cloned().unwrap_or(Value::Null))
        .collect();

    let distinct_agents_rows = crate::db_json::query_rows(
        db,
        "SELECT DISTINCT agent FROM brain.brain_fires WHERE session_id = ?",
        &[session_id.clone()],
    )?;
    let distinct_agents: Vec<Value> = distinct_agents_rows
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|r| r.get("agent").cloned().unwrap_or(Value::Null))
        .collect();

    let session_start_rows = crate::db_json::query_rows(
        db,
        "SELECT decided_at, inputs_json FROM brain.brain_decisions\n      WHERE session_id = ? AND decision = 'session_start'\n      ORDER BY id ASC LIMIT 1",
        &[session_id.clone()],
    )?;
    let session_start = session_start_rows.as_array().and_then(|a| a.first()).cloned();

    let session_end_rows = crate::db_json::query_rows(
        db,
        "SELECT decided_at, inputs_json FROM brain.brain_decisions\n      WHERE session_id = ? AND decision = 'session_end'\n      ORDER BY id DESC LIMIT 1",
        &[session_id.clone()],
    )?;
    let session_end = session_end_rows.as_array().and_then(|a| a.first()).cloned();

    // status = 'active' unless a session_end row exists, in which case:
    //   try { status = JSON.parse(sessionEnd.inputs_json).status || 'completed' } catch { status = 'completed' }
    let mut status = json!("active");
    if let Some(se) = &session_end {
        let ij = se.get("inputs_json").cloned().unwrap_or(Value::Null);
        // JSON.parse(x) on a non-string coerces via String(x) first; a NULL column becomes "null".
        let ij_str = match &ij {
            Value::String(s) => s.clone(),
            Value::Null => "null".to_string(),
            other => other.to_string(),
        };
        status = match serde_json::from_str::<Value>(&ij_str) {
            Ok(parsed) if parsed.is_null() => json!("completed"), // `null.status` would throw in JS -> caught
            Ok(parsed) => {
                let s = parsed.get("status").cloned();
                if truthy(s.as_ref()) { s.unwrap() } else { json!("completed") }
            }
            Err(_) => json!("completed"),
        };
    }

    let started_at = match &session_start {
        Some(ss) => nz(ss.get("decided_at")).cloned().unwrap_or(Value::Null),
        None => Value::Null,
    };
    let ended_at = match &session_end {
        Some(se) => nz(se.get("decided_at")).cloned().unwrap_or(Value::Null),
        None => Value::Null,
    };

    // goal: sessionStart ? (JSON.parse(sessionStart.inputs_json).goal ?? null) : null — NOT wrapped
    // in try/catch in the JS, so a parse failure here would throw; brain-session-start.mjs always
    // writes a valid JSON object, so the success path never hits that.
    let goal = match &session_start {
        Some(ss) => {
            let ij = ss.get("inputs_json").cloned().unwrap_or(Value::Null);
            let ij_str = match &ij {
                Value::String(s) => s.clone(),
                Value::Null => "null".to_string(),
                other => other.to_string(),
            };
            let parsed: Value = serde_json::from_str(&ij_str)
                .map_err(|e| format!("brain-session-summary: {e}"))?;
            if parsed.is_null() {
                return Err("brain-session-summary: Cannot read properties of null (reading 'goal')".into());
            }
            nz(parsed.get("goal")).cloned().unwrap_or(Value::Null)
        }
        None => Value::Null,
    };

    let mut out = Map::new();
    out.insert("session_id".into(), session_id);
    out.insert("status".into(), status);
    out.insert("started_at".into(), started_at);
    out.insert("ended_at".into(), ended_at);
    out.insert("goal".into(), goal);
    out.insert("fire_count".into(), fires.get("fire_count").cloned().unwrap_or(Value::Null));
    out.insert("total_cost_usd".into(), fires.get("total_cost_usd").cloned().unwrap_or(Value::Null));
    out.insert("total_tokens_in".into(), fires.get("total_tokens_in").cloned().unwrap_or(Value::Null));
    out.insert("total_tokens_out".into(), fires.get("total_tokens_out").cloned().unwrap_or(Value::Null));
    out.insert("decision_count".into(), decision_count);
    out.insert("distinct_cards".into(), json!(distinct_cards));
    out.insert("distinct_agents".into(), json!(distinct_agents));
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    // Real brain shapes with the timestamp DEFAULTs frozen the same way the fixture does.
    const SCHEMA_FIRES: &str = "CREATE TABLE brain.brain_fires ( id INTEGER PRIMARY KEY AUTOINCREMENT, session_id TEXT NOT NULL, fired_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')), ended_at TEXT, card_slug TEXT, agent TEXT NOT NULL, action TEXT NOT NULL, inferred_state TEXT, duration_ms INTEGER, tokens_in INTEGER, tokens_out INTEGER, cost_usd REAL, outcome TEXT, result_summary TEXT, CHECK (outcome IN ('success','failed','timeout','rejected','truncated') OR outcome IS NULL) )";
    const SCHEMA_DECISIONS: &str = "CREATE TABLE brain.brain_decisions ( id INTEGER PRIMARY KEY AUTOINCREMENT, session_id TEXT NOT NULL, decided_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')), card_slug TEXT, decision TEXT NOT NULL, rationale TEXT NOT NULL, inputs_json TEXT, fire_id INTEGER, CHECK (decision IN ('fire','wait','rollback','escalate','done','pause','session_start','session_end','goal_set','classify','gate_verdict','rule_match','sandbox_create','review_pass','schedule_and_dispatch','agent_step')) )";

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS brain").unwrap();
        c.execute_batch(SCHEMA_FIRES).unwrap();
        c.execute_batch(SCHEMA_DECISIONS).unwrap();
        c
    }

    #[test]
    fn empty_session_shows_zeros_and_active_status() {
        let c = db();
        c.execute(
            "INSERT INTO brain.brain_decisions (session_id, decided_at, decision, rationale, inputs_json) VALUES ('S-1', '2025-12-31 09:00:00', 'session_start', 'Session started: do nothing', '{\"goal\":\"do nothing\",\"started_by\":\"t\"}')",
            [],
        ).unwrap();
        let r = brainSessionSummary(&json!({ "session_id": "S-1" }), &c).unwrap();
        assert_eq!(r["status"], json!("active"));
        assert_eq!(r["fire_count"], json!(0));
        assert_eq!(r["total_cost_usd"], json!(0));
        assert_eq!(r["decision_count"], json!(1));
        assert_eq!(r["distinct_cards"], json!([]));
        assert_eq!(r["distinct_agents"], json!([]));
        assert_eq!(r["goal"], json!("do nothing"));
        assert_eq!(r["started_at"], json!("2025-12-31 09:00:00"));
        assert_eq!(r["ended_at"], Value::Null);
    }

    #[test]
    fn aggregates_fires_and_excludes_null_card_slug_from_distinct_cards() {
        let c = db();
        c.execute_batch(
            "INSERT INTO brain.brain_fires (session_id, fired_at, card_slug, agent, action, tokens_in, tokens_out, cost_usd, outcome) VALUES \
                ('S-2', '2025-12-31 10:00:00', 'C-1', 'pl', 'pl.plan', 100, 50, 0.10, 'success'), \
                ('S-2', '2025-12-31 10:05:00', 'C-1', 'tl', 'tl.review', 80, 30, 0.05, 'success'), \
                ('S-2', '2025-12-31 10:10:00', 'C-2', 'pl', 'pl.plan', 40, 10, 0.02, 'failed'), \
                ('S-2', '2025-12-31 10:15:00', NULL, 'qa', 'qa.review', 5, 5, 0.01, 'success'), \
                ('S-other', '2025-12-31 11:00:00', 'C-9', 'dev', 'dev.card.add', 999, 999, 9.0, 'success'); \
             INSERT INTO brain.brain_decisions (session_id, decided_at, decision, rationale, inputs_json) VALUES \
                ('S-2', '2025-12-31 09:55:00', 'session_start', 'Session started', '{\"goal\":null,\"started_by\":\"t\"}'), \
                ('S-2', '2025-12-31 10:01:00', 'wait', 'cooldown', NULL), \
                ('S-2', '2025-12-31 10:02:00', 'fire', 'go DEV', NULL);",
        ).unwrap();
        let r = brainSessionSummary(&json!({ "session_id": "S-2" }), &c).unwrap();
        assert_eq!(r["fire_count"], json!(4));
        assert_eq!(r["total_tokens_in"], json!(225));
        assert_eq!(r["total_tokens_out"], json!(95));
        assert_eq!(r["decision_count"], json!(3));
        let mut cards = r["distinct_cards"].as_array().unwrap().clone();
        cards.sort_by(|a, b| a.as_str().cmp(&b.as_str()));
        assert_eq!(cards, vec![json!("C-1"), json!("C-2")]);
        let mut agents = r["distinct_agents"].as_array().unwrap().clone();
        agents.sort_by(|a, b| a.as_str().cmp(&b.as_str()));
        assert_eq!(agents, vec![json!("pl"), json!("qa"), json!("tl")]);
        assert_eq!(r["goal"], Value::Null); // explicit null goal stays null
    }

    #[test]
    fn status_reflects_session_end_override() {
        let c = db();
        c.execute_batch(
            "INSERT INTO brain.brain_decisions (session_id, decided_at, decision, rationale, inputs_json) VALUES \
                ('S-3', '2025-12-31 09:00:00', 'session_start', 'Session started', '{\"goal\":null,\"started_by\":\"t\"}'), \
                ('S-3', '2026-01-01 00:00:00', 'session_end', 'Session aborted: 0 fires, $0.0000', '{\"status\":\"aborted\",\"total_fires\":0,\"total_cost_usd\":0,\"total_tokens_in\":0,\"total_tokens_out\":0,\"notes\":\"user cancelled\"}');",
        ).unwrap();
        let r = brainSessionSummary(&json!({ "session_id": "S-3" }), &c).unwrap();
        assert_eq!(r["status"], json!("aborted"));
        assert_eq!(r["ended_at"], json!("2026-01-01 00:00:00"));
    }

    #[test]
    fn session_end_without_status_field_defaults_to_completed() {
        let c = db();
        c.execute(
            "INSERT INTO brain.brain_decisions (session_id, decided_at, decision, rationale, inputs_json) VALUES ('S-4', '2026-01-01 00:00:00', 'session_end', 'Session ended', '{\"total_fires\":0}')",
            [],
        ).unwrap();
        let r = brainSessionSummary(&json!({ "session_id": "S-4" }), &c).unwrap();
        assert_eq!(r["status"], json!("completed"));
    }

    #[test]
    fn malformed_session_end_inputs_json_is_caught_and_defaults_to_completed() {
        let c = db();
        c.execute(
            "INSERT INTO brain.brain_decisions (session_id, decided_at, decision, rationale, inputs_json) VALUES ('S-5', '2026-01-01 00:00:00', 'session_end', 'Session ended', 'not-json')",
            [],
        ).unwrap();
        let r = brainSessionSummary(&json!({ "session_id": "S-5" }), &c).unwrap();
        assert_eq!(r["status"], json!("completed"));
    }

    #[test]
    fn active_session_with_no_decisions_at_all() {
        let c = db();
        c.execute(
            "INSERT INTO brain.brain_fires (session_id, fired_at, card_slug, agent, action, tokens_in, tokens_out, cost_usd, outcome) VALUES ('S-6', '2025-12-31 10:00:00', 'C-1', 'pl', 'pl.plan', 10, 5, 0.01, 'success')",
            [],
        ).unwrap();
        let r = brainSessionSummary(&json!({ "session_id": "S-6" }), &c).unwrap();
        assert_eq!(r["status"], json!("active"));
        assert_eq!(r["started_at"], Value::Null);
        assert_eq!(r["ended_at"], Value::Null);
        assert_eq!(r["goal"], Value::Null);
        assert_eq!(r["fire_count"], json!(1));
    }

    #[test]
    fn rejects_missing_session_id() {
        let c = db();
        assert!(brainSessionSummary(&json!({}), &c).is_err());
        assert!(brainSessionSummary(&json!({ "session_id": null }), &c).is_err());
    }
}
