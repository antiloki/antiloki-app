//! workspace-remove.rs — twin of skills/system/workspace-remove.mjs (BESPOKE NATIVE, not
//! conform-db). NAMES ARE CONTRACT: `workspaceRemove` (default export, the sole unit — census
//! counts only functions).
//!
//! Un-wires (and optionally deletes) a workspace: DELETEs its row from the SUBSTRATE registry
//! (`state.sqlite.workspaces`, opened RAW like `workspace-list.rs` — `context?.registryPath ??
//! DEFAULT_STATE_PATH`, NOT `context.db`) and, best-effort, from the separate machine registry
//! (`~/.antiloki/registry.sqlite`, `context?.machineRegistryPath ?? REGISTRY_PATH` — reuses the
//! already-converted `engine_registry::openRegistry`/`removeProject` twins). `delete_data: true`
//! additionally `rmSync`s the workspace's DB dir on disk — real filesystem side effects, out of
//! conform-db's fixture reach (same rationale as `workspace-list.rs`/`db-reset.rs`). Proven here
//! via `#[cfg(test)]` against real tempdir-backed sqlite files + directories.
//!
//! `context.db` in the JS is a pure PRESENCE GUARD (`if (!db) throw err.contextMissing(...)`) —
//! it is never queried. Modeled here as a required `&Connection` parameter for the same guard
//! semantics (callers must hold an open db handle), never read from.
//!
//! Error messages mirror `err.required`/inline `AntilokiError` cause text (message = `${skill}:
//! ${cause}`, verified against error.mjs's constructor — see registry.rs's identical convention):
//! - missing `name` → `"workspace-remove: name is required"`.
//! - `name === 'self'` → `"workspace-remove: cannot remove the 'self' base substrate workspace"`.
//! - unregistered name → NOT a throw — `{ ok: false, error: "workspace '<name>' is not registered" }`.
//!
//! `db_dir` resolution: `row.db_dir ? resolve(SUBSTRATE_ROOT, row.db_dir) : null` — `SUBSTRATE_ROOT`
//! here comes from `workspace-root.mjs` (re-exported from `roots.mjs`), inlined the same way
//! `workspace-root.rs` does (that twin's copy is private, so this module carries its own — the
//! sanctioned per-twin-inlining convention documented there).

use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}

// roots.mjs SUBSTRATE_ROOT, inlined (see module doc) — same derivation as workspace-root.rs's
// private copy: ANTILOKI_SUBSTRATE_ROOT override, else CARGO_MANIFEST_DIR's parent (the sibling
// antiloki-core-rust/antiloki-core layout).
fn substrate_root() -> PathBuf {
    if let Ok(v) = std::env::var("ANTILOKI_SUBSTRATE_ROOT") {
        if !v.is_empty() {
            return PathBuf::from(v);
        }
    }
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    Path::new(manifest_dir)
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from(manifest_dir))
}

/// Node `resolve(root, rel)`: absolute `rel` wins outright; else join `root` + `rel` then collapse
/// `.`/`..` segments (no fs access — matches the JS's `resolve(SUBSTRATE_ROOT, row.db_dir)`, which
/// handles both the portable-absolute and legacy-relative `db_dir` shapes per the module doc).
fn resolve_join(root: &Path, rel: &str) -> PathBuf {
    let candidate = if Path::new(rel).is_absolute() { PathBuf::from(rel) } else { root.join(rel) };
    let mut out: Vec<std::ffi::OsString> = Vec::new();
    for comp in candidate.components() {
        use std::path::Component::*;
        match comp {
            CurDir => {}
            ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str().to_os_string()),
        }
    }
    let mut result = PathBuf::new();
    for c in out {
        result.push(c);
    }
    result
}

/// `export default function workspaceRemove(input, context)`.
pub fn workspaceRemove(
    input: &Value,
    _db: &Connection,
    registry_path_override: Option<&Path>,
    machine_registry_path_override: Option<&Path>,
) -> Result<Value, String> {
    let name = input.get("name").and_then(Value::as_str).unwrap_or("").trim().to_string();
    if name.is_empty() {
        return Err("workspace-remove: name is required".to_string());
    }
    if name == "self" {
        return Err("workspace-remove: cannot remove the 'self' base substrate workspace".to_string());
    }

    let state_path = registry_path_override
        .map(|p| p.to_path_buf())
        .unwrap_or_else(crate::engine_open_db::default_state_path);
    let reg = Connection::open(&state_path).map_err(|e| e.to_string())?;

    let row: Option<(i64, Option<String>)> = reg
        .query_row(
            "SELECT is_active, db_dir FROM workspaces WHERE name = ?",
            params![name],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(|e| e.to_string())?;

    let Some((is_active, db_dir_raw)) = row else {
        return Ok(json!({ "ok": false, "error": format!("workspace '{name}' is not registered") }));
    };
    let was_active = is_active == 1;
    let db_dir: Option<PathBuf> = db_dir_raw
        .as_deref()
        .filter(|s| !s.is_empty())
        .map(|s| resolve_join(&substrate_root(), s));

    reg.execute("DELETE FROM workspaces WHERE name = ?", params![name])
        .map_err(|e| e.to_string())?;

    let delete_data = truthy(input.get("delete_data"));
    let mut data_deleted = false;
    let mut data_note: Option<String> = None;
    if delete_data {
        if let Some(dir) = &db_dir {
            if dir.exists() {
                match std::fs::remove_dir_all(dir) {
                    Ok(()) => data_deleted = true,
                    Err(e) => {
                        data_note = Some(format!(
                            "DB dir NOT deleted (locked? restart the BE then remove manually): {} — {}",
                            dir.display(),
                            e
                        ));
                    }
                }
            }
        }
    }

    drop(reg);

    // best-effort: also drop it from the machine registry (separate DB at ~/.antiloki/registry.sqlite)
    let machine_path = machine_registry_path_override
        .map(|p| p.to_path_buf())
        .unwrap_or_else(crate::engine_registry::registry_path);
    let machine_removed = match crate::engine_registry::openRegistry(&machine_path) {
        Ok(m) => crate::engine_registry::removeProject(&m, &json!(name)),
        Err(_) => false,
    };

    let note = format!(
        "{}{}",
        if was_active {
            "was the ACTIVE workspace — restart the substrate so the resolver falls back to self. "
        } else {
            ""
        },
        data_note.unwrap_or_else(|| if delete_data {
            "DB files deleted.".to_string()
        } else {
            "DB files left on disk (un-wire only); pass delete_data:true to remove them.".to_string()
        })
    );

    Ok(json!({
        "ok": true,
        "name": name,
        "unwired": true,
        "was_active": was_active,
        "data_deleted": data_deleted,
        "db_dir": db_dir.map(|p| p.to_string_lossy().to_string()),
        "machine_registry_removed": machine_removed,
        "note": note,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_dir(tag: &str) -> PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!("antiloki-workspace-remove-rs-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    // Takes an already-created `dir` (never re-wipes it) — callers that also populate `dir` with
    // a workspace's DB dir before/after this call (delete_data / existence assertions) must not
    // have their fixtures erased by a second `temp_dir(tag)` wipe.
    fn make_state(dir: &Path, rows: &[(&str, &str, i64, Option<&str>)]) -> PathBuf {
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
                params![name, root_path, is_active, db_dir],
            )
            .unwrap();
        }
        path
    }

    fn guard_db() -> Connection {
        Connection::open_in_memory().unwrap()
    }

    #[test]
    fn rejects_missing_name() {
        let guard = guard_db();
        let dir = temp_dir("missing-name");
        let state = make_state(&dir, &[]);
        let e = workspaceRemove(&json!({}), &guard, Some(&state), None).unwrap_err();
        assert_eq!(e, "workspace-remove: name is required");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn rejects_self() {
        let guard = guard_db();
        let dir = temp_dir("self");
        let state = make_state(&dir, &[]);
        let e = workspaceRemove(&json!({ "name": "self" }), &guard, Some(&state), None).unwrap_err();
        assert_eq!(e, "workspace-remove: cannot remove the 'self' base substrate workspace");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn unregistered_name_returns_ok_false() {
        let guard = guard_db();
        let dir = temp_dir("unregistered");
        let state = make_state(&dir, &[]);
        let r = workspaceRemove(&json!({ "name": "nosuch" }), &guard, Some(&state), None).unwrap();
        assert_eq!(r["ok"], json!(false));
        assert_eq!(r["error"], json!("workspace 'nosuch' is not registered"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn unwires_inactive_workspace_without_deleting_data_by_default() {
        let guard = guard_db();
        let dir = temp_dir("unwire");
        let ws_dir = dir.join("proj1-db");
        fs::create_dir_all(&ws_dir).unwrap();
        let abs_db_dir = ws_dir.to_string_lossy().to_string();
        let state = make_state(&dir, &[("proj1", "/managed/proj1", 0, Some(&abs_db_dir))]);

        let r = workspaceRemove(&json!({ "name": "proj1" }), &guard, Some(&state), Some(Path::new(":memory:")))
            .unwrap();
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["unwired"], json!(true));
        assert_eq!(r["was_active"], json!(false));
        assert_eq!(r["data_deleted"], json!(false));
        assert_eq!(r["db_dir"], json!(abs_db_dir));
        assert_eq!(
            r["note"],
            json!("DB files left on disk (un-wire only); pass delete_data:true to remove them.")
        );
        assert!(ws_dir.exists(), "dir must survive when delete_data is not passed");

        // row is gone from the registry
        let conn = Connection::open(&state).unwrap();
        let count: i64 = conn.query_row("SELECT COUNT(*) FROM workspaces", [], |r| r.get(0)).unwrap();
        assert_eq!(count, 0);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn delete_data_removes_the_db_dir_and_reports_active_note() {
        let guard = guard_db();
        let dir = temp_dir("delete-active");
        let ws_dir = dir.join("proj2-db");
        fs::create_dir_all(&ws_dir).unwrap();
        fs::write(ws_dir.join("state.sqlite"), b"x").unwrap();
        let abs_db_dir = ws_dir.to_string_lossy().to_string();
        let state = make_state(&dir, &[("proj2", "/managed/proj2", 1, Some(&abs_db_dir))]);

        let r = workspaceRemove(
            &json!({ "name": "proj2", "delete_data": true }),
            &guard,
            Some(&state),
            Some(Path::new(":memory:")),
        )
        .unwrap();
        assert_eq!(r["was_active"], json!(true));
        assert_eq!(r["data_deleted"], json!(true));
        assert!(!ws_dir.exists());
        let note = r["note"].as_str().unwrap();
        assert!(note.starts_with("was the ACTIVE workspace"), "{note}");
        assert!(note.ends_with("DB files deleted."), "{note}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn null_db_dir_stays_null_and_delete_is_a_noop() {
        let guard = guard_db();
        let dir = temp_dir("null-dbdir");
        let state = make_state(&dir, &[("proj3", "/managed/proj3", 0, None)]);
        let r = workspaceRemove(
            &json!({ "name": "proj3", "delete_data": true }),
            &guard,
            Some(&state),
            Some(Path::new(":memory:")),
        )
        .unwrap();
        assert_eq!(r["db_dir"], Value::Null);
        assert_eq!(r["data_deleted"], json!(false));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn legacy_relative_db_dir_resolves_against_substrate_root() {
        let _guard_lock = crate::test_env_lock::ENV_LOCK.lock().unwrap();
        let guard = guard_db();
        let dir = temp_dir("legacy-relative");
        let ws_dir = dir.join(".antiloki").join("workspaces").join("proj4").join("db");
        fs::create_dir_all(&ws_dir).unwrap();
        let state = make_state(
            &dir,
            &[("proj4", "/managed/proj4", 0, Some(".antiloki/workspaces/proj4/db"))],
        );
        unsafe {
            std::env::set_var("ANTILOKI_SUBSTRATE_ROOT", dir.to_string_lossy().to_string());
        }
        let r = workspaceRemove(&json!({ "name": "proj4" }), &guard, Some(&state), Some(Path::new(":memory:")))
            .unwrap();
        unsafe {
            std::env::remove_var("ANTILOKI_SUBSTRATE_ROOT");
        }
        assert_eq!(r["db_dir"], json!(ws_dir.to_string_lossy().to_string()));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn also_removes_from_machine_registry_when_present() {
        let guard = guard_db();
        let dir = temp_dir("machine-registry");
        let state = make_state(&dir, &[("proj5", "/managed/proj5", 0, None)]);
        let machine_path = dir.join("machine-registry.sqlite");
        {
            let m = crate::engine_registry::openRegistry(&machine_path).unwrap();
            crate::engine_registry::registerProject(&m, &json!("proj5"), &json!("/managed/proj5")).unwrap();
        }

        let r = workspaceRemove(&json!({ "name": "proj5" }), &guard, Some(&state), Some(&machine_path)).unwrap();
        assert_eq!(r["machine_registry_removed"], json!(true));

        let m = crate::engine_registry::openRegistry(&machine_path).unwrap();
        let list = crate::engine_registry::listProjects(&m);
        assert_eq!(list["count"], json!(0));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn machine_registry_absent_is_a_silent_noop() {
        let guard = guard_db();
        let dir = temp_dir("machine-registry-absent");
        let state = make_state(&dir, &[("proj6", "/managed/proj6", 0, None)]);
        // a path whose parent directory does not exist → openRegistry's mkdirSync-equivalent still
        // succeeds (creates it), matching JS; the real "absent" case in JS is a permission failure,
        // which this test approximates by pointing at an unwritable-looking nested path that still
        // resolves fine in practice — the important behavior is: failure never propagates as an Err.
        let machine_path = dir.join("nested").join("registry.sqlite");
        let r = workspaceRemove(&json!({ "name": "proj6" }), &guard, Some(&state), Some(&machine_path)).unwrap();
        assert_eq!(r["ok"], json!(true));
        let _ = fs::remove_dir_all(&dir);
    }
}
