//! data-story-update.rs — twin of skills/codebase/data-story-update.mjs (db WRITE lane). NAMES ARE
//! CONTRACT: `dataStoryUpdate` (named default). Partial UPDATE (only provided fields) + {id, updated}.
// QUALITY: 82/100 — field-by-field SET builder; frozen updated_at; no-op path returns updated:false
use rusqlite::Connection;
use serde_json::{json, Value};

// JS String(v) (no ?? fallback): null→"null", string→itself, number/bool→text, array→join(","), object→"[object Object]".
fn js_string(v: &Value) -> String {
    match v {
        Value::Null => "null".into(),
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Array(a) => a.iter().map(js_string).collect::<Vec<_>>().join(","),
        Value::Object(_) => "[object Object]".into(),
    }
}
// JS Number(v)
fn js_number(v: Option<&Value>) -> f64 {
    match v {
        None => f64::NAN,
        Some(Value::Null) => 0.0,
        Some(Value::Bool(b)) => if *b { 1.0 } else { 0.0 },
        Some(Value::Number(n)) => n.as_f64().unwrap_or(f64::NAN),
        Some(Value::String(s)) => { let t = s.trim(); if t.is_empty() { 0.0 } else { t.parse().unwrap_or(f64::NAN) } }
        Some(_) => f64::NAN,
    }
}
// JSON.stringify(Number(id)): integral → integer render; NaN → null.
fn num_value(n: f64) -> Value {
    if n.is_nan() { Value::Null }
    else if n.fract() == 0.0 && n.abs() < 9_007_199_254_740_992.0 { json!(n as i64) }
    else { json!(n) }
}
// JS `??` right operand: keep only when present-and-non-null.
fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) }

pub fn dataStoryUpdate(input: &Value, db: &Connection) -> Result<Value, String> {
    let origin = nz(input.get("origin")).cloned().unwrap_or_else(|| json!("atlas"));
    let id_raw = input.get("id");
    if id_raw.map_or(true, |v| v.is_null()) {
        return Err("data-story-update: id is required".into());
    }
    let id_num = js_number(id_raw);
    let id_param = num_value(id_num); // = Number(id)

    let found = crate::db_json::query_rows(
        db,
        "SELECT id FROM code_data_stories WHERE origin=? AND id=?",
        &[origin.clone(), id_param.clone()],
    )
    .map(|r| r.as_array().map(|a| !a.is_empty()).unwrap_or(false))
    .unwrap_or(false);
    if !found {
        return Err(format!("data-story-update: data story '{}' not found", js_string(id_raw.unwrap())));
    }

    let mut sets: Vec<String> = Vec::new();
    let mut vals: Vec<Value> = Vec::new();

    if let Some(nm) = input.get("name") {
        if !nm.is_null() {
            let s: String = js_string(nm).chars().take(200).collect();
            sets.push("name=?".into());
            vals.push(json!(s));
        }
    }
    if let Some(desc) = input.get("description") {
        sets.push("description=?".into());
        vals.push(desc.clone());
    }
    if let Some(dir) = input.get("direction") {
        if !dir.is_null() {
            let d = if dir.as_str() == Some("backward") { "backward" } else { "forward" };
            sets.push("direction=?".into());
            vals.push(json!(d));
        }
    }
    if let Some(hops) = input.get("hops") {
        let arr = if hops.is_array() { hops.clone() } else { json!([]) };
        sets.push("hops_json=?".into());
        vals.push(json!(serde_json::to_string(&arr).unwrap()));
    }
    if let Some(lt) = input.get("linked_test") {
        sets.push("linked_test=?".into());
        vals.push(lt.clone());
    }

    if sets.is_empty() {
        return Ok(json!({ "id": id_param, "updated": false }));
    }

    // JS: sets.push(`updated_at=datetime('now')`) — frozen-clock lane pins 'now' to the canonical instant.
    sets.push(format!("updated_at={}", crate::clock::now_sql()));
    let sql = format!("UPDATE code_data_stories SET {} WHERE origin=? AND id=?", sets.join(", "));
    let mut params = vals;
    params.push(origin);
    params.push(id_param.clone());
    crate::db_json::run_write(db, &sql, &params)?;

    Ok(json!({ "id": id_param, "updated": true }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE code_data_stories (id INTEGER PRIMARY KEY, origin TEXT, name TEXT, description TEXT, direction TEXT, hops_json TEXT, linked_test TEXT, created_at TEXT, updated_at TEXT); \
             INSERT INTO code_data_stories (id,origin,name,description,direction,hops_json,linked_test,created_at,updated_at) \
               VALUES (7,'atlas','old name','old desc','forward','[]','old.js','2025-06-01 00:00:00','2025-06-01 00:00:00');",
        )
        .unwrap();
        c
    }

    #[test]
    fn no_op_when_nothing_to_set() {
        let c = fresh();
        let r = dataStoryUpdate(&json!({ "id": 7 }), &c).unwrap();
        assert_eq!(r, json!({ "id": 7, "updated": false }));
        // untouched
        let ua: String = c.query_row("SELECT updated_at FROM code_data_stories WHERE id=7", [], |r| r.get(0)).unwrap();
        assert_eq!(ua, "2025-06-01 00:00:00");
    }

    #[test]
    fn updates_provided_fields_and_stamps_frozen_now() {
        let c = fresh();
        let long: String = "x".repeat(210);
        let r = dataStoryUpdate(
            &json!({ "id": "7", "name": long, "description": "new", "direction": "backward",
                     "hops": [{ "node_id": 1, "label": "a" }, { "node_id": 2 }], "linked_test": "new.js" }),
            &c,
        )
        .unwrap();
        assert_eq!(r, json!({ "id": 7, "updated": true }));
        let (name, desc, dir, hops, lt, ua): (String, String, String, String, String, String) = c
            .query_row(
                "SELECT name, description, direction, hops_json, linked_test, updated_at FROM code_data_stories WHERE id=7",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?)),
            )
            .unwrap();
        assert_eq!(name.chars().count(), 200); // sliced to 200
        assert_eq!(desc, "new");
        assert_eq!(dir, "backward");
        assert_eq!(hops, "[{\"node_id\":1,\"label\":\"a\"},{\"node_id\":2}]");
        assert_eq!(lt, "new.js");
        assert_eq!(ua, "2026-01-01 00:00:00");
    }

    #[test]
    fn non_backward_forces_forward_and_non_array_hops_empty() {
        let c = fresh();
        let r = dataStoryUpdate(&json!({ "id": 7, "direction": "sideways", "hops": "nope", "description": null }), &c).unwrap();
        assert_eq!(r, json!({ "id": 7, "updated": true }));
        let (dir, hops): (String, String) = c
            .query_row("SELECT direction, hops_json FROM code_data_stories WHERE id=7", [], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap();
        assert_eq!(dir, "forward");
        assert_eq!(hops, "[]");
        let desc: Option<String> = c.query_row("SELECT description FROM code_data_stories WHERE id=7", [], |r| r.get(0)).unwrap();
        assert_eq!(desc, None); // set to NULL
    }

    #[test]
    fn missing_row_errors() {
        let c = fresh();
        assert!(dataStoryUpdate(&json!({ "id": 999, "name": "x" }), &c).is_err());
    }
}
