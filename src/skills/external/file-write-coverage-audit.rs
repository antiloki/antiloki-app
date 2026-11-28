//! file-write-coverage-audit.rs — twin of skills/external/file-write-coverage-audit.mjs
//! (db lane; `context`/`db` unused — JS destructures neither). NAMES ARE CONTRACT:
//! `fileWriteCoverageAudit` (default), `isCovered` / `isIgnored` (named, no-db helpers).
//! `COVERED_PREFIXES` / `COVERED_FILES` / `IGNORED_PREFIXES` / `IGNORED_FILES` are the JS
//! module's re-exported DATA consts (arrays/Set) — not units per the census rule (only
//! functions count); transcribed here as Rust consts and used by both the helpers and the walk.
//!
//! Real recursive filesystem walk, not simulated. Unlike a roots.mjs-anchored fixed path
//! (SUBSTRATE_ROOT etc — bespoke-native per the crossing playbook, e.g. migrate-list.rs's
//! MIGRATIONS_DIR), the root here is a DIRECT INPUT PARAMETER (`input.root`, defaulting to
//! the self-located `PROJECT_ROOT` only when omitted) — even more directly fixture-controllable
//! than the DB-derived stack-facts precedent (no db indirection needed at all). Fixture cases
//! seed `args: [{ root: "<testdata dir>" }]` pointing at real on-disk trees committed under
//! `antiloki-crossing/fixtures/testdata/file-write-coverage-audit-*/`, alongside this unit's
//! fixture JSON — both lanes walk the identical bytes.
//!
//! The "root omitted" `PROJECT_ROOT` fallback (JS: `resolve(HERE, '..','..','..')` from this
//! skill's own file location, i.e. 3 hops up from `antiloki-core/skills/external/` to the outer
//! monorepo root) is implemented below for behavioral parity but deliberately NOT exercised —
//! neither by this unit's fixture NOR by a native `#[cfg(test)]` full walk — same convention as
//! stack-facts skipping its SUBSTRATE_ROOT fallback: walking the REAL repo root isn't just
//! environment-fragile here, it's genuinely expensive (`antiloki-core-rust/target/` and similar
//! build-artifact trees aren't in IGNORED_PREFIXES, so a real walk would recurse through
//! thousands of build outputs). `default_project_root()` mirrors it via `CARGO_MANIFEST_DIR`
//! (this crate's own dir, `antiloki-core-rust/`) `.parent()` — one hop lands on the same outer
//! monorepo root the JS computes in three, a coincidence of this repo's specific layout
//! (`antiloki-core-rust` sits directly under the root the JS skill is buried three levels
//! under), not a general HERE-to-root algorithm. The one native test for this path only proves
//! the arithmetic (non-empty, absolute), never walks it.
//!
//! `out.covered` / `out.uncovered` are `.sort()`-ed before being read (JS: `out.covered.sort();
//! out.uncovered.sort();`), so their content AND the derived `uncovered_by_top_level` /
//! `uncovered_sample` fields are fully deterministic regardless of the real OS's `readdir` order.
//! `out.ignored` is the one field the JS never sorts, and it is exposed only when
//! `include_full` is truthy (inside `full.ignored`). `readdir` order is OS-unspecified in
//! principle, but both lanes call the identical underlying syscall against the identical
//! committed directory on the same machine — still, the `-full` testdata fixture below is
//! deliberately built with exactly ONE ignored entry so no cross-lane ordering assumption is
//! ever exercised.
//!
//! Directories are NEVER pushed to any of the three lists — only files are (JS: `st.isFile()`
//! guards the covered/uncovered push; `isIgnored` is checked on every entry, file or dir, but a
//! directory whose OWN relative name doesn't yet carry a trailing path segment never matches an
//! IGNORED_PREFIX string that ends in `/` — e.g. relative name `"node_modules"` does NOT
//! `startsWith("node_modules/")` — so the walk recurses ONE level into ignored roots before the
//! prefix match fires on their children's relative paths; an EMPTY ignored-prefix directory is
//! therefore invisible in all three lists). This twin preserves that quirk exactly by porting
//! the JS control flow 1:1 (`is_ignored` checked before `is_dir`/`is_file`, dirs never pushed).
// QUALITY: 80/100 — real fs walk ported 1:1 (ignore-before-recurse quirk preserved), fixture-backed via committed testdata dirs

use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::path::Path;

const COVERED_PREFIXES: &[&str] = &[".antiloki/skills/", "bin/", "docs/"];
const COVERED_FILES: &[&str] = &[".mcp.json", "package.json", "tsconfig.json", ".npmrc", ".editorconfig", ".gitattributes"];
const IGNORED_PREFIXES: &[&str] = &[
    ".git/", "node_modules/", ".antiloki/auth/", ".antiloki/db/", ".antiloki/sandbox/",
    ".antiloki/tmp/", ".antiloki/run/", ".antiloki/learning-center/", ".antiloki/backups/",
    ".antiloki/knowledge/", ".obsidian/", ".claude/", ".cursor/", ".idea/", ".vscode/",
];
const IGNORED_FILES: &[&str] = &[
    ".gitignore", ".dockerignore", ".gitmodules", ".mailmap", ".travis.yml",
    "LICENSE.md", "AGENTS.md", "CLAUDE.md", "README.md", "ARCHITECTURE.md",
    "package-lock.json", "bun.lockb", "yarn.lock",
];

fn truthy(v: Option<&Value>) -> bool { match v { None|Some(Value::Null)=>false, Some(Value::Bool(b))=>*b, Some(Value::Number(n))=>n.as_f64().map_or(false,|f|f!=0.0&&!f.is_nan()), Some(Value::String(s))=>!s.is_empty(), Some(_)=>true } }

fn is_covered_str(rel: &str) -> bool {
    COVERED_PREFIXES.iter().any(|p| rel.starts_with(p)) || COVERED_FILES.contains(&rel)
}
fn is_ignored_str(rel: &str) -> bool {
    IGNORED_PREFIXES.iter().any(|p| rel.starts_with(p)) || IGNORED_FILES.contains(&rel)
}

pub fn isCovered(rel: &Value) -> Value { json!(is_covered_str(rel.as_str().unwrap_or(""))) }
pub fn isIgnored(rel: &Value) -> Value { json!(is_ignored_str(rel.as_str().unwrap_or(""))) }

struct WalkOut {
    covered: Vec<String>,
    uncovered: Vec<String>,
    ignored: Vec<String>,
}

// JS walk(dir, root, out): `root` stays fixed across the recursion; here the equivalent
// invariant is carried as `rel_prefix`, the already-known relative path down to `dir`, built up
// incrementally with forward slashes (same net string `relative(root, abs).replace(/\\/g,'/')`
// produces on Windows) rather than re-derived per entry via path-diffing.
fn walk(dir: &Path, rel_prefix: &str, out: &mut WalkOut) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        let name = entry.file_name().to_string_lossy().into_owned();
        let rel = if rel_prefix.is_empty() { name } else { format!("{rel_prefix}/{name}") };
        if is_ignored_str(&rel) {
            out.ignored.push(rel);
            continue;
        }
        let abs = entry.path();
        let meta = match std::fs::metadata(&abs) {
            Ok(m) => m,
            Err(_) => continue,
        };
        if meta.is_dir() {
            walk(&abs, &rel, out);
        } else if meta.is_file() {
            if is_covered_str(&rel) { out.covered.push(rel); } else { out.uncovered.push(rel); }
        }
    }
}

fn default_project_root() -> String {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default()
}

pub fn fileWriteCoverageAudit(input: &Value, _db: &Connection) -> Result<Value, String> {
    let root = input.get("root").and_then(Value::as_str).map(String::from).unwrap_or_else(default_project_root);

    let mut out = WalkOut { covered: Vec::new(), uncovered: Vec::new(), ignored: Vec::new() };
    walk(Path::new(&root), "", &mut out);
    out.covered.sort();
    out.uncovered.sort();

    // grouped[top] = (grouped[top] ?? 0) + 1, iterating the ALREADY-SORTED uncovered array —
    // `order` records first-occurrence position (JS object insertion order) for the stable
    // tie-break below.
    let mut order: Vec<String> = Vec::new();
    let mut counts: HashMap<String, i64> = HashMap::new();
    for p in &out.uncovered {
        let top = p.split('/').next().unwrap_or("").to_string();
        if !counts.contains_key(&top) { order.push(top.clone()); }
        *counts.entry(top).or_insert(0) += 1;
    }
    let mut top_entries: Vec<(String, i64)> = order.into_iter().map(|k| { let c = counts[&k]; (k, c) }).collect();
    top_entries.sort_by(|a, b| b.1.cmp(&a.1)); // Array.prototype.sort is stable: ties keep insertion order
    let mut grouped = Map::new();
    for (k, v) in top_entries { grouped.insert(k, json!(v)); }

    let mut scopes = Map::new();
    scopes.insert("prefixes".into(), json!(COVERED_PREFIXES));
    scopes.insert("files".into(), json!(COVERED_FILES));

    let uncovered_sample: Vec<Value> = out.uncovered.iter().take(30).map(|s| json!(s)).collect();

    let full = if truthy(input.get("include_full")) {
        json!({ "covered": out.covered, "uncovered": out.uncovered, "ignored": out.ignored })
    } else {
        Value::Null
    };

    let mut result = Map::new();
    result.insert("root".into(), json!(root));
    result.insert("scopes".into(), Value::Object(scopes));
    result.insert("covered_count".into(), json!(out.covered.len() as i64));
    result.insert("uncovered_count".into(), json!(out.uncovered.len() as i64));
    result.insert("ignored_count".into(), json!(out.ignored.len() as i64));
    result.insert("uncovered_by_top_level".into(), Value::Object(grouped));
    result.insert("uncovered_sample".into(), json!(uncovered_sample));
    result.insert("full".into(), full);
    Ok(Value::Object(result))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn db0() -> Connection { Connection::open_in_memory().unwrap() }

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!("antiloki-file-write-coverage-audit-rs-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }
    fn write_at(root: &Path, rel: &str) {
        let p = root.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(&p, "").unwrap();
    }

    #[test]
    fn is_covered_matches_prefixes_and_exact_files() {
        assert_eq!(isCovered(&json!(".antiloki/skills/foo.mjs")), json!(true));
        assert_eq!(isCovered(&json!("bin/x.mjs")), json!(true));
        assert_eq!(isCovered(&json!("docs/y.md")), json!(true));
        assert_eq!(isCovered(&json!(".mcp.json")), json!(true));
        assert_eq!(isCovered(&json!("src/secret.mjs")), json!(false));
        // bare dir name (no trailing segment) never satisfies a `/`-terminated prefix
        assert_eq!(isCovered(&json!("bin")), json!(false));
    }

    #[test]
    fn is_ignored_matches_well_known_noise() {
        assert_eq!(isIgnored(&json!(".git/HEAD")), json!(true));
        assert_eq!(isIgnored(&json!("node_modules/x")), json!(true));
        assert_eq!(isIgnored(&json!(".antiloki/db/state.sqlite")), json!(true));
        assert_eq!(isIgnored(&json!(".gitignore")), json!(true));
        assert_eq!(isIgnored(&json!("README.md")), json!(true));
        assert_eq!(isIgnored(&json!("src/keep.mjs")), json!(false));
        assert_eq!(isIgnored(&json!(".git")), json!(false));
    }

    #[test]
    fn audit_synthetic_project_tree_groups_as_expected() {
        let root = temp_dir("synthetic");
        // Covered
        write_at(&root, ".antiloki/skills/x.mjs");
        write_at(&root, "bin/y.mjs");
        write_at(&root, "package.json");
        // Uncovered (no substrate scope)
        write_at(&root, "src/escape.mjs");
        write_at(&root, "random.txt");
        // Ignored
        write_at(&root, ".git/HEAD");
        write_at(&root, ".gitignore");

        let r = fileWriteCoverageAudit(&json!({ "root": root.to_string_lossy() }), &db0()).unwrap();
        assert_eq!(r["covered_count"], json!(3));
        assert_eq!(r["uncovered_count"], json!(2));
        assert!(r["ignored_count"].as_i64().unwrap() >= 2);
        let sample = r["uncovered_sample"].as_array().unwrap();
        assert!(sample.contains(&json!("src/escape.mjs")));
        assert!(sample.contains(&json!("random.txt")));
        assert_eq!(r["full"], Value::Null);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn audit_groups_uncovered_by_top_level_descending_with_stable_ties() {
        let root = temp_dir("grouping");
        write_at(&root, "lib/a.js");
        write_at(&root, "lib/b.js");
        write_at(&root, "random.txt");
        write_at(&root, "scripts/build.sh");
        write_at(&root, "src/index.mjs");
        write_at(&root, "src/utils/helper.mjs");
        write_at(&root, "src/utils/other.mjs");

        let r = fileWriteCoverageAudit(&json!({ "root": root.to_string_lossy() }), &db0()).unwrap();
        let keys: Vec<&String> = r["uncovered_by_top_level"].as_object().unwrap().keys().collect();
        assert_eq!(keys, vec!["src", "lib", "random.txt", "scripts"]);
        assert_eq!(r["uncovered_by_top_level"]["src"], json!(3));
        assert_eq!(r["uncovered_by_top_level"]["lib"], json!(2));
        assert_eq!(r["uncovered_by_top_level"]["random.txt"], json!(1));
        assert_eq!(r["uncovered_by_top_level"]["scripts"], json!(1));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn audit_include_full_exposes_raw_sorted_lists_and_ignored() {
        let root = temp_dir("full");
        write_at(&root, "package.json");
        write_at(&root, "notes.md");
        write_at(&root, ".gitignore");

        let r = fileWriteCoverageAudit(&json!({ "root": root.to_string_lossy(), "include_full": true }), &db0()).unwrap();
        assert_eq!(r["full"]["covered"], json!(["package.json"]));
        assert_eq!(r["full"]["uncovered"], json!(["notes.md"]));
        assert_eq!(r["full"]["ignored"], json!([".gitignore"]));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn audit_empty_ignored_prefix_dir_is_invisible_in_every_list() {
        // An IGNORED_PREFIXES dir with nothing inside never gets pushed anywhere: its own bare
        // relative name never satisfies a `/`-terminated prefix, so the walk recurses into it,
        // finds no children, and nothing is added to covered/uncovered/ignored for it.
        let root = temp_dir("empty-ignored");
        fs::create_dir_all(root.join("node_modules")).unwrap();
        write_at(&root, "package.json");

        let r = fileWriteCoverageAudit(&json!({ "root": root.to_string_lossy() }), &db0()).unwrap();
        assert_eq!(r["covered_count"], json!(1));
        assert_eq!(r["uncovered_count"], json!(0));
        assert_eq!(r["ignored_count"], json!(0));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn default_project_root_is_a_nonempty_absolute_path() {
        // The "root omitted" fallback is deliberately NOT exercised via a real
        // `fileWriteCoverageAudit({})` call here (see module doc): the real monorepo root
        // contains unignored, unbounded build-artifact trees (e.g. `antiloki-core-rust/target/`
        // isn't in IGNORED_PREFIXES), so walking it in a unit test would be slow and
        // environment-fragile. This only proves the path-resolution arithmetic itself.
        let root = default_project_root();
        assert!(!root.is_empty());
        assert!(Path::new(&root).is_absolute());
    }
}
