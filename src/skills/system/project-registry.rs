//! project-registry.rs — twin of skills/system/project-registry.mjs. NAMES ARE CONTRACT:
//! `projectRegistry` (default; JS `projectRegistry`).
//!
//! THIN DISPATCHER over the machine-level PROJECT registry (`~/.antiloki/registry.sqlite`,
//! BE-owned, opened RAW — never `context.db`): op-routed CRUD (register | list | activate |
//! deactivate | get_active | discover) delegating every real mutation to
//! `crate::engine_registry` (twin of `engine/registry.mjs`) and, on `register` with
//! `provision: true`, to `crate::engine_open_db::openDb` (twin of `engine/open-db.mjs`) to seed a
//! brand-new project folder's substrate. Both dependencies already have Rust twins with real
//! sqlite-file + real fs side effects proven by their own `#[cfg(test)]` suites
//! (`src/skills/engine/registry.rs`, `src/skills/engine/open-db.rs`) — this file reuses them
//! rather than reimplementing registry CRUD or DB-open/seed/attach logic.
//!
//! Genuinely non-deterministic (real sqlite file open + real fs walk for `discover`), so this is
//! BESPOKE NATIVE TESTING (no fixture, no frontier entry) — see manifest. Proven below via
//! `#[cfg(test)]` against real tempdir-backed registries, reusing `with_home()`-style env
//! injection is unnecessary here (the registry path is passed explicitly, mirroring the JS
//! `context.registryPath` test override).
//!
//! Throw fidelity: the single JS guard-throw (`err.required('project-registry', 'op')` from
//! engine/error.mjs) is reproduced as `Err(String)` carrying the exact `AntilokiError` message
//! text (`"<skill>: <field> is required"` — see error.mjs's `parts.join(' ')`), matching the
//! convention already used by `engine/registry.rs`'s own throw-fidelity notes (no JS test file
//! exists for this module either).

use serde_json::{json, Value};
use std::fs::create_dir_all;
use std::path::{Path, PathBuf};

use crate::engine_open_db::openDb;
use crate::engine_registry::{
    activateProject, deactivateProjects, listProjects, openRegistry, registerProject, registry_path,
};

fn str_field(v: &Value, field: &str) -> Option<String> {
    v.get(field).and_then(|x| x.as_str()).map(|s| s.to_string())
}

/// Node `path.resolve(p)`: absolute against cwd, with `.`/`..` segments collapsed — no fs access.
fn resolve_path(p: &str) -> PathBuf {
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
    result
}

/// `export default function projectRegistry(input, context)`.
pub fn projectRegistry(input: &Value, context: &Value) -> Result<Value, String> {
    let op = str_field(input, "op").unwrap_or_default();
    if op.is_empty() {
        return Err("project-registry: op is required".to_string());
    }
    let reg_path = str_field(context, "registryPath")
        .map(PathBuf::from)
        .unwrap_or_else(registry_path);
    let reg = openRegistry(&reg_path)?;

    match op.as_str() {
        "register" => {
            let name = input.get("name").cloned().unwrap_or(Value::Null);
            let root_path = input.get("root_path").cloned().unwrap_or(Value::Null);
            match registerProject(&reg, &name, &root_path) {
                Err(e) => Ok(json!({ "ok": false, "error": e })),
                Ok(project) => {
                    // Opt-in (provision:true): seed a fresh substrate for a brand-new folder (no
                    // db yet). An already-provisioned project (e.g. one copied here WITH its
                    // .antiloki/db) is left untouched.
                    let mut provisioned = false;
                    let root_path_str = project["root_path"].as_str().unwrap_or("").to_string();
                    let db_dir = Path::new(&root_path_str).join(".antiloki").join("db");
                    let provision_requested = matches!(input.get("provision"), Some(Value::Bool(true)));
                    if provision_requested && !db_dir.join("state.sqlite").exists() {
                        let db_input = json!({
                            "statePath": db_dir.join("state.sqlite").to_string_lossy(),
                            "memoryPath": db_dir.join("memory.sqlite").to_string_lossy(),
                            "brainPath": db_dir.join("brain.sqlite").to_string_lossy(),
                            "lawsPath": db_dir.join("laws.sqlite").to_string_lossy(),
                            "knowledgePath": db_dir.join("knowledge.sqlite").to_string_lossy(),
                        });
                        let pdb = openDb(&db_input, &json!({}))?;
                        drop(pdb);
                        provisioned = true;
                    }
                    Ok(json!({ "ok": true, "project": project, "provisioned": provisioned }))
                }
            }
        }
        "list" => {
            let list = listProjects(&reg);
            let mut out = list.as_object().cloned().unwrap_or_default();
            out.insert("ok".into(), json!(true));
            Ok(reorder_ok_first(out))
        }
        "get_active" => {
            let active = listProjects(&reg)["active"].clone();
            Ok(json!({ "ok": true, "active": active }))
        }
        "activate" => {
            let name = input.get("name").cloned().unwrap_or(Value::Null);
            let project = activateProject(&reg, &name);
            if project.is_null() {
                let name_str = name.as_str().unwrap_or("");
                return Ok(json!({
                    "ok": false,
                    "error": format!("project '{name_str}' is not registered"),
                }));
            }
            Ok(json!({
                "ok": true,
                "active": project["name"],
                "project": project,
                "note": "active project flipped in the machine registry; openDb opens its .antiloki/db (BE reopenDb / next CLI open)",
            }))
        }
        "deactivate" => {
            deactivateProjects(&reg);
            Ok(json!({ "ok": true, "active": Value::Null }))
        }
        "discover" => {
            // Walk up from `path` (or cwd) to the nearest ancestor holding a `.antiloki/db`
            // substrate — like git finding `.git`. Looks for `.antiloki/db` (not bare
            // `.antiloki/`) so the machine-level ~/.antiloki HOME (auth/run/registry, no db/) is
            // never mistaken for a project. Read-only.
            let mut dir = match str_field(input, "path") {
                Some(p) => resolve_path(&p),
                None => std::env::current_dir().unwrap_or_default(),
            };
            let mut found: Option<PathBuf> = None;
            loop {
                if dir.join(".antiloki").join("db").exists() {
                    found = Some(dir.clone());
                    break;
                }
                match dir.parent() {
                    Some(parent) if parent != dir => dir = parent.to_path_buf(),
                    _ => break,
                }
            }
            let provisioned = found
                .as_ref()
                .map(|f| f.join(".antiloki").join("db").join("state.sqlite").exists())
                .unwrap_or(false);
            Ok(json!({
                "ok": true,
                "found": found.is_some(),
                "root_path": found.map(|f| f.to_string_lossy().to_string()),
                "provisioned": provisioned,
            }))
        }
        other => Ok(json!({
            "ok": false,
            "error": format!("unknown op '{other}' (register|list|activate|deactivate|get_active|discover)"),
        })),
    }
}

// list op spreads `{ ok: true, ...listProjects(reg) }` — JS object key order is insertion order,
// so `ok` lands FIRST, then `projects`/`active`/`count` in listProjects' own order. serde_json's
// Map (preserve_order feature assumed elsewhere in this crate) inserts `ok` last above; rebuild
// with `ok` first to match JS enumeration order exactly.
fn reorder_ok_first(mut out: serde_json::Map<String, Value>) -> Value {
    let ok = out.remove("ok").unwrap_or(Value::Bool(true));
    let mut ordered = serde_json::Map::new();
    ordered.insert("ok".to_string(), ok);
    for (k, v) in out {
        ordered.insert(k, v);
    }
    Value::Object(ordered)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_dir(tag: &str) -> PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!("antiloki-project-registry-rs-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn ctx_for(reg_path: &Path) -> Value {
        json!({ "registryPath": reg_path.to_string_lossy() })
    }

    #[test]
    fn op_is_required() {
        let dir = temp_dir("op-required");
        let reg_path = dir.join("registry.sqlite");
        let e = projectRegistry(&json!({}), &ctx_for(&reg_path)).unwrap_err();
        assert_eq!(e, "project-registry: op is required");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn unknown_op_returns_ok_false() {
        let dir = temp_dir("unknown-op");
        let reg_path = dir.join("registry.sqlite");
        let r = projectRegistry(&json!({ "op": "bogus" }), &ctx_for(&reg_path)).unwrap();
        assert_eq!(r["ok"], json!(false));
        assert_eq!(
            r["error"],
            json!("unknown op 'bogus' (register|list|activate|deactivate|get_active|discover)")
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn register_without_provision_leaves_no_db_dir() {
        let dir = temp_dir("register-noprovision");
        let reg_path = dir.join("registry.sqlite");
        let project_root = temp_dir("register-noprovision-root");
        let r = projectRegistry(
            &json!({ "op": "register", "name": "proj", "root_path": project_root.to_string_lossy() }),
            &ctx_for(&reg_path),
        )
        .unwrap();
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["provisioned"], json!(false));
        assert!(!project_root.join(".antiloki").join("db").exists());
        let _ = fs::remove_dir_all(&dir);
        let _ = fs::remove_dir_all(&project_root);
    }

    #[test]
    fn register_with_provision_true_seeds_fresh_substrate() {
        let dir = temp_dir("register-provision");
        let reg_path = dir.join("registry.sqlite");
        let project_root = temp_dir("register-provision-root");
        let r = projectRegistry(
            &json!({
                "op": "register", "name": "proj", "root_path": project_root.to_string_lossy(),
                "provision": true,
            }),
            &ctx_for(&reg_path),
        )
        .unwrap();
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["provisioned"], json!(true));
        assert!(project_root.join(".antiloki").join("db").join("state.sqlite").exists());
        let _ = fs::remove_dir_all(&dir);
        let _ = fs::remove_dir_all(&project_root);
    }

    #[test]
    fn register_with_provision_true_skips_already_provisioned() {
        let dir = temp_dir("register-already");
        let reg_path = dir.join("registry.sqlite");
        let project_root = temp_dir("register-already-root");
        let db_dir = project_root.join(".antiloki").join("db");
        fs::create_dir_all(&db_dir).unwrap();
        fs::write(db_dir.join("state.sqlite"), b"already here").unwrap();
        let r = projectRegistry(
            &json!({
                "op": "register", "name": "proj", "root_path": project_root.to_string_lossy(),
                "provision": true,
            }),
            &ctx_for(&reg_path),
        )
        .unwrap();
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["provisioned"], json!(false));
        // Untouched: still the sentinel bytes, not a real sqlite header.
        assert_eq!(fs::read(db_dir.join("state.sqlite")).unwrap(), b"already here");
        let _ = fs::remove_dir_all(&dir);
        let _ = fs::remove_dir_all(&project_root);
    }

    #[test]
    fn register_error_surfaces_as_ok_false_not_err() {
        let dir = temp_dir("register-clash");
        let reg_path = dir.join("registry.sqlite");
        let dir_a = temp_dir("register-clash-a");
        let dir_b = temp_dir("register-clash-b");
        projectRegistry(
            &json!({ "op": "register", "name": "shared", "root_path": dir_a.to_string_lossy() }),
            &ctx_for(&reg_path),
        )
        .unwrap();
        let r = projectRegistry(
            &json!({ "op": "register", "name": "shared", "root_path": dir_b.to_string_lossy() }),
            &ctx_for(&reg_path),
        )
        .unwrap();
        assert_eq!(r["ok"], json!(false));
        assert_eq!(r["error"], json!("registry: name 'shared' already used by another project"));
        let _ = fs::remove_dir_all(&dir);
        let _ = fs::remove_dir_all(&dir_a);
        let _ = fs::remove_dir_all(&dir_b);
    }

    #[test]
    fn list_and_get_active_and_activate_and_deactivate_roundtrip() {
        let dir = temp_dir("roundtrip");
        let reg_path = dir.join("registry.sqlite");
        let root_a = temp_dir("roundtrip-a");
        let root_b = temp_dir("roundtrip-b");
        projectRegistry(
            &json!({ "op": "register", "name": "a", "root_path": root_a.to_string_lossy() }),
            &ctx_for(&reg_path),
        )
        .unwrap();
        projectRegistry(
            &json!({ "op": "register", "name": "b", "root_path": root_b.to_string_lossy() }),
            &ctx_for(&reg_path),
        )
        .unwrap();

        let listed = projectRegistry(&json!({ "op": "list" }), &ctx_for(&reg_path)).unwrap();
        assert_eq!(listed["ok"], json!(true));
        assert_eq!(listed["count"], json!(2));
        assert_eq!(listed["active"], Value::Null);

        let ga = projectRegistry(&json!({ "op": "get_active" }), &ctx_for(&reg_path)).unwrap();
        assert_eq!(ga, json!({ "ok": true, "active": Value::Null }));

        let act = projectRegistry(&json!({ "op": "activate", "name": "a" }), &ctx_for(&reg_path)).unwrap();
        assert_eq!(act["ok"], json!(true));
        assert_eq!(act["active"], json!("a"));
        assert_eq!(act["project"]["name"], json!("a"));

        let ga2 = projectRegistry(&json!({ "op": "get_active" }), &ctx_for(&reg_path)).unwrap();
        assert_eq!(ga2["active"]["name"], json!("a"));

        let deact = projectRegistry(&json!({ "op": "deactivate" }), &ctx_for(&reg_path)).unwrap();
        assert_eq!(deact, json!({ "ok": true, "active": Value::Null }));

        let _ = fs::remove_dir_all(&dir);
        let _ = fs::remove_dir_all(&root_a);
        let _ = fs::remove_dir_all(&root_b);
    }

    #[test]
    fn activate_unknown_name_returns_ok_false() {
        let dir = temp_dir("activate-unknown");
        let reg_path = dir.join("registry.sqlite");
        let r = projectRegistry(&json!({ "op": "activate", "name": "nosuch" }), &ctx_for(&reg_path)).unwrap();
        assert_eq!(r["ok"], json!(false));
        assert_eq!(r["error"], json!("project 'nosuch' is not registered"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn discover_walks_up_to_nearest_antiloki_db() {
        let dir = temp_dir("discover");
        let db_dir = dir.join(".antiloki").join("db");
        create_dir_all(&db_dir).unwrap();
        let nested = dir.join("sub").join("deeper");
        create_dir_all(&nested).unwrap();

        let reg_path = dir.join("registry.sqlite"); // reg itself unused by discover but required to open
        let r = projectRegistry(
            &json!({ "op": "discover", "path": nested.to_string_lossy() }),
            &ctx_for(&reg_path),
        )
        .unwrap();
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["found"], json!(true));
        assert_eq!(r["root_path"], json!(dir.to_string_lossy().to_string()));
        assert_eq!(r["provisioned"], json!(false));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn discover_reports_not_found_with_no_ancestor_match() {
        let dir = temp_dir("discover-none");
        let far = dir.join("a").join("b").join("c");
        create_dir_all(&far).unwrap();
        // Guaranteed to not find `.antiloki/db` anywhere up to filesystem root in a temp scratch tree.
        let reg_path = dir.join("registry.sqlite");
        let r = projectRegistry(
            &json!({ "op": "discover", "path": far.to_string_lossy() }),
            &ctx_for(&reg_path),
        )
        .unwrap();
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["found"], json!(false));
        assert_eq!(r["root_path"], Value::Null);
        let _ = fs::remove_dir_all(&dir);
    }
}
