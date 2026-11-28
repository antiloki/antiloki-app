//! driver-failures-list.rs — twin of skills/driver/driver-failures-list.mjs (db READ, attach lane).
//! NAMES ARE CONTRACT: `driverFailuresList` (default). Lists MAN's failure diagnoses from
//! brain.driver_diagnoses (one row per signature), pending (classification IS NULL) first,
//! then recurrences DESC, then updated_at DESC. Returns { diagnoses, count, pending }.
use rusqlite::Connection;
use serde_json::{json, Map, Value};

fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) } // JS `??` right operand
fn js_number(v: Option<&Value>) -> f64 { match v { None=>f64::NAN, Some(Value::Null)=>0.0, Some(Value::Bool(b))=>if *b {1.0} else {0.0}, Some(Value::Number(n))=>n.as_f64().unwrap_or(f64::NAN), Some(Value::String(s))=>{let t=s.trim(); if t.is_empty(){0.0}else{t.parse().unwrap_or(f64::NAN)}}, Some(_)=>f64::NAN } } // = JS Number(v)

pub fn driverFailuresList(input: &Value, db: &Connection) -> Result<Value, String> {
    // limit = Math.min(Math.max(Number(input?.limit ?? 50), 1), 200)
    let raw = match nz(input.get("limit")) { Some(v) => js_number(Some(v)), None => 50.0 };
    let limit = raw.max(1.0).min(200.0);
    let only_pending = input.get("only_pending") == Some(&Value::Bool(true)); // strict === true

    let sql = format!(
        "\n    SELECT id, signature, classification, recurrences, confidence, card_slug, tick_id,\n           root_cause_md, suggested_fix_md, model_id, created_at, updated_at\n    FROM brain.driver_diagnoses\n    {}\n    ORDER BY (classification IS NULL) DESC, recurrences DESC, updated_at DESC\n    LIMIT ?\n  ",
        if only_pending { "WHERE classification IS NULL" } else { "" }
    );
    // node:sqlite binds integer-valued JS numbers as INTEGER
    let limit_param = if limit.fract() == 0.0 { json!(limit as i64) } else { json!(limit) };
    let rows = crate::db_json::query_rows(db, &sql, &[limit_param])?;

    let pending = crate::db_json::query_rows(db, "SELECT COUNT(*) AS n FROM brain.driver_diagnoses WHERE classification IS NULL", &[])?
        .as_array().and_then(|a| a.first()).and_then(|r| r.get("n")).cloned().unwrap_or(Value::Null);

    let count = rows.as_array().map(|a| a.len()).unwrap_or(0);
    let mut out = Map::new();
    out.insert("diagnoses".into(), rows);
    out.insert("count".into(), json!(count));
    out.insert("pending".into(), pending);
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS brain; \
          CREATE TABLE brain.driver_diagnoses ( id INTEGER PRIMARY KEY AUTOINCREMENT, signature TEXT NOT NULL UNIQUE, tick_id INTEGER, card_slug TEXT, classification TEXT, root_cause_md TEXT, suggested_fix_md TEXT, confidence REAL, gather_json TEXT, recurrences INTEGER NOT NULL DEFAULT 1, model_id TEXT, created_at TEXT NOT NULL, updated_at TEXT NOT NULL ); \
          INSERT INTO brain.driver_diagnoses (signature, tick_id, card_slug, classification, root_cause_md, suggested_fix_md, confidence, gather_json, recurrences, model_id, created_at, updated_at) VALUES \
            ('a :: x', 11, 'CARD-A', 'card-bug',  'rc-a', 'fix-a', 0.85, NULL, 5, 'm1', '2025-12-27 08:00:00', '2025-12-30 10:00:00'), \
            ('b :: y', 12, 'CARD-B', NULL,        NULL,   NULL,    NULL, NULL, 1, NULL, '2025-12-29 09:00:00', '2025-12-29 09:00:00'), \
            ('c :: z', 13, 'CARD-C', 'config',    'rc-c', 'fix-c', 0.6,  NULL, 2, 'm1', '2025-12-28 07:00:00', '2025-12-30 11:00:00'), \
            ('d :: w', 14, NULL,     NULL,        NULL,   NULL,    NULL, NULL, 3, NULL, '2025-12-28 08:00:00', '2025-12-28 08:00:00'), \
            ('e :: v', 15, 'CARD-E', 'transient', 'rc-e', 'fix-e', 0.7,  NULL, 2, 'm2', '2025-12-28 09:00:00', '2025-12-30 12:00:00');").unwrap();
        c
    }
    fn sigs(r: &Value) -> Vec<String> {
        r["diagnoses"].as_array().unwrap().iter().map(|d| d["signature"].as_str().unwrap().to_string()).collect()
    }

    #[test]
    fn lists_pending_first_then_recurrences_then_updated_at() {
        let c = db();
        let r = driverFailuresList(&json!({}), &c).unwrap();
        assert_eq!(r["count"], json!(5));
        assert_eq!(r["pending"], json!(2));
        // pending on top (higher recurrences first), then classified by recurrences DESC,
        // recurrences tie (c vs e, both 2) broken by updated_at DESC
        assert_eq!(sigs(&r), vec!["d :: w", "b :: y", "a :: x", "e :: v", "c :: z"]);
        // row objects keep SELECT column order
        let keys: Vec<String> = r["diagnoses"][0].as_object().unwrap().keys().cloned().collect();
        assert_eq!(keys, vec!["id", "signature", "classification", "recurrences", "confidence", "card_slug", "tick_id", "root_cause_md", "suggested_fix_md", "model_id", "created_at", "updated_at"]);
    }

    #[test]
    fn only_pending_filters_and_clamps() {
        let c = db();
        let p = driverFailuresList(&json!({ "only_pending": true }), &c).unwrap();
        assert_eq!(p["count"], json!(2));
        assert_eq!(sigs(&p), vec!["d :: w", "b :: y"]);
        // only_pending must be strictly true — 1 is not a filter
        assert_eq!(driverFailuresList(&json!({ "only_pending": 1 }), &c).unwrap()["count"], json!(5));
        // limit: clamped low (0 → 1), passthrough (2), string coercion ("3" → 3)
        assert_eq!(driverFailuresList(&json!({ "limit": 0 }), &c).unwrap()["count"], json!(1));
        assert_eq!(sigs(&driverFailuresList(&json!({ "limit": 2 }), &c).unwrap()), vec!["d :: w", "b :: y"]);
        assert_eq!(driverFailuresList(&json!({ "limit": "3" }), &c).unwrap()["count"], json!(3));
        // null → default 50; huge → clamped to 200 (all 5 rows either way)
        assert_eq!(driverFailuresList(&json!({ "limit": null }), &c).unwrap()["count"], json!(5));
        assert_eq!(driverFailuresList(&json!({ "limit": 999 }), &c).unwrap()["count"], json!(5));
        // pending count ignores limit
        assert_eq!(driverFailuresList(&json!({ "limit": 1 }), &c).unwrap()["pending"], json!(2));
    }

    #[test]
    fn empty_table() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS brain; \
          CREATE TABLE brain.driver_diagnoses ( id INTEGER PRIMARY KEY AUTOINCREMENT, signature TEXT NOT NULL UNIQUE, tick_id INTEGER, card_slug TEXT, classification TEXT, root_cause_md TEXT, suggested_fix_md TEXT, confidence REAL, gather_json TEXT, recurrences INTEGER NOT NULL DEFAULT 1, model_id TEXT, created_at TEXT NOT NULL, updated_at TEXT NOT NULL );").unwrap();
        let r = driverFailuresList(&json!({}), &c).unwrap();
        assert_eq!(r, json!({ "diagnoses": [], "count": 0, "pending": 0 }));
    }
}
