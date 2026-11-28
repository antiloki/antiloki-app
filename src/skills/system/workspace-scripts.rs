//! workspace-scripts.rs — twin of skills/system/workspace-scripts.mjs (db lane). NAMES ARE
//! CONTRACT: `workspaceScripts` (default, standard) · `detectScripts` (no-db helper) ·
//! `parseEnvConfig` (no-db helper) · `resolveScript` (db-first helper). The scripts registry:
//! one decided answer per workspace for build/dev/test (+ install/env config), stored in the
//! `cost_settings` kv table; `detectScripts` is the package.json-sniffing fallback.
//!
//! `detectScripts` reads `{root}/package.json` off the REAL filesystem — `root` is a plain
//! positional string argument here (not resolved from `db` inside this fn), so it is directly
//! fixture-controllable: pass a real on-disk testdata directory
//! (`antiloki-crossing/fixtures/testdata/workspace-scripts-*/package.json`, committed alongside
//! this unit's fixtures) as the argument. `resolveScript` and `workspaceScripts` call
//! `detectScripts` internally with a DB-DERIVED root via the already-converted
//! `engine_workspace_root::getWorkspaceRoot` twin (reused, not reimplemented, per the playbook's
//! cross-module rule) — their fixtures seed an active `workspaces` row pointing at the same
//! committed testdata dirs, same technique as `codebase_stack_facts__stack_facts`.
//!
//! `context.db` missing → JS throws `err.contextMissing('workspace-scripts', 'db')`. Not
//! modeled: Rust's `db: &Connection` is a required, non-nullable reference, so this guard is
//! unreachable here — same "guard-throws are not exercised" convention used by every sibling
//! twin. Likewise `op === 'gate.set'` with an invalid `gate` throws `err.enumVal(...)`: a
//! guard-throw, not exercised (fixture only covers the success path per the playbook).
//!
//! `console.log(...)` calls (gate-toggle / scripts-registry audit lines, which embed
//! `new Date().toISOString()`) are pure stdout side effects — never part of the returned value
//! or `db_after` — so they are not ported.
// QUALITY: 80/100 — package.json script-sniffing table, recorded-over-detected kv resolver, gate toggles

use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::path::Path;
use std::sync::OnceLock;
use regex::Regex;

fn truthy(v: Option<&Value>) -> bool { match v { None|Some(Value::Null)=>false, Some(Value::Bool(b))=>*b, Some(Value::Number(n))=>n.as_f64().map_or(false,|f|f!=0.0&&!f.is_nan()), Some(Value::String(s))=>!s.is_empty(), Some(_)=>true } }
fn str_qq(v: Option<&Value>) -> String { match v { None|Some(Value::Null)=>String::new(), Some(Value::String(s))=>s.clone(), Some(Value::Bool(b))=>b.to_string(), Some(n@Value::Number(_))=>n.to_string(), Some(o)=>o.to_string() } }
fn js_string(v: &Value) -> String { match v { Value::Null=>"null".into(), Value::String(s)=>s.clone(), Value::Number(n)=>n.to_string(), Value::Bool(b)=>b.to_string(), Value::Array(a)=>a.iter().map(js_string).collect::<Vec<_>>().join(","), Value::Object(_)=>"[object Object]".into() } }

// `KEYS` object literal, verbatim order — drives both `Object.keys(KEYS)` iteration (op=set) and
// the kind → cost_settings key mapping (resolveScript / readAll).
const KEYS: &[(&str, &str)] = &[
    ("build", "ws_script_build"),
    ("dev", "ws_script_dev"),
    ("test", "ws_script_test"),
    ("install", "ws_script_install"),
    ("env", "ws_env_vars"),
];

fn no_test_specified_re() -> &'static Regex { static C: OnceLock<Regex> = OnceLock::new(); C.get_or_init(|| Regex::new(r"(?i)no test specified").unwrap()) }
fn crlf_re() -> &'static Regex { static C: OnceLock<Regex> = OnceLock::new(); C.get_or_init(|| Regex::new(r"\r?\n").unwrap()) }

/// `export function detectScripts(root)` — sniffs `{root}/package.json` for the conventional
/// npm script names; degrades to all-null on any read/parse failure (a non-node workspace).
pub fn detectScripts(root: &Value) -> Value {
    let mut out = Map::new();
    out.insert("build".into(), Value::Null);
    out.insert("dev".into(), Value::Null);
    out.insert("test".into(), Value::Null);
    out.insert("install".into(), Value::Null);
    out.insert("env".into(), Value::Null);

    let root_s = str_qq(Some(root));
    let pkg_path = Path::new(&root_s).join("package.json");
    if let Ok(text) = std::fs::read_to_string(&pkg_path) {
        if let Ok(pkg) = serde_json::from_str::<Value>(&text) {
            let s = pkg.get("scripts").and_then(|v| v.as_object()).cloned().unwrap_or_default();
            if truthy(s.get("build")) { out.insert("build".into(), json!("npm run build")); }
            if truthy(s.get("dev")) {
                out.insert("dev".into(), json!("npm run dev"));
            } else if truthy(s.get("start")) {
                out.insert("dev".into(), json!("npm start"));
            } else if truthy(s.get("serve")) {
                out.insert("dev".into(), json!("npm run serve"));
            }
            let test_v = s.get("test");
            if truthy(test_v) && !no_test_specified_re().is_match(&js_string(test_v.unwrap())) {
                out.insert("test".into(), json!("npm test"));
            } else if truthy(s.get("test:unit")) {
                out.insert("test".into(), json!("npm run test:unit"));
            }
            let lock_path = Path::new(&root_s).join("package-lock.json");
            out.insert("install".into(), json!(if lock_path.exists() { "npm ci --no-audit --no-fund" } else { "npm install --no-audit --no-fund" }));
        }
    }
    Value::Object(out)
}

/// `export function parseEnvConfig(text)` — `KEY=VALUE` per line, `#` comments, blank lines
/// skipped; a line with no `=` (or `=` at position 0) is skipped. Duplicate keys keep their
/// first-seen position but the LAST value wins (plain object-key-assignment semantics).
pub fn parseEnvConfig(text: &Value) -> Value {
    let s = str_qq(Some(text));
    let mut out = Map::new();
    for raw in crlf_re().split(&s) {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') { continue; }
        let eq = match line.find('=') { Some(i) => i, None => continue };
        if eq < 1 { continue; }
        let key = line[..eq].trim().to_string();
        let val = line[eq + 1..].trim().to_string();
        out.insert(key, json!(val));
    }
    Value::Object(out)
}

/// `export function resolveScript(db, kind)` — the one resolver every gate uses: a recorded
/// `cost_settings` row (even an explicit `''` = "explicitly none") wins over detection; a missing
/// row falls to `detectScripts(getWorkspaceRoot(db))[kind]`. An unknown `kind` returns null.
pub fn resolveScript(db: &Connection, kind: &str) -> Value {
    let key = match KEYS.iter().find(|(k, _)| *k == kind) {
        Some((_, key)) => *key,
        None => return Value::Null,
    };
    if let Ok(rows) = crate::db_json::query_rows(db, "SELECT value FROM cost_settings WHERE key = ?", &[json!(key)]) {
        if let Some(row) = rows.as_array().and_then(|a| a.first()) {
            let value = row.get("value").cloned().unwrap_or(Value::Null);
            return if value.as_str() == Some("") { Value::Null } else { value };
        }
    }
    let root = crate::engine_workspace_root::getWorkspaceRoot(db);
    detectScripts(&root).get(kind).cloned().unwrap_or(Value::Null)
}

fn gate_read(db: &Connection, key: &str) -> bool {
    match crate::db_json::query_rows(db, "SELECT value FROM cost_settings WHERE key = ?", &[json!(key)]) {
        Ok(rows) => rows.as_array().and_then(|a| a.first()).map(|r| r.get("value").cloned().unwrap_or(Value::Null) != json!("off")).unwrap_or(true),
        Err(_) => true,
    }
}

fn ensure_cost_settings(db: &Connection) -> Result<(), String> {
    db.execute_batch("CREATE TABLE IF NOT EXISTS cost_settings (key TEXT PRIMARY KEY, value TEXT)").map_err(|e| e.to_string())
}
fn upsert_cost_setting(db: &Connection, key: &str, value: &str) -> Result<(), String> {
    crate::db_json::run_write(db, "INSERT INTO cost_settings (key, value) VALUES (?, ?) ON CONFLICT(key) DO UPDATE SET value = excluded.value", &[json!(key), json!(value)])?;
    Ok(())
}

/// `readAll(db)` (JS-private, not exported/not a unit) — `{ detected, root, scripts, source }`
/// merged by `op` 'get'/'set'. `scripts[kind]` = recorded (even if null-from-'') when a
/// cost_settings row exists, else the detected value; `source[kind]` = 'recorded' | 'detected' | 'none'.
fn read_all(db: &Connection) -> Map<String, Value> {
    let root = crate::engine_workspace_root::getWorkspaceRoot(db);
    let detected = detectScripts(&root);
    let mut scripts = Map::new();
    let mut source = Map::new();
    for (kind, key) in KEYS {
        let row = crate::db_json::query_rows(db, "SELECT value FROM cost_settings WHERE key = ?", &[json!(*key)]).ok()
            .and_then(|rows| rows.as_array().and_then(|a| a.first()).cloned());
        let has = row.is_some();
        let recorded = row.map(|r| {
            let v = r.get("value").cloned().unwrap_or(Value::Null);
            if v.as_str() == Some("") { Value::Null } else { v }
        }).unwrap_or(Value::Null);
        let detected_kind = detected.get(*kind).cloned().unwrap_or(Value::Null);
        scripts.insert((*kind).to_string(), if has { recorded } else { detected_kind.clone() });
        source.insert((*kind).to_string(), json!(if has { "recorded" } else if truthy(Some(&detected_kind)) { "detected" } else { "none" }));
    }
    let mut out = Map::new();
    out.insert("detected".into(), detected);
    out.insert("root".into(), root);
    out.insert("scripts".into(), Value::Object(scripts));
    out.insert("source".into(), Value::Object(source));
    out
}

/// `export default function workspaceScripts(input, context)` — ops: detect / gates / gate.set /
/// set / (default) get.
pub fn workspaceScripts(input: &Value, db: &Connection) -> Result<Value, String> {
    let op = input.get("op").and_then(|v| v.as_str()).unwrap_or("get");

    if op == "detect" {
        let root = crate::engine_workspace_root::getWorkspaceRoot(db);
        let detected = if truthy(Some(&root)) { detectScripts(&root) } else { json!({ "build": null, "dev": null, "test": null }) };
        return Ok(json!({ "ok": true, "detected": detected, "root": root }));
    }

    if op == "gates" {
        return Ok(json!({ "ok": true, "build_enabled": gate_read(db, "gate_build_enabled"), "test_enabled": gate_read(db, "gate_test_enabled") }));
    }
    if op == "gate.set" {
        let gate = str_qq(input.get("gate"));
        if gate != "build" && gate != "test" {
            return Err(format!("workspace-scripts: gate must be one of: build, test (got '{gate}')"));
        }
        ensure_cost_settings(db)?;
        let enabled_off = input.get("enabled").and_then(|v| v.as_bool()) == Some(false);
        upsert_cost_setting(db, &format!("gate_{gate}_enabled"), if enabled_off { "off" } else { "on" })?;
        return Ok(json!({ "ok": true, "build_enabled": gate_read(db, "gate_build_enabled"), "test_enabled": gate_read(db, "gate_test_enabled") }));
    }

    if op == "set" {
        ensure_cost_settings(db)?;
        let mut saved = Map::new();
        for (kind, key) in KEYS {
            let raw = match input.get(*kind) { Some(v) => v, None => continue }; // JS: input?.[kind] === undefined → skip
            let v = str_qq(Some(raw)).trim().to_string();
            upsert_cost_setting(db, key, &v)?;
            saved.insert((*kind).to_string(), if v.is_empty() { Value::Null } else { json!(v) });
        }
        let mut out = Map::new();
        out.insert("ok".into(), json!(true));
        out.insert("saved".into(), Value::Object(saved));
        for (k, v) in read_all(db) { out.insert(k, v); }
        return Ok(Value::Object(out));
    }

    let mut out = Map::new();
    out.insert("ok".into(), json!(true));
    for (k, v) in read_all(db) { out.insert(k, v); }
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE workspaces (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL UNIQUE, \
             root_path TEXT NOT NULL, vcs TEXT NOT NULL DEFAULT 'git', is_active INTEGER NOT NULL DEFAULT 0, \
             created_at TEXT NOT NULL DEFAULT '2026-01-01 00:00:00')",
        ).unwrap();
        c
    }
    fn seed_active(c: &Connection, root_path: &str) {
        c.execute(
            "INSERT INTO workspaces (name, root_path, vcs, is_active, created_at) VALUES ('w', ?1, 'git', 1, '2026-01-01 00:00:00')",
            rusqlite::params![root_path],
        ).unwrap();
    }
    const FULL: &str = concat!(env!("ANTILOKI_FIXTURES"), "/workspace-scripts-full");
    const FALLBACKS: &str = concat!(env!("ANTILOKI_FIXTURES"), "/workspace-scripts-fallbacks");
    const MISSING: &str = concat!(env!("ANTILOKI_FIXTURES"), "/workspace-scripts-missing");

    #[test]
    fn detect_scripts_full_manifest_dev_beats_start_serve_lockfile_gives_ci() {
        let r = detectScripts(&json!(FULL));
        assert_eq!(r, json!({ "build": "npm run build", "dev": "npm run dev", "test": "npm test", "install": "npm ci --no-audit --no-fund", "env": null }));
    }

    #[test]
    fn detect_scripts_fallback_chain_and_placeholder_test() {
        let r = detectScripts(&json!(FALLBACKS));
        assert_eq!(r["dev"], json!("npm start")); // start beats serve
        assert_eq!(r["test"], json!("npm run test:unit")); // placeholder test → test:unit fallback
        assert_eq!(r["install"], json!("npm install --no-audit --no-fund")); // no lockfile
    }

    #[test]
    fn detect_scripts_no_package_json_degrades_to_all_null() {
        assert_eq!(detectScripts(&json!(MISSING)), json!({ "build": null, "dev": null, "test": null, "install": null, "env": null }));
    }

    #[test]
    fn parse_env_config_comments_blank_lines_crlf_duplicate_key_position() {
        let text = "NODE_OPTIONS=--openssl-legacy-provider\n# a comment\n\nFOO = bar \nBAD_LINE_NO_EQUALS\n=starts_with_eq\r\nNODE_OPTIONS=overridden\r\n";
        let r = parseEnvConfig(&json!(text));
        assert_eq!(r, json!({ "NODE_OPTIONS": "overridden", "FOO": "bar" }));
        let keys: Vec<&String> = r.as_object().unwrap().keys().collect();
        assert_eq!(keys, vec!["NODE_OPTIONS", "FOO"]); // first-seen position preserved on overwrite
        assert_eq!(parseEnvConfig(&Value::Null), json!({}));
    }

    #[test]
    fn resolve_script_recorded_wins_explicit_empty_is_none_unknown_kind_null() {
        let c = db();
        seed_active(&c, FULL);
        c.execute_batch("CREATE TABLE cost_settings (key TEXT PRIMARY KEY, value TEXT)").unwrap();
        c.execute("INSERT INTO cost_settings (key, value) VALUES ('ws_script_build', 'npm run custom-build')", []).unwrap();
        assert_eq!(resolveScript(&c, "build"), json!("npm run custom-build"));
        c.execute("INSERT INTO cost_settings (key, value) VALUES ('ws_script_dev', '')", []).unwrap();
        assert_eq!(resolveScript(&c, "dev"), Value::Null); // explicit '' → none, not detect
        assert_eq!(resolveScript(&c, "install"), json!("npm ci --no-audit --no-fund")); // no row → detect fallback
        assert_eq!(resolveScript(&c, "bogus"), Value::Null);
    }

    #[test]
    fn workspace_scripts_default_get_merges_recorded_over_detected() {
        let c = db();
        seed_active(&c, FULL);
        c.execute_batch("CREATE TABLE cost_settings (key TEXT PRIMARY KEY, value TEXT)").unwrap();
        c.execute("INSERT INTO cost_settings (key, value) VALUES ('ws_script_test', 'npm run test:ci')", []).unwrap();
        let r = workspaceScripts(&json!({}), &c).unwrap();
        assert_eq!(r["scripts"]["test"], json!("npm run test:ci"));
        assert_eq!(r["source"]["test"], json!("recorded"));
        assert_eq!(r["scripts"]["build"], json!("npm run build"));
        assert_eq!(r["source"]["build"], json!("detected"));
        assert_eq!(r["scripts"]["env"], Value::Null);
        assert_eq!(r["source"]["env"], json!("none"));
    }

    #[test]
    fn workspace_scripts_gates_default_on_and_gate_set_flips_one() {
        let c = db();
        seed_active(&c, FULL);
        let g1 = workspaceScripts(&json!({ "op": "gates" }), &c).unwrap();
        assert_eq!(g1, json!({ "ok": true, "build_enabled": true, "test_enabled": true }));
        let g2 = workspaceScripts(&json!({ "op": "gate.set", "gate": "test", "enabled": false }), &c).unwrap();
        assert_eq!(g2, json!({ "ok": true, "build_enabled": true, "test_enabled": false }));
    }

    #[test]
    fn workspace_scripts_set_persists_only_provided_keys() {
        let c = db();
        seed_active(&c, FULL);
        let r = workspaceScripts(&json!({ "op": "set", "build": "npm run build:custom", "dev": "", "test": null }), &c).unwrap();
        assert_eq!(r["saved"], json!({ "build": "npm run build:custom", "dev": null, "test": null }));
        assert_eq!(r["scripts"]["build"], json!("npm run build:custom"));
        assert_eq!(r["source"]["build"], json!("recorded"));
        assert_eq!(r["scripts"]["dev"], Value::Null);
        assert_eq!(r["source"]["dev"], json!("recorded")); // cleared, still 'recorded'
        assert_eq!(r["scripts"]["install"], json!("npm ci --no-audit --no-fund")); // untouched → detected
        assert_eq!(r["source"]["install"], json!("detected"));
    }
}
