//! atlas-file-snapshot-get.rs — twin of skills/codebase/atlas-file-snapshot-get.mjs (db READ, attach lane).
//! NAMES ARE CONTRACT: `atlasFileSnapshotGet` (default). Latest persisted file snapshot for a path;
//! `stale` = stored content_hash vs current atlas file node hash; { snapshot: null } when none yet.
use rusqlite::Connection;
use serde_json::{json, Map, Value};

fn truthy(v: Option<&Value>) -> bool { match v { None|Some(Value::Null)=>false, Some(Value::Bool(b))=>*b, Some(Value::Number(n))=>n.as_f64().map_or(false,|f|f!=0.0&&!f.is_nan()), Some(Value::String(s))=>!s.is_empty(), Some(_)=>true } }
// JS safe(j) = try { JSON.parse(j ?? 'null') } catch { null } — TEXT column: string or null.
fn safe(v: Option<&Value>) -> Value {
    match v {
        Some(Value::String(s)) => serde_json::from_str::<Value>(s).unwrap_or(Value::Null),
        _ => Value::Null,
    }
}

pub fn atlasFileSnapshotGet(input: &Value, db: &Connection) -> Result<Value, String> {
    let origin = input.get("origin").filter(|v| !v.is_null()).cloned().unwrap_or(json!("atlas"));
    let path = input.get("path").cloned().unwrap_or(Value::Null);
    if !truthy(Some(&path)) { return Err("atlas-file-snapshot-get: path is required".into()); }

    let rows = crate::db_json::query_rows(db,
        "SELECT id, content_hash, snapshot_json, model_id, created_at FROM code.code_file_snapshots WHERE origin=? AND path=? ORDER BY id DESC LIMIT 1",
        &[origin.clone(), path.clone()])?;
    let arr = rows.as_array().unwrap();
    if arr.is_empty() {
        let mut out = Map::new();
        out.insert("path".into(), path);
        out.insert("snapshot".into(), Value::Null);
        return Ok(Value::Object(out));
    }
    let row = arr[0].as_object().unwrap();

    let curq = crate::db_json::query_rows(db,
        "SELECT content_hash FROM code.code_nodes WHERE origin=? AND path=? AND type='file' LIMIT 1",
        &[origin, path.clone()])?;
    let cur = curq.as_array().unwrap().first()
        .and_then(|r| r.get("content_hash")).filter(|v| !v.is_null()).cloned().unwrap_or(Value::Null);
    let row_hash = row.get("content_hash").cloned().unwrap_or(Value::Null);
    let stale = truthy(Some(&cur)) && truthy(Some(&row_hash)) && cur != row_hash;

    let mut out = Map::new();
    out.insert("path".into(), path);
    out.insert("snapshot_id".into(), row.get("id").cloned().unwrap_or(Value::Null));
    out.insert("snapshot".into(), safe(row.get("snapshot_json")));
    out.insert("model_id".into(), row.get("model_id").cloned().unwrap_or(Value::Null));
    out.insert("created_at".into(), row.get("created_at").cloned().unwrap_or(Value::Null));
    out.insert("stale".into(), json!(stale));
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS code; \
          CREATE TABLE code.code_file_snapshots ( id INTEGER PRIMARY KEY AUTOINCREMENT, origin TEXT NOT NULL, path TEXT NOT NULL, content_hash TEXT, snapshot_json TEXT NOT NULL, model_id TEXT, created_at TEXT NOT NULL ); \
          CREATE TABLE code.code_nodes ( id TEXT PRIMARY KEY, origin TEXT NOT NULL, path TEXT NOT NULL, type TEXT NOT NULL, content_hash TEXT );").unwrap();
        c
    }

    #[test]
    fn none_yet_returns_null_snapshot() {
        let c = db();
        let r = atlasFileSnapshotGet(&json!({ "path": "src/a.ts" }), &c).unwrap();
        assert_eq!(r, json!({ "path": "src/a.ts", "snapshot": null }));
    }

    #[test]
    fn latest_row_wins_and_stale_flags() {
        let c = db();
        c.execute_batch("INSERT INTO code.code_file_snapshots (id,origin,path,content_hash,snapshot_json,model_id,created_at) VALUES \
            (1,'atlas','src/a.ts','h-old','{\"v\":1}','m1','2025-12-30 10:00:00'), \
            (2,'atlas','src/a.ts','h-new','{\"v\":2}','m2','2025-12-31 10:00:00'); \
          INSERT INTO code.code_nodes (id,origin,path,type,content_hash) VALUES \
            ('src/a.ts@atlas','atlas','src/a.ts','file','h-new');").unwrap();
        let r = atlasFileSnapshotGet(&json!({ "path": "src/a.ts" }), &c).unwrap();
        assert_eq!(r["snapshot_id"], json!(2)); // ORDER BY id DESC picks latest
        assert_eq!(r["snapshot"], json!({ "v": 2 }));
        assert_eq!(r["model_id"], json!("m2"));
        assert_eq!(r["stale"], json!(false)); // hashes match
        // node hash moves on → stale true
        c.execute_batch("UPDATE code.code_nodes SET content_hash='h-newer' WHERE path='src/a.ts';").unwrap();
        assert_eq!(atlasFileSnapshotGet(&json!({ "path": "src/a.ts" }), &c).unwrap()["stale"], json!(true));
        // key order is part of the contract
        let r2 = atlasFileSnapshotGet(&json!({ "path": "src/a.ts" }), &c).unwrap();
        let keys: Vec<String> = r2.as_object().unwrap().keys().cloned().collect();
        assert_eq!(keys, vec!["path", "snapshot_id", "snapshot", "model_id", "created_at", "stale"]);
    }

    #[test]
    fn null_hashes_and_bad_json_never_stale() {
        let c = db();
        c.execute_batch("INSERT INTO code.code_file_snapshots (id,origin,path,content_hash,snapshot_json,model_id,created_at) VALUES \
            (1,'atlas','src/b.ts',NULL,'not-json',NULL,'2025-12-31 10:00:00');").unwrap();
        // no node row → cur null → stale false; invalid snapshot_json → null
        let r = atlasFileSnapshotGet(&json!({ "path": "src/b.ts" }), &c).unwrap();
        assert_eq!(r["snapshot"], Value::Null);
        assert_eq!(r["stale"], json!(false));
        // node exists with a hash but stored hash NULL → still false
        c.execute_batch("INSERT INTO code.code_nodes (id,origin,path,type,content_hash) VALUES ('src/b.ts@atlas','atlas','src/b.ts','file','h1');").unwrap();
        assert_eq!(atlasFileSnapshotGet(&json!({ "path": "src/b.ts" }), &c).unwrap()["stale"], json!(false));
    }

    #[test]
    fn origin_scopes_and_non_file_nodes_ignored() {
        let c = db();
        c.execute_batch("INSERT INTO code.code_file_snapshots (id,origin,path,content_hash,snapshot_json,model_id,created_at) VALUES \
            (1,'bp','src/c.ts','h1','{\"v\":9}','m','2025-12-31 10:00:00'); \
          INSERT INTO code.code_nodes (id,origin,path,type,content_hash) VALUES \
            ('fn1','bp','src/c.ts','function','zzz');").unwrap();
        // default origin 'atlas' misses the bp snapshot
        assert_eq!(atlasFileSnapshotGet(&json!({ "path": "src/c.ts" }), &c).unwrap()["snapshot"], Value::Null);
        // explicit origin hits it; only type='file' nodes count → cur null → stale false
        let r = atlasFileSnapshotGet(&json!({ "path": "src/c.ts", "origin": "bp" }), &c).unwrap();
        assert_eq!(r["snapshot_id"], json!(1));
        assert_eq!(r["stale"], json!(false));
    }

    #[test]
    fn missing_path_errs() {
        let c = db();
        assert!(atlasFileSnapshotGet(&json!({}), &c).is_err());
        assert!(atlasFileSnapshotGet(&json!({ "path": "" }), &c).is_err());
    }
}
