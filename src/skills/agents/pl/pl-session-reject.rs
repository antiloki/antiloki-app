//! pl-session-reject.rs — twin of skills/agents/pl/pl-session-reject.mjs (db WRITE lane, attached
//! `memory` db via learnInsight). NAMES ARE CONTRACT: `plSessionReject` (default).
//!
//! Flow: validate proposed_card_id + reason (>=10 trimmed chars) -> load pl_proposed_cards row ->
//! require status='awaiting_approval' -> record a learning insight via learnInsight (the chokepoint,
//! NOT a direct memory-add) -> UPDATE pl_proposed_cards status='rejected' -> UPDATE pl_sessions
//! status='abandoned'.
//!
//! Cross-module: learnInsight (skills/ai/learn-insight.mjs, CONVERTED — `crate::ai_learn_insight::learnInsight`).
//!
//! `rejected_by` mirrors pl-session-approve's `approved_by`: JS default is `context?.actor ??
//! 'human:cli'`, only reachable via the literal default in the db lane (never threaded), so an
//! ABSENT input key falls back to "human:cli"; an explicit key (even null) is passed through as-is.
//!
//! JS's `err.contextMissing('pl-session-reject','db')` guard is unreachable here (db lane always
//! injects a Connection) — dropped per the with-audit/action-run precedent (see pl-session-approve.rs).
use rusqlite::Connection;
use serde_json::{Map, Value, json};

fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}
// = JS `${v}` interpolation
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
fn nz(v: Option<&Value>) -> Option<&Value> {
    v.filter(|x| !x.is_null())
}
fn take_chars(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

pub fn plSessionReject(input: &Value, db: &Connection) -> Result<Value, String> {
    let proposed_card_id = input.get("proposed_card_id").cloned().unwrap_or(Value::Null);
    let reason = input.get("reason").cloned().unwrap_or(Value::Null);
    // JS: `rejected_by = context?.actor ?? 'human:cli'` — reachable default (db lane never threads
    // context.actor) is the literal, applied only when the key is absent.
    let rejected_by_val = input.get("rejected_by").cloned().unwrap_or(json!("human:cli"));

    if !truthy(Some(&proposed_card_id)) {
        return Err("pl-session-reject: proposed_card_id is required".into());
    }
    let reason_str = reason.as_str().unwrap_or("");
    if !truthy(Some(&reason)) || reason_str.trim().chars().count() < 10 {
        return Err("pl-session-reject: reason (\u{2265}10 chars) is required".into());
    }

    let proposed_rows = crate::db_json::query_rows(db, "SELECT * FROM pl_proposed_cards WHERE id = ?", &[proposed_card_id.clone()])?;
    let proposed = match proposed_rows.as_array().and_then(|a| a.first()) {
        Some(r) => r.clone(),
        None => return Err(format!("pl-session-reject: proposed card {} not found", tmpl(Some(&proposed_card_id)))),
    };
    let status = proposed.get("status").and_then(|v| v.as_str()).unwrap_or("");
    if status != "awaiting_approval" {
        return Err(format!("pl-session-reject: proposed status='{status}', expected 'awaiting_approval'"));
    }

    let session_id = proposed.get("session_id").cloned().unwrap_or(Value::Null);
    let slug_suggestion = proposed.get("slug_suggestion").cloned().unwrap_or(Value::Null);
    let template_kind = proposed.get("template_kind").cloned().unwrap_or(Value::Null);
    let body_md = proposed.get("body_md").and_then(|v| v.as_str()).unwrap_or("");

    let situation_md = format!(
        "PL proposed card '{}' (template={}) from session {}",
        tmpl(Some(&slug_suggestion)),
        tmpl(Some(&template_kind)),
        tmpl(Some(&session_id))
    );
    let naive_md = take_chars(body_md, 800);
    let corrected_md = format!("User rejected the proposed card. Reason: {}", tmpl(Some(&reason)));
    let tag3 = nz(Some(&template_kind)).cloned().unwrap_or(json!("no-template"));

    let insight = crate::ai_learn_insight::learnInsight(
        &json!({
            "moment_kind": "rejection",
            "emitter": "pl.session.reject",
            "source_ref": format!("pl-session-{}", tmpl(Some(&session_id))),
            "situation_md": situation_md,
            "naive_md": naive_md,
            "corrected_md": corrected_md,
            "tags": ["pl-session", "user-rejection", tag3],
            "created_by": rejected_by_val,
        }),
        db,
    )?;

    crate::db_json::run_write(
        db,
        "UPDATE pl_proposed_cards SET status='rejected', rejection_reason_md=? WHERE id = ?",
        &[reason.clone(), proposed_card_id.clone()],
    )?;
    crate::db_json::run_write(
        db,
        &format!("UPDATE pl_sessions SET status='abandoned', updated_at={} WHERE id = ?", crate::clock::now_sql()),
        &[session_id.clone()],
    )?;

    let mut out = Map::new();
    out.insert("rejected".into(), json!(true));
    out.insert("proposed_card_id".into(), proposed_card_id);
    out.insert("session_id".into(), session_id);
    out.insert("rejected_by".into(), rejected_by_val);
    out.insert("reason".into(), reason);
    out.insert("memory_id".into(), insight.get("memory_id").cloned().unwrap_or(Value::Null));
    out.insert("insight_path".into(), insight.get("insight_path").cloned().unwrap_or(json!([])));
    out.insert("links_added".into(), insight.get("links_added").cloned().unwrap_or(json!([])));
    out.insert(
        "consolidation_triggered".into(),
        insight.get("consolidation_triggered").cloned().unwrap_or(json!(false)),
    );
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "ATTACH ':memory:' AS memory;
            CREATE TABLE pl_sessions (id INTEGER PRIMARY KEY, status TEXT, raw_intent TEXT, approved_card_slug TEXT, updated_at TEXT);
            CREATE TABLE pl_proposed_cards (id INTEGER PRIMARY KEY, session_id INTEGER, slug_suggestion TEXT, title TEXT, body_md TEXT, template_kind TEXT, status TEXT, approved_by TEXT, approved_at TEXT, rejection_reason_md TEXT);
            CREATE TABLE memory.catalog_embeddings (entity_type TEXT NOT NULL, entity_name TEXT NOT NULL, model TEXT NOT NULL, dims INTEGER NOT NULL, vector BLOB NOT NULL);
            CREATE TABLE memory.memories (
              id              INTEGER PRIMARY KEY AUTOINCREMENT,
              type            TEXT    NOT NULL,
              title           TEXT    NOT NULL,
              situation       TEXT    NOT NULL,
              guidance        TEXT    NOT NULL,
              naive_response  TEXT,
              rationale       TEXT,
              tags_json       TEXT    NOT NULL DEFAULT '[]',
              source_card     TEXT,
              source_event    INTEGER,
              source_session  TEXT,
              confidence      INTEGER NOT NULL DEFAULT 0,
              validated       INTEGER NOT NULL DEFAULT 0,
              superseded_by   INTEGER,
              agent_slug      TEXT,
              created_at      TEXT    NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')),
              created_by      TEXT    NOT NULL,
              CHECK (type IN ('lesson', 'preference', 'decision', 'reference', 'correction', 'narrative')),
              CHECK (confidence >= 0 AND confidence <= 1),
              CHECK (validated IN (0, 1))
            );
            CREATE TABLE memory.memory_links (
              id INTEGER PRIMARY KEY AUTOINCREMENT,
              from_memory_id INTEGER NOT NULL,
              to_memory_id INTEGER NOT NULL,
              kind TEXT NOT NULL,
              rationale TEXT,
              weight REAL NOT NULL DEFAULT 1.0,
              created_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')),
              created_by TEXT NOT NULL,
              CHECK (kind IN ('contradicts', 'extends', 'derived_from', 'related', 'corrects')),
              CHECK (from_memory_id != to_memory_id),
              CHECK (weight >= 0.0 AND weight <= 1.0),
              UNIQUE (from_memory_id, to_memory_id, kind)
            );",
        )
        .unwrap();
        c
    }

    #[test]
    fn rejects_proposed_card_and_abandons_session() {
        let c = db();
        c.execute(
            "INSERT INTO pl_sessions (id, status, raw_intent, updated_at) VALUES (1, 'awaiting_approval', 'x', '2025-12-01 00:00:00')",
            [],
        )
        .unwrap();
        c.execute(
            "INSERT INTO pl_proposed_cards (id, session_id, slug_suggestion, title, body_md, template_kind, status) VALUES (1, 1, 'FEAT-X', 't', 'body text', 'feature-add', 'awaiting_approval')",
            [],
        )
        .unwrap();
        let r = plSessionReject(&json!({ "proposed_card_id": 1, "reason": "not the right approach" }), &c).unwrap();
        assert_eq!(r["rejected"], json!(true));
        assert_eq!(r["proposed_card_id"], json!(1));
        assert_eq!(r["session_id"], json!(1));
        assert_eq!(r["rejected_by"], json!("human:cli"));
        assert_eq!(r["memory_id"], json!(1));

        let status: String = c.query_row("SELECT status FROM pl_proposed_cards WHERE id = 1", [], |row| row.get(0)).unwrap();
        assert_eq!(status, "rejected");
        let sstatus: String = c.query_row("SELECT status FROM pl_sessions WHERE id = 1", [], |row| row.get(0)).unwrap();
        assert_eq!(sstatus, "abandoned");
    }

    #[test]
    fn requires_reason_at_least_10_chars() {
        let c = db();
        let r = plSessionReject(&json!({ "proposed_card_id": 1, "reason": "too short" }), &c);
        assert!(r.is_err());
    }

    #[test]
    fn not_found_errors() {
        let c = db();
        let r = plSessionReject(&json!({ "proposed_card_id": 999, "reason": "a good long reason" }), &c);
        assert!(r.is_err());
    }
}
