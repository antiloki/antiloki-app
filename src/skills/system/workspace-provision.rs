//! workspace-provision.rs — twin of skills/system/workspace-provision.mjs. NAMES ARE CONTRACT:
//! `workspaceProvision` (default export).
//!
//! BESPOKE-NATIVE, not conform-db fixture-tested: this creates a brand-new 5+4-DB set on real
//! disk (`mkdir`s real dirs, opens real sqlite files via the already-converted `openDb` twin,
//! opens a SECOND real sqlite connection to the live source registry, ATTACHes its sibling DBs,
//! reconciles tables/columns via live `sqlite_master`/`PRAGMA table_info` introspection, and
//! copies catalog rows). None of that is expressible as a conform-db fixture — same "db-file
//! open" bespoke-native class as `engine/open-db.rs`. Proven below via `#[cfg(test)]` against
//! real temp directories.
//!
//! Cross-module call (already-converted twin, reused not reimplemented — playbook rule 11):
//! `crate::engine_open_db::openDb`.
//!
//! `err.required(skill, field)` (engine/error.mjs, not yet a twin) has a fixed message shape —
//! `"${skill}: ${field} is required"` — reproduced inline rather than pulled through a stub twin.

use regex::Regex;
use rusqlite::Connection;
use serde_json::{json, Value};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

fn str_qq(v: Option<&Value>) -> String { match v { None|Some(Value::Null)=>String::new(), Some(Value::String(s))=>s.clone(), Some(Value::Bool(b))=>b.to_string(), Some(n@Value::Number(_))=>n.to_string(), Some(o)=>o.to_string() } }

const SCHEMA_FILES: [(&str, &str); 4] = [
    ("memory", "memory.sqlite"),
    ("brain", "brain.sqlite"),
    ("laws", "laws.sqlite"),
    ("knowledge", "knowledge.sqlite"),
];
const SCHEMAS: [&str; 5] = ["main", "memory", "brain", "laws", "knowledge"];

fn to_sql_value(v: &Value) -> rusqlite::types::Value {
    use rusqlite::types::Value as S;
    match v {
        Value::Null => S::Null,
        Value::Bool(b) => S::Integer(*b as i64),
        Value::Number(n) => n.as_i64().map(S::Integer).unwrap_or_else(|| S::Real(n.as_f64().unwrap_or(0.0))),
        Value::String(s) => S::Text(s.clone()),
        other => S::Text(other.to_string()),
    }
}

/// SQLite stores attached-table DDL WITHOUT the schema prefix — inject `<schema>.` before the
/// object name for any non-main schema, matching the JS regex-replace exactly.
fn inject_schema(sql: &str, schema: &str) -> String {
    if schema == "main" { return sql.to_string(); }
    let re = Regex::new(r"(?i)^(\s*CREATE\s+(?:TABLE|(?:UNIQUE\s+)?INDEX|TRIGGER|VIEW)\s+(?:IF\s+NOT\s+EXISTS\s+)?)").unwrap();
    match re.find(sql) {
        Some(m) => format!("{}{}.{}", &sql[..m.end()], schema, &sql[m.end()..]),
        None => sql.to_string(),
    }
}

fn table_names(db: &Connection, schema: &str, extra_where: &str) -> Result<Vec<Value>, String> {
    let rows = crate::db_json::query_rows(
        db,
        &format!("SELECT name FROM {schema}.sqlite_master WHERE type='table' {extra_where}"),
        &[],
    )?;
    Ok(rows.as_array().cloned().unwrap_or_default())
}

fn reconcile_tables(source: &Connection, dest: &Connection, schema: &str) -> Result<i64, String> {
    let dest_tables: HashSet<String> = table_names(dest, schema, "")?
        .iter().filter_map(|r| r.get("name").and_then(Value::as_str).map(str::to_string)).collect();
    let src_rows = crate::db_json::query_rows(
        source,
        &format!("SELECT name, sql FROM {schema}.sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' AND sql IS NOT NULL"),
        &[],
    )?;
    let mut created = 0i64;
    for t in src_rows.as_array().cloned().unwrap_or_default() {
        let name = str_qq(t.get("name"));
        if dest_tables.contains(&name) { continue; }
        let sql = str_qq(t.get("sql"));
        dest.execute_batch(&inject_schema(&sql, schema)).map_err(|e| e.to_string())?;
        created += 1;
    }
    Ok(created)
}

fn reconcile_columns(source: &Connection, dest: &Connection, schema: &str) -> Result<i64, String> {
    let tables: Vec<String> = table_names(dest, schema, "AND name NOT LIKE 'sqlite_%'")?
        .iter().filter_map(|r| r.get("name").and_then(Value::as_str).map(str::to_string)).collect();
    let mut added = 0i64;
    for table in tables {
        let src_cols = crate::db_json::query_rows(source, &format!("PRAGMA {schema}.table_info({table})"), &[])?;
        let src_cols = src_cols.as_array().cloned().unwrap_or_default();
        if src_cols.is_empty() { continue; } // table absent in source — nothing to mirror
        let dst_cols = crate::db_json::query_rows(dest, &format!("PRAGMA {schema}.table_info({table})"), &[])?;
        let dst_names: HashSet<String> = dst_cols.as_array().cloned().unwrap_or_default()
            .iter().filter_map(|c| c.get("name").and_then(Value::as_str).map(str::to_string)).collect();
        for c in &src_cols {
            let name = str_qq(c.get("name"));
            if dst_names.contains(&name) { continue; }
            let ctype = str_qq(c.get("type"));
            let mut ddl = format!("{name} {ctype}").trim().to_string();
            if let Some(dflt) = c.get("dflt_value").filter(|v| !v.is_null()) {
                ddl.push_str(&format!(" DEFAULT {}", str_qq(Some(dflt))));
            }
            let notnull = c.get("notnull").and_then(Value::as_i64).unwrap_or(0);
            if notnull != 0 { ddl.push_str(" NOT NULL"); }
            dest.execute_batch(&format!("ALTER TABLE {schema}.{table} ADD COLUMN {ddl}")).map_err(|e| e.to_string())?;
            added += 1;
        }
    }
    Ok(added)
}

fn copy_catalog(source: &Connection, dest: &Connection, table: &str, where_clause: &str) -> Result<i64, String> {
    let src_cols: HashSet<String> = crate::db_json::query_rows(source, &format!("PRAGMA table_info({table})"), &[])?
        .as_array().cloned().unwrap_or_default()
        .iter().filter_map(|c| c.get("name").and_then(Value::as_str).map(str::to_string)).collect();
    let dst_cols: Vec<String> = crate::db_json::query_rows(dest, &format!("PRAGMA table_info({table})"), &[])?
        .as_array().cloned().unwrap_or_default()
        .iter().filter_map(|c| c.get("name").and_then(Value::as_str).map(str::to_string)).collect();
    let cols: Vec<String> = dst_cols.into_iter().filter(|c| src_cols.contains(c) && c != "id").collect();
    if cols.is_empty() { return Ok(0); }
    let col_list = cols.join(", ");
    let placeholders = cols.iter().map(|_| "?").collect::<Vec<_>>().join(", ");
    let rows = crate::db_json::query_rows(source, &format!("SELECT {col_list} FROM {table} {where_clause}"), &[])?;
    let rows = rows.as_array().cloned().unwrap_or_default();
    let mut stmt = dest.prepare(&format!("INSERT OR IGNORE INTO {table} ({col_list}) VALUES ({placeholders})")).map_err(|e| e.to_string())?;
    for r in &rows {
        let params: Vec<rusqlite::types::Value> = cols.iter().map(|c| to_sql_value(r.get(c).unwrap_or(&Value::Null))).collect();
        stmt.execute(rusqlite::params_from_iter(params)).map_err(|e| e.to_string())?;
    }
    Ok(rows.len() as i64)
}

fn resolve_absolute(p: &str) -> PathBuf {
    let path = Path::new(p);
    if path.is_absolute() { path.to_path_buf() } else { std::env::current_dir().unwrap_or_default().join(path) }
}

fn esc(p: &str) -> String { p.replace('\'', "''") }

/// `export default function workspaceProvision(input, context)`.
pub fn workspaceProvision(input: &Value, context: &Value) -> Result<Value, String> {
    let name = input.get("name").and_then(Value::as_str).filter(|s| !s.is_empty());
    let name = match name {
        Some(n) => n,
        None => return Err("workspace-provision: name is required".to_string()),
    };
    let root_path = input.get("root_path").and_then(Value::as_str).filter(|s| !s.is_empty());
    let root_path = match root_path {
        Some(r) => r,
        None => return Err("workspace-provision: root_path is required".to_string()),
    };

    let db_base = context.get("dbBase").and_then(Value::as_str).filter(|s| !s.is_empty());
    let db_dir_abs: PathBuf = match db_base {
        Some(base) => Path::new(base).join(name).join("db"),
        None => resolve_absolute(root_path).join(".antiloki").join("db"),
    };
    std::fs::create_dir_all(&db_dir_abs).map_err(|e| e.to_string())?;
    let substrate_root = db_dir_abs.parent().map(Path::to_path_buf).unwrap_or_else(|| db_dir_abs.clone());
    for d in ["sources", "knowledge", "backups", "archive"] {
        let _ = std::fs::create_dir_all(substrate_root.join(d)); // best-effort
    }

    let source_path = context.get("sourceRegistryPath").and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| crate::engine_open_db::default_state_path().to_string_lossy().into_owned());

    let ws_input = json!({
        "statePath": db_dir_abs.join("state.sqlite").to_string_lossy(),
        "memoryPath": db_dir_abs.join("memory.sqlite").to_string_lossy(),
        "brainPath": db_dir_abs.join("brain.sqlite").to_string_lossy(),
        "lawsPath": db_dir_abs.join("laws.sqlite").to_string_lossy(),
        "knowledgePath": db_dir_abs.join("knowledge.sqlite").to_string_lossy(),
    });
    let ws_db = crate::engine_open_db::openDb(&ws_input, &json!({}))?;

    let mut actions_copied = 0i64;
    let mut columns_reconciled = 0i64;
    let mut tables_created = 0i64;
    {
        let source = Connection::open(&source_path).map_err(|e| e.to_string())?;

        let src_dir = Path::new(&source_path).parent().unwrap_or_else(|| Path::new(".")).to_path_buf();
        for (schema, file) in SCHEMA_FILES {
            let p = esc(&src_dir.join(file).to_string_lossy());
            let _ = source.execute_batch(&format!("ATTACH DATABASE '{p}' AS {schema}")); // sibling absent → reconcile main only
        }

        for schema in SCHEMAS {
            tables_created += reconcile_tables(&source, &ws_db, schema)?;
            columns_reconciled += reconcile_columns(&source, &ws_db, schema)?;
        }

        // Agents FIRST (self-referential parent_agent FK + actions.owner_agent / agent_actions.agent_name FKs).
        copy_catalog(&source, &ws_db, "agents", "ORDER BY (parent_agent IS NOT NULL)")?;
        actions_copied = copy_catalog(&source, &ws_db, "actions", "")?;
        copy_catalog(&source, &ws_db, "agent_actions", "WHERE revoked_at IS NULL")?;
        // `source` closes here (scope end), mirroring the JS inner try/finally.
    }

    ws_db.execute_batch("DELETE FROM workspaces").map_err(|e| e.to_string())?;
    ws_db.execute(
        "INSERT INTO workspaces (name, root_path, vcs, is_active, db_dir) VALUES (?1, ?2, 'git', 1, ?3)",
        rusqlite::params![name, root_path, db_dir_abs.to_string_lossy()],
    ).map_err(|e| e.to_string())?;

    let db_dir_str = db_dir_abs.to_string_lossy().into_owned();
    Ok(json!({
        "name": name,
        "root_path": root_path,
        "db_dir": db_dir_str,
        "db_dir_abs": db_dir_str,
        "actions_copied": actions_copied,
        "columns_reconciled": columns_reconciled,
        "tables_created": tables_created,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_dir(tag: &str) -> PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!("antiloki-workspace-provision-rs-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    // Builds a minimal live "source registry" (state.sqlite + memory sibling) with an agents row,
    // an actions row, and an agent_actions grant — enough to exercise reconcile + copy paths.
    fn seed_source(dir: &Path) -> PathBuf {
        let state_path = dir.join("state.sqlite");
        let src = Connection::open(&state_path).unwrap();
        src.execute_batch(
            "CREATE TABLE agents (id INTEGER PRIMARY KEY, name TEXT UNIQUE, description TEXT, parent_agent TEXT);
             CREATE TABLE actions (id INTEGER PRIMARY KEY, name TEXT UNIQUE, description TEXT, owner_agent TEXT, steps_json TEXT);
             CREATE TABLE agent_actions (id INTEGER PRIMARY KEY, agent_name TEXT, action_name TEXT, granted_by TEXT, revoked_at TEXT);
             CREATE TABLE workspaces (id INTEGER PRIMARY KEY, name TEXT, root_path TEXT, vcs TEXT, is_active INTEGER, db_dir TEXT);
             CREATE TABLE extra_only_in_source (id INTEGER PRIMARY KEY, note TEXT);"
        ).unwrap();
        // description (agents/actions), steps_json (actions) and granted_by (agent_actions) are
        // NOT NULL with no DEFAULT on the real canonical tables (open-db/schemas.mjs) — the
        // fixture must carry them or copyCatalog's INSERT OR IGNORE silently no-ops the row.
        // Verified against the real JS via a node repro: an incomplete fixture reproduces the
        // exact same silent-drop (missing NOT NULL col) or "FOREIGN KEY constraint failed"
        // (agent_actions.action_name dangling because the actions row was silently dropped) —
        // not a Rust-only divergence, so the fixture (not the port) needed the extra columns.
        src.execute("INSERT INTO agents (name, description, parent_agent) VALUES ('root', 'root agent', NULL)", []).unwrap();
        src.execute("INSERT INTO agents (name, description, parent_agent) VALUES ('child', 'child agent', 'root')", []).unwrap();
        src.execute("INSERT INTO actions (name, description, owner_agent, steps_json) VALUES ('system.demo.act', 'demo action', 'root', '[]')", []).unwrap();
        src.execute("INSERT INTO agent_actions (agent_name, action_name, granted_by, revoked_at) VALUES ('root', 'system.demo.act', 'root', NULL)", []).unwrap();
        src.execute("INSERT INTO agent_actions (agent_name, action_name, granted_by, revoked_at) VALUES ('child', 'system.demo.act', 'root', '2025-01-01')", []).unwrap();
        drop(src);

        let mem_path = dir.join("memory.sqlite");
        let mem = Connection::open(&mem_path).unwrap();
        mem.execute_batch("CREATE TABLE error_events (id INTEGER PRIMARY KEY, note TEXT)").unwrap();
        drop(mem);

        state_path
    }

    #[test]
    fn workspace_provision_creates_db_set_and_workspace_row() {
        let root = temp_dir("basic-root");
        let src_dir = temp_dir("basic-src");
        let source_path = seed_source(&src_dir);

        let input = json!({ "name": "alfa", "root_path": root.to_string_lossy() });
        let context = json!({ "sourceRegistryPath": source_path.to_string_lossy() });
        let r = workspaceProvision(&input, &context).unwrap();

        assert_eq!(r["name"], json!("alfa"));
        assert_eq!(r["root_path"], json!(root.to_string_lossy().into_owned()));
        let db_dir = root.join(".antiloki").join("db");
        assert!(db_dir.join("state.sqlite").exists());
        assert!(db_dir.join("memory.sqlite").exists());
        assert!(db_dir.join("brain.sqlite").exists());
        assert!(db_dir.join("laws.sqlite").exists());
        assert!(db_dir.join("knowledge.sqlite").exists());
        for d in ["sources", "knowledge", "backups", "archive"] {
            assert!(root.join(".antiloki").join(d).is_dir(), "{d} sibling dir created");
        }

        let ws = Connection::open(db_dir.join("state.sqlite")).unwrap();
        let n_ws: i64 = ws.query_row("SELECT COUNT(*) FROM workspaces", [], |row| row.get(0)).unwrap();
        assert_eq!(n_ws, 1, "seeded 'self' row replaced by exactly one active row");
        let (ws_name, ws_root, is_active): (String, String, i64) = ws.query_row(
            "SELECT name, root_path, is_active FROM workspaces", [], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        ).unwrap();
        assert_eq!(ws_name, "alfa");
        assert_eq!(ws_root, root.to_string_lossy().into_owned());
        assert_eq!(is_active, 1);

        assert_eq!(r["actions_copied"], json!(1i64), "1 action row copied from source (canonical seed already has none named system.demo.act)");
        let n_extra: i64 = ws.query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='extra_only_in_source'", [], |row| row.get(0),
        ).unwrap();
        assert_eq!(n_extra, 1, "reconcileTables mirrored the source-only table");

        let _ = fs::remove_dir_all(&root);
        let _ = fs::remove_dir_all(&src_dir);
    }

    #[test]
    fn workspace_provision_copies_agents_before_actions_and_active_grant_only() {
        let root = temp_dir("agents-root");
        let src_dir = temp_dir("agents-src");
        let source_path = seed_source(&src_dir);

        let input = json!({ "name": "beta", "root_path": root.to_string_lossy() });
        let context = json!({ "sourceRegistryPath": source_path.to_string_lossy() });
        workspaceProvision(&input, &context).unwrap();

        let db_dir = root.join(".antiloki").join("db");
        let ws = Connection::open(db_dir.join("state.sqlite")).unwrap();
        let child_exists: i64 = ws.query_row("SELECT COUNT(*) FROM agents WHERE name='child'", [], |row| row.get(0)).unwrap();
        assert_eq!(child_exists, 1, "child agent (with parent_agent FK) copied without error");
        let root_grant: i64 = ws.query_row(
            "SELECT COUNT(*) FROM agent_actions WHERE agent_name='root' AND action_name='system.demo.act'", [], |row| row.get(0),
        ).unwrap();
        assert_eq!(root_grant, 1, "active grant copied");
        let revoked_grant: i64 = ws.query_row(
            "SELECT COUNT(*) FROM agent_actions WHERE agent_name='child' AND action_name='system.demo.act'", [], |row| row.get(0),
        ).unwrap();
        assert_eq!(revoked_grant, 0, "revoked grant NOT copied (WHERE revoked_at IS NULL)");

        let _ = fs::remove_dir_all(&root);
        let _ = fs::remove_dir_all(&src_dir);
    }

    #[test]
    fn workspace_provision_missing_name_returns_exact_message() {
        let e = workspaceProvision(&json!({ "root_path": "C:/x" }), &json!({})).unwrap_err();
        assert_eq!(e, "workspace-provision: name is required");
    }

    #[test]
    fn workspace_provision_missing_root_path_returns_exact_message() {
        let e = workspaceProvision(&json!({ "name": "alfa" }), &json!({})).unwrap_err();
        assert_eq!(e, "workspace-provision: root_path is required");
    }

    #[test]
    fn workspace_provision_db_base_override_uses_name_scoped_dir() {
        let base = temp_dir("dbbase-root");
        let src_dir = temp_dir("dbbase-src");
        let source_path = seed_source(&src_dir);

        let input = json!({ "name": "gamma", "root_path": "C:/some/managed/project" });
        let context = json!({ "dbBase": base.to_string_lossy(), "sourceRegistryPath": source_path.to_string_lossy() });
        let r = workspaceProvision(&input, &context).unwrap();

        let expected_dir = base.join("gamma").join("db");
        assert_eq!(r["db_dir"], json!(expected_dir.to_string_lossy().into_owned()));
        assert!(expected_dir.join("state.sqlite").exists());

        let _ = fs::remove_dir_all(&base);
        let _ = fs::remove_dir_all(&src_dir);
    }

    #[test]
    fn inject_schema_prefixes_non_main_schema_before_object_name() {
        assert_eq!(
            inject_schema("CREATE TABLE foo (id INTEGER)", "memory"),
            "CREATE TABLE memory.foo (id INTEGER)"
        );
        assert_eq!(
            inject_schema("CREATE TABLE IF NOT EXISTS foo (id INTEGER)", "brain"),
            "CREATE TABLE IF NOT EXISTS brain.foo (id INTEGER)"
        );
        assert_eq!(
            inject_schema("CREATE UNIQUE INDEX idx_x ON foo(id)", "laws"),
            "CREATE UNIQUE INDEX laws.idx_x ON foo(id)"
        );
        assert_eq!(inject_schema("CREATE TABLE foo (id INTEGER)", "main"), "CREATE TABLE foo (id INTEGER)");
    }
}
