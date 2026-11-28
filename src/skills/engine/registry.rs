//! registry.rs — twin of skills/engine/registry.mjs. NAMES ARE CONTRACT: `REGISTRY_PATH` (data,
//! modeled as `registry_path()`), `openRegistry`, `resolveActiveProjectDataDir`, `registerProject`,
//! `listProjects`, `activateProject`, `deactivateProjects`, `removeProject`.
//!
//! The machine-level PROJECT registry (`~/.antiloki/registry.sqlite`), BE-owned — a standalone
//! SQLite at HOME_DIR (roots.mjs), opened RAW (not ATTACHed to the operational 9-db set). Real
//! sqlite-file open/create + real CRUD against a real on-disk (or `:memory:`) database: genuinely
//! non-deterministic (filesystem side effects) and out of conform-db's fixture reach. BESPOKE
//! NATIVE TESTING only (see manifest: no fixture, no frontier entry), proven below via
//! `#[cfg(test)]` against real tempdir-backed sqlite files, reusing the `with_home()`-style
//! env-var-injection convention from auth-init.rs / auth-sign.rs (here the registry path is passed
//! explicitly as an arg rather than read from ANTILOKI_HOME, mirroring the JS optional-param
//! default — `roots.mjs`'s HOME_DIR resolution is reproduced only in `registry_path()`, the twin
//! of the module-level `REGISTRY_PATH` constant).
//!
//! `:memory:` is honored for tests exactly like the JS (`registryPath !== ':memory:'` guards),
//! matching the source's own test-affordance.
//!
//! Throw fidelity (verified by reading registry.mjs's own throw sites — no JS test file exists for
//! this module to run against, so these are derived directly from err.required / the inline
//! `AntilokiError` construction, matching the message shape used elsewhere in this crate for
//! `err.required` / ALREADY_EXISTS, e.g. auth-sign.rs's `"<skill>: <field> is required"`):
//! - missing `name` → `"registry: name is required"`.
//! - missing `root_path` → `"registry: root_path is required"`.
//! - name already used by a DIFFERENT root_path → `"registry: name '<nm>' already used by another project"`.

use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

#[cfg(windows)]
fn homedir() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("USERPROFILE") {
        if !p.is_empty() {
            return Some(PathBuf::from(p));
        }
    }
    let drive = std::env::var("HOMEDRIVE").ok().filter(|s| !s.is_empty());
    let path = std::env::var("HOMEPATH").ok().filter(|s| !s.is_empty());
    match (drive, path) {
        (Some(d), Some(p)) => Some(PathBuf::from(format!("{d}{p}"))),
        _ => None,
    }
}

#[cfg(not(windows))]
fn homedir() -> Option<PathBuf> {
    std::env::var("HOME").ok().filter(|s| !s.is_empty()).map(PathBuf::from)
}

/// roots.mjs: HOME_DIR = resolve($ANTILOKI_HOME) if set+truthy, else join(homedir(), '.antiloki').
/// REGISTRY_PATH = join(HOME_DIR, 'registry.sqlite').
pub fn registry_path() -> PathBuf {
    let home_dir = match std::env::var("ANTILOKI_HOME") {
        Ok(v) if !v.is_empty() => PathBuf::from(v),
        _ => homedir().unwrap_or_default().join(".antiloki"),
    };
    home_dir.join("registry.sqlite")
}

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS projects (
  id             INTEGER PRIMARY KEY AUTOINCREMENT,
  name           TEXT    NOT NULL UNIQUE,
  root_path      TEXT    NOT NULL UNIQUE,
  is_active      INTEGER NOT NULL DEFAULT 0,
  created_at     TEXT    NOT NULL DEFAULT (datetime('now')),
  last_opened_at TEXT
);";

/// `export function openRegistry(registryPath = REGISTRY_PATH)` — creating + schema-ensuring.
/// `':memory:'` is honored for tests.
pub fn openRegistry(registry_path: &Path) -> Result<Connection, String> {
    let path_str = registry_path.to_string_lossy();
    if path_str != ":memory:" {
        if let Some(parent) = registry_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
    }
    let db = if path_str == ":memory:" {
        Connection::open_in_memory().map_err(|e| e.to_string())?
    } else {
        Connection::open(registry_path).map_err(|e| e.to_string())?
    };
    db.execute_batch(SCHEMA).map_err(|e| e.to_string())?;
    Ok(db)
}

fn project_row(reg: &Connection, id: i64) -> Value {
    reg.query_row(
        "SELECT id, name, root_path, is_active, created_at, last_opened_at FROM projects WHERE id = ?",
        params![id],
        row_to_value,
    )
    .unwrap_or(Value::Null)
}

fn row_to_value(row: &rusqlite::Row) -> rusqlite::Result<Value> {
    Ok(json!({
        "id": row.get::<_, i64>(0)?,
        "name": row.get::<_, String>(1)?,
        "root_path": row.get::<_, String>(2)?,
        "is_active": row.get::<_, i64>(3)?,
        "created_at": row.get::<_, String>(4)?,
        "last_opened_at": row.get::<_, Option<String>>(5)?,
    }))
}

/// `export function resolveActiveProjectDataDir(registryPath = REGISTRY_PATH)` — the active
/// project's substrate dir (`<root_path>/.antiloki/db`), or `None`. Fully guarded: no registry
/// file, no active project, or a missing `state.sqlite` all resolve to `None` (openDb's fallback
/// path). Opens the registry RAW (no re-entrancy through openDb's own resolver).
pub fn resolveActiveProjectDataDir(registry_path: &Path) -> Option<PathBuf> {
    let path_str = registry_path.to_string_lossy();
    if path_str != ":memory:" && !registry_path.exists() {
        return None;
    }
    let reg = Connection::open(registry_path).ok()?;
    let root_path: Option<String> = reg
        .query_row(
            "SELECT root_path FROM projects WHERE is_active = 1 LIMIT 1",
            [],
            |row| row.get(0),
        )
        .optional()
        .ok()
        .flatten();
    let root_path = root_path?;
    if root_path.is_empty() {
        return None;
    }
    let dir = Path::new(&root_path).join(".antiloki").join("db");
    if !dir.join("state.sqlite").exists() {
        return None;
    }
    Some(dir)
}

/// `export function registerProject(reg, { name, root_path })` — register (or relabel an
/// already-registered folder). Identity = `root_path`.
pub fn registerProject(reg: &Connection, name: &Value, root_path: &Value) -> Result<Value, String> {
    let nm = name.as_str().unwrap_or("").trim().to_string();
    let rp_raw = root_path.as_str().unwrap_or("").trim().to_string();
    if nm.is_empty() {
        return Err("registry: name is required".to_string());
    }
    if rp_raw.is_empty() {
        return Err("registry: root_path is required".to_string());
    }
    // JS `resolve(String(root_path))` normalizes to an absolute, `.`/`..`-collapsed path against
    // process.cwd(); mirrored here via std::path canonicalization semantics without touching the
    // filesystem (resolve does not require the path to exist).
    let rp = resolve_path(&rp_raw);

    let name_clash = |nm: &str| -> String {
        format!("registry: name '{nm}' already used by another project")
    };

    let by_path: Option<(i64, String)> = reg
        .query_row(
            "SELECT id, name FROM projects WHERE root_path = ?",
            params![rp],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(|e| e.to_string())?;

    if let Some((id, existing_name)) = by_path {
        if existing_name != nm {
            let clash: Option<i64> = reg
                .query_row(
                    "SELECT id FROM projects WHERE name = ? AND id != ?",
                    params![nm, id],
                    |row| row.get(0),
                )
                .optional()
                .map_err(|e| e.to_string())?;
            if clash.is_some() {
                return Err(name_clash(&nm));
            }
            reg.execute("UPDATE projects SET name = ? WHERE id = ?", params![nm, id])
                .map_err(|e| e.to_string())?;
        }
        return Ok(project_row(reg, id));
    }

    let name_exists: Option<i64> = reg
        .query_row("SELECT id FROM projects WHERE name = ?", params![nm], |row| row.get(0))
        .optional()
        .map_err(|e| e.to_string())?;
    if name_exists.is_some() {
        return Err(name_clash(&nm));
    }

    reg.execute(
        "INSERT INTO projects (name, root_path, is_active) VALUES (?, ?, 0)",
        params![nm, rp],
    )
    .map_err(|e| e.to_string())?;
    let id = reg.last_insert_rowid();
    Ok(project_row(reg, id))
}

/// Node `path.resolve(p)`: absolute against cwd, with `.`/`..` segments collapsed — no fs access.
fn resolve_path(p: &str) -> String {
    let candidate = if Path::new(p).is_absolute() {
        PathBuf::from(p)
    } else {
        std::env::current_dir().unwrap_or_default().join(p)
    };
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
    let s = result.to_string_lossy().to_string();
    if s.is_empty() { "/".to_string() } else { s }
}

/// `export function listProjects(reg)`.
pub fn listProjects(reg: &Connection) -> Value {
    let mut stmt = reg
        .prepare("SELECT id, name, root_path, is_active, created_at, last_opened_at FROM projects ORDER BY id ASC")
        .expect("listProjects: prepare");
    let projects: Vec<Value> = stmt
        .query_map([], row_to_value)
        .expect("listProjects: query")
        .filter_map(Result::ok)
        .collect();
    let active = projects
        .iter()
        .find(|p| p["is_active"] == json!(1))
        .cloned()
        .unwrap_or(Value::Null);
    let count = projects.len() as i64;
    json!({ "projects": projects, "active": active, "count": count })
}

/// `export function activateProject(reg, name)` — returns the activated row, or `null` if the
/// name is not registered.
pub fn activateProject(reg: &Connection, name: &Value) -> Value {
    let nm = name.as_str().unwrap_or("").trim().to_string();
    let target: Option<i64> = reg
        .query_row("SELECT id FROM projects WHERE name = ?", params![nm], |row| row.get(0))
        .optional()
        .expect("activateProject: query");
    let Some(id) = target else { return Value::Null };
    reg.execute("UPDATE projects SET is_active = 0 WHERE is_active = 1", [])
        .expect("activateProject: deactivate");
    reg.execute(
        "UPDATE projects SET is_active = 1, last_opened_at = datetime('now') WHERE id = ?",
        params![id],
    )
    .expect("activateProject: activate");
    project_row(reg, id)
}

/// `export function deactivateProjects(reg)`.
pub fn deactivateProjects(reg: &Connection) {
    reg.execute("UPDATE projects SET is_active = 0 WHERE is_active = 1", [])
        .expect("deactivateProjects: update");
}

/// `export function removeProject(reg, name)` — `true` if a row was deleted.
pub fn removeProject(reg: &Connection, name: &Value) -> bool {
    let nm = name.as_str().unwrap_or("").trim().to_string();
    reg.execute("DELETE FROM projects WHERE name = ?", params![nm])
        .expect("removeProject: delete")
        > 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_dir(tag: &str) -> PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!("antiloki-registry-rs-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn open_registry_creates_schema_on_disk() {
        let dir = temp_dir("open-disk");
        let path = dir.join("nested").join("registry.sqlite");
        let reg = openRegistry(&path).unwrap();
        assert!(path.exists());
        let list = listProjects(&reg);
        assert_eq!(list["count"], json!(0));
        assert_eq!(list["projects"], json!([]));
        assert_eq!(list["active"], Value::Null);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn open_registry_honors_in_memory() {
        let reg = openRegistry(Path::new(":memory:")).unwrap();
        let list = listProjects(&reg);
        assert_eq!(list["count"], json!(0));
    }

    #[test]
    fn register_project_requires_name_and_root_path() {
        let reg = openRegistry(Path::new(":memory:")).unwrap();
        let e = registerProject(&reg, &Value::Null, &json!("/tmp/x")).unwrap_err();
        assert_eq!(e, "registry: name is required");
        let e = registerProject(&reg, &json!("proj"), &Value::Null).unwrap_err();
        assert_eq!(e, "registry: root_path is required");
    }

    #[test]
    fn register_project_inserts_then_relabels_by_root_path_identity() {
        let reg = openRegistry(Path::new(":memory:")).unwrap();
        let dir = temp_dir("register");
        let root = dir.to_string_lossy().to_string();
        let p1 = registerProject(&reg, &json!("proj-a"), &json!(root.clone())).unwrap();
        assert_eq!(p1["name"], json!("proj-a"));
        assert_eq!(p1["is_active"], json!(0));

        // Re-register same root_path under a NEW name → identity is root_path, so this RELABELS.
        let p2 = registerProject(&reg, &json!("proj-a-renamed"), &json!(root.clone())).unwrap();
        assert_eq!(p2["id"], p1["id"]);
        assert_eq!(p2["name"], json!("proj-a-renamed"));

        let list = listProjects(&reg);
        assert_eq!(list["count"], json!(1));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn register_project_rejects_name_clash_across_different_roots() {
        let reg = openRegistry(Path::new(":memory:")).unwrap();
        let dir_a = temp_dir("clash-a");
        let dir_b = temp_dir("clash-b");
        registerProject(&reg, &json!("shared-name"), &json!(dir_a.to_string_lossy().to_string())).unwrap();
        let e = registerProject(&reg, &json!("shared-name"), &json!(dir_b.to_string_lossy().to_string()))
            .unwrap_err();
        assert_eq!(e, "registry: name 'shared-name' already used by another project");
        let _ = fs::remove_dir_all(&dir_a);
        let _ = fs::remove_dir_all(&dir_b);
    }

    #[test]
    fn activate_project_flips_is_active_exclusively() {
        let reg = openRegistry(Path::new(":memory:")).unwrap();
        let dir_a = temp_dir("activate-a");
        let dir_b = temp_dir("activate-b");
        registerProject(&reg, &json!("a"), &json!(dir_a.to_string_lossy().to_string())).unwrap();
        registerProject(&reg, &json!("b"), &json!(dir_b.to_string_lossy().to_string())).unwrap();

        let activated = activateProject(&reg, &json!("a"));
        assert_eq!(activated["name"], json!("a"));
        assert_eq!(activated["is_active"], json!(1));
        assert!(activated["last_opened_at"].is_string());

        let list = listProjects(&reg);
        assert_eq!(list["active"]["name"], json!("a"));

        // Activating b flips a back off (exclusive activation).
        activateProject(&reg, &json!("b"));
        let list = listProjects(&reg);
        assert_eq!(list["active"]["name"], json!("b"));
        assert_eq!(list["projects"].as_array().unwrap().len(), 2);

        let _ = fs::remove_dir_all(&dir_a);
        let _ = fs::remove_dir_all(&dir_b);
    }

    #[test]
    fn activate_project_unknown_name_returns_null() {
        let reg = openRegistry(Path::new(":memory:")).unwrap();
        let r = activateProject(&reg, &json!("nosuch"));
        assert_eq!(r, Value::Null);
    }

    #[test]
    fn deactivate_projects_clears_active_flag() {
        let reg = openRegistry(Path::new(":memory:")).unwrap();
        let dir = temp_dir("deactivate");
        registerProject(&reg, &json!("only"), &json!(dir.to_string_lossy().to_string())).unwrap();
        activateProject(&reg, &json!("only"));
        assert_eq!(listProjects(&reg)["active"]["name"], json!("only"));
        deactivateProjects(&reg);
        assert_eq!(listProjects(&reg)["active"], Value::Null);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn remove_project_deletes_and_reports_bool() {
        let reg = openRegistry(Path::new(":memory:")).unwrap();
        let dir = temp_dir("remove");
        registerProject(&reg, &json!("gone"), &json!(dir.to_string_lossy().to_string())).unwrap();
        assert!(removeProject(&reg, &json!("gone")));
        assert!(!removeProject(&reg, &json!("gone")));
        assert_eq!(listProjects(&reg)["count"], json!(0));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_active_project_data_dir_null_when_no_registry_file() {
        let dir = temp_dir("resolve-missing");
        let path = dir.join("registry.sqlite");
        assert_eq!(resolveActiveProjectDataDir(&path), None);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_active_project_data_dir_null_when_no_active_project() {
        let dir = temp_dir("resolve-inactive");
        let path = dir.join("registry.sqlite");
        let reg = openRegistry(&path).unwrap();
        registerProject(&reg, &json!("p"), &json!(dir.to_string_lossy().to_string())).unwrap();
        drop(reg);
        assert_eq!(resolveActiveProjectDataDir(&path), None);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_active_project_data_dir_null_when_state_sqlite_missing() {
        let dir = temp_dir("resolve-unprovisioned");
        let path = dir.join("registry.sqlite");
        let project_root = temp_dir("resolve-unprovisioned-root");
        {
            let reg = openRegistry(&path).unwrap();
            registerProject(&reg, &json!("p"), &json!(project_root.to_string_lossy().to_string())).unwrap();
            activateProject(&reg, &json!("p"));
        }
        assert_eq!(resolveActiveProjectDataDir(&path), None);
        let _ = fs::remove_dir_all(&dir);
        let _ = fs::remove_dir_all(&project_root);
    }

    #[test]
    fn resolve_active_project_data_dir_returns_dir_when_provisioned() {
        let dir = temp_dir("resolve-ok");
        let path = dir.join("registry.sqlite");
        let project_root = temp_dir("resolve-ok-root");
        let db_dir = project_root.join(".antiloki").join("db");
        fs::create_dir_all(&db_dir).unwrap();
        fs::write(db_dir.join("state.sqlite"), b"").unwrap();
        {
            let reg = openRegistry(&path).unwrap();
            registerProject(&reg, &json!("p"), &json!(project_root.to_string_lossy().to_string())).unwrap();
            activateProject(&reg, &json!("p"));
        }
        let resolved = resolveActiveProjectDataDir(&path).unwrap();
        assert_eq!(resolved, db_dir);
        let _ = fs::remove_dir_all(&dir);
        let _ = fs::remove_dir_all(&project_root);
    }
}
