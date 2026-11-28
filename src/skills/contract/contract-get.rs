//! contract-get.rs — twin of skills/contract/contract-get.mjs (db lane). NAMES ARE CONTRACT:
//! `contractGet` (named default). SELECT * + parse 8 JSON columns; null when not found.
// QUALITY: 86/100 — the parseJsonCols spread: keep _json, add the stripped parsed key (or null)
use rusqlite::Connection;
use serde_json::{json, Value};

const JSON_COLS: [&str; 8] = [
    "files_touched_json", "acceptance_json", "naming_json", "edge_cases_json",
    "must_obey_json", "out_of_scope_json", "reference_artifacts_json", "generated_paths_json",
];

pub fn contractGet(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS: slug = typeof input === 'string' ? input : input?.slug
    let slug = match input.as_str() {
        Some(s) => s.to_string(),
        None => input.get("slug").and_then(|v| v.as_str()).unwrap_or("").to_string(),
    };
    if slug.is_empty() {
        return Err("contract-get: slug is required".into());
    }
    let rows = crate::db_json::query_rows(db, "SELECT * FROM contracts WHERE slug = ?", &[json!(slug)])?;
    let arr = rows.as_array().unwrap();
    if arr.is_empty() {
        return Ok(Value::Null);
    }
    let mut out = arr[0].as_object().unwrap().clone();
    for col in JSON_COLS {
        if let Some(Value::String(s)) = out.get(col).cloned() {
            let parsed = serde_json::from_str::<Value>(&s).unwrap_or(Value::Null);
            out.insert(col.strip_suffix("_json").unwrap().to_string(), parsed);
        }
    }
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_json_cols_or_null() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE contracts (slug TEXT, acceptance_json TEXT, naming_json TEXT);
            INSERT INTO contracts VALUES ('K','[{\"ac\":1}]','bad');").unwrap();
        let r = contractGet(&json!({ "slug": "K" }), &c).unwrap();
        assert_eq!(r["acceptance"], json!([{ "ac": 1 }]));
        assert_eq!(r["naming"], Value::Null); // invalid → null
        assert_eq!(r["acceptance_json"], json!("[{\"ac\":1}]")); // _json kept
        assert_eq!(contractGet(&json!({ "slug": "ghost" }), &c).unwrap(), Value::Null);
        assert!(contractGet(&json!({}), &c).is_err());
    }
}
