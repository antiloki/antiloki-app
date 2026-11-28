//! blueprint-apply.rs — twin of skills/blueprint/blueprint-apply.mjs (db lane). NAMES ARE
//! CONTRACT: `blueprintApply` (default). Pipeline v2 BUILD step 4: loads the active blueprint +
//! its authored snippets, composes each file block via the already-converted
//! `blueprint_blueprint_compose::blueprintCompose` twin (fed through the already-converted
//! `blueprint_block_shape::normalizeBlueprint` twin for the {blocks,bindings,coverage} shape),
//! and WRITES each block's composed source to `<target_root>/<block.path>` on the REAL
//! filesystem (`mkdirSync` + `writeFileSync`).
//!
//! BESPOKE-NATIVE (per the crossing playbook's fs-I/O rule): from this skill's own perspective
//! `target_root` is a caller-supplied `input` value, not resolved via any internal DB lookup or
//! hardcoded roots.mjs constant — the driver (`driver/action-input-builder.rs`, `"blueprint.apply"`
//! arm) resolves it upstream from `contract.worktree_path`/`sandbox_path` before dispatch, but
//! blueprint-apply itself never touches that table, so there is nothing DB-derived to seed a
//! testdata dir against (the `codebase/stack-facts.rs` precedent's technique doesn't apply). And
//! unlike a hardcoded roots.mjs path, a literal fixture string COULD be threaded through — but
//! the FS-injection lane in conform-db.mjs only injects a `root` key for READ-only skills
//! declaring a `files` fixture map, nothing analogous exists for a WRITE, and conform-db's case
//! comparison never inspects real filesystem state anyway (only the return value + `db_after`),
//! so a fixture would leave the one thing this skill actually does — writing correct bytes to
//! disk — just as unverified as it is here, while adding a real fs side effect to every future
//! CI conform run. Proven correct instead via `#[cfg(test)]` native tests against a real tempdir
//! — same "real fs side effect, not simulated, no conform-db fixture" precedent as
//! `agents/god/god-driver-halt.rs` / `system/driver-halt.rs`. NO conform-db fixture, NO frontier
//! entry for this unit.
//!
//! `safePath` (`normalize_segments`/`safe_path` below): defense-in-depth path-escape guard
//! (blueprint-lint already gates this at design time) — `path` must be non-empty, non-absolute,
//! and its normalized form must not retain a `..` segment (mirrors Node `path.normalize` exactly:
//! an internal `a/../b` cancels to `b`, but a leading `../a` on a relative path cannot resolve
//! further and survives as `..`).
//!
//! `AntilokiError`/`err.*` guard-throws (missing `contract_slug`/`target_root`, blueprint not
//! found, unsafe path) map to `Err(String)` carrying the exact JS message. `context.db` missing
//! is NOT modeled — `db: &Connection` is a required, non-nullable reference here, same
//! "guard-throws are not exercised" convention used by every sibling twin.

use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::path::Path;

fn truthy(v: Option<&Value>) -> bool { match v { None|Some(Value::Null)=>false, Some(Value::Bool(b))=>*b, Some(Value::Number(n))=>n.as_f64().map_or(false,|f|f!=0.0&&!f.is_nan()), Some(Value::String(s))=>!s.is_empty(), Some(_)=>true } }
fn str_qq(v: Option<&Value>) -> String { match v { None|Some(Value::Null)=>String::new(), Some(Value::String(s))=>s.clone(), Some(Value::Bool(b))=>b.to_string(), Some(n@Value::Number(_))=>n.to_string(), Some(o)=>o.to_string() } }
fn tmpl(v: Option<&Value>) -> String { match v { None=>"undefined".into(), Some(Value::Null)=>"null".into(), Some(x)=>str_qq(Some(x)) } }

// `const parse = (j, f) => { try { return JSON.parse(j ?? f); } catch { return JSON.parse(f); } };`
// — j is a nullable TEXT column, f is always the literal "[]"; a NULL column or malformed JSON
// both fall through to parsing the fallback literal (which never itself fails to parse).
fn parse_or(j: Option<&Value>, fallback: &str) -> Value {
    let text = match j { Some(Value::String(s)) => Some(s.as_str()), _ => None };
    text.and_then(|t| serde_json::from_str::<Value>(t).ok())
        .unwrap_or_else(|| serde_json::from_str(fallback).unwrap())
}

// Node `path.normalize` for a relative path: split on separators, drop `.`/empty segments,
// collapse `real-segment` + `..` pairs, keep a `..` that has nothing left to cancel.
fn normalize_segments(p: &str) -> Vec<&str> {
    let mut stack: Vec<&str> = Vec::new();
    for seg in p.split(['/', '\\']) {
        match seg {
            "" | "." => {}
            ".." => match stack.last() {
                Some(&last) if last != ".." => { stack.pop(); }
                _ => stack.push(".."),
            },
            s => stack.push(s),
        }
    }
    stack
}

// JS: `const p = String(path ?? ''); if (!p || isAbsolute(p)) return false; return
// !normalize(p).split(/[\\/]/).includes('..');`
// Node path.win32.isAbsolute: true if the path starts with a separator ('/' or '\'), OR a
// drive-letter + ':' + separator (UNC '\\server\share' also starts with a separator, so the
// leading-separator check alone covers it). std::path::Path::is_absolute() on Windows requires a
// drive-letter/UNC prefix, so a leading-separator-only path (e.g. "/etc/passwd") wrongly returns
// false there — silently weakening this escape defense (a real security-relevant divergence).
fn js_is_absolute(p: &str) -> bool {
    let b = p.as_bytes();
    // `C:\x` / `C:/x` is an escape attempt on every host, even where std's `is_absolute` says it is relative
    let drive_abs = b.len() > 2 && b[0].is_ascii_alphabetic() && b[1] == b':' && (b[2] == b'/' || b[2] == b'\\');
    p.starts_with('/') || p.starts_with('\\') || drive_abs || Path::new(p).is_absolute()
}

fn safe_path(path: Option<&Value>) -> bool {
    let p = str_qq(path);
    if p.is_empty() || js_is_absolute(&p) { return false; }
    !normalize_segments(&p).contains(&"..")
}

pub fn blueprintApply(input: &Value, db: &Connection) -> Result<Value, String> {
    let contract_slug_raw = input.get("contract_slug");
    if !truthy(contract_slug_raw) {
        return Err("blueprint-apply: contract_slug is required".to_string());
    }
    let contract_slug = contract_slug_raw.cloned().unwrap();

    let target_root_raw = input.get("target_root");
    if !truthy(target_root_raw) {
        return Err("blueprint-apply: target_root is required".to_string());
    }
    let target_root = str_qq(target_root_raw);

    let rows = crate::db_json::query_rows(
        db,
        "SELECT id, version, blueprint_json FROM blueprints WHERE contract_slug = ? AND superseded_by IS NULL ORDER BY id DESC LIMIT 1",
        &[contract_slug.clone()],
    )?;
    let row = rows.as_array().and_then(|a| a.first()).cloned().ok_or_else(|| {
        format!("blueprint-apply: blueprint '{}' not found", tmpl(Some(&contract_slug)))
    })?;
    let row_id = row.get("id").cloned().unwrap_or(Value::Null);

    // normalize-on-read: default the {blocks,bindings,coverage} arrays if any are missing.
    let parsed = row
        .get("blueprint_json")
        .and_then(Value::as_str)
        .and_then(|t| serde_json::from_str::<Value>(t).ok())
        .unwrap_or_else(|| json!({}));
    let blueprint = crate::blueprint_block_shape::normalizeBlueprint(&parsed);
    let empty = vec![];
    let blocks = blueprint.get("blocks").and_then(Value::as_array).unwrap_or(&empty).clone();

    // authored = { [block_id]: {code, imports, exports} } from the snippet swarm.
    let snippet_rows = crate::db_json::query_rows(
        db,
        "SELECT block_id, code, imports_json, exports_json FROM blueprint_snippets WHERE blueprint_id = ?",
        &[row_id.clone()],
    )?;
    let mut authored = Map::new();
    for s in snippet_rows.as_array().cloned().unwrap_or_default() {
        let block_id = match s.get("block_id") { Some(Value::String(bid)) => bid.clone(), other => str_qq(other) };
        let code = match s.get("code") { Some(Value::String(c)) => c.clone(), _ => String::new() };
        let imports = parse_or(s.get("imports_json"), "[]");
        let exports = parse_or(s.get("exports_json"), "[]");
        authored.insert(block_id, json!({ "code": code, "imports": imports, "exports": exports }));
    }
    let authored_v = Value::Object(authored);

    let mut written = Vec::new();
    for block in &blocks {
        let path_v = block.get("path");
        if !safe_path(path_v) {
            return Err(format!("blueprint-apply: unsafe block path '{}'", tmpl(path_v)));
        }
        let composed = crate::blueprint_blueprint_compose::blueprintCompose(
            &json!({ "block": block.clone(), "authored": authored_v.clone() }),
            db,
        )?;
        let source = composed.get("source").and_then(Value::as_str).unwrap_or_default().to_string();

        let block_path_str = str_qq(path_v);
        let abs = Path::new(&target_root).join(&block_path_str);
        if let Some(parent) = abs.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        std::fs::write(&abs, source.as_bytes()).map_err(|e| e.to_string())?;

        let block_path = path_v.cloned().unwrap_or(Value::Null);
        let bytes = source.as_bytes().len() as i64;
        written.push(json!({ "path": block_path, "bytes": bytes }));
    }

    let mut out = Map::new();
    out.insert("contract_slug".into(), contract_slug);
    out.insert("blueprint_id".into(), row_id);
    out.insert("count".into(), json!(written.len() as i64));
    out.insert("written".into(), json!(written));
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE blueprints (id INTEGER PRIMARY KEY AUTOINCREMENT, contract_slug TEXT NOT NULL, \
             version INTEGER NOT NULL DEFAULT 1, blueprint_json TEXT NOT NULL DEFAULT '{}', \
             lint_passed INTEGER NOT NULL DEFAULT 0, lint_json TEXT NOT NULL DEFAULT '{}', \
             created_by TEXT NOT NULL DEFAULT 'unknown', created_at TEXT NOT NULL DEFAULT '2026-01-01 00:00:00', \
             superseded_by INTEGER REFERENCES blueprints(id)); \
             CREATE TABLE blueprint_snippets (id INTEGER PRIMARY KEY AUTOINCREMENT, blueprint_id INTEGER NOT NULL, \
             block_id TEXT NOT NULL, code TEXT, imports_json TEXT, exports_json TEXT, authored_by TEXT, \
             authored_at TEXT NOT NULL DEFAULT '2026-01-01 00:00:00', UNIQUE (blueprint_id, block_id));",
        ).unwrap();
        c
    }

    fn seed_blueprint(c: &Connection, contract_slug: &str, blueprint_json: &Value) -> i64 {
        c.execute(
            "INSERT INTO blueprints (contract_slug, version, blueprint_json, lint_passed) VALUES (?1, 1, ?2, 1)",
            rusqlite::params![contract_slug, blueprint_json.to_string()],
        ).unwrap();
        c.last_insert_rowid()
    }

    fn seed_snippet(c: &Connection, blueprint_id: i64, block_id: &str, code: &str, imports: &Value) {
        c.execute(
            "INSERT INTO blueprint_snippets (blueprint_id, block_id, code, imports_json, exports_json, authored_by) VALUES (?1, ?2, ?3, ?4, '[]', 'dev')",
            rusqlite::params![blueprint_id, block_id, code, imports.to_string()],
        ).unwrap();
    }

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!("antiloki-blueprint-apply-rs-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn blueprint_one_block() -> Value {
        json!({
            "blocks": [{ "id": "src/filter.ts", "type": "file", "path": "src/filter.ts", "intent": "filter",
                "children": [{ "id": "src/filter.ts.f", "type": "function" }] }],
            "bindings": [], "coverage": [],
        })
    }

    #[test]
    fn composes_each_file_block_from_its_snippets_and_writes_the_file_under_target_root() {
        let c = db();
        let bid = seed_blueprint(&c, "CT-A", &blueprint_one_block());
        seed_snippet(&c, bid, "src/filter.ts.f", "export const f = (u) => u.filter((x) => x.age > 30)", &json!(["import {U} from './u'"]));
        let root = temp_dir("happy");

        let r = blueprintApply(&json!({ "contract_slug": "CT-A", "target_root": root.to_str().unwrap() }), &c).unwrap();
        assert_eq!(r["contract_slug"], json!("CT-A"));
        assert_eq!(r["blueprint_id"], json!(bid));
        assert_eq!(r["count"], json!(1));
        let keys: Vec<&String> = r.as_object().unwrap().keys().collect();
        assert_eq!(keys, ["contract_slug", "blueprint_id", "count", "written"]);

        let abs = root.join("src/filter.ts");
        assert!(abs.exists());
        let src = fs::read_to_string(&abs).unwrap();
        assert!(src.contains("export const f ="));
        assert!(src.contains("import {U} from './u'"));
        assert_eq!(r["written"][0]["path"], json!("src/filter.ts"));
        assert_eq!(r["written"][0]["bytes"], json!(src.as_bytes().len() as i64));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn an_unsafe_file_block_path_is_refused() {
        let c = db();
        seed_blueprint(&c, "CT-A", &json!({
            "blocks": [{ "id": "../evil.ts", "type": "file", "path": "../evil.ts", "intent": "x", "children": [] }],
            "bindings": [], "coverage": [],
        }));
        let root = temp_dir("unsafe");
        let r = blueprintApply(&json!({ "contract_slug": "CT-A", "target_root": root.to_str().unwrap() }), &c);
        assert_eq!(r, Err("blueprint-apply: unsafe block path '../evil.ts'".to_string()));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn no_active_blueprint_returns_not_found() {
        let c = db();
        let root = temp_dir("notfound");
        let r = blueprintApply(&json!({ "contract_slug": "CT-B", "target_root": root.to_str().unwrap() }), &c);
        assert_eq!(r, Err("blueprint-apply: blueprint 'CT-B' not found".to_string()));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn superseded_blueprint_is_ignored_latest_active_version_wins() {
        let c = db();
        let old = seed_blueprint(&c, "CT-A", &blueprint_one_block());
        let newer = seed_blueprint(&c, "CT-A", &blueprint_one_block());
        c.execute("UPDATE blueprints SET superseded_by = ?1 WHERE id = ?2", rusqlite::params![newer, old]).unwrap();
        let root = temp_dir("superseded");
        let r = blueprintApply(&json!({ "contract_slug": "CT-A", "target_root": root.to_str().unwrap() }), &c).unwrap();
        assert_eq!(r["blueprint_id"], json!(newer));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn missing_contract_slug_or_target_root_is_required() {
        let c = db();
        let root = temp_dir("required");
        assert_eq!(
            blueprintApply(&json!({ "target_root": root.to_str().unwrap() }), &c),
            Err("blueprint-apply: contract_slug is required".to_string())
        );
        assert_eq!(
            blueprintApply(&json!({ "contract_slug": "CT-A" }), &c),
            Err("blueprint-apply: target_root is required".to_string())
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn whole_file_snippet_takes_precedence_over_children() {
        let c = db();
        let bid = seed_blueprint(&c, "CT-W", &json!({
            "blocks": [{ "id": "f.ts", "type": "file", "path": "f.ts", "children": [{ "id": "child", "type": "function" }] }],
            "bindings": [], "coverage": [],
        }));
        seed_snippet(&c, bid, "f.ts", "whole file body", &json!([]));
        let root = temp_dir("whole-file");
        let r = blueprintApply(&json!({ "contract_slug": "CT-W", "target_root": root.to_str().unwrap() }), &c).unwrap();
        assert_eq!(r["count"], json!(1));
        let src = fs::read_to_string(root.join("f.ts")).unwrap();
        assert_eq!(src, "whole file body\n");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn safe_path_rejects_absolute_and_escaping_paths_accepts_internal_cancel() {
        assert!(!safe_path(None));
        assert!(!safe_path(Some(&Value::Null)));
        assert!(!safe_path(Some(&json!(""))));
        assert!(!safe_path(Some(&json!("/etc/passwd"))));
        assert!(!safe_path(Some(&json!("C:\\Windows\\evil"))));
        assert!(!safe_path(Some(&json!("../evil.ts"))));
        assert!(!safe_path(Some(&json!("a/../../b"))));
        assert!(safe_path(Some(&json!("src/x.ts"))));
        assert!(safe_path(Some(&json!("a/../b"))));
    }
}
