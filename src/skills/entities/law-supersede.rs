//! law-supersede.rs — twin of skills/entities/law-supersede.mjs (db WRITE lane, cross-db laws
//! via ATTACH). NAMES ARE CONTRACT: `lawSupersede` (default). Append-only replace: sets
//! old.superseded_by = new.id + old.superseded_at = now (frozen clock); the old row STAYS —
//! partial indexes WHERE superseded_by IS NULL hide it from "active" queries.
use rusqlite::Connection;
use serde_json::Value;

fn tmpl(v: Option<&Value>) -> String { match v { None=>"undefined".into(), Some(Value::Null)=>"null".into(), Some(Value::String(s))=>s.clone(), Some(Value::Bool(b))=>b.to_string(), Some(n@Value::Number(_))=>n.to_string(), Some(o)=>o.to_string() } }

// JS Number.isInteger: a Number whose value has no fractional part (5.0 counts; strings/bools/null don't).
fn is_js_integer(v: Option<&Value>) -> bool {
    match v {
        Some(Value::Number(n)) => n.as_i64().is_some() || n.as_u64().is_some()
            || n.as_f64().map_or(false, |f| f.is_finite() && f.fract() == 0.0),
        _ => false,
    }
}

pub fn lawSupersede(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS: `const { db } = context ?? {}` — the db lane always supplies db; contextMissing can't fire.
    let old_id_v = input.get("old_id");
    let new_id_v = input.get("new_id");
    if !is_js_integer(old_id_v) { return Err("law-supersede: old_id (integer) is required".into()); }
    if !is_js_integer(new_id_v) { return Err("law-supersede: new_id (integer) is required".into()); }
    let old_id = old_id_v.expect("integer implies present").clone();
    let new_id = new_id_v.expect("integer implies present").clone();
    // JS `old_id === new_id` — both integers by the guards above, so numeric equality (5 === 5.0).
    if old_id.as_f64() == new_id.as_f64() {
        return Err("law-supersede: old_id and new_id must differ".into());
    }

    let old_rows = crate::db_json::query_rows(db,
        "SELECT id, slug, superseded_by FROM laws.laws WHERE id = ?", &[old_id.clone()])?;
    let old_row = match old_rows.as_array().and_then(|a| a.first()) {
        Some(r) => r.clone(),
        None => return Err(format!("law-supersede: law id '{}' not found", tmpl(Some(&old_id)))),
    };
    // JS `oldRow.superseded_by != null` — loose: fires for any non-NULL value.
    if old_row.get("superseded_by").map_or(false, |v| !v.is_null()) {
        return Err(format!("law-supersede: law id {} (slug '{}') is already superseded by {}",
            tmpl(Some(&old_id)), tmpl(old_row.get("slug")), tmpl(old_row.get("superseded_by"))));
    }

    let new_rows = crate::db_json::query_rows(db,
        "SELECT id, slug FROM laws.laws WHERE id = ?", &[new_id.clone()])?;
    if new_rows.as_array().map_or(true, |a| a.is_empty()) {
        return Err(format!("law-supersede: law id {} not found (cannot supersede with nonexistent law)",
            tmpl(Some(&new_id))));
    }

    // JS datetime('now') — frozen-clock lane pins the instant; write the frozen literal.
    crate::db_json::run_write(db,
        &format!("UPDATE laws.laws SET superseded_by = ?, superseded_at = {} WHERE id = ?", crate::clock::now_sql()),
        &[new_id, old_id.clone()])?;

    let out = crate::db_json::query_rows(db, "SELECT * FROM laws.laws WHERE id = ?", &[old_id])?;
    // JS `.get()` → the row object (always present here — we just updated it).
    Ok(out.as_array().and_then(|a| a.first()).cloned().unwrap_or(Value::Null))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS laws; \
          CREATE TABLE laws.laws (id INTEGER PRIMARY KEY, slug TEXT NOT NULL, title TEXT, kind TEXT NOT NULL, enforcement TEXT NOT NULL, established_by TEXT, superseded_by INTEGER, superseded_at TEXT); \
          INSERT INTO laws.laws (id,slug,title,kind,enforcement,established_by,superseded_by,superseded_at) VALUES \
            (1,'NO_FRIDAY_DEPLOYS','No deploys on Friday','procedural','soft','human:dani',NULL,NULL), \
            (2,'NO_FRIDAY_DEPLOYS_V2','No deploys on Friday v2','procedural','soft','human:dani',NULL,NULL), \
            (3,'OLD_RULE','Old rule','decision','soft','interviewer',2,'2025-11-01 09:00:00');").unwrap();
        c
    }

    #[test]
    fn supersede_marks_old_row_with_new_id_and_frozen_now() {
        let c = db();
        let row = lawSupersede(&json!({ "old_id": 1, "new_id": 2 }), &c).unwrap();
        assert_eq!(serde_json::to_string(&row).unwrap(),
            "{\"id\":1,\"slug\":\"NO_FRIDAY_DEPLOYS\",\"title\":\"No deploys on Friday\",\"kind\":\"procedural\",\"enforcement\":\"soft\",\"established_by\":\"human:dani\",\"superseded_by\":2,\"superseded_at\":\"2026-01-01 00:00:00\"}");
        // append-only: the old row stays; the new row is untouched
        let all = crate::db_json::query_rows(&c, "SELECT id, superseded_by FROM laws.laws ORDER BY id", &[]).unwrap();
        assert_eq!(all, json!([{ "id": 1, "superseded_by": 2 }, { "id": 2, "superseded_by": null }, { "id": 3, "superseded_by": 2 }]));
    }

    #[test]
    fn already_superseded_old_row_refuses() {
        let c = db();
        let e = lawSupersede(&json!({ "old_id": 3, "new_id": 1 }), &c).unwrap_err();
        assert!(e.contains("already superseded by 2"), "{e}");
    }

    #[test]
    fn missing_rows_refuse() {
        let c = db();
        assert!(lawSupersede(&json!({ "old_id": 99, "new_id": 1 }), &c).unwrap_err().contains("not found"));
        assert!(lawSupersede(&json!({ "old_id": 1, "new_id": 99 }), &c).unwrap_err().contains("nonexistent law"));
    }

    #[test]
    fn guards_reject_invalid_input() {
        let c = db();
        assert!(lawSupersede(&json!({ "new_id": 2 }), &c).is_err()); // old_id absent
        assert!(lawSupersede(&json!({ "old_id": "1", "new_id": 2 }), &c).is_err()); // string, not integer
        assert!(lawSupersede(&json!({ "old_id": 1.5, "new_id": 2 }), &c).is_err()); // fractional
        assert!(lawSupersede(&json!({ "old_id": 1, "new_id": null }), &c).is_err()); // null new_id
        assert!(lawSupersede(&json!({ "old_id": 2, "new_id": 2 }), &c).unwrap_err().contains("must differ")); // self
    }
}
