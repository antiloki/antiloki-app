//! atlas-test-map.rs — twin of skills/codebase/atlas-test-map.mjs (db lane). NAMES ARE CONTRACT:
//! isTestPath (no-db) · atlasTestMap (default) · coveredPaths (transitive — returns a Set). Wave C.
// QUALITY: 81/100 — test-path regex, forward coverage map + inverse; coveredPaths verified transitively
use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::collections::{BTreeSet, HashMap};
use std::sync::OnceLock;
use regex::Regex;

fn test_re() -> &'static Regex { static C: OnceLock<Regex> = OnceLock::new(); C.get_or_init(|| Regex::new(r"(?i)\.test\.|\.spec\.|__tests__|(^|/)tests?/").unwrap()) }
fn js_str(v: &Value) -> String { match v { Value::String(s) => s.clone(), Value::Null => String::new(), o => o.to_string() } }

pub fn isTestPath(p: &Value) -> Value { json!(test_re().is_match(&js_str(p))) }
fn is_test(p: &str) -> bool { test_re().is_match(p) }

/// A file that carries its OWN tests. `#[cfg(test)]` is the module gate and `#[test]` the fn
/// attribute; either is proof the file is exercised. Deliberately not `isTestPath`'s business —
/// such a file is production source that happens to be tested, not a test file, and calling it one
/// would drop it out of the source population it belongs to.
fn inline_test_re() -> &'static Regex {
    static C: OnceLock<Regex> = OnceLock::new();
    C.get_or_init(|| Regex::new(r"#\[\s*cfg\s*\(\s*test\s*\)\s*\]|#\[\s*test\s*\]").unwrap())
}

pub fn atlasTestMap(input: &Value, db: &Connection) -> Result<Value, String> {
    let origin = input.get("origin").and_then(|v| v.as_str()).unwrap_or("atlas").to_string();
    let files = crate::db_json::query_rows(db, "SELECT id, path FROM code_nodes WHERE origin = ? AND type = 'file'", &[json!(origin)])?;
    let files = files.as_array().cloned().unwrap_or_default();
    let id_to_path: HashMap<String, String> = files.iter().map(|f| (js_str(f.get("id").unwrap_or(&Value::Null)), js_str(f.get("path").unwrap_or(&Value::Null)))).collect();
    let test_files: Vec<&Value> = files.iter().filter(|f| is_test(&js_str(f.get("path").unwrap_or(&Value::Null)))).collect();

    let mut tests: Vec<Value> = Vec::new();
    let mut cov_order: Vec<String> = Vec::new();               // source paths in first-seen order (JS object key order)
    let mut cov_map: HashMap<String, BTreeSet<String>> = HashMap::new();
    for t in &test_files {
        let t_id = js_str(t.get("id").unwrap_or(&Value::Null));
        let t_path = js_str(t.get("path").unwrap_or(&Value::Null));
        let mut covers: BTreeSet<String> = BTreeSet::new();    // [...covers].sort()
        let imports = crate::db_json::query_rows(db, &format!("SELECT DISTINCT to_node FROM code_edges WHERE origin = ? AND from_node = ? AND {}", crate::codebase_atlas_build::RUNTIME_IMPORTS), &[json!(origin), json!(t_id)])?;
        let mut targets: Vec<String> = imports.as_array().unwrap().iter().map(|r| js_str(r.get("to_node").unwrap_or(&Value::Null))).collect();
        // BARRELS: a test that imports `components/index.ts` is testing what the barrel
        // re-exports, not the barrel. Follow `reexport` edges one hop so `Editor.tsx` behind
        // `export {Editor} from './Editor'` counts as reached. One hop, on purpose: a barrel of
        // barrels is rare and a transitive walk would call the whole app "covered" by one import.
        let mut through_barrels: Vec<String> = Vec::new();
        for to in &targets {
            let re = crate::db_json::query_rows(db, "SELECT DISTINCT to_node FROM code_edges WHERE origin = ? AND from_node = ? AND type = 'imports' AND kind = 'reexport'", &[json!(origin), json!(to)])?;
            through_barrels.extend(re.as_array().unwrap().iter().map(|r| js_str(r.get("to_node").unwrap_or(&Value::Null))));
        }
        targets.extend(through_barrels);
        for to in targets {
            if let Some(p) = id_to_path.get(&to) {
                if !is_test(p) {
                    covers.insert(p.clone());
                    if !cov_map.contains_key(p) { cov_order.push(p.clone()); }
                    cov_map.entry(p.clone()).or_default().insert(t_path.clone());
                }
            }
        }
        tests.push(json!({ "path": t_path, "covers": covers.into_iter().collect::<Vec<_>>() }));
    }
    // ── INLINE TESTS: the languages that do not have a separate test FILE ───────────────────────
    //
    // Everything above is the JavaScript model: a `svc.test.ts` sits next to `svc.ts`, imports it,
    // and that import edge is the coverage link. Rust has neither half — its tests live INSIDE the
    // file in `#[cfg(test)] mod tests`, so there is no test file to match and no edge to follow.
    // Every .rs file therefore came back UNCOVERED, including ones with a dozen tests in them.
    //
    // That was not cosmetic. `covered` feeds the tests axis, the tests axis feeds health, and the
    // review then told the operator that a well-tested file was "uncovered — a break lands on every
    // arrow" and prescribed a JavaScript test file (`skills.test.rs`, `import { run } from …`) that
    // Rust would neither compile nor run. Measured on a real crate: skills.rs has `#[cfg(test)]`
    // with two `#[test]` fns and scored 19 on the tests axis.
    //
    // A file carrying its own tests covers ITSELF — that is what the language means by covered.
    // Read from disk rather than from the atlas because the test module's contents are ordinary
    // defs in the graph, indistinguishable from production code without the attribute.
    let inline_root = crate::engine_workspace_root::getWorkspaceRoot(db);
    let inline_root = js_str(&inline_root);
    if !inline_root.is_empty() {
        for f in &files {
            let p = js_str(f.get("path").unwrap_or(&Value::Null));
            if !p.to_lowercase().ends_with(".rs") || is_test(&p) || cov_map.contains_key(&p) {
                continue;
            }
            let abs = std::path::Path::new(&inline_root).join(&p);
            let Ok(src) = std::fs::read_to_string(&abs) else { continue };
            if inline_test_re().is_match(&src) {
                cov_order.push(p.clone());
                cov_map.entry(p.clone()).or_default().insert(p.clone());
                tests.push(json!({ "path": p, "covers": [p.clone()], "inline": true }));
            }
        }
    }

    // {src: [test,...] sorted}, keys in first-seen order
    let mut cbs: Map<String, Value> = Map::new();
    for k in &cov_order { cbs.insert(k.clone(), json!(cov_map[k].iter().cloned().collect::<Vec<_>>())); }
    let source_count = files.len() as i64 - test_files.len() as i64;
    let covered_count = cbs.len() as i64;
    // THE INSTRUMENT, stated: what could have measured coverage here. A test file that imports
    // nothing from the source set (a Playwright/Cypress spec driving a URL) is a real test this
    // static map cannot attribute — it is counted so the reader can say so instead of "0".
    let inline_files = tests.iter().filter(|t| t.get("inline").is_some()).count();
    let e2e_specs = tests.iter().filter(|t| t.get("inline").is_none() && t.get("covers").and_then(Value::as_array).map_or(true, |c| c.is_empty())).count();
    let instrument = json!({
        "test_files": test_files.len(),
        "static_links": tests.iter().filter(|t| t.get("inline").is_none() && t.get("covers").and_then(Value::as_array).map_or(false, |c| !c.is_empty())).count(),
        "inline_files": inline_files,
        "e2e_specs": e2e_specs,
        "lcov": false,
    });
    Ok(json!({
        "origin": origin, "test_count": test_files.len(), "source_count": source_count,
        "covered_count": covered_count, "uncovered_count": std::cmp::max(0, source_count - covered_count),
        "tests": tests, "coverage_by_source": Value::Object(cbs), "instrument": instrument,
    }))
}

// TRANSITIVE: the set of source paths with ≥1 test importing them.
pub fn coveredPaths(db: &Connection, origin: &str) -> Value {
    let m = atlasTestMap(&json!({ "origin": origin }), db).unwrap_or(json!({ "coverage_by_source": {} }));
    let keys: Vec<Value> = m.get("coverage_by_source").and_then(|v| v.as_object()).map(|o| o.keys().map(|k| json!(k)).collect()).unwrap_or_default();
    json!(keys)
}

/// `coveredPaths` plus the basis: what measured it. `basis` is `static-import` when test files
/// exist and import sources, `inline` when only `#[cfg(test)]` self-coverage exists, and
/// `unmeasured` when there is nothing that could have measured coverage (no test files at all,
/// or only specs that import nothing). `e2e_only` is the honest sub-case: tests exist, the
/// instrument cannot see what they exercise.
pub fn coveredPathsWithBasis(db: &Connection, origin: &str) -> (Value, String, bool) {
    let m = atlasTestMap(&json!({ "origin": origin }), db).unwrap_or(json!({ "coverage_by_source": {}, "instrument": {} }));
    let keys: Vec<Value> = m.get("coverage_by_source").and_then(|v| v.as_object()).map(|o| o.keys().map(|k| json!(k)).collect()).unwrap_or_default();
    let inst = m.get("instrument").cloned().unwrap_or(json!({}));
    let n = |k: &str| inst.get(k).and_then(Value::as_i64).unwrap_or(0);
    let e2e_only = n("test_files") > 0 && n("static_links") == 0 && n("inline_files") == 0 && n("e2e_specs") > 0;
    let basis = if n("static_links") > 0 {
        "static-import"
    } else if n("inline_files") > 0 {
        "inline"
    } else {
        "unmeasured"
    };
    (json!(keys), basis.to_string(), e2e_only)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn maps_coverage() {
        assert_eq!(isTestPath(&json!("a.test.ts")), json!(true));
        assert_eq!(isTestPath(&json!("src/svc.ts")), json!(false));
        assert_eq!(isTestPath(&json!("tests/e2e.ts")), json!(true));
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE code_nodes (id TEXT, origin TEXT, path TEXT, type TEXT); CREATE TABLE code_edges (origin TEXT, from_node TEXT, to_node TEXT, type TEXT, kind TEXT); \
          INSERT INTO code_nodes VALUES ('svc@','atlas','src/svc.ts','file'),('util@','atlas','src/util.ts','file'),('t@','atlas','src/svc.test.ts','file'); \
          INSERT INTO code_edges (origin,from_node,to_node,type) VALUES ('atlas','t@','svc@','imports');").unwrap();
        let r = atlasTestMap(&json!({}), &c).unwrap();
        assert_eq!(r["test_count"], json!(1));
        assert_eq!(r["source_count"], json!(2));
        assert_eq!(r["coverage_by_source"]["src/svc.ts"], json!(["src/svc.test.ts"]));
        assert_eq!(r["uncovered_count"], json!(1));
        assert_eq!(coveredPaths(&c, "atlas"), json!(["src/svc.ts"]));
    }

    #[test]
    fn instrument_says_what_measured_coverage_and_when_nothing_could() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE code_nodes (id TEXT, origin TEXT, path TEXT, type TEXT); CREATE TABLE code_edges (origin TEXT, from_node TEXT, to_node TEXT, type TEXT, kind TEXT); \
          INSERT INTO code_nodes VALUES ('svc@','atlas','src/svc.ts','file'),('e2e@','atlas','__tests__/e2e/page.spec.ts','file');").unwrap();
        // a Playwright spec that imports nothing: a test exists, the static map cannot see what it exercises
        let (paths, basis, e2e_only) = coveredPathsWithBasis(&c, "atlas");
        assert_eq!(paths, json!([]));
        assert_eq!(basis, "unmeasured");
        assert!(e2e_only, "tests exist but none is attributable");
        let r = atlasTestMap(&json!({}), &c).unwrap();
        assert_eq!(r["instrument"]["test_files"], json!(1));
        assert_eq!(r["instrument"]["e2e_specs"], json!(1));
        assert_eq!(r["instrument"]["static_links"], json!(0));

        c.execute("INSERT INTO code_edges (origin,from_node,to_node,type) VALUES ('atlas','e2e@','svc@','imports')", []).unwrap();
        let (_, basis, e2e_only) = coveredPathsWithBasis(&c, "atlas");
        assert_eq!(basis, "static-import");
        assert!(!e2e_only);

        let empty = Connection::open_in_memory().unwrap();
        empty.execute_batch("CREATE TABLE code_nodes (id TEXT, origin TEXT, path TEXT, type TEXT); CREATE TABLE code_edges (origin TEXT, from_node TEXT, to_node TEXT, type TEXT, kind TEXT); \
          INSERT INTO code_nodes VALUES ('svc@','atlas','src/svc.ts','file');").unwrap();
        let (_, basis, e2e_only) = coveredPathsWithBasis(&empty, "atlas");
        assert_eq!(basis, "unmeasured");
        assert!(!e2e_only, "no tests at all is plain unmeasured");
    }

    #[test]
    fn a_test_that_imports_a_barrel_covers_what_the_barrel_reexports() {
        // `page.test.tsx` imports `components/index.ts`; the index re-exports Editor and Header.
        // Static coverage used to stop at the barrel — the two real files stayed "uncovered".
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE code_nodes (id TEXT, origin TEXT, path TEXT, type TEXT); CREATE TABLE code_edges (origin TEXT, from_node TEXT, to_node TEXT, type TEXT, kind TEXT); \
          INSERT INTO code_nodes VALUES ('idx@','atlas','src/index.ts','file'),('ed@','atlas','src/Editor.tsx','file'),('hd@','atlas','src/Header.tsx','file'),('far@','atlas','src/Far.tsx','file'),('t@','atlas','src/page.test.tsx','file'); \
          INSERT INTO code_edges (origin,from_node,to_node,type,kind) VALUES ('atlas','t@','idx@','imports','value'),('atlas','idx@','ed@','imports','reexport'),('atlas','idx@','hd@','imports','reexport'),('atlas','ed@','far@','imports','value');").unwrap();
        let r = atlasTestMap(&json!({}), &c).unwrap();
        assert_eq!(r["coverage_by_source"]["src/Editor.tsx"], json!(["src/page.test.tsx"]));
        assert_eq!(r["coverage_by_source"]["src/Header.tsx"], json!(["src/page.test.tsx"]));
        assert!(r["coverage_by_source"].get("src/Far.tsx").is_none(), "one hop only — a value import behind the barrel is not reached");
    }

    #[test]
    fn rust_files_with_inline_cfg_test_count_as_covered() {
        // THE REGRESSION: coverage was an import edge from a separate `x.test.ts`. Rust has no such
        // file — `#[cfg(test)] mod tests` lives inside the source — so every .rs file came back
        // uncovered no matter how well tested. That fed the tests axis, which fed health, and the
        // review then told the operator to add a JavaScript test file to a Rust crate.
        let dir = std::env::temp_dir().join(format!("atlas-testmap-inline-{}", std::process::id()));
        let src = dir.join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("tested.rs"),
            "pub fn run() -> i64 { 1 }\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn works() { assert_eq!(super::run(), 1); }\n}\n").unwrap();
        std::fs::write(src.join("bare.rs"), "pub fn other() -> i64 { 2 }\n").unwrap();

        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE code_nodes (id TEXT, origin TEXT, path TEXT, type TEXT); \
             CREATE TABLE code_edges (origin TEXT, from_node TEXT, to_node TEXT, type TEXT, kind TEXT); \
             CREATE TABLE workspaces (id INTEGER PRIMARY KEY, name TEXT, root_path TEXT, vcs TEXT, is_active INTEGER, created_at TEXT); \
             INSERT INTO code_nodes VALUES ('a@','atlas','src/tested.rs','file'),('b@','atlas','src/bare.rs','file');",
        ).unwrap();
        c.execute("INSERT INTO workspaces (name, root_path, is_active) VALUES ('t', ?1, 1)",
                  rusqlite::params![dir.to_string_lossy()]).unwrap();

        let r = atlasTestMap(&json!({}), &c).unwrap();
        let cov = &r["coverage_by_source"];
        assert_eq!(cov["src/tested.rs"], json!(["src/tested.rs"]), "a file with #[cfg(test)] covers itself");
        assert!(cov.get("src/bare.rs").is_none(), "a file with no tests stays uncovered");
        // It is COVERED source, not a test file — it must remain in the source population.
        assert_eq!(r["source_count"], json!(2), "inline tests must not reclassify source as a test file");
        assert_eq!(r["uncovered_count"], json!(1));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
