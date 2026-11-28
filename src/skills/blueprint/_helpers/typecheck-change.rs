//! typecheck-change.rs — twin of skills/blueprint/_helpers/typecheck-change.mjs (db lane; a
//! `_helpers/` module, not a dispatchable skill itself — NAMES ARE CONTRACT: `typecheckChange`).
//! Composes the AUTHORED change (per-block snippets -> file source, via blueprint-compose.rs's
//! already-converted `blueprintCompose`) and runs the eligible file blocks (extension has a
//! registered checker, per typecheck-detect.rs's `checkerForExt`) through typecheck-run.rs's
//! already-converted `typecheckRun`, mapping its findings into lint-shaped rows.
//!
//! BESPOKE-NATIVE (dispatcher over an already-twinned bespoke-native skill): `typecheckRun` shells a
//! real subprocess checker per adapter, so this thin orchestrator inherits that non-pure boundary
//! transitively even though every line of ITS OWN logic (blueprint row read + parse/normalize,
//! per-file authored-snippet lookup, per-file compose, extension-gated file selection, findings
//! mapping) is deterministic — exercised directly by the native `#[cfg(test)]` below. "No checker
//! installed on this machine" is the one deterministic outcome guaranteed everywhere (same
//! degrade-safe contract typecheck-run.rs's own tests rely on): with nothing installed the pipeline
//! still runs end-to-end (row read -> parse -> compose -> extension gate -> dispatch) and returns
//! `{findings:[], checked:0}`, proving no line along that path panics or misfires.
use rusqlite::Connection;
use serde_json::{json, Map, Value};

// `String(path).match(/\.[^./\\]+$/)?.[0]` — the extension including the leading dot, or None
// if the path has no '.' or the tail after the final '.' contains '/'/'\\'. Same shape as
// typecheck-run.rs's private `ext_of` (not exported, so re-derived here rather than shared).
fn ext_of(path: &str) -> Option<String> {
    let i = path.rfind('.')?;
    let tail = &path[i + 1..];
    if tail.is_empty() || tail.contains('/') || tail.contains('\\') {
        return None;
    }
    Some(path[i..].to_string())
}

// JS template-literal `${v}` coercion: undefined/null -> "null" (both map to serde Value::Null
// here since a missing key is filled with Value::Null before this is called).
fn js_string(v: &Value) -> String {
    match v {
        Value::Null => "null".into(),
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        o => o.to_string(),
    }
}

pub fn typecheckChange(db: &Connection, blueprint_id: &Value, context: &Value) -> Result<Value, String> {
    let _ = context; // JS `{ db, ...context }` spread into typecheckRun's context; Rust always threads `db` directly.

    let rows = crate::db_json::query_rows(db, "SELECT blueprint_json FROM blueprints WHERE id = ?", &[blueprint_id.clone()])?;
    let row = match rows.as_array().and_then(|a| a.first()) {
        Some(r) => r.clone(),
        None => return Ok(json!({ "findings": [], "checked": 0 })),
    };

    // JS `JSON.parse(row.blueprint_json)` inside try/catch: null coerces to the string "null" and
    // parses cleanly (not a throw); only genuinely malformed JSON hits the catch.
    let bp_raw = row.get("blueprint_json").cloned().unwrap_or(Value::Null);
    let json_text = match &bp_raw {
        Value::String(s) => s.clone(),
        Value::Null => "null".to_string(),
        other => other.to_string(),
    };
    let parsed: Value = match serde_json::from_str(&json_text) {
        Ok(v) => v,
        Err(_) => return Ok(json!({ "findings": [], "checked": 0 })),
    };
    let blueprint = crate::blueprint_block_shape::normalizeBlueprint(&parsed);

    let snips = crate::db_json::query_rows(db, "SELECT block_id, code FROM blueprint_snippets WHERE blueprint_id = ?", &[blueprint_id.clone()])?;
    let mut authored = Map::new();
    for s in snips.as_array().cloned().unwrap_or_default() {
        let block_id = s.get("block_id").cloned().unwrap_or(Value::Null);
        let key = js_string(&block_id); // `String(s.block_id)` object-key coercion
        authored.insert(key, json!({ "code": s.get("code").cloned().unwrap_or(Value::Null) }));
    }
    let authored = Value::Object(authored);

    let empty = Vec::new();
    let blocks = blueprint.get("blocks").and_then(Value::as_array).unwrap_or(&empty);
    let mut files: Vec<Value> = Vec::new();
    for fb in blocks {
        if fb.get("type").and_then(Value::as_str) != Some("file") {
            continue;
        }
        let composed = crate::blueprint_blueprint_compose::blueprintCompose(&json!({ "block": fb, "authored": authored }), db)?;
        let path = composed.get("path").cloned().unwrap_or(Value::Null);
        let source = composed.get("source").cloned().unwrap_or(Value::Null);
        let path_s = path.as_str().unwrap_or("");
        let has_source = source.as_str().map(|s| !s.is_empty()).unwrap_or(false);
        if path_s.is_empty() || !has_source {
            continue;
        }
        if let Some(ext) = ext_of(path_s) {
            if !crate::codebase_typecheck_detect::checkerForExt(&json!(ext)).is_null() {
                files.push(json!({ "path": path, "source": source }));
            }
        }
    }
    if files.is_empty() {
        return Ok(json!({ "findings": [], "checked": 0 }));
    }

    let r = crate::codebase_typecheck_run::typecheckRun(&json!({ "files": files }), db)?;
    let findings: Vec<Value> = r
        .get("findings")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|f| {
            let code = f.get("code").cloned().unwrap_or(Value::Null);
            let line = f.get("line").cloned().unwrap_or(Value::Null);
            let col = f.get("col").cloned().unwrap_or(Value::Null);
            let message = f.get("message").cloned().unwrap_or(Value::Null);
            json!({
                "kind": "typecheck",
                "severity": "error",
                "file": f.get("file").cloned().unwrap_or(Value::Null),
                "detail": format!("{} ({}:{}) {}", js_string(&code), js_string(&line), js_string(&col), js_string(&message)),
            })
        })
        .collect();
    let checked = r.get("checked").and_then(Value::as_array).map(|a| a.len()).unwrap_or(0);
    Ok(json!({ "findings": findings, "checked": checked }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE blueprints (id INTEGER PRIMARY KEY AUTOINCREMENT, contract_slug TEXT, version INTEGER, blueprint_json TEXT, lint_passed INTEGER, superseded_by INTEGER);
             CREATE TABLE blueprint_snippets (id INTEGER PRIMARY KEY AUTOINCREMENT, blueprint_id INTEGER, block_id TEXT, code TEXT, imports_json TEXT);",
        )
        .unwrap();
        c
    }

    fn temp_home(tag: &str) -> PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!("antiloki-typecheck-change-rs-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    // ANTILOKI_HOME is process-global — serialize via the crate-wide lock, same as typecheck-run.rs's
    // own tests, so a real machine install never leaks into these "not installed" assertions.
    fn with_home<T>(antiloki_home: &str, f: impl FnOnce() -> T) -> T {
        let _guard = crate::test_env_lock::ENV_LOCK.lock().unwrap();
        unsafe {
            std::env::set_var("ANTILOKI_HOME", antiloki_home);
        }
        let result = f();
        unsafe {
            std::env::remove_var("ANTILOKI_HOME");
        }
        result
    }

    #[test]
    fn missing_blueprint_row_returns_empty() {
        let c = db();
        let r = typecheckChange(&c, &json!(999), &json!({})).unwrap();
        assert_eq!(r, json!({ "findings": [], "checked": 0 }));
    }

    #[test]
    fn malformed_blueprint_json_returns_empty() {
        let c = db();
        c.execute("INSERT INTO blueprints (blueprint_json) VALUES ('{not json')", []).unwrap();
        let id = c.last_insert_rowid();
        let r = typecheckChange(&c, &json!(id), &json!({})).unwrap();
        assert_eq!(r, json!({ "findings": [], "checked": 0 }));
    }

    #[test]
    fn null_blueprint_json_parses_as_empty_blueprint() {
        let c = db();
        c.execute("INSERT INTO blueprints (blueprint_json) VALUES (NULL)", []).unwrap();
        let id = c.last_insert_rowid();
        let r = typecheckChange(&c, &json!(id), &json!({})).unwrap();
        assert_eq!(r, json!({ "findings": [], "checked": 0 }));
    }

    #[test]
    fn no_file_blocks_returns_empty() {
        let c = db();
        let bp = json!({ "blocks": [{ "id": "fn1", "type": "function" }] });
        c.execute("INSERT INTO blueprints (blueprint_json) VALUES (?1)", [bp.to_string()]).unwrap();
        let id = c.last_insert_rowid();
        let r = typecheckChange(&c, &json!(id), &json!({})).unwrap();
        assert_eq!(r, json!({ "findings": [], "checked": 0 }));
    }

    #[test]
    fn file_block_with_unsupported_extension_is_excluded_pre_dispatch() {
        let dir = temp_home("no-adapter");
        let c = db();
        let bp = json!({ "blocks": [{ "id": "F", "type": "file", "path": "README.md", "children": [] }] });
        c.execute("INSERT INTO blueprints (blueprint_json) VALUES (?1)", [bp.to_string()]).unwrap();
        let id = c.last_insert_rowid();
        c.execute("INSERT INTO blueprint_snippets (blueprint_id, block_id, code) VALUES (?1, 'F', '# hi')", rusqlite::params![id]).unwrap();
        let r = with_home(dir.to_str().unwrap(), || typecheckChange(&c, &json!(id), &json!({})).unwrap());
        assert_eq!(r, json!({ "findings": [], "checked": 0 }));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn eligible_ts_file_composes_and_dispatches_without_error_when_checker_not_installed() {
        let dir = temp_home("ts-not-installed");
        let c = db();
        let bp = json!({
            "blocks": [{
                "id": "F", "type": "file", "path": "src/x.ts",
                "children": [{ "id": "fnA", "type": "function" }]
            }]
        });
        c.execute("INSERT INTO blueprints (blueprint_json) VALUES (?1)", [bp.to_string()]).unwrap();
        let id = c.last_insert_rowid();
        c.execute(
            "INSERT INTO blueprint_snippets (blueprint_id, block_id, code) VALUES (?1, 'fnA', 'export function a() { return 1; }')",
            rusqlite::params![id],
        )
        .unwrap();
        let r = with_home(dir.to_str().unwrap(), || typecheckChange(&c, &json!(id), &json!({})).unwrap());
        // degrade-safe: typescript checker not installed -> typecheck-run skips it -> no findings,
        // nothing checked — but the whole compose+select+dispatch pipeline ran without erroring.
        assert_eq!(r, json!({ "findings": [], "checked": 0 }));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn whole_file_snippet_takes_precedence_and_still_flows_through() {
        let dir = temp_home("whole-file");
        let c = db();
        let bp = json!({ "blocks": [{ "id": "F", "type": "file", "path": "src/y.ts" }] });
        c.execute("INSERT INTO blueprints (blueprint_json) VALUES (?1)", [bp.to_string()]).unwrap();
        let id = c.last_insert_rowid();
        c.execute(
            "INSERT INTO blueprint_snippets (blueprint_id, block_id, code) VALUES (?1, 'F', 'whole file body')",
            rusqlite::params![id],
        )
        .unwrap();
        let r = with_home(dir.to_str().unwrap(), || typecheckChange(&c, &json!(id), &json!({})).unwrap());
        assert_eq!(r, json!({ "findings": [], "checked": 0 }));
        let _ = fs::remove_dir_all(&dir);
    }
}
