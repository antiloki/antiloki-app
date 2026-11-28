//! load-skill.rs — the skill INDEX. NAMES ARE CONTRACT: `findSkillPath`, `clearCache`,
//! `listSkills`, `importSkill`, `loadSkill`.
//!
//! ## PORTED TO THE RUST TREE (antiloki-core deleted)
//! This began as a twin of skills/engine/load-skill.mjs and indexed
//! `<parent>/antiloki-core/skills/**/*.mjs` — a sibling checkout that no longer exists. The
//! skills it indexed are all here now, as `.rs`, so the walk moved to `src/skills/**/*.rs`.
//!
//! Two differences follow from the tree being Rust rather than JavaScript, and both are contract
//! changes rather than path changes:
//!
//! 1. NO `*.test.mjs` EXCLUSION. Rust keeps tests INSIDE the file (`#[cfg(test)] mod tests`), so
//!    there is no test sibling to skip — every `.rs` file under the tree is a skill.
//! 2. `importSkill` RESOLVES THROUGH THE REGISTRY, not the filesystem. JS could `import(url)` an
//!    arbitrary file at runtime; a compiled binary cannot. `skill_registry::dispatch` is the
//!    faithful equivalent — it is the same name→function table `action-run` uses, so "can this
//!    skill be imported" and "can this skill be dispatched" are ONE question with one answer,
//!    which is what the JS pair only approximated.
//!
//! BESPOKE-NATIVE, not conform-db fixture-tested: this module walks a REAL directory tree on
//! disk (`buildIndex`, JS-private, ported here as the private `build_index`) and, for
//! `importSkill`/`loadSkill`, performs a REAL `import(url)` of an arbitrary `.mjs` file resolved
//! at runtime. Real fs directory walking is faithfully ported and proven below against real
//! temp directories (same class as terminal-sandbox.rs / auth-sign.rs's real-fs/real-subprocess
//! testing).
//!
//! ## `importSkill` is MIRRORED, not deferred (2026-08-09)
//! This module previously recorded dynamic `import()` as having "no faithful Rust equivalent" and
//! returned a deferred error. That conflated TRANSLATION with PURPOSE. Rust genuinely cannot hand
//! back a function pointer for a path known only at runtime — but the *purpose* of `importSkill`
//! ("give me something callable, by name") has an exact Rust-native form that this crate already
//! generates: `skill_registry`, a 774-arm name→fn table.
//!
//! So `importSkill` returns a HANDLE into the registry and `invokeSkill` is the call. The two JS
//! failure modes keep their meanings: an unresolvable name still yields `findSkillPath`'s
//! "skill 'X' not found", and a file that resolves but has no callable default yields JS's
//! "has no default export" — which in Rust is "resolves but has no registered arm", the same
//! condition, since the generator only emits an arm for a default export it could find and type.
//!
//! The gap that remains is honest and narrow: the registry covers 774 dispatchable skills, so a
//! skill file that exists but was skipped by the generator resolves and then reports no default
//! export. `skill_registry::registered_names()` is the authoritative list.
//!
//! Errors mirror `engine/error.mjs`'s `AntilokiError` "{skill}: {cause}" message shape:
//! - unknown name: `SKILL_NOT_FOUND` → `"load-skill: skill '<name>' not found"`.
//! - duplicate name across two files during a walk: `INTERNAL_ERROR` →
//!   `"load-skill: duplicate skill name '<name>': found in <first_path> and <duplicate_path>"`.
//! - missing `name` input to `loadSkill`: `VALIDATION_REQUIRED` → `"load-skill: name is required"`.
//!
//! `SKILLS_ROOT` (roots.mjs: `resolve(HERE, '..')`, i.e. the `antiloki-core/skills` dir derived
//! from `import.meta.url`) has no Rust equivalent path derivation (no `import.meta.url`
//! analogue) — production default here mirrors workspace-root.rs's sibling-directories
//! assumption (`antiloki-core-rust` and `antiloki-core` are siblings under the same repo root),
//! walking `<repo_root>/antiloki-core/skills`. `ANTILOKI_SKILLS_ROOT` is a Rust-twin-only test
//! seam (no JS counterpart env var) letting tests point the walk at a real temp tree, same
//! pattern as `with_home()`'s `ANTILOKI_HOME` in auth-sign.rs — guarded by the same shared
//! `test_env_lock::ENV_LOCK` since the global skill-name CACHE (mirroring JS's module-level
//! `let cache = null`) is itself process-global state, requiring test serialization regardless.
// QUALITY: real fs-walk fully ported and tested; dynamic dispatch MIRRORED through skill_registry
// (importSkill returns a handle, invokeSkill runs it) rather than deferred

use rusqlite::Connection;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};

// Mirrors JS `let cache = null;` at module scope — a Vec (not HashMap) to preserve the same
// insertion/iteration order `Map` gives JS's `cache.entries()` (listSkills output order).
static CACHE: LazyLock<Mutex<Option<Vec<(String, String)>>>> = LazyLock::new(|| Mutex::new(None));

fn is_private_dir_name(name: &str) -> bool {
    name.starts_with('.') || name.starts_with('_')
}

/// The skill tree: this crate's own `src/skills`. ANTILOKI_SKILLS_ROOT still overrides it, which
/// is what lets the tests below walk a temp directory.
fn skills_root() -> PathBuf {
    if let Ok(v) = std::env::var("ANTILOKI_SKILLS_ROOT") {
        if !v.is_empty() {
            return PathBuf::from(v);
        }
    }
    // CARGO_MANIFEST_DIR is this crate's root, so the tree is directly beneath it. It used to be
    // `.parent().join("antiloki-core").join("skills")` — one directory UP and sideways, into the
    // deleted checkout.
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src").join("skills")
}

/// JS-private `buildIndex(root = SKILLS_ROOT)`: BFS over `root`, skipping directories whose name
/// starts with `.` or `_`, collecting `*.rs` files keyed by basename
/// (extension stripped). Errors on a duplicate basename found in two different files.
fn build_index(root: &Path) -> Result<Vec<(String, String)>, String> {
    let mut index: Vec<(String, String)> = Vec::new();
    let mut lookup: HashMap<String, String> = HashMap::new();
    let mut queue: Vec<PathBuf> = vec![root.to_path_buf()];
    while let Some(dir) = queue.pop() {
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(e) => return Err(format!("load-skill: {e}")),
        };
        // read_dir order is not guaranteed identical to Node's readdirSync (OS-dependent either
        // way); sort for a stable, reproducible walk across runs.
        let mut names: Vec<std::fs::DirEntry> = entries.filter_map(|e| e.ok()).collect();
        names.sort_by_key(|e| e.file_name());
        for entry in names {
            let file_name = entry.file_name();
            let name = file_name.to_string_lossy().into_owned();
            let file_type = match entry.file_type() {
                Ok(t) => t,
                Err(_) => continue,
            };
            if file_type.is_dir() {
                if is_private_dir_name(&name) {
                    continue;
                }
                queue.push(entry.path());
            } else if file_type.is_file() && name.ends_with(".rs") {
                let key = name.trim_end_matches(".rs").to_string();
                let full_path = entry.path().to_string_lossy().into_owned();
                if let Some(first_path) = lookup.get(&key) {
                    return Err(format!(
                        "load-skill: duplicate skill name '{key}': found in {first_path} and {full_path}"
                    ));
                }
                lookup.insert(key.clone(), full_path.clone());
                index.push((key, full_path));
            }
        }
    }
    Ok(index)
}

/// CACHE is a rebuildable index, not a data invariant, so a panicking test must not poison it for
/// every later caller — recover the guard instead (same rationale as `test_env_lock::ENV_LOCK`).
fn cache_guard() -> std::sync::MutexGuard<'static, Option<Vec<(String, String)>>> {
    CACHE.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Populate the index in place. Callers hold the lock ACROSS build-then-read: releasing it between
/// the two (as a separate ensure-then-relock pair did) lets a concurrent `clearCache()` land in the
/// gap, so the read then unwraps a `None` and panics — which in turn poisoned CACHE for every
/// later caller.
fn ensure_cache_locked(guard: &mut Option<Vec<(String, String)>>) -> Result<(), String> {
    if guard.is_none() {
        *guard = Some(build_index(&skills_root())?);
    }
    Ok(())
}

/// `export function findSkillPath(name)`.
pub fn findSkillPath(name: &Value) -> Result<Value, String> {
    let name_str = name.as_str().unwrap_or_default();
    let mut guard = cache_guard();
    ensure_cache_locked(&mut guard)?;
    let entries = guard.as_ref().expect("ensure_cache_locked populates the index or returns Err");
    match entries.iter().find(|(k, _)| k == name_str) {
        Some((_, path)) => Ok(json!(path)),
        None => Err(format!("load-skill: skill '{name_str}' not found")),
    }
}

/// `export function clearCache()`.
pub fn clearCache() {
    *cache_guard() = None;
}

/// `export function listSkills()` → `Array.from(cache.entries()).map(([name, path]) => ({name, path}))`.
pub fn listSkills() -> Value {
    let mut guard = cache_guard();
    if ensure_cache_locked(&mut guard).is_err() {
        return json!([]);
    }
    let entries = guard.as_ref().expect("ensure_cache_locked populates the index or returns Err");
    Value::Array(
        entries
            .iter()
            .map(|(name, path)| json!({ "name": name, "path": path }))
            .collect(),
    )
}

/// `export async function importSkill(name)` — DEFERRED boundary, see module doc. Resolution
/// (name → path, including the not-found error) is real; the actual `import(url)` + default-export
/// callable is not representable in Rust and is reported as a deferred error instead.
/// `export async function importSkill(name)` — MIRRORED, not translated (2026-08-09).
///
/// JS returns the module's default-export **function**. A Rust fn pointer is not a `Value`, so a
/// literal translation is impossible — but the *purpose* ("give me something I can call, by name")
/// has an exact Rust-native form: the generated `skill_registry`, a 774-arm name→fn table. This
/// returns a HANDLE into it; [`invokeSkill`] is the call.
///
/// The two JS failure modes are preserved with their meanings intact:
///   - name does not resolve to a file → `findSkillPath`'s "skill 'X' not found" (unchanged)
///   - file resolves but has no callable default → JS's `SKILL_INVALID` / "has no default export".
///     Here that is "resolves but has no registered arm", which is the same condition: the
///     generator only emits an arm for a file whose default export it could find and type.
pub fn importSkill(name: &Value) -> Result<Value, String> {
    let path = findSkillPath(name)?;
    let n = name.as_str().unwrap_or_default();
    if !crate::skill_registry::is_registered(n) {
        return Err(format!("load-skill: skill '{n}' has no default export"));
    }
    Ok(json!({ "skill": n, "path": path, "callable": true, "via": "skill_registry" }))
}

/// The call half of [`importSkill`]. JS does `const fn = await importSkill(n); fn(input, ctx)`;
/// here that second step is this, because the handle is a name rather than a closure.
pub fn invokeSkill(name: &Value, input: &Value, db: &Connection) -> Result<Value, String> {
    let n = name.as_str().unwrap_or_default();
    let ctx = crate::skill_registry::RunCtx { db, actor: "unknown", source: "load-skill" };
    match crate::skill_registry::dispatch(n, input, &ctx) {
        Some(r) => r,
        None => Err(format!("load-skill: skill '{n}' has no default export")),
    }
}

/// `export default async function loadSkill(input, _context)`.
pub fn loadSkill(input: &Value, _context: &Value) -> Result<Value, String> {
    let name = match input {
        Value::String(s) => Some(s.clone()),
        Value::Object(o) => o.get("name").and_then(Value::as_str).map(|s| s.to_string()),
        _ => None,
    };
    let name = match name.filter(|s| !s.is_empty()) {
        Some(n) => n,
        None => return Err("load-skill: name is required".to_string()),
    };
    importSkill(&json!(name))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_dir(tag: &str) -> PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!("antiloki-load-skill-rs-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn with_skills_root<T>(root: &Path, f: impl FnOnce() -> T) -> T {
        let _guard = crate::test_env_lock::ENV_LOCK.lock().unwrap();
        unsafe {
            std::env::set_var("ANTILOKI_SKILLS_ROOT", root.to_str().unwrap());
        }
        clearCache();
        let result = f();
        clearCache();
        unsafe {
            std::env::remove_var("ANTILOKI_SKILLS_ROOT");
        }
        result
    }

    #[test]
    fn find_skill_path_walks_real_nested_dirs_and_finds_rs_files() {
        let dir = temp_dir("walk");
        let sub = dir.join("entities");
        fs::create_dir_all(&sub).unwrap();
        fs::write(dir.join("root-skill.rs"), "pub fn skill() {}").unwrap();
        fs::write(sub.join("card-add.rs"), "pub fn skill() {}").unwrap();
        with_skills_root(&dir, || {
            let p1 = findSkillPath(&json!("root-skill")).unwrap();
            assert_eq!(p1, json!(dir.join("root-skill.rs").to_string_lossy().into_owned()));
            let p2 = findSkillPath(&json!("card-add")).unwrap();
            assert_eq!(p2, json!(sub.join("card-add.rs").to_string_lossy().into_owned()));
        });
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn find_skill_path_excludes_hidden_and_private_dirs() {
        let dir = temp_dir("exclude");
        let hidden = dir.join(".workflow_test");
        let underscore = dir.join("_helpers");
        fs::create_dir_all(&hidden).unwrap();
        fs::create_dir_all(&underscore).unwrap();
        fs::write(dir.join("card-add.rs"), "pub fn skill() {}").unwrap();
        fs::write(hidden.join("hidden-skill.rs"), "pub fn skill() {}").unwrap();
        fs::write(underscore.join("helper-skill.rs"), "pub fn skill() {}").unwrap();
        with_skills_root(&dir, || {
            assert!(findSkillPath(&json!("card-add")).is_ok());
            // No `.test.rs` case: Rust keeps tests inside the file, so there is no test
            // sibling to exclude. Hidden and `_private` dirs still must stay out of the index.
            let e2 = findSkillPath(&json!("hidden-skill")).unwrap_err();
            assert!(e2.contains("not found"), "{e2}");
            let e3 = findSkillPath(&json!("helper-skill")).unwrap_err();
            assert!(e3.contains("not found"), "{e3}");
        });
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn find_skill_path_not_found_returns_exact_message() {
        let dir = temp_dir("notfound");
        with_skills_root(&dir, || {
            let e = findSkillPath(&json!("nope")).unwrap_err();
            assert_eq!(e, "load-skill: skill 'nope' not found");
        });
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn build_index_duplicate_name_errors_with_both_paths() {
        let dir = temp_dir("dup");
        let a = dir.join("a");
        let b = dir.join("b");
        fs::create_dir_all(&a).unwrap();
        fs::create_dir_all(&b).unwrap();
        fs::write(a.join("dupe.rs"), "pub fn skill() {}").unwrap();
        fs::write(b.join("dupe.rs"), "pub fn skill() {}").unwrap();
        with_skills_root(&dir, || {
            let e = findSkillPath(&json!("dupe")).unwrap_err();
            assert!(e.starts_with("load-skill: duplicate skill name 'dupe': found in "), "{e}");
            assert!(e.contains("dupe.rs"));
        });
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn list_skills_returns_name_path_pairs() {
        let dir = temp_dir("list");
        fs::write(dir.join("one.rs"), "pub fn skill() {}").unwrap();
        fs::write(dir.join("two.rs"), "pub fn skill() {}").unwrap();
        with_skills_root(&dir, || {
            let list = listSkills();
            let arr = list.as_array().unwrap();
            assert_eq!(arr.len(), 2);
            let names: Vec<&str> = arr.iter().map(|e| e["name"].as_str().unwrap()).collect();
            assert!(names.contains(&"one"));
            assert!(names.contains(&"two"));
            for e in arr {
                assert!(e.get("path").is_some());
            }
        });
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn clear_cache_forces_a_rescan_that_picks_up_new_files() {
        let dir = temp_dir("rescan");
        fs::write(dir.join("first.rs"), "pub fn skill() {}").unwrap();
        with_skills_root(&dir, || {
            assert!(findSkillPath(&json!("first")).is_ok());
            assert!(findSkillPath(&json!("second")).is_err());
            fs::write(dir.join("second.rs"), "pub fn skill() {}").unwrap();
            // Without clearCache, the stale cache still misses the new file.
            assert!(findSkillPath(&json!("second")).is_err());
            clearCache();
            assert!(findSkillPath(&json!("second")).is_ok());
        });
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn import_skill_refuses_a_file_with_no_registered_arm() {
        // A file that exists but the generator never registered: resolves, then reports the same
        // condition JS reports for a module with no callable default.
        let dir = temp_dir("import");
        fs::write(dir.join("some-skill.rs"), "pub fn skill() {}").unwrap();
        with_skills_root(&dir, || {
            let e = importSkill(&json!("some-skill")).unwrap_err();
            assert_eq!(e, "load-skill: skill 'some-skill' has no default export");
            // Unknown name still surfaces the real not-found error, a DIFFERENT failure.
            let e2 = importSkill(&json!("missing")).unwrap_err();
            assert_eq!(e2, "load-skill: skill 'missing' not found");
        });
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn import_skill_returns_a_usable_handle_for_a_registered_skill() {
        // against the REAL skills root: card-list is a registered arm
        let _guard = crate::test_env_lock::ENV_LOCK.lock().unwrap();
        clearCache();
        let h = importSkill(&json!("card-list")).expect("card-list is registered");
        assert_eq!(h["skill"], json!("card-list"));
        assert_eq!(h["callable"], json!(true));
        assert_eq!(h["via"], json!("skill_registry"));
        assert!(h["path"].as_str().unwrap().ends_with("card-list.rs"), "{h}");
    }

    #[test]
    fn invoke_skill_actually_runs_the_registered_twin() {
        // the handle is not decorative — invokeSkill dispatches it for real
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE cards (slug TEXT, status TEXT, title TEXT, id INTEGER);
                         INSERT INTO cards VALUES ('A','doing','a',1);").unwrap();
        let out = invokeSkill(&json!("card-list"), &json!({}), &c).expect("dispatch");
        assert!(out.is_array() || out.is_object(), "got {out}");
    }

    #[test]
    fn invoke_skill_on_an_unregistered_name_reports_no_default_export() {
        let c = Connection::open_in_memory().unwrap();
        let e = invokeSkill(&json!("__nope__"), &json!({}), &c).unwrap_err();
        assert_eq!(e, "load-skill: skill '__nope__' has no default export");
    }

    #[test]
    fn the_registry_is_the_authoritative_reach_list() {
        let names = crate::skill_registry::registered_names();
        assert!(names.len() > 700, "expected the full table, got {}", names.len());
        assert!(crate::skill_registry::is_registered("card-list"));
        assert!(!crate::skill_registry::is_registered("__nope__"));
    }

    #[test]
    fn load_skill_requires_name_accepts_string_or_object_input() {
        let dir = temp_dir("loadskill");
        fs::write(dir.join("named.rs"), "pub fn skill() {}").unwrap();
        with_skills_root(&dir, || {
            let e = loadSkill(&json!({}), &json!(null)).unwrap_err();
            assert_eq!(e, "load-skill: name is required");

            let e2 = loadSkill(&json!(""), &json!(null)).unwrap_err();
            assert_eq!(e2, "load-skill: name is required");

            // loadSkill delegates to importSkill, so an unregistered file reports the same
            // "no default export" as importSkill does — via BOTH input shapes.
            let r1 = loadSkill(&json!("named"), &json!(null)).unwrap_err();
            assert_eq!(r1, "load-skill: skill 'named' has no default export");

            let r2 = loadSkill(&json!({ "name": "named" }), &json!(null)).unwrap_err();
            assert_eq!(r2, "load-skill: skill 'named' has no default export");
        });
        let _ = fs::remove_dir_all(&dir);
    }
}
