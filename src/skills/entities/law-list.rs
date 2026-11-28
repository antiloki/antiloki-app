//! law-list.rs — twin of skills/entities/law-list.mjs (db READ lane, cross-db laws via ATTACH).
//! NAMES ARE CONTRACT: `lawList` (default). Filtered law read: active by default
//! (superseded_by IS NULL), kind/domain/enforcement/slug filters AND-composed,
//! ORDER BY kind, domain, slug; optional per-law clauses embed via withClauses.
// QUALITY: 80/100 — SQL-building logic + per-law clauses append (key lands AFTER the SELECT * columns)
use rusqlite::Connection;
use serde_json::Value;

fn truthy(v: Option<&Value>) -> bool { match v { None|Some(Value::Null)=>false, Some(Value::Bool(b))=>*b, Some(Value::Number(n))=>n.as_f64().map_or(false,|f|f!=0.0&&!f.is_nan()), Some(Value::String(s))=>!s.is_empty(), Some(_)=>true } }

pub fn lawList(input: &Value, db: &Connection) -> Result<Value, String> {
    let kind = input.get("kind");
    let domain = input.get("domain");
    let enforcement = input.get("enforcement");
    let slug = input.get("slug");

    let mut sql = String::from("SELECT * FROM laws.laws WHERE 1=1");
    let mut params: Vec<Value> = Vec::new();
    if !truthy(input.get("includeSuperseded")) { sql.push_str(" AND superseded_by IS NULL"); }
    if truthy(kind) { sql.push_str(" AND kind = ?"); params.push(kind.unwrap().clone()); }
    if truthy(domain) { sql.push_str(" AND domain = ?"); params.push(domain.unwrap().clone()); }
    if truthy(enforcement) { sql.push_str(" AND enforcement = ?"); params.push(enforcement.unwrap().clone()); }
    if truthy(slug) { sql.push_str(" AND slug = ?"); params.push(slug.unwrap().clone()); }
    sql.push_str(" ORDER BY kind, domain, slug");
    // JS: if (limit && Number.isInteger(limit) && limit > 0) — a finite integral positive NUMBER
    if let Some(l) = input.get("limit") {
        if let Some(f) = l.as_f64() {
            if f.is_finite() && f.fract() == 0.0 && f > 0.0 { sql.push_str(" LIMIT ?"); params.push(l.clone()); }
        }
    }

    let mut laws = crate::db_json::query_rows(db, &sql, &params)?;

    // JS: if (withClauses && laws.length > 0) — embed clauses per law; `law.clauses = ...`
    // is a NEW key on the row object, so it appends after all SELECT * columns.
    if truthy(input.get("withClauses")) && laws.as_array().map_or(false, |a| !a.is_empty()) {
        if let Some(arr) = laws.as_array_mut() {
            for law in arr.iter_mut() {
                let id = law.get("id").cloned().unwrap_or(Value::Null);
                let clauses = crate::db_json::query_rows(
                    db,
                    "SELECT clause_type, text, order_index FROM laws.law_clauses WHERE law_id = ? ORDER BY order_index",
                    &[id],
                )?;
                if let Some(o) = law.as_object_mut() { o.insert("clauses".into(), clauses); }
            }
        }
    }

    Ok(laws)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS laws; \
          CREATE TABLE laws.laws (id INTEGER PRIMARY KEY, slug TEXT, kind TEXT, domain TEXT, title TEXT, statement TEXT, rationale TEXT, enforcement TEXT, enforcement_hook TEXT, trigger_sql TEXT, established_at TEXT, established_by TEXT, source_card TEXT, source_event INTEGER, source_memory INTEGER, superseded_by INTEGER, superseded_at TEXT); \
          CREATE TABLE laws.law_clauses (id INTEGER PRIMARY KEY, law_id INTEGER, clause_type TEXT, text TEXT, order_index INTEGER, created_at TEXT); \
          INSERT INTO laws.laws (id,slug,kind,domain,title,enforcement,established_at,established_by) VALUES \
            (1,'NEVER_BYPASS_WITH_AUDIT','constitutional','audit','All mutations go through with-audit','hard','2025-12-01 09:00:00','human:dani'), \
            (2,'ASK_BEFORE_DESTRUCTIVE','procedural','security','Confirm before destructive ops','soft','2025-12-02 09:00:00','system:bootstrap'), \
            (3,'MINIMUM_VIABLE_CHANGE','principle','dev','Least code that solves the problem','soft','2025-12-03 09:00:00','system:bootstrap'), \
            (4,'READ_BEFORE_WRITING','procedural','dev','Read the file before editing','soft','2025-12-04 09:00:00','system:bootstrap'), \
            (6,'MULTI_DB_SPLIT','decision','architecture','Multi DB split by quadrant','soft','2025-12-06 09:00:00','human:dani'); \
          INSERT INTO laws.laws (id,slug,kind,domain,title,enforcement,established_at,established_by,superseded_by,superseded_at) VALUES \
            (5,'OLD_FOUR_DB_SPLIT','decision','architecture','Four DB split','soft','2025-12-05 09:00:00','human:dani',6,'2025-12-30 10:00:00'); \
          INSERT INTO laws.law_clauses (id,law_id,clause_type,text,order_index) VALUES \
            (1,1,'example','call card-update which composes with-audit',1), \
            (2,1,'antipattern','raw UPDATE against cards from a skill',0), \
            (3,2,'exception','human explicitly confirms in the current turn',0), \
            (4,6,'consequence','cross-db FKs impossible; use TEXT slug refs',0);").unwrap();
        c
    }
    fn ids(v: &Value) -> Vec<i64> { v.as_array().unwrap().iter().map(|r| r["id"].as_i64().unwrap()).collect() }

    #[test]
    fn filters_order_superseded() {
        let c = db();
        // superseded hidden by default; ORDER BY kind, domain, slug
        assert_eq!(ids(&lawList(&json!({}), &c).unwrap()), vec![1, 6, 3, 4, 2]);
        // includeSuperseded shows history; ties on (kind, domain) fall to slug (MULTI < OLD_FOUR)
        assert_eq!(ids(&lawList(&json!({"includeSuperseded": true}), &c).unwrap()), vec![1, 6, 5, 3, 4, 2]);
        // per-column filters
        assert_eq!(ids(&lawList(&json!({"kind": "procedural"}), &c).unwrap()), vec![4, 2]);
        assert_eq!(ids(&lawList(&json!({"domain": "dev"}), &c).unwrap()), vec![3, 4]);
        assert_eq!(ids(&lawList(&json!({"enforcement": "hard"}), &c).unwrap()), vec![1]);
        assert_eq!(ids(&lawList(&json!({"slug": "ASK_BEFORE_DESTRUCTIVE"}), &c).unwrap()), vec![2]);
        // slug of a superseded law: hidden unless includeSuperseded
        assert_eq!(lawList(&json!({"slug": "OLD_FOUR_DB_SPLIT"}), &c).unwrap(), json!([]));
        assert_eq!(ids(&lawList(&json!({"slug": "OLD_FOUR_DB_SPLIT", "includeSuperseded": true}), &c).unwrap()), vec![5]);
        // filters compose with AND
        assert_eq!(ids(&lawList(&json!({"kind": "procedural", "domain": "dev"}), &c).unwrap()), vec![4]);
        // falsy filters ignored; null input = JS `input ?? {}`
        assert_eq!(lawList(&json!({"kind": null, "domain": "", "enforcement": null}), &c).unwrap().as_array().unwrap().len(), 5);
        assert_eq!(lawList(&Value::Null, &c).unwrap().as_array().unwrap().len(), 5);
        // column key order is part of the output contract (table column order, unlisted cols NULL)
        assert_eq!(serde_json::to_string(&lawList(&json!({"enforcement": "hard"}), &c).unwrap()[0]).unwrap(),
            "{\"id\":1,\"slug\":\"NEVER_BYPASS_WITH_AUDIT\",\"kind\":\"constitutional\",\"domain\":\"audit\",\"title\":\"All mutations go through with-audit\",\"statement\":null,\"rationale\":null,\"enforcement\":\"hard\",\"enforcement_hook\":null,\"trigger_sql\":null,\"established_at\":\"2025-12-01 09:00:00\",\"established_by\":\"human:dani\",\"source_card\":null,\"source_event\":null,\"source_memory\":null,\"superseded_by\":null,\"superseded_at\":null}");
    }

    #[test]
    fn with_clauses_and_limit() {
        let c = db();
        let wc = lawList(&json!({"withClauses": true}), &c).unwrap();
        let rows = wc.as_array().unwrap();
        // clauses ordered by order_index (seeded out of order), only the 3 selected columns
        assert_eq!(rows[0]["clauses"], json!([
            {"clause_type": "antipattern", "text": "raw UPDATE against cards from a skill", "order_index": 0},
            {"clause_type": "example", "text": "call card-update which composes with-audit", "order_index": 1}]));
        assert_eq!(rows[1]["clauses"], json!([
            {"clause_type": "consequence", "text": "cross-db FKs impossible; use TEXT slug refs", "order_index": 0}]));
        // a law with no clauses gets an EMPTY array (not a missing key)
        assert_eq!(rows[2]["clauses"], json!([]));
        // the clauses key is APPENDED after the SELECT * columns
        assert!(serde_json::to_string(&rows[2]).unwrap().ends_with("\"clauses\":[]}"));
        // without withClauses, no clauses key at all
        assert!(lawList(&json!({}), &c).unwrap()[0].get("clauses").is_none());
        // withClauses with ZERO matching laws: clause query skipped, plain empty array
        assert_eq!(lawList(&json!({"withClauses": true, "slug": "NO_SUCH_LAW"}), &c).unwrap(), json!([]));
        // limit applies only for positive integers (JS Number.isInteger gate)
        assert_eq!(ids(&lawList(&json!({"limit": 2}), &c).unwrap()), vec![1, 6]);
        assert_eq!(ids(&lawList(&json!({"kind": "procedural", "domain": "dev", "limit": 1}), &c).unwrap()), vec![4]);
        assert_eq!(lawList(&json!({"limit": 0}), &c).unwrap().as_array().unwrap().len(), 5);
        assert_eq!(lawList(&json!({"limit": 2.5}), &c).unwrap().as_array().unwrap().len(), 5);
        assert_eq!(lawList(&json!({"limit": "2"}), &c).unwrap().as_array().unwrap().len(), 5);
        // everything composes: history + clauses + limit
        assert_eq!(ids(&lawList(&json!({"includeSuperseded": true, "withClauses": true, "limit": 3}), &c).unwrap()), vec![1, 6, 5]);
    }
}
