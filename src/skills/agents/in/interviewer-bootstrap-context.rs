//! interviewer-bootstrap-context.rs — twin of skills/agents/in/interviewer-bootstrap-context.mjs
//! (db READ lane, cross-db ATTACH memory). NAMES ARE CONTRACT: `interviewerBootstrapContext` (default).
//!
//! Composes a "WHERE YOU LEFT OFF" block for turn 0 of a new session: most-recent prior
//! session + its last 2 turns, up to 5 open cards, up to 3 recent validated corrections
//! (memory.memories, cross-db ATTACH), and pending high-risk contract approvals (the
//! `detectContractAwaitingHumanApproval` export of skills/system/reconcile.mjs — inlined
//! here as `detect_contract_awaiting_human_approval` rather than called cross-module: the JS
//! import target is a plain read-only query function, not a dispatched skill/twin, so porting
//! its logic directly avoids depending on the whole reconcile.rs surface for one query).
//!
//! Frozen-clock lane: `datetime('now', ?)` in fetch-recent-corrections becomes the frozen
//! literal `datetime('2026-01-01 00:00:00', ?)`, modifier kept verbatim.
// QUALITY: db-read composition twin — no computed risk score (informational bootstrap block)

use rusqlite::Connection;
use serde_json::{json, Value};

const MAX_CONTEXT_CHARS: usize = 500;
const OPEN_CARD_LIMIT: i64 = 5;
const CORRECTION_LIMIT: i64 = 3;
const CORRECTION_WINDOW_DAYS: i64 = 7;

fn truthy(v: Option<&Value>) -> bool { match v { None|Some(Value::Null)=>false, Some(Value::Bool(b))=>*b, Some(Value::Number(n))=>n.as_f64().map_or(false,|f|f!=0.0&&!f.is_nan()), Some(Value::String(s))=>!s.is_empty(), Some(_)=>true } }
fn str_qq(v: Option<&Value>) -> String { match v { None|Some(Value::Null)=>String::new(), Some(Value::String(s))=>s.clone(), Some(Value::Bool(b))=>b.to_string(), Some(n@Value::Number(_))=>n.to_string(), Some(o)=>o.to_string() } } // = JS String(v ?? '')
fn js_string(v: &Value) -> String { match v { Value::Null=>"null".into(), Value::String(s)=>s.clone(), Value::Number(n)=>n.to_string(), Value::Bool(b)=>b.to_string(), Value::Array(a)=>a.iter().map(js_string).collect::<Vec<_>>().join(","), Value::Object(_)=>"[object Object]".into() } } // = JS String(v)
fn tmpl(v: Option<&Value>) -> String { match v { None=>"undefined".into(), Some(Value::Null)=>"null".into(), Some(x)=>str_qq(Some(x)) } } // = JS `${v}`
fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) } // JS `??` right operand: nz(a).or(nz(b))

fn fetch_prior_session(db: &Connection, user_identifier: &Value, current_session_id: &Value) -> Option<Value> {
    let sql = "SELECT id, started_at, last_active_at, status, context_summary_md \
               FROM interviewer_sessions \
               WHERE user_identifier IS ? AND id IS NOT ? \
               ORDER BY COALESCE(last_active_at, started_at) DESC LIMIT 1";
    let rows = crate::db_json::query_rows(db, sql, &[user_identifier.clone(), current_session_id.clone()]).ok()?;
    rows.as_array().and_then(|a| a.first()).cloned()
}

fn fetch_prior_turns(db: &Connection, session_id: &Value, limit: i64) -> Vec<Value> {
    let sql = "SELECT user_text, classified_kind, classified_reason \
               FROM interviewer_turns WHERE session_id = ? ORDER BY turn_index DESC LIMIT ?";
    match crate::db_json::query_rows(db, sql, &[session_id.clone(), json!(limit)]) {
        Ok(v) => { let mut a = v.as_array().cloned().unwrap_or_default(); a.reverse(); a }
        Err(_) => vec![],
    }
}

fn fetch_open_cards(db: &Connection) -> Vec<Value> {
    let sql = "SELECT slug, title, status FROM cards \
               WHERE status NOT IN ('done','blocked','archived','rejected','superseded') \
               ORDER BY updated_at DESC LIMIT ?";
    match crate::db_json::query_rows(db, sql, &[json!(OPEN_CARD_LIMIT)]) {
        Ok(v) => v.as_array().cloned().unwrap_or_default(),
        Err(_) => vec![],
    }
}

// Ported from skills/system/reconcile.mjs's `detectContractAwaitingHumanApproval` export — only
// the `contract_slug` field survives into this module's output (compose() reads nothing else off
// these rows), so the finding object is trimmed to that one field rather than the full shape.
fn detect_contract_awaiting_human_approval(db: &Connection) -> Vec<Value> {
    let sql = "SELECT slug FROM contracts \
               WHERE status = 'tl_approved' AND tier = 'high-risk' AND superseded_by IS NULL \
               ORDER BY updated_at DESC";
    match crate::db_json::query_rows(db, sql, &[]) {
        Ok(v) => v.as_array().cloned().unwrap_or_default().iter()
            .map(|r| json!({ "contract_slug": r.get("slug").cloned().unwrap_or(Value::Null) }))
            .collect(),
        Err(_) => vec![],
    }
}

fn fetch_pending_approvals(db: &Connection) -> Vec<Value> {
    detect_contract_awaiting_human_approval(db)
}

fn fetch_recent_corrections(db: &Connection) -> Vec<Value> {
    let sql = &format!("SELECT id, title, guidance FROM memory.memories \
               WHERE type = 'correction' \
                 AND COALESCE(validated, 0) = 1 \
                 AND COALESCE(superseded_by, '') = '' \
                 AND created_at >= datetime({}, ?) \
               ORDER BY id DESC LIMIT ?", crate::clock::now_sql_inner());
    match crate::db_json::query_rows(db, sql, &[json!(format!("-{} days", CORRECTION_WINDOW_DAYS)), json!(CORRECTION_LIMIT)]) {
        Ok(v) => v.as_array().cloned().unwrap_or_default(),
        Err(_) => vec![],
    }
}

fn compose(prior_session: Option<&Value>, prior_turns: &[Value], open_cards: &[Value], corrections: &[Value], pending_approvals: &[Value]) -> String {
    let mut lines: Vec<String> = Vec::new();

    if !pending_approvals.is_empty() {
        let slugs: Vec<String> = pending_approvals.iter().take(3).map(|p| tmpl(p.get("contract_slug"))).collect();
        let more = if pending_approvals.len() > 3 { format!(" +{} more", pending_approvals.len() - 3) } else { String::new() };
        lines.push(format!(
            "PENDING HUMAN APPROVAL: {} high-risk contract(s) — {}{} — mention these in your reply.",
            pending_approvals.len(), slugs.join(", "), more
        ));
    }

    if let Some(prior) = prior_session {
        let last_turn = prior_turns.last();
        let when = str_qq(nz(prior.get("last_active_at")).or_else(|| nz(prior.get("started_at"))));
        let when16: String = when.chars().take(16).collect();
        let user_text = last_turn.and_then(|lt| lt.get("user_text"));
        if truthy(user_text) {
            let txt: String = js_string(user_text.unwrap()).chars().take(100).collect();
            lines.push(format!("LAST SESSION ({}): \"{}\"", when16, txt));
        } else {
            lines.push(format!("LAST SESSION ({}): no recorded turns.", when16));
        }
    }

    if !open_cards.is_empty() {
        let items: Vec<String> = open_cards.iter().take(3)
            .map(|c| format!("{} ({})", tmpl(c.get("slug")), tmpl(c.get("status"))))
            .collect();
        let suffix = if open_cards.len() > 3 { format!(", +{} more", open_cards.len() - 3) } else { String::new() };
        lines.push(format!("OPEN WORK: {}{}", items.join(", "), suffix));
    }

    if !corrections.is_empty() {
        let items: Vec<String> = corrections.iter().take(2)
            .map(|c| format!("\"{}\"", str_qq(c.get("title")).chars().take(60).collect::<String>()))
            .collect();
        lines.push(format!("RECENT CORRECTIONS: {}", items.join("; ")));
    }

    lines.join("\n").chars().take(MAX_CONTEXT_CHARS).collect()
}

pub fn interviewerBootstrapContext(input: &Value, db: &Connection) -> Result<Value, String> {
    let user_identifier = input.get("user_identifier").cloned().unwrap_or(Value::Null);
    let current_session_id = input.get("current_session_id").cloned().unwrap_or(Value::Null);

    let prior = fetch_prior_session(db, &user_identifier, &current_session_id);
    let cards = fetch_open_cards(db);
    let corrections = fetch_recent_corrections(db);
    let pending_approvals = fetch_pending_approvals(db);

    if prior.is_none() && pending_approvals.is_empty() {
        return Ok(json!({ "context_md": "", "has_context": false, "prior_session_id": null }));
    }

    let prior_id = prior.as_ref().and_then(|p| p.get("id")).cloned().unwrap_or(Value::Null);

    let turns = match &prior {
        Some(p) => fetch_prior_turns(db, &p.get("id").cloned().unwrap_or(Value::Null), 2),
        None => vec![],
    };

    if turns.is_empty() && cards.is_empty() && corrections.is_empty() && pending_approvals.is_empty() {
        return Ok(json!({ "context_md": "", "has_context": false, "prior_session_id": prior_id }));
    }

    let md = compose(prior.as_ref(), &turns, &cards, &corrections, &pending_approvals);
    Ok(json!({
        "context_md": md,
        "has_context": !md.is_empty(),
        "prior_session_id": prior_id,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db_with_schema() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE interviewer_sessions (id INTEGER PRIMARY KEY AUTOINCREMENT, user_identifier TEXT, \
             started_at TEXT, last_active_at TEXT, status TEXT, context_summary_md TEXT);
             CREATE TABLE interviewer_turns (id INTEGER PRIMARY KEY AUTOINCREMENT, session_id INTEGER, \
             turn_index INTEGER, user_text TEXT, classified_kind TEXT, classified_reason TEXT);
             CREATE TABLE cards (slug TEXT PRIMARY KEY, title TEXT, status TEXT, updated_at TEXT);
             CREATE TABLE contracts (slug TEXT PRIMARY KEY, card_slug TEXT, tier TEXT, risk INTEGER, \
             complexity TEXT, updated_at TEXT, status TEXT, superseded_by TEXT);
             ATTACH DATABASE ':memory:' AS memory;
             CREATE TABLE memory.memories (id INTEGER PRIMARY KEY AUTOINCREMENT, type TEXT, validated INTEGER, \
             superseded_by TEXT, created_at TEXT, title TEXT, guidance TEXT);",
        ).unwrap();
        c
    }

    #[test]
    fn silent_when_nothing_at_all() {
        let c = db_with_schema();
        let r = interviewerBootstrapContext(&json!({}), &c).unwrap();
        assert_eq!(r, json!({ "context_md": "", "has_context": false, "prior_session_id": null }));
    }

    #[test]
    fn prior_session_with_no_signal_stays_silent_but_reports_id() {
        let c = db_with_schema();
        c.execute("INSERT INTO interviewer_sessions (id, started_at, last_active_at, status) VALUES (1, '2025-12-01 10:00:00', '2025-12-01 10:00:00', 'closed')", []).unwrap();
        let r = interviewerBootstrapContext(&json!({ "current_session_id": 2 }), &c).unwrap();
        assert_eq!(r, json!({ "context_md": "", "has_context": false, "prior_session_id": 1 }));
    }

    #[test]
    fn pending_approval_surfaces_even_with_no_prior_session() {
        let c = db_with_schema();
        c.execute("INSERT INTO contracts (slug, card_slug, tier, risk, complexity, updated_at, status, superseded_by) VALUES ('CONTRACT-A','CARD-A','high-risk',4,'medium','2025-12-30 00:00:00','tl_approved',NULL)", []).unwrap();
        let r = interviewerBootstrapContext(&json!({}), &c).unwrap();
        assert_eq!(r["context_md"], json!("PENDING HUMAN APPROVAL: 1 high-risk contract(s) — CONTRACT-A — mention these in your reply."));
        assert_eq!(r["has_context"], json!(true));
        assert_eq!(r["prior_session_id"], json!(null));
    }

    #[test]
    fn pending_approvals_over_three_get_a_plus_n_more_suffix() {
        let c = db_with_schema();
        for (slug, ts) in [("CONTRACT-A", "2025-12-30 00:00:00"), ("CONTRACT-B", "2025-12-29 00:00:00"), ("CONTRACT-C", "2025-12-28 00:00:00"), ("CONTRACT-D", "2025-12-27 00:00:00")] {
            c.execute("INSERT INTO contracts (slug, card_slug, tier, risk, complexity, updated_at, status, superseded_by) VALUES (?, 'CARD', 'high-risk', 3, 'low', ?, 'tl_approved', NULL)", rusqlite::params![slug, ts]).unwrap();
        }
        let r = interviewerBootstrapContext(&json!({}), &c).unwrap();
        assert_eq!(r["context_md"], json!("PENDING HUMAN APPROVAL: 4 high-risk contract(s) — CONTRACT-A, CONTRACT-B, CONTRACT-C +1 more — mention these in your reply."));
    }

    #[test]
    fn prior_session_with_last_turn_text() {
        let c = db_with_schema();
        c.execute("INSERT INTO interviewer_sessions (id, started_at, last_active_at, status) VALUES (1, '2025-12-01 10:00:00', '2025-12-20 15:30:00', 'closed')", []).unwrap();
        c.execute("INSERT INTO interviewer_turns (session_id, turn_index, user_text) VALUES (1, 0, 'add a card for the login bug')", []).unwrap();
        c.execute("INSERT INTO interviewer_turns (session_id, turn_index, user_text) VALUES (1, 1, 'now file it')", []).unwrap();
        let r = interviewerBootstrapContext(&json!({ "current_session_id": 2 }), &c).unwrap();
        assert_eq!(r["context_md"], json!("LAST SESSION (2025-12-20 15:30): \"now file it\""));
        assert_eq!(r["prior_session_id"], json!(1));
    }

    #[test]
    fn prior_session_without_turns_reports_no_recorded_turns() {
        let c = db_with_schema();
        c.execute("INSERT INTO interviewer_sessions (id, started_at, last_active_at, status) VALUES (1, '2025-12-01 10:00:00', NULL, 'closed')", []).unwrap();
        c.execute("INSERT INTO cards (slug, title, status, updated_at) VALUES ('CARD-A', 'A', 'backlog', '2025-12-01 00:00:00')", []).unwrap();
        let r = interviewerBootstrapContext(&json!({}), &c).unwrap();
        assert_eq!(r["context_md"], json!("LAST SESSION (2025-12-01 10:00): no recorded turns.\nOPEN WORK: CARD-A (backlog)"));
    }

    #[test]
    fn open_cards_over_three_get_a_plus_n_more_suffix() {
        // The "at least a prior session or a pending approval" gate (JS lines 139-141) means
        // cards alone never surface — anchor with a prior session that has no turns of its own.
        let c = db_with_schema();
        c.execute("INSERT INTO interviewer_sessions (id, started_at, last_active_at, status) VALUES (1, '2025-12-01 10:00:00', NULL, 'closed')", []).unwrap();
        for (slug, ts) in [("CARD-A", "2025-12-04"), ("CARD-B", "2025-12-03"), ("CARD-C", "2025-12-02"), ("CARD-D", "2025-12-01")] {
            c.execute("INSERT INTO cards (slug, title, status, updated_at) VALUES (?, 't', 'backlog', ?)", rusqlite::params![slug, ts]).unwrap();
        }
        let r = interviewerBootstrapContext(&json!({}), &c).unwrap();
        assert_eq!(r["context_md"], json!("LAST SESSION (2025-12-01 10:00): no recorded turns.\nOPEN WORK: CARD-A (backlog), CARD-B (backlog), CARD-C (backlog), +1 more"));
    }

    #[test]
    fn recent_corrections_within_window() {
        // Same anchor requirement as above — corrections alone don't clear the top gate.
        let c = db_with_schema();
        c.execute("INSERT INTO interviewer_sessions (id, started_at, last_active_at, status) VALUES (1, '2025-12-01 10:00:00', NULL, 'closed')", []).unwrap();
        c.execute("INSERT INTO memory.memories (type, validated, superseded_by, created_at, title, guidance) VALUES ('correction', 1, '', '2025-12-30 00:00:00', 'never touch DBs directly', 'go through skills')", []).unwrap();
        c.execute("INSERT INTO memory.memories (type, validated, superseded_by, created_at, title, guidance) VALUES ('correction', 0, '', '2025-12-30 00:00:00', 'unvalidated one', 'skip')", []).unwrap();
        c.execute("INSERT INTO memory.memories (type, validated, superseded_by, created_at, title, guidance) VALUES ('correction', 1, '', '2025-11-01 00:00:00', 'too old', 'skip')", []).unwrap();
        let r = interviewerBootstrapContext(&json!({}), &c).unwrap();
        assert_eq!(r["context_md"], json!("LAST SESSION (2025-12-01 10:00): no recorded turns.\nRECENT CORRECTIONS: \"never touch DBs directly\""));
    }
}
