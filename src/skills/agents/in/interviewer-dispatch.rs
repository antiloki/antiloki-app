//! interviewer-dispatch.rs — twin of skills/agents/in/interviewer-dispatch.mjs. NAMES ARE
//! CONTRACT: `interviewerDispatch` (default export, only pub fn).
//!
//! BESPOKE NATIVE (no fixture, no frontier entry): the real work here is executing a
//! previously-stored plan via `action-run`'s `skillRunAs` — which itself does real db writes
//! (runtime_events, law_violations, and whatever the dispatched actions' own steps mutate),
//! reads a real driver-token file from disk for `system.driver.*` targets, and mints a real HMAC
//! over a real wall-clock timestamp. None of that is fixture-reproducible; proven below via
//! `#[cfg(test)]` against a seeded in-memory db, same class as auth-sign.rs / driver-token.rs.
//!
//! Composes 5 already-converted twins by their real pub fn signatures:
//!   - `interviewer_card_proposal_normalize::interviewerCardProposalNormalize` (re-normalize the
//!     stored plan before dispatch, exactly like the JS)
//!   - `interviewer_route::interviewerRoute` + `interviewer_confirm_gate::interviewerConfirmGate`
//!     (re-derive the expected confirmation from the turn's stored classification fields)
//!   - `audit_driver_token::{loadOrCreateDriverToken, signDriverCall}` (driver-namespaced actions
//!     get an HMAC-signed trust attestation, mirroring bin/driver-fire.mjs)
//!   - `engine_action_run::skillRunAs` (the actual dispatcher) — `pub(crate)`, not `pub`, because
//!     JS's `actionRun` is called here with a non-default actor/source ('interviewer:confirmed' /
//!     'interviewer:driver') exactly like action-run.rs's own module doc anticipates for such
//!     callers (workflow-run, action-verify are the other two; this is the third).
//!
//! SCOPE — three fields the JS threads into `actionRun`'s context are NOT observable through
//! `engine_action_run::skillRunAs`'s ported signature (it only accepts actor/source/user_goal_id;
//! see its own module doc for the full list of context fields it deliberately drops) and are
//! therefore not passed through here either — this is a real, disclosed gap, not a silent one:
//!   - `dispatch_turn_id` — read only by the (unconverted) LONE_SKILL_DISPATCH_FORBIDDEN law hook,
//!     which action-run.rs's `KNOWN_HOOKS` table doesn't dispatch to.
//!   - `driver_hmac` / `driver_ts` — the freshly-signed attestation for `system.driver.*` targets.
//!     This twin still calls `signDriverCall` for real (same nonce/wall-clock non-determinism as
//!     JS) so the deterministic SURFACE around the call is faithfully exercised, but the resulting
//!     hmac/ts have nowhere to land in `skillRunAs`'s context — no converted law hook reads them.
//!   - `gitRunner` passthrough — `skillRunAs` has no such parameter; N/A until a git-touching step
//!     skill is added to its `dispatch_step_skill` table.
//! `user_goal_id` (JS: `turn_id`, both branches) DOES thread through — `skillRunAs`'s fourth
//! parameter — matching real runtime_events rows JS would produce for that field.
//!
//! `interviewerClassify` is imported by the JS file but never called in its body (dead import,
//! verified by reading the source) — not ported, nothing to port.
//!
//! Error messages mirror `engine/error.mjs`'s `AntilokiError` "{skill}: {cause}" shape, matching
//! every other converted twin's `Err(String)` convention (`AntilokiError` itself is deferred to
//! the dispatch layer per error.rs's own module doc).

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

fn is_driver_action(name: Option<&str>) -> bool {
    name.map_or(false, |n| n.starts_with("system.driver."))
}

// Howard Hinnant's civil_from_days (inverse of driver-token.rs's days_from_civil), used only to
// render a real ISO-8601 wall-clock timestamp for `signDriverCall`'s `ts` — same non-determinism
// class as auth-sign.rs's `Date.now()`, no fixture depends on the exact value.
fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

fn now_iso() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let total_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    let days = total_ms.div_euclid(86_400_000);
    let ms_of_day = total_ms.rem_euclid(86_400_000);
    let (y, m, d) = civil_from_days(days);
    let hour = ms_of_day / 3_600_000;
    let minute = (ms_of_day / 60_000) % 60;
    let second = (ms_of_day / 1000) % 60;
    let millis = ms_of_day % 1000;
    format!("{y:04}-{m:02}-{d:02}T{hour:02}:{minute:02}:{second:02}.{millis:03}Z")
}

/// `export default async function interviewerDispatch(input, context)`. `context` collapses to
/// `db: &Connection` — the `!db` guard is unreachable-by-construction in Rust (same convention
/// action-run.rs and with-audit.rs use for their own copies of this exact guard).
pub fn interviewerDispatch(input: &Value, db: &Connection) -> Result<Value, String> {
    let turn_id = input.get("turn_id");
    if !truthy(turn_id) {
        return Err("interviewer-dispatch: turn_id is required".into());
    }
    let turn_id = turn_id.unwrap().clone();

    match input.get("confirmation_text") {
        None | Some(Value::Null) => {
            return Err("interviewer-dispatch: confirmation_text is required".into());
        }
        _ => {}
    }
    let confirmation_text = input.get("confirmation_text").cloned().unwrap();

    let rows = crate::db_json::query_rows(
        db,
        "SELECT * FROM interviewer_turns WHERE id = ?",
        &[turn_id.clone()],
    )?;
    let turn = match rows.as_array().and_then(|a| a.first()) {
        Some(t) => t.clone(),
        None => {
            return Err(format!(
                "interviewer-dispatch: turn '{}' not found",
                tmpl(Some(&turn_id))
            ));
        }
    };

    if turn.get("confirmed").and_then(Value::as_i64) == Some(1) {
        let dispatched = crate::engine_json_utils::loadJson(
            turn.get("dispatched_action_ids_json").unwrap_or(&Value::Null),
            &json!([]),
        );
        return Ok(json!({ "already_confirmed": true, "dispatched_action_ids": dispatched }));
    }

    let stored_plan = crate::engine_json_utils::loadJson(
        turn.get("proposed_actions_json").unwrap_or(&Value::Null),
        &json!([]),
    );
    let normalized = crate::agents_in_interviewer_card_proposal_normalize::interviewerCardProposalNormalize(
        &json!({ "plan": stored_plan }),
        db,
    )?;
    let plan: Vec<Value> = normalized.get("plan").and_then(Value::as_array).cloned().unwrap_or_default();
    if plan.is_empty() {
        return Err(format!(
            "interviewer-dispatch: turn '{}' has no proposed actions to dispatch",
            tmpl(Some(&turn_id))
        ));
    }

    let suggested_actions: Vec<Value> = plan
        .iter()
        .map(|p| {
            let mut m = Map::new();
            if let Some(a) = p.get("action") {
                m.insert("name".into(), a.clone());
            }
            m.insert("args".into(), p.get("args").cloned().unwrap_or(json!({})));
            if let Some(rt) = p.get("risk_tier") {
                m.insert("risk_tier".into(), rt.clone());
            }
            Value::Object(m)
        })
        .collect();
    let classification = json!({
        "kind": turn.get("classified_kind").cloned().unwrap_or(Value::Null),
        "confidence": turn.get("classification_confidence").cloned().unwrap_or(Value::Null),
        "reason": turn.get("classified_reason").cloned().unwrap_or(Value::Null),
        "suggested_actions": suggested_actions,
    });

    let route = crate::agents_in_interviewer_route::interviewerRoute(
        &json!({ "classification": classification }),
        db,
    )?;
    let mut gate_input = route.as_object().cloned().unwrap_or_default();
    gate_input.insert("classification".into(), classification.clone());
    let gate = crate::agents_in_interviewer_confirm_gate::interviewerConfirmGate(&Value::Object(gate_input), db)?;

    let bypass = truthy(gate.get("bypass"));
    if !bypass && gate.get("expected_confirmation") != Some(&confirmation_text) {
        let hard_gate = truthy(gate.get("hard_gate"));
        return Err(format!(
            "interviewer-dispatch: confirmation rejected: expected '{}', got '{}'. {}",
            tmpl(gate.get("expected_confirmation")),
            tmpl(Some(&confirmation_text)),
            if hard_gate { "Critical destructive ops require the literal slug." } else { "Type 'go' to confirm." }
        ));
    }

    let has_driver_action = plan.iter().any(|p| is_driver_action(p.get("action").and_then(Value::as_str)));
    let driver_token: Option<String> = if has_driver_action {
        Some(crate::audit_driver_token::loadOrCreateDriverToken()?)
    } else {
        None
    };

    let mut results: Vec<Value> = Vec::new();
    for p in &plan {
        let action_name = p.get("action").and_then(Value::as_str).unwrap_or("").to_string();
        let args = p.get("args").cloned().unwrap_or(json!({}));
        let action_input = json!({ "action": action_name, "input": args });

        let dispatch_result = if is_driver_action(Some(action_name.as_str())) {
            let ts = now_iso();
            let _signed = crate::audit_driver_token::signDriverCall(&json!({
                "action": action_name, "input": args, "ts": ts,
                "token": driver_token.clone(), "actor": "interviewer:driver",
            }));
            crate::engine_action_run::skillRunAs(&action_input, db, "interviewer:driver", Some("driver"), &turn_id)
        } else {
            crate::engine_action_run::skillRunAs(&action_input, db, "interviewer:confirmed", Some("interviewer:confirmed"), &turn_id)
        };

        match dispatch_result {
            Ok(r) => results.push(json!({ "action": action_name, "ok": true, "result": r })),
            Err(e) => results.push(json!({ "action": action_name, "ok": false, "error": e })),
        }
    }

    let any_ok = results.iter().any(|r| r.get("ok") == Some(&Value::Bool(true)));
    if any_ok {
        let max_rows = crate::db_json::query_rows(db, "SELECT MAX(id) AS m FROM sacred_timeline", &[])?;
        let last_id = max_rows.as_array().and_then(|a| a.first()).and_then(|r| r.get("m")).cloned().unwrap_or(Value::Null);
        let dispatched_json = serde_json::to_string(&json!([last_id])).unwrap();
        crate::db_json::run_write(
            db,
            "UPDATE interviewer_turns SET confirmed=1, dispatched_action_ids_json=? WHERE id = ?",
            &[json!(dispatched_json), turn_id.clone()],
        )?;
    }

    let errors: Vec<Value> = results
        .iter()
        .filter(|r| r.get("ok") != Some(&Value::Bool(true)))
        .map(|r| json!({ "action": r.get("action").cloned().unwrap_or(Value::Null), "error": r.get("error").cloned().unwrap_or(Value::Null) }))
        .collect();

    Ok(json!({
        "turn_id": turn_id,
        "confirmed": any_ok,
        "ok": any_ok,
        "failed": !any_ok,
        "errors": errors,
        "results": results,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    // SAFETY: ANTILOKI_HOME is process-global; shared crate-wide lock, same convention as
    // driver-token.rs / auth-sign.rs.
    fn with_home<T>(antiloki_home: &str, f: impl FnOnce() -> T) -> T {
        let _guard = crate::test_env_lock::ENV_LOCK.lock().unwrap();
        unsafe {
            std::env::set_var("ANTILOKI_HOME", antiloki_home);
        }
        let result = f();
        unsafe {
            std::env::remove_var("ANTILOKI_HOME");
        }
        result
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!("antiloki-interviewer-dispatch-rs-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    const ACTIONS_SCHEMA: &str = "CREATE TABLE actions (name TEXT UNIQUE, description TEXT, owner_agent TEXT, steps_json TEXT, returns TEXT, on_failure TEXT, kind TEXT)";
    const RUNTIME_EVENTS_SCHEMA: &str = "CREATE TABLE runtime_events (id INTEGER PRIMARY KEY AUTOINCREMENT, ts TEXT NOT NULL DEFAULT '2026-01-01 00:00:00', parent_event_id INTEGER, actor TEXT NOT NULL, agent TEXT, action TEXT, skill TEXT, duration_ms REAL, outcome TEXT NOT NULL CHECK (outcome IN ('success','error','blocked','skipped','in_flight')), error_code TEXT, context_json TEXT, user_goal_id INTEGER, source TEXT)";
    const CARDS_SCHEMA: &str = "CREATE TABLE cards (slug TEXT, status TEXT, score REAL)";
    const LAWS_SCHEMA: &str = "CREATE TABLE laws.laws (id INTEGER PRIMARY KEY, slug TEXT, kind TEXT, domain TEXT, statement TEXT, enforcement TEXT, enforcement_hook TEXT, superseded_by INTEGER)";
    const LAW_VIOLATIONS_SCHEMA: &str = "CREATE TABLE laws.law_violations (id INTEGER PRIMARY KEY, law_id INTEGER NOT NULL, attempted_at TEXT NOT NULL DEFAULT '2026-01-01 00:00:00', attempted_by TEXT NOT NULL, agent_name TEXT, action_name TEXT, context_json TEXT, outcome TEXT NOT NULL, CHECK (outcome IN ('blocked', 'warned', 'escaped', 'allowed_exception')))";
    const INTERVIEWER_TURNS_SCHEMA: &str = "CREATE TABLE interviewer_turns (id INTEGER PRIMARY KEY AUTOINCREMENT, session_id INTEGER NOT NULL, ts TEXT NOT NULL DEFAULT '2026-01-01 00:00:00', turn_index INTEGER NOT NULL, user_text TEXT NOT NULL, classified_kind TEXT NOT NULL, classification_confidence REAL, classified_reason TEXT, proposed_actions_json TEXT NOT NULL DEFAULT '[]', confirmed INTEGER NOT NULL DEFAULT 0, dispatched_action_ids_json TEXT, interviewer_response_md TEXT)";
    const SACRED_TIMELINE_SCHEMA: &str = "CREATE TABLE sacred_timeline (id INTEGER PRIMARY KEY AUTOINCREMENT, entity_id TEXT)";

    fn seeded_db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS laws;").unwrap();
        c.execute_batch(&format!(
            "{ACTIONS_SCHEMA}; {RUNTIME_EVENTS_SCHEMA}; {CARDS_SCHEMA}; {LAWS_SCHEMA}; {LAW_VIOLATIONS_SCHEMA};
             {INTERVIEWER_TURNS_SCHEMA}; {SACRED_TIMELINE_SCHEMA};
             INSERT INTO cards VALUES ('A','doing',5),('B','backlog',NULL);
             INSERT INTO actions (name, description, owner_agent, steps_json, returns, kind) VALUES
               ('board-keeper.card.add', 'add a card', 'board-keeper', '[{{\"skill\":\"card-list\",\"input\":{{}},\"as\":\"cards\"}}]', '$.results.cards', 'atomic'),
               ('system.driver.echo', 'driver echo', 'system', '[{{\"skill\":\"card-list\",\"input\":{{}},\"as\":\"cards\"}}]', '$.results.cards', 'atomic');"
        ))
        .unwrap();
        c
    }

    fn insert_turn(c: &Connection, kind: &str, plan_json: &str, confirmed: i64) -> i64 {
        c.execute(
            "INSERT INTO interviewer_turns (session_id, turn_index, user_text, classified_kind, proposed_actions_json, confirmed) VALUES (1, 1, 'do it', ?, ?, ?)",
            rusqlite::params![kind, plan_json, confirmed],
        ).unwrap();
        c.last_insert_rowid()
    }

    #[test]
    fn missing_turn_id_and_confirmation_text() {
        let c = seeded_db();
        assert_eq!(
            interviewerDispatch(&json!({ "confirmation_text": "go" }), &c).unwrap_err(),
            "interviewer-dispatch: turn_id is required"
        );
        assert_eq!(
            interviewerDispatch(&json!({ "turn_id": 1 }), &c).unwrap_err(),
            "interviewer-dispatch: confirmation_text is required"
        );
        // confirmation_text: "" is NOT null — passes the required-check (fails later for another reason).
        let e = interviewerDispatch(&json!({ "turn_id": 999, "confirmation_text": "" }), &c).unwrap_err();
        assert_eq!(e, "interviewer-dispatch: turn '999' not found");
    }

    #[test]
    fn turn_not_found() {
        let c = seeded_db();
        let e = interviewerDispatch(&json!({ "turn_id": 42, "confirmation_text": "go" }), &c).unwrap_err();
        assert_eq!(e, "interviewer-dispatch: turn '42' not found");
    }

    #[test]
    fn already_confirmed_short_circuits() {
        let c = seeded_db();
        let id = insert_turn(&c, "read", "[]", 1);
        c.execute(
            "UPDATE interviewer_turns SET dispatched_action_ids_json = '[7]' WHERE id = ?",
            [id],
        ).unwrap();
        let r = interviewerDispatch(&json!({ "turn_id": id, "confirmation_text": "anything" }), &c).unwrap();
        assert_eq!(r, json!({ "already_confirmed": true, "dispatched_action_ids": [7] }));
    }

    #[test]
    fn empty_plan_errors() {
        let c = seeded_db();
        let id = insert_turn(&c, "read", "[]", 0);
        let e = interviewerDispatch(&json!({ "turn_id": id, "confirmation_text": "go" }), &c).unwrap_err();
        assert_eq!(e, format!("interviewer-dispatch: turn '{id}' has no proposed actions to dispatch"));
    }

    #[test]
    fn low_risk_bypass_dispatches_and_confirms() {
        let c = seeded_db();
        let plan = r#"[{"action":"board-keeper.card.add","args":{}}]"#;
        let id = insert_turn(&c, "read", plan, 0);
        let r = interviewerDispatch(&json!({ "turn_id": id, "confirmation_text": "" }), &c).unwrap();
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["confirmed"], json!(true));
        assert_eq!(r["failed"], json!(false));
        assert_eq!(r["errors"], json!([]));
        assert_eq!(r["results"][0]["ok"], json!(true));

        let confirmed = crate::db_json::query_rows(&c, "SELECT confirmed FROM interviewer_turns WHERE id = ?", &[json!(id)]).unwrap();
        assert_eq!(confirmed[0]["confirmed"], json!(1));
    }

    #[test]
    fn medium_risk_wrong_confirmation_rejected() {
        let c = seeded_db();
        let plan = r#"[{"action":"board-keeper.card.add","args":{}}]"#;
        let id = insert_turn(&c, "mutate", plan, 0);
        let e = interviewerDispatch(&json!({ "turn_id": id, "confirmation_text": "nope" }), &c).unwrap_err();
        assert_eq!(
            e,
            "interviewer-dispatch: confirmation rejected: expected 'go', got 'nope'. Type 'go' to confirm."
        );
        // rejected before dispatch — turn stays unconfirmed
        let confirmed = crate::db_json::query_rows(&c, "SELECT confirmed FROM interviewer_turns WHERE id = ?", &[json!(id)]).unwrap();
        assert_eq!(confirmed[0]["confirmed"], json!(0));
    }

    #[test]
    fn medium_risk_correct_confirmation_dispatches() {
        let c = seeded_db();
        let plan = r#"[{"action":"board-keeper.card.add","args":{}}]"#;
        let id = insert_turn(&c, "mutate", plan, 0);
        let r = interviewerDispatch(&json!({ "turn_id": id, "confirmation_text": "go" }), &c).unwrap();
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["confirmed"], json!(true));
    }

    #[test]
    fn partial_failure_still_confirms_and_reports_errors() {
        let c = seeded_db();
        let plan = r#"[{"action":"board-keeper.card.add","args":{}},{"action":"no.such.action","args":{}}]"#;
        let id = insert_turn(&c, "read", plan, 0);
        let r = interviewerDispatch(&json!({ "turn_id": id, "confirmation_text": "" }), &c).unwrap();
        assert_eq!(r["ok"], json!(true)); // at least one action landed
        assert_eq!(r["confirmed"], json!(true));
        assert_eq!(r["failed"], json!(false));
        let errors = r["errors"].as_array().unwrap();
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0]["action"], json!("no.such.action"));
        assert!(errors[0]["error"].as_str().unwrap().contains("not found"));
    }

    #[test]
    fn all_failed_dispatch_leaves_turn_unconfirmed() {
        let c = seeded_db();
        let plan = r#"[{"action":"no.such.action","args":{}}]"#;
        let id = insert_turn(&c, "read", plan, 0);
        let r = interviewerDispatch(&json!({ "turn_id": id, "confirmation_text": "" }), &c).unwrap();
        assert_eq!(r["ok"], json!(false));
        assert_eq!(r["confirmed"], json!(false));
        assert_eq!(r["failed"], json!(true));
        let confirmed = crate::db_json::query_rows(&c, "SELECT confirmed FROM interviewer_turns WHERE id = ?", &[json!(id)]).unwrap();
        assert_eq!(confirmed[0]["confirmed"], json!(0));
    }

    #[test]
    fn driver_action_dispatch_signs_a_real_hmac_and_loads_a_real_token() {
        let dir = temp_dir("driver");
        let c = seeded_db();
        let plan = r#"[{"action":"system.driver.echo","args":{}}]"#;
        let id = insert_turn(&c, "mutate", plan, 0);
        let r = with_home(dir.to_str().unwrap(), || {
            interviewerDispatch(&json!({ "turn_id": id, "confirmation_text": "go" }), &c).unwrap()
        });
        assert_eq!(r["ok"], json!(true));
        assert!(dir.join("auth").join("driver.token").exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn now_iso_produces_a_well_formed_timestamp() {
        let s = now_iso();
        assert_eq!(s.len(), 24);
        assert_eq!(&s[4..5], "-");
        assert_eq!(&s[10..11], "T");
        assert_eq!(&s[23..24], "Z");
    }
}
