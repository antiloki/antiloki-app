//! memory-use-list.rs — twin of skills/entities/memory-use-list.mjs (db READ lane, attached
//! `memory` db). NAMES ARE CONTRACT: `memoryUseList` (default). Lists memory_uses with the LATEST
//! outcome per use (LEFT JOIN on MAX(rated_at) subquery); filters AND-compose; ORDER BY u.id DESC.
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

pub fn memoryUseList(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS throws err.contextMissing('memory-use-list','db') when no db — the db lane always
    // injects one, so that guard is unreachable here.
    let mut sql = String::from(
        "\n    SELECT u.*,\n           o.outcome   AS latest_outcome,\n           o.rated_by  AS latest_outcome_rated_by,\n           o.rated_at  AS latest_outcome_rated_at\n    FROM memory.memory_uses u\n    LEFT JOIN (\n      SELECT use_id, outcome, rated_by, rated_at\n      FROM memory.memory_use_outcomes o1\n      WHERE rated_at = (\n        SELECT MAX(rated_at) FROM memory.memory_use_outcomes o2 WHERE o2.use_id = o1.use_id\n      )\n    ) o ON o.use_id = u.id\n    WHERE 1=1",
    );
    let mut params: Vec<Value> = Vec::new();

    // JS `memory_id != null`: fires for ANY present non-null value (0 and '' included).
    if let Some(v) = input.get("memory_id").filter(|x| !x.is_null()) {
        sql.push_str(" AND u.memory_id = ?");
        params.push(v.clone());
    }
    if truthy(input.get("used_by")) {
        sql.push_str(" AND u.used_by = ?");
        params.push(input.get("used_by").cloned().unwrap());
    }
    if truthy(input.get("context_card")) {
        sql.push_str(" AND u.context_card = ?");
        params.push(input.get("context_card").cloned().unwrap());
    }
    if truthy(input.get("context_session")) {
        sql.push_str(" AND u.context_session = ?");
        params.push(input.get("context_session").cloned().unwrap());
    }
    if truthy(input.get("since")) {
        sql.push_str(" AND u.used_at >= ?");
        params.push(input.get("since").cloned().unwrap());
    }
    if truthy(input.get("ratedOnly")) {
        sql.push_str(" AND o.outcome IS NOT NULL");
    }
    if truthy(input.get("outcome")) {
        sql.push_str(" AND o.outcome = ?");
        params.push(input.get("outcome").cloned().unwrap());
    }
    sql.push_str(" ORDER BY u.id DESC");
    // JS: limit && Number.isInteger(limit) && limit > 0 (integer-VALUED numbers pass, 2.5 does not)
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
            CREATE TABLE memory.memory_uses ( id INTEGER PRIMARY KEY AUTOINCREMENT, memory_id INTEGER NOT NULL, used_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')), used_by TEXT NOT NULL, context_card TEXT, context_session TEXT, context_skill TEXT, retrieval_method TEXT, retrieval_score REAL, notes TEXT, CHECK (retrieval_method IN ('similarity', 'graph', 'rag-context', 'direct') OR retrieval_method IS NULL) );
            CREATE TABLE memory.memory_use_outcomes ( id INTEGER PRIMARY KEY AUTOINCREMENT, use_id INTEGER NOT NULL, outcome TEXT NOT NULL, rated_by TEXT NOT NULL, rated_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')), notes TEXT, CHECK (outcome IN ('helpful', 'irrelevant', 'misleading', 'partial')), FOREIGN KEY (use_id) REFERENCES memory_uses(id) );
            INSERT INTO memory.memory_uses (id,memory_id,used_at,used_by,context_card,context_session,context_skill,retrieval_method,retrieval_score,notes) VALUES
              (1,10,'2025-12-20 08:00:00','agent:pl','C-1','s1',NULL,'similarity',0.5,NULL),
              (2,10,'2025-12-28 09:00:00','agent:tl','C-2','s1',NULL,NULL,NULL,'n2'),
              (3,11,'2026-01-01 00:00:00','agent:pl','C-2','s2',NULL,'direct',NULL,NULL),
              (4,11,'2025-11-01 12:00:00','human:dani',NULL,NULL,NULL,NULL,NULL,NULL);
            INSERT INTO memory.memory_use_outcomes (id,use_id,outcome,rated_by,rated_at,notes) VALUES
              (1,1,'partial','agent:self','2025-12-21 10:00:00',NULL),
              (2,1,'helpful','human:dani','2025-12-22 10:00:00',NULL),
              (3,3,'misleading','agent:qa','2026-01-01 00:00:00',NULL);").unwrap();
        c
    }
    fn ids(v: &Value) -> Vec<i64> { v.as_array().unwrap().iter().map(|r| r["id"].as_i64().unwrap()).collect() }

    #[test]
    fn lists_all_desc_with_latest_outcome_per_use() {
        let r = memoryUseList(&json!({}), &seeded()).unwrap();
        assert_eq!(ids(&r), vec![4, 3, 2, 1]);
        let rows = r.as_array().unwrap();
        // use 1: TWO ratings — latest (max rated_at) wins
        assert_eq!(rows[3]["latest_outcome"], json!("helpful"));
        assert_eq!(rows[3]["latest_outcome_rated_by"], json!("human:dani"));
        assert_eq!(rows[3]["latest_outcome_rated_at"], json!("2025-12-22 10:00:00"));
        // use 2: never rated — nulls from the LEFT JOIN
        assert_eq!(rows[2]["latest_outcome"], Value::Null);
        assert_eq!(rows[2]["latest_outcome_rated_by"], Value::Null);
        // u.* carries every memory_uses column
        assert_eq!(rows[3]["retrieval_method"], json!("similarity"));
        assert_eq!(rows[3]["retrieval_score"], json!(0.5));
        assert_eq!(rows[2]["notes"], json!("n2"));
    }

    #[test]
    fn filters_compose() {
        let c = seeded();
        assert_eq!(ids(&memoryUseList(&json!({ "memory_id": 10 }), &c).unwrap()), vec![2, 1]);
        assert_eq!(ids(&memoryUseList(&json!({ "used_by": "agent:pl" }), &c).unwrap()), vec![3, 1]);
        assert_eq!(ids(&memoryUseList(&json!({ "context_card": "C-2" }), &c).unwrap()), vec![3, 2]);
        assert_eq!(ids(&memoryUseList(&json!({ "context_session": "s1" }), &c).unwrap()), vec![2, 1]);
        assert_eq!(ids(&memoryUseList(&json!({ "since": "2025-12-25 00:00:00" }), &c).unwrap()), vec![3, 2]);
        assert_eq!(ids(&memoryUseList(&json!({ "ratedOnly": true }), &c).unwrap()), vec![3, 1]);
        assert_eq!(ids(&memoryUseList(&json!({ "outcome": "helpful" }), &c).unwrap()), vec![1]);
        assert_eq!(ids(&memoryUseList(&json!({ "limit": 2 }), &c).unwrap()), vec![4, 3]);
        assert_eq!(ids(&memoryUseList(&json!({ "memory_id": 11, "ratedOnly": true, "limit": 1 }), &c).unwrap()), vec![3]);
    }

    #[test]
    fn falsy_filters_and_non_integer_limit_are_ignored() {
        let c = seeded();
        // used_by:'' is falsy, limit:0 fails `limit > 0`, limit:2.5 fails Number.isInteger
        assert_eq!(ids(&memoryUseList(&json!({ "used_by": "", "limit": 0 }), &c).unwrap()), vec![4, 3, 2, 1]);
        assert_eq!(ids(&memoryUseList(&json!({ "limit": 2.5 }), &c).unwrap()), vec![4, 3, 2, 1]);
        assert_eq!(ids(&memoryUseList(&Value::Null, &c).unwrap()), vec![4, 3, 2, 1]); // input ?? {}
    }

    #[test]
    fn empty_when_no_uses() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS memory;
            CREATE TABLE memory.memory_uses ( id INTEGER PRIMARY KEY AUTOINCREMENT, memory_id INTEGER NOT NULL, used_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')), used_by TEXT NOT NULL, context_card TEXT, context_session TEXT, context_skill TEXT, retrieval_method TEXT, retrieval_score REAL, notes TEXT );
            CREATE TABLE memory.memory_use_outcomes ( id INTEGER PRIMARY KEY AUTOINCREMENT, use_id INTEGER NOT NULL, outcome TEXT NOT NULL, rated_by TEXT NOT NULL, rated_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')), notes TEXT );").unwrap();
        assert_eq!(memoryUseList(&json!({}), &c).unwrap(), json!([]));
    }

    #[test]
    fn rated_at_tie_duplicates_the_join_row() {
        // two outcomes share MAX(rated_at) → subquery keeps both → use row appears twice (JS parity)
        let c = seeded();
        c.execute_batch("INSERT INTO memory.memory_use_outcomes (id,use_id,outcome,rated_by,rated_at) VALUES
            (4,2,'partial','agent:a','2025-12-30 00:00:00'),
            (5,2,'irrelevant','agent:b','2025-12-30 00:00:00');").unwrap();
        assert_eq!(ids(&memoryUseList(&json!({ "memory_id": 10 }), &c).unwrap()), vec![2, 2, 1]);
    }
}
