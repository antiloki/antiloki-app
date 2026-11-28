//! symbol-find.rs — twin of skills/codebase/symbol-find.mjs (db READ lane). NAMES ARE CONTRACT:
//! `symbolFind` (named default). Ranked name/path resolution + immediate neighbors (Wave D).
// QUALITY: 78/100 — manual LIKE-escape (no regex crate), CASE rank SQL, per-row neighbor query
use rusqlite::Connection;
use serde_json::{json, Map, Value};

// JS: query.replace(/[%_\\]/g, m => '\\' + m) — escape LIKE metachars. Done by hand to avoid the regex crate.
fn like_escape(q: &str) -> String {
    let mut out = String::new();
    for ch in q.chars() {
        if ch == '%' || ch == '_' || ch == '\\' {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

pub fn symbolFind(input: &Value, db: &Connection) -> Result<Value, String> {
    let query = input.get("query").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
    if query.is_empty() {
        return Err("symbol-find: query is required".into());
    }
    let origin = input.get("origin").and_then(|v| v.as_str()).unwrap_or("atlas").to_string();
    let limit = (input.get("limit").and_then(|v| v.as_f64()).unwrap_or(10.0) as i64).clamp(1, 100);
    let like = format!("%{}%", like_escape(&query));

    let rows = crate::db_json::query_rows(
        db,
        "SELECT id, origin, path, type, name, signature, intent, parent_id, \
           CASE WHEN name = ? THEN 0 WHEN name LIKE ? ESCAPE '\\' THEN 1 ELSE 2 END AS rank \
         FROM code_nodes \
         WHERE origin = ? AND (name = ? OR name LIKE ? ESCAPE '\\' OR path LIKE ? ESCAPE '\\') \
         ORDER BY rank ASC, length(COALESCE(name, '')) ASC, path ASC \
         LIMIT ?",
        &[json!(query), json!(like), json!(origin), json!(query), json!(like), json!(like), json!(limit)],
    )?;

    let mut matches: Vec<Value> = Vec::new();
    for row in rows.as_array().unwrap() {
        let mut node = row.as_object().cloned().unwrap_or_default();
        node.remove("rank"); // { rank, ...node } — strip the ranking column
        let id = row.get("id").cloned().unwrap_or(Value::Null);
        let neighbors = crate::db_json::query_rows(
            db,
            "SELECT id, path, name, type FROM code_nodes WHERE id IN ( \
               SELECT to_node FROM code_edges WHERE origin = ? AND from_node = ? \
               UNION SELECT from_node FROM code_edges WHERE origin = ? AND to_node = ? \
             ) LIMIT 20",
            &[json!(origin), id.clone(), json!(origin), id],
        )?;
        let mut m = Map::new();
        m.insert("node".into(), Value::Object(node));
        m.insert("neighbors".into(), neighbors);
        matches.push(Value::Object(m));
    }
    Ok(json!({ "query": query, "origin": origin, "count": matches.len(), "matches": matches }))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ranks_exact_then_substring() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE code_nodes (id TEXT, origin TEXT, path TEXT, type TEXT, name TEXT, signature TEXT, intent TEXT, parent_id TEXT); \
          CREATE TABLE code_edges (origin TEXT, from_node TEXT, to_node TEXT, type TEXT, kind TEXT); \
          INSERT INTO code_nodes (id,origin,path,type,name) VALUES ('a','atlas','src/auth.ts','function','login'),('b','atlas','src/o.ts','function','loginThrottle'); \
          INSERT INTO code_edges (origin,from_node,to_node,type) VALUES ('atlas','a','x','calls');").unwrap();
        let r = symbolFind(&json!({ "query": "login" }), &c).unwrap();
        assert_eq!(r["matches"][0]["node"]["name"], json!("login")); // exact first
        assert_eq!(r["count"], json!(2));
        assert!(symbolFind(&json!({ "query": "  " }), &c).is_err());
    }
}
