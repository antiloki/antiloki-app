//! design-snapshot-build.rs — twin of skills/codebase/design-snapshot-build.mjs. NAMES ARE
//! CONTRACT: `designSnapshotBuild` (default, db). FEAT-DESIGN-SYSTEM (snapshots): walks the
//! project's real `.jsx`/`.tsx` files on disk, converts each found component/screen to static
//! HTML (`codebase_jsx_to_html::convertJsx`, itself built on the tree-sitter CST via
//! `codebase_parse_treesitter::parseToTree`), compiles the project's CSS
//! (`codebase_design_css_compile::compileProjectCss`), and persists both into
//! `code_design_snapshots` (wipe+repopulate per origin).
//!
//! BESPOKE-NATIVE, not conform-db fixture-tested: this unit does REAL, non-fixture-seedable
//! filesystem work — it reads `root` from the LAST `code_onboard_jobs` row and then walks/reads
//! arbitrary files under that root directly off disk (`readdirSync`/`readFileSync` in the JS
//! source, `std::fs` here), the same category as `design-css-compile.rs`'s own root-walk (that
//! twin's module doc: "REAL FILESYSTEM ... root paths point at real committed testdata
//! directories" — used there via a fixture because `root` is a plain top-level input field; here
//! `root` is instead DB-DERIVED from `code_onboard_jobs`, same non-seedable-via-JSON-fixture
//! category as `stack-facts.rs`/`constitution-estimate.rs`). Proven via `#[cfg(test)]` native
//! tests against real temp directories instead, same precedent as `auth-sign.rs`/
//! `driver-queue.rs`.
//!
//! Cross-module calls (all pre-existing Rust twins, per the crossing playbook's cross-module-call
//! convention): `codebase_code_schema::ensureCodeSchema`, `codebase_design_css_compile::compileProjectCss`,
//! `codebase_parse_treesitter::parseToTree`, `codebase_design_components_extract::extractComponents`,
//! `codebase_jsx_to_html::convertJsx`.
//!
//! `ensureCodeSchema` never throws in this Rust twin (its DDL exec swallows errors via `let _ =
//! db.execute_batch(...)`), so the JS `try { ensureCodeSchema(db,'code.') } catch { try {
//! ensureCodeSchema(db,'') } catch {} }` double-fallback collapses to one unconditional call with
//! the `'code.'` prefix — the first branch always "succeeds" here, matching the JS happy path
//! (the fallback only exists for a genuinely failing prefixed exec, which this twin's swallow-all
//! `ensureCodeSchema` never produces).
//!
//! `context.db` is always injected by the db lane in production, so the JS `if (!db) throw
//! err.contextMissing(...)` guard is unreachable here (mirrors every sibling twin's convention,
//! e.g. `code-schema.rs`'s `codeSchema`).

use regex::Regex;
use rusqlite::Connection;
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

fn nz(v: Option<&Value>) -> Option<&Value> {
    v.filter(|x| !x.is_null())
}
fn str_qq(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(),
        Some(o) => o.to_string(),
    }
}

const SKIP_DIR: [&str; 9] = [
    "node_modules", ".git", "dist", "build", "coverage", ".next", "out", ".cache", ".antiloki",
];
const COMP_EXT: [&str; 2] = [".jsx", ".tsx"];

fn ext_of(name: &str) -> String {
    static R: OnceLock<Regex> = OnceLock::new();
    let re = R.get_or_init(|| Regex::new(r"\.[^./\\]+$").unwrap());
    re.find(name).map(|m| m.as_str().to_lowercase()).unwrap_or_default()
}

fn is_test_like(name: &str) -> bool {
    static R: OnceLock<Regex> = OnceLock::new();
    let re = R.get_or_init(|| Regex::new(r"\.(test|spec|stories)\.\w+$").unwrap());
    re.is_match(name)
}

fn is_screen_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"(?i)(^|/)(pages?|routes?|screens?|views?)(/|$)").unwrap())
}
fn is_screen_name_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"(Page|Screen|View|App)$").unwrap())
}
fn is_screen(path: &str, name: &str) -> bool {
    is_screen_re().is_match(path) || is_screen_name_re().is_match(name)
}

/// `function walkCompFiles(root)` — bounded (9000-dir budget) DFS via a LIFO stack, skipping
/// SKIP_DIR + dot-dirs, collecting absolute paths whose extension is `.jsx`/`.tsx` and whose
/// basename is not a `.test.`/`.spec.`/`.stories.` file. An unreadable dir is silently skipped.
fn walk_comp_files(root: &str) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    let mut stack: Vec<PathBuf> = vec![PathBuf::from(root)];
    let mut budget: i64 = 9000;
    while !stack.is_empty() {
        if budget <= 0 {
            break;
        }
        budget -= 1;
        let dir = stack.pop().unwrap();
        let entries = match fs::read_dir(&dir) {
            Ok(e) => e,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let ft = match entry.file_type() {
                Ok(ft) => ft,
                Err(_) => continue,
            };
            let name = entry.file_name().to_string_lossy().into_owned();
            if ft.is_dir() {
                if !SKIP_DIR.contains(&name.as_str()) && !name.starts_with('.') {
                    stack.push(dir.join(&name));
                }
            } else if COMP_EXT.contains(&ext_of(&name).as_str()) && !is_test_like(&name) {
                out.push(dir.join(&name));
            }
        }
    }
    out
}

fn to_posix_rel(root: &Path, file: &Path) -> String {
    let rel = file.strip_prefix(root).unwrap_or(file);
    rel.to_string_lossy().replace('\\', "/")
}

#[allow(non_snake_case)]
pub fn designSnapshotBuild(input: &Value, db: &Connection) -> Result<Value, String> {
    let origin_v = nz(input.get("origin")).cloned().unwrap_or_else(|| json!("atlas"));
    let origin = str_qq(Some(&origin_v));

    crate::codebase_code_schema::ensureCodeSchema(db, &json!("code."));

    let root: Option<String> = db
        .prepare("SELECT root FROM code_onboard_jobs ORDER BY rowid DESC LIMIT 1")
        .and_then(|mut stmt| stmt.query_row([], |row| row.get::<_, Option<String>>(0)))
        .ok()
        .flatten();

    let Some(root) = root else {
        return Ok(json!({
            "ok": true,
            "origin": origin,
            "snapshots": 0,
            "note": "no project root — rebuild the atlas first",
        }));
    };

    let css_result = crate::codebase_design_css_compile::compileProjectCss(&json!(root.as_str()), &json!({}));
    let css = css_result.get("css").and_then(Value::as_str).unwrap_or("").to_string();

    let root_path = PathBuf::from(&root);
    let mut rows: Vec<Value> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();

    for file in walk_comp_files(&root) {
        if rows.len() >= 400 {
            break;
        }
        let src = match fs::read_to_string(&file) {
            Ok(s) => s,
            Err(_) => continue,
        };
        if src.is_empty() || src.chars().count() > 200000 {
            continue;
        }
        let rel = to_posix_rel(&root_path, &file);
        let Some(tree) = crate::codebase_parse_treesitter::parseToTree(&src, "tsx") else {
            continue;
        };
        let src_bytes = src.as_bytes();
        let comps = crate::codebase_design_components_extract::extractComponents(
            Some(tree.root_node()),
            &rel,
            src_bytes,
        );
        for c in comps.into_iter().take(16) {
            let name = c.get("name").and_then(Value::as_str).unwrap_or("").to_string();
            let key = format!("{rel}#{name}");
            if !seen.insert(key) {
                continue;
            }
            let r = crate::codebase_jsx_to_html::convertJsx(&json!(src.as_str()), &json!(name));
            let ok = r.get("ok").and_then(Value::as_bool).unwrap_or(false);
            let html = r.get("html").and_then(Value::as_str).unwrap_or("");
            if !ok || html.trim().is_empty() {
                continue;
            }
            rows.push(json!({
                "kind": if is_screen(&rel, &name) { "screen" } else { "component" },
                "name": name,
                "path": rel,
                "framework": "react",
                "html": html,
            }));
        }
    }

    let mut persisted = false;
    if db.execute("DELETE FROM code_design_snapshots WHERE origin = ?", rusqlite::params![origin]).is_ok() {
        persisted = true;
    }
    if persisted {
        if !css.is_empty() {
            let _ = db.execute(
                "INSERT INTO code_design_snapshots (origin, kind, name, path, framework, html, css) VALUES (?,?,?,?,?,?,?)",
                rusqlite::params![origin, "stylesheet", "__css__", Option::<String>::None, Option::<String>::None, Option::<String>::None, css],
            );
        }
        for r in &rows {
            let kind = r.get("kind").and_then(Value::as_str).unwrap_or_default();
            let name = r.get("name").and_then(Value::as_str).unwrap_or_default();
            let path = r.get("path").and_then(Value::as_str).unwrap_or_default();
            let framework = r.get("framework").and_then(Value::as_str).unwrap_or_default();
            let html = r.get("html").and_then(Value::as_str).unwrap_or_default();
            let _ = db.execute(
                "INSERT INTO code_design_snapshots (origin, kind, name, path, framework, html, css) VALUES (?,?,?,?,?,?,?)",
                rusqlite::params![origin, kind, name, path, framework, html, Option::<String>::None],
            );
        }
    }

    Ok(json!({
        "ok": true,
        "origin": origin,
        "snapshots": rows.len() as i64,
        "css_bytes": css.chars().count() as i64,
        "persisted": persisted,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!("antiloki-design-snapshot-build-rs-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn seed_root(c: &Connection, root: &str) {
        // Tests use an in-memory connection with no ATTACHed "code" schema, so seed via the
        // unprefixed ("") schema — exactly like code-schema.rs's own tests. Unqualified table
        // names inside designSnapshotBuild (`code_onboard_jobs`, `code_design_snapshots`) then
        // resolve to these main-db tables. The twin's own internal `ensureCodeSchema(db,"code.")`
        // call is a no-op here (no "code" schema attached → execute_batch silently fails, per
        // its own swallow-all contract) and does not disturb these pre-seeded tables.
        crate::codebase_code_schema::ensureCodeSchema(c, &json!(""));
        c.execute(
            "INSERT INTO code_onboard_jobs (job_id, root) VALUES (?, ?)",
            rusqlite::params!["job-1", root],
        )
        .unwrap();
    }

    #[test]
    fn no_root_returns_note_and_no_snapshots() {
        let c = db();
        crate::codebase_code_schema::ensureCodeSchema(&c, &json!(""));
        let r = designSnapshotBuild(&json!({}), &c).unwrap();
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["origin"], json!("atlas"));
        assert_eq!(r["snapshots"], json!(0));
        assert!(r["note"].as_str().unwrap().contains("rebuild the atlas"));
    }

    #[test]
    fn walks_real_dir_and_persists_component_snapshot() {
        let dir = temp_dir("basic");
        fs::write(
            dir.join("Widget.tsx"),
            "export function Widget({label}) {\n  return <div className=\"w\">{label}</div>;\n}\n",
        )
        .unwrap();
        fs::write(dir.join("styles.css"), "body { color: red; }\n").unwrap();

        let c = db();
        seed_root(&c, dir.to_str().unwrap());

        let r = designSnapshotBuild(&json!({ "origin": "atlas" }), &c).unwrap();
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["snapshots"], json!(1));
        assert_eq!(r["persisted"], json!(true));
        assert!(r["css_bytes"].as_i64().unwrap() > 0);

        let count: i64 = c
            .query_row(
                "SELECT COUNT(*) FROM code_design_snapshots WHERE origin='atlas'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        // 1 stylesheet row + 1 component row
        assert_eq!(count, 2);

        let comp_name: String = c
            .query_row(
                "SELECT name FROM code_design_snapshots WHERE origin='atlas' AND kind='component'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(comp_name, "Widget");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn screen_named_component_is_classified_as_screen() {
        let dir = temp_dir("screen");
        fs::write(
            dir.join("HomePage.tsx"),
            "export function HomePage() {\n  return <div>home</div>;\n}\n",
        )
        .unwrap();

        let c = db();
        seed_root(&c, dir.to_str().unwrap());
        let r = designSnapshotBuild(&json!({}), &c).unwrap();
        assert_eq!(r["snapshots"], json!(1));

        let kind: String = c
            .query_row(
                "SELECT kind FROM code_design_snapshots WHERE origin='atlas' AND name='HomePage'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(kind, "screen");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_files_are_skipped() {
        let dir = temp_dir("skip-test");
        fs::write(
            dir.join("Widget.test.tsx"),
            "export function Widget() { return <div>x</div>; }\n",
        )
        .unwrap();

        let c = db();
        seed_root(&c, dir.to_str().unwrap());
        let r = designSnapshotBuild(&json!({}), &c).unwrap();
        assert_eq!(r["snapshots"], json!(0));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn skip_dirs_are_not_walked() {
        let dir = temp_dir("skip-dir");
        let nm = dir.join("node_modules");
        fs::create_dir_all(&nm).unwrap();
        fs::write(nm.join("Vendor.tsx"), "export function Vendor() { return <div>v</div>; }\n").unwrap();

        let c = db();
        seed_root(&c, dir.to_str().unwrap());
        let r = designSnapshotBuild(&json!({}), &c).unwrap();
        assert_eq!(r["snapshots"], json!(0));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn distinct_origins_do_not_clobber_each_other() {
        let dir = temp_dir("origins");
        fs::write(
            dir.join("Widget.tsx"),
            "export function Widget() { return <div>x</div>; }\n",
        )
        .unwrap();
        fs::write(dir.join("styles.css"), "body { color: blue; }\n").unwrap();

        let c = db();
        crate::codebase_code_schema::ensureCodeSchema(&c, &json!(""));
        c.execute(
            "INSERT INTO code_onboard_jobs (job_id, root) VALUES (?, ?)",
            rusqlite::params!["job-1", dir.to_str().unwrap()],
        )
        .unwrap();

        designSnapshotBuild(&json!({ "origin": "atlas" }), &c).unwrap();
        designSnapshotBuild(&json!({ "origin": "blueprint:x@1" }), &c).unwrap();

        let atlas_count: i64 = c
            .query_row(
                "SELECT COUNT(*) FROM code_design_snapshots WHERE origin='atlas'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let bp_count: i64 = c
            .query_row(
                "SELECT COUNT(*) FROM code_design_snapshots WHERE origin='blueprint:x@1'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(atlas_count, 2);
        assert_eq!(bp_count, 2);

        let _ = fs::remove_dir_all(&dir);
    }
}
