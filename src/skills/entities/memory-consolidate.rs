//! memory-consolidate.rs — twin of skills/entities/memory-consolidate.mjs (db WRITE lane, attached
//! `memory` db). NAMES ARE CONTRACT: `memoryConsolidate` (default). Validates every constituent raw
//! memory id exists, inserts the narrative via the memory-add twin, then links narrative <- each
//! constituent through memory_links (kind='derived_from'), silently ignoring UNIQUE re-links —
//! mirrors the JS `try { linkStmt.run(...) } catch { /* already linked */ }`.
use rusqlite::Connection;
use serde_json::{json, Value};

fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}
// = JS `${v}` interpolation (feeds error messages only).
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
// = JS Number.isInteger(v): finite number with zero fractional part; non-numbers are false.
fn is_integer(v: &Value) -> bool {
    matches!(v, Value::Number(n) if n.as_f64().map_or(false, |f| f.is_finite() && f.fract() == 0.0))
}

pub fn memoryConsolidate(input: &Value, db: &Connection) -> Result<Value, String> {
    let title = input.get("title").cloned().unwrap_or(Value::Null);
    let situation = input.get("situation").cloned().unwrap_or(Value::Null);
    let guidance = input.get("guidance").cloned().unwrap_or(Value::Null);
    let rationale = input.get("rationale").cloned().unwrap_or(Value::Null);
    let tags = match input.get("tags") { None => json!(["narrative"]), Some(v) => v.clone() };
    // JS: created_by = actor default. The db lane always calls with context == { db } (no actor
    // field — see conform-db.mjs), so context?.actor is always undefined and the destructure
    // default 'unknown' fires whenever the caller omits created_by (same reasoning as memory-add.rs).
    let created_by = match input.get("created_by") { None => json!("unknown"), Some(v) => v.clone() };
    let source_card = input.get("source_card").cloned().unwrap_or(Value::Null);
    let source_session = input.get("source_session").cloned().unwrap_or(Value::Null);

    if !truthy(Some(&title)) { return Err("memory-consolidate: title is required".into()); }
    if !truthy(Some(&situation)) { return Err("memory-consolidate: situation is required".into()); }
    if !truthy(Some(&guidance)) { return Err("memory-consolidate: guidance is required".into()); }

    let ids: Vec<Value> = match input.get("constituent_memory_ids").and_then(|v| v.as_array()) {
        Some(a) if !a.is_empty() => a.clone(),
        _ => return Err("memory-consolidate: constituent_memory_ids (non-empty array of integer memory IDs) is required".into()),
    };

    // Validate all constituents exist BEFORE inserting anything (avoids partial inserts).
    for cid in &ids {
        if !is_integer(cid) {
            return Err("memory-consolidate: each constituent id must be an integer".into());
        }
        let rows = crate::db_json::query_rows(db, "SELECT id FROM memory.memories WHERE id = ?", &[cid.clone()])?;
        if rows.as_array().map_or(true, |a| a.is_empty()) {
            return Err(format!("memory-consolidate: memory id '{}' not found", tmpl(Some(cid))));
        }
    }

    // Insert the narrative memory first (cross-module call into the already-converted memory-add twin).
    let narrative = crate::entities_memory_add::memoryAdd(&json!({
        "type": "narrative",
        "title": title,
        "situation": situation,
        "guidance": guidance,
        "rationale": rationale,
        "tags": tags,
        "source_card": source_card,
        "source_session": source_session,
        "confidence": 1.0,
        "created_by": created_by,
    }), db)?;
    let narrative_id = narrative.get("id").cloned().unwrap_or(Value::Null);

    // Link narrative <- constituents via derived_from.
    for cid in &ids {
        let rationale_text = format!("narrative '{}' derived from this memory", tmpl(Some(&title)));
        // JS wraps this INSERT in try/catch and swallows ANY error (comment says "UNIQUE constraint
        // — already linked" but the catch has no filter) — swallow every write error the same way.
        let _ = crate::db_json::run_write(
            db,
            "INSERT INTO memory.memory_links (from_memory_id, to_memory_id, kind, rationale, weight, created_by) VALUES (?, ?, 'derived_from', ?, 1.0, ?)",
            &[narrative_id.clone(), cid.clone(), json!(rationale_text), created_by.clone()],
        );
    }

    Ok(json!({
        "narrative": narrative,
        "linked_count": ids.len() as i64,
        "constituent_memory_ids": ids,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "ATTACH ':memory:' AS memory;
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

    fn raw(c: &Connection, title: &str) -> Value {
        crate::entities_memory_add::memoryAdd(&json!({
            "type": "lesson", "title": title, "situation": "s", "guidance": "g", "created_by": "test"
        }), c).unwrap()
    }

    #[test]
    fn creates_narrative_and_links_constituents() {
        let c = db();
        let m1 = raw(&c, "lesson A");
        let m2 = raw(&c, "lesson B");
        let r = memoryConsolidate(&json!({
            "title": "why we never bypass with-audit",
            "situation": "multiple incidents around audit bypasses",
            "guidance": "all mutations through with-audit",
            "rationale": "preserves chain integrity",
            "constituent_memory_ids": [m1["id"], m2["id"]],
            "created_by": "human:dani",
        }), &c).unwrap();
        assert_eq!(r["narrative"]["type"], json!("narrative"));
        assert_eq!(r["narrative"]["confidence"], json!(1));
        assert_eq!(r["linked_count"], json!(2));
        assert_eq!(r["constituent_memory_ids"], json!([m1["id"], m2["id"]]));
        let links = crate::db_json::query_rows(&c, "SELECT * FROM memory.memory_links WHERE from_memory_id = ?", &[r["narrative"]["id"].clone()]).unwrap();
        let links = links.as_array().unwrap();
        assert_eq!(links.len(), 2);
        assert_eq!(links[0]["kind"], json!("derived_from"));
    }

    #[test]
    fn duplicate_constituent_ids_swallow_unique_violation() {
        let c = db();
        let m1 = raw(&c, "lesson A");
        let r = memoryConsolidate(&json!({
            "title": "t", "situation": "s", "guidance": "g",
            "constituent_memory_ids": [m1["id"], m1["id"]],
        }), &c).unwrap();
        assert_eq!(r["linked_count"], json!(2)); // counts the input array, not distinct links written
        let links = crate::db_json::query_rows(&c, "SELECT * FROM memory.memory_links WHERE from_memory_id = ?", &[r["narrative"]["id"].clone()]).unwrap();
        assert_eq!(links.as_array().unwrap().len(), 1); // second insert silently swallowed (UNIQUE)
    }

    #[test]
    fn defaults_apply_when_optional_fields_are_absent() {
        let c = db();
        let m1 = raw(&c, "lesson A");
        let r = memoryConsolidate(&json!({
            "title": "t", "situation": "s", "guidance": "g",
            "constituent_memory_ids": [m1["id"]],
        }), &c).unwrap();
        assert_eq!(r["narrative"]["rationale"], Value::Null);
        assert_eq!(r["narrative"]["tags_json"], json!("[\"narrative\"]"));
        assert_eq!(r["narrative"]["source_card"], Value::Null);
        assert_eq!(r["narrative"]["source_session"], Value::Null);
        assert_eq!(r["narrative"]["created_by"], json!("unknown"));
    }

    #[test]
    fn guards_reject_invalid_input() {
        let c = db();
        let m1 = raw(&c, "lesson A");
        assert!(memoryConsolidate(&json!({ "situation": "s", "guidance": "g", "constituent_memory_ids": [m1["id"]] }), &c).is_err()); // title missing
        assert!(memoryConsolidate(&json!({ "title": "t", "situation": "s", "guidance": "g", "constituent_memory_ids": [] }), &c).is_err()); // empty array
        assert!(memoryConsolidate(&json!({ "title": "t", "situation": "s", "guidance": "g", "constituent_memory_ids": [9999] }), &c).is_err()); // not found
    }
}
