//! stack-facts.rs — twin of skills/codebase/stack-facts.mjs. NAMES ARE CONTRACT:
//! `stackFactsFromManifest` (named, no-db, pure), `stackFacts` (default, db lane).
//!
//! `stackFacts` reads `{workspace_root}/package.json` off the REAL filesystem. Unlike a
//! roots.mjs-anchored fixed path (SUBSTRATE_ROOT etc — never fixture-controllable per the
//! crossing playbook), the root here is DB-DERIVED via the already-converted
//! `engine_workspace_root::getWorkspaceRoot` twin (reused, not reimplemented, per the
//! playbook's cross-module rule) — so it IS fixture-controllable: seed an active `workspaces`
//! row whose `root_path` points at a real on-disk testdata directory
//! (`antiloki-crossing/fixtures/testdata/stack-facts-*/package.json`, committed alongside this
//! unit's fixture JSON) and both lanes read the identical bytes.
//!
//! The "no active workspace" fallback to SUBSTRATE_ROOT is deliberately NOT exercised by this
//! unit's fixture — that string-level fallback is already CONFORMANT-tested by
//! `engine_workspace_root`'s own fixture, and chaining a real fs read onto the actual repo root
//! would make found/not-found depend on whether the repo root happens to carry a `package.json`
//! on a given day, which is exactly the environment-fragile assertion this playbook avoids.
//!
//! `context.db` missing → JS throws `err.contextMissing('stack-facts', 'db')`
//! (`"stack-facts: context.db is required"`). Not modeled: Rust's `db: &Connection` is a
//! required, non-nullable reference, so this guard is unreachable here — same "guard-throws are
//! not exercised" convention used by every sibling twin.
//!
//! `readFileSync` failure (ENOENT etc) AND `JSON.parse` failure (malformed JSON) both collapse
//! into the JS `catch { pkg = null }` branch — modeled here as a single `Option` chain
//! (`.ok()...and_then(...ok())`) that maps either failure to `None`, then `truthy()`-filtered so
//! a parsed-but-falsy manifest (`null`/`0`/`""`) also takes the `found:false` branch, exactly
//! like JS's `if (!pkg)`.
// QUALITY: 82/100 — table-driven first-match framework/test/build/state detection ported 1:1
// from the JS source tables; fs read is real and fixture-backed via a committed testdata dir.

use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::path::Path;

fn truthy(v: Option<&Value>) -> bool { match v { None|Some(Value::Null)=>false, Some(Value::Bool(b))=>*b, Some(Value::Number(n))=>n.as_f64().map_or(false,|f|f!=0.0&&!f.is_nan()), Some(Value::String(s))=>!s.is_empty(), Some(_)=>true } }
fn str_qq(v: Option<&Value>) -> String { match v { None|Some(Value::Null)=>String::new(), Some(Value::String(s))=>s.clone(), Some(Value::Bool(b))=>b.to_string(), Some(n@Value::Number(_))=>n.to_string(), Some(o)=>o.to_string() } }
fn tmpl(v: Option<&Value>) -> String { match v { None=>"undefined".into(), Some(Value::Null)=>"null".into(), Some(x)=>str_qq(Some(x)) } }

// State-management libs, in check order — first present wins (see `first`). Order mirrors the
// JS STATE_LIBS array verbatim.
const STATE_LIBS: &[(&str, &str)] = &[
    ("@reduxjs/toolkit", "Redux Toolkit"), ("react-redux", "Redux"), ("redux", "Redux"),
    ("zustand", "Zustand"), ("mobx", "MobX"), ("recoil", "Recoil"), ("jotai", "Jotai"),
    ("valtio", "Valtio"), ("xstate", "XState"), ("effector", "Effector"),
    ("@ngrx/store", "NgRx"), ("vuex", "Vuex"), ("pinia", "Pinia"),
];
const STATE_CHECKED: &str = "redux/zustand/mobx/recoil/jotai/xstate";
// Frameworks, most-specific first (a Next app also has react — report Next).
const FRAMEWORKS: &[(&str, &str)] = &[
    ("next", "Next.js"), ("@remix-run/react", "Remix"), ("gatsby", "Gatsby"), ("@angular/core", "Angular"),
    ("nuxt", "Nuxt"), ("@nestjs/core", "NestJS"), ("svelte", "Svelte"), ("vue", "Vue"),
    ("react", "React"), ("express", "Express"), ("fastify", "Fastify"), ("koa", "Koa"), ("@hapi/hapi", "hapi"),
];
const TEST_LIBS: &[(&str, &str)] = &[
    ("vitest", "Vitest"), ("@playwright/test", "Playwright"), ("jest", "Jest"), ("mocha", "Mocha"),
    ("cypress", "Cypress"), ("ava", "AVA"), ("jasmine", "Jasmine"),
];
const BUILD_LIBS: &[(&str, &str)] = &[
    ("vite", "Vite"), ("webpack", "webpack"), ("rollup", "Rollup"), ("parcel", "Parcel"),
    ("esbuild", "esbuild"), ("@swc/core", "SWC"),
];
// = JS `/^(react-router|@tanstack\/react-query|swr|axios|graphql|@apollo\/client|
// styled-components|@emotion\/|tailwindcss|@mui\/|antd|zod|yup|prisma|@trpc\/|socket\.io)/`
// — a PREFIX match (no trailing `$`), so `starts_with` per alternative is the exact port.
const NOTABLE_PREFIXES: &[&str] = &[
    "react-router", "@tanstack/react-query", "swr", "axios", "graphql", "@apollo/client",
    "styled-components", "@emotion/", "tailwindcss", "@mui/", "antd", "zod", "yup", "prisma",
    "@trpc/", "socket.io",
];
fn notable(name: &str) -> bool { NOTABLE_PREFIXES.iter().any(|p| name.starts_with(p)) }

fn first(deps: &Map<String, Value>, table: &[(&str, &str)]) -> Option<String> {
    for (k, label) in table {
        if let Some(v) = deps.get(*k) {
            if truthy(Some(v)) { return Some((*label).to_string()); }
        }
    }
    None
}

/// `export function stackFactsFromManifest(pkg)` — pure: parsed package.json → distilled facts
/// + compact markdown. Returns `{ facts, facts_md, tokens }` (matches the JS return shape
/// exactly; `stackFacts` below wraps this in `{ ok, found, ... }`).
pub fn stackFactsFromManifest(pkg: &Value) -> Value {
    // `{ ...(pkg?.dependencies || {}), ...(pkg?.devDependencies || {}) }` — devDependencies wins
    // on key collision. Iteration ORDER of the merged map is never observed (key_deps is sorted;
    // `first` walks its own table, not deps' insertion order), so a plain overwrite-merge suffices.
    let mut deps: Map<String, Value> = Map::new();
    if let Some(d) = pkg.get("dependencies").and_then(Value::as_object) {
        for (k, v) in d { deps.insert(k.clone(), v.clone()); }
    }
    if let Some(d) = pkg.get("devDependencies").and_then(Value::as_object) {
        for (k, v) in d { deps.insert(k.clone(), v.clone()); }
    }

    let has_ts = truthy(deps.get("typescript")) || truthy(deps.get("ts-node")) || truthy(deps.get("@types/node"));
    let esm = pkg.get("type").and_then(Value::as_str) == Some("module");
    let node_req = pkg.get("engines").and_then(Value::as_object).and_then(|e| e.get("node")).cloned();

    let mut runtime = String::new();
    if esm { runtime.push_str("ESM "); }
    runtime.push_str(if has_ts { "TypeScript" } else { "JavaScript" });
    if truthy(node_req.as_ref()) { runtime.push_str(&format!(" · Node {}", tmpl(node_req.as_ref()))); }

    let framework = first(&deps, FRAMEWORKS);
    let state_lib = first(&deps, STATE_LIBS);
    let state = state_lib.unwrap_or_else(|| format!("none detected — local component state (no {STATE_CHECKED} in dependencies)"));
    let test = first(&deps, TEST_LIBS);
    let build = first(&deps, BUILD_LIBS);

    let mut key_deps: Vec<String> = deps.keys().filter(|k| notable(k)).cloned().collect();
    key_deps.sort();
    key_deps.truncate(8);

    let mut facts = Map::new();
    facts.insert("runtime".into(), json!(runtime));
    facts.insert("framework".into(), framework.clone().map_or(Value::Null, |f| json!(f)));
    facts.insert("state".into(), json!(state));
    facts.insert("test".into(), test.clone().map_or(Value::Null, |t| json!(t)));
    facts.insert("build".into(), build.clone().map_or(Value::Null, |b| json!(b)));
    facts.insert("key_deps".into(), json!(key_deps));

    let mut lines: Vec<String> = vec!["## Stack & conventions (from package.json — deterministic)".to_string()];
    lines.push(format!("- runtime: {runtime}"));
    if let Some(f) = &framework { lines.push(format!("- framework: {f}")); }
    lines.push(format!("- state management: {state}"));
    if let Some(t) = &test { lines.push(format!("- test runner: {t}")); }
    if let Some(b) = &build { lines.push(format!("- build: {b}")); }
    if !key_deps.is_empty() { lines.push(format!("- key deps: {}", key_deps.join(", "))); }
    let facts_md = lines.join("\n");

    let tokens = crate::system_tokens_count::estimateTokens(&json!(facts_md));

    json!({ "facts": Value::Object(facts), "facts_md": facts_md, "tokens": tokens })
}

/// `export default function stackFacts(input, context)` — `input` is unused (JS destructures
/// only `context.db`). Resolves the workspace root, reads its `package.json` off the real
/// filesystem, degrades to `found:false` on any read/parse failure (see module doc).
pub fn stackFacts(input: &Value, db: &Connection) -> Result<Value, String> {
    let _ = input;
    let root_v = crate::engine_workspace_root::getWorkspaceRoot(db);
    let root = root_v.as_str().unwrap_or_default();
    let pkg_path = Path::new(root).join("package.json");

    let pkg = std::fs::read_to_string(&pkg_path).ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .filter(|v| truthy(Some(v)));

    match pkg {
        None => Ok(json!({ "ok": true, "found": false, "facts": Value::Null, "facts_md": "", "tokens": 0 })),
        Some(pkg_val) => {
            let r = stackFactsFromManifest(&pkg_val);
            let facts = r.get("facts").cloned().unwrap_or(Value::Null);
            let facts_md = r.get("facts_md").cloned().unwrap_or(json!(""));
            let tokens = r.get("tokens").cloned().unwrap_or(json!(0));
            Ok(json!({ "ok": true, "found": true, "facts": facts, "facts_md": facts_md, "tokens": tokens }))
        }
    }
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
            "INSERT INTO workspaces (name, root_path, vcs, is_active, created_at) VALUES ('x', ?1, 'git', 1, '2026-01-01 00:00:00')",
            rusqlite::params![root_path],
        ).unwrap();
    }

    #[test]
    fn stack_facts_from_manifest_states_absence_explicitly_no_redux() {
        let pkg = json!({
            "dependencies": { "react": "^18.2.0", "react-dom": "^18.2.0", "react-router-dom": "^6.4.0", "axios": "^1.3.0" },
            "devDependencies": { "vitest": "^0.34.0", "vite": "^4.0.0", "typescript": "^5.0.0" }
        });
        let r = stackFactsFromManifest(&pkg);
        assert_eq!(r["facts"]["framework"], json!("React"));
        assert_eq!(r["facts"]["test"], json!("Vitest"));
        assert_eq!(r["facts"]["build"], json!("Vite"));
        assert_eq!(r["facts"]["runtime"], json!("TypeScript"));
        assert!(r["facts"]["state"].as_str().unwrap().starts_with("none detected"));
        assert!(r["facts"]["state"].as_str().unwrap().contains("no redux"));
        assert!(r["facts_md"].as_str().unwrap().contains("state management: none detected"));
        assert_eq!(r["facts"]["key_deps"], json!(["axios", "react-router-dom"]));
    }

    #[test]
    fn stack_facts_from_manifest_names_redux_when_present() {
        let r = stackFactsFromManifest(&json!({ "dependencies": { "react": "^18", "@reduxjs/toolkit": "^2.0", "react-redux": "^9" } }));
        assert_eq!(r["facts"]["state"], json!("Redux Toolkit"));
    }

    #[test]
    fn stack_facts_from_manifest_next_beats_react() {
        let r = stackFactsFromManifest(&json!({ "dependencies": { "next": "^14", "react": "^18", "react-dom": "^18" } }));
        assert_eq!(r["facts"]["framework"], json!("Next.js"));
    }

    #[test]
    fn stack_facts_from_manifest_empty_manifest_degrades() {
        let r = stackFactsFromManifest(&json!({}));
        assert_eq!(r["facts"]["framework"], Value::Null);
        assert!(r["facts"]["state"].as_str().unwrap().starts_with("none detected"));
        assert_eq!(r["facts"]["key_deps"], json!([]));
        assert!(r["facts_md"].as_str().unwrap().starts_with("## Stack & conventions"));
    }

    #[test]
    fn stack_facts_from_manifest_esm_typescript_node_runtime_string() {
        let r = stackFactsFromManifest(&json!({
            "type": "module", "engines": { "node": ">=20" },
            "dependencies": { "express": "^4", "zod": "^3", "socket.io": "^4" },
            "devDependencies": { "@types/node": "^20", "webpack": "^5", "mocha": "^10" }
        }));
        assert_eq!(r["facts"]["runtime"], json!("ESM TypeScript · Node >=20"));
        assert_eq!(r["facts"]["framework"], json!("Express"));
        assert_eq!(r["facts"]["test"], json!("Mocha"));
        assert_eq!(r["facts"]["build"], json!("webpack"));
        assert_eq!(r["facts"]["key_deps"], json!(["socket.io", "zod"]));
    }

    #[test]
    fn stack_facts_reads_real_package_json_under_the_workspace_root() {
        let c = db();
        seed_active(&c, concat!(env!("ANTILOKI_FIXTURES"), "/stack-facts-found"));
        let r = stackFacts(&json!({}), &c).unwrap();
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["found"], json!(true));
        assert_eq!(r["facts"]["framework"], json!("React"));
    }

    #[test]
    fn stack_facts_degrades_when_package_json_is_an_empty_object() {
        let c = db();
        seed_active(&c, concat!(env!("ANTILOKI_FIXTURES"), "/stack-facts-empty"));
        let r = stackFacts(&json!({}), &c).unwrap();
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["found"], json!(true));
        assert_eq!(r["facts"]["framework"], Value::Null);
    }

    #[test]
    fn stack_facts_not_found_when_no_package_json_exists() {
        let c = db();
        seed_active(&c, concat!(env!("ANTILOKI_FIXTURES"), "/stack-facts-missing"));
        let r = stackFacts(&json!({}), &c).unwrap();
        assert_eq!(r, json!({ "ok": true, "found": false, "facts": null, "facts_md": "", "tokens": 0 }));
    }
}
