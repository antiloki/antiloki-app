//! atlas-register.rs — twin of skills/codebase/atlas-register.mjs. NAMES ARE CONTRACT:
//! `atlasRegister` (default, db). REGISTER-AT-END: copies a rebuild child's private staging code
//! db (`code.rebuild.sqlite`, real file, path given by the caller) into the LIVE `code` schema in
//! one write transaction — REPLACE_TABLES wipe+copy origin-scoped rows, APPEND_TABLES copy every
//! staging row onto live history unconditionally (no origin filter — matches the JS SELECT with
//! no WHERE clause). Never writes `stage` — only ATTACHes it, SELECTs from it, then DETACHes.
//!
//! FS/cross-db classification: `staging_code_path` is neither a hardcoded constant nor
//! DB-derived — it is a plain `input` field, the MOST directly fixture-controllable case (the
//! fixture's `args` supplies the path outright). Fixture-tested via a committed, read-only
//! testdata sqlite file (`antiloki-crossing/fixtures/testdata/atlas-register-stage.sqlite`, built
//! by the sibling `_build-atlas-register-stage.mjs`) that both lanes ATTACH by the same absolute
//! path — same bytes, same engine, so the copy is deterministic. `context.db` is the LIVE
//! multi-db connection where `code` is itself an ATTACHed schema (not main), so the fixture
//! attaches `code` in-memory per the cross-db lane (`"attach": ["code"]`) and the skill's own
//! runtime `ATTACH DATABASE '<path>' AS stage` layers a SECOND, real-file attach on top — no
//! harness support needed for that half since it is just SQL the skill executes itself, honored
//! identically by rusqlite and node:sqlite.
//!
//! Only 4 of the 13 REPLACE/APPEND tables are created in the fixture's live `code` schema
//! (code_nodes, code_signals; code_reports, code_api_snapshots) — the other 9 are deliberately
//! left absent so their per-table try/catch takes the "table absent on one side" fallback
//! (`replaced[t] = -1` / `appended[t] = -1`), the same schema-drift path production hits without
//! needing to transcribe all 22 code-schema tables here.
//!
//! Guard-throws (`!db` / `!staging_code_path`) are not fixture-exercised (playbook rule 9); the
//! `!db` guard is additionally unreachable in Rust (`db: &Connection` is non-nullable).
// QUALITY: 81/100 — origin-scoped REPLACE (DELETE+bound-param, INSERT...SELECT with a manually
// escaped origin literal matching the JS's asymmetric parameterization) vs unconditional APPEND;
// autoincrement-id column skip via pragma_table_info introspection; BEGIN/COMMIT/ROLLBACK +
// stage ATTACH/DETACH lifecycle ported 1:1 including the "detach even after rollback" ordering.

use rusqlite::Connection;
use serde_json::{json, Map, Value};

const REPLACE_TABLES: &[&str] = &[
    "code_nodes", "code_edges", "code_parse_health", "code_entry_points", "code_signals",
    "code_decorators", "code_descriptions", "code_design_tokens", "code_design_components",
];
const APPEND_TABLES: &[&str] = &["code_reports", "code_digests", "code_entry_point_snapshots", "code_api_snapshots"];

fn truthy(v: Option<&Value>) -> bool { match v { None|Some(Value::Null)=>false, Some(Value::Bool(b))=>*b, Some(Value::Number(n))=>n.as_f64().map_or(false,|f|f!=0.0&&!f.is_nan()), Some(Value::String(s))=>!s.is_empty(), Some(_)=>true } }
fn js_string(v: &Value) -> String { match v { Value::Null=>"null".into(), Value::String(s)=>s.clone(), Value::Number(n)=>n.to_string(), Value::Bool(b)=>b.to_string(), Value::Array(a)=>a.iter().map(js_string).collect::<Vec<_>>().join(","), Value::Object(_)=>"[object Object]".into() } }
fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) }

// JS `copyCols(db, table)`: `pragma_table_info('<table>', 'code')` against the LIVE `code`
// schema (the target defines the shape); a missing table just yields zero rows (no error) — the
// caller's DELETE/INSERT is what actually throws for an absent table. Skips an autoincrement
// `id` (INTEGER PRIMARY KEY named `id`) so appended history gets fresh ids and REPLACE never
// collides an autoincrement id across origins.
fn copy_cols(db: &Connection, table: &str) -> Vec<String> {
    let sql = format!("SELECT name, type, pk FROM pragma_table_info('{table}', 'code')");
    let rows = crate::db_json::query_rows(db, &sql, &[]).unwrap_or_else(|_| json!([]));
    rows.as_array().cloned().unwrap_or_default().into_iter().filter_map(|c| {
        let name = c.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let ty = c.get("type").and_then(|v| v.as_str()).unwrap_or("").to_uppercase();
        let pk = c.get("pk").and_then(|v| v.as_i64()).unwrap_or(0);
        if pk != 0 && name == "id" && ty.contains("INT") { None } else { Some(name) }
    }).collect()
}

fn col_list(cols: &[String]) -> String { cols.iter().map(|c| format!("\"{c}\"")).collect::<Vec<_>>().join(", ") }

// REPLACE_TABLES: wipe the live origin-scoped rows, copy staging's matching-origin rows in.
// `changes()` after the INSERT (not the DELETE) — the row count actually COPIED.
fn copy_replace_table(db: &Connection, t: &str, origin: &str) -> Result<i64, String> {
    let cols = copy_cols(db, t);
    let list = col_list(&cols);
    db.execute(&format!("DELETE FROM code.{t} WHERE origin = ?1"), rusqlite::params![origin]).map_err(|e| e.to_string())?;
    let insert = format!(
        "INSERT INTO code.{t} ({list}) SELECT {list} FROM stage.{t} WHERE origin = '{}'",
        origin.replace('\'', "''"),
    );
    db.execute_batch(&insert).map_err(|e| e.to_string())?;
    Ok(db.changes() as i64)
}

// APPEND_TABLES: copy EVERY staging row onto live history — no origin filter (matches the JS
// SELECT, which carries no WHERE clause here unlike the REPLACE branch above).
fn copy_append_table(db: &Connection, t: &str) -> Result<i64, String> {
    let cols = copy_cols(db, t);
    let list = col_list(&cols);
    let insert = format!("INSERT INTO code.{t} ({list}) SELECT {list} FROM stage.{t}");
    db.execute_batch(&insert).map_err(|e| e.to_string())?;
    Ok(db.changes() as i64)
}

pub fn atlasRegister(input: &Value, db: &Connection) -> Result<Value, String> {
    let staging_v = nz(input.get("staging_code_path"));
    if !truthy(staging_v) {
        return Err("atlas-register: staging_code_path is required".to_string());
    }
    let staging = js_string(staging_v.unwrap());
    let origin = nz(input.get("origin")).and_then(|v| v.as_str()).unwrap_or("atlas").to_string();

    let attach = format!("ATTACH DATABASE '{}' AS stage", staging.replace('\'', "''"));
    db.execute_batch(&attach).map_err(|e| e.to_string())?;

    let mut replaced = Map::new();
    let mut appended = Map::new();
    let run: Result<(), String> = (|| -> Result<(), String> {
        db.execute_batch("BEGIN IMMEDIATE").map_err(|e| e.to_string())?;
        for t in REPLACE_TABLES {
            let n = copy_replace_table(db, t, &origin).unwrap_or(-1);
            replaced.insert((*t).to_string(), json!(n));
        }
        for t in APPEND_TABLES {
            let n = copy_append_table(db, t).unwrap_or(-1);
            appended.insert((*t).to_string(), json!(n));
        }
        db.execute_batch("COMMIT").map_err(|e| e.to_string())?;
        Ok(())
    })();

    if let Err(e) = run {
        let _ = db.execute_batch("ROLLBACK");
        let _ = db.execute_batch("DETACH DATABASE stage");
        return Err(e);
    }
    let _ = db.execute_batch("DETACH DATABASE stage");

    Ok(json!({ "origin": origin, "replaced": Value::Object(replaced), "appended": Value::Object(appended) }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    // A real on-disk staging sqlite file (not ':memory:' — ATTACH DATABASE needs a real path to
    // exercise the same code path as the fixture's committed testdata file), built fresh per test
    // via a temp-dir + pid-unique filename (mirrors the conform binary's own temp-dir convention).
    fn build_stage(rows_atlas: &[(&str, &str)], rows_other: &[(&str, &str)]) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!("atlas-register-rs-test-{}.sqlite", std::process::id()));
        let _ = fs::remove_file(&path);
        let c = Connection::open(&path).unwrap();
        c.execute_batch(
            "CREATE TABLE code_nodes (id TEXT PRIMARY KEY, origin TEXT NOT NULL, path TEXT NOT NULL, type TEXT NOT NULL, name TEXT, version INTEGER NOT NULL DEFAULT 1, created_at TEXT NOT NULL DEFAULT (datetime('now')));
             CREATE TABLE code_reports (id INTEGER PRIMARY KEY AUTOINCREMENT, origin TEXT NOT NULL, health_score REAL, created_at TEXT NOT NULL DEFAULT (datetime('now')));",
        ).unwrap();
        for (i, (id, path_v)) in rows_atlas.iter().enumerate() {
            c.execute("INSERT INTO code_nodes (id, origin, path, type, name, created_at) VALUES (?1,'atlas',?2,'file',?3,'2026-01-01 00:00:00')",
                rusqlite::params![id, path_v, format!("n{i}")]).unwrap();
        }
        for (id, path_v) in rows_other {
            c.execute("INSERT INTO code_nodes (id, origin, path, type, name, created_at) VALUES (?1,'other',?2,'file','o','2026-01-01 00:00:00')",
                rusqlite::params![id, path_v]).unwrap();
        }
        c.execute("INSERT INTO code_reports (origin, health_score, created_at) VALUES ('atlas', 91.0, '2026-01-01 00:00:00')", []).unwrap();
        c.close().unwrap();
        path
    }

    fn live_db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS code").unwrap();
        c.execute_batch(
            "CREATE TABLE code.code_nodes (id TEXT PRIMARY KEY, origin TEXT NOT NULL, path TEXT NOT NULL, type TEXT NOT NULL, name TEXT, version INTEGER NOT NULL DEFAULT 1, created_at TEXT NOT NULL DEFAULT (datetime('now')));
             CREATE TABLE code.code_reports (id INTEGER PRIMARY KEY AUTOINCREMENT, origin TEXT NOT NULL, health_score REAL, created_at TEXT NOT NULL DEFAULT (datetime('now')));",
        ).unwrap();
        c.execute("INSERT INTO code.code_nodes (id, origin, path, type, name, created_at) VALUES ('old-1','atlas','OLD.js','file','OLD','2026-01-01 00:00:00')", []).unwrap();
        c.execute("INSERT INTO code.code_nodes (id, origin, path, type, name, created_at) VALUES ('keep-1','other','keep.js','file','keep','2026-01-01 00:00:00')", []).unwrap();
        c.execute("INSERT INTO code.code_reports (origin, health_score, created_at) VALUES ('atlas', 50.0, '2026-01-01 00:00:00')", []).unwrap();
        c
    }

    #[test]
    fn replaces_origin_scoped_rows_and_appends_history_unconditionally() {
        let stage_path = build_stage(&[("n-1", "a.js"), ("n-2", "b.js")], &[("n-o", "x.js")]);
        let c = live_db();
        let r = atlasRegister(&json!({ "staging_code_path": stage_path.to_string_lossy().to_string() }), &c).unwrap();
        assert_eq!(r["origin"], json!("atlas"));
        assert_eq!(r["replaced"]["code_nodes"], json!(2)); // only the 2 origin='atlas' stage rows
        assert_eq!(r["replaced"]["code_edges"], json!(-1)); // table absent in live 'code' — fallback
        assert_eq!(r["appended"]["code_reports"], json!(1)); // all stage rows, no origin filter
        assert_eq!(r["appended"]["code_digests"], json!(-1));

        let ids: Vec<String> = c.prepare("SELECT id FROM code.code_nodes ORDER BY id").unwrap()
            .query_map([], |row| row.get::<_, String>(0)).unwrap().map(|x| x.unwrap()).collect();
        assert_eq!(ids, vec!["keep-1".to_string(), "n-1".to_string(), "n-2".to_string()]); // old-1 deleted, keep-1 (other origin) survives
        let report_count: i64 = c.query_row("SELECT COUNT(*) FROM code.code_reports", [], |r| r.get(0)).unwrap();
        assert_eq!(report_count, 2); // pre-existing + 1 appended

        // stage was DETACHed — only main + code remain
        let names: Vec<String> = c.prepare("PRAGMA database_list").unwrap()
            .query_map([], |r| r.get::<_, String>(1)).unwrap().map(|x| x.unwrap()).collect();
        assert_eq!(names, vec!["main".to_string(), "code".to_string()]);
        let _ = fs::remove_file(&stage_path);
    }

    #[test]
    fn escapes_a_quote_in_the_origin_literal() {
        let path = std::env::temp_dir().join(format!("atlas-register-rs-test-quote-{}.sqlite", std::process::id()));
        let _ = fs::remove_file(&path);
        let sc = Connection::open(&path).unwrap();
        sc.execute_batch("CREATE TABLE code_nodes (id TEXT PRIMARY KEY, origin TEXT NOT NULL, path TEXT NOT NULL, type TEXT NOT NULL, name TEXT, version INTEGER NOT NULL DEFAULT 1, created_at TEXT NOT NULL DEFAULT (datetime('now')));
                            CREATE TABLE code_reports (id INTEGER PRIMARY KEY AUTOINCREMENT, origin TEXT NOT NULL, health_score REAL, created_at TEXT NOT NULL DEFAULT (datetime('now')));").unwrap();
        sc.execute("INSERT INTO code_nodes (id, origin, path, type, name, created_at) VALUES ('q-1',?1,'q.js','file','q','2026-01-01 00:00:00')", rusqlite::params!["ali'ce"]).unwrap();
        sc.close().unwrap();

        let c = live_db();
        let r = atlasRegister(&json!({ "staging_code_path": path.to_string_lossy().to_string(), "origin": "ali'ce" }), &c).unwrap();
        assert_eq!(r["origin"], json!("ali'ce"));
        assert_eq!(r["replaced"]["code_nodes"], json!(1));
        let names: Vec<String> = c.prepare("SELECT id FROM code.code_nodes ORDER BY id").unwrap()
            .query_map([], |row| row.get::<_, String>(0)).unwrap().map(|x| x.unwrap()).collect();
        assert_eq!(names, vec!["keep-1".to_string(), "old-1".to_string(), "q-1".to_string()]); // no origin='atlas' match this time → old-1 survives too
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn missing_staging_code_path_errs() {
        let c = live_db();
        assert!(atlasRegister(&json!({}), &c).unwrap_err().contains("staging_code_path is required"));
    }
}
