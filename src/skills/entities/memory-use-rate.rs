//! memory-use-rate.rs — twin of skills/entities/memory-use-rate.mjs (db WRITE lane, attached
//! `memory` db). NAMES ARE CONTRACT: `memoryUseRate` (default). Records an outcome rating for a
//! memory use (append-only — many ratings per use): validate use_id/outcome/rated_by, confirm the
//! memory_uses row exists, INSERT a memory_use_outcomes row, SELECT * the new row back by rowid.
use rusqlite::Connection;
use serde_json::{json, Value};

const ALLOWED_OUTCOMES: [&str; 4] = ["helpful", "irrelevant", "misleading", "partial"];

// JS truthiness (the `!outcome` / `!rated_by` guards).
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

pub fn memoryUseRate(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS throws err.contextMissing('memory-use-rate','db') when no db — the db lane always
    // injects one, so that guard is unreachable here.
    let obj = input.as_object();
    let get = |k: &str| obj.and_then(|o| o.get(k));
    let use_id = get("use_id").cloned().unwrap_or(Value::Null);
    let outcome = get("outcome").cloned().unwrap_or(Value::Null);
    let rated_by = get("rated_by").cloned().unwrap_or(Value::Null);
    // JS destructure default (`notes = null`) fires only on absent key; explicit null stays null —
    // both land on Value::Null, exactly JS's result.
    let notes = get("notes").cloned().unwrap_or(Value::Null);

    if !is_js_integer(Some(&use_id)) {
        return Err("memory-use-rate: use_id (integer) is required".into());
    }
    if !truthy(Some(&outcome)) {
        return Err("memory-use-rate: outcome is required".into());
    }
    if !outcome.as_str().map_or(false, |s| ALLOWED_OUTCOMES.contains(&s)) {
        return Err(format!(
            "memory-use-rate: outcome must be one of: {} (got '{}')",
            ALLOWED_OUTCOMES.join(", "),
            tmpl(Some(&outcome))
        ));
    }
    if !truthy(Some(&rated_by)) {
        return Err("memory-use-rate: rated_by is required".into());
    }

    let use_rows = crate::db_json::query_rows(db, "SELECT id FROM memory.memory_uses WHERE id = ?", &[use_id.clone()])?;
    if use_rows.as_array().map_or(true, |a| a.is_empty()) {
        return Err(format!("memory-use-rate: memory_use id '{}' not found", tmpl(Some(&use_id))));
    }

    let (_changes, rowid) = crate::db_json::run_write(
        db,
        "INSERT INTO memory.memory_use_outcomes (use_id, outcome, rated_by, notes)\n     VALUES (?, ?, ?, ?)",
        &[use_id, outcome, rated_by, notes],
    )?;
    let rows = crate::db_json::query_rows(db, "SELECT * FROM memory.memory_use_outcomes WHERE id = ?", &[json!(rowid)])?;
    Ok(rows.as_array().and_then(|a| a.first()).cloned().unwrap_or(Value::Null))
}

#[cfg(test)]
mod tests {
    use super::*;

    // Real memory shapes with the rated_at DEFAULT frozen the same way the fixture does.
    const SCHEMA: &str = "CREATE TABLE memory.memory_uses ( id INTEGER PRIMARY KEY AUTOINCREMENT, memory_id INTEGER NOT NULL, used_by TEXT NOT NULL ); \
        CREATE TABLE memory.memory_use_outcomes ( id INTEGER PRIMARY KEY AUTOINCREMENT, use_id INTEGER NOT NULL, outcome TEXT NOT NULL, rated_by TEXT NOT NULL, rated_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')), notes TEXT, CHECK (outcome IN ('helpful', 'irrelevant', 'misleading', 'partial')), FOREIGN KEY (use_id) REFERENCES memory_uses(id) )";

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS memory").unwrap();
        c.execute_batch(SCHEMA).unwrap();
        c.execute("INSERT INTO memory.memory_uses (id, memory_id, used_by) VALUES (1, 10, 'agent:test')", []).unwrap();
        c
    }
    fn count(c: &Connection) -> i64 {
        c.query_row("SELECT COUNT(*) FROM memory.memory_use_outcomes", [], |r| r.get(0)).unwrap()
    }

    #[test]
    fn records_a_basic_outcome_rating() {
        let c = db();
        let r = memoryUseRate(&json!({ "use_id": 1, "outcome": "helpful", "rated_by": "agent:self" }), &c).unwrap();
        assert_eq!(
            r,
            json!({ "id": 1, "use_id": 1, "outcome": "helpful", "rated_by": "agent:self",
                    "rated_at": "2026-01-01 00:00:00", "notes": null })
        );
        assert_eq!(count(&c), 1);
    }

    #[test]
    fn carries_notes_and_explicit_null_behaves_like_absent() {
        let c = db();
        let r = memoryUseRate(
            &json!({ "use_id": 1, "outcome": "misleading", "rated_by": "human:dani", "notes": "wrong path" }),
            &c,
        )
        .unwrap();
        assert_eq!(r["notes"], json!("wrong path"));
        let r2 = memoryUseRate(
            &json!({ "use_id": 1, "outcome": "partial", "rated_by": "agent:qa", "notes": null }),
            &c,
        )
        .unwrap();
        assert_eq!(r2["notes"], Value::Null);
    }

    #[test]
    fn multiple_ratings_per_use_append_only() {
        let c = db();
        memoryUseRate(&json!({ "use_id": 1, "outcome": "partial", "rated_by": "agent:self" }), &c).unwrap();
        let r = memoryUseRate(&json!({ "use_id": 1, "outcome": "helpful", "rated_by": "human:dani" }), &c).unwrap();
        assert_eq!(r["id"], json!(2));
        assert_eq!(count(&c), 2);
    }

    #[test]
    fn accepts_all_four_outcomes() {
        for outcome in ALLOWED_OUTCOMES {
            let c = db();
            let r = memoryUseRate(&json!({ "use_id": 1, "outcome": outcome, "rated_by": "t" }), &c).unwrap();
            assert_eq!(r["outcome"], json!(outcome));
        }
    }

    #[test]
    fn rejects_bad_input_without_writing() {
        let c = db();
        // non-integer / missing use_id
        assert_eq!(
            memoryUseRate(&json!({ "outcome": "helpful", "rated_by": "t" }), &c).unwrap_err(),
            "memory-use-rate: use_id (integer) is required"
        );
        assert!(memoryUseRate(&json!({ "use_id": "1", "outcome": "helpful", "rated_by": "t" }), &c).is_err());
        assert!(memoryUseRate(&json!({ "use_id": 1.5, "outcome": "helpful", "rated_by": "t" }), &c).is_err());
        // missing / unknown outcome
        assert_eq!(
            memoryUseRate(&json!({ "use_id": 1, "rated_by": "t" }), &c).unwrap_err(),
            "memory-use-rate: outcome is required"
        );
        assert_eq!(
            memoryUseRate(&json!({ "use_id": 1, "outcome": "meh", "rated_by": "t" }), &c).unwrap_err(),
            "memory-use-rate: outcome must be one of: helpful, irrelevant, misleading, partial (got 'meh')"
        );
        // missing rated_by
        assert_eq!(
            memoryUseRate(&json!({ "use_id": 1, "outcome": "helpful" }), &c).unwrap_err(),
            "memory-use-rate: rated_by is required"
        );
        // nonexistent use
        assert_eq!(
            memoryUseRate(&json!({ "use_id": 9999, "outcome": "helpful", "rated_by": "t" }), &c).unwrap_err(),
            "memory-use-rate: memory_use id '9999' not found"
        );
        assert_eq!(count(&c), 0);
    }
}
