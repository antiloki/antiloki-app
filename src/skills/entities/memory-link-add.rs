//! memory-link-add.rs — twin of skills/entities/memory-link-add.mjs (db WRITE lane, attached
//! `memory` db). NAMES ARE CONTRACT: `memoryLinkAdd` (default). Adds a typed directed edge
//! between two memories (contradicts | extends | derived_from | related | corrects) for GraphRAG
//! traversal: validate ids/kind/weight/created_by, confirm both memories exist, INSERT a
//! memory_links row (UNIQUE (from,to,kind) — duplicates throw), SELECT * the new row back.
use rusqlite::Connection;
use serde_json::{json, Value};

const ALLOWED_KINDS: [&str; 5] = ["contradicts", "extends", "derived_from", "related", "corrects"];

// JS truthiness (the `!kind` / `!created_by` guards).
fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Value::String(s) => !s.is_empty(),
        _ => true,
    }
}
// JS `${v}` interpolation for the values that reach error messages.
fn tmpl(v: &Value) -> String {
    match v {
        Value::Null => "null".into(),
        Value::String(s) => s.clone(),
        Value::Bool(b) => b.to_string(),
        n @ Value::Number(_) => n.to_string(),
        o => o.to_string(),
    }
}
// JS Number.isInteger: numbers only, whole-valued (5.0 counts), non-finite never.
fn is_js_integer(v: &Value) -> bool {
    match v {
        Value::Number(n) => {
            n.as_i64().is_some()
                || n.as_u64().is_some()
                || n.as_f64().map_or(false, |f| f.is_finite() && f.fract() == 0.0)
        }
        _ => false,
    }
}

pub fn memoryLinkAdd(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS throws err.contextMissing('memory-link-add','db') when no db — the db lane always
    // injects one, so that guard is unreachable here.
    let obj = input.as_object();
    let get = |k: &str| obj.and_then(|o| o.get(k)).cloned().unwrap_or(Value::Null);

    let from_memory_id = get("from_memory_id");
    let to_memory_id = get("to_memory_id");
    let kind = get("kind");
    // rationale defaults to null on absence — explicit null lands on the same value.
    let rationale = get("rationale");
    // weight defaults to 1.0 ONLY when the key is absent (JS destructure default fires on
    // `undefined`, not on explicit `null`); explicit null stays null and fails the typeof guard.
    let weight = match obj.and_then(|o| o.get("weight")) {
        None => json!(1.0),
        Some(v) => v.clone(),
    };
    let created_by = get("created_by");

    if !is_js_integer(&from_memory_id) {
        return Err("memory-link-add: from_memory_id (integer) is required".into());
    }
    if !is_js_integer(&to_memory_id) {
        return Err("memory-link-add: to_memory_id (integer) is required".into());
    }
    // both already confirmed whole-valued numbers, so an f64 compare mirrors JS `===`.
    if from_memory_id.as_f64() == to_memory_id.as_f64() {
        return Err("memory-link-add: cannot link a memory to itself".into());
    }
    if !truthy(&kind) {
        return Err("memory-link-add: kind is required".into());
    }
    if !kind.as_str().map_or(false, |s| ALLOWED_KINDS.contains(&s)) {
        return Err(format!(
            "memory-link-add: kind must be one of: {} (got '{}')",
            ALLOWED_KINDS.join(", "),
            tmpl(&kind)
        ));
    }
    if !weight.as_f64().map_or(false, |f| (0.0..=1.0).contains(&f)) {
        return Err(format!(
            "memory-link-add: weight must be between 0 and 1 (got {})",
            tmpl(&weight)
        ));
    }
    if !truthy(&created_by) {
        return Err("memory-link-add: created_by is required".into());
    }

    let from_rows = crate::db_json::query_rows(db, "SELECT id FROM memory.memories WHERE id=?", &[from_memory_id.clone()])?;
    if from_rows.as_array().map_or(true, |a| a.is_empty()) {
        return Err(format!("memory-link-add: memory id {} (from) not found", tmpl(&from_memory_id)));
    }
    let to_rows = crate::db_json::query_rows(db, "SELECT id FROM memory.memories WHERE id=?", &[to_memory_id.clone()])?;
    if to_rows.as_array().map_or(true, |a| a.is_empty()) {
        return Err(format!("memory-link-add: memory id {} (to) not found", tmpl(&to_memory_id)));
    }

    let (_changes, rowid) = crate::db_json::run_write(
        db,
        "INSERT INTO memory.memory_links (from_memory_id, to_memory_id, kind, rationale, weight, created_by)\n     VALUES (?, ?, ?, ?, ?, ?)",
        &[from_memory_id, to_memory_id, kind, rationale, weight, created_by],
    )?;
    let rows = crate::db_json::query_rows(db, "SELECT * FROM memory.memory_links WHERE id=?", &[json!(rowid)])?;
    Ok(rows.as_array().and_then(|a| a.first()).cloned().unwrap_or(Value::Null))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCHEMA: &str = "CREATE TABLE memory.memories ( id INTEGER PRIMARY KEY AUTOINCREMENT, title TEXT ); \
        CREATE TABLE memory.memory_links ( id INTEGER PRIMARY KEY AUTOINCREMENT, from_memory_id INTEGER NOT NULL, to_memory_id INTEGER NOT NULL, kind TEXT NOT NULL, rationale TEXT, weight REAL NOT NULL DEFAULT 1.0, created_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')), created_by TEXT NOT NULL, CHECK (kind IN ('contradicts', 'extends', 'derived_from', 'related', 'corrects')), CHECK (from_memory_id != to_memory_id), CHECK (weight >= 0.0 AND weight <= 1.0), UNIQUE (from_memory_id, to_memory_id, kind), FOREIGN KEY (from_memory_id) REFERENCES memories(id), FOREIGN KEY (to_memory_id) REFERENCES memories(id) )";

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS memory").unwrap();
        c.execute_batch(SCHEMA).unwrap();
        c.execute("INSERT INTO memory.memories (id, title) VALUES (1, 'a'), (2, 'b')", []).unwrap();
        c
    }
    fn count(c: &Connection) -> i64 {
        c.query_row("SELECT COUNT(*) FROM memory.memory_links", [], |r| r.get(0)).unwrap()
    }

    #[test]
    fn creates_a_link_with_default_weight() {
        let c = db();
        let r = memoryLinkAdd(&json!({ "from_memory_id": 1, "to_memory_id": 2, "kind": "related", "created_by": "test" }), &c).unwrap();
        assert_eq!(r["from_memory_id"], json!(1));
        assert_eq!(r["to_memory_id"], json!(2));
        assert_eq!(r["kind"], json!("related"));
        assert_eq!(r["weight"], json!(1.0));
        assert_eq!(r["rationale"], Value::Null);
        assert_eq!(r["created_at"], json!("2026-01-01 00:00:00"));
        assert_eq!(count(&c), 1);
    }

    #[test]
    fn accepts_all_five_kinds() {
        for kind in ALLOWED_KINDS {
            let c = db();
            let r = memoryLinkAdd(&json!({ "from_memory_id": 1, "to_memory_id": 2, "kind": kind, "created_by": "t" }), &c).unwrap();
            assert_eq!(r["kind"], json!(kind));
        }
    }

    #[test]
    fn records_explicit_rationale_and_weight() {
        let c = db();
        let r = memoryLinkAdd(
            &json!({ "from_memory_id": 1, "to_memory_id": 2, "kind": "contradicts", "rationale": "newer evidence disagrees", "weight": 0.65, "created_by": "human:dani" }),
            &c,
        )
        .unwrap();
        assert_eq!(r["rationale"], json!("newer evidence disagrees"));
        assert_eq!(r["weight"], json!(0.65));
    }

    #[test]
    fn weight_boundaries_are_accepted() {
        let c = db();
        let lo = memoryLinkAdd(&json!({ "from_memory_id": 1, "to_memory_id": 2, "kind": "extends", "weight": 0, "created_by": "t" }), &c).unwrap();
        assert_eq!(lo["weight"], json!(0.0));
        let hi = memoryLinkAdd(&json!({ "from_memory_id": 1, "to_memory_id": 2, "kind": "derived_from", "weight": 1, "created_by": "t" }), &c).unwrap();
        assert_eq!(hi["weight"], json!(1.0));
    }

    #[test]
    fn same_pair_different_kinds_both_land() {
        let c = db();
        memoryLinkAdd(&json!({ "from_memory_id": 1, "to_memory_id": 2, "kind": "related", "created_by": "t" }), &c).unwrap();
        let second = memoryLinkAdd(&json!({ "from_memory_id": 1, "to_memory_id": 2, "kind": "extends", "created_by": "t" }), &c).unwrap();
        assert_eq!(second["kind"], json!("extends"));
        assert_eq!(count(&c), 2);
    }

    #[test]
    fn rejects_self_link() {
        let c = db();
        assert!(memoryLinkAdd(&json!({ "from_memory_id": 1, "to_memory_id": 1, "kind": "related", "created_by": "t" }), &c)
            .unwrap_err()
            .contains("cannot link a memory to itself"));
        assert_eq!(count(&c), 0);
    }

    #[test]
    fn rejects_nonexistent_memory_ids() {
        let c = db();
        assert!(memoryLinkAdd(&json!({ "from_memory_id": 9999, "to_memory_id": 1, "kind": "related", "created_by": "t" }), &c)
            .unwrap_err()
            .contains("memory id 9999 (from) not found"));
        assert!(memoryLinkAdd(&json!({ "from_memory_id": 1, "to_memory_id": 9999, "kind": "related", "created_by": "t" }), &c)
            .unwrap_err()
            .contains("memory id 9999 (to) not found"));
        assert_eq!(count(&c), 0);
    }

    #[test]
    fn rejects_bad_input_without_writing() {
        let c = db();
        assert_eq!(
            memoryLinkAdd(&json!({ "to_memory_id": 2, "kind": "related", "created_by": "t" }), &c).unwrap_err(),
            "memory-link-add: from_memory_id (integer) is required"
        );
        assert_eq!(
            memoryLinkAdd(&json!({ "from_memory_id": 1, "kind": "related", "created_by": "t" }), &c).unwrap_err(),
            "memory-link-add: to_memory_id (integer) is required"
        );
        assert_eq!(
            memoryLinkAdd(&json!({ "from_memory_id": 1, "to_memory_id": 2, "created_by": "t" }), &c).unwrap_err(),
            "memory-link-add: kind is required"
        );
        assert_eq!(
            memoryLinkAdd(&json!({ "from_memory_id": 1, "to_memory_id": 2, "kind": "whatever", "created_by": "t" }), &c).unwrap_err(),
            "memory-link-add: kind must be one of: contradicts, extends, derived_from, related, corrects (got 'whatever')"
        );
        assert!(memoryLinkAdd(&json!({ "from_memory_id": 1, "to_memory_id": 2, "kind": "related", "weight": 1.5, "created_by": "t" }), &c)
            .unwrap_err()
            .contains("weight"));
        assert_eq!(
            memoryLinkAdd(&json!({ "from_memory_id": 1, "to_memory_id": 2, "kind": "related" }), &c).unwrap_err(),
            "memory-link-add: created_by is required"
        );
        assert_eq!(count(&c), 0);
    }
}
