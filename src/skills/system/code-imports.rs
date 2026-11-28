//! code-imports.rs — twin of skills/system/code-imports.mjs (db READ+cache-write lane). NAMES ARE
//! CONTRACT: `codeImports` (default). Static import graph for one file in the active workspace,
//! parsed from git-tracked source and cached in `brain.code_connections` (scoped by
//! `workspace_id`). The cache is a derived artifact (not an audited entity — direct writes are
//! allowed, matching the JS module doc), rebuilt lazily on a cache miss or `rebuild:true`.
//!
//! GIT SUBPROCESS — DISCLOSED UNTESTED (playbook lesson 5 / constitution-estimate.rs's
//! `git_diff_paths` precedent): `listTracked` shells out to `git -C <root> ls-files`, but ONLY on
//! the cache-miss path (`rebuild || have.n === 0`). This twin ports the subprocess call for real
//! (`list_tracked`, mirroring `spawnSync('git', ['-C', root, 'ls-files'], ...)` including its
//! non-zero-exit throw) and the full `rebuildCache` walk (real fs reads via `std::fs::read_to_string`,
//! same as constitution-estimate.rs's `walk`), but NO fixture case sets `rebuild:true` or seeds an
//! empty cache — the fixture only exercises the cache-HIT path (seed `brain.code_connections`
//! directly, call with `rebuild` omitted/false). A real `git ls-files` run depends on the actual
//! repo tree on disk, not deterministically fixture-controllable the way a seeded table is.
//!
//! POSIX PATH PORT: `resolveSpecifier`'s `posix.normalize(posix.join(posix.dirname(fromPath), spec))`
//! reuses the byte-for-byte posix `normalize`/`join`/`dirname` algorithm already proven correct by
//! dep-resolve.rs's own test suite (`posix_matches_node`); duplicated here (private, no direct JS
//! export counterpart from THIS file) per the established per-module-copy convention (see
//! workspace-root.rs's module doc for the same rationale).
// QUALITY: 78/100 — cache-hit lane fixture-tested; cache-rebuild lane (git subprocess + fs read)
// real but disclosed-untested per playbook lesson 5.

use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}

const TRY_EXTS: [&str; 7] = ["", ".ts", ".tsx", ".js", ".jsx", ".mjs", ".cjs"];
const TRY_INDEX: [&str; 5] = ["/index.ts", "/index.tsx", "/index.js", "/index.jsx", "/index.mjs"];

fn src_re() -> &'static regex::Regex {
    static R: OnceLock<regex::Regex> = OnceLock::new();
    R.get_or_init(|| regex::Regex::new(r"\.(mjs|cjs|js|jsx|ts|tsx)$").unwrap())
}

fn spec_res() -> &'static Vec<regex::Regex> {
    static R: OnceLock<Vec<regex::Regex>> = OnceLock::new();
    R.get_or_init(|| {
        vec![
            regex::Regex::new(r#"\bimport\s+[^;'"]*?\bfrom\s*['"]([^'"]+)['"]"#).unwrap(), // import x from 'y'
            regex::Regex::new(r#"\bexport\s+[^;'"]*?\bfrom\s*['"]([^'"]+)['"]"#).unwrap(), // export x from 'y'
            regex::Regex::new(r#"\bimport\s*['"]([^'"]+)['"]"#).unwrap(),                  // import 'y'
            regex::Regex::new(r#"\bimport\s*\(\s*['"]([^'"]+)['"]\s*\)"#).unwrap(),        // import('y')
            regex::Regex::new(r#"\brequire\s*\(\s*['"]([^'"]+)['"]\s*\)"#).unwrap(),       // require('y')
        ]
    })
}

// ── node:path posix, ported byte-for-byte (dep-resolve.rs precedent, see module doc) ──────
fn is_sep(c: u8) -> bool { c == b'/' }

fn normalize_string(path: &[u8], allow_above_root: bool) -> String {
    let mut res: Vec<u8> = Vec::new();
    let mut last_segment_length: isize = 0;
    let mut last_slash: isize = -1;
    let mut dots: isize = 0;
    let mut ch: u8 = 0;
    let n = path.len() as isize;
    let mut i: isize = 0;
    while i <= n {
        if i < n {
            ch = path[i as usize];
        } else if is_sep(ch) {
            break;
        } else {
            ch = b'/';
        }
        if is_sep(ch) {
            if last_slash == i - 1 || dots == 1 {
                // NOOP: `//` collapses, `/./` drops
            } else if dots == 2 {
                if res.len() < 2 || last_segment_length != 2
                    || res[res.len() - 1] != b'.' || res[res.len() - 2] != b'.'
                {
                    if res.len() > 2 {
                        match res.iter().rposition(|&c| c == b'/') {
                            Some(lsi) => {
                                res.truncate(lsi);
                                let new_last = res.iter().rposition(|&c| c == b'/').map(|x| x as isize).unwrap_or(-1);
                                last_segment_length = res.len() as isize - 1 - new_last;
                            }
                            None => { res.clear(); last_segment_length = 0; }
                        }
                        last_slash = i;
                        dots = 0;
                        i += 1;
                        continue;
                    } else if !res.is_empty() {
                        res.clear();
                        last_segment_length = 0;
                        last_slash = i;
                        dots = 0;
                        i += 1;
                        continue;
                    }
                }
                if allow_above_root {
                    if !res.is_empty() { res.push(b'/'); res.push(b'.'); res.push(b'.'); }
                    else { res.push(b'.'); res.push(b'.'); }
                    last_segment_length = 2;
                }
            } else {
                let seg = &path[(last_slash + 1) as usize..i as usize];
                if !res.is_empty() { res.push(b'/'); res.extend_from_slice(seg); }
                else { res.extend_from_slice(seg); }
                last_segment_length = i - last_slash - 1;
            }
            last_slash = i;
            dots = 0;
        } else if ch == b'.' && dots != -1 {
            dots += 1;
        } else {
            dots = -1;
        }
        i += 1;
    }
    String::from_utf8(res).unwrap_or_default()
}

fn normalize(path: &str) -> String {
    let bytes = path.as_bytes();
    if bytes.is_empty() { return ".".into(); }
    let is_absolute = bytes[0] == b'/';
    let trailing_sep = *bytes.last().unwrap() == b'/';
    let mut p = normalize_string(bytes, !is_absolute);
    if p.is_empty() {
        if is_absolute { return "/".into(); }
        return if trailing_sep { "./".into() } else { ".".into() };
    }
    if trailing_sep { p.push('/'); }
    if is_absolute { format!("/{}", p) } else { p }
}

fn join(parts: &[&str]) -> String {
    if parts.is_empty() { return ".".into(); }
    let mut joined: Option<String> = None;
    for &arg in parts {
        if !arg.is_empty() {
            match &mut joined {
                None => joined = Some(arg.to_string()),
                Some(j) => { j.push('/'); j.push_str(arg); }
            }
        }
    }
    match joined { Some(j) => normalize(&j), None => ".".into() }
}

fn dirname(path: &str) -> String {
    let bytes = path.as_bytes();
    if bytes.is_empty() { return ".".into(); }
    let has_root = bytes[0] == b'/';
    let mut end: isize = -1;
    let mut matched_slash = true;
    let mut i: isize = bytes.len() as isize - 1;
    while i >= 1 {
        if bytes[i as usize] == b'/' {
            if !matched_slash { end = i; break; }
        } else {
            matched_slash = false;
        }
        i -= 1;
    }
    if end == -1 { return if has_root { "/".into() } else { ".".into() }; }
    if has_root && end == 1 { return "//".into(); }
    String::from_utf8(bytes[..end as usize].to_vec()).unwrap_or_default()
}

// .replace(/^\.\//, '') — strip a single leading "./"
fn strip_dot_slash(s: &str) -> String {
    match s.strip_prefix("./") { Some(rest) => rest.to_string(), None => s.to_string() }
}

// `function listTracked(root)` — real `git -C <root> ls-files` subprocess. See module doc:
// DISCLOSED UNTESTED (only reached on a cache miss, which no fixture case triggers).
fn list_tracked(root: &str) -> Result<Vec<String>, String> {
    let output = crate::proc::cmd("git")
        .arg("-C").arg(root)
        .arg("ls-files")
        .output()
        .map_err(|e| e.to_string())?;
    if !output.status.success() {
        return Err(format!("code-imports: git ls-files not found ({root})"));
    }
    let text = String::from_utf8_lossy(&output.stdout);
    Ok(text.split('\n').map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect())
}

// `function parseSpecifiers(src)` — ordered-unique (JS `Set` preserves insertion order).
fn parse_specifiers(src: &str) -> Vec<String> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut out: Vec<String> = Vec::new();
    for re in spec_res() {
        for cap in re.captures_iter(src) {
            let s = cap[1].to_string();
            if seen.insert(s.clone()) { out.push(s); }
        }
    }
    out
}

// `function resolveSpecifier(fromPath, spec, trackedSet)` — relative-only; bare/package
// specifiers and traversal above the tracked root return `None` (JS `null`).
fn resolve_specifier(from_path: &str, spec: &str, tracked: &HashSet<String>) -> Option<String> {
    if !spec.starts_with('.') && !spec.starts_with('/') { return None; }
    let dir = dirname(from_path);
    let base = strip_dot_slash(&normalize(&join(&[dir.as_str(), spec])));
    if base.starts_with("..") { return None; }
    for ext in TRY_EXTS { let c = format!("{base}{ext}"); if tracked.contains(&c) { return Some(c); } }
    for idx in TRY_INDEX { let c = format!("{base}{idx}"); if tracked.contains(&c) { return Some(c); } }
    None
}

// `resolve(root, rel)` — every `rel` here comes from `git ls-files`, always repo-relative
// forward-slash (never absolute), so a plain join suffices (unlike constitution-estimate.rs's
// `abs_path`, which also has to handle arbitrary caller-supplied absolute paths).
fn abs_path(root: &str, rel: &str) -> PathBuf { Path::new(root).join(rel) }

// `function rebuildCache(db, workspace_id, root, tracked)` — see module doc: DISCLOSED UNTESTED.
fn rebuild_cache(db: &Connection, workspace_id: i64, root: &str, tracked: &[String]) -> Result<(), String> {
    let tracked_set: HashSet<String> = tracked.iter().cloned().collect();
    crate::db_json::run_write(db, "DELETE FROM brain.code_connections WHERE workspace_id = ?", &[json!(workspace_id)])?;
    for rel in tracked {
        if !src_re().is_match(rel) { continue; }
        let src = match std::fs::read_to_string(abs_path(root, rel)) { Ok(s) => s, Err(_) => continue };
        for spec in parse_specifiers(&src) {
            let to_path = resolve_specifier(rel, &spec, &tracked_set);
            crate::db_json::run_write(
                db,
                "INSERT OR IGNORE INTO brain.code_connections (workspace_id, from_path, specifier, to_path) VALUES (?, ?, ?, ?)",
                &[json!(workspace_id), json!(rel.clone()), json!(spec), to_path.map(Value::String).unwrap_or(Value::Null)],
            )?;
        }
    }
    Ok(())
}

/// The cache's answer plus the atlas's: importers are the sorted union; an atlas import target the cache did not resolve joins the imports list with the specifier `(atlas)`. Pure - tested.
fn merge_atlas(mut imports: Vec<Value>, imported_by: Vec<Value>, atlas_in: &[String], atlas_out: &[String]) -> (Vec<Value>, Vec<Value>) {
    let mut by: Vec<String> = imported_by.iter().filter_map(|v| v.as_str().map(String::from)).collect();
    for f in atlas_in { if !by.contains(f) { by.push(f.clone()); } }
    by.sort();
    let known: HashSet<String> = imports.iter().filter_map(|i| i.get("to_path").and_then(Value::as_str).map(String::from)).collect();
    for t in atlas_out { if !known.contains(t) { imports.push(json!({ "specifier": "(atlas)", "to_path": t })); } }
    (imports, by.into_iter().map(Value::String).collect())
}

/// `export default function codeImports(input, context)`
pub fn codeImports(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS: `if (!db) throw err.contextMissing(...)` — unreachable in the db lane (db is always present).
    let path_str = match input.get("path").and_then(Value::as_str).filter(|s| !s.is_empty()) {
        Some(s) => s,
        None => return Err("code-imports: path is required".into()),
    };
    let rel = path_str.replace('\\', "/");
    // JS `const { path, rebuild = false } = input ?? {}; if (rebuild || ...)`: the destructuring
    // default only replaces an ABSENT/undefined key, so an explicit `rebuild:null` also falls
    // through to the `||`'s truthy check — `truthy()` collapses both cases identically.
    let rebuild_flag = truthy(input.get("rebuild"));

    let root_v = crate::engine_workspace_root::getWorkspaceRoot(db);
    let root = root_v.as_str().unwrap_or_default().to_string();
    let workspace_id: i64 = crate::engine_workspace_root::getActiveWorkspace(db)
        .get("id")
        .and_then(Value::as_i64)
        .unwrap_or(0);

    let have = crate::db_json::query_rows(db, "SELECT COUNT(*) AS n FROM brain.code_connections WHERE workspace_id = ?", &[json!(workspace_id)])?;
    let have_n = have.as_array().and_then(|a| a.first()).and_then(|r| r.get("n")).and_then(Value::as_i64).unwrap_or(0);
    if rebuild_flag || have_n == 0 {
        let tracked = list_tracked(&root)?;
        rebuild_cache(db, workspace_id, &root, &tracked)?;
    }

    let imports: Vec<Value> = crate::db_json::query_rows(
        db,
        "SELECT specifier, to_path FROM brain.code_connections WHERE workspace_id = ? AND from_path = ? ORDER BY specifier",
        &[json!(workspace_id), json!(rel.clone())],
    )?
    .as_array()
    .cloned()
    .unwrap_or_default();
    let imported_by: Vec<Value> = crate::db_json::query_rows(
        db,
        "SELECT DISTINCT from_path FROM brain.code_connections WHERE workspace_id = ? AND to_path = ? ORDER BY from_path",
        &[json!(workspace_id), json!(rel.clone())],
    )?
    .as_array()
    .cloned()
    .unwrap_or_default()
    .iter()
    .map(|r| r.get("from_path").cloned().unwrap_or(Value::Null))
    .collect();

    // THE ATLAS KNOWS MORE THAN THIS CACHE (2026-10-10, found baking the intel for the React A/B): the cache above is built with a relative-path-only resolver, so `import x from 'shared/getComponentNameFromType'`
    // (a workspace package name, how React imports almost everything) resolved to nothing and the pack said "imported by 1 file" where 16 import it. The atlas's file-level import edges come from the workspace-aware
    // resolver and are rebuilt on every ingest; they are merged in (never replacing what the cache found), and an atlas that is absent or empty changes nothing.
    let atlas_in: Vec<String> = crate::db_json::query_rows(db, "SELECT DISTINCT from_node FROM code.code_edges WHERE origin = 'atlas' AND type = 'imports' AND to_node = ? ORDER BY from_node", &[json!(rel.clone())])
        .ok().and_then(|v| v.as_array().cloned()).unwrap_or_default().iter().filter_map(|r| r.get("from_node").and_then(Value::as_str).map(String::from)).collect();
    let atlas_out: Vec<String> = crate::db_json::query_rows(db, "SELECT DISTINCT to_node FROM code.code_edges WHERE origin = 'atlas' AND type = 'imports' AND from_node = ? ORDER BY to_node", &[json!(rel.clone())])
        .ok().and_then(|v| v.as_array().cloned()).unwrap_or_default().iter().filter_map(|r| r.get("to_node").and_then(Value::as_str).map(String::from)).collect();
    let (imports, imported_by) = merge_atlas(imports, imported_by, &atlas_in, &atlas_out);

    let internal_count = imports.iter().filter(|i| truthy(i.get("to_path"))).count() as i64;
    let external_count = imports.iter().filter(|i| !truthy(i.get("to_path"))).count() as i64;

    let mut out = Map::new();
    out.insert("path".into(), json!(rel));
    out.insert("imports".into(), json!(imports));
    out.insert("imported_by".into(), json!(imported_by));
    out.insert("internal_count".into(), json!(internal_count));
    out.insert("external_count".into(), json!(external_count));
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS brain").unwrap();
        c.execute_batch(
            "CREATE TABLE workspaces (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL UNIQUE, \
             root_path TEXT NOT NULL, vcs TEXT NOT NULL DEFAULT 'git', is_active INTEGER NOT NULL DEFAULT 0, \
             created_at TEXT NOT NULL DEFAULT '2026-01-01 00:00:00'); \
             CREATE TABLE brain.code_connections (id INTEGER PRIMARY KEY AUTOINCREMENT, workspace_id INTEGER NOT NULL, \
             from_path TEXT NOT NULL, specifier TEXT NOT NULL, to_path TEXT, \
             created_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')), \
             UNIQUE (workspace_id, from_path, specifier));",
        )
        .unwrap();
        c
    }
    fn seed(c: &Connection, workspace_id: i64, from_path: &str, specifier: &str, to_path: Option<&str>) {
        c.execute(
            "INSERT INTO brain.code_connections (workspace_id, from_path, specifier, to_path) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![workspace_id, from_path, specifier, to_path],
        )
        .unwrap();
    }

    #[test]
    fn cache_hit_returns_internal_and_external_edges_sorted_by_specifier() {
        let c = db();
        seed(&c, 0, "src/App.tsx", "./util", Some("src/util.ts"));
        seed(&c, 0, "src/App.tsx", "react", None);
        seed(&c, 0, "src/App.tsx", "./Button", Some("src/Button.tsx"));
        let r = codeImports(&json!({ "path": "src/App.tsx" }), &c).unwrap();
        assert_eq!(r["path"], json!("src/App.tsx"));
        assert_eq!(
            r["imports"],
            json!([
                { "specifier": "./Button", "to_path": "src/Button.tsx" },
                { "specifier": "./util", "to_path": "src/util.ts" },
                { "specifier": "react", "to_path": null },
            ])
        );
        assert_eq!(r["internal_count"], json!(2));
        assert_eq!(r["external_count"], json!(1));
        assert_eq!(r["imported_by"], json!([]));
    }

    #[test]
    fn backslash_path_is_normalized_to_forward_slash() {
        let c = db();
        seed(&c, 0, "src/app/App.tsx", "./x", None);
        let r = codeImports(&json!({ "path": "src\\app\\App.tsx" }), &c).unwrap();
        assert_eq!(r["path"], json!("src/app/App.tsx"));
        assert_eq!(r["imports"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn the_atlas_fills_in_importers_the_cache_could_not_resolve() {
        let (imp, by) = merge_atlas(vec![json!({ "specifier": "./a", "to_path": "src/a.js" }), json!({ "specifier": "shared/x", "to_path": null })], vec![json!("src/z.js")], &["src/b.js".to_string(), "src/z.js".to_string()], &["src/a.js".to_string(), "shared/x.js".to_string()]);
        assert_eq!(by, vec![json!("src/b.js"), json!("src/z.js")], "the union, sorted, once each");
        assert_eq!(imp.len(), 3, "an atlas target the cache lacked is added, one it had is not");
        assert_eq!(imp[2], json!({ "specifier": "(atlas)", "to_path": "shared/x.js" }));
        let (i2, b2) = merge_atlas(vec![], vec![json!("q")], &[], &[]);
        assert!(i2.is_empty() && b2 == vec![json!("q")], "an empty atlas changes nothing");
    }
    #[test]
    fn imported_by_reports_distinct_sorted_reverse_edges() {
        let c = db();
        seed(&c, 0, "b.mjs", "./util", Some("util.mjs"));
        seed(&c, 0, "a.mjs", "./util", Some("util.mjs"));
        seed(&c, 0, "a.mjs", "./util-again", Some("util.mjs")); // same from_path, DISTINCT collapses
        let r = codeImports(&json!({ "path": "util.mjs" }), &c).unwrap();
        assert_eq!(r["imported_by"], json!(["a.mjs", "b.mjs"]));
    }

    #[test]
    fn cache_present_but_no_matching_edges_does_not_trigger_rebuild() {
        let c = db();
        seed(&c, 0, "other.mjs", "./x", None); // any row → have.n > 0, cache considered warm
        let r = codeImports(&json!({ "path": "lonely.mjs" }), &c).unwrap();
        assert_eq!(r, json!({ "path": "lonely.mjs", "imports": [], "imported_by": [], "internal_count": 0, "external_count": 0 }));
    }

    #[test]
    fn guard_throw_is_not_swallowed() {
        let c = db();
        assert_eq!(codeImports(&json!({}), &c).unwrap_err(), "code-imports: path is required");
        assert_eq!(codeImports(&json!({ "path": "" }), &c).unwrap_err(), "code-imports: path is required");
    }

    #[test]
    fn posix_helpers_match_node() {
        assert_eq!(dirname("src/App.tsx"), "src");
        assert_eq!(normalize("././b.mjs"), "b.mjs");
        assert_eq!(join(&["src", "./util"]), "src/util");
        assert_eq!(strip_dot_slash("./src/util"), "src/util");
        let mut tracked = HashSet::new();
        tracked.insert("src/util.ts".to_string());
        assert_eq!(resolve_specifier("src/App.tsx", "./util", &tracked), Some("src/util.ts".to_string()));
        assert_eq!(resolve_specifier("src/App.tsx", "react", &tracked), None);
        // parseSpecifiers iterates SPEC_RES regex-by-regex (not position-by-position): the
        // import-from regex's matches land in the Set before export-from's, before bare-import's,
        // before require's — so insertion order is ['./a' (import-from), './d' (export-from),
        // './b' (bare import), './c' (require)], NOT the source's left-to-right textual order.
        assert_eq!(parse_specifiers("import x from './a';\nimport './b';\nrequire('./c');\nexport { y } from './d';"),
            vec!["./a".to_string(), "./d".to_string(), "./b".to_string(), "./c".to_string()]);
    }
}
