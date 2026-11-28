//! codebase-parse-health.rs — twin of skills/codebase/codebase-parse-health.mjs (db lane).
//! NAMES ARE CONTRACT: `codebaseParseHealth` (named default). Files + status tallies.
// QUALITY: 86/100 — query + count + status filters; clamped clean = total - listed
use rusqlite::Connection;
use serde_json::{json, Value};

pub fn codebaseParseHealth(input: &Value, db: &Connection) -> Result<Value, String> {
    let origin = input.get("origin").and_then(|v| v.as_str()).unwrap_or("atlas");
    let limit = input.get("limit").and_then(|v| v.as_i64()).filter(|&n| n > 0).unwrap_or(500);
    let files = crate::db_json::query_rows(db,
        "SELECT path, lang, status, detail FROM code_parse_health WHERE origin = ? ORDER BY CASE status WHEN 'fail' THEN 0 WHEN 'degraded' THEN 1 ELSE 2 END, path LIMIT ?",
        &[json!(origin), json!(limit)]).unwrap_or_else(|_| json!([]));
    let farr = files.as_array().unwrap();
    let total_files = crate::db_json::query_rows(db,
        "SELECT COUNT(*) AS n FROM code_nodes WHERE origin = ? AND type = 'file'", &[json!(origin)])
        .ok().and_then(|r| r.as_array().unwrap().first().and_then(|x| x.get("n")).and_then(|v| v.as_i64())).unwrap_or(0);
    let count = |st: &str| farr.iter().filter(|f| f.get("status").and_then(|v| v.as_str()) == Some(st)).count() as i64;
    Ok(json!({
        "origin": origin,
        "total_files": total_files,
        "clean": (total_files - farr.len() as i64).max(0),
        "fail": count("fail"), "degraded": count("degraded"), "skipped": count("skipped"),
        "files": files,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn tallies_and_clean() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE code_parse_health (origin TEXT, path TEXT, lang TEXT, status TEXT, detail TEXT);
            CREATE TABLE code_nodes (origin TEXT, type TEXT);
            INSERT INTO code_parse_health VALUES ('atlas','a','js','fail',NULL),('atlas','b','js','degraded',NULL);
            INSERT INTO code_nodes VALUES ('atlas','file'),('atlas','file'),('atlas','file');").unwrap();
        let r = codebaseParseHealth(&json!({}), &c).unwrap();
        assert_eq!(r["total_files"], json!(3));
        assert_eq!(r["fail"], json!(1));
        assert_eq!(r["clean"], json!(1)); // 3 - 2 listed
    }
}
