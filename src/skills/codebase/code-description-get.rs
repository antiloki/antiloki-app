//! code-description-get.rs — twin of skills/codebase/code-description-get.mjs (db lane).
//! NAMES ARE CONTRACT: `codeDescriptionGet` (named default). Two reads + a stale flag.
// QUALITY: 85/100 — facts parse + content_hash staleness compare across two tables
use rusqlite::Connection;
use serde_json::{json, Value};

pub fn codeDescriptionGet(input: &Value, db: &Connection) -> Result<Value, String> {
    let path = match input.get("path").and_then(|v| v.as_str()).filter(|s| !s.is_empty()) {
        Some(p) => p,
        None => return Err("code-description-get: path is required".into()),
    };
    let origin = input.get("origin").and_then(|v| v.as_str()).unwrap_or("atlas");
    let rows = crate::db_json::query_rows(db,
        "SELECT facts_json, summary_md, content_hash, model_id, generated_at FROM code_descriptions WHERE origin = ? AND path = ?",
        &[json!(origin), json!(path)])?;
    let arr = rows.as_array().unwrap();
    if arr.is_empty() {
        return Ok(json!({ "path": path, "origin": origin, "facts": null, "summary_md": null,
            "has_essay": false, "stale": false, "model_id": null, "generated_at": null }));
    }
    let row = arr[0].as_object().unwrap();
    let facts = row.get("facts_json").and_then(|v| v.as_str())
        .and_then(|s| serde_json::from_str::<Value>(s).ok()).unwrap_or(Value::Null);
    let cur = crate::db_json::query_rows(db,
        "SELECT content_hash FROM code_nodes WHERE origin = ? AND path = ? AND type = 'file' LIMIT 1",
        &[json!(origin), json!(path)])?;
    let cur_hash = cur.as_array().unwrap().first().and_then(|r| r.get("content_hash")).cloned().unwrap_or(Value::Null);
    let summary_md = row.get("summary_md").cloned().unwrap_or(Value::Null);
    let has_essay = summary_md.as_str().map_or(false, |s| !s.is_empty()); // JS !!row.summary_md
    let row_hash = row.get("content_hash").cloned().unwrap_or(Value::Null);
    let stale = !cur_hash.is_null() && row_hash != cur_hash;
    Ok(json!({
        "path": path, "origin": origin, "facts": facts, "summary_md": summary_md,
        "has_essay": has_essay, "stale": stale,
        "model_id": row.get("model_id").cloned().unwrap_or(Value::Null),
        "generated_at": row.get("generated_at").cloned().unwrap_or(Value::Null),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn found_stale_and_missing() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE code_descriptions (origin TEXT, path TEXT, facts_json TEXT, summary_md TEXT, content_hash TEXT, model_id TEXT, generated_at TEXT);
            CREATE TABLE code_nodes (origin TEXT, path TEXT, type TEXT, content_hash TEXT);
            INSERT INTO code_descriptions VALUES ('atlas','a.js','{\"loc\":10}','essay','h1','m','t');
            INSERT INTO code_nodes VALUES ('atlas','a.js','file','h2');").unwrap();
        let r = codeDescriptionGet(&json!({ "path": "a.js" }), &c).unwrap();
        assert_eq!(r["facts"], json!({ "loc": 10 }));
        assert_eq!(r["has_essay"], json!(true));
        assert_eq!(r["stale"], json!(true)); // h1 != h2
        assert_eq!(codeDescriptionGet(&json!({ "path": "gone.js" }), &c).unwrap()["facts"], Value::Null);
    }
}
