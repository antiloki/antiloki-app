//! design-snapshots-get.rs — twin of skills/codebase/design-snapshots-get.mjs (db lane).
//! NAMES ARE CONTRACT: `designSnapshotsGet` (named default). Snapshots + shared css + summary.
// QUALITY: 85/100 — two defensive reads; css_bytes uses UTF-16 length to match JS .length
use rusqlite::Connection;
use serde_json::{json, Value};

pub fn designSnapshotsGet(input: &Value, db: &Connection) -> Result<Value, String> {
    let origin = input.get("origin").and_then(|v| v.as_str()).unwrap_or("atlas");
    let snapshots = crate::db_json::query_rows(db,
        "SELECT kind, name, path, framework, html FROM code_design_snapshots WHERE origin = ? AND kind != 'stylesheet' ORDER BY kind DESC, name",
        &[json!(origin)]).unwrap_or_else(|_| json!([]));
    let style = crate::db_json::query_rows(db,
        "SELECT css, path FROM code_design_snapshots WHERE origin = ? AND kind = 'stylesheet' LIMIT 1",
        &[json!(origin)]).unwrap_or_else(|_| json!([]));
    let strow = style.as_array().unwrap().first();
    let css = strow.and_then(|r| r.get("css")).and_then(|v| v.as_str()).unwrap_or("").to_string();
    let base_url = strow.and_then(|r| r.get("path")).and_then(|v| v.as_str()).unwrap_or("").to_string();
    let ss = snapshots.as_array().unwrap();
    let components = ss.iter().filter(|s| s.get("kind").and_then(|v| v.as_str()) == Some("component")).count();
    let screens = ss.iter().filter(|s| s.get("kind").and_then(|v| v.as_str()) == Some("screen")).count();
    Ok(json!({
        "ok": true, "origin": origin, "css": css, "baseUrl": base_url, "snapshots": snapshots,
        "summary": { "components": components, "screens": screens, "css_bytes": css.encode_utf16().count() },
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn snapshots_css_and_summary() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE code_design_snapshots (origin TEXT, kind TEXT, name TEXT, path TEXT, framework TEXT, html TEXT, css TEXT);
            INSERT INTO code_design_snapshots (origin,kind,name,html) VALUES ('atlas','component','Btn','<b>'),('atlas','screen','Home','<h>');
            INSERT INTO code_design_snapshots (origin,kind,css,path) VALUES ('atlas','stylesheet','.x{}','/base');").unwrap();
        let r = designSnapshotsGet(&json!({}), &c).unwrap();
        assert_eq!(r["summary"]["components"], json!(1));
        assert_eq!(r["summary"]["screens"], json!(1));
        assert_eq!(r["css"], json!(".x{}"));
        assert_eq!(r["summary"]["css_bytes"], json!(4));
    }
}
