//! composition-check.rs — twin of skills/swarm/composition-check.mjs. NAMES ARE CONTRACT:
//! resolveImportPath (no-db) · checkComposition (no-db) · parseExports/parseImports (transitive — the
//! JS returns Sets, not JSON-serializable) · compositionCheck (default, db lane; compute-only, delegates
//! to swarm_manifest_lint::parseManifest). Verifies authored code realizes the manifest's bindings.
// QUALITY: heuristic ESM/CJS export+import parser (regex, no lookaround) + per-binding export/import checks
use rusqlite::Connection;
use regex::Regex;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::OnceLock;

// ── JS-parity helpers ──────────────────────────────────────────────────────────
fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}
// String(v ?? '')
fn str_qq(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(),
        Some(o) => o.to_string(),
    }
}
// JS `${v}` interpolation.
fn tmpl(v: Option<&Value>) -> String {
    match v {
        None => "undefined".into(),
        Some(Value::Null) => "null".into(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(),
        Some(o) => o.to_string(),
    }
}
fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) }
// JS Map key by value: strings vs numbers etc. stay distinct (SameValueZero for primitives).
fn key_of(v: &Value) -> String { serde_json::to_string(v).unwrap_or_default() }
fn add_unique(v: &mut Vec<String>, s: &str) { if !v.iter().any(|x| x == s) { v.push(s.to_string()); } }

// ── posix-ish path math (deterministic, separator-stable) ───────────────────────
fn re_ext() -> &'static Regex { static R: OnceLock<Regex> = OnceLock::new(); R.get_or_init(|| Regex::new(r"\.(mjs|cjs|jsx?|tsx?)$").unwrap()) }
fn strip_ext(p: &str) -> String { re_ext().replace(p, "").into_owned() }
fn norm_slash(p: &str) -> String {
    let a = p.replace('\\', "/");
    match a.strip_prefix("./") { Some(r) => r.to_string(), None => a }
}
fn dirname(p: &str) -> String {
    let s = norm_slash(p);
    match s.rfind('/') { Some(i) => s[..i].to_string(), None => String::new() }
}
fn normalize_segments(p: &str) -> String {
    let s = norm_slash(p);
    let mut out: Vec<&str> = Vec::new();
    for seg in s.split('/') {
        if seg.is_empty() || seg == "." { continue; }
        if seg == ".." { out.pop(); } else { out.push(seg); }
    }
    out.join("/")
}
fn strip_index(s: &str) -> String { match s.strip_suffix("/index") { Some(r) => r.to_string(), None => s.to_string() } }

// resolveImportPath core: relative specifiers resolve; bare specifiers are external (None).
fn resolve_path(from_file: &str, spec: &str) -> Option<String> {
    if !spec.starts_with('.') { return None; }
    let joined = normalize_segments(&format!("{}/{}", dirname(from_file), spec));
    let joined = strip_index(&joined);
    Some(strip_ext(&joined))
}
// fileKey: normSlash → stripExt → strip trailing /index (deliberately a different order than resolve).
fn file_key(p: Option<&Value>) -> String {
    let e = strip_ext(&norm_slash(&str_qq(p)));
    strip_index(&e)
}

pub fn resolveImportPath(from_file: &Value, spec: &Value) -> Value {
    match resolve_path(&str_qq(Some(from_file)), &str_qq(Some(spec))) {
        Some(x) => json!(x),
        None => Value::Null,
    }
}

// ── export parsing (ESM + CommonJS) ─────────────────────────────────────────────
struct Exports { names: Vec<String>, has_default: bool }

fn re_export_decl() -> &'static Regex { static R: OnceLock<Regex> = OnceLock::new(); R.get_or_init(|| Regex::new(r"\bexport\s+(?:async\s+)?(?:function\*?|class|const|let|var)\s+([A-Za-z_$][\w$]*)").unwrap()) }
fn re_export_default() -> &'static Regex { static R: OnceLock<Regex> = OnceLock::new(); R.get_or_init(|| Regex::new(r"\bexport\s+default\b").unwrap()) }
fn re_export_list() -> &'static Regex { static R: OnceLock<Regex> = OnceLock::new(); R.get_or_init(|| Regex::new(r"\bexport\s*\{([^}]*)\}").unwrap()) }
fn re_as_alias() -> &'static Regex { static R: OnceLock<Regex> = OnceLock::new(); R.get_or_init(|| Regex::new(r"(?:[\w$]+)\s+as\s+([\w$]+)").unwrap()) }
fn re_cjs_exports_prop() -> &'static Regex { static R: OnceLock<Regex> = OnceLock::new(); R.get_or_init(|| Regex::new(r"\b(?:module\.)?exports\.([A-Za-z_$][\w$]*)\s*=").unwrap()) }
fn re_module_exports_obj() -> &'static Regex { static R: OnceLock<Regex> = OnceLock::new(); R.get_or_init(|| Regex::new(r"\bmodule\.exports\s*=\s*\{([^}]*)\}").unwrap()) }
fn re_module_exports_ident() -> &'static Regex { static R: OnceLock<Regex> = OnceLock::new(); R.get_or_init(|| Regex::new(r"\bmodule\.exports\s*=\s*[A-Za-z_$]").unwrap()) }

fn parse_exports_inner(code: &str) -> Exports {
    let mut names: Vec<String> = Vec::new();
    let mut has_default = false;
    for cap in re_export_decl().captures_iter(code) {
        if let Some(m) = cap.get(1) { add_unique(&mut names, m.as_str()); }
    }
    if re_export_default().is_match(code) { has_default = true; }
    for cap in re_export_list().captures_iter(code) {
        let inner = cap.get(1).map(|m| m.as_str()).unwrap_or("");
        for part in inner.split(',') {
            let t = part.trim();
            if t.is_empty() { continue; }
            let name = match re_as_alias().captures(t) {
                Some(am) => am.get(1).map(|m| m.as_str().to_string()).unwrap_or_default(),
                None => t.split_whitespace().next().unwrap_or("").to_string(),
            };
            if name == "default" { has_default = true; }
            else if !name.is_empty() { add_unique(&mut names, &name); }
        }
    }
    for cap in re_cjs_exports_prop().captures_iter(code) {
        if let Some(m) = cap.get(1) { add_unique(&mut names, m.as_str()); }
    }
    let me_obj = re_module_exports_obj().captures(code);
    if let Some(mo) = &me_obj {
        let inner = mo.get(1).map(|m| m.as_str()).unwrap_or("");
        for part in inner.split(',') {
            let t = part.trim();
            if t.is_empty() { continue; }
            let name = t.split(':').next().unwrap_or("").trim();
            if !name.is_empty() { add_unique(&mut names, name); }
        }
    }
    if re_module_exports_ident().is_match(code) && me_obj.is_none() { has_default = true; }
    Exports { names, has_default }
}

// TRANSITIVE (JS returns { names: Set, hasDefault }): pub fn exists for names_ok; names rendered as an
// ordered array here (insertion order preserved) so the internal check + the surface stay faithful.
pub fn parseExports(code: &Value) -> Value {
    let ex = parse_exports_inner(&str_qq(Some(code)));
    json!({ "names": ex.names, "hasDefault": ex.has_default })
}

// ── import parsing (ESM + CommonJS) ─────────────────────────────────────────────
struct Import { from: String, names: Vec<String>, default: Option<String>, namespace: Option<String> }

fn re_import_from() -> &'static Regex { static R: OnceLock<Regex> = OnceLock::new(); R.get_or_init(|| Regex::new(r#"\bimport\s+([^;'"]+?)\s+from\s*['"]([^'"]+)['"]"#).unwrap()) }
fn re_import_bare() -> &'static Regex { static R: OnceLock<Regex> = OnceLock::new(); R.get_or_init(|| Regex::new(r#"\bimport\s*['"]([^'"]+)['"]"#).unwrap()) }
fn re_require() -> &'static Regex { static R: OnceLock<Regex> = OnceLock::new(); R.get_or_init(|| Regex::new(r#"\b(?:const|let|var)\s+(\{[^}]*\}|[\w$]+)\s*=\s*require\(\s*['"]([^'"]+)['"]\s*\)"#).unwrap()) }
fn re_ns() -> &'static Regex { static R: OnceLock<Regex> = OnceLock::new(); R.get_or_init(|| Regex::new(r"\*\s+as\s+([\w$]+)").unwrap()) }
fn re_braces() -> &'static Regex { static R: OnceLock<Regex> = OnceLock::new(); R.get_or_init(|| Regex::new(r"\{([^}]*)\}").unwrap()) }
fn re_as_kw() -> &'static Regex { static R: OnceLock<Regex> = OnceLock::new(); R.get_or_init(|| Regex::new(r"\s+as\s+").unwrap()) }
fn re_ident() -> &'static Regex { static R: OnceLock<Regex> = OnceLock::new(); R.get_or_init(|| Regex::new(r"^[A-Za-z_$][\w$]*$").unwrap()) }

fn parse_imports_inner(code: &str) -> Vec<Import> {
    let mut out: Vec<Import> = Vec::new();
    // import <clause> from 'path'
    for cap in re_import_from().captures_iter(code) {
        let clause = cap.get(1).map(|m| m.as_str()).unwrap_or("").trim().to_string();
        let from = cap.get(2).map(|m| m.as_str()).unwrap_or("").to_string();
        let mut names: Vec<String> = Vec::new();
        let namespace = re_ns().captures(&clause).and_then(|c| c.get(1).map(|m| m.as_str().to_string()));
        if let Some(bc) = re_braces().captures(&clause) {
            let inner = bc.get(1).map(|m| m.as_str()).unwrap_or("");
            for part in inner.split(',') {
                let t = part.trim();
                if t.is_empty() { continue; }
                // import { A as B } → the IMPORTED (source) name is A
                let name = re_as_kw().split(t).next().unwrap_or("").trim();
                if !name.is_empty() { add_unique(&mut names, name); }
            }
        }
        let step1 = re_braces().replace(&clause, "");
        let step2 = re_ns().replace(&step1, "");
        let step3 = step2.replace(',', " ");
        let step3 = step3.trim();
        let def_tok = step3.split_whitespace().next().unwrap_or("");
        let default = if !def_tok.is_empty() && def_tok != "{" && namespace.is_none() && re_ident().is_match(def_tok) {
            Some(def_tok.to_string())
        } else { None };
        out.push(Import { from, names, default, namespace });
    }
    // import 'path' (side-effect only)
    for cap in re_import_bare().captures_iter(code) {
        let from = cap.get(1).map(|m| m.as_str()).unwrap_or("").to_string();
        if !out.iter().any(|r| r.from == from) {
            out.push(Import { from, names: Vec::new(), default: None, namespace: None });
        }
    }
    // const { A, B } = require('path')  /  const X = require('path')
    for cap in re_require().captures_iter(code) {
        let binding = cap.get(1).map(|m| m.as_str()).unwrap_or("");
        let from = cap.get(2).map(|m| m.as_str()).unwrap_or("").to_string();
        let mut names: Vec<String> = Vec::new();
        let mut default = None;
        let mut namespace = None;
        if binding.starts_with('{') {
            let inner = &binding[1..binding.len() - 1]; // slice(1,-1)
            for part in inner.split(',') {
                let t = part.trim();
                if t.is_empty() { continue; }
                let name = t.split(':').next().unwrap_or("").trim();
                if !name.is_empty() { add_unique(&mut names, name); }
            }
        } else {
            default = Some(binding.to_string());
            namespace = Some(binding.to_string());
        }
        out.push(Import { from, names, default, namespace });
    }
    out
}

// TRANSITIVE (JS returns [{ from, names: Set, default, namespace }]).
pub fn parseImports(code: &Value) -> Value {
    let ims = parse_imports_inner(&str_qq(Some(code)));
    Value::Array(ims.iter().map(|im| json!({
        "from": im.from,
        "names": im.names,
        "default": im.default.clone().map(Value::String).unwrap_or(Value::Null),
        "namespace": im.namespace.clone().map(Value::String).unwrap_or(Value::Null),
    })).collect())
}

// ── checkComposition ────────────────────────────────────────────────────────────
fn check_composition_inner(manifest: &Value, files: &Value) -> Value {
    let blocks: Vec<Value> = manifest.get("blocks").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    let bindings: Vec<Value> = manifest.get("bindings").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    let files_arr: Vec<Value> = files.as_array().cloned().unwrap_or_default();

    // fileByPath / blockById: JS Map — later entries overwrite earlier for a duplicate key.
    let mut file_by_path: HashMap<String, &Value> = HashMap::new();
    for f in &files_arr { file_by_path.insert(file_key(f.get("path")), f); }
    let mut block_by_id: HashMap<String, &Value> = HashMap::new();
    for a in &blocks {
        if truthy(a.get("id")) { block_by_id.insert(key_of(a.get("id").unwrap()), a); }
    }

    let mut mismatches: Vec<Value> = Vec::new();
    let notes: Vec<Value> = Vec::new();
    let mut checked: i64 = 0;

    for b in &bindings {
        let from = b.get("from");
        let to = b.get("to");
        let fb = from.and_then(|f| f.get("block"));
        let tb = to.and_then(|t| t.get("block"));
        let fp = from.and_then(|f| f.get("port"));
        let tp = to.and_then(|t| t.get("port"));
        if !truthy(fb) || !truthy(tb) || !truthy(fp) || !truthy(tp) { continue; }
        let fb = fb.unwrap(); let tb = tb.unwrap(); let fp = fp.unwrap(); let tp = tp.unwrap();
        let (prod_block, cons_block) = match (block_by_id.get(&key_of(fb)).copied(), block_by_id.get(&key_of(tb)).copied()) {
            (Some(p), Some(c)) => (p, c),
            _ => continue,
        };
        let prod_file = file_by_path.get(&file_key(prod_block.get("file"))).copied();
        let cons_file = file_by_path.get(&file_key(cons_block.get("file"))).copied();
        let label: Value = match nz(b.get("id")) {
            Some(v) => v.clone(),
            None => json!(format!("{}.{}->{}.{}", tmpl(Some(fb)), tmpl(Some(fp)), tmpl(Some(tb)), tmpl(Some(tp)))),
        };
        if prod_file.is_none() {
            mismatches.push(json!({ "binding": label, "kind": "missing_file",
                "detail": format!("producer block '{}' file '{}' was not authored", tmpl(Some(fb)), tmpl(prod_block.get("file"))) }));
            continue;
        }
        if cons_file.is_none() {
            mismatches.push(json!({ "binding": label, "kind": "missing_file",
                "detail": format!("consumer block '{}' file '{}' was not authored", tmpl(Some(tb)), tmpl(cons_block.get("file"))) }));
            continue;
        }
        let prod_file = prod_file.unwrap();
        let cons_file = cons_file.unwrap();
        checked += 1;
        let port = fp; // b.from.port
        let port_str = port.as_str();

        // 1) producer really exports the port symbol
        let ex = parse_exports_inner(&str_qq(prod_file.get("content")));
        let has_export = port_str.map_or(false, |s| ex.names.iter().any(|n| n == s))
            || (port_str == Some("default") && ex.has_default);
        if !has_export {
            let joined = ex.names.join(", ");
            let names_part = if joined.is_empty() { "\u{2205}".to_string() } else { joined };
            let default_part = if ex.has_default { ", default" } else { "" };
            mismatches.push(json!({ "binding": label.clone(), "kind": "missing_export",
                "detail": format!("producer '{}' does not export '{}' (exports: {}{})",
                    tmpl(prod_block.get("file")), tmpl(Some(port)), names_part, default_part) }));
        }

        // 2) consumer really imports the port symbol from the producer's path
        let ims = parse_imports_inner(&str_qq(cons_file.get("content")));
        let prod_k = file_key(prod_block.get("file"));
        let cons_file_field = str_qq(cons_block.get("file"));
        let mut imported_somewhere = false;
        let mut imported_from_producer = false;
        for im in &ims {
            let has = port_str.map_or(false, |s| im.names.iter().any(|n| n == s))
                || (port_str == Some("default") && im.default.is_some())
                || im.namespace.is_some();
            if !has { continue; }
            imported_somewhere = true;
            if let Some(resolved) = resolve_path(&cons_file_field, &im.from) {
                if resolved == prod_k { imported_from_producer = true; }
            }
        }
        if !imported_somewhere {
            mismatches.push(json!({ "binding": label.clone(), "kind": "missing_import",
                "detail": format!("consumer '{}' does not import '{}' at all", tmpl(cons_block.get("file")), tmpl(Some(port))) }));
        } else if !imported_from_producer {
            mismatches.push(json!({ "binding": label.clone(), "kind": "wrong_source",
                "detail": format!("consumer '{}' imports '{}' but not from the producer '{}'", tmpl(cons_block.get("file")), tmpl(Some(port)), tmpl(prod_block.get("file"))) }));
        }
    }

    let ok = mismatches.is_empty();
    json!({ "ok": ok, "mismatches": mismatches, "checked": checked, "notes": notes })
}

pub fn checkComposition(manifest: &Value, files: &Value) -> Value { check_composition_inner(manifest, files) }

// ── default: compositionCheck (compute-only; db lane) ────────────────────────────
pub fn compositionCheck(input: &Value, _db: &Connection) -> Result<Value, String> {
    let manifest = crate::swarm_manifest_lint::parseManifest(input);
    if !truthy(Some(&manifest)) { return Err("composition-check: manifest is required".into()); }
    let files = match input.get("files") {
        Some(f) if f.is_array() => f.clone(),
        _ => return Err("composition-check: files is required".into()),
    };
    Ok(check_composition_inner(&manifest, &files))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest() -> Value {
        json!({
            "version": 1, "goal": "config -> server",
            "blocks": [
                { "id": "config", "file": "src/config.js", "role": "config", "produces": [{ "name": "loadConfig", "type": "Fn" }], "consumes": [] },
                { "id": "server", "file": "src/server.js", "role": "server", "produces": [], "consumes": [{ "name": "loadConfig", "type": "Fn" }] }
            ],
            "bindings": [{ "id": "b1", "from": { "block": "config", "port": "loadConfig" }, "to": { "block": "server", "port": "loadConfig" }, "type": "Fn" }]
        })
    }

    #[test]
    fn resolves_paths() {
        assert_eq!(resolveImportPath(&json!("src/server.js"), &json!("./config.js")), json!("src/config"));
        assert_eq!(resolveImportPath(&json!("src/a/b.js"), &json!("../config")), json!("src/config"));
        assert_eq!(resolveImportPath(&json!("src/server.js"), &json!("express")), Value::Null);
        assert_eq!(resolveImportPath(&json!("src/server.js"), &json!("./dir/index")), json!("src/dir"));
    }

    #[test]
    fn parses_exports_and_imports() {
        let ex = parse_exports_inner("export function a(){}\nexport const b = 1\nexport class C {}\nexport { d, e as f }\nexport default 3");
        assert!(["a", "b", "C", "d", "f"].iter().all(|n| ex.names.iter().any(|x| x == n)));
        assert!(ex.has_default);
        let cjs = parse_exports_inner("module.exports = { a, b }\nexports.c = 1");
        assert!(["a", "b", "c"].iter().all(|n| cjs.names.iter().any(|x| x == n)));

        let ims = parse_imports_inner("import x from './x.js'\nimport { a, b as c } from './ab.js'\nimport * as ns from './ns.js'\nconst { d } = require('./d.js')\nconst e = require('./e.js')");
        let byf = |from: &str| ims.iter().find(|i| i.from == from).unwrap();
        assert_eq!(byf("./x.js").default.as_deref(), Some("x"));
        assert!(byf("./ab.js").names.iter().any(|n| n == "a") && byf("./ab.js").names.iter().any(|n| n == "b"));
        assert_eq!(byf("./ns.js").namespace.as_deref(), Some("ns"));
        assert!(byf("./d.js").names.iter().any(|n| n == "d"));
        assert_eq!(byf("./e.js").default.as_deref(), Some("e"));
        // transitive surface exists
        assert_eq!(parseExports(&json!("export default 1"))["hasDefault"], json!(true));
        assert!(parseImports(&json!("import q from './q.js'")).is_array());
    }

    #[test]
    fn checks_composition_branches() {
        // ok: code that really composes
        let ok = checkComposition(&manifest(), &json!([
            { "path": "src/config.js", "content": "export function loadConfig(){ return {} }" },
            { "path": "src/server.js", "content": "import { loadConfig } from './config.js'\nexport function start(){ loadConfig() }" }
        ]));
        assert_eq!(ok["ok"], json!(true));
        assert_eq!(ok["checked"], json!(1));
        assert_eq!(ok["mismatches"].as_array().unwrap().len(), 0);
        assert_eq!(ok["notes"], json!([]));

        // missing_export
        let me = checkComposition(&manifest(), &json!([
            { "path": "src/config.js", "content": "export function somethingElse(){}" },
            { "path": "src/server.js", "content": "import { loadConfig } from './config.js'" }
        ]));
        assert_eq!(me["ok"], json!(false));
        let m0 = &me["mismatches"][0];
        assert_eq!(m0["kind"], json!("missing_export"));
        assert_eq!(m0["detail"], json!("producer 'src/config.js' does not export 'loadConfig' (exports: somethingElse)"));

        // missing_import
        let mi = checkComposition(&manifest(), &json!([
            { "path": "src/config.js", "content": "export function loadConfig(){}" },
            { "path": "src/server.js", "content": "const x = 1" }
        ]));
        assert!(mi["mismatches"].as_array().unwrap().iter().any(|m| m["kind"] == json!("missing_import")));

        // wrong_source
        let ws = checkComposition(&manifest(), &json!([
            { "path": "src/config.js", "content": "export function loadConfig(){}" },
            { "path": "src/server.js", "content": "import { loadConfig } from './other.js'" }
        ]));
        assert!(ws["mismatches"].as_array().unwrap().iter().any(|m| m["kind"] == json!("wrong_source")));

        // missing_file (producer unauthored)
        let mf = checkComposition(&manifest(), &json!([
            { "path": "src/server.js", "content": "import { loadConfig } from './config.js'" }
        ]));
        assert!(mf["mismatches"].as_array().unwrap().iter().any(|m| m["kind"] == json!("missing_file")));
    }

    #[test]
    fn default_delegates_to_parse_manifest() {
        let conn = Connection::open_in_memory().unwrap();
        let r = compositionCheck(&json!({
            "manifest": manifest(),
            "files": [
                { "path": "src/config.js", "content": "export function loadConfig(){ return {} }" },
                { "path": "src/server.js", "content": "import { loadConfig } from './config.js'\nexport function start(){ loadConfig() }" }
            ]
        }), &conn).unwrap();
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["checked"], json!(1));
    }
}
