//! data-story-get.rs — twin of skills/codebase/data-story-get.mjs (db lane). NAMES ARE CONTRACT:
//! `dataStoryGet` (default) + `verifyStory` (named, db-first helper). Read a story + verify vs the atlas.
// QUALITY: 80/100 — spine-hop edge verification, per-hop meta resolve, status compute; helper via sig lane
use rusqlite::Connection;
use serde_json::{json, Value};

fn js_string(v: &Value) -> String {
    match v { Value::String(s) => s.clone(), Value::Null => "undefined".into(), o => o.to_string() }
}
fn truthy(v: &Value) -> bool {
    match v { Value::Bool(b) => *b, Value::Null => false, Value::String(s) => !s.is_empty(),
        Value::Number(n) => n.as_f64().map(|f| f != 0.0).unwrap_or(false), _ => true }
}

// db-first helper: verify a story's hops against the live call/import graph.
pub fn verifyStory(db: &Connection, origin: &str, hops: &Value) -> Value {
    let meta_of = |id: &Value| -> Value {
        crate::db_json::query_rows(db, "SELECT id, name, path, type FROM code_nodes WHERE origin=? AND id=? LIMIT 1", &[json!(origin), json!(js_string(id))])
            .ok().and_then(|r| r.as_array().and_then(|a| a.first()).cloned()).unwrap_or(Value::Null)
    };
    let connected = |a: &str, b: &str| -> bool {
        crate::db_json::query_rows(db,
            "SELECT 1 FROM code_edges WHERE origin=? AND ((from_node=? AND to_node=?) OR (from_node=? AND to_node=?)) \
             AND type IN ('calls','imports','uses','renders','dataflow','dispatches') LIMIT 1",
            &[json!(origin), json!(a), json!(b), json!(b), json!(a)])
            .ok().map(|r| !r.as_array().map(|x| x.is_empty()).unwrap_or(true)).unwrap_or(false)
    };

    let hop_list = hops.as_array().cloned().unwrap_or_default();
    let mut out: Vec<Value> = Vec::new();
    let mut prev_spine: Option<String> = None;
    let (mut broken, mut missing) = (0i64, 0i64);
    for h in &hop_list {
        let node_id = h.get("node_id").cloned().unwrap_or(Value::Null);
        let meta = meta_of(&node_id);
        let is_branch = h.get("branch").map(truthy).unwrap_or(false);
        let mut edge_ok = Value::Null;
        if let Some(ps) = &prev_spine {
            let ok = connected(ps, &js_string(&node_id));
            edge_ok = json!(ok);
            if !ok { broken += 1; }
        }
        let exists = !meta.is_null();
        if !exists { missing += 1; }
        let mut ho = h.as_object().cloned().unwrap_or_default(); // {...h, meta, exists, edge_ok}
        ho.insert("meta".into(), meta);
        ho.insert("exists".into(), json!(exists));
        ho.insert("edge_ok".into(), edge_ok);
        out.push(Value::Object(ho));
        if !is_branch { prev_spine = Some(js_string(&node_id)); }
    }
    let status = if out.is_empty() { "empty" } else if missing > 0 || broken > 0 { "broken" } else { "ok" };
    json!({ "hops": out, "status": status, "broken": broken, "missing": missing })
}

pub fn dataStoryGet(input: &Value, db: &Connection) -> Result<Value, String> {
    let origin = input.get("origin").and_then(|v| v.as_str()).unwrap_or("atlas").to_string();
    let id = input.get("id");
    if id.map(|v| v.is_null()).unwrap_or(true) { return Err("data-story-get: id is required".into()); }
    let id = id.unwrap();
    let id_num = id.as_i64().or_else(|| id.as_str().and_then(|s| s.trim().parse::<i64>().ok())).unwrap_or(0); // Number(id)

    let row = crate::db_json::query_rows(db, "SELECT id, name, description, direction, hops_json, linked_test, created_at, updated_at FROM code_data_stories WHERE origin=? AND id=?", &[json!(origin), json!(id_num)])?;
    let row0 = match row.as_array().and_then(|a| a.first()) {
        Some(r) => r.clone(),
        None => return Err(format!("data-story-get: data story '{}' not found", js_string(id))),
    };
    // hops = JSON.parse(hops_json ?? '[]') catch []
    let hops = match row0.get("hops_json").and_then(|v| v.as_str()) {
        Some(s) => serde_json::from_str::<Value>(s).unwrap_or(json!([])),
        None => json!([]),
    };
    let v = verifyStory(db, &origin, &hops);

    let linked_test = row0.get("linked_test").cloned().unwrap_or(Value::Null);
    let test_covered = if linked_test.as_str().map(|s| !s.is_empty()).unwrap_or(false) {
        let lt = linked_test.as_str().unwrap();
        let ex = crate::db_json::query_rows(db, "SELECT 1 FROM code_nodes WHERE origin=? AND type='file' AND path=? LIMIT 1", &[json!(origin), json!(lt)])?;
        json!({ "linked_test": lt, "exists": !ex.as_array().map(|a| a.is_empty()).unwrap_or(true) })
    } else { Value::Null };

    let g = |k: &str| row0.get(k).cloned().unwrap_or(Value::Null);
    Ok(json!({
        "id": g("id"), "name": g("name"), "description": g("description"), "direction": g("direction"),
        "linked_test": g("linked_test"), "created_at": g("created_at"), "updated_at": g("updated_at"),
        "hops": v["hops"], "status": v["status"], "broken": v["broken"], "missing": v["missing"], "test_covered": test_covered
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn verifies_story() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE code_nodes (id TEXT, origin TEXT, name TEXT, path TEXT, type TEXT); \
          CREATE TABLE code_edges (origin TEXT, from_node TEXT, to_node TEXT, type TEXT, kind TEXT); \
          CREATE TABLE code_data_stories (id INTEGER, origin TEXT, name TEXT, description TEXT, direction TEXT, hops_json TEXT, linked_test TEXT, created_at TEXT, updated_at TEXT); \
          INSERT INTO code_nodes VALUES ('a','atlas','a','a.ts','function'),('b','atlas','b','b.ts','function'); \
          INSERT INTO code_edges (origin,from_node,to_node,type) VALUES ('atlas','a','b','calls'); \
          INSERT INTO code_data_stories VALUES (1,'atlas','s','d','forward','[{\"node_id\":\"a\"},{\"node_id\":\"b\"}]',NULL,NULL,NULL);").unwrap();
        let r = dataStoryGet(&json!({ "id": 1 }), &c).unwrap();
        assert_eq!(r["status"], json!("ok")); // a→b connected
        assert_eq!(r["hops"].as_array().unwrap().len(), 2);
        assert!(dataStoryGet(&json!({ "id": 99 }), &c).is_err());
    }
}
