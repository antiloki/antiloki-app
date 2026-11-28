//! contract-gate-human-reject.rs — twin of skills/contract/contract-gate-human-reject.mjs (db WRITE lane).
//! NAMES ARE CONTRACT: `contractGateHumanReject` (default). HITL rejection for high-risk contracts:
//! writes a correction memory via learn-insight, then either sets status='rejected' (terminal) or
//! leaves status='tl_approved' for TL revision — both branches persist rejection_reason_md + failure_kind.
//!
//! Cross-module: learnInsight (skills/ai/learn-insight.mjs, CONVERTED — crate::ai_learn_insight).
//! Fixture only exercises the success paths (both decision_after branches); the JS source's various
//! AntilokiError throws (missing slug/reason, bad decision_after, not found, wrong tier, learn-insight
//! failure) are reproduced as Err(String) with matching-ish messages but are not fixture-compared
//! (GOTCHA 9 — thrown-error messages are fragile / out of scope for conform).
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
fn take_chars(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

pub fn contractGateHumanReject(input: &Value, db: &Connection) -> Result<Value, String> {
    let empty = json!({});
    let input = if input.is_object() { input } else { &empty };

    let slug = input.get("slug").cloned().unwrap_or(Value::Null);
    let reason = input.get("reason").cloned().unwrap_or(Value::Null);
    // JS destructuring defaults fire only on `undefined` (missing key), NOT on an explicit null.
    let rejected_by = match input.get("rejected_by") { None => json!("human:cli"), Some(v) => v.clone() };
    let decision_after = match input.get("decision_after") { None => json!("await_revision"), Some(v) => v.clone() };

    if !truthy(Some(&slug)) {
        return Err("contract-gate-human-reject: slug is required".into());
    }
    let reason_s = reason.as_str().unwrap_or("");
    if reason.is_null() || reason_s.trim().chars().count() < 10 {
        return Err("contract-gate-human-reject: reason (\u{2265}10 chars) is required".into());
    }
    let decision_after_s = decision_after.as_str().unwrap_or("");
    if decision_after_s != "rejected" && decision_after_s != "await_revision" {
        return Err(format!(
            "contract-gate-human-reject: decision_after must be one of: rejected, await_revision (got '{}')",
            decision_after_s
        ));
    }

    let rows = crate::db_json::query_rows(
        db,
        "SELECT status, tier, card_slug FROM contracts WHERE slug = ?",
        &[slug.clone()],
    )?;
    let row = match rows.as_array().and_then(|a| a.first()) {
        Some(r) => r.clone(),
        None => return Err(format!("contract-gate-human-reject: contract '{}' not found", slug.as_str().unwrap_or(""))),
    };

    let tier = row.get("tier").cloned().unwrap_or(Value::Null);
    if tier.as_str() != Some("high-risk") {
        return Err(format!(
            "contract-gate-human-reject: human rejection only for tier='high-risk' contracts; this one is '{}'",
            tier.as_str().unwrap_or("null")
        ));
    }

    let card_slug = nz(row.get("card_slug")).and_then(|v| v.as_str()).unwrap_or("?").to_string();
    let situation_md = format!("high-risk contract for card '{}' awaited human approval", card_slug);
    let mut naive_obj = Map::new();
    naive_obj.insert("slug".into(), slug.clone());
    naive_obj.insert("tier".into(), tier.clone());
    let naive_md = take_chars(&serde_json::to_string(&Value::Object(naive_obj)).unwrap_or_default(), 400);
    let corrected_md = format!("Human rejected with reason: {}", reason_s);

    let insight = crate::ai_learn_insight::learnInsight(
        &json!({
            "moment_kind": "rejection",
            "emitter": "contract.gate.human.reject",
            "source_ref": slug,
            "situation_md": situation_md,
            "naive_md": naive_md,
            "corrected_md": corrected_md,
            "tags": ["human-rejection", "contract", "high-risk"],
            "created_by": rejected_by,
        }),
        db,
    )
    .map_err(|e| format!("contract-gate-human-reject: failed to record learning insight: {e}"))?;
    let memory_id = nz(insight.get("memory_id")).cloned().unwrap_or(Value::Null);

    let failure_kind = if decision_after_s == "rejected" { "human_terminal" } else { "human_approach_mismatch" };

    if decision_after_s == "rejected" {
        crate::db_json::run_write(
            db,
            &format!("UPDATE contracts SET status='rejected', rejection_reason_md=?, failure_kind=?, updated_at={} WHERE slug = ?", crate::clock::now_sql()),
            &[reason.clone(), json!(failure_kind), slug.clone()],
        )?;
    } else {
        crate::db_json::run_write(
            db,
            &format!("UPDATE contracts SET rejection_reason_md=?, failure_kind=?, updated_at={} WHERE slug = ?", crate::clock::now_sql()),
            &[reason.clone(), json!(failure_kind), slug.clone()],
        )?;
    }

    let mut out = Map::new();
    out.insert("rejected".into(), json!(true));
    out.insert("slug".into(), slug);
    out.insert("decision_after".into(), decision_after.clone());
    out.insert("new_status".into(), json!(if decision_after_s == "rejected" { "rejected" } else { "tl_approved" }));
    out.insert("failure_kind".into(), json!(failure_kind));
    out.insert("memory_id".into(), memory_id);
    out.insert("rejected_by".into(), rejected_by);
    out.insert("reason".into(), reason);
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "ATTACH ':memory:' AS memory;
            CREATE TABLE contracts (
              id INTEGER PRIMARY KEY AUTOINCREMENT,
              slug TEXT NOT NULL UNIQUE,
              title TEXT NOT NULL,
              status TEXT NOT NULL DEFAULT 'proposed',
              template_kind TEXT,
              size TEXT,
              card_slug TEXT,
              files_touched_json TEXT NOT NULL DEFAULT '[]',
              tier TEXT,
              rejection_reason_md TEXT,
              failure_kind TEXT,
              updated_at TEXT
            );
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
              created_by      TEXT    NOT NULL
            );
            CREATE TABLE memory.memory_links (
              id INTEGER PRIMARY KEY AUTOINCREMENT,
              from_memory_id INTEGER NOT NULL,
              to_memory_id INTEGER NOT NULL,
              kind TEXT NOT NULL,
              rationale TEXT,
              weight REAL NOT NULL DEFAULT 1.0,
              created_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')),
              created_by TEXT NOT NULL
            );",
        )
        .unwrap();
        c
    }

    fn seed(c: &Connection, slug: &str) {
        c.execute(
            "INSERT INTO contracts (slug, title, status, tier, card_slug) VALUES (?, 't', 'tl_approved', 'high-risk', 'CARD-X')",
            [slug],
        )
        .unwrap();
    }

    #[test]
    fn rejected_sets_terminal_status() {
        let c = db();
        seed(&c, "C1");
        let r = contractGateHumanReject(
            &json!({ "slug": "C1", "reason": "this approach will break the existing migrations pattern", "decision_after": "rejected" }),
            &c,
        )
        .unwrap();
        assert_eq!(r["new_status"], json!("rejected"));
        assert_eq!(r["failure_kind"], json!("human_terminal"));
        assert!(r["memory_id"].is_number());
        let (status, reason_md, fk): (String, String, String) = c
            .query_row("SELECT status, rejection_reason_md, failure_kind FROM contracts WHERE slug='C1'", [], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
            .unwrap();
        assert_eq!(status, "rejected");
        assert_eq!(fk, "human_terminal");
        assert!(reason_md.contains("break the existing migrations"));
    }

    #[test]
    fn await_revision_keeps_tl_approved() {
        let c = db();
        seed(&c, "C2");
        let r = contractGateHumanReject(
            &json!({ "slug": "C2", "reason": "wrong file_path scope; should touch X not Y", "decision_after": "await_revision" }),
            &c,
        )
        .unwrap();
        assert_eq!(r["new_status"], json!("tl_approved"));
        assert_eq!(r["failure_kind"], json!("human_approach_mismatch"));
        let (status, fk): (String, String) = c
            .query_row("SELECT status, failure_kind FROM contracts WHERE slug='C2'", [], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap();
        assert_eq!(status, "tl_approved");
        assert_eq!(fk, "human_approach_mismatch");
    }

    #[test]
    fn defaults_applied() {
        let c = db();
        seed(&c, "C3");
        let r = contractGateHumanReject(&json!({ "slug": "C3", "reason": "a real reason of ten plus chars" }), &c).unwrap();
        assert_eq!(r["decision_after"], json!("await_revision"));
        assert_eq!(r["rejected_by"], json!("human:cli"));
        let keys: Vec<&String> = r.as_object().unwrap().keys().collect();
        assert_eq!(keys, vec!["rejected", "slug", "decision_after", "new_status", "failure_kind", "memory_id", "rejected_by", "reason"]);
    }

    #[test]
    fn guards_reject() {
        let c = db();
        assert!(contractGateHumanReject(&json!({}), &c).is_err()); // missing slug
        assert!(contractGateHumanReject(&json!({ "slug": "X", "reason": "short" }), &c).is_err()); // short reason
        assert!(contractGateHumanReject(&json!({ "slug": "NOPE", "reason": "a real reason of ten plus chars" }), &c).is_err()); // not found
        c.execute("INSERT INTO contracts (slug, status, tier, title) VALUES ('L','tl_approved','standard','t')", []).unwrap();
        assert!(contractGateHumanReject(&json!({ "slug": "L", "reason": "a real reason of ten plus chars" }), &c).is_err()); // wrong tier
    }
}
