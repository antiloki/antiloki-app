//! code-digest-latest.rs — twin of skills/codebase/code-digest-latest.mjs (db READ lane). NAMES ARE
//! CONTRACT: `codeDigestLatest` (default). Reflect the newest code_digests row into render sections.
// QUALITY: 78/100 — SELECT latest row → digest-spec sectionsFromRow + groupsOf (cross-module), no recompute
use rusqlite::Connection;
use serde_json::{json, Value};

pub fn codeDigestLatest(input: &Value, db: &Connection) -> Result<Value, String> {
    let origin = input.get("origin").and_then(|v| v.as_str()).unwrap_or("atlas").to_string();
    let rows = match crate::db_json::query_rows(db, "SELECT * FROM code_digests WHERE origin = ? ORDER BY id DESC LIMIT 1", &[json!(origin)]) {
        Ok(r) => r.as_array().cloned().unwrap_or_default(),
        Err(_) => return Ok(json!({ "ok": true, "found": false, "origin": origin, "sections": [], "groups": [], "error": "code_digests not present — restart the BE, then rebuild" })),
    };
    let row = match rows.into_iter().next() {
        Some(r) => r,
        None => return Ok(json!({ "ok": true, "found": false, "origin": origin, "sections": [], "groups": [] })),
    };
    let sections = crate::codebase_digest_spec::sectionsFromRow(&row);
    let groups = crate::codebase_digest_spec::groupsOf(&sections);
    Ok(json!({ "ok": true, "found": true, "digest_id": row.get("id").cloned().unwrap_or(Value::Null), "created_at": row.get("created_at").cloned().unwrap_or(Value::Null), "origin": origin, "sections": sections, "groups": groups }))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn latest() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE code_digests (id INTEGER PRIMARY KEY, origin TEXT, created_at TEXT, total_files_num REAL, languages_json TEXT, narrative_md TEXT); \
          INSERT INTO code_digests (origin, created_at, total_files_num, languages_json, narrative_md) VALUES ('atlas','t1', 10, '{\"ts\":5}', 'the arch'); \
          INSERT INTO code_digests (origin, created_at, total_files_num, languages_json, narrative_md) VALUES ('atlas','t2', 20, '{\"ts\":9}', 'newer');").unwrap();
        let r = codeDigestLatest(&json!({ "origin": "atlas" }), &c).unwrap();
        assert_eq!(r["found"], json!(true));
        assert_eq!(r["digest_id"], json!(2)); // newest by id
        assert_eq!(r["created_at"], json!("t2"));
        assert!(r["sections"].as_array().unwrap().iter().any(|s| s["col"] == json!("total_files_num") && s["value"] == json!(20.0)));
        assert!(r["groups"].as_array().unwrap().contains(&json!("Overview")));
        // absent origin → not found
        assert_eq!(codeDigestLatest(&json!({ "origin": "other" }), &c).unwrap()["found"], json!(false));
    }
}
