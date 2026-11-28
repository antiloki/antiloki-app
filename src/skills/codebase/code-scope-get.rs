//! code-scope-get.rs — twin of skills/codebase/code-scope-get.mjs (db lane). NAMES ARE CONTRACT:
//! `codeScopeGet` (default) + `loadScope` (named, db-first helper). Reads the per-workspace atlas
//! SCOPE config (exclude/include globs); degrades to defaults ([],[]) when the table/row is absent.
use rusqlite::Connection;
use serde_json::{json, Map, Value};

// db-first helper: loadScope(db, origin='atlas') -> {exclude, include}. Table-absent / bad-JSON
// both fall through to the {exclude:[],include:[]} default, mirroring the JS try/catch scope.
pub fn loadScope(db: &Connection, origin: &Value) -> Value {
    let origin_s = origin.as_str().unwrap_or("atlas").to_string();
    let row: rusqlite::Result<(Option<String>, Option<String>)> = db.query_row(
        "SELECT exclude_json, include_json FROM code_scope WHERE origin = ?",
        rusqlite::params![origin_s],
        |r| Ok((r.get(0)?, r.get(1)?)),
    );
    if let Ok((exclude_json, include_json)) = row {
        let exclude_str = exclude_json.unwrap_or_else(|| "[]".to_string());
        let include_str = include_json.unwrap_or_else(|| "[]".to_string());
        if let (Ok(exclude), Ok(include)) = (
            serde_json::from_str::<Value>(&exclude_str),
            serde_json::from_str::<Value>(&include_str),
        ) {
            return json!({ "exclude": exclude, "include": include });
        }
    }
    json!({ "exclude": [], "include": [] })
}

pub fn codeScopeGet(input: &Value, db: &Connection) -> Result<Value, String> {
    let origin = input
        .get("origin")
        .filter(|v| !v.is_null())
        .cloned()
        .unwrap_or(json!("atlas"));
    let scope = loadScope(db, &origin);

    let mut out = Map::new();
    out.insert("origin".into(), origin);
    out.insert("exclude".into(), scope["exclude"].clone());
    out.insert("include".into(), scope["include"].clone());
    out.insert(
        "supported_exts".into(),
        crate::codebase_ast_parse::supportedExtensions(),
    );
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_saved_scope() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE code_scope (origin TEXT, exclude_json TEXT, include_json TEXT); \
             INSERT INTO code_scope VALUES ('atlas', '[\"node_modules\"]', '[\"src\"]');",
        )
        .unwrap();
        let r = codeScopeGet(&json!({}), &c).unwrap();
        assert_eq!(r["origin"], json!("atlas"));
        assert_eq!(r["exclude"], json!(["node_modules"]));
        assert_eq!(r["include"], json!(["src"]));
        assert!(r["supported_exts"].as_array().unwrap().contains(&json!(".rs")));
    }

    #[test]
    fn defaults_when_no_row() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE code_scope (origin TEXT, exclude_json TEXT, include_json TEXT);")
            .unwrap();
        let r = codeScopeGet(&json!({ "origin": "other" }), &c).unwrap();
        assert_eq!(r["origin"], json!("other"));
        assert_eq!(r["exclude"], json!([]));
        assert_eq!(r["include"], json!([]));
    }

    #[test]
    fn defaults_when_table_absent() {
        let c = Connection::open_in_memory().unwrap();
        let r = codeScopeGet(&json!({}), &c).unwrap();
        assert_eq!(r["exclude"], json!([]));
        assert_eq!(r["include"], json!([]));
    }
}
