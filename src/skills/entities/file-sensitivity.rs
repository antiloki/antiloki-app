//! file-sensitivity.rs — twin of skills/entities/file-sensitivity.mjs (db READ lane). NAMES ARE
//! CONTRACT: `fileSensitivityMatch` (default). glob → risk-weight lookup; worst-case (max) weight wins.
//! `globToRegex` is verified TRANSITIVELY (it returns a RegExp — not JSON-conformable — exercised here).
// QUALITY: 79/100 — simple glob (* non-slash, ** any-depth, ? one), max-weight over all matching patterns
use regex::Regex;
use rusqlite::Connection;
use serde_json::{json, Value};

// glob → anchored regex: escape specials (not * ?), ** → any-depth, * → non-slash run, ? → one char.
// NAMES ARE CONTRACT: `globToRegex` is the JS export; verified TRANSITIVELY (RegExp return isn't
// JSON-conformable) via fileSensitivityMatch, which exercises it below.
pub fn globToRegex(glob: &str) -> Regex {
    let specials = Regex::new(r"[.+^${}()|\[\]\\]").unwrap();
    let escaped = specials.replace_all(glob, |c: &regex::Captures| format!("\\{}", &c[0]));
    let s = escaped
        .replace("**", "__DOUBLESTAR__")
        .replace('*', "[^/]*")
        .replace("__DOUBLESTAR__", ".*")
        .replace('?', ".");
    Regex::new(&format!("^{}$", s)).unwrap() // escaping guarantees validity (JS `new RegExp` throws on invalid)
}

pub fn fileSensitivityMatch(input: &Value, db: &Connection) -> Result<Value, String> {
    let path = match input.get("path").and_then(|v| v.as_str()) {
        Some(p) if !p.is_empty() => p,
        _ => return Err("file-sensitivity: 'path' is required".into()),
    };
    let rows = crate::db_json::query_rows(db, "SELECT path_glob, risk_weight, description FROM file_sensitivity", &[])
        .ok().and_then(|r| r.as_array().cloned()).unwrap_or_default();

    let mut matches: Vec<Value> = Vec::new();
    for r in &rows {
        let glob = r.get("path_glob").and_then(|v| v.as_str()).unwrap_or("");
        if globToRegex(glob).is_match(path) { matches.push(r.clone()); }
    }
    let max_weight = if matches.is_empty() { json!(0) } else {
        matches.iter().map(|m| m.get("risk_weight").cloned().unwrap_or(json!(0)))
            .max_by(|a, b| a.as_f64().unwrap_or(0.0).partial_cmp(&b.as_f64().unwrap_or(0.0)).unwrap_or(std::cmp::Ordering::Equal))
            .unwrap()
    };
    Ok(json!({ "path": path, "match_count": matches.len(), "max_weight": max_weight, "matches": matches }))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn matches_and_weights() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE file_sensitivity (path_glob TEXT, risk_weight INTEGER, description TEXT); \
          INSERT INTO file_sensitivity VALUES ('**/*.env', 5, 'secrets'), ('src/**', 2, 'source'), ('src/auth/*.ts', 4, 'auth');").unwrap();
        let r = fileSensitivityMatch(&json!({ "path": "src/auth/login.ts" }), &c).unwrap();
        assert_eq!(r["match_count"], json!(2)); // src/** and src/auth/*.ts
        assert_eq!(r["max_weight"], json!(4));
        assert_eq!(r["matches"][0]["path_glob"], json!("src/**")); // row order preserved
        let none = fileSensitivityMatch(&json!({ "path": "README.md" }), &c).unwrap();
        assert_eq!(none["match_count"], json!(0));
        assert_eq!(none["max_weight"], json!(0));
        // glob semantics: * stops at /, ** crosses
        assert!(globToRegex("**/*.env").is_match("a/b/c.env"));
        assert!(!globToRegex("src/*.ts").is_match("src/deep/x.ts"));
    }
}
