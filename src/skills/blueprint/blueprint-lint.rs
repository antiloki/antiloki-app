//! blueprint-lint.rs — twin of skills/blueprint/blueprint-lint.mjs (no-db, pure). NAMES ARE CONTRACT:
//! `blueprintLint` (default) · detectCycle · normPath · normIO. Deterministic gate over a blueprint graph.
// QUALITY: 83/100 — file/leaf intent, files_touched scope+coverage, binding dangling/type-match, cycle DFS
use regex::Regex;
use rusqlite::Connection;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

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
fn tmpl(v: Option<&Value>) -> String { match v { None => "undefined".into(), Some(Value::Null) => "null".into(), Some(x) => str_qq(Some(x)) } }
fn jstr(v: &Value) -> String { serde_json::to_string(v).unwrap_or_else(|_| "null".into()) }

fn norm_path_str(v: Option<&Value>) -> String {
    static R: OnceLock<Regex> = OnceLock::new();
    let re = R.get_or_init(|| Regex::new(r"^\.?/").unwrap());
    let s = str_qq(v);
    let s = s.trim();
    re.replace(s, "").into_owned()
}
fn norm_io_str(v: Option<&Value>) -> String {
    static Q: OnceLock<Regex> = OnceLock::new();
    static W: OnceLock<Regex> = OnceLock::new();
    let q = Q.get_or_init(|| Regex::new(r#"['"]"#).unwrap());
    let w = W.get_or_init(|| Regex::new(r"\s+").unwrap());
    let s = str_qq(v);
    let s = s.trim();
    let s = q.replace_all(s, "\"");
    let s = w.replace_all(&s, " ");
    s.to_lowercase()
}

pub fn normPath(p: &Value) -> Value { json!(norm_path_str(Some(p))) }
pub fn normIO(s: &Value) -> Value { json!(norm_io_str(Some(s))) }

const WHITE: u8 = 0;
const GRAY: u8 = 1;
const BLACK: u8 = 2;
struct Cyc { adj: HashMap<String, Vec<String>>, color: HashMap<String, u8>, stack: Vec<String>, cycle: Vec<String> }
impl Cyc {
    fn dfs(&mut self, u: &str) -> bool {
        self.color.insert(u.to_string(), GRAY);
        self.stack.push(u.to_string());
        let neighbors = self.adj.get(u).cloned().unwrap_or_default();
        for v in &neighbors {
            if self.color.get(v).copied() == Some(GRAY) {
                let idx = self.stack.iter().position(|x| x == v).unwrap();
                self.cycle = self.stack[idx..].iter().cloned().chain(std::iter::once(v.clone())).collect();
                return true;
            }
            if self.color.get(v).copied().unwrap_or(WHITE) == WHITE && self.dfs(v) { return true; }
        }
        self.color.insert(u.to_string(), BLACK);
        self.stack.pop();
        false
    }
}
pub fn detectCycle(bindings: &Value) -> Value {
    let mut order: Vec<String> = Vec::new();
    let mut adj: HashMap<String, Vec<String>> = HashMap::new();
    for b in bindings.as_array().map(|a| a.as_slice()).unwrap_or(&[]) {
        let from = b.get("from");
        let to = b.get("to");
        if from.map_or(true, |v| v.is_null()) || to.map_or(true, |v| v.is_null()) { continue; }
        let k = str_qq(from);
        if !adj.contains_key(&k) { order.push(k.clone()); adj.insert(k.clone(), Vec::new()); }
        adj.get_mut(&k).unwrap().push(str_qq(to));
    }
    let mut c = Cyc { adj, color: HashMap::new(), stack: Vec::new(), cycle: Vec::new() };
    for u in &order {
        if c.color.get(u).copied().unwrap_or(WHITE) == WHITE && c.dfs(u) { break; }
    }
    json!(c.cycle)
}

pub fn blueprintLint(input: &Value, _db: &Connection) -> Result<Value, String> {
    // bp = normalizeBlueprint(input?.blueprint ?? input ?? {})
    let bp_src = match input.get("blueprint") { Some(v) if !v.is_null() => v.clone(), _ => if input.is_null() { json!({}) } else { input.clone() } };
    let bp = crate::blueprint_block_shape::normalizeBlueprint(&bp_src);
    let blocks: Vec<Value> = bp.get("blocks").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    let bindings_v = bp.get("bindings").cloned().unwrap_or(json!([]));
    let bindings: Vec<Value> = bindings_v.as_array().cloned().unwrap_or_default();
    let files_touched: Option<Vec<String>> = input.get("files_touched").and_then(|v| v.as_array()).map(|a| a.iter().map(|x| str_qq(Some(x))).collect());

    let mut issues: Vec<Value> = Vec::new();
    let flat = crate::blueprint_block_shape::flattenBlocks(&json!(blocks));
    let flat_obj = flat.as_object().cloned().unwrap_or_default();

    // 1. file blocks well-formed
    for b in &blocks {
        if b.get("type").and_then(|v| v.as_str()) == Some("file") && (!truthy(b.get("path")) || str_qq(b.get("path")).trim().is_empty()) {
            issues.push(json!({ "severity": "blocker", "kind": "block_no_path", "ref": b.get("id").filter(|v| !v.is_null()).cloned().unwrap_or(json!("")), "problem": "file block has no path" }));
        }
        if !truthy(b.get("intent")) || str_qq(b.get("intent")).trim().is_empty() {
            let ref_v = b.get("path").filter(|v| !v.is_null()).or_else(|| b.get("id").filter(|v| !v.is_null())).cloned().unwrap_or(json!(""));
            let label = match b.get("path").filter(|v| !v.is_null()).or_else(|| b.get("id").filter(|v| !v.is_null())) { Some(v) => str_qq(Some(v)), None => "undefined".into() };
            issues.push(json!({ "severity": "blocker", "kind": "block_no_intent", "ref": ref_v, "problem": format!("file block '{}' has no intent (logic description)", label) }));
        }
    }

    // 2. path in scope + files_touched coverage (only when non-empty)
    if let Some(ft) = &files_touched { if !ft.is_empty() {
        let allowed: HashSet<String> = ft.iter().map(|f| norm_path_str(Some(&json!(f)))).collect();
        for b in &blocks {
            if truthy(b.get("path")) && !allowed.contains(&norm_path_str(b.get("path"))) {
                issues.push(json!({ "severity": "blocker", "kind": "path_out_of_scope", "ref": b.get("path").cloned().unwrap_or(json!("")), "problem": format!("file block '{}' is not in contract.files_touched", str_qq(b.get("path"))) }));
            }
        }
        let path_set: HashSet<String> = blocks.iter().map(|b| norm_path_str(b.get("path"))).collect();
        for f in ft {
            if !path_set.contains(&norm_path_str(Some(&json!(f)))) {
                issues.push(json!({ "severity": "blocker", "kind": "files_touched_uncovered", "ref": f, "problem": format!("files_touched entry '{}' has no file block in the blueprint", f) }));
            }
        }
    }}

    // 3. leaf blocks (non-file) have an intent
    for (id, val) in &flat_obj {
        let b = val.get("block").cloned().unwrap_or(Value::Null);
        let file_id = val.get("fileId");
        if b.get("type").and_then(|v| v.as_str()) == Some("file") { continue; }
        let is_leaf = !b.get("children").map_or(false, |v| v.is_array()) || b.get("children").and_then(|v| v.as_array()).map_or(true, |a| a.is_empty());
        if is_leaf && (!truthy(b.get("intent")) || str_qq(b.get("intent")).trim().is_empty()) {
            issues.push(json!({ "severity": "blocker", "kind": "block_no_intent", "ref": id, "problem": format!("leaf block '{}' ({}) has no intent", id, tmpl(file_id)) }));
        }
    }

    // 4. bindings reference real blocks + type-match
    for b in &bindings {
        let from: Option<String> = b.get("from").filter(|v| !v.is_null()).map(|v| str_qq(Some(v)));
        let to: Option<String> = b.get("to").filter(|v| !v.is_null()).map(|v| str_qq(Some(v)));
        let from_ok = from.as_ref().map_or(false, |f| flat_obj.contains_key(f));
        let to_ok = to.as_ref().map_or(false, |t| flat_obj.contains_key(t));
        if from.as_ref().map_or(true, |f| f.is_empty()) || !from_ok {
            issues.push(json!({ "severity": "blocker", "kind": "binding_dangling_from", "ref": from.clone().unwrap_or_default(), "problem": format!("binding.from '{}' is not a block in the blueprint", from.clone().map_or("null".to_string(), |f| f)) }));
        }
        if to.as_ref().map_or(true, |t| t.is_empty()) || !to_ok {
            issues.push(json!({ "severity": "blocker", "kind": "binding_dangling_to", "ref": to.clone().unwrap_or_default(), "problem": format!("binding.to '{}' is not a block in the blueprint", to.clone().map_or("null".to_string(), |t| t)) }));
        }
        let out_present = b.get("output").map_or(false, |v| !v.is_null());
        let in_present = b.get("input").map_or(false, |v| !v.is_null());
        if let (Some(f), Some(t)) = (&from, &to) {
            if !f.is_empty() && !t.is_empty() && from_ok && to_ok && out_present && in_present && norm_io_str(b.get("output")) != norm_io_str(b.get("input")) {
                issues.push(json!({ "severity": "warn", "kind": "binding_type_mismatch", "ref": format!("{}->{}", f, t),
                    "problem": format!("binding {}->{}: output {} vs input {} (descriptions differ — verify they refer to the same data)", f, t, jstr(b.get("output").unwrap()), jstr(b.get("input").unwrap())) }));
            }
        }
    }

    // 5. cycles (warn)
    let cycle = detectCycle(&bindings_v);
    let cyc: Vec<String> = cycle.as_array().unwrap_or(&vec![]).iter().map(|v| str_qq(Some(v))).collect();
    if !cyc.is_empty() {
        issues.push(json!({ "severity": "warn", "kind": "binding_cycle", "ref": cyc.join("->"), "problem": format!("binding cycle (legal for recursion, flagged for review): {}", cyc.join(" -> ")) }));
    }

    let passed = issues.iter().all(|i| i.get("severity").and_then(|v| v.as_str()) != Some("blocker"));
    Ok(json!({ "passed": passed, "issues": issues, "block_count": blocks.len(), "nested_count": flat_obj.len() as i64 - blocks.len() as i64, "binding_count": bindings.len() }))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn call(i: Value) -> Value { blueprintLint(&i, &Connection::open_in_memory().unwrap()).unwrap() }
    #[test]
    fn lints() {
        assert_eq!(normPath(&json!("./a/b.ts")), json!("a/b.ts"));
        assert_eq!(normIO(&json!("  User's List  ")), json!("user\"s list"));
        assert_eq!(detectCycle(&json!([{ "from": "a", "to": "b" }, { "from": "b", "to": "a" }])), json!(["a", "b", "a"]));
        let clean = call(json!({ "blueprint": { "blocks": [{ "id": "f1", "type": "file", "path": "src/a.ts", "intent": "does a" }], "bindings": [] } }));
        assert_eq!(clean["passed"], json!(true));
        assert_eq!(clean["block_count"], json!(1));
        let bad = call(json!({ "blueprint": { "blocks": [{ "id": "f1", "type": "file", "path": "", "intent": "" }], "bindings": [{ "from": "nope", "to": "f1" }] } }));
        assert_eq!(bad["passed"], json!(false));
        let kinds: Vec<&str> = bad["issues"].as_array().unwrap().iter().map(|i| i["kind"].as_str().unwrap()).collect();
        assert!(kinds.contains(&"block_no_path") && kinds.contains(&"block_no_intent") && kinds.contains(&"binding_dangling_from"));
    }
}
