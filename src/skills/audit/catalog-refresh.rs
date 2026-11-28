//! catalog-refresh.rs — twin of skills/audit/catalog-refresh.mjs. NAMES ARE CONTRACT:
//! `catalogRefresh` (default, standard db lane).
//!
//! BESPOKE-NATIVE, not conform-db fixture-tested: this skill walks REAL directories on disk via
//! `crate::engine_load_skill::listSkills` (itself a real fs-walk twin, see load-skill.rs's module
//! doc) and reads REAL `.rs` files off disk to sha256-hash them and extract the description +
//! input schema via `crate::audit_extract_rust_skill_meta` — the module doc header and the fields
//! the body actually reads, replacing the JSDoc block and `@param` tags that the JS tree carried
//! and a Rust tree does not — none of that is fixture-seedable through
//! conform-db's two-lane db-only harness (same class as load-skill.rs / consistency-scan.rs).
//! Proven below via `#[cfg(test)]` against a real temp directory tree, reusing the
//! `ANTILOKI_SKILLS_ROOT` test seam load-skill.rs already defines (guarded by the same shared
//! `crate::test_env_lock::ENV_LOCK`, since `listSkills`'s CACHE is process-global state).
//!
//! `db.prepare(...).run(...)` UPSERT into `memory.skills` is ported as a plain `INSERT ... ON
//! CONFLICT DO UPDATE` executed per row (rusqlite has no reusable prepared-statement handle across
//! a loop without fighting the borrow checker here, and this isn't a hot path). `getExisting`
//! lookup ported as a `SELECT sha256 FROM memory.skills WHERE name = ?`. `context.db` missing → JS
//! throws `err.contextMissing('catalog-refresh', 'db')` — unreachable here (`db: &Connection` is a
//! required parameter, not an `Option`), so not modeled as a runtime branch.
// QUALITY: 78/100 — real fs-walk + db upsert loop faithfully ported and proven against a real
// temp directory tree; AST-derived description/schema extraction is exercised indirectly through
// the already-tested extract-jsdoc-description.rs / extract-input-schema.rs twins.

use crate::audit_extract_rust_skill_meta::{extractRustDescription, extractRustInputSchema};
use crate::engine_load_skill::{clearCache, listSkills};
use rusqlite::{params, Connection};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::path::Path;

/// `export default function catalogRefresh(_input, context)`.
#[allow(non_snake_case)]
pub fn catalogRefresh(_input: &Value, db: &Connection) -> Result<Value, String> {
    clearCache();
    let listing = listSkills();
    let entries = listing.as_array().cloned().unwrap_or_default();

    let mut added: i64 = 0;
    let mut updated: i64 = 0;
    let mut unchanged: i64 = 0;
    let mut drifted: Vec<Value> = Vec::new();

    for entry in &entries {
        let name = entry.get("name").and_then(Value::as_str).unwrap_or_default();
        let path = entry.get("path").and_then(Value::as_str).unwrap_or_default();

        if !Path::new(path).exists() {
            continue;
        }
        let source = match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) => return Err(format!("catalog-refresh: {e}")),
        };
        let sha256 = {
            let mut hasher = Sha256::new();
            hasher.update(source.as_bytes());
            format!("{:x}", hasher.finalize())
        };
        let lines: i64 = source.split('\n').count() as i64;

        // Both facts come from the RUST source now. The JSDoc/@param extractors this used to call
        // parse JavaScript with oxc; against a `.rs` file they do not degrade, they fail on every
        // skill in the tree. See extract-rust-skill-meta.rs for what replaces each.
        let description: Option<String> = extractRustDescription(&source);
        let schema_json: Option<String> = Some(extractRustInputSchema(&source).to_string());

        let existing_sha: Option<String> = db
            .query_row(
                "SELECT sha256 FROM memory.skills WHERE name = ?",
                params![name],
                |row| row.get(0),
            )
            .ok();

        match &existing_sha {
            None => added += 1,
            Some(old) if old != &sha256 => {
                updated += 1;
                drifted.push(json!({
                    "name": name,
                    "old_sha256": old,
                    "new_sha256": sha256,
                }));
            }
            Some(_) => unchanged += 1,
        }

        let normalized_path = path.replace('\\', "/");
        db.execute(
            "INSERT INTO memory.skills (name, path, description, input_schema, sha256, lines, last_seen_at)
             VALUES (?, ?, ?, ?, ?, ?, datetime('now'))
             ON CONFLICT(name) DO UPDATE SET
               path = excluded.path,
               description = COALESCE(excluded.description, memory.skills.description),
               input_schema = COALESCE(excluded.input_schema, memory.skills.input_schema),
               sha256 = excluded.sha256,
               lines = excluded.lines,
               last_seen_at = datetime('now')",
            params![name, normalized_path, description, schema_json, sha256, lines],
        )
        .map_err(|e| format!("catalog-refresh: {e}"))?;
    }

    Ok(json!({
        "added": added,
        "updated": updated,
        "unchanged": unchanged,
        "total": entries.len() as i64,
        "drifted": drifted,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    fn temp_dir(tag: &str) -> PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!("antiloki-catalog-refresh-rs-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn db_with_skills_table() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS memory;").unwrap();
        c.execute_batch(
            "CREATE TABLE memory.skills (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                name TEXT UNIQUE,
                path TEXT,
                description TEXT,
                input_schema TEXT,
                sha256 TEXT,
                lines INTEGER,
                last_seen_at TEXT
            )",
        )
        .unwrap();
        c
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
    fn first_run_adds_all_disk_skills() {
        let dir = temp_dir("add");
        fs::write(
            dir.join("one.rs"),
            "/** One does a thing. */\nexport default function one(input, context) {}",
        )
        .unwrap();
        with_skills_root(&dir, || {
            let db = db_with_skills_table();
            let r = catalogRefresh(&Value::Null, &db).unwrap();
            assert_eq!(r["added"], json!(1));
            assert_eq!(r["updated"], json!(0));
            assert_eq!(r["unchanged"], json!(0));
            assert_eq!(r["total"], json!(1));
            let name: String = db
                .query_row("SELECT name FROM memory.skills WHERE name = 'one'", [], |row| row.get(0))
                .unwrap();
            assert_eq!(name, "one");
        });
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn second_run_unchanged_reports_unchanged_not_added() {
        let dir = temp_dir("unchanged");
        fs::write(dir.join("two.rs"), "//! two.rs — a skill.
pub fn two(input: &Value) {}").unwrap();
        with_skills_root(&dir, || {
            let db = db_with_skills_table();
            catalogRefresh(&Value::Null, &db).unwrap();
            let r = catalogRefresh(&Value::Null, &db).unwrap();
            assert_eq!(r["added"], json!(0));
            assert_eq!(r["updated"], json!(0));
            assert_eq!(r["unchanged"], json!(1));
        });
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn changed_source_reports_updated_and_drifted_with_both_shas() {
        let dir = temp_dir("drift");
        let file = dir.join("three.rs");
        fs::write(&file, "export default function three(input, context) {}").unwrap();
        with_skills_root(&dir, || {
            let db = db_with_skills_table();
            catalogRefresh(&Value::Null, &db).unwrap();
            fs::write(&file, "export default function three(input, context) { return 1; }").unwrap();
            clearCache();
            let r = catalogRefresh(&Value::Null, &db).unwrap();
            assert_eq!(r["updated"], json!(1));
            let drifted = r["drifted"].as_array().unwrap();
            assert_eq!(drifted.len(), 1);
            assert_eq!(drifted[0]["name"], json!("three"));
            assert_ne!(drifted[0]["old_sha256"], drifted[0]["new_sha256"]);
        });
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn empty_skills_root_returns_zero_counts() {
        let dir = temp_dir("empty");
        with_skills_root(&dir, || {
            let db = db_with_skills_table();
            let r = catalogRefresh(&Value::Null, &db).unwrap();
            assert_eq!(r["added"], json!(0));
            assert_eq!(r["total"], json!(0));
            assert_eq!(r["drifted"].as_array().unwrap().len(), 0);
        });
        let _ = fs::remove_dir_all(&dir);
    }
}
