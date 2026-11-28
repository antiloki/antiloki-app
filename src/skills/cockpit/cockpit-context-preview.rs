//! cockpit-context-preview.rs — twin of skills/cockpit/cockpit-context-preview.mjs (db READ lane).
//! NAMES ARE CONTRACT: `cockpitContextPreview` (default). Resolves ONE source kind to its live
//! grounded markdown (the same text notebook-corpus `gather` assembles) so the Context Studio's
//! source-chip tiles can preview what the agent will actually consult.
//!
//! `_helpers/context-brief.mjs`'s `resolveSourceText` is a thin (11-line) wrapper over the
//! already-converted `notebooks_notebook_corpus::gather` twin — it is not itself an exported skill
//! unit, so its logic is inlined here (`resolve_source_text`) rather than given its own module.

use rusqlite::Connection;
use serde_json::{json, Map, Value};

// exported list mirrors the JS `SOURCE_KINDS` — the allowlist assemble-context-envelope's
// 'source' reference resolver also shares (see JS source header).
const SOURCE_KINDS: [&str; 21] = [
    "ast", "descriptions", "digest", "entrypoints", "data_stories", "design", "tests", "git", "docs",
    "cards", "contracts", "failures", "court", "decisions", "timeline", "runtime",
    "memory", "blueprint", "user_stories",
    "faculty_arch", "faculty_product",
];

// ── standard JS-coercion helpers (playbook) ──
fn str_qq(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(),
        Some(o) => o.to_string(),
    }
}

// = JS `resolveSourceText(db, kind, context)` from _helpers/context-brief.mjs: ast/descriptions
// preview over the atlas's own files (best-effort — a missing `code_nodes` table degrades to '',
// mirrored by query_rows().ok() swallowing the SQL error exactly like the JS try/catch).
fn resolve_source_text(db: &Connection, kind: &str) -> String {
    let mut paths: Vec<Value> = vec![];
    if kind == "ast" || kind == "descriptions" {
        paths = crate::db_json::query_rows(
            db,
            "SELECT path FROM code_nodes WHERE origin = 'atlas' AND type = 'file' ORDER BY path LIMIT 60",
            &[],
        )
        .ok()
        .and_then(|v| v.as_array().cloned())
        .unwrap_or_default()
        .iter()
        .map(|r| r.get("path").cloned().unwrap_or(Value::Null))
        .collect();
    }
    let scope = json!({ "paths": paths });
    let s = json!({ "kind": kind });
    let context = json!({});
    let result = crate::notebooks_notebook_corpus::gather(db, &s, &scope, &context);
    result.as_str().unwrap_or("").to_string()
}

pub fn cockpitContextPreview(input: &Value, db: &Connection) -> Result<Value, String> {
    let kind = str_qq(input.get("kind"));
    if let Some(slug) = kind.strip_prefix("pack:") {
        let text = crate::context_context_library::pack_body(db, slug).unwrap_or_default();
        return Ok(json!({ "ok": true, "kind": kind, "text": text, "chars": text.chars().count(), "note": if text.is_empty() { "no such pack in the library" } else { "" } }));
    }
    if !SOURCE_KINDS.contains(&kind.as_str()) {
        return Err(format!(
            "cockpit-context-preview: kind must be one of [{}], got '{}'",
            SOURCE_KINDS.join(", "),
            kind
        ));
    }
    let text = resolve_source_text(db, &kind);
    let capped: String = text.chars().take(8000).collect();
    let mut out = Map::new();
    out.insert("ok".into(), json!(true));
    out.insert("kind".into(), json!(kind));
    out.insert("text".into(), json!(capped));
    if text.is_empty() {
        out.insert("note".into(), json!("this source has no rows in this workspace yet"));
    }
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> Connection {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(
            "CREATE TABLE cards (slug TEXT, title TEXT, status TEXT, updated_at TEXT, created_at TEXT);
             CREATE TABLE code_nodes (path TEXT, origin TEXT, type TEXT, name TEXT, signature TEXT);",
        )
        .unwrap();
        db
    }

    #[test]
    fn cards_kind_has_rows_no_note() {
        let db = setup();
        db.execute(
            "INSERT INTO cards (slug, title, status, updated_at, created_at) VALUES ('CARD-1','First card','backlog','2026-01-01 00:00:00','2025-12-01 00:00:00')",
            [],
        )
        .unwrap();
        let out = cockpitContextPreview(&json!({ "kind": "cards" }), &db).unwrap();
        assert_eq!(out["ok"], json!(true));
        assert_eq!(out["kind"], json!("cards"));
        assert!(out["text"].as_str().unwrap().contains("CARD-1"));
        assert!(out.get("note").is_none());
    }

    #[test]
    fn digest_kind_empty_gets_note() {
        let db = setup();
        let out = cockpitContextPreview(&json!({ "kind": "digest" }), &db).unwrap();
        assert_eq!(out["ok"], json!(true));
        assert_eq!(out["text"], json!(""));
        assert_eq!(out["note"], json!("this source has no rows in this workspace yet"));
    }

    #[test]
    fn ast_kind_resolves_atlas_paths_then_gathers() {
        let db = setup();
        db.execute(
            "INSERT INTO code_nodes (path, origin, type, name, signature) VALUES ('src/foo.mjs','atlas','file',NULL,NULL)",
            [],
        )
        .unwrap();
        db.execute(
            "INSERT INTO code_nodes (path, origin, type, name, signature) VALUES ('src/foo.mjs','atlas','function','foo','function foo()')",
            [],
        )
        .unwrap();
        let out = cockpitContextPreview(&json!({ "kind": "ast" }), &db).unwrap();
        assert!(out["text"].as_str().unwrap().contains("src/foo.mjs"));
        assert!(out["text"].as_str().unwrap().contains("foo"));
    }
}
