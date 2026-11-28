//! code-decorate.rs — twin of skills/codebase/code-decorate.mjs. NAMES ARE CONTRACT: `codeDecorate`
//! (default, standard db lane).
//!
//! ## PARITY-DEFERRED: the acorn/acorn-typescript rich AST path (`decoratorsFromAcorn`)
//! The JS source's real value is a deep, hand-rolled walk of a RAW ESTree tree returned by
//! `js-ast.mjs`'s `parseModule` (acorn + acorn-typescript — locations, TS types, JSX). That parser
//! has NO Rust twin: `parse-js.rs` (oxc_parser) and `parse-treesitter.rs` twin a DIFFERENT contract
//! — they emit ALREADY-NORMALIZED atlas nodes/edges (the same shape `code_nodes`/`code_edges`
//! store), not a raw, generically-walkable ESTree+TS tree with `start`/`end`/`loc` on every node.
//! `extractFile`/`decorateFn`/`decorateClass`/`decorateType`/`decorateValue`/`logicSteps`/
//! `analyzeBody` all walk that raw tree directly (generic `for k in node` traversal keyed on
//! `node.type`) — porting them needs the SAME raw-tree parser, which does not exist here. Per the
//! conversion playbook's lane C, that specific capability gap is deferred; the REST of the file
//! (the universal tree-sitter/atlas-backed path, the sweep+persist orchestration, the dispatcher)
//! is ported faithfully below.
//!
//! `decoratorsFromAcorn` is kept as a real fn (for `names_ok`) that performs the SAME first
//! observable step the JS does — read the file off the active workspace root — and then always
//! reports failure (no parser to hand off to), exactly matching what the JS lane also does for
//! every conform-db fixture (fixture paths are never real on-disk files, so `codebaseFileRead`
//! throws `notFound` there too and the JS lane falls through to the atlas path). This keeps
//! `decoratorsForFile`'s fallback-to-atlas behavior byte-identical for the harness while being
//! honest that the rich branch itself is unported.
// QUALITY: bespoke-native/deferred hybrid — the DB-backed atlas path + sweep/persist orchestration
// ported faithfully (conformable); the acorn-rich JS/TS analysis path is PARITY-DEFERRED (see above).

use regex::Regex;
use rusqlite::Connection;
use serde_json::{json, Value};
use std::sync::OnceLock;

fn ext_of(p: &str) -> String {
    match p.rfind('.') {
        Some(i) if !p[i..].contains(['/', '\\']) => p[i..].to_lowercase(),
        _ => String::new(),
    }
}
fn base_ext(p: &str) -> String { let e = ext_of(p); if e.is_empty() { e } else { e[1..].to_string() } }

fn cap(mut v: Vec<Value>, n: usize) -> Vec<Value> { if v.len() > n { v.truncate(n); } v }
fn uniq_strings(v: Vec<String>) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for s in v { if seen.insert(s.clone()) { out.push(s); } }
    out
}

// `roleOf(kind, output, effects, name)` — the JS calls it here with `output:null, effects:[]`, so
// only the `kind`/`name` branches are ever live: `/reducer|store|state|slice/i` and
// `/\bapi\b|service|fetch|db|query|repo/i`.
fn state_re() -> &'static Regex { static R: OnceLock<Regex> = OnceLock::new(); R.get_or_init(|| Regex::new(r"(?i)reducer|store|state|slice").unwrap()) }
fn data_re() -> &'static Regex { static R: OnceLock<Regex> = OnceLock::new(); R.get_or_init(|| Regex::new(r"(?i)\bapi\b|service|fetch|db|query|repo").unwrap()) }
fn role_of(kind: &str, name: &str) -> &'static str {
    if kind == "import" { return "external"; }
    if kind == "component" { return "ui"; }
    if state_re().is_match(name) { return "state"; }
    if data_re().is_match(name) { return "data"; }
    "logic"
}

// `const NODE_KIND = { function:'function', method:'function', class:'class', component:'component', hook:'hook', const:'const', type:'const' };`
fn node_kind(t: &str) -> String {
    match t {
        "function" | "method" => "function",
        "class" => "class",
        "component" => "component",
        "hook" => "hook",
        "const" | "type" => "const",
        other => return other.to_string(),
    }.to_string()
}

// `function sigParams(sig)` — best-effort, language-agnostic param names from "(a, b, c)".
fn sig_params(sig: &str) -> Vec<Value> {
    let open = match sig.find('(') { Some(i) => i, None => return Vec::new() };
    let close = match sig[open..].find(')') { Some(i) => open + i, None => return Vec::new() };
    let inner = sig[open + 1..close].trim();
    if inner.is_empty() { return Vec::new(); }
    let parts: Vec<Value> = inner.split(',')
        .map(|p| p.trim().split(|c| c == ':' || c == ' ' || c == '=').next().unwrap_or("").to_string())
        .filter(|n| !n.is_empty())
        .map(|n| json!({ "name": n, "kind": "plain" }))
        .collect();
    cap(parts, 8)
}

/// `function decoratorsFromAtlas(db, file, origin)` — EVERY other language (tree-sitter atlas):
/// basic decorators built from the normalized `code_nodes`/`code_edges` (kind/signature/line/
/// calls/complexity). Also the fallback path for JS/TS when the rich path is unavailable.
fn decorators_from_atlas(db: &Connection, file: &str, origin: &str) -> Value {
    let nodes = match crate::db_json::query_rows(
        db,
        "SELECT id, type, name, signature, span_json, exported FROM code_nodes WHERE origin = ? AND path = ? AND type NOT IN ('file','import') ORDER BY id",
        &[json!(origin), json!(file)],
    ) {
        Ok(v) => v.as_array().cloned().unwrap_or_default(),
        Err(e) => return json!({ "ok": false, "file": file, "error": format!("no atlas: {}", &e[..e.len().min(100)]) }),
    };
    if nodes.is_empty() {
        return json!({ "ok": true, "file": file, "lang": base_ext(file), "decorators": [], "note": "no atlas symbols for this file (rebuild the codebase)" });
    }

    let mut decorators: Vec<Value> = Vec::new();
    for n in &nodes {
        let id = n.get("id").cloned().unwrap_or(Value::Null);
        let name = n.get("name").and_then(Value::as_str).unwrap_or("?").to_string();
        let ntype = n.get("type").and_then(Value::as_str).unwrap_or("");
        let sig = n.get("signature").and_then(Value::as_str).unwrap_or("");

        let (mut line, mut end_line, mut complexity) = (Value::Null, Value::Null, 1i64);
        if let Some(span_txt) = n.get("span_json").and_then(Value::as_str) {
            if let Ok(s) = serde_json::from_str::<Value>(span_txt) {
                if !s.is_null() {
                    line = s.get("start_line").cloned().unwrap_or(Value::Null);
                    end_line = s.get("end_line").cloned().unwrap_or(Value::Null);
                    if let Some(c) = s.get("complexity").and_then(Value::as_i64) { complexity = c; }
                }
            }
        }

        let mut calls: Vec<String> = Vec::new();
        if let Ok(edges) = crate::db_json::query_rows(db, "SELECT to_node FROM code_edges WHERE origin = ? AND from_node = ? AND type = 'calls'", &[json!(origin), id.clone()]) {
            for e in edges.as_array().cloned().unwrap_or_default() {
                let to = e.get("to_node").cloned().unwrap_or(Value::Null);
                let nm = to.as_str().and_then(|to_id| {
                    crate::db_json::query_rows(db, "SELECT name FROM code_nodes WHERE id = ? LIMIT 1", &[json!(to_id)]).ok()
                        .and_then(|r| r.as_array().and_then(|a| a.first()).and_then(|row| row.get("name")).and_then(Value::as_str).map(|s| s.to_string()))
                }).or_else(|| {
                    // `String(e.to_node).split(/[#/\\]/).pop()` fallback when the target name lookup misses.
                    let s = to.as_str().map(|x| x.to_string()).unwrap_or_else(|| to.to_string());
                    s.split(['#', '/', '\\']).last().map(|x| x.to_string())
                });
                if let Some(nm) = nm { if !nm.is_empty() { calls.push(nm); } }
            }
        }
        let calls = cap(uniq_strings(calls).into_iter().map(Value::String).collect(), 8);
        let calls_strs: Vec<String> = calls.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect();

        let kind = node_kind(ntype);
        let role = role_of(&kind, &name);
        let sig_out = if sig.is_empty() { name.clone() } else if sig.starts_with('(') { format!("{}{}", name, sig) } else { format!("{} {}", name, sig) };
        let summary = format!(
            "{} {}{}.",
            kind, name,
            if calls_strs.is_empty() { String::new() } else { format!(" — calls {}", calls_strs.iter().take(3).cloned().collect::<Vec<_>>().join(", ")) }
        ).trim().to_string();

        decorators.push(json!({
            "name": name, "kind": kind, "role": role, "line": line, "end_line": end_line,
            "signature": sig_out, "inputs": sig_params(sig), "output": { "kind": "unknown", "label": "" },
            // `exported` is the one flag the atlas already knows (parse-time). The fix generators
            // read it: a test that imports a module-private function does not compile.
            "effects": [], "calls": calls, "uses": [], "complexity": complexity,
            "flags": if n.get("exported").and_then(Value::as_i64) == Some(1) { json!(["exported"]) } else { json!([]) },
            "steps": [],
            "summary": summary,
        }));
    }
    json!({ "ok": true, "file": file, "lang": base_ext(file), "decorators": decorators })
}

/// `function decoratorsFromAcorn(db, file)` — PARITY-DEFERRED (see module doc). Faithfully performs
/// the JS lane's real first step (read the file off the active workspace root) so a fixture whose
/// path IS a real on-disk file still observes a real read; there is no parser twin to hand the
/// content to, so this always reports failure — matching the JS lane's own behavior against every
/// conform-db fixture (no fixture path is a real on-disk file, so `codebaseFileRead` throws there
/// too, and `decoratorsForFile` falls through to the atlas path in both lanes).
fn decorators_from_acorn(db: &Connection, file: &str) -> Value {
    let root: Option<String> = db.query_row("SELECT root_path FROM workspaces WHERE is_active = 1 LIMIT 1", [], |r| r.get(0)).ok();
    let root = match root { Some(r) => r, None => return json!({ "ok": false, "file": file, "error": "unreadable: root (no active workspace)" }) };
    let full = std::path::Path::new(&root).join(file);
    match std::fs::read_to_string(&full) {
        Ok(src) if !src.trim().is_empty() => json!({ "ok": false, "file": file, "error": "PARITY-DEFERRED: no acorn/acorn-typescript AST parser twin available in this Rust twin" }),
        Ok(_) => json!({ "ok": false, "file": file, "error": "file is empty" }),
        Err(e) => json!({ "ok": false, "file": file, "error": format!("unreadable: {}", e) }),
    }
}

const JS_EXT: [&str; 6] = [".js", ".mjs", ".cjs", ".jsx", ".ts", ".tsx"];

/// `function decoratorsForFile(db, file, origin = 'atlas')` — dispatch by language.
fn decorators_for_file(db: &Connection, file: &str, origin: &str) -> Value {
    if !JS_EXT.contains(&ext_of(file).as_str()) { return decorators_from_atlas(db, file, origin); }
    let rich = decorators_from_acorn(db, file);
    if rich.get("ok").and_then(Value::as_bool) == Some(true) { return rich; }
    let basic = decorators_from_atlas(db, file, origin);
    let basic_ok = basic.get("ok").and_then(Value::as_bool) == Some(true);
    let basic_has = basic.get("decorators").and_then(Value::as_array).map(|a| !a.is_empty()).unwrap_or(false);
    if basic_ok && basic_has { basic } else { rich }
}

const COLS: [&str; 15] = ["name", "kind", "role", "line", "end_line", "signature", "inputs_json", "output_json", "effects_json", "calls_json", "uses_json", "complexity", "flags_json", "summary", "steps_json"];

fn persist_row_params(d: &Value) -> Vec<Value> {
    vec![
        d.get("name").cloned().unwrap_or(Value::Null),
        d.get("kind").cloned().unwrap_or(Value::Null),
        d.get("role").cloned().unwrap_or(Value::Null),
        d.get("line").cloned().unwrap_or(Value::Null),
        d.get("end_line").cloned().unwrap_or(Value::Null),
        d.get("signature").cloned().unwrap_or(Value::Null),
        json!(serde_json::to_string(d.get("inputs").unwrap_or(&json!([]))).unwrap_or_else(|_| "[]".into())),
        json!(serde_json::to_string(d.get("output").unwrap_or(&Value::Null)).unwrap_or_else(|_| "null".into())),
        json!(serde_json::to_string(d.get("effects").unwrap_or(&json!([]))).unwrap_or_else(|_| "[]".into())),
        json!(serde_json::to_string(d.get("calls").unwrap_or(&json!([]))).unwrap_or_else(|_| "[]".into())),
        json!(serde_json::to_string(d.get("uses").unwrap_or(&json!([]))).unwrap_or_else(|_| "[]".into())),
        d.get("complexity").cloned().unwrap_or(Value::Null),
        json!(serde_json::to_string(d.get("flags").unwrap_or(&json!([]))).unwrap_or_else(|_| "[]".into())),
        d.get("summary").cloned().unwrap_or(Value::Null),
        json!(serde_json::to_string(d.get("steps").unwrap_or(&json!([]))).unwrap_or_else(|_| "[]".into())),
    ]
}

/// `export default function codeDecorate(input, context)` — one-file live mode ({path}) or the
/// sweep+persist rebuild mode ({all:true, origin?}).
pub fn codeDecorate(input: &Value, db: &Connection) -> Result<Value, String> {
    if let Some(path) = input.get("path").and_then(Value::as_str) {
        let origin = input.get("origin").and_then(Value::as_str).unwrap_or("atlas");
        return Ok(decorators_for_file(db, path, origin));
    }

    if input.get("all").map(|v| matches!(v, Value::Bool(true))).unwrap_or(false) {
        let origin = input.get("origin").and_then(Value::as_str).unwrap_or("atlas").to_string();
        let files: Vec<String> = crate::db_json::query_rows(db, "SELECT path FROM code_nodes WHERE origin = ? AND type = 'file' ORDER BY path", &[json!(origin.clone())])?
            .as_array().cloned().unwrap_or_default().into_iter()
            .filter_map(|r| r.get("path").and_then(Value::as_str).map(|s| s.to_string())).collect();

        let (mut total, mut done) = (0i64, 0i64);
        let wipe = crate::db_json::run_write(db, "DELETE FROM code_decorators WHERE origin = ?", &[json!(origin.clone())]).is_ok();

        for f in &files {
            let r = decorators_for_file(db, f, &origin);
            if r.get("ok").and_then(Value::as_bool) != Some(true) { continue; }
            let decs = match r.get("decorators").and_then(Value::as_array) { Some(a) if !a.is_empty() => a.clone(), _ => continue };
            done += 1;
            for d in &decs {
                total += 1;
                if wipe {
                    let mut params = vec![json!(origin.clone()), json!(f.clone())];
                    params.extend(persist_row_params(d));
                    let cols_list = COLS.join(", ");
                    let placeholders = (0..COLS.len()).map(|_| "?").collect::<Vec<_>>().join(", ");
                    let sql = format!("INSERT INTO code_decorators (origin, path, {cols_list}) VALUES (?, ?, {placeholders})");
                    let _ = crate::db_json::run_write(db, &sql, &params); // `catch { /* skip a bad row */ }`
                }
            }
        }
        return Ok(json!({ "ok": true, "origin": origin, "files": done, "total": total, "persisted": wipe }));
    }

    Ok(json!({ "ok": false, "error": "code-decorate: pass { path } or { all:true }" }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exported_symbols_carry_the_flag() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE code_nodes (id TEXT, origin TEXT, path TEXT, type TEXT, name TEXT, signature TEXT, span_json TEXT, exported INTEGER); \
             CREATE TABLE code_edges (origin TEXT, from_node TEXT, to_node TEXT, type TEXT, kind TEXT); \
             INSERT INTO code_nodes VALUES ('a.ts#pub','atlas','a.ts','function','pub','()','{\"start_line\":1,\"end_line\":2}',1); \
             INSERT INTO code_nodes VALUES ('a.ts#priv','atlas','a.ts','function','priv','()','{\"start_line\":3,\"end_line\":4}',0);").unwrap();
        let r = decorators_from_atlas(&c, "a.ts", "atlas");
        let d = r["decorators"].as_array().unwrap();
        let by = |n: &str| d.iter().find(|x| x["name"] == json!(n)).cloned().unwrap();
        assert_eq!(by("pub")["flags"], json!(["exported"]));
        assert_eq!(by("priv")["flags"], json!([]));
    }

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE workspaces (id INTEGER PRIMARY KEY AUTOINCREMENT, root_path TEXT, is_active INTEGER);
             CREATE TABLE code_nodes (id TEXT, origin TEXT, path TEXT, type TEXT, name TEXT, signature TEXT, span_json TEXT, exported INTEGER);
             CREATE TABLE code_edges (origin TEXT, from_node TEXT, to_node TEXT, type TEXT, kind TEXT);
             CREATE TABLE code_decorators (id INTEGER PRIMARY KEY AUTOINCREMENT, origin TEXT, path TEXT, name TEXT, kind TEXT, role TEXT, line INTEGER, end_line INTEGER, signature TEXT, inputs_json TEXT, output_json TEXT, effects_json TEXT, calls_json TEXT, uses_json TEXT, complexity INTEGER, flags_json TEXT, summary TEXT, steps_json TEXT);",
        ).unwrap();
        c
    }

    #[test]
    fn one_file_mode_builds_atlas_decorators_for_a_non_js_file() {
        let c = db();
        c.execute("INSERT INTO code_nodes (id, origin, path, type, name, signature, span_json) VALUES ('n1','atlas','app/main.py','function','run','(args)','{\"start_line\":3,\"end_line\":9,\"complexity\":2}')", []).unwrap();
        c.execute("INSERT INTO code_nodes (id, origin, path, type, name) VALUES ('n2','atlas','app/main.py','function','helper')", []).unwrap();
        c.execute("INSERT INTO code_edges (origin, from_node, to_node, type) VALUES ('atlas','n1','n2','calls')", []).unwrap();
        let r = codeDecorate(&json!({ "path": "app/main.py" }), &c).unwrap();
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["lang"], json!("py"));
        let decs = r["decorators"].as_array().unwrap();
        assert_eq!(decs.len(), 2);
        assert_eq!(decs[0]["name"], json!("run"));
        assert_eq!(decs[0]["kind"], json!("function"));
        assert_eq!(decs[0]["line"], json!(3));
        assert_eq!(decs[0]["complexity"], json!(2));
        assert_eq!(decs[0]["calls"], json!(["helper"]));
        assert_eq!(decs[0]["signature"], json!("run(args)"));
    }

    #[test]
    fn js_file_with_no_real_disk_file_falls_back_to_atlas() {
        let c = db();
        c.execute("INSERT INTO code_nodes (id, origin, path, type, name) VALUES ('n1','atlas','src/a.js','function','doThing')", []).unwrap();
        let r = codeDecorate(&json!({ "path": "src/a.js" }), &c).unwrap();
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["decorators"][0]["name"], json!("doThing"));
    }

    #[test]
    fn no_atlas_symbols_notes_rebuild() {
        let c = db();
        let r = codeDecorate(&json!({ "path": "src/empty.py" }), &c).unwrap();
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["decorators"], json!([]));
        assert!(r["note"].as_str().unwrap().contains("rebuild"));
    }

    #[test]
    fn sweep_mode_wipes_and_repopulates() {
        let c = db();
        c.execute("INSERT INTO code_nodes (id, origin, path, type) VALUES ('f1','atlas','app/a.py','file')", []).unwrap();
        c.execute("INSERT INTO code_nodes (id, origin, path, type, name) VALUES ('n1','atlas','app/a.py','function','run')", []).unwrap();
        c.execute("INSERT INTO code_decorators (origin, path, name) VALUES ('atlas','stale.py','oldOne')", []).unwrap();
        let r = codeDecorate(&json!({ "all": true }), &c).unwrap();
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["files"], json!(1));
        assert_eq!(r["total"], json!(1));
        assert_eq!(r["persisted"], json!(true));
        let rows = crate::db_json::query_rows(&c, "SELECT name, path FROM code_decorators WHERE origin='atlas'", &[]).unwrap();
        let arr = rows.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["name"], json!("run"));
        assert_eq!(arr[0]["path"], json!("app/a.py"));
    }

    #[test]
    fn missing_mode_returns_error() {
        let c = db();
        let r = codeDecorate(&json!({}), &c).unwrap();
        assert_eq!(r["ok"], json!(false));
    }
}
