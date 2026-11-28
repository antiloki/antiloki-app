//! memory-use-record.rs — twin of skills/entities/memory-use-record.mjs (db WRITE lane, attached
//! `memory` db). NAMES ARE CONTRACT: `memoryUseRecord` (default). Records that a memory was
//! retrieved/used in some context (half of the self-learning loop; memory-use-rate is the other):
//! validate memory_id/used_by/retrieval_method/retrieval_score, confirm the memory exists,
//! INSERT a memory_uses row, SELECT * the new row back by rowid.
use rusqlite::Connection;
use serde_json::{json, Value};

const ALLOWED_METHODS: [&str; 4] = ["similarity", "graph", "rag-context", "direct"];

// JS truthiness (the `!used_by` guard).
fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}
// JS `${v}` interpolation for the values that reach error messages.
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
// JS Number.isInteger: numbers only, whole-valued (5.0 counts), non-finite never.
fn is_js_integer(v: Option<&Value>) -> bool {
    match v {
        Some(Value::Number(n)) => {
            n.as_i64().is_some()
                || n.as_u64().is_some()
                || n.as_f64().map_or(false, |f| f.is_finite() && f.fract() == 0.0)
        }
        _ => false,
    }
}

pub fn memoryUseRecord(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS throws err.contextMissing('memory-use-record','db') when no db — the db lane always
    // injects one, so that guard is unreachable here.
    let obj = input.as_object();
    let get = |k: &str| obj.and_then(|o| o.get(k));
    let memory_id = get("memory_id").cloned().unwrap_or(Value::Null);
    let used_by = get("used_by").cloned().unwrap_or(Value::Null);
    // JS destructure defaults (`= null`) fire only on absent keys; explicit null stays null —
    // both land on Value::Null, exactly JS's result.
    let context_card = get("context_card").cloned().unwrap_or(Value::Null);
    let context_session = get("context_session").cloned().unwrap_or(Value::Null);
    let context_skill = get("context_skill").cloned().unwrap_or(Value::Null);
    let retrieval_method = get("retrieval_method").cloned().unwrap_or(Value::Null);
    let retrieval_score = get("retrieval_score").cloned().unwrap_or(Value::Null);
    let notes = get("notes").cloned().unwrap_or(Value::Null);

    if !is_js_integer(Some(&memory_id)) {
        return Err("memory-use-record: memory_id (integer) is required".into());
    }
    if !truthy(Some(&used_by)) {
        return Err("memory-use-record: used_by is required".into());
    }
    // JS `retrieval_method != null` is false for both null and undefined — both are Null here.
    if !retrieval_method.is_null()
        && !retrieval_method.as_str().map_or(false, |s| ALLOWED_METHODS.contains(&s))
    {
        return Err(format!(
            "memory-use-record: retrieval_method must be one of: {} (got '{}')",
            ALLOWED_METHODS.join(", "),
            tmpl(Some(&retrieval_method))
        ));
    }
    // JS: typeof !== 'number' || !Number.isFinite — JSON numbers are always finite, so
    // is-a-number is the whole check.
    if !retrieval_score.is_null() && !matches!(retrieval_score, Value::Number(_)) {
        return Err("memory-use-record: retrieval_score must be a finite number when provided".into());
    }

    let mem_rows = crate::db_json::query_rows(db, "SELECT id FROM memory.memories WHERE id = ?", &[memory_id.clone()])?;
    if mem_rows.as_array().map_or(true, |a| a.is_empty()) {
        return Err(format!("memory-use-record: memory id '{}' not found", tmpl(Some(&memory_id))));
    }

    let (_changes, rowid) = crate::db_json::run_write(
        db,
        "INSERT INTO memory.memory_uses\n       (memory_id, used_by, context_card, context_session, context_skill,\n        retrieval_method, retrieval_score, notes)\n     VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        &[memory_id, used_by, context_card, context_session, context_skill, retrieval_method, retrieval_score, notes],
    )?;
    let rows = crate::db_json::query_rows(db, "SELECT * FROM memory.memory_uses WHERE id = ?", &[json!(rowid)])?;
    Ok(rows.as_array().and_then(|a| a.first()).cloned().unwrap_or(Value::Null))
}

#[cfg(test)]
mod tests {
    use super::*;

    // Real memory shapes with the used_at DEFAULT frozen the same way the fixture does.
    const SCHEMA: &str = "CREATE TABLE memory.memories ( id INTEGER PRIMARY KEY AUTOINCREMENT, title TEXT ); \
        CREATE TABLE memory.memory_uses ( id INTEGER PRIMARY KEY AUTOINCREMENT, memory_id INTEGER NOT NULL, used_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')), used_by TEXT NOT NULL, context_card TEXT, context_session TEXT, context_skill TEXT, retrieval_method TEXT, retrieval_score REAL, notes TEXT, CHECK (retrieval_method IN ('similarity', 'graph', 'rag-context', 'direct') OR retrieval_method IS NULL), FOREIGN KEY (memory_id) REFERENCES memories(id) )";

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS memory").unwrap();
        c.execute_batch(SCHEMA).unwrap();
        c.execute("INSERT INTO memory.memories (id, title) VALUES (1, 'lesson one')", []).unwrap();
        c
    }
    fn count(c: &Connection) -> i64 {
        c.query_row("SELECT COUNT(*) FROM memory.memory_uses", [], |r| r.get(0)).unwrap()
    }

    #[test]
    fn records_a_basic_use() {
        let c = db();
        let r = memoryUseRecord(&json!({ "memory_id": 1, "used_by": "agent:pl" }), &c).unwrap();
        assert_eq!(
            r,
            json!({ "id": 1, "memory_id": 1, "used_at": "2026-01-01 00:00:00", "used_by": "agent:pl",
                    "context_card": null, "context_session": null, "context_skill": null,
                    "retrieval_method": null, "retrieval_score": null, "notes": null })
        );
        assert_eq!(count(&c), 1);
    }

    #[test]
    fn records_full_context() {
        let c = db();
        let r = memoryUseRecord(
            &json!({ "memory_id": 1, "used_by": "agent:dev", "context_card": "PROJ-1",
                     "context_session": "sess-abc123", "context_skill": "board-keeper.card.add",
                     "retrieval_method": "rag-context", "retrieval_score": 0.87,
                     "notes": "Top match for situation embedding" }),
            &c,
        )
        .unwrap();
        assert_eq!(r["context_card"], json!("PROJ-1"));
        assert_eq!(r["context_session"], json!("sess-abc123"));
        assert_eq!(r["context_skill"], json!("board-keeper.card.add"));
        assert_eq!(r["retrieval_method"], json!("rag-context"));
        assert_eq!(r["retrieval_score"], json!(0.87));
        assert_eq!(r["notes"], json!("Top match for situation embedding"));
    }

    #[test]
    fn accepts_all_four_retrieval_methods() {
        for method in ALLOWED_METHODS {
            let c = db();
            let r = memoryUseRecord(&json!({ "memory_id": 1, "used_by": "t", "retrieval_method": method }), &c).unwrap();
            assert_eq!(r["retrieval_method"], json!(method));
        }
    }

    #[test]
    fn multiple_uses_of_same_memory_accumulate() {
        let c = db();
        memoryUseRecord(&json!({ "memory_id": 1, "used_by": "a" }), &c).unwrap();
        memoryUseRecord(&json!({ "memory_id": 1, "used_by": "b" }), &c).unwrap();
        let r = memoryUseRecord(&json!({ "memory_id": 1, "used_by": "c" }), &c).unwrap();
        assert_eq!(r["id"], json!(3));
        assert_eq!(count(&c), 3);
    }

    #[test]
    fn explicit_null_optionals_behave_like_absent() {
        let c = db();
        let r = memoryUseRecord(
            &json!({ "memory_id": 1, "used_by": "t", "context_card": null, "retrieval_method": null,
                     "retrieval_score": null, "notes": null }),
            &c,
        )
        .unwrap();
        assert_eq!(r["context_card"], Value::Null);
        assert_eq!(r["retrieval_method"], Value::Null);
        assert_eq!(r["notes"], Value::Null);
    }

    #[test]
    fn rejects_bad_input_without_writing() {
        let c = db();
        // non-integer / missing memory_id
        assert_eq!(
            memoryUseRecord(&json!({ "used_by": "t" }), &c).unwrap_err(),
            "memory-use-record: memory_id (integer) is required"
        );
        assert!(memoryUseRecord(&json!({ "memory_id": "1", "used_by": "t" }), &c).is_err());
        assert!(memoryUseRecord(&json!({ "memory_id": 1.5, "used_by": "t" }), &c).is_err());
        // missing used_by
        assert_eq!(
            memoryUseRecord(&json!({ "memory_id": 1 }), &c).unwrap_err(),
            "memory-use-record: used_by is required"
        );
        // unknown retrieval_method
        assert_eq!(
            memoryUseRecord(&json!({ "memory_id": 1, "used_by": "t", "retrieval_method": "magic" }), &c).unwrap_err(),
            "memory-use-record: retrieval_method must be one of: similarity, graph, rag-context, direct (got 'magic')"
        );
        // non-number retrieval_score
        assert_eq!(
            memoryUseRecord(&json!({ "memory_id": 1, "used_by": "t", "retrieval_score": "high" }), &c).unwrap_err(),
            "memory-use-record: retrieval_score must be a finite number when provided"
        );
        // nonexistent memory
        assert_eq!(
            memoryUseRecord(&json!({ "memory_id": 9999, "used_by": "t" }), &c).unwrap_err(),
            "memory-use-record: memory id '9999' not found"
        );
        assert_eq!(count(&c), 0);
    }
}
