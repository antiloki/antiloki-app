//! knowledge-find.rs — twin of skills/entities/knowledge-find.mjs (db lane, cross-db via ATTACH).
//! NAMES ARE CONTRACT: `knowledgeFind` (default). SQL fallback + structured query over
//! knowledge.extracts joined with knowledge.documents for provenance.
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

pub fn knowledgeFind(input: &Value, db: &Connection) -> Result<Value, String> {
    let mut sql = String::from(
        "\n    SELECT e.*, d.vault_path, d.doc_type, d.summary AS document_summary\n    FROM knowledge.extracts e\n    JOIN knowledge.documents d ON d.id = e.document_id\n    WHERE d.superseded_by IS NULL\n  ",
    );
    let mut params: Vec<Value> = Vec::new();
    if truthy(input.get("extract_type")) {
        sql.push_str(" AND e.extract_type = ?");
        params.push(input.get("extract_type").cloned().unwrap_or(Value::Null));
    }
    // JS `document_id != null` is LOOSE: only null/undefined skip — 0/''/false still filter.
    if let Some(id) = input.get("document_id").filter(|v| !v.is_null()) {
        sql.push_str(" AND e.document_id = ?");
        params.push(id.clone());
    }
    if truthy(input.get("doc_type")) {
        sql.push_str(" AND d.doc_type = ?");
        params.push(input.get("doc_type").cloned().unwrap_or(Value::Null));
    }
    if truthy(input.get("query")) {
        let q = input
            .get("query")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "query.toLowerCase is not a function".to_string())?;
        let like = format!("%{}%", q.to_lowercase());
        sql.push_str(" AND (LOWER(e.text) LIKE ? OR LOWER(COALESCE(e.context, '')) LIKE ?)");
        params.push(json!(like));
        params.push(json!(like));
    }
    sql.push_str(" ORDER BY e.id DESC");
    // destructuring default fires only when the key is ABSENT (JS undefined) — explicit null stays null
    let limit = input.get("limit").cloned().unwrap_or(json!(20));
    // JS: `limit && Number.isInteger(limit) && limit > 0` — non-numbers never pass Number.isInteger
    let use_limit = match &limit {
        Value::Number(n) => n.as_f64().map_or(false, |f| f.is_finite() && f.fract() == 0.0 && f > 0.0),
        _ => false,
    };
    if use_limit {
        sql.push_str(" LIMIT ?");
        params.push(limit);
    }
    crate::db_json::query_rows(db, &sql, &params)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn seeded() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "ATTACH ':memory:' AS knowledge;
            CREATE TABLE knowledge.documents (id INTEGER PRIMARY KEY, vault_path TEXT, doc_type TEXT, sha256 TEXT, bytes INTEGER, summary TEXT, ingested_at TEXT, ingested_by TEXT, external_source TEXT, superseded_by INTEGER, superseded_at TEXT);
            CREATE TABLE knowledge.extracts (id INTEGER PRIMARY KEY, document_id INTEGER, extract_type TEXT, text TEXT, context TEXT, page_or_section TEXT, created_at TEXT, created_by TEXT);
            INSERT INTO knowledge.documents (id, vault_path, doc_type, sha256, bytes, summary, ingested_at, ingested_by) VALUES
              (1,'books/alpha.md','md','aaa',100,'alpha book','2025-12-01 10:00:00','tester'),
              (2,'papers/beta.pdf','pdf','bbb',200,'beta paper','2025-12-02 10:00:00','tester');
            INSERT INTO knowledge.documents (id, vault_path, doc_type, sha256, bytes, summary, ingested_at, ingested_by, superseded_by) VALUES
              (3,'books/old-alpha.md','md','ccc',90,'old alpha','2025-11-01 10:00:00','tester',1);
            INSERT INTO knowledge.extracts (id, document_id, extract_type, text, context, page_or_section, created_at, created_by) VALUES
              (1,1,'quote','The Mixed Case Sentence',NULL,'p.1','2025-12-01 11:00:00','tester'),
              (2,1,'insight','plain body here','surrounding MIXED words','ch.2','2025-12-01 12:00:00','tester'),
              (3,2,'quote','beta doc line','beta ctx',NULL,'2025-12-02 11:00:00','tester'),
              (4,3,'quote','superseded mixed text',NULL,NULL,'2025-11-01 11:00:00','tester');",
        )
        .unwrap();
        c
    }
    fn ids(v: &Value) -> Vec<i64> {
        v.as_array().unwrap().iter().map(|r| r["id"].as_i64().unwrap()).collect()
    }
    #[test]
    fn filters_join_and_limit() {
        let c = seeded();
        // no filters: id DESC, superseded doc's extract (4) excluded, default LIMIT 20
        let all = knowledgeFind(&json!({}), &c).unwrap();
        assert_eq!(ids(&all), vec![3, 2, 1]);
        assert_eq!(all[0]["vault_path"], json!("papers/beta.pdf"));
        assert_eq!(all[0]["document_summary"], json!("beta paper"));
        // null input = all defaults
        assert_eq!(ids(&knowledgeFind(&Value::Null, &c).unwrap()), vec![3, 2, 1]);
        assert_eq!(ids(&knowledgeFind(&json!({ "extract_type": "quote" }), &c).unwrap()), vec![3, 1]);
        assert_eq!(ids(&knowledgeFind(&json!({ "document_id": 2 }), &c).unwrap()), vec![3]);
        assert_eq!(ids(&knowledgeFind(&json!({ "doc_type": "md" }), &c).unwrap()), vec![2, 1]);
        // case-insensitive over text OR context (COALESCE covers NULL context)
        assert_eq!(ids(&knowledgeFind(&json!({ "query": "MiXeD" }), &c).unwrap()), vec![2, 1]);
        assert_eq!(ids(&knowledgeFind(&json!({ "query": "no-hit" }), &c).unwrap()), Vec::<i64>::new());
        assert_eq!(ids(&knowledgeFind(&json!({ "limit": 1 }), &c).unwrap()), vec![3]);
        // limit 0 / null / non-integer → no LIMIT clause
        assert_eq!(ids(&knowledgeFind(&json!({ "limit": 0 }), &c).unwrap()), vec![3, 2, 1]);
        assert_eq!(ids(&knowledgeFind(&json!({ "limit": null }), &c).unwrap()), vec![3, 2, 1]);
        assert_eq!(ids(&knowledgeFind(&json!({ "limit": 2.5 }), &c).unwrap()), vec![3, 2, 1]);
        // combined
        assert_eq!(ids(&knowledgeFind(&json!({ "extract_type": "quote", "query": "beta", "limit": 5 }), &c).unwrap()), vec![3]);
    }
}
