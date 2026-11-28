//! constitution-patterns — per-file DESIGN-PATTERN readout. REUSES the already-converted
//! `codebase_code_decorate::codeDecorate` + `codebase_atlas_entry_points::atlasEntryPoints` +
//! `codebase_code_patterns::detectFilePatterns` twins — the same deterministic detector
//! code-docs-build.rs uses (see code-docs-build.rs's `detectFilePatterns` call site). $0, no AI.
//!
//! input:  { path } + context.db
//! output: { path, patterns:[{pattern,confidence,evidence}], blocks }

use rusqlite::Connection;
use serde_json::{json, Value};

pub fn constitutionPatterns(input: &Value, db: &Connection) -> Result<Value, String> {
    let path = input.get("path").and_then(Value::as_str).unwrap_or("").replace('\\', "/").trim().to_string();
    if path.is_empty() { return Err("constitution-patterns: path is required".to_string()); }

    let blocks_v = match crate::codebase_code_decorate::codeDecorate(&json!({ "path": path.clone() }), db) {
        Ok(r) => r.get("decorators").cloned().unwrap_or_else(|| json!([])),
        Err(_) => json!([]),
    };
    let blocks_len = blocks_v.as_array().map(|a| a.len()).unwrap_or(0);

    let root_v = crate::engine_workspace_root::getWorkspaceRoot(db);
    let entries_v = match crate::codebase_atlas_entry_points::atlasEntryPoints(&json!({ "origin": "atlas", "root": root_v }), db) {
        Ok(r) => r.get("by_path").and_then(|bp| bp.get(&path)).cloned().unwrap_or_else(|| json!([])),
        Err(_) => json!([]),
    };

    let patterns = crate::codebase_code_patterns::detectFilePatterns(&blocks_v, &entries_v);

    Ok(json!({ "path": path, "patterns": patterns, "blocks": blocks_len as i64 }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE workspaces (id INTEGER PRIMARY KEY AUTOINCREMENT, root_path TEXT, is_active INTEGER);
             CREATE TABLE code_nodes (id TEXT, origin TEXT, path TEXT, type TEXT, name TEXT, signature TEXT, span_json TEXT);
             CREATE TABLE code_edges (origin TEXT, from_node TEXT, to_node TEXT, type TEXT, kind TEXT);
             CREATE TABLE code_decorators (id INTEGER PRIMARY KEY AUTOINCREMENT, origin TEXT, path TEXT, name TEXT, kind TEXT, role TEXT, line INTEGER, end_line INTEGER, signature TEXT, inputs_json TEXT, output_json TEXT, effects_json TEXT, calls_json TEXT, uses_json TEXT, complexity INTEGER, flags_json TEXT, summary TEXT, steps_json TEXT);
             CREATE TABLE code_entry_points (origin TEXT, node_id TEXT, path TEXT, kind TEXT, subtype TEXT, label TEXT, http_method TEXT, line INTEGER);"
        ).unwrap();
        c
    }

    #[test]
    fn missing_path_errors() {
        let r = constitutionPatterns(&json!({}), &db());
        assert!(r.is_err());
    }

    #[test]
    fn no_data_returns_empty_patterns() {
        let r = constitutionPatterns(&json!({ "path": "a/b.js" }), &db()).unwrap();
        assert_eq!(r["path"], json!("a/b.js"));
        assert_eq!(r["blocks"], json!(0));
        assert_eq!(r["patterns"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn backslashes_normalized() {
        let r = constitutionPatterns(&json!({ "path": "a\\b.js" }), &db()).unwrap();
        assert_eq!(r["path"], json!("a/b.js"));
    }
}
