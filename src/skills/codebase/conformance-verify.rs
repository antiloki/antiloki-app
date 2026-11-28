//! conformance-verify.rs — twin of skills/codebase/conformance-verify.mjs, the author-time
//! QA/SECURITY gate. NAMES ARE CONTRACT: `conformanceVerify` (default export, async in JS —
//! awaited to its resolved value per the async-default-export lane; this twin is an ordinary
//! sync `pub fn`, ignoring the async/await keywords).
//!
//! Depends on `ast-parse.rs`'s `astParse` (already converted this pass) to parse the authored
//! files (AST #2) — called with a fresh, unused `Connection` argument the same way the JS source
//! passes `{}` as ast-parse's context (ast-parse's `_context`/`_db` param is unused either way).

use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::collections::{HashMap, HashSet};

fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}
fn str_qq(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(),
        Some(o) => o.to_string(),
    }
}
fn tmpl(v: Option<&Value>) -> String {
    match v {
        None => "undefined".into(),
        Some(Value::Null) => "null".into(),
        Some(x) => str_qq(Some(x)),
    }
}
fn js_number(v: Option<&Value>) -> f64 {
    match v {
        None => f64::NAN,
        Some(Value::Null) => 0.0,
        Some(Value::Bool(b)) => if *b { 1.0 } else { 0.0 },
        Some(Value::Number(n)) => n.as_f64().unwrap_or(f64::NAN),
        Some(Value::String(s)) => {
            let t = s.trim();
            if t.is_empty() { 0.0 } else { t.parse().unwrap_or(f64::NAN) }
        }
        Some(_) => f64::NAN,
    }
}

const STRUCTURAL_TYPES: &[&str] = &["function", "class", "method", "component", "hook"];
const DANGEROUS_MODULES: &[&str] = &[
    "child_process", "node:child_process",
    "fs", "node:fs",
    "net", "node:net",
    "http", "node:http",
    "https", "node:https",
    "dgram", "node:dgram",
    "vm", "node:vm",
    "worker_threads", "node:worker_threads",
    "module", "node:module",
];

fn posix_path(v: Option<&Value>) -> String {
    str_qq(v).replace('\\', "/")
}

// = JS: const m = signature.match(/\(([^)]*)\)/); const inner = (m ? m[1] : signature).trim();
// then split(',') with defaults/type-annotations/rest stripped. Best-effort, tolerant.
fn param_names(signature: &str) -> Vec<String> {
    let inner = match signature.find('(') {
        Some(s) => match signature[s + 1..].find(')') {
            Some(rel_e) => signature[s + 1..s + 1 + rel_e].to_string(),
            None => signature.to_string(),
        },
        None => signature.to_string(),
    };
    let inner = inner.trim();
    if inner.is_empty() {
        return vec![];
    }
    inner
        .split(',')
        .filter_map(|raw| {
            let mut s = raw.trim();
            if s.is_empty() {
                return None;
            }
            if let Some(rest) = s.strip_prefix("...") {
                s = rest;
            }
            let s = s.split(':').next().unwrap_or("").split('=').next().unwrap_or("").trim();
            if s.is_empty() { None } else { Some(s.to_string()) }
        })
        .collect()
}

// Number(v) || 1 — falsy (0, NaN, null, undefined) collapses to 1.
fn version_or_default(v: Option<&Value>) -> i64 {
    let n = js_number(v);
    if n.is_nan() || n == 0.0 { 1 } else { n as i64 }
}

#[allow(non_snake_case)]
pub fn conformanceVerify(input: &Value, db: &Connection) -> Result<Value, String> {
    let obj = input.as_object();
    let slug = obj.and_then(|o| o.get("contract_slug")).filter(|v| truthy(Some(v)));
    let Some(slug_v) = slug else {
        return Err("conformance-verify: contract_slug is required".into());
    };
    let slug = str_qq(Some(slug_v));
    let files: Vec<Value> = obj
        .and_then(|o| o.get("files"))
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    // ── load the blueprint's projected nodes ──
    let bp_rows = crate::db_json::query_rows(
        db,
        "SELECT id, origin, path, type, name, signature, version FROM code_nodes WHERE origin LIKE 'blueprint:' || ? || '@%'",
        &[json!(slug)],
    )?
    .as_array()
    .cloned()
    .unwrap_or_default();

    let max_version = bp_rows.iter().fold(1i64, |mx, r| {
        mx.max(version_or_default(r.get("version")))
    });
    let blueprint: Vec<&Value> = bp_rows
        .iter()
        .filter(|r| version_or_default(r.get("version")) == max_version)
        .collect();

    let declared_file_paths: HashSet<String> = blueprint
        .iter()
        .filter(|n| n.get("type").and_then(|v| v.as_str()) == Some("file"))
        .map(|n| posix_path(n.get("path")))
        .collect();
    let projected: Vec<&Value> = blueprint
        .iter()
        .filter(|n| {
            let t = n.get("type").and_then(|v| v.as_str()).unwrap_or("");
            STRUCTURAL_TYPES.contains(&t)
        })
        .cloned()
        .collect();

    // ── parse the authored files (AST #2) ──
    let mut authored_nodes: Vec<Value> = Vec::new();
    let mut authored_imports: Vec<(Value, String)> = Vec::new(); // (specifier, path)
    let mut authored_file_paths: Vec<String> = Vec::new(); // insertion-order Set
    for f in &files {
        let fo = f.as_object();
        let path = posix_path(fo.and_then(|o| o.get("path")));
        if !authored_file_paths.contains(&path) {
            authored_file_paths.push(path.clone());
        }
        let ast_input = json!({
            "path": fo.and_then(|o| o.get("path")).cloned().unwrap_or(Value::Null),
            "source": fo.and_then(|o| o.get("source")).cloned().unwrap_or(Value::Null),
        });
        let parsed = crate::codebase_ast_parse::astParse(&ast_input, db)?;
        let nodes = parsed.get("nodes").and_then(|v| v.as_array()).cloned().unwrap_or_default();
        for n in nodes {
            authored_nodes.push(n);
        }
        let imports = parsed.get("imports").and_then(|v| v.as_array()).cloned().unwrap_or_default();
        for imp in imports {
            let specifier = imp.get("specifier").cloned().unwrap_or(Value::Null);
            authored_imports.push((specifier, path.clone()));
        }
    }

    let mut findings: Vec<Value> = Vec::new();

    // ── check 1: conformance ──
    for p in &projected {
        let p_path = posix_path(p.get("path"));
        let p_name = p.get("name").cloned().unwrap_or(Value::Null);
        let matched = authored_nodes
            .iter()
            .find(|a| a.get("name") == Some(&p_name) && posix_path(a.get("path")) == p_path);
        let Some(matched) = matched else {
            let mut node = Map::new();
            node.insert("type".into(), p.get("type").cloned().unwrap_or(Value::Null));
            node.insert("name".into(), p_name);
            node.insert("path".into(), json!(p_path));
            let mut f = Map::new();
            f.insert("kind".into(), json!("missing"));
            f.insert(
                "detail".into(),
                json!(format!(
                    "blueprint promised {} '{}' in {}, but no matching authored node was found",
                    tmpl(p.get("type")),
                    tmpl(p.get("name")),
                    p_path
                )),
            );
            f.insert("node".into(), Value::Object(node));
            findings.push(Value::Object(f));
            continue;
        };
        if truthy(p.get("signature")) && truthy(matched.get("signature")) {
            let want = param_names(&str_qq(p.get("signature")));
            let have: HashSet<String> = param_names(&str_qq(matched.get("signature"))).into_iter().collect();
            let missing_params: Vec<String> = want.into_iter().filter(|w| !have.contains(w)).collect();
            if !missing_params.is_empty() {
                let mut node = Map::new();
                node.insert("type".into(), p.get("type").cloned().unwrap_or(Value::Null));
                node.insert("name".into(), p.get("name").cloned().unwrap_or(Value::Null));
                node.insert("path".into(), json!(p_path));
                let mut f = Map::new();
                f.insert("kind".into(), json!("signature"));
                f.insert(
                    "detail".into(),
                    json!(format!(
                        "{} '{}' is missing projected param(s): {} (projected {}, authored {})",
                        tmpl(p.get("type")),
                        tmpl(p.get("name")),
                        missing_params.join(", "),
                        tmpl(p.get("signature")),
                        tmpl(matched.get("signature")),
                    )),
                );
                f.insert("node".into(), Value::Object(node));
                findings.push(Value::Object(f));
            }
        }
    }

    // ── check 2: scope ──
    for path in &authored_file_paths {
        if !declared_file_paths.contains(path) {
            let mut node = Map::new();
            node.insert("type".into(), json!("file"));
            node.insert("path".into(), json!(path));
            let mut f = Map::new();
            f.insert("kind".into(), json!("out_of_scope"));
            f.insert(
                "detail".into(),
                json!(format!("authored file {} is not among the blueprint's declared files", path)),
            );
            f.insert("node".into(), Value::Object(node));
            findings.push(Value::Object(f));
        }
    }

    // ── check 3: security ──
    for (specifier, path) in &authored_imports {
        let spec_s = tmpl(Some(specifier));
        if DANGEROUS_MODULES.contains(&spec_s.as_str()) {
            let mut node = Map::new();
            node.insert("type".into(), json!("import"));
            node.insert("name".into(), specifier.clone());
            node.insert("path".into(), json!(path));
            let mut f = Map::new();
            f.insert("kind".into(), json!("security"));
            f.insert(
                "detail".into(),
                json!(format!(
                    "authored import of dangerous module '{}' in {} was not declared by the blueprint",
                    tmpl(Some(specifier)),
                    path
                )),
            );
            f.insert("node".into(), Value::Object(node));
            findings.push(Value::Object(f));
        }
    }

    // ── check 4: blast-radius ──
    let atlas_symbols = crate::db_json::query_rows(
        db,
        "SELECT id, name, path, signature FROM code_nodes WHERE origin = 'atlas' AND type IN ('function','class','method','component','hook')",
        &[],
    )?
    .as_array()
    .cloned()
    .unwrap_or_default();
    let mut atlas_by_key: HashMap<String, Value> = HashMap::new();
    for a in &atlas_symbols {
        let key = format!("{}::{}", posix_path(a.get("path")), tmpl(a.get("name")));
        atlas_by_key.insert(key, a.clone());
    }
    if !atlas_by_key.is_empty() {
        let authored_set: HashSet<&String> = authored_file_paths.iter().collect();
        for a in &authored_nodes {
            let a_type = a.get("type").and_then(|v| v.as_str()).unwrap_or("");
            if !STRUCTURAL_TYPES.contains(&a_type) {
                continue;
            }
            let key = format!("{}::{}", posix_path(a.get("path")), tmpl(a.get("name")));
            let Some(existing) = atlas_by_key.get(&key) else {
                continue;
            };
            if !truthy(existing.get("signature")) || !truthy(a.get("signature")) {
                continue;
            }
            let want = param_names(&str_qq(existing.get("signature"))).join(",");
            let have = param_names(&str_qq(a.get("signature"))).join(",");
            if want == have {
                continue;
            }
            let existing_id = existing.get("id").cloned().unwrap_or(Value::Null);
            let rev = crate::db_json::query_rows(
                db,
                "SELECT DISTINCT from_node FROM code_edges WHERE origin = 'atlas' AND to_node = ?",
                &[existing_id],
            )?;
            let mut seen: HashSet<String> = HashSet::new();
            let mut dependents: Vec<String> = Vec::new();
            if let Some(rows) = rev.as_array() {
                for r in rows {
                    let from_node = r.get("from_node").cloned().unwrap_or(Value::Null);
                    let path_row = crate::db_json::query_rows(
                        db,
                        "SELECT path FROM code_nodes WHERE origin = 'atlas' AND id = ?",
                        &[from_node],
                    )?;
                    let p = path_row
                        .as_array()
                        .and_then(|arr| arr.first())
                        .and_then(|row| row.get("path"))
                        .cloned();
                    if let Some(p) = p {
                        if truthy(Some(&p)) {
                            let pp = posix_path(Some(&p));
                            if seen.insert(pp.clone()) {
                                dependents.push(pp);
                            }
                        }
                    }
                }
            }
            let dependents: Vec<String> = dependents.into_iter().filter(|p| !authored_set.contains(p)).collect();
            if !dependents.is_empty() {
                let shown: Vec<&String> = dependents.iter().take(8).collect();
                let mut shown_str = shown.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ");
                if dependents.len() > 8 {
                    shown_str.push('…');
                }
                let mut node = Map::new();
                node.insert("type".into(), json!(a_type));
                node.insert("name".into(), a.get("name").cloned().unwrap_or(Value::Null));
                node.insert("path".into(), json!(posix_path(a.get("path"))));
                let mut f = Map::new();
                f.insert("kind".into(), json!("blast_radius"));
                f.insert(
                    "detail".into(),
                    json!(format!(
                        "signature change to {} '{}' ({} → {}) affects {} caller file(s) not in this change: {}",
                        a_type,
                        tmpl(a.get("name")),
                        tmpl(existing.get("signature")),
                        tmpl(a.get("signature")),
                        dependents.len(),
                        shown_str,
                    )),
                );
                f.insert("node".into(), Value::Object(node));
                findings.push(Value::Object(f));
            }
        }
    }

    let hard: HashSet<&str> = ["missing", "out_of_scope", "security"].into_iter().collect();
    let verdict = if findings
        .iter()
        .any(|f| hard.contains(f.get("kind").and_then(|v| v.as_str()).unwrap_or("")))
    {
        "fail"
    } else {
        "pass"
    };

    Ok(json!({
        "verdict": verdict,
        "findings": findings,
        "checked": {
            "blueprint_nodes": blueprint.len() as i64,
            "authored_files": authored_file_paths.len() as i64,
        },
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seed(db: &Connection) {
        db.execute_batch(
            "CREATE TABLE code_nodes (id TEXT, origin TEXT, path TEXT, type TEXT, name TEXT, signature TEXT, version INTEGER);
             CREATE TABLE code_edges (origin TEXT, from_node TEXT, to_node TEXT, type TEXT, kind TEXT);
             INSERT INTO code_nodes (id, origin, path, type, name, signature, version) VALUES
               ('blueprint:FOO@1:auth.mjs', 'blueprint:FOO@1', 'auth.mjs', 'file', 'auth.mjs', NULL, 1),
               ('blueprint:FOO@1:auth.mjs#validate', 'blueprint:FOO@1', 'auth.mjs', 'function', 'validate', '(token, opts)', 1);",
        )
        .unwrap();
    }

    fn db() -> Connection {
        Connection::open_in_memory().unwrap()
    }

    #[test]
    fn pass_when_promised_fn_is_authored() {
        let c = db();
        seed(&c);
        let source = "export function validate(token, opts) { return !!token; }\n";
        let r = conformanceVerify(&json!({ "contract_slug": "FOO", "files": [{ "path": "auth.mjs", "source": source }] }), &c).unwrap();
        assert_eq!(r["verdict"], json!("pass"));
        assert_eq!(r["findings"].as_array().unwrap().len(), 0);
        assert_eq!(r["checked"], json!({ "blueprint_nodes": 2, "authored_files": 1 }));
    }

    #[test]
    fn missing_promised_fn_fails() {
        let c = db();
        seed(&c);
        let source = "export function helper(a) { return a; }\n";
        let r = conformanceVerify(&json!({ "contract_slug": "FOO", "files": [{ "path": "auth.mjs", "source": source }] }), &c).unwrap();
        assert_eq!(r["verdict"], json!("fail"));
        let f = r["findings"].as_array().unwrap().iter().find(|x| x["kind"] == "missing").unwrap();
        assert_eq!(f["node"]["name"], json!("validate"));
    }

    #[test]
    fn dangerous_import_fails_security() {
        let c = db();
        seed(&c);
        let source = "import { execSync } from 'child_process';\nexport function validate(token, opts) { return execSync('echo ' + token); }\n";
        let r = conformanceVerify(&json!({ "contract_slug": "FOO", "files": [{ "path": "auth.mjs", "source": source }] }), &c).unwrap();
        assert_eq!(r["verdict"], json!("fail"));
        assert!(r["findings"].as_array().unwrap().iter().any(|x| x["kind"] == "security"));
    }

    #[test]
    fn out_of_scope_file_fails() {
        let c = db();
        seed(&c);
        let source = "export function validate(token, opts) { return !!token; }\n";
        let r = conformanceVerify(&json!({ "contract_slug": "FOO", "files": [{ "path": "rogue.mjs", "source": source }] }), &c).unwrap();
        assert_eq!(r["verdict"], json!("fail"));
        assert!(r["findings"].as_array().unwrap().iter().any(|x| x["kind"] == "out_of_scope"));
    }

    #[test]
    fn extra_authored_code_still_passes() {
        let c = db();
        seed(&c);
        let source = "export function validate(token, opts) { return !!token; }\nfunction helper(a) { return a; }\n";
        let r = conformanceVerify(&json!({ "contract_slug": "FOO", "files": [{ "path": "auth.mjs", "source": source }] }), &c).unwrap();
        assert_eq!(r["verdict"], json!("pass"));
        assert_eq!(r["findings"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn missing_projected_param_is_advisory_signature_finding() {
        let c = db();
        seed(&c);
        let source = "export function validate(token) { return !!token; }\n";
        let r = conformanceVerify(&json!({ "contract_slug": "FOO", "files": [{ "path": "auth.mjs", "source": source }] }), &c).unwrap();
        assert_eq!(r["verdict"], json!("pass"));
        let f = r["findings"].as_array().unwrap().iter().find(|x| x["kind"] == "signature").unwrap();
        assert!(f["detail"].as_str().unwrap().contains("opts"));
    }

    #[test]
    fn blast_radius_advisory_when_caller_not_in_change() {
        let c = db();
        seed(&c);
        c.execute_batch(
            "INSERT INTO code_nodes (id, origin, path, type, name, signature, version) VALUES
               ('auth.mjs', 'atlas', 'auth.mjs', 'file', 'auth.mjs', NULL, 1),
               ('auth.mjs#validate', 'atlas', 'auth.mjs', 'function', 'validate', '(token, opts)', 1),
               ('caller.mjs', 'atlas', 'caller.mjs', 'file', 'caller.mjs', NULL, 1),
               ('caller.mjs#useValidate', 'atlas', 'caller.mjs', 'function', 'useValidate', NULL, 1);
             INSERT INTO code_edges (origin, from_node, to_node, type) VALUES
               ('atlas', 'caller.mjs#useValidate', 'auth.mjs#validate', 'calls');",
        )
        .unwrap();
        let source = "export function validate(token) { return !!token; }\n";
        let r = conformanceVerify(&json!({ "contract_slug": "FOO", "files": [{ "path": "auth.mjs", "source": source }] }), &c).unwrap();
        assert_eq!(r["verdict"], json!("pass"));
        let f = r["findings"].as_array().unwrap().iter().find(|x| x["kind"] == "blast_radius").unwrap();
        assert!(f["detail"].as_str().unwrap().contains("caller.mjs"));
    }

    #[test]
    fn latest_blueprint_version_wins() {
        let c = db();
        c.execute_batch(
            "CREATE TABLE code_nodes (id TEXT, origin TEXT, path TEXT, type TEXT, name TEXT, signature TEXT, version INTEGER);
             CREATE TABLE code_edges (origin TEXT, from_node TEXT, to_node TEXT, type TEXT, kind TEXT);
             INSERT INTO code_nodes (id, origin, path, type, name, signature, version) VALUES
               ('blueprint:FOO@1:auth.mjs', 'blueprint:FOO@1', 'auth.mjs', 'file', 'auth.mjs', NULL, 1),
               ('blueprint:FOO@1:auth.mjs#validate', 'blueprint:FOO@1', 'auth.mjs', 'function', 'validate', '(token, opts)', 1),
               ('blueprint:FOO@2:auth.mjs', 'blueprint:FOO@2', 'auth.mjs', 'file', 'auth.mjs', NULL, 2),
               ('blueprint:FOO@2:auth.mjs#verify', 'blueprint:FOO@2', 'auth.mjs', 'function', 'verify', '(token)', 2);",
        )
        .unwrap();
        let source = "export function verify(token) { return !!token; }\n";
        let r = conformanceVerify(&json!({ "contract_slug": "FOO", "files": [{ "path": "auth.mjs", "source": source }] }), &c).unwrap();
        assert_eq!(r["verdict"], json!("pass"), "{:?}", r["findings"]);
    }
}
