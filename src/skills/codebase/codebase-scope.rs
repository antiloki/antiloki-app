//! codebase-scope.rs — twin of skills/codebase/codebase-scope.mjs, the "what's in the atlas"
//! preview gate: compute the in-scope source files under a root with optional include/exclude
//! globs, and tally them by language. NAMES ARE CONTRACT: `codebaseScope` (default).
//!
//! BESPOKE-NATIVE (thin wrapper over a real fs walk), not conform-db: dispatches to
//! `codebase_atlas_ingest::discover` (already-converted twin — recursive directory walk, the
//! same non-deterministic-shaped I/O class as atlas-ingest.rs / terminal-sandbox.rs) and
//! `codebase_ast_parse::adapterFor` (already-converted twin, pure/deterministic) to bucket each
//! discovered file by its adapter's first lang, falling back to `"unknown"`. Proven below via
//! `#[cfg(test)]` against a real temp dir — same class as the two twins it composes.
use serde_json::{json, Value};

#[allow(non_snake_case)]
pub fn codebaseScope(input: &Value, _context: &Value) -> Result<Value, String> {
    let obj = input.as_object();
    let root = match obj.and_then(|o| o.get("root")).and_then(|v| v.as_str()) {
        Some(r) if !r.is_empty() => r.to_string(),
        _ => return Err("codebase-scope: root is required".to_string()),
    };

    let include: Vec<String> = obj
        .and_then(|o| o.get("include"))
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
        .unwrap_or_default();
    let exclude: Vec<String> = obj
        .and_then(|o| o.get("exclude"))
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
        .unwrap_or_default();

    let files = crate::codebase_atlas_ingest::discover(&root, &include, &exclude);

    let mut by_lang = serde_json::Map::new();
    for f in &files {
        let adapter = crate::codebase_ast_parse::adapterFor(&json!(f));
        let lang = adapter
            .get("langs")
            .and_then(|l| l.get(0))
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string();
        let entry = by_lang.entry(lang).or_insert(json!(0));
        *entry = json!(entry.as_i64().unwrap_or(0) + 1);
    }

    Ok(json!({
        "root": root.replace('\\', "/"),
        "count": files.len(),
        "by_lang": Value::Object(by_lang),
        "files": files,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("antiloki-codebase-scope-test-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn missing_root_errors() {
        let e = codebaseScope(&json!({}), &Value::Null).unwrap_err();
        assert_eq!(e, "codebase-scope: root is required");
    }

    #[test]
    fn scopes_files_by_lang_and_normalizes_root() {
        let dir = temp_dir("basic");
        fs::write(dir.join("a.js"), "const a = 1;\n").unwrap();
        fs::write(dir.join("b.py"), "x = 1\n").unwrap();
        fs::write(dir.join("readme.txt"), "hi\n").unwrap();
        fs::create_dir_all(dir.join("node_modules")).unwrap();
        fs::write(dir.join("node_modules").join("c.js"), "const c = 1;\n").unwrap();

        let root = dir.to_string_lossy().replace('\\', "/");
        let r = codebaseScope(&json!({ "root": dir.to_string_lossy() }), &Value::Null).unwrap();
        assert_eq!(r["root"], json!(root));
        assert_eq!(r["count"], json!(2));
        assert_eq!(r["by_lang"]["javascript"], json!(1));
        assert_eq!(r["by_lang"]["python"], json!(1));
        let files = r["files"].as_array().unwrap();
        assert!(files.iter().any(|f| f == "a.js"));
        assert!(files.iter().any(|f| f == "b.py"));
        assert!(!files.iter().any(|f| f.as_str().unwrap().contains("node_modules")));

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn include_exclude_filters_apply() {
        let dir = temp_dir("filters");
        fs::write(dir.join("keep.js"), "const a = 1;\n").unwrap();
        fs::write(dir.join("skip.js"), "const b = 1;\n").unwrap();
        fs::write(dir.join("keep.test.js"), "const c = 1;\n").unwrap();

        let r = codebaseScope(
            &json!({ "root": dir.to_string_lossy(), "include": ["keep*.js"], "exclude": ["*.test.js"] }),
            &Value::Null,
        )
        .unwrap();
        assert_eq!(r["count"], json!(1));
        let files = r["files"].as_array().unwrap();
        assert_eq!(files[0], json!("keep.js"));

        fs::remove_dir_all(&dir).ok();
    }
}
