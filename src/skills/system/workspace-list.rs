//! workspace-list.rs — twin of skills/system/workspace-list.mjs (BESPOKE NATIVE, not conform-db;
//! see rationale below). NAMES ARE CONTRACT: `workspaceList` (default export, the sole unit —
//! census counts only functions; there are no other exports in the JS file).
//!
//! Lists every registered workspace from the substrate STATE registry — deliberately NOT
//! `context.db`. Once a non-self workspace is active, `openDb` redirects `context.db` to that
//! workspace's own DB set (which carries only its single self-aware row), so the switcher opens a
//! RAW second connection straight at the registry file
//! (`new DatabaseSync(context?.registryPath ?? DEFAULT_STATE_PATH)`) to see every workspace
//! regardless of which one is currently mounted.
//!
//! BESPOKE NATIVE TEST (not conform-db), same rationale as `db-reset.rs`'s `archive_dir_override`:
//! `context.registryPath` IS a genuine caller-injectable override in the JS itself (proven by the
//! JS test, which points `context.db` at an UNRELATED in-memory db and passes `registryPath`
//! separately, to prove the skill reads the registry file and ignores `context.db`) — so this twin
//! takes it as a plain `Option<&Path>` parameter rather than the env-var + tempdir indirection the
//! `god-driver-*` precedents needed. But the conform-db harness only ever seeds/inspects ONE
//! connection (`context.db`); this skill deliberately opens a SECOND, independent connection at a
//! real file path, which the harness has no seam for — the playbook's cross-db VET note says
//! exactly this: "still block if the skill OPENS a db file itself ... the lane only covers
//! qualified SQL through `context.db`". Tested here instead via `#[cfg(test)]` Rust-native tests
//! that open a real temp-dir SQLite file and pass its path directly as the override.
//!
//! `DEFAULT_STATE_PATH` (roots.mjs `DB_PATHS.state` = `SUBSTRATE_ROOT/.antiloki/db/state.sqlite`,
//! `SUBSTRATE_ROOT` = `$ANTILOKI_SUBSTRATE_ROOT` else self-derived from the engine module's own
//! file location) has no exact analog in a compiled Rust binary — same caveat as `db-reset.rs`'s
//! `default_archive_dir`; used ONLY when no override is passed (every test below passes one).
//!
//! `active = workspaces.find(w => w.is_active === 1) ?? null` — strict `=== 1`, not truthy;
//! `db_dir` stays `NULL`/`None` verbatim for rows that carry it (e.g. the self-hosting row) —
//! never coalesced. Output key order matches the JS literal `{ workspaces, active, count }`;
//! `workspaces` row key order matches the SELECT list (`id, name, root_path, vcs, is_active,
//! db_dir, created_at`), same as node:sqlite's row-object convention.

use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};

// No exact JS analog: roots.mjs's SUBSTRATE_ROOT chains ANTILOKI_SUBSTRATE_ROOT (else self-derived
// from the engine module's own file location) + '.antiloki/db/state.sqlite'. A compiled Rust binary
// has no notion of "its own module file location" — best-effort fallback used ONLY when no
// `registry_path_override` is passed (every test below passes one explicitly).
// Delegates to the crate's ONE resolver. This module used to carry its own copy that
// consulted only ANTILOKI_SUBSTRATE_ROOT and otherwise fell back to the process CWD —
// so under the BE (cwd = antiloki-BE) it opened a state.sqlite that does not exist and
// every /api/v1/workspaces/list returned 500 on the Rust engine while JS answered fine.
// The JS twin reads DEFAULT_STATE_PATH from open-db, which honours ANTILOKI_DATA_DIR;
// engine_open_db::default_state_path() is that same rule, in one place.
fn default_state_path() -> PathBuf {
    crate::engine_open_db::default_state_path()
}

/// Is a registered workspace's folder gone from the disk (moved, trashed, a drive that is not mounted)? An empty path is missing too. Shared with `workspace-activate`.
pub fn folder_missing(root_path: &str) -> bool { root_path.trim().is_empty() || !Path::new(root_path.trim()).is_dir() }

pub fn workspaceList(_input: &Value, registry_path_override: Option<&Path>) -> Result<Value, String> {
    let path = registry_path_override.map(|p| p.to_path_buf()).unwrap_or_else(default_state_path);
    let conn = Connection::open(&path).map_err(|e| e.to_string())?;
    let rows = crate::db_json::query_rows(
        &conn,
        "SELECT id, name, root_path, vcs, is_active, db_dir, created_at FROM workspaces ORDER BY id ASC",
        &[],
    )?;
    // A FOLDER THAT IS GONE IS SAID, NEVER DELETED (operator 2026-10-01: "ministore-2" moved to the Trash and "browsed" leftovers stayed in the list, and picking
    // one failed far from the cause). The row keeps everything it had and gains `missing: true` - the FE dims it and offers the existing remove; the registry is the
    // operator's to edit, so nothing here removes a row.
    let workspaces: Vec<Value> = rows.as_array().cloned().unwrap_or_default().into_iter().map(|mut w| {
        let missing = folder_missing(w.get("root_path").and_then(Value::as_str).unwrap_or(""));
        if let Some(o) = w.as_object_mut() { o.insert("missing".into(), json!(missing)); }
        w
    }).collect();
    let active = workspaces
        .iter()
        .find(|w| w.get("is_active").and_then(Value::as_i64) == Some(1))
        .cloned()
        .unwrap_or(Value::Null);
    let mut out = Map::new();
    let count = workspaces.len();
    out.insert("workspaces".into(), Value::Array(workspaces));
    out.insert("active".into(), active);
    out.insert("count".into(), json!(count as i64));
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_dir(tag: &str) -> PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!("antiloki-workspace-list-rs-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn make_registry(tag: &str, rows: &[(&str, &str, i64, Option<&str>)]) -> PathBuf {
        let dir = temp_dir(tag);
        let path = dir.join("state.sqlite");
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE workspaces (
                id           INTEGER PRIMARY KEY AUTOINCREMENT,
                name         TEXT    NOT NULL UNIQUE,
                root_path    TEXT    NOT NULL,
                vcs          TEXT    NOT NULL DEFAULT 'git',
                is_active    INTEGER NOT NULL DEFAULT 0,
                db_dir       TEXT,
                created_at   TEXT    NOT NULL DEFAULT '2026-01-01 00:00:00'
            )",
        )
        .unwrap();
        for (name, root_path, is_active, db_dir) in rows {
            conn.execute(
                "INSERT INTO workspaces (name, root_path, vcs, is_active, db_dir) VALUES (?1, ?2, 'git', ?3, ?4)",
                rusqlite::params![name, root_path, is_active, db_dir],
            )
            .unwrap();
        }
        path
    }

    #[test]
    fn returns_full_registry_and_marks_active() {
        let path = make_registry(
            "full",
            &[("self", "/substrate", 0, None), ("proj1", "/managed/proj1", 1, Some("proj1/db"))],
        );
        let r = workspaceList(&json!({}), Some(&path)).unwrap();
        assert_eq!(r["count"], json!(2));
        let names: Vec<&str> =
            r["workspaces"].as_array().unwrap().iter().map(|w| w["name"].as_str().unwrap()).collect();
        assert_eq!(names, ["self", "proj1"]);
        assert_eq!(r["active"]["name"], json!("proj1"));
        let keys: Vec<&String> = r.as_object().unwrap().keys().collect();
        assert_eq!(keys, ["workspaces", "active", "count"]);
        let row_keys: Vec<&String> = r["workspaces"][0].as_object().unwrap().keys().collect();
        assert_eq!(row_keys, ["id", "name", "root_path", "vcs", "is_active", "db_dir", "created_at", "missing"]);
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn self_hosting_only_self_active_db_dir_null() {
        let path = make_registry("self-only", &[("self", "/substrate", 1, None)]);
        let r = workspaceList(&json!({}), Some(&path)).unwrap();
        assert_eq!(r["count"], json!(1));
        assert_eq!(r["active"]["name"], json!("self"));
        assert_eq!(r["active"]["db_dir"], Value::Null);
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn no_active_workspace_returns_null() {
        let path = make_registry("no-active", &[("self", "/substrate", 0, None)]);
        let r = workspaceList(&json!({}), Some(&path)).unwrap();
        assert_eq!(r["active"], Value::Null);
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn empty_registry() {
        let path = make_registry("empty", &[]);
        let r = workspaceList(&json!({}), Some(&path)).unwrap();
        assert_eq!(r["count"], json!(0));
        assert_eq!(r["workspaces"], json!([]));
        assert_eq!(r["active"], Value::Null);
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn ignores_input_argument() {
        let path = make_registry("ignore-input", &[("self", "/substrate", 1, None)]);
        let r = workspaceList(&json!({ "anything": "goes" }), Some(&path)).unwrap();
        assert_eq!(r["count"], json!(1));
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    // OPERATOR 2026-10-01: "ministore-2" was moved to the Trash and the list still offered it; "browsed" leftovers likewise.
    #[test]
    fn a_workspace_whose_folder_is_gone_is_flagged_and_never_dropped() {
        let here = temp_dir("missing-root");   // a real folder
        let gone = temp_dir("missing-gone");
        fs::remove_dir_all(&gone).unwrap();
        let path = make_registry("missing", &[("here", here.to_str().unwrap(), 1, Some("here/db")), ("ministore-2", gone.to_str().unwrap(), 0, Some("m/db")), ("browsed", "", 0, Some("b/db"))]);
        let r = workspaceList(&json!({}), Some(&path)).unwrap();
        let by = |n: &str| r["workspaces"].as_array().unwrap().iter().find(|w| w["name"] == json!(n)).unwrap().clone();
        assert_eq!(by("here")["missing"], json!(false));
        assert_eq!(by("ministore-2")["missing"], json!(true));
        assert_eq!(by("browsed")["missing"], json!(true), "an empty path is a folder that is not there");
        assert_eq!(r["count"], json!(3), "no row is removed by listing");
        assert_eq!(r["active"]["missing"], json!(false));
        let _ = fs::remove_dir_all(path.parent().unwrap());
        let _ = fs::remove_dir_all(&here);
    }
}
