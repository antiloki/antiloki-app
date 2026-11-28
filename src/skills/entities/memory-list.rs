//! memory-list.rs — twin of skills/entities/memory-list.mjs (db lane, cross-db: memory.memories).
//! NAMES ARE CONTRACT: `memoryList`. Active (non-superseded) memories by default; filters AND-compose.
// QUALITY: 88/100 — filter SQL building (type/tag/source_card/validated/minConfidence/limit); db_json runs it
use rusqlite::Connection;
use serde_json::{json, Value};

fn truthy(v: Option<&Value>) -> bool { match v { None|Some(Value::Null)=>false, Some(Value::Bool(b))=>*b, Some(Value::Number(n))=>n.as_f64().map_or(false,|f|f!=0.0&&!f.is_nan()), Some(Value::String(s))=>!s.is_empty(), Some(_)=>true } }

pub fn memoryList(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS throws err.contextMissing('memory-list','db') when no db — the db lane always injects one.
    let mut sql = String::from("SELECT m.* FROM memory.memories m WHERE 1=1");
    let mut params: Vec<Value> = Vec::new();

    // destructuring default: activeOnly = true only when the key is absent (JSON has no undefined)
    let active_only = match input.get("activeOnly") { None => true, v => truthy(v) };
    if active_only { sql.push_str(" AND m.superseded_by IS NULL"); }
    if truthy(input.get("type")) {
        sql.push_str(" AND m.type = ?");
        params.push(input.get("type").cloned().unwrap());
    }
    if truthy(input.get("source_card")) {
        sql.push_str(" AND m.source_card = ?");
        params.push(input.get("source_card").cloned().unwrap());
    }
    // strict === true / === false: only JSON booleans pass (1/0/"true" do not)
    match input.get("validated") {
        Some(Value::Bool(true)) => sql.push_str(" AND m.validated = 1"),
        Some(Value::Bool(false)) => sql.push_str(" AND m.validated = 0"),
        _ => {}
    }
    if let Some(n @ Value::Number(_)) = input.get("minConfidence") {
        sql.push_str(" AND m.confidence >= ?");
        params.push(n.clone());
    }
    if truthy(input.get("tag")) {
        sql.push_str(" AND EXISTS (SELECT 1 FROM json_each(m.tags_json) je WHERE je.value = ?)");
        params.push(input.get("tag").cloned().unwrap());
    }
    sql.push_str(" ORDER BY m.id DESC");
    // JS: limit && Number.isInteger(limit) && limit > 0 (integer-VALUED numbers pass, 5.5 does not)
    if let Some(l) = input.get("limit").and_then(|v| v.as_f64()).filter(|f| f.is_finite() && f.fract() == 0.0 && *f > 0.0) {
        sql.push_str(" LIMIT ?");
        params.push(json!(l as i64));
    }
    crate::db_json::query_rows(db, &sql, &params)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn seeded() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS memory;
            CREATE TABLE memory.memories (id INTEGER PRIMARY KEY AUTOINCREMENT, type TEXT NOT NULL, title TEXT NOT NULL, tags_json TEXT NOT NULL DEFAULT '[]', source_card TEXT, confidence INTEGER NOT NULL DEFAULT 0, validated INTEGER NOT NULL DEFAULT 0, superseded_by INTEGER, created_by TEXT NOT NULL);
            INSERT INTO memory.memories (id,type,title,tags_json,source_card,confidence,validated,superseded_by,created_by) VALUES
              (1,'lesson','T1','[\"ai\"]',NULL,30,0,NULL,'test'),
              (2,'preference','T2','[\"style\"]',NULL,60,1,NULL,'test'),
              (3,'decision','T3','[\"ai\",\"arch\"]','CARD-X',90,0,NULL,'test'),
              (4,'lesson','T4-old','[]',NULL,50,0,3,'test');").unwrap();
        c
    }
    fn ids(v: &Value) -> Vec<i64> { v.as_array().unwrap().iter().map(|r| r["id"].as_i64().unwrap()).collect() }

    #[test]
    fn default_active_only_desc() {
        let r = memoryList(&json!({}), &seeded()).unwrap();
        assert_eq!(ids(&r), vec![3, 2, 1]); // superseded id=4 excluded, id DESC
    }
    #[test]
    fn active_only_false_includes_superseded() {
        assert_eq!(ids(&memoryList(&json!({ "activeOnly": false }), &seeded()).unwrap()), vec![4, 3, 2, 1]);
    }
    #[test]
    fn filters_compose() {
        let c = seeded();
        assert_eq!(ids(&memoryList(&json!({ "type": "lesson" }), &c).unwrap()), vec![1]);
        assert_eq!(ids(&memoryList(&json!({ "tag": "ai" }), &c).unwrap()), vec![3, 1]);
        assert_eq!(ids(&memoryList(&json!({ "source_card": "CARD-X" }), &c).unwrap()), vec![3]);
        assert_eq!(ids(&memoryList(&json!({ "validated": true }), &c).unwrap()), vec![2]);
        assert_eq!(ids(&memoryList(&json!({ "validated": false }), &c).unwrap()), vec![3, 1]);
        assert_eq!(ids(&memoryList(&json!({ "minConfidence": 60 }), &c).unwrap()), vec![3, 2]);
        assert_eq!(ids(&memoryList(&json!({ "limit": 2 }), &c).unwrap()), vec![3, 2]);
    }
    #[test]
    fn non_bool_validated_and_zero_limit_are_ignored() {
        // validated: 1 fails strict ===true/===false; limit: 0 is falsy — both filters skipped
        assert_eq!(ids(&memoryList(&json!({ "validated": 1, "limit": 0 }), &seeded()).unwrap()), vec![3, 2, 1]);
        assert_eq!(ids(&memoryList(&Value::Null, &seeded()).unwrap()), vec![3, 2, 1]); // input ?? {}
    }
}
