//! workspace-activate.rs — twin of skills/system/workspace-activate.mjs (BESPOKE NATIVE, not
//! conform-db; see rationale below, same class as the sibling `workspace-list.rs`). NAMES ARE
//! CONTRACT: `workspaceActivate` (default export, the sole unit — census counts only functions).
//!
//! Flips `is_active` in the substrate STATE registry — deliberately NOT `context.db`. Once a
//! non-self workspace is active, `openDb` redirects `context.db` to that workspace's own DB set
//! (carrying only its single self-aware row), so the switcher opens a RAW second connection
//! straight at the registry file (`new DatabaseSync(context?.registryPath ?? DEFAULT_STATE_PATH)`)
//! to see/flip every workspace regardless of which one is currently mounted — the substrate
//! registry is the single source of truth the `openDb` resolver reads at boot.
//!
//! BESPOKE NATIVE TEST (not conform-db), same rationale as `workspace-list.rs`/`db-reset.rs`'s
//! override parameters: `context.registryPath` IS a genuine caller-injectable override in the JS
//! itself (proven by the sibling `workspace-list.test.mjs`/`workspace-remove.test.mjs`, which point
//! `context.db` at an UNRELATED db and pass `registryPath` separately to prove the skill reads the
//! registry file and ignores `context.db`) — so this twin takes it as a plain `Option<&Path>`
//! parameter rather than the env-var + tempdir indirection the `god-driver-*` precedents needed.
//! But the conform-db harness only ever seeds/inspects ONE connection (`context.db`); this skill
//! deliberately opens a SECOND, independent connection at a real file path, which the harness has
//! no seam for — the playbook's cross-db VET note says exactly this: "still block if the skill
//! OPENS a db file itself ... the lane only covers qualified SQL through `context.db`". Tested here
//! instead via `#[cfg(test)]` Rust-native tests that pass a real temp-dir SQLite file path directly
//! as the override.
//!
//! `context.db` presence check (`if (!db) throw err.contextMissing(...)`) is a guard-throw over a
//! parameter the real logic never reads (same "guard-throws are not exercised" convention as every
//! sibling twin) — dropped entirely here along with the `db` parameter itself, same as
//! `workspace-list.rs`.
//!
//! `DEFAULT_STATE_PATH` (roots.mjs `DB_PATHS.state` = `SUBSTRATE_ROOT/.antiloki/db/state.sqlite`)
//! has no exact analog in a compiled Rust binary — same caveat as `workspace-list.rs`'s
//! `default_state_path`; used ONLY when no override is passed (every test below passes one).
//!
//! Output key order matches the JS literal `{ ok, active, root_path, db_dir, self_hosting, note }`;
//! `db_dir` stays `NULL`/`None` verbatim (never coalesced) — `self_hosting` is `db_dir == null`.

use rusqlite::{Connection, OptionalExtension};
use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};

// Same no-exact-JS-analog fallback as workspace-list.rs's default_state_path — used ONLY when no
// registry_path_override is passed (every test below passes one explicitly).
// Delegates to the crate's ONE resolver. This module used to carry its own copy that
// consulted only ANTILOKI_SUBSTRATE_ROOT and otherwise fell back to the process CWD —
// so under the BE (cwd = antiloki-BE) it opened a state.sqlite that does not exist and
// every /api/v1/workspaces/list returned 500 on the Rust engine while JS answered fine.
// The JS twin reads DEFAULT_STATE_PATH from open-db, which honours ANTILOKI_DATA_DIR;
// engine_open_db::default_state_path() is that same rule, in one place.
fn default_state_path() -> PathBuf {
    crate::engine_open_db::default_state_path()
}

pub fn workspaceActivate(input: &Value, registry_path_override: Option<&Path>) -> Result<Value, String> {
    let name = input.get("name").and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty());
    let name = match name {
        Some(n) => n,
        None => return Err("workspace-activate: name is required".into()),
    };

    let path = registry_path_override.map(Path::to_path_buf).unwrap_or_else(default_state_path);
    let reg = Connection::open(&path).map_err(|e| e.to_string())?;

    let target: Option<(String, Option<String>)> = reg
        .query_row(
            "SELECT root_path, db_dir FROM workspaces WHERE name = ?1",
            rusqlite::params![name],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(|e| e.to_string())?;

    let (root_path, db_dir) = match target {
        Some(t) => t,
        None => return Err(format!("workspace-activate: workspace '{name}' is not registered")),
    };

    // A FOLDER THAT IS GONE CANNOT BE OPENED (operator 2026-10-01: "ministore-2" went to the Trash and picking it failed far from the cause). Said before anything is flipped, in a
    // sentence that names the folder; the row is left for the operator to remove.
    if crate::system_workspace_list::folder_missing(&root_path) {
        return Err(format!("workspace-activate: the folder of workspace '{name}' is missing on disk: '{root_path}' - it was moved or deleted. Put it back, or remove the workspace from the list"));
    }

    reg.execute("UPDATE workspaces SET is_active = 0 WHERE is_active = 1", [])
        .map_err(|e| e.to_string())?;
    reg.execute("UPDATE workspaces SET is_active = 1 WHERE name = ?1", rusqlite::params![name])
        .map_err(|e| e.to_string())?;

    let mut out = Map::new();
    out.insert("ok".into(), json!(true));
    out.insert("active".into(), json!(name));
    out.insert("root_path".into(), json!(root_path));
    out.insert("db_dir".into(), db_dir.clone().map_or(Value::Null, |s| json!(s)));
    out.insert("self_hosting".into(), json!(db_dir.is_none()));
    out.insert(
        "note".into(),
        json!("active workspace flipped in the registry; the BE re-mounts its DB set live (reopenDb) — no restart needed"),
    );
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_dir(tag: &str) -> PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!("antiloki-workspace-activate-rs-{tag}-{}", std::process::id()));
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

    fn is_active_of(path: &Path, name: &str) -> i64 {
        let conn = Connection::open(path).unwrap();
        conn.query_row("SELECT is_active FROM workspaces WHERE name = ?1", rusqlite::params![name], |r| r.get(0))
            .unwrap()
    }

    // a registered folder that really exists (activation refuses a missing one)
    fn real_dir(tag: &str) -> String { temp_dir(&format!("root-{tag}")).to_string_lossy().into_owned() }

    #[test]
    fn activates_target_and_deactivates_previous() {
        let proj1 = real_dir("flip");
        let sub = real_dir("flip-self");
        let path = make_registry(
            "flip",
            &[("self", &sub, 1, None), ("proj1", &proj1, 0, Some("proj1/db"))],
        );
        let r = workspaceActivate(&json!({ "name": "proj1" }), Some(&path)).unwrap();
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["active"], json!("proj1"));
        assert_eq!(r["root_path"], json!(proj1));
        assert_eq!(r["db_dir"], json!("proj1/db"));
        assert_eq!(r["self_hosting"], json!(false));
        assert!(r["note"].as_str().unwrap().contains("reopenDb"));
        let keys: Vec<&String> = r.as_object().unwrap().keys().collect();
        assert_eq!(keys, ["ok", "active", "root_path", "db_dir", "self_hosting", "note"]);

        assert_eq!(is_active_of(&path, "proj1"), 1);
        assert_eq!(is_active_of(&path, "self"), 0);
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn self_hosting_true_when_target_db_dir_is_null() {
        let sub = real_dir("self-hosting");
        let path = make_registry("self-hosting", &[("self", &sub, 0, None)]);
        let r = workspaceActivate(&json!({ "name": "self" }), Some(&path)).unwrap();
        assert_eq!(r["self_hosting"], json!(true));
        assert_eq!(r["db_dir"], Value::Null);
        assert_eq!(is_active_of(&path, "self"), 1);
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn unregistered_name_errs_not_registered() {
        let path = make_registry("missing", &[("self", "/substrate", 1, None)]);
        let e = workspaceActivate(&json!({ "name": "ghost" }), Some(&path)).unwrap_err();
        assert!(e.contains("ghost"), "{e}");
        assert!(e.contains("not registered"), "{e}");
        // untouched: no row was flipped by the failed lookup
        assert_eq!(is_active_of(&path, "self"), 1);
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn missing_name_errs_required() {
        let path = make_registry("noname", &[("self", "/substrate", 1, None)]);
        let e = workspaceActivate(&json!({}), Some(&path)).unwrap_err();
        assert!(e.contains("name is required"), "{e}");
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn name_is_trimmed() {
        let (sub, p1) = (real_dir("trim-self"), real_dir("trim"));
        let path = make_registry(
            "trim",
            &[("self", &sub, 1, None), ("proj1", &p1, 0, None)],
        );
        let r = workspaceActivate(&json!({ "name": "  proj1  " }), Some(&path)).unwrap();
        assert_eq!(r["active"], json!("proj1"));
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn blank_name_errs_required() {
        let path = make_registry("blank", &[("self", "/substrate", 1, None)]);
        let e = workspaceActivate(&json!({ "name": "   " }), Some(&path)).unwrap_err();
        assert!(e.contains("name is required"), "{e}");
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    // OPERATOR 2026-10-01: "ministore-2" was moved to the Trash; picking it failed far from the cause. Now it fails here, in a sentence that names the folder, and flips nothing.
    #[test]
    fn a_workspace_whose_folder_is_missing_cannot_be_activated_and_nothing_is_flipped_or_deleted() {
        let sub = real_dir("gone-self");
        let gone = temp_dir("gone-dir"); fs::remove_dir_all(&gone).unwrap();
        let path = make_registry("gone", &[("self", &sub, 1, None), ("ministore-2", gone.to_str().unwrap(), 0, Some("m/db"))]);
        let e = workspaceActivate(&json!({ "name": "ministore-2" }), Some(&path)).unwrap_err();
        assert!(e.contains("ministore-2") && e.contains("missing on disk") && e.contains(gone.to_str().unwrap()), "{e}");
        assert_eq!(is_active_of(&path, "self"), 1, "the active workspace stays active");
        assert_eq!(is_active_of(&path, "ministore-2"), 0);
        let rows: i64 = Connection::open(&path).unwrap().query_row("SELECT COUNT(*) FROM workspaces", [], |r| r.get(0)).unwrap();
        assert_eq!(rows, 2, "the registry row is the operator's to remove");
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }
}
