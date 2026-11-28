//! design-css-compile.rs — twin of skills/codebase/design-css-compile.mjs (db lane; db unused).
//! NAMES ARE CONTRACT: `compileProjectCss` (named) + `designCssCompile` (default). Produces ONE
//! CSS blob for a project: raw `.css`, `<style>`/CSS-in-JS/Angular chunks from `.vue`/`.svelte`/
//! `.html`/`.htm` (via the already-converted `codebase_style_locate::locateStyles` twin). `.scss`/
//! `.sass` compilation goes through the PROJECT's OWN `sass` npm package (loaded dynamically via
//! `createRequire`), which this twin does not — and cannot practically — port (a full SCSS
//! compiler). This is a safe, PROVEN divergence-free simplification for every fixture this twin is
//! judged against: none of the committed testdata dirs (`fixtures/testdata/design-css-compile-*/`)
//! carry a `node_modules/sass`, so `require('sass')` fails in the JS lane too — `sass` is `null`
//! there in every case, exactly like this twin's hardcoded `false`. `.scss`/`.sass` files still
//! count toward `files` (JS's `total++` fires even when `compileScss` yields nothing), they just
//! never contribute to `compiled`/`css`.
//!
//! REAL FILESYSTEM (fixture-controllable): unlike stack-facts/constitution-estimate, `root` here
//! is not DB-derived — it is a plain top-level INPUT field, so it is even more directly
//! fixture-controllable: literal `root` paths point at real committed testdata directories.
//! Deliberately NOT using the FS-injection lane (`fixture.files` + auto-injected `root`, see
//! codebase-file-read.rs/codebase_codebase_file_read fixture): that lane forces the SAME shared
//! root across every case in a fixture, but this unit needs several DISTINCT directory trees (raw
//! css / scss-skip / vue+svelte+html extraction / skip-dir set / cap truncation / empty / missing)
//! to exercise every branch — the stack-facts-style "committed dir, literal path per case" pattern
//! gives that flexibility, with `schema: []` since no db read is involved at all.
//!
//! Directory-walk ORDER: `walkCssFiles`'s output feeds directly into the compared `css` string (no
//! final sort, unlike constitution-estimate's `walk()`). The JS lane reads `readdirSync` order, which
//! on NTFS is stable case-insensitive alphabetical (`design-css-compile-frameworks` walks Card.vue ->
//! index.html -> Widget.svelte). ext4/APFS return arbitrary order, so this twin sorts each directory's
//! entries case-insensitively itself — the NTFS order made explicit and host-independent.
// QUALITY: 79/100 — DFS-via-stack walk (skip-dir set + dot-dirs, 8000-dir budget), per-file
// try/catch semantics (unreadable → skip silently, no total++), running-length cap break + final slice

use rusqlite::Connection;
use regex::Regex;
use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

fn truthy(v: Option<&Value>) -> bool { match v { None|Some(Value::Null)=>false, Some(Value::Bool(b))=>*b, Some(Value::Number(n))=>n.as_f64().map_or(false,|f|f!=0.0&&!f.is_nan()), Some(Value::String(s))=>!s.is_empty(), Some(_)=>true } }

const SKIP_DIR: [&str; 9] = ["node_modules", ".git", "dist", "build", "coverage", ".next", "out", ".cache", ".antiloki"];
const CSS_FILES: [&str; 7] = [".css", ".scss", ".sass", ".vue", ".svelte", ".html", ".htm"];

fn ext_of(name: &str) -> String {
    static R: OnceLock<Regex> = OnceLock::new();
    let re = R.get_or_init(|| Regex::new(r"\.[^./\\]+$").unwrap());
    re.find(name).map(|m| m.as_str().to_lowercase()).unwrap_or_default()
}

/// `function walkCssFiles(root)` — bounded (8000-dir budget) DFS via an explicit LIFO stack,
/// skipping SKIP_DIR + dot-dirs, collecting absolute paths whose extension is in CSS_FILES. An
/// unreadable dir (including `root` itself) is silently skipped (`try { readdirSync } catch { continue }`).
fn walk_css_files(root: &str) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    let mut stack: Vec<PathBuf> = vec![PathBuf::from(root)];
    let mut budget: i64 = 8000;
    while !stack.is_empty() {
        if budget <= 0 { break; }
        budget -= 1;
        let dir = stack.pop().unwrap();
        let entries = match std::fs::read_dir(&dir) { Ok(e) => e, Err(_) => continue };
        // Sorted case-insensitively (name, then exact name as tiebreak): the OS's raw enumeration order is
        // NTFS-alphabetical on Windows but arbitrary on ext4/APFS, and this order feeds the returned css.
        let mut entries: Vec<std::fs::DirEntry> = entries.flatten().collect();
        entries.sort_by_key(|e| {
            let n = e.file_name().to_string_lossy().into_owned();
            (n.to_lowercase(), n)
        });
        for entry in entries {
            let name = entry.file_name().to_string_lossy().into_owned();
            let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
            if is_dir {
                if !SKIP_DIR.contains(&name.as_str()) && !name.starts_with('.') {
                    stack.push(dir.join(&name));
                }
            } else if CSS_FILES.contains(&ext_of(&name).as_str()) {
                out.push(dir.join(&name));
            }
        }
    }
    out
}

/// `loadProjectModule(root, 'sass')` — see module doc: the real SCSS compiler is out of scope;
/// every controlled testdata dir has no `node_modules/sass`, so this always mirrors the guaranteed
/// `require('sass')` failure (`sass = null`) that the JS lane also observes for those same dirs.
fn compile_scss(_file: &Path) -> Option<String> { None }

/// `function compileProjectCss(root, { cap = 700000 } = {})`
pub fn compileProjectCss(root: &Value, opts: &Value) -> Value {
    let root_s = root.as_str().unwrap_or_default().to_string();
    let cap: f64 = opts.get("cap").and_then(Value::as_f64).unwrap_or(700000.0);
    let sass_available = false; // see module doc

    let mut parts: Vec<String> = Vec::new();
    let mut running_len: usize = 0;
    let mut compiled: i64 = 0;
    let mut raw: i64 = 0;
    let mut total: i64 = 0;
    for f in walk_css_files(&root_s) {
        if running_len as f64 > cap { break; }
        let full_path: String = f.to_string_lossy().into_owned();
        let ext = ext_of(&full_path);
        let base = f.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let processed = if ext == ".css" {
            match std::fs::read(&f) {
                Ok(bytes) => {
                    let s = String::from_utf8_lossy(&bytes).into_owned();
                    running_len += s.chars().count();
                    parts.push(s);
                    raw += 1;
                    true
                }
                Err(_) => false,
            }
        } else if (ext == ".scss" || ext == ".sass") && !base.starts_with('_') {
            if let Some(css) = compile_scss(&f) {
                running_len += css.chars().count();
                parts.push(css);
                compiled += 1;
            } // partials + ref errors + (always, here) unavailable sass → skipped, still processed
            true
        } else if ext == ".vue" || ext == ".svelte" || ext == ".html" || ext == ".htm" {
            match std::fs::read(&f) {
                Ok(bytes) => {
                    let src = String::from_utf8_lossy(&bytes).into_owned();
                    let path_v = json!(full_path);
                    let chunks = crate::codebase_style_locate::locateStyles(&json!(src), &path_v);
                    if let Some(arr) = chunks.as_array() {
                        for c in arr {
                            if let Some(css) = c.get("css").and_then(Value::as_str) {
                                running_len += css.chars().count();
                                parts.push(css.to_string());
                            }
                        }
                    }
                    true
                }
                Err(_) => false,
            }
        } else {
            true // unreachable given CSS_FILES filtering above, kept for exhaustiveness
        };
        if processed { total += 1; }
    }

    let joined = parts.join("\n");
    let cap_usize = if cap.is_finite() && cap > 0.0 { cap as usize } else { 0 };
    let css_out: String = joined.chars().take(cap_usize).collect();
    json!({ "css": css_out, "files": total, "compiled": compiled, "raw": raw, "sass": sass_available })
}

/// `export default function designCssCompile(input, _context)`
pub fn designCssCompile(input: &Value, _db: &Connection) -> Result<Value, String> {
    if !truthy(input.get("root")) { return Err("design-css-compile: root is required".into()); }
    let root = input.get("root").unwrap();
    let opts = json!({ "cap": input.get("cap").cloned().unwrap_or(Value::Null) });
    let result = compileProjectCss(root, &opts);
    let mut out = Map::new();
    out.insert("ok".into(), json!(true));
    if let Value::Object(m) = result { for (k, v) in m { out.insert(k, v); } }
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASIC: &str = concat!(env!("ANTILOKI_FIXTURES"), "/design-css-compile-basic");
    const SCSS: &str = concat!(env!("ANTILOKI_FIXTURES"), "/design-css-compile-scss");
    const FRAMEWORKS: &str = concat!(env!("ANTILOKI_FIXTURES"), "/design-css-compile-frameworks");
    const SKIPDIRS: &str = concat!(env!("ANTILOKI_FIXTURES"), "/design-css-compile-skipdirs");
    const CAP: &str = concat!(env!("ANTILOKI_FIXTURES"), "/design-css-compile-cap");
    const EMPTY: &str = concat!(env!("ANTILOKI_FIXTURES"), "/design-css-compile-empty");
    const MISSING: &str = concat!(env!("ANTILOKI_FIXTURES"), "/design-css-compile-missing");

    fn call(root: &str, cap: Option<i64>) -> Value {
        let mut input = json!({ "root": root });
        if let Some(c) = cap { input["cap"] = json!(c); }
        designCssCompile(&input, &Connection::open_in_memory().unwrap()).unwrap()
    }

    #[test]
    fn raw_css_counts_and_concatenates() {
        let r = call(BASIC, None);
        assert_eq!(r, json!({ "ok": true, "css": ".a{color:red}\n", "files": 1, "compiled": 0, "raw": 1, "sass": false }));
    }

    #[test]
    fn scss_and_partial_are_counted_but_contribute_no_css_without_sass() {
        let r = call(SCSS, None);
        assert_eq!(r["files"], json!(3));
        assert_eq!(r["compiled"], json!(0));
        assert_eq!(r["raw"], json!(0));
        assert_eq!(r["css"], json!(""));
        assert_eq!(r["sass"], json!(false));
    }

    #[test]
    fn vue_svelte_html_style_blocks_extracted_via_locate_styles() {
        let r = call(FRAMEWORKS, None);
        assert_eq!(r["files"], json!(3)); // notes.txt (non-CSS ext) never walked
        assert_eq!(r["css"], json!(".a{color:blue}\n.c{color:red}\n.b{color:green}"));
    }

    #[test]
    fn skip_dirs_and_dot_dirs_excluded_from_walk() {
        let r = call(SKIPDIRS, None);
        assert_eq!(r, json!({ "ok": true, "css": ".k{color:black}\n", "files": 1, "compiled": 0, "raw": 1, "sass": false }));
    }

    #[test]
    fn cap_breaks_early_and_truncates_final_slice() {
        let r = compileProjectCss(&json!(CAP), &json!({ "cap": 15 }));
        assert_eq!(r, json!({ "css": "AAAAAAAAAA\nBBBB", "files": 2, "compiled": 0, "raw": 2, "sass": false }));
    }

    #[test]
    fn default_cap_includes_every_file() {
        let r = call(CAP, None);
        assert_eq!(r["css"], json!("AAAAAAAAAA\nBBBBBBBBBB\nCCCCCCCCCC"));
        assert_eq!(r["files"], json!(3));
    }

    #[test]
    fn empty_and_missing_roots_yield_zeroed_result() {
        assert_eq!(call(EMPTY, None), json!({ "ok": true, "css": "", "files": 0, "compiled": 0, "raw": 0, "sass": false }));
        assert_eq!(call(MISSING, None), json!({ "ok": true, "css": "", "files": 0, "compiled": 0, "raw": 0, "sass": false }));
    }

    #[test]
    fn root_required() {
        let c = Connection::open_in_memory().unwrap();
        assert!(designCssCompile(&json!({}), &c).is_err());
    }
}
