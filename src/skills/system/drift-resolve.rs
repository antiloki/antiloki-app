//! drift-resolve.rs — twin of skills/system/drift-resolve.mjs (db WRITE lane, THIN CONSUMER of
//! already-converted twins). NAMES ARE CONTRACT: `driftResolve` (default). Executes the operator's
//! chosen resolution for a drift item: either dispatch a proposed action (A/B/C path — synthesize
//! an interviewer_turns "confirmed" row, then dispatch via `engine_action_run::skillRunAs`) or the
//! free-text path (`interviewer.turn` then `interviewer.dispatch`, also via `skillRunAs`). Every
//! dispatch threads `actor="web-fe:drift-resolver"` and the JS's own `source` literal
//! ('interviewer:confirmed' / 'cli') — NOT plain `skillRun`, which hardcodes `actor="unknown"`
//! (see action-run.rs's own doc comment: this is the exact real-caller pattern that split
//! `skillRun`/`skillRunAs` in the first place).
//!
//! `db.prepare(...).get(dispatch_action)?.owner_agent` is read via `db_json::query_rows` — a
//! missing row AND a present row with a NULL owner_agent column both collapse to a falsy owner
//! (JS `row?.owner_agent` is undefined either way), matching the `owner === 'god'` / `!owner`
//! checks that follow.
//!
//! `skillRunAs`'s Err is a flat `String` (no `.code`/`.name`/`.stack`, same limitation
//! action-run.rs itself documents) — `logError`'s `info.error` is built as `{"message": <full,
//! UNCAPPED err string>}`. The SEPARATE `reason` returned to the caller / passed to `logReceipt`
//! is capped to 600 chars, matching JS's `(e?.message ?? String(e)).slice(0, 600)` vs. the raw
//! `error: e` passed to `logError` (which caps to 1000 internally) — two independent caps, not one
//! feeding the other.
//!
//! Guard-throws (`drift_id` required, empty free-text, neither dispatch_action/free_text
//! provided) are ported as `Err(String)` but NOT fixture-tested (playbook gotcha #9 — the harness
//! only judges the success path, and these error MESSAGES aren't worth pinning byte-exact). The
//! free-text SUCCESS sub-branch (turn_id resolved truthy + `interviewer.dispatch` also succeeds)
//! is ported for real but likewise NOT fixture-tested this round: reaching a truthy `turn_id`
//! through the 2-entry `dispatch_step_skill` table (`card-list` / `with-audit`) needs a full
//! with-audit → card-status-change round trip (sacred_timeline + cards schema) for a branch whose
//! OWN logic (`??` chaining + a final truthy gate) is plain data-shuffling already exercised by
//! other twins' fixtures. This fixture instead covers the two REAL failure shapes of that path
//! (`interviewer.turn` dispatch itself throwing, and it resolving with no truthy turn_id) plus the
//! full A/B/C path (noop / god-owned / not-found / dispatch success / dispatch failure with
//! errorLog + tick-id-derived card_slug + contract_slug/old_slug fallback).
use regex::Regex;
use rusqlite::Connection;
use serde_json::{json, Value};
use std::sync::OnceLock;

fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}
fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) }
fn str_qq(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(),
        Some(o) => o.to_string(),
    }
}
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
fn tmpl(v: Option<&Value>) -> String {
    match v {
        None => "undefined".into(),
        Some(Value::Null) => "null".into(),
        Some(x) => str_qq(Some(x)),
    }
}

fn re_tick() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"^tick-(\d+)$").unwrap())
}

// `function parseTickId(driftId) { const m = String(driftId ?? '').match(/^tick-(\d+)$/); return
// m ? Number(m[1]) : null; }` — driftId is always present/non-null by the time this is called
// (guarded at the top of driftResolve), so str_qq's defined-value arm is the only branch reached.
fn parse_tick_id(drift_id: &Value) -> Option<i64> {
    let s = str_qq(Some(drift_id));
    re_tick().captures(&s).and_then(|c| c[1].parse::<i64>().ok())
}

// `db.prepare('SELECT card_slug FROM brain.driver_ticks WHERE id = ?').get(tickId)?.card_slug ??
// null` wrapped in try/catch -> null. `tickId` falsy (None or 0) short-circuits before any query.
fn card_slug_for_drift(db: &Connection, tick_id: Option<i64>) -> Value {
    let Some(tid) = tick_id.filter(|&t| t != 0) else { return Value::Null };
    crate::db_json::query_rows(db, "SELECT card_slug FROM brain.driver_ticks WHERE id = ?", &[json!(tid)])
        .ok()
        .and_then(|rows| rows.as_array().and_then(|a| a.first()).and_then(|r| r.get("card_slug")).cloned())
        .unwrap_or(Value::Null)
}

// Find-or-create the drift-resolver session (one long-lived interviewer_session per substrate).
fn get_drift_session(db: &Connection) -> Result<i64, String> {
    let rows = crate::db_json::query_rows(
        db,
        "SELECT id FROM interviewer_sessions WHERE user_identifier = 'web-fe:drift-resolver' AND status = 'active' ORDER BY id DESC LIMIT 1",
        &[],
    )?;
    if let Some(id) = rows.as_array().and_then(|a| a.first()).and_then(|r| r.get("id")).and_then(|v| v.as_i64()) {
        return Ok(id);
    }
    let (_, rowid) = crate::db_json::run_write(
        db,
        "INSERT INTO interviewer_sessions (user_identifier, status) VALUES ('web-fe:drift-resolver', 'active')",
        &[],
    )?;
    Ok(rowid)
}

fn synthesize_confirmed_turn(db: &Connection, drift_id: &Value, action: &str, args: &Value, choice_label: Option<&Value>) -> Result<i64, String> {
    let session_id = get_drift_session(db)?;
    let rows = crate::db_json::query_rows(db, "SELECT COUNT(*) AS n FROM interviewer_turns WHERE session_id = ?", &[json!(session_id)])?;
    let turn_idx = rows.as_array().and_then(|a| a.first()).and_then(|r| r.get("n")).and_then(|v| v.as_i64()).unwrap_or(0);

    let label = nz(choice_label).cloned().unwrap_or_else(|| json!(action));
    let user_text = format!("drift:{} \u{2192} {}", tmpl(Some(drift_id)), tmpl(Some(&label)));
    let proposed = serde_json::to_string(&json!([{ "action": action, "args": args.clone(), "risk_tier": "low" }])).unwrap();

    let (_, rowid) = crate::db_json::run_write(
        db,
        "INSERT INTO interviewer_turns (\n      session_id, turn_index, user_text,\n      classified_kind, classification_confidence, classified_reason,\n      proposed_actions_json, confirmed\n    ) VALUES (?, ?, ?, 'mutate', 1.0, ?, ?, 0)",
        &[json!(session_id), json!(turn_idx), json!(user_text), json!("operator picked from drift questionnaire UI"), json!(proposed)],
    )?;
    Ok(rowid)
}

// Best-effort receipt: swallows any db failure (JS `try {...} catch {}`). If the INSERT itself
// fails, the dismissal UPDATE never runs — matches JS's single try block (a thrown INSERT jumps
// straight to catch, skipping the UPDATE), not two independently-swallowed statements.
fn log_receipt(db: &Connection, drift_id: &Value, dispatched: &str, error_reason: Option<&str>, error_event_id: Option<&Value>) {
    let _: Result<(), String> = (|| {
        let message = match error_reason {
            Some(er) => {
                let capped: String = er.chars().take(300).collect();
                format!("Drift {} \u{b7} {} failed: {}", tmpl(Some(drift_id)), dispatched, capped)
            }
            None => format!("Drift {} resolved via {}", tmpl(Some(drift_id)), dispatched),
        };
        let card_slug = card_slug_for_drift(db, parse_tick_id(drift_id));
        let kind = if error_reason.is_some() { "drift-resolution-failed" } else { "drift-resolved" };
        let severity = if error_reason.is_some() { "warn" } else { "info" };
        let payload = json!({
            "drift_id": drift_id.clone(),
            "dispatched": dispatched,
            "error_reason": error_reason,
            "error_event_id": error_event_id.cloned().unwrap_or(Value::Null),
        });
        crate::db_json::run_write(
            db,
            "INSERT INTO notifications (kind, severity, source, message, related_card_slug, payload_json) VALUES (?, ?, 'drift-resolver', ?, ?, ?)",
            &[json!(kind), json!(severity), json!(message), card_slug.clone(), json!(serde_json::to_string(&payload).unwrap())],
        )?;

        if error_reason.is_none() && truthy(Some(&card_slug)) {
            crate::db_json::run_write(
                db,
                &format!("UPDATE notifications SET dismissed_at = {} WHERE related_card_slug = ? AND kind IN ('drift-resolution-failed', 'driver-tick-failed') AND dismissed_at IS NULL", crate::clock::now_sql()),
                &[card_slug],
            )?;
        }
        Ok(())
    })();
}

// `turn?.turn_id ?? turn?.results?.turn?.id ?? turn?.id ?? null` — nullish (not truthy) chaining;
// the caller applies its OWN truthy gate afterward (JS: `if (!turn_id)`), which is why this
// returns the raw nullish-resolved value rather than folding the truthy check in here too.
fn extract_turn_id(turn: &Value) -> Value {
    nz(turn.get("turn_id")).cloned()
        .or_else(|| nz(turn.get("results").and_then(|r| r.get("turn")).and_then(|t| t.get("id"))).cloned())
        .or_else(|| nz(turn.get("id")).cloned())
        .unwrap_or(Value::Null)
}

pub fn driftResolve(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS: `if (!db) throw err.contextMissing(...)` — unreachable in the db lane (db is always present).
    let drift_id = match input.get("drift_id").cloned().filter(|v| truthy(Some(v))) {
        Some(v) => v,
        None => return Err("drift-resolve: drift_id is required".into()),
    };
    let question_id = input.get("question_id");
    let choice_label = input.get("choice_label");
    let dispatch_action_v = input.get("dispatch_action").cloned();
    let dispatch_input = input.get("dispatch_input");
    let free_text_v = input.get("free_text");

    // ── A/B/C path — synthesize confirmed turn + dispatch ───────────────
    if truthy(dispatch_action_v.as_ref()) && !truthy(free_text_v) {
        let dispatch_action = dispatch_action_v.unwrap();
        let dispatch_action_s = dispatch_action.as_str().unwrap_or("");
        let dispatch_input_v = nz(dispatch_input).cloned().unwrap_or_else(|| json!({}));

        if dispatch_action_s == "noop" {
            log_receipt(db, &drift_id, "noop", None, None);
            return Ok(json!({ "ok": true, "drift_id": drift_id, "dispatched_action": "noop", "result": Value::Null }));
        }

        // Early refusal for god actions — interviewer-confirmed cannot reach them by design.
        let owner_val: Option<Value> = crate::db_json::query_rows(db, "SELECT owner_agent FROM actions WHERE name = ?", &[json!(dispatch_action_s)])
            .ok()
            .and_then(|rows| rows.as_array().and_then(|a| a.first()).and_then(|r| r.get("owner_agent")).cloned());

        if owner_val.as_ref().and_then(|v| v.as_str()) == Some("god") {
            let reason = format!("action '{dispatch_action_s}' is god-owned and cannot be dispatched from the drift surface. Open the waterfall and use the per-card RUN/PAUSE button (it carries the god-keyed path).");
            log_receipt(db, &drift_id, dispatch_action_s, Some(&reason), None);
            return Ok(json!({ "ok": false, "drift_id": drift_id, "dispatched_action": dispatch_action, "error": reason }));
        }
        if !truthy(owner_val.as_ref()) {
            let reason = format!("action '{dispatch_action_s}' does not exist in the catalog. The questionnaire may have proposed a stale action name.");
            log_receipt(db, &drift_id, dispatch_action_s, Some(&reason), None);
            return Ok(json!({ "ok": false, "drift_id": drift_id, "dispatched_action": dispatch_action, "error": reason }));
        }

        let turn_id = match synthesize_confirmed_turn(db, &drift_id, dispatch_action_s, &dispatch_input_v, choice_label) {
            Ok(id) => id,
            Err(e) => {
                let reason = format!("failed to synthesize confirmed turn: {e}");
                log_receipt(db, &drift_id, dispatch_action_s, Some(&reason), None);
                return Ok(json!({ "ok": false, "drift_id": drift_id, "dispatched_action": dispatch_action, "error": reason }));
            }
        };

        let action_input = json!({ "action": dispatch_action.clone(), "input": dispatch_input_v.clone() });
        match crate::engine_action_run::skillRunAs(&action_input, db, "web-fe:drift-resolver", Some("interviewer:confirmed"), &json!(turn_id)) {
            Ok(r) => {
                let _ = crate::db_json::run_write(
                    db,
                    "UPDATE interviewer_turns SET confirmed = 1, confirmation_text = ? WHERE id = ?",
                    &[json!("operator-confirmed-via-drift-ui"), json!(turn_id)],
                );
                log_receipt(db, &drift_id, dispatch_action_s, None, None);
                Ok(json!({ "ok": true, "drift_id": drift_id, "dispatched_action": dispatch_action, "result": r, "turn_id": turn_id }))
            }
            Err(e) => {
                let reason: String = e.chars().take(600).collect();
                let tick_id = parse_tick_id(&drift_id);
                let card_slug = card_slug_for_drift(db, tick_id);
                let contract_slug = nz(dispatch_input_v.get("contract_slug"))
                    .or_else(|| nz(dispatch_input_v.get("old_slug")))
                    .cloned()
                    .unwrap_or(Value::Null);
                let mut meta = serde_json::Map::new();
                if let Some(v) = question_id { meta.insert("question_id".into(), v.clone()); }
                if let Some(v) = choice_label { meta.insert("choice_label".into(), v.clone()); }
                let error_log = crate::system_log_error::logError(db, &json!({
                    "error": { "message": e },
                    "action_name": dispatch_action.clone(),
                    "source": "web-fe",
                    "actor": "web-fe:drift-resolver",
                    "input": dispatch_input_v.clone(),
                    "related_card_slug": card_slug,
                    "related_contract_slug": contract_slug,
                    "related_drift_id": drift_id.clone(),
                    "related_tick_id": tick_id.map(|n| json!(n)).unwrap_or(Value::Null),
                    "related_turn_id": turn_id,
                    "meta": Value::Object(meta),
                }));
                let error_event_id = error_log.get("id").cloned();
                log_receipt(db, &drift_id, dispatch_action_s, Some(&reason), error_event_id.as_ref());
                Ok(json!({
                    "ok": false,
                    "drift_id": drift_id,
                    "dispatched_action": dispatch_action,
                    "error": reason,
                    "turn_id": turn_id,
                    "error_event_id": error_log.get("id").cloned().unwrap_or(Value::Null),
                    "error_category": error_log.get("category").cloned().unwrap_or(Value::Null),
                    "error_pattern_key": error_log.get("pattern_key").cloned().unwrap_or(Value::Null),
                }))
            }
        }
    } else if truthy(free_text_v) || input.get("text").map_or(false, |v| v.is_string()) {
        // ── D / free-text path ───────────────────────────────────────────
        let text_field = input.get("text");
        let text_source: Value = if truthy(free_text_v) {
            free_text_v.cloned().unwrap()
        } else if truthy(text_field) {
            text_field.cloned().unwrap()
        } else {
            json!("")
        };
        let text = js_string(&text_source).trim().to_string();
        if text.is_empty() {
            return Err("drift-resolve: free-text resolution chosen but no text provided".into());
        }

        let turn_action_input = json!({
            "action": "interviewer.turn",
            "input": {
                "user_text": text,
                "context": {
                    "drift_id": drift_id.clone(),
                    "question_id": question_id.cloned().unwrap_or(Value::Null),
                    "choice_label": choice_label.cloned().unwrap_or(Value::Null),
                },
            },
        });

        let attempt: Result<Value, String> = (|| {
            let turn = crate::engine_action_run::skillRunAs(&turn_action_input, db, "web-fe:drift-resolver", Some("cli"), &Value::Null)?;
            let turn_id = extract_turn_id(&turn);
            if !truthy(Some(&turn_id)) {
                let reason = "interviewer.turn returned no turn_id";
                log_receipt(db, &drift_id, "free-text", Some(reason), None);
                // .clone(): the closure only BORROWS the outer `drift_id` (it's reused below,
                // after the closure returns, in the Err(e) match arm) — it must not move it.
                return Ok(json!({ "ok": false, "drift_id": drift_id.clone(), "error": reason }));
            }
            let dispatch2_input = json!({
                "action": "interviewer.dispatch",
                "input": { "turn_id": turn_id.clone(), "confirmation_text": "go" },
            });
            let dispatched = crate::engine_action_run::skillRunAs(&dispatch2_input, db, "web-fe:drift-resolver", Some("cli"), &Value::Null)?;
            log_receipt(db, &drift_id, &format!("free-text \u{2192} turn {}", tmpl(Some(&turn_id))), None, None);
            // .clone(): same reasoning as the no-turn-id branch above — the closure only
            // borrows the outer `drift_id`, it must not move it.
            Ok(json!({ "ok": true, "drift_id": drift_id.clone(), "dispatched_action": "interviewer.dispatch", "turn_id": turn_id, "result": dispatched }))
        })();

        match attempt {
            Ok(v) => Ok(v),
            Err(e) => {
                let reason: String = e.chars().take(600).collect();
                log_receipt(db, &drift_id, "free-text", Some(&reason), None);
                Ok(json!({ "ok": false, "drift_id": drift_id, "error": reason }))
            }
        }
    } else {
        Err("drift-resolve: neither dispatch_action nor free_text provided".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ACTIONS_SCHEMA: &str = "CREATE TABLE actions (name TEXT UNIQUE, description TEXT, owner_agent TEXT, steps_json TEXT, returns TEXT, on_failure TEXT, kind TEXT)";
    const RUNTIME_EVENTS_SCHEMA: &str = "CREATE TABLE runtime_events (id INTEGER PRIMARY KEY AUTOINCREMENT, ts TEXT NOT NULL DEFAULT '2026-01-01 00:00:00', parent_event_id INTEGER, actor TEXT NOT NULL, agent TEXT, action TEXT, skill TEXT, duration_ms REAL, outcome TEXT NOT NULL CHECK (outcome IN ('success','error','blocked','skipped','in_flight')), error_code TEXT, context_json TEXT, user_goal_id INTEGER, source TEXT)";
    const CARDS_SCHEMA: &str = "CREATE TABLE cards (slug TEXT PRIMARY KEY, title TEXT, status TEXT, score REAL, updated_at TEXT)";
    const SESSIONS_SCHEMA: &str = "CREATE TABLE interviewer_sessions (id INTEGER PRIMARY KEY AUTOINCREMENT, user_identifier TEXT, status TEXT)";
    const TURNS_SCHEMA: &str = "CREATE TABLE interviewer_turns (id INTEGER PRIMARY KEY AUTOINCREMENT, session_id INTEGER, turn_index INTEGER, user_text TEXT, classified_kind TEXT, classification_confidence REAL, classified_reason TEXT, proposed_actions_json TEXT, confirmed INTEGER, confirmation_text TEXT)";
    const NOTIFICATIONS_SCHEMA: &str = "CREATE TABLE notifications (id INTEGER PRIMARY KEY AUTOINCREMENT, kind TEXT, severity TEXT, source TEXT, message TEXT, related_card_slug TEXT, payload_json TEXT, dismissed_at TEXT)";
    const LAWS_SCHEMA: &str = "CREATE TABLE laws.laws (id INTEGER PRIMARY KEY, slug TEXT, kind TEXT, domain TEXT, statement TEXT, enforcement TEXT, enforcement_hook TEXT, superseded_by INTEGER)";
    const BRAIN_SCHEMA: &str = "CREATE TABLE brain.driver_ticks (id INTEGER PRIMARY KEY, card_slug TEXT)";
    const ERROR_EVENTS_SCHEMA: &str = "CREATE TABLE memory.error_events (id INTEGER PRIMARY KEY AUTOINCREMENT, ts TEXT NOT NULL DEFAULT '2026-01-01 00:00:00', error_code TEXT, error_message TEXT NOT NULL, error_stack TEXT, action_name TEXT, skill_name TEXT, source TEXT, actor TEXT, input_json TEXT, related_card_slug TEXT, related_contract_slug TEXT, related_drift_id TEXT, related_tick_id INTEGER, related_turn_id INTEGER, category TEXT NOT NULL DEFAULT 'unknown', pattern_key TEXT, recovery_action TEXT, recovery_outcome TEXT, recovered_at TEXT, meta_json TEXT)";

    fn seeded_db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS laws; ATTACH ':memory:' AS brain; ATTACH ':memory:' AS memory;").unwrap();
        c.execute_batch(&format!(
            "{ACTIONS_SCHEMA}; {RUNTIME_EVENTS_SCHEMA}; {CARDS_SCHEMA}; {SESSIONS_SCHEMA}; {TURNS_SCHEMA}; \
             {NOTIFICATIONS_SCHEMA}; {LAWS_SCHEMA}; {BRAIN_SCHEMA}; {ERROR_EVENTS_SCHEMA};"
        ))
        .unwrap();
        c
    }

    fn notif_count(c: &Connection) -> i64 {
        c.query_row("SELECT COUNT(*) FROM notifications", [], |r| r.get(0)).unwrap()
    }

    #[test]
    fn noop_short_circuits_before_any_catalog_lookup() {
        let c = seeded_db();
        let r = driftResolve(&json!({ "drift_id": "D-1", "dispatch_action": "noop" }), &c).unwrap();
        assert_eq!(r, json!({ "ok": true, "drift_id": "D-1", "dispatched_action": "noop", "result": null }));
        assert_eq!(notif_count(&c), 1);
        let (kind, severity): (String, String) = c
            .query_row("SELECT kind, severity FROM notifications", [], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap();
        assert_eq!(kind, "drift-resolved");
        assert_eq!(severity, "info");
    }

    #[test]
    fn god_owned_action_is_refused_early() {
        let c = seeded_db();
        c.execute("INSERT INTO actions (name, owner_agent, steps_json, kind) VALUES ('system.card.driver.pause', 'god', '[]', 'atomic')", []).unwrap();
        let r = driftResolve(&json!({ "drift_id": "D-2", "dispatch_action": "system.card.driver.pause" }), &c).unwrap();
        assert_eq!(r["ok"], json!(false));
        assert!(r["error"].as_str().unwrap().contains("is god-owned"));
        assert_eq!(r["dispatched_action"], json!("system.card.driver.pause"));
        assert_eq!(notif_count(&c), 1);
        // no interviewer session/turn was ever synthesized for a god-owned refusal
        let sessions: i64 = c.query_row("SELECT COUNT(*) FROM interviewer_sessions", [], |r| r.get(0)).unwrap();
        assert_eq!(sessions, 0);
    }

    #[test]
    fn stale_action_name_not_in_catalog() {
        let c = seeded_db();
        let r = driftResolve(&json!({ "drift_id": "D-3", "dispatch_action": "stale.action.name" }), &c).unwrap();
        assert_eq!(r["ok"], json!(false));
        assert!(r["error"].as_str().unwrap().contains("does not exist in the catalog"));
    }

    #[test]
    fn dispatch_success_synthesizes_turn_and_confirms_it() {
        let c = seeded_db();
        c.execute("INSERT INTO cards (slug, title, status, score, updated_at) VALUES ('A','card A','backlog',5,'2025-12-01 00:00:00')", []).unwrap();
        c.execute("INSERT INTO cards (slug, title, status, score, updated_at) VALUES ('B','card B','doing',NULL,'2025-12-05 00:00:00')", []).unwrap();
        c.execute(
            "INSERT INTO actions (name, owner_agent, steps_json, returns, kind) VALUES \
             ('board-keeper.card.list', 'board-keeper', '[{\"skill\":\"card-list\",\"input\":{},\"as\":\"cards\"}]', '$.results.cards', 'atomic')",
            [],
        ).unwrap();

        let r = driftResolve(&json!({
            "drift_id": "D-4", "question_id": "Q4", "choice_label": "List cards",
            "dispatch_action": "board-keeper.card.list", "dispatch_input": { "filter": "none" }
        }), &c).unwrap();

        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["dispatched_action"], json!("board-keeper.card.list"));
        assert_eq!(r["result"].as_array().unwrap().len(), 2);
        let turn_id = r["turn_id"].as_i64().unwrap();

        let (confirmed, confirmation_text): (i64, String) = c
            .query_row("SELECT confirmed, confirmation_text FROM interviewer_turns WHERE id = ?", [turn_id], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap();
        assert_eq!(confirmed, 1);
        assert_eq!(confirmation_text, "operator-confirmed-via-drift-ui");
        assert_eq!(notif_count(&c), 1);
    }

    #[test]
    fn dispatch_failure_logs_error_and_resolves_card_slug_via_tick_id() {
        let c = seeded_db();
        c.execute("INSERT INTO actions (name, owner_agent, steps_json, kind) VALUES ('broken.action', 'system', '[{\"skill\":\"no-such-skill\",\"input\":{},\"as\":\"x\"}]', 'atomic')", []).unwrap();
        c.execute("INSERT INTO brain.driver_ticks (id, card_slug) VALUES (7, 'CARD-X')", []).unwrap();

        let r = driftResolve(&json!({
            "drift_id": "tick-7", "dispatch_action": "broken.action",
            "dispatch_input": { "contract_slug": "CT-1", "old_slug": "CT-OLD-IGNORED" }
        }), &c).unwrap();

        assert_eq!(r["ok"], json!(false));
        assert!(r["error"].as_str().unwrap().contains("no entry for 'no-such-skill'"));
        assert!(r["turn_id"].as_i64().is_some());
        assert!(r["error_event_id"].as_i64().is_some());
        assert_eq!(r["error_category"], json!("unknown"));

        let (related_card_slug, related_contract_slug, related_tick_id): (String, String, i64) = c
            .query_row(
                "SELECT related_card_slug, related_contract_slug, related_tick_id FROM memory.error_events WHERE id = ?",
                [r["error_event_id"].as_i64().unwrap()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(related_card_slug, "CARD-X");
        assert_eq!(related_contract_slug, "CT-1"); // contract_slug wins over old_slug
        assert_eq!(related_tick_id, 7);
    }

    #[test]
    fn dispatch_failure_without_tick_pattern_falls_back_to_old_slug() {
        let c = seeded_db();
        c.execute("INSERT INTO actions (name, owner_agent, steps_json, kind) VALUES ('broken.action', 'system', '[{\"skill\":\"no-such-skill\",\"input\":{},\"as\":\"x\"}]', 'atomic')", []).unwrap();

        let r = driftResolve(&json!({
            "drift_id": "drift-99", "dispatch_action": "broken.action",
            "dispatch_input": { "old_slug": "CT-OLD" }
        }), &c).unwrap();

        assert_eq!(r["ok"], json!(false));
        let (related_card_slug, related_contract_slug, related_tick_id): (Option<String>, String, Option<i64>) = c
            .query_row(
                "SELECT related_card_slug, related_contract_slug, related_tick_id FROM memory.error_events WHERE id = ?",
                [r["error_event_id"].as_i64().unwrap()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(related_card_slug, None);
        assert_eq!(related_contract_slug, "CT-OLD");
        assert_eq!(related_tick_id, None);
    }

    #[test]
    fn free_text_path_when_interviewer_turn_action_is_missing() {
        let c = seeded_db();
        let r = driftResolve(&json!({ "drift_id": "D-6", "free_text": "please handle this manually" }), &c).unwrap();
        assert_eq!(r["ok"], json!(false));
        assert_eq!(r["drift_id"], json!("D-6"));
        assert!(r["error"].as_str().unwrap().contains("action 'interviewer.turn' not found"));
        assert_eq!(notif_count(&c), 1);
    }

    #[test]
    fn free_text_path_no_turn_id_extracted() {
        let c = seeded_db();
        c.execute("INSERT INTO cards (slug, title, status, score, updated_at) VALUES ('Z','z','backlog',NULL,'2025-12-01 00:00:00')", []).unwrap();
        c.execute(
            "INSERT INTO actions (name, owner_agent, steps_json, returns, kind) VALUES \
             ('interviewer.turn', 'system', '[{\"skill\":\"card-list\",\"input\":{},\"as\":\"cards\"}]', '$.results.cards', 'atomic')",
            [],
        ).unwrap();
        let r = driftResolve(&json!({ "drift_id": "D-7", "free_text": "needs manual review" }), &c).unwrap();
        assert_eq!(r, json!({ "ok": false, "drift_id": "D-7", "error": "interviewer.turn returned no turn_id" }));
    }

    #[test]
    fn guard_throws_are_not_swallowed() {
        let c = seeded_db();
        assert_eq!(driftResolve(&json!({}), &c).unwrap_err(), "drift-resolve: drift_id is required");
        assert_eq!(driftResolve(&json!({ "drift_id": "D-8" }), &c).unwrap_err(), "drift-resolve: neither dispatch_action nor free_text provided");
        assert_eq!(
            driftResolve(&json!({ "drift_id": "D-9", "free_text": "   " }), &c).unwrap_err(),
            "drift-resolve: free-text resolution chosen but no text provided"
        );
    }
}
