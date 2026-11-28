//! law-violations-list.rs — twin of skills/entities/law-violations-list.mjs (db READ lane,
//! cross-db laws via ATTACH). NAMES ARE CONTRACT: `lawViolationsList` (default). Filtered read
//! of the law_violations log (law_id/agent_name/action_name/outcome/since/limit), joined with
//! laws for slug/kind/domain, newest attempts first.
use rusqlite::Connection;
use serde_json::Value;

fn truthy(v: Option<&Value>) -> bool { match v { None|Some(Value::Null)=>false, Some(Value::Bool(b))=>*b, Some(Value::Number(n))=>n.as_f64().map_or(false,|f|f!=0.0&&!f.is_nan()), Some(Value::String(s))=>!s.is_empty(), Some(_)=>true } }

pub fn lawViolationsList(input: &Value, db: &Connection) -> Result<Value, String> {
    let law_id = input.get("law_id").filter(|v| !v.is_null()); // JS `law_id != null`: present + non-null (0/''/false still filter)
    let agent_name = input.get("agent_name");
    let action_name = input.get("action_name");
    let outcome = input.get("outcome");
    let since = input.get("since");

    let mut sql = String::from("\n    SELECT v.*, l.slug AS law_slug, l.kind AS law_kind, l.domain AS law_domain\n    FROM laws.law_violations v\n    JOIN laws.laws l ON l.id = v.law_id\n    WHERE 1=1\n  ");
    let mut params: Vec<Value> = Vec::new();
    if let Some(v) = law_id { sql.push_str(" AND v.law_id = ?"); params.push(v.clone()); }
    if truthy(agent_name) { sql.push_str(" AND v.agent_name = ?"); params.push(agent_name.unwrap().clone()); }
    if truthy(action_name) { sql.push_str(" AND v.action_name = ?"); params.push(action_name.unwrap().clone()); }
    if truthy(outcome) { sql.push_str(" AND v.outcome = ?"); params.push(outcome.unwrap().clone()); }
    if truthy(since) { sql.push_str(" AND v.attempted_at >= ?"); params.push(since.unwrap().clone()); }
    sql.push_str(" ORDER BY v.attempted_at DESC");
    // JS: if (limit && Number.isInteger(limit) && limit > 0) — a finite integral positive NUMBER
    if let Some(l) = input.get("limit") {
        if let Some(f) = l.as_f64() {
            if f.is_finite() && f.fract() == 0.0 && f > 0.0 { sql.push_str(" LIMIT ?"); params.push(l.clone()); }
        }
    }
    crate::db_json::query_rows(db, &sql, &params)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS laws; \
          CREATE TABLE laws.laws (id INTEGER PRIMARY KEY, slug TEXT, kind TEXT, domain TEXT); \
          CREATE TABLE laws.law_violations (id INTEGER PRIMARY KEY, law_id INTEGER, attempted_at TEXT, attempted_by TEXT, agent_name TEXT, action_name TEXT, context_json TEXT, outcome TEXT); \
          INSERT INTO laws.laws (id,slug,kind,domain) VALUES \
            (1,'NEVER_BYPASS_WITH_AUDIT','constitutional','audit'), \
            (2,'ASK_BEFORE_DESTRUCTIVE','procedural','security'); \
          INSERT INTO laws.law_violations (id,law_id,attempted_at,attempted_by,agent_name,action_name,context_json,outcome) VALUES \
            (1,1,'2025-12-28 09:00:00','agent:dev','dev','dev.file.write','{\"path\":\".git/HEAD\"}','blocked'), \
            (2,1,'2025-12-29 10:00:00','agent:qa','qa','qa.card.update',NULL,'warned'), \
            (3,2,'2025-12-30 11:00:00','agent:dev','dev','dev.card.delete',NULL,'escaped'), \
            (4,2,'2025-12-31 12:00:00','human:dani',NULL,NULL,NULL,'allowed_exception');").unwrap();
        c
    }
    #[test]
    fn filters_join_order_limit() {
        let c = db();
        // no filters: all 4, ORDER BY attempted_at DESC, joined law columns appended after v.*
        let all = lawViolationsList(&json!({}), &c).unwrap();
        let rows = all.as_array().unwrap();
        assert_eq!(rows.iter().map(|r| r["id"].as_i64().unwrap()).collect::<Vec<_>>(), vec![4, 3, 2, 1]);
        assert_eq!(rows[0]["law_slug"], json!("ASK_BEFORE_DESTRUCTIVE"));
        assert_eq!(rows[3]["law_kind"], json!("constitutional"));
        assert_eq!(rows[3]["law_domain"], json!("audit"));
        // column key order is part of the output contract (v.* then aliases)
        assert_eq!(serde_json::to_string(&rows[3]).unwrap(),
            "{\"id\":1,\"law_id\":1,\"attempted_at\":\"2025-12-28 09:00:00\",\"attempted_by\":\"agent:dev\",\"agent_name\":\"dev\",\"action_name\":\"dev.file.write\",\"context_json\":\"{\\\"path\\\":\\\".git/HEAD\\\"}\",\"outcome\":\"blocked\",\"law_slug\":\"NEVER_BYPASS_WITH_AUDIT\",\"law_kind\":\"constitutional\",\"law_domain\":\"audit\"}");
        // law_id filter
        assert_eq!(lawViolationsList(&json!({"law_id": 1}), &c).unwrap().as_array().unwrap().iter().map(|r| r["id"].as_i64().unwrap()).collect::<Vec<_>>(), vec![2, 1]);
        // agent_name / action_name / outcome filters
        assert_eq!(lawViolationsList(&json!({"agent_name": "dev"}), &c).unwrap().as_array().unwrap().iter().map(|r| r["id"].as_i64().unwrap()).collect::<Vec<_>>(), vec![3, 1]);
        assert_eq!(lawViolationsList(&json!({"action_name": "qa.card.update"}), &c).unwrap()[0]["id"], json!(2));
        assert_eq!(lawViolationsList(&json!({"outcome": "escaped"}), &c).unwrap()[0]["id"], json!(3));
        // since: attempted_at >= (string compare)
        assert_eq!(lawViolationsList(&json!({"since": "2025-12-30 00:00:00"}), &c).unwrap().as_array().unwrap().iter().map(|r| r["id"].as_i64().unwrap()).collect::<Vec<_>>(), vec![4, 3]);
        // limit applies; combined filters compose
        assert_eq!(lawViolationsList(&json!({"limit": 2}), &c).unwrap().as_array().unwrap().iter().map(|r| r["id"].as_i64().unwrap()).collect::<Vec<_>>(), vec![4, 3]);
        assert_eq!(lawViolationsList(&json!({"agent_name": "dev", "outcome": "blocked", "limit": 1}), &c).unwrap().as_array().unwrap().iter().map(|r| r["id"].as_i64().unwrap()).collect::<Vec<_>>(), vec![1]);
        // non-positive / non-integer / non-number limits are IGNORED (JS Number.isInteger gate)
        assert_eq!(lawViolationsList(&json!({"limit": 0}), &c).unwrap().as_array().unwrap().len(), 4);
        assert_eq!(lawViolationsList(&json!({"limit": 2.5}), &c).unwrap().as_array().unwrap().len(), 4);
        assert_eq!(lawViolationsList(&json!({"limit": "2"}), &c).unwrap().as_array().unwrap().len(), 4);
        // falsy filters ignored; null input = JS `input ?? {}`
        assert_eq!(lawViolationsList(&json!({"law_id": null, "agent_name": "", "outcome": null}), &c).unwrap().as_array().unwrap().len(), 4);
        assert_eq!(lawViolationsList(&Value::Null, &c).unwrap().as_array().unwrap().len(), 4);
        // no match → empty array
        assert_eq!(lawViolationsList(&json!({"agent_name": "sec"}), &c).unwrap(), json!([]));
    }
}
