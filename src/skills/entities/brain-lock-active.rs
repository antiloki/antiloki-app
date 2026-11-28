//! brain-lock-active.rs — twin of skills/entities/brain-lock-active.mjs (db READ, attach lane).
//! NAMES ARE CONTRACT: `brainLockActive` (default). Lists currently-held brain.brain_locks rows;
//! optional truthy filters resource_type / resource_id / holder_session; newest acquired first.
//! Expired locks are still returned — this skill does not auto-expire.
use rusqlite::Connection;
use serde_json::Value;

fn truthy(v: Option<&Value>) -> bool { match v { None|Some(Value::Null)=>false, Some(Value::Bool(b))=>*b, Some(Value::Number(n))=>n.as_f64().map_or(false,|f|f!=0.0&&!f.is_nan()), Some(Value::String(s))=>!s.is_empty(), Some(_)=>true } }

pub fn brainLockActive(input: &Value, db: &Connection) -> Result<Value, String> {
    let mut sql = String::from("SELECT * FROM brain.brain_locks WHERE 1=1");
    let mut params: Vec<Value> = Vec::new();
    for key in ["resource_type", "resource_id", "holder_session"] {
        let v = input.get(key);
        if truthy(v) {
            sql.push_str(&format!(" AND {key} = ?"));
            params.push(v.unwrap().clone());
        }
    }
    sql.push_str(" ORDER BY acquired_at DESC");
    crate::db_json::query_rows(db, &sql, &params)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS brain; \
          CREATE TABLE brain.brain_locks ( resource_type TEXT NOT NULL, resource_id TEXT NOT NULL, state TEXT NOT NULL, holder_session TEXT NOT NULL, acquired_at TEXT NOT NULL, expires_at TEXT, reason TEXT, PRIMARY KEY (resource_type, resource_id), CHECK (resource_type IN ('card', 'agent')), CHECK (state IN ('driving','thinking','executing','waiting')) ); \
          INSERT INTO brain.brain_locks VALUES \
            ('card','A','driving','s1','2025-12-31 10:00:00',NULL,NULL), \
            ('agent','dev','thinking','s1','2025-12-31 11:00:00','2025-12-31 12:00:00','tick'), \
            ('card','B','driving','s2','2025-12-31 12:00:00',NULL,NULL);").unwrap();
        c
    }

    #[test]
    fn lists_all_newest_first() {
        let c = db();
        let r = brainLockActive(&json!({}), &c).unwrap();
        let rows = r.as_array().unwrap();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0]["resource_id"], json!("B"));
        assert_eq!(rows[2]["resource_id"], json!("A"));
        // SELECT * keeps table column order in each row object
        let keys: Vec<String> = rows[0].as_object().unwrap().keys().cloned().collect();
        assert_eq!(keys, vec!["resource_type", "resource_id", "state", "holder_session", "acquired_at", "expires_at", "reason"]);
    }

    #[test]
    fn filters_compose_and_falsy_is_ignored() {
        let c = db();
        assert_eq!(brainLockActive(&json!({"resource_type":"card"}), &c).unwrap().as_array().unwrap().len(), 2);
        assert_eq!(brainLockActive(&json!({"resource_id":"A"}), &c).unwrap().as_array().unwrap()[0]["resource_id"], json!("A"));
        assert_eq!(brainLockActive(&json!({"holder_session":"s1"}), &c).unwrap().as_array().unwrap().len(), 2);
        assert_eq!(brainLockActive(&json!({"resource_type":"card","holder_session":"s1"}), &c).unwrap().as_array().unwrap().len(), 1);
        // JS truthiness: null / "" filters are skipped, not matched
        assert_eq!(brainLockActive(&json!({"resource_type":null,"resource_id":""}), &c).unwrap().as_array().unwrap().len(), 3);
        assert_eq!(brainLockActive(&json!({"resource_type":"agent","resource_id":"nope"}), &c).unwrap(), json!([]));
    }
}
