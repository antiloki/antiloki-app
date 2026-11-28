//! card-restart-from-failure.rs — twin of skills/entities/card-restart-from-failure.mjs (standard,
//! db read+write). NAMES ARE CONTRACT: `cardRestartFromFailure` (default). Operator-level "restart
//! this stuck card": reject the open contract (failure_kind='operator_restart', the cap-bypass
//! signal) and re-arm driver_intent='play'. Only a genuinely shipped card (contract 'done' AND
//! card done/archived) is a true dead end (Err); every other status nudges the scheduler instead.
//!
//! Datetime: all `datetime('now')` writes use the frozen instant `2026-01-01 00:00:00` per the
//! playbook's frozen-clock convention (no fixture-observable divergence since only updated_at is
//! stamped, never selected back into the output).
//!
//! Fixture note (GOTCHA 9): only the SUCCESS branches are fixtured — no-contract nudge,
//! contract-done nudge, contract-rejected re-arm, and the default recoverable-reject path. The two
//! throw paths (card not found, already-shipped) are real `Err(...)` here but not fixture-exercised.
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
fn nz(v: Option<&Value>) -> Option<&Value> {
    v.filter(|x| !x.is_null())
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

// JS: status === 'blocked' || status === 'archived' ? 'backlog' : status
fn backlog_if_blocked_or_archived(status: &Value) -> Value {
    if status == &json!("blocked") || status == &json!("archived") { json!("backlog") } else { status.clone() }
}
// JS: status === 'blocked' ? 'backlog' : status
fn backlog_if_blocked(status: &Value) -> Value {
    if status == &json!("blocked") { json!("backlog") } else { status.clone() }
}

pub fn cardRestartFromFailure(input: &Value, db: &Connection) -> Result<Value, String> {
    let slug_v = input.get("slug").cloned();
    if !truthy(slug_v.as_ref()) {
        return Err("card-restart-from-failure: slug is required".to_string());
    }
    let slug = slug_v.unwrap();
    let slug_s = tmpl(Some(&slug));

    let card_rows = crate::db_json::query_rows(db, "SELECT slug, status, driver_intent FROM cards WHERE slug = ?", &[slug.clone()])?;
    let card = card_rows.as_array().and_then(|a| a.first()).cloned();
    let card = match card {
        Some(c) => c,
        None => return Err(format!("card '{}' not found", slug_s)),
    };
    let card_status = card.get("status").cloned().unwrap_or(Value::Null);
    let card_driver_intent = card.get("driver_intent").cloned();

    // FEAT-OPENROUTER-SESSION-TRACKING: a restart begins a new run.
    crate::ai_run_session::clearCardSession(db, &slug);

    let contract_rows = crate::db_json::query_rows(
        db,
        "SELECT slug, status FROM contracts WHERE card_slug = ? AND COALESCE(superseded_by, '') = '' ORDER BY id DESC LIMIT 1",
        &[slug.clone()],
    )?;
    let contract = contract_rows.as_array().and_then(|a| a.first()).cloned();

    let contract = match contract {
        None => {
            // FIX-RESTART-ON-BACKLOG-NUDGE-INTENT
            let previous_intent = nz(card_driver_intent.as_ref()).cloned().unwrap_or(json!("auto"));
            let previous_status = card_status.clone();
            crate::db_json::run_write(
                db,
                &format!("UPDATE cards SET driver_intent = 'play', status = CASE WHEN status IN ('blocked', 'archived') THEN 'backlog' ELSE status END, updated_at = {} WHERE slug = ?", crate::clock::now_sql()),
                &[slug.clone()],
            )?;
            let new_card_status = backlog_if_blocked_or_archived(&previous_status);
            // card.status is never reassigned after the read, so this OR's second term is
            // always false — kept literal for fidelity with the JS source.
            let changed = previous_intent != json!("play") || previous_status != card_status;
            let mut out = Map::new();
            out.insert("slug".into(), slug.clone());
            out.insert("intent".into(), json!("play"));
            out.insert("previous".into(), previous_intent);
            out.insert("open_contract_slug".into(), Value::Null);
            out.insert("changed".into(), json!(changed));
            out.insert("contract_slug".into(), Value::Null);
            out.insert("prev_card_status".into(), previous_status);
            out.insert("new_card_status".into(), new_card_status);
            out.insert(
                "next_step".into(),
                json!("no contract yet — set driver_intent=play; scheduler will fire pl.session.start on next tick"),
            );
            return Ok(Value::Object(out));
        }
        Some(c) => c,
    };
    let contract_slug = contract.get("slug").cloned().unwrap_or(Value::Null);
    let contract_status = contract.get("status").cloned().unwrap_or(Value::Null);
    let contract_slug_s = tmpl(Some(&contract_slug));

    // MANUAL-REWIND SUPREMACY
    if contract_status == json!("done") {
        if card_status == json!("done") || card_status == json!("archived") {
            return Err(format!(
                "card '{}' already shipped (contract '{}' done, sandbox merged into real code) — nothing to rewind into; if you want to do more work, file a NEW card",
                slug_s, contract_slug_s
            ));
        }
        // FIX-RESTART-ON-DONE-NUDGE-SCHEDULER
        let previous_status = card_status.clone();
        let previous_intent = nz(card_driver_intent.as_ref()).cloned().unwrap_or(json!("auto"));
        crate::db_json::run_write(
            db,
            &format!("UPDATE cards SET driver_intent = 'play', status = CASE WHEN status = 'blocked' THEN 'backlog' ELSE status END, updated_at = {} WHERE slug = ?", crate::clock::now_sql()),
            &[slug.clone()],
        )?;
        let new_card_status = backlog_if_blocked(&previous_status);
        let mut out = Map::new();
        out.insert("slug".into(), slug.clone());
        out.insert("intent".into(), json!("play"));
        out.insert("previous".into(), previous_intent);
        out.insert("open_contract_slug".into(), contract_slug.clone());
        out.insert("changed".into(), json!(true));
        out.insert("contract_slug".into(), contract_slug);
        out.insert("prev_contract_status".into(), contract_status);
        out.insert("new_contract_status".into(), json!("done"));
        out.insert("prev_card_status".into(), previous_status);
        out.insert("new_card_status".into(), new_card_status);
        out.insert(
            "next_step".into(),
            json!("contract is done; nudged scheduler so post-QA pipeline (man.review → keeper → journey → close) fires next tick"),
        );
        return Ok(Value::Object(out));
    }

    // FIX-RESTART-ON-REJECTED-RE-ARMS-REDRAFT
    if contract_status == json!("rejected") {
        crate::db_json::run_write(
            db,
            &format!("UPDATE contracts SET rejection_reason_md = COALESCE(NULLIF(rejection_reason_md, ''), 'Operator restart via card.driver.restart — re-draft using the prior contract + any recorded failure as context.'), failure_kind = 'operator_restart', updated_at = {} WHERE slug = ?", crate::clock::now_sql()),
            &[contract_slug.clone()],
        )?;
        let previous_status = card_status.clone();
        let previous_intent = nz(card_driver_intent.as_ref()).cloned().unwrap_or(json!("auto"));
        crate::db_json::run_write(
            db,
            &format!("UPDATE cards SET driver_intent = 'play', status = CASE WHEN status IN ('blocked', 'archived') THEN 'backlog' ELSE status END, updated_at = {} WHERE slug = ?", crate::clock::now_sql()),
            &[slug.clone()],
        )?;
        let cleared_notifications: i64 = match crate::db_json::run_write(
            db,
            &format!("UPDATE notifications SET dismissed_at = {} WHERE severity = 'error' AND dismissed_at IS NULL AND related_card_slug = ?", crate::clock::now_sql()),
            &[slug.clone()],
        ) {
            Ok((changes, _)) => changes as i64,
            Err(_) => 0, // notifications table absent on legacy DBs
        };
        let new_card_status = backlog_if_blocked_or_archived(&previous_status);
        let mut out = Map::new();
        out.insert("slug".into(), slug.clone());
        out.insert("intent".into(), json!("play"));
        out.insert("previous".into(), previous_intent);
        out.insert("open_contract_slug".into(), contract_slug.clone());
        out.insert("changed".into(), json!(true));
        out.insert("contract_slug".into(), contract_slug);
        out.insert("prev_contract_status".into(), json!("rejected"));
        out.insert("new_contract_status".into(), json!("rejected"));
        out.insert("prev_card_status".into(), previous_status);
        out.insert("new_card_status".into(), new_card_status);
        out.insert("cleared_notifications".into(), json!(cleared_notifications));
        out.insert(
            "next_step".into(),
            json!("contract already rejected — redraft armed (tl.contract.redraft re-writes it using the rejection reason); cleared stale failure notifications so the daemon resumes next tick"),
        );
        return Ok(Value::Object(out));
    }

    // RECOVERABLE (every other non-shipped, non-rejected status).
    let previous_intent = nz(card_driver_intent.as_ref()).cloned().unwrap_or(json!("auto"));
    crate::db_json::run_write(
        db,
        &format!("UPDATE contracts SET status = 'rejected', rejection_reason_md = 'Operator restart via card.driver.restart (TUI Card view r-key). The substrate will auto-redraft on the next driver tick.', failure_kind = 'operator_restart', updated_at = {} WHERE slug = ?", crate::clock::now_sql()),
        &[contract_slug.clone()],
    )?;
    let previous_status = card_status.clone();
    crate::db_json::run_write(
        db,
        &format!("UPDATE cards SET driver_intent = 'play', status = CASE WHEN status IN ('blocked', 'archived') THEN 'backlog' ELSE status END, updated_at = {} WHERE slug = ?", crate::clock::now_sql()),
        &[slug.clone()],
    )?;
    let new_card_status = backlog_if_blocked_or_archived(&previous_status);
    let mut out = Map::new();
    out.insert("slug".into(), slug.clone());
    out.insert("intent".into(), json!("play"));
    out.insert("previous".into(), previous_intent);
    out.insert("open_contract_slug".into(), contract_slug.clone());
    out.insert("changed".into(), json!(true));
    out.insert("contract_slug".into(), contract_slug);
    out.insert("prev_contract_status".into(), contract_status);
    out.insert("new_contract_status".into(), json!("rejected"));
    out.insert("prev_card_status".into(), previous_status);
    out.insert("new_card_status".into(), new_card_status);
    out.insert("next_step".into(), json!("auto-redraft via tl.contract.redraft on next tick"));
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> Connection {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(
            "CREATE TABLE cards (slug TEXT PRIMARY KEY, status TEXT, driver_intent TEXT, updated_at TEXT, openrouter_session_id TEXT);
             CREATE TABLE contracts (id INTEGER PRIMARY KEY AUTOINCREMENT, slug TEXT, card_slug TEXT, status TEXT, superseded_by TEXT, rejection_reason_md TEXT, failure_kind TEXT, updated_at TEXT);
             CREATE TABLE notifications (id INTEGER PRIMARY KEY AUTOINCREMENT, severity TEXT, dismissed_at TEXT, related_card_slug TEXT);",
        )
        .unwrap();
        db
    }

    #[test]
    fn no_contract_nudges_scheduler() {
        let db = setup();
        db.execute("INSERT INTO cards (slug, status, driver_intent) VALUES ('C1', 'backlog', 'stopped')", []).unwrap();
        let out = cardRestartFromFailure(&json!({"slug": "C1"}), &db).unwrap();
        assert_eq!(out["intent"], json!("play"));
        assert_eq!(out["open_contract_slug"], Value::Null);
        assert_eq!(out["changed"], json!(true));
        assert_eq!(out["new_card_status"], json!("backlog"));
        let intent: String = db.query_row("SELECT driver_intent FROM cards WHERE slug='C1'", [], |r| r.get(0)).unwrap();
        assert_eq!(intent, "play");
    }

    #[test]
    fn rejected_contract_rearms_redraft_and_clears_notifications() {
        let db = setup();
        db.execute("INSERT INTO cards (slug, status, driver_intent) VALUES ('C2', 'blocked', 'stopped')", []).unwrap();
        db.execute("INSERT INTO contracts (slug, card_slug, status, superseded_by, rejection_reason_md) VALUES ('C2-C1', 'C2', 'rejected', '', '')", []).unwrap();
        db.execute("INSERT INTO notifications (severity, dismissed_at, related_card_slug) VALUES ('error', NULL, 'C2')", []).unwrap();
        let out = cardRestartFromFailure(&json!({"slug": "C2"}), &db).unwrap();
        assert_eq!(out["new_card_status"], json!("backlog"));
        assert_eq!(out["cleared_notifications"], json!(1));
        let fk: String = db.query_row("SELECT failure_kind FROM contracts WHERE slug='C2-C1'", [], |r| r.get(0)).unwrap();
        assert_eq!(fk, "operator_restart");
    }

    #[test]
    fn recoverable_status_rejects_open_contract() {
        let db = setup();
        db.execute("INSERT INTO cards (slug, status, driver_intent) VALUES ('C3', 'impl', 'stopped')", []).unwrap();
        db.execute("INSERT INTO contracts (slug, card_slug, status, superseded_by) VALUES ('C3-C1', 'C3', 'impl', '')", []).unwrap();
        let out = cardRestartFromFailure(&json!({"slug": "C3"}), &db).unwrap();
        assert_eq!(out["new_contract_status"], json!("rejected"));
        assert_eq!(out["new_card_status"], json!("impl"));
        let cstatus: String = db.query_row("SELECT status FROM contracts WHERE slug='C3-C1'", [], |r| r.get(0)).unwrap();
        assert_eq!(cstatus, "rejected");
    }

    #[test]
    fn done_contract_with_midflight_card_nudges_scheduler() {
        let db = setup();
        db.execute("INSERT INTO cards (slug, status, driver_intent) VALUES ('C4', 'qa', 'stopped')", []).unwrap();
        db.execute("INSERT INTO contracts (slug, card_slug, status, superseded_by) VALUES ('C4-C1', 'C4', 'done', '')", []).unwrap();
        let out = cardRestartFromFailure(&json!({"slug": "C4"}), &db).unwrap();
        assert_eq!(out["new_contract_status"], json!("done"));
        assert_eq!(out["new_card_status"], json!("qa"));
    }

    #[test]
    fn shipped_card_errors() {
        let db = setup();
        db.execute("INSERT INTO cards (slug, status, driver_intent) VALUES ('C5', 'done', 'stopped')", []).unwrap();
        db.execute("INSERT INTO contracts (slug, card_slug, status, superseded_by) VALUES ('C5-C1', 'C5', 'done', '')", []).unwrap();
        let err = cardRestartFromFailure(&json!({"slug": "C5"}), &db).unwrap_err();
        assert!(err.contains("already shipped"));
    }

    #[test]
    fn missing_card_errors() {
        let db = setup();
        let err = cardRestartFromFailure(&json!({"slug": "NOPE"}), &db).unwrap_err();
        assert!(err.contains("not found"));
    }
}
