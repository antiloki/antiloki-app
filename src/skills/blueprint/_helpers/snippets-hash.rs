//! snippets-hash.rs — twin of skills/blueprint/_helpers/snippets-hash.mjs. NAMES ARE CONTRACT:
//! snippetsContentHash. FIX-V2-SNIPPET-FRESHNESS: content identity of a blueprint's authored
//! snippets. blueprint-judge stamps this hash onto the snippets judgment case; the driver state
//! machine compares it against the CURRENT snippets — a judgment is fresh iff it judged exactly
//! this code. Shared helper (not a skill): pure read, no DB writes.
use rusqlite::Connection;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

pub fn snippetsContentHash(db: &Connection, blueprint_id: &Value) -> Result<Value, String> {
    let rows = crate::db_json::query_rows(
        db,
        "SELECT block_id, code FROM blueprint_snippets WHERE blueprint_id = ? ORDER BY block_id",
        &[blueprint_id.clone()],
    )?;

    let pairs: Vec<Value> = rows
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|r| {
            let block_id = r.get("block_id").cloned().unwrap_or(Value::Null);
            let code = match r.get("code") {
                Some(Value::Null) | None => Value::String(String::new()), // code ?? ''
                Some(v) => v.clone(),
            };
            json!([block_id, code])
        })
        .collect();

    let serialized = serde_json::to_string(&Value::Array(pairs)).unwrap_or_else(|_| "[]".to_string());
    Ok(Value::String(hex::encode(Sha256::digest(serialized.as_bytes()))))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seeded(rows: &[(i64, &str, Option<&str>)]) -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE blueprint_snippets (id INTEGER PRIMARY KEY AUTOINCREMENT, blueprint_id INTEGER NOT NULL, block_id TEXT NOT NULL, code TEXT)",
        )
        .unwrap();
        for (bp, block, code) in rows {
            conn.execute(
                "INSERT INTO blueprint_snippets (blueprint_id, block_id, code) VALUES (?1, ?2, ?3)",
                rusqlite::params![bp, block, code],
            )
            .unwrap();
        }
        conn
    }

    #[test]
    fn deterministic_hash_over_ordered_pairs() {
        let conn = seeded(&[(1, "b2", Some("code-two")), (1, "b1", Some("code-one")), (2, "b1", Some("other-blueprint"))]);
        let hash = snippetsContentHash(&conn, &json!(1)).unwrap();
        let expected_json = serde_json::to_string(&json!([["b1", "code-one"], ["b2", "code-two"]])).unwrap();
        let expected = hex::encode(Sha256::digest(expected_json.as_bytes()));
        assert_eq!(hash, json!(expected));
    }

    #[test]
    fn stable_across_repeated_calls() {
        let conn = seeded(&[(5, "a", Some("x"))]);
        let h1 = snippetsContentHash(&conn, &json!(5)).unwrap();
        let h2 = snippetsContentHash(&conn, &json!(5)).unwrap();
        assert_eq!(h1, h2);
    }

    #[test]
    fn null_code_coerces_to_empty_string() {
        let conn_null = seeded(&[(3, "b1", None)]);
        let conn_empty = seeded(&[(3, "b1", Some(""))]);
        assert_eq!(snippetsContentHash(&conn_null, &json!(3)).unwrap(), snippetsContentHash(&conn_empty, &json!(3)).unwrap());
    }

    #[test]
    fn no_rows_produces_empty_array_hash() {
        let conn = seeded(&[]);
        let hash = snippetsContentHash(&conn, &json!(999)).unwrap();
        let expected = hex::encode(Sha256::digest(b"[]"));
        assert_eq!(hash, json!(expected));
    }

    #[test]
    fn different_code_changes_hash() {
        let conn_a = seeded(&[(1, "b1", Some("version-a"))]);
        let conn_b = seeded(&[(1, "b1", Some("version-b"))]);
        assert_ne!(snippetsContentHash(&conn_a, &json!(1)).unwrap(), snippetsContentHash(&conn_b, &json!(1)).unwrap());
    }

    #[test]
    fn missing_table_propagates_error() {
        let conn = Connection::open_in_memory().unwrap();
        let result = snippetsContentHash(&conn, &json!(1));
        assert!(result.is_err());
    }
}
