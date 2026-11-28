//! anatomy-snapshot.rs — twin of skills/entities/anatomy-snapshot.mjs (db WRITE lane, default
//! export `anatomySnapshot`). NAMES ARE CONTRACT.
//!
//! Calls `extractAnatomy` (crate::audit_extract_anatomy::extractAnatomy) to produce the payload,
//! `JSON.stringify`s it, sha256-hashes the string, and INSERTs into `memory.anatomy_snapshots`.
//! `UNIQUE(sha256)` means a redundant snapshot (anatomy unchanged since the last one) is silently
//! skipped — the existing row is returned instead, decorated with `deduped: true`. A fresh insert
//! is decorated with `deduped: false`.
//!
//! `JSON.stringify(anatomy)` parity: serde_json's `preserve_order` feature (Cargo.toml) keeps
//! object key insertion order, and `serde_json::to_string` emits the same compact (no-space) form
//! as JS's default `JSON.stringify` — so as long as `extractAnatomy` builds its `Value` in the
//! same key order the JS object literal would, the stringified bytes — and therefore the sha256 —
//! match byte-for-byte across the two runtimes.
//!
//! `err.required('anatomy-snapshot', 'taken_by')` message text reproduced verbatim (engine/error.mjs
//! `${skill}: ${field} is required` assembly): `"anatomy-snapshot: taken_by is required"`.
//! `taken_by`'s JS destructuring `if (!taken_by) throw ...` is a truthiness check (not just
//! presence) — an explicit `""`/`0`/`false`/`null` all trigger it, mirrored by `truthy()` below.
//! `git_sha = null` is a destructuring DEFAULT — it fires only on an ABSENT key; an explicit
//! `git_sha: null` in the input stays null either way, so no special-casing is needed.

use rusqlite::Connection;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}

fn sha256(s: &str) -> String {
    hex::encode(Sha256::digest(s.as_bytes()))
}

pub fn anatomySnapshot(input: &Value, db: &Connection) -> Result<Value, String> {
    let empty = json!({});
    let input = if input.is_object() { input } else { &empty };

    let taken_by_val = input.get("taken_by");
    if !truthy(taken_by_val) {
        return Err("anatomy-snapshot: taken_by is required".to_string());
    }
    let taken_by = taken_by_val.cloned().unwrap_or(Value::Null);

    // JS: `const { git_sha = null } = input` — default fires only on an ABSENT key.
    let git_sha = match input.get("git_sha") {
        None => Value::Null,
        Some(v) => v.clone(),
    };

    let anatomy = crate::audit_extract_anatomy::extractAnatomy(&json!({}), db)?;
    let payload_json = serde_json::to_string(&anatomy).map_err(|e| e.to_string())?;
    let hash = sha256(&payload_json);

    let existing_rows = crate::db_json::query_rows(
        db,
        "SELECT * FROM memory.anatomy_snapshots WHERE sha256 = ?",
        &[json!(hash)],
    )?;
    if let Some(existing) = existing_rows.as_array().and_then(|a| a.first()) {
        let mut o: Map<String, Value> = existing.as_object().cloned().unwrap_or_default();
        o.insert("deduped".into(), json!(true));
        return Ok(Value::Object(o));
    }

    let (_changes, id) = crate::db_json::run_write(
        db,
        "INSERT INTO memory.anatomy_snapshots (payload_json, sha256, taken_by, git_sha) VALUES (?, ?, ?, ?)",
        &[json!(payload_json), json!(hash), taken_by, git_sha],
    )?;

    let inserted_rows = crate::db_json::query_rows(
        db,
        "SELECT * FROM memory.anatomy_snapshots WHERE id = ?",
        &[json!(id)],
    )?;
    let mut o: Map<String, Value> = inserted_rows
        .as_array()
        .and_then(|a| a.first())
        .and_then(|r| r.as_object())
        .cloned()
        .unwrap_or_default();
    o.insert("deduped".into(), json!(false));
    Ok(Value::Object(o))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "ATTACH ':memory:' AS memory;
            CREATE TABLE memory.anatomy_snapshots (
              id           INTEGER PRIMARY KEY AUTOINCREMENT,
              taken_at     TEXT    NOT NULL DEFAULT (datetime('now')),
              payload_json TEXT    NOT NULL,
              sha256       TEXT    NOT NULL UNIQUE,
              taken_by     TEXT    NOT NULL,
              git_sha      TEXT
            );
            CREATE TABLE actions (name TEXT PRIMARY KEY, owner_agent TEXT, description TEXT);
            CREATE TABLE agents (name TEXT PRIMARY KEY, description TEXT, skills_json TEXT);
            CREATE TABLE cards (id INTEGER PRIMARY KEY);
            CREATE TABLE sacred_timeline (id INTEGER PRIMARY KEY);
            ATTACH ':memory:' AS brain;
            ATTACH ':memory:' AS laws;
            ATTACH ':memory:' AS knowledge;
            ATTACH ':memory:' AS timeline;
            CREATE TABLE memory.memories (id INTEGER PRIMARY KEY);
            CREATE TABLE memory.embeddings (id INTEGER PRIMARY KEY);
            CREATE TABLE memory.memory_links (id INTEGER PRIMARY KEY);
            CREATE TABLE memory.memory_uses (id INTEGER PRIMARY KEY);
            CREATE TABLE memory.memory_use_outcomes (id INTEGER PRIMARY KEY);
            CREATE TABLE brain.brain_fires (id INTEGER PRIMARY KEY);
            CREATE TABLE brain.brain_decisions (id INTEGER PRIMARY KEY);
            CREATE TABLE brain.brain_locks (id INTEGER PRIMARY KEY);
            CREATE TABLE laws.laws (id INTEGER PRIMARY KEY);
            CREATE TABLE laws.law_clauses (id INTEGER PRIMARY KEY);
            CREATE TABLE laws.law_links (id INTEGER PRIMARY KEY);
            CREATE TABLE laws.law_violations (id INTEGER PRIMARY KEY);
            CREATE TABLE knowledge.documents (id INTEGER PRIMARY KEY);
            CREATE TABLE knowledge.extracts (id INTEGER PRIMARY KEY);",
        )
        .unwrap();
        c
    }

    #[test]
    fn requires_taken_by() {
        let c = db();
        let r = anatomySnapshot(&json!({}), &c);
        assert_eq!(r, Err("anatomy-snapshot: taken_by is required".to_string()));
        let r2 = anatomySnapshot(&json!({ "taken_by": "" }), &c);
        assert_eq!(r2, Err("anatomy-snapshot: taken_by is required".to_string()));
    }

    #[test]
    fn inserts_first_snapshot_not_deduped() {
        let c = db();
        let r = anatomySnapshot(&json!({ "taken_by": "tester" }), &c).unwrap();
        assert_eq!(r["deduped"], json!(false));
        assert_eq!(r["taken_by"], json!("tester"));
        assert_eq!(r["git_sha"], Value::Null);
        assert_eq!(r["sha256"].as_str().unwrap().len(), 64);
        assert!(r["id"].is_number());
        // payload_json must be exactly JSON.stringify(extractAnatomy(...)) — a valid JSON object.
        let payload: Value = serde_json::from_str(r["payload_json"].as_str().unwrap()).unwrap();
        assert!(payload.is_object());
        assert!(payload.get("counts").is_some());
    }

    #[test]
    fn repeat_snapshot_with_unchanged_anatomy_is_deduped() {
        let c = db();
        let first = anatomySnapshot(&json!({ "taken_by": "a" }), &c).unwrap();
        let second = anatomySnapshot(&json!({ "taken_by": "b", "git_sha": "abc123" }), &c).unwrap();
        assert_eq!(second["deduped"], json!(true));
        // existing row is returned unchanged — still 'a', not overwritten by the second caller
        assert_eq!(second["taken_by"], json!("a"));
        assert_eq!(second["id"], first["id"]);
        assert_eq!(second["sha256"], first["sha256"]);
    }

    #[test]
    fn git_sha_absent_defaults_null_but_explicit_value_is_kept() {
        let c = db();
        let r = anatomySnapshot(&json!({ "taken_by": "a", "git_sha": "deadbeef" }), &c).unwrap();
        assert_eq!(r["git_sha"], json!("deadbeef"));
    }
}
