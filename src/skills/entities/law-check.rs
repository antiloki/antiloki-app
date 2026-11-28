//! law-check.rs — twin of skills/entities/law-check.mjs (db READ lane, cross-db laws.laws via ATTACH).
//! NAMES ARE CONTRACT: `lawCheck` (default). Candidate-list provider for action-run's pre-dispatch
//! permission check: every ACTIVE HARD law with an enforcement_hook set, optionally filtered by
//! kind/domain, ordered by id. It does NOT run the hooks — that's the runner's job.
use rusqlite::Connection;
use serde_json::Value;

fn truthy(v: Option<&Value>) -> bool { match v { None|Some(Value::Null)=>false, Some(Value::Bool(b))=>*b, Some(Value::Number(n))=>n.as_f64().map_or(false,|f|f!=0.0&&!f.is_nan()), Some(Value::String(s))=>!s.is_empty(), Some(_)=>true } }

pub fn lawCheck(input: &Value, db: &Connection) -> Result<Value, String> {
    let kind = input.get("kind");
    let domain = input.get("domain");

    let mut sql = String::from("\n    SELECT id, slug, enforcement_hook, kind, domain, statement\n    FROM laws.laws\n    WHERE superseded_by IS NULL\n      AND enforcement = 'hard'\n      AND enforcement_hook IS NOT NULL\n  ");
    let mut params: Vec<Value> = Vec::new();
    if truthy(kind) { sql.push_str(" AND kind = ?"); params.push(kind.unwrap().clone()); }
    if truthy(domain) { sql.push_str(" AND domain = ?"); params.push(domain.unwrap().clone()); }
    sql.push_str(" ORDER BY id");

    crate::db_json::query_rows(db, &sql, &params)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS laws; \
          CREATE TABLE laws.laws (id INTEGER PRIMARY KEY, slug TEXT, kind TEXT, domain TEXT, statement TEXT, enforcement TEXT, enforcement_hook TEXT, superseded_by INTEGER); \
          INSERT INTO laws.laws (id,slug,kind,domain,statement,enforcement,enforcement_hook,superseded_by) VALUES \
            (1,'NEVER_BYPASS_WITH_AUDIT','constitutional','audit','All mutations go through with-audit','hard','law-hook-mutation-via-with-audit',NULL), \
            (2,'GOD_MODE_TTY_REQUIRED','constitutional','security','GOD requires TTY','hard','auth-verify',NULL), \
            (3,'ENTITY_ID_STORED_AS_STRING','constraint','audit','entity_id is TEXT','hard',NULL,NULL), \
            (4,'ASK_BEFORE_DESTRUCTIVE','procedural','security','confirm destructive ops','soft','soft-hook',NULL), \
            (5,'OLD_HOOKED_LAW','constitutional','audit','superseded rule','hard','law-hook-old',1);").unwrap();
        c
    }
    #[test]
    fn active_hard_hooked_with_filters() {
        let c = db();
        // no filters: active hard hooked only (excludes hookless, soft, superseded), ordered by id
        let all = lawCheck(&json!({}), &c).unwrap();
        let rows = all.as_array().unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["slug"], json!("NEVER_BYPASS_WITH_AUDIT"));
        assert_eq!(rows[1]["slug"], json!("GOD_MODE_TTY_REQUIRED"));
        // column key order is part of the output contract (SELECT order)
        assert_eq!(serde_json::to_string(&rows[0]).unwrap(),
            "{\"id\":1,\"slug\":\"NEVER_BYPASS_WITH_AUDIT\",\"enforcement_hook\":\"law-hook-mutation-via-with-audit\",\"kind\":\"constitutional\",\"domain\":\"audit\",\"statement\":\"All mutations go through with-audit\"}");
        // kind filter
        assert_eq!(lawCheck(&json!({"kind":"constitutional"}), &c).unwrap().as_array().unwrap().len(), 2);
        // domain filter
        let audit = lawCheck(&json!({"domain":"audit"}), &c).unwrap();
        assert_eq!(audit.as_array().unwrap().len(), 1);
        assert_eq!(audit[0]["slug"], json!("NEVER_BYPASS_WITH_AUDIT"));
        // both filters
        let both = lawCheck(&json!({"kind":"constitutional","domain":"security"}), &c).unwrap();
        assert_eq!(both.as_array().unwrap().len(), 1);
        assert_eq!(both[0]["slug"], json!("GOD_MODE_TTY_REQUIRED"));
        // falsy filters (null / empty string) are ignored, like JS `if (kind)`
        assert_eq!(lawCheck(&json!({"kind":null,"domain":""}), &c).unwrap().as_array().unwrap().len(), 2);
        // null input = JS `input ?? {}`
        assert_eq!(lawCheck(&Value::Null, &c).unwrap().as_array().unwrap().len(), 2);
        // no match → empty array
        assert_eq!(lawCheck(&json!({"kind":"decision"}), &c).unwrap(), json!([]));
    }
}
