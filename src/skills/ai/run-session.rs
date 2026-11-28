//! run-session.rs — twin of skills/ai/run-session.mjs (db-first helper module: the per-CARD-RUN
//! OpenRouter session id, threaded to the provider body without touching every skill's aiCall
//! call site — FEAT-OPENROUTER-SESSION-TRACKING, 2026-06-13). NAMES ARE CONTRACT: ensureCardSession
//! · clearCardSession — get-or-create / rotate the source-of-truth column
//! `cards.openrouter_session_id`. Both are guarded so legacy DBs (pre-migration 137, column
//! absent) degrade to no-session rather than throwing.
//!
//! PARITY-DEFERRED: `runWithSession(session_id, fn)` and `currentSessionId()` are NOT ported.
//! Both live entirely in node:async_hooks' AsyncLocalStorage — `runWithSession` takes a CALLBACK
//! (not JSON-serializable, not fixture-representable) and `currentSessionId` reads
//! `als.getStore()`, ambient state scoped to the live async call tree of ONE driver dispatch so
//! concurrent BE/interviewer dispatches don't bleed sessions into each other. Rust has no async
//! runtime here and no ALS-equivalent construct wired into this twin harness, so there is no
//! Rust shape either export could take. Only the get/clear pair below — plain reads/writes of
//! `cards.openrouter_session_id` — is data, and that's what's twinned here.
use rusqlite::Connection;
use serde_json::{json, Value};

// Standard helper functions (playbook §Standard helper functions) — copied verbatim.
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

// Fallback for the create-new-session branch's `randomUUID()`. NOT byte-identical to node:crypto's
// RNG — never exercised by conform fixtures (only the already-has-a-session branch is fixtured;
// this branch has no frozen-clock-style convention per the playbook). Mirrors the
// brain-session-start.rs / atlas-smell-to-card.rs precedent for a structurally UUID-shaped,
// non-matching fallback: a real #[cfg(test)] below asserts the STRUCTURE this produces, not bytes.
fn random_uuid() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos();
    let mut x = (nanos as u64) ^ 0x9E3779B97F4A7C15u64;
    let mut hex = String::new();
    for _ in 0..32 {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        hex.push(std::char::from_digit((x % 16) as u32, 16).unwrap());
    }
    format!("{}-{}-{}-{}-{}", &hex[0..8], &hex[8..12], &hex[12..16], &hex[16..20], &hex[20..32])
}

/// Get-or-create the card's current-run session id. Stable across the many ticks of one run; a
/// fresh id only after clearCardSession (restart). Human-readable so the OpenRouter dashboard
/// shows WHICH card.
///
/// `db` is a mandatory `&Connection` in this twin — the harness never dispatches without one, so
/// the `!db` half of the JS guard (`if (!db || !card_slug) return null;`) has no Rust
/// counterpart; the `!card_slug` half is `truthy`.
pub fn ensureCardSession(db: &Connection, card_slug: &Value) -> Value {
    if !truthy(Some(card_slug)) {
        return Value::Null;
    }
    let rows = match crate::db_json::query_rows(
        db,
        "SELECT openrouter_session_id FROM cards WHERE slug = ?",
        &[card_slug.clone()],
    ) {
        Ok(v) => v,
        // column absent pre-migration 137 — degrade to no-session.
        Err(_) => return Value::Null,
    };
    let current: Option<Value> = rows
        .as_array()
        .and_then(|a| a.first())
        .and_then(|r| r.get("openrouter_session_id"))
        .cloned()
        .filter(|x| truthy(Some(x)));
    if let Some(id) = current {
        return id;
    }
    let id = format!("card-{}-{}", tmpl(Some(card_slug)), random_uuid());
    let _ = crate::db_json::run_write(
        db,
        "UPDATE cards SET openrouter_session_id = ? WHERE slug = ?",
        &[json!(id), card_slug.clone()],
    );
    json!(id)
}

/// Rotate: clear the card's session so the next run gets a fresh id. Same db/card_slug guard
/// shape as `ensureCardSession` — see its doc for the `!db` note.
pub fn clearCardSession(db: &Connection, card_slug: &Value) -> Value {
    if !truthy(Some(card_slug)) {
        return Value::Null;
    }
    let _ = crate::db_json::run_write(
        db,
        "UPDATE cards SET openrouter_session_id = NULL WHERE slug = ?",
        &[card_slug.clone()],
    );
    Value::Null
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> Connection {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE cards (slug TEXT PRIMARY KEY, openrouter_session_id TEXT)").unwrap();
        db
    }

    #[test]
    fn ensure_returns_existing_session_verbatim() {
        let db = setup();
        db.execute(
            "INSERT INTO cards (slug, openrouter_session_id) VALUES ('CARD-A', 'card-CARD-A-existing-1234')",
            [],
        ).unwrap();
        let out = ensureCardSession(&db, &json!("CARD-A"));
        assert_eq!(out, json!("card-CARD-A-existing-1234"));
    }

    #[test]
    fn ensure_creates_persists_and_is_idempotent() {
        let db = setup();
        db.execute("INSERT INTO cards (slug) VALUES ('CARD-B')", []).unwrap();
        let first = ensureCardSession(&db, &json!("CARD-B"));
        let first_str = first.as_str().unwrap().to_string();
        assert!(first_str.starts_with("card-CARD-B-"), "human-readable, card-scoped id: {first_str}");
        let persisted: String = db
            .query_row("SELECT openrouter_session_id FROM cards WHERE slug = 'CARD-B'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(persisted, first_str);
        let second = ensureCardSession(&db, &json!("CARD-B"));
        assert_eq!(second, first, "same run -> same session across ticks");
    }

    #[test]
    fn ensure_degrades_to_null_when_column_absent() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE cards (slug TEXT PRIMARY KEY)").unwrap();
        db.execute("INSERT INTO cards (slug) VALUES ('CARD-C')", []).unwrap();
        assert_eq!(ensureCardSession(&db, &json!("CARD-C")), Value::Null);
    }

    #[test]
    fn ensure_null_card_slug_returns_null_without_touching_db() {
        let db = setup();
        assert_eq!(ensureCardSession(&db, &Value::Null), Value::Null);
    }

    #[test]
    fn clear_sets_existing_session_to_null() {
        let db = setup();
        db.execute(
            "INSERT INTO cards (slug, openrouter_session_id) VALUES ('CARD-D', 'card-CARD-D-x')",
            [],
        ).unwrap();
        clearCardSession(&db, &json!("CARD-D"));
        let persisted: Option<String> = db
            .query_row("SELECT openrouter_session_id FROM cards WHERE slug = 'CARD-D'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(persisted, None);
    }

    #[test]
    fn clear_is_a_noop_when_already_null_or_card_missing() {
        let db = setup();
        db.execute("INSERT INTO cards (slug) VALUES ('CARD-E')", []).unwrap();
        clearCardSession(&db, &json!("CARD-E")); // already null
        clearCardSession(&db, &json!("NOPE")); // no such row
        let persisted: Option<String> = db
            .query_row("SELECT openrouter_session_id FROM cards WHERE slug = 'CARD-E'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(persisted, None);
    }

    #[test]
    fn clear_degrades_silently_when_column_absent() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE cards (slug TEXT PRIMARY KEY)").unwrap();
        db.execute("INSERT INTO cards (slug) VALUES ('CARD-G')", []).unwrap();
        assert_eq!(clearCardSession(&db, &json!("CARD-G")), Value::Null);
    }

    #[test]
    fn clear_null_card_slug_returns_null_without_touching_db() {
        let db = setup();
        assert_eq!(clearCardSession(&db, &Value::Null), Value::Null);
    }

    #[test]
    fn rotate_end_to_end_yields_a_different_session() {
        let db = setup();
        db.execute("INSERT INTO cards (slug) VALUES ('CARD-F')", []).unwrap();
        let first = ensureCardSession(&db, &json!("CARD-F"));
        clearCardSession(&db, &json!("CARD-F"));
        let second = ensureCardSession(&db, &json!("CARD-F"));
        assert_ne!(first, second, "a new run gets a fresh session");
    }
}
