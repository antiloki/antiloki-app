//! workflow-list.rs — twin of skills/workflow/workflow-list.mjs (db lane). NAMES ARE CONTRACT:
//! `workflowList` (named default). Read-only query over the workflows table.
// QUALITY: 86/100 — the SQL-building logic is the port; db_json runs it
use rusqlite::Connection;
use serde_json::{json, Value};

pub fn workflowList(input: &Value, db: &Connection) -> Result<Value, String> {
    let mut sql = String::from("SELECT id, name, description, owner_agent, on_failure, created_at FROM workflows WHERE 1=1");
    let mut params: Vec<Value> = Vec::new();
    // JS: owner = input?.owner_agent ?? null; if (owner) ...  (truthy string)
    if let Some(o) = input.get("owner_agent").and_then(|v| v.as_str()).filter(|s| !s.is_empty()) {
        sql.push_str(" AND owner_agent = ?");
        params.push(json!(o));
    }
    sql.push_str(" ORDER BY name");
    // JS: Number.isInteger(input?.limit) && input.limit > 0 ? input.limit : null
    if let Some(l) = input.get("limit").and_then(|v| v.as_i64()).filter(|&n| n > 0) {
        sql.push_str(" LIMIT ?");
        params.push(json!(l));
    }
    crate::db_json::query_rows(db, &sql, &params)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn seeded() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE workflows (id INTEGER PRIMARY KEY, name TEXT, description TEXT, owner_agent TEXT, on_failure TEXT, created_at TEXT);
            INSERT INTO workflows VALUES (1,'b','B','system','abort','t1'),(2,'a','A','god','skip','t2');").unwrap();
        c
    }
    #[test]
    fn orders_by_name_and_filters_owner() {
        let all = workflowList(&json!({}), &seeded()).unwrap();
        assert_eq!(all.as_array().unwrap().len(), 2);
        assert_eq!(all[0]["name"], json!("a")); // ordered
        let sys = workflowList(&json!({ "owner_agent": "system" }), &seeded()).unwrap();
        assert_eq!(sys.as_array().unwrap().len(), 1);
        assert_eq!(sys[0]["name"], json!("b"));
    }
}
