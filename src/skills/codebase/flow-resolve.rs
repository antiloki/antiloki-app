//! flow-resolve.rs — twin of skills/codebase/flow-resolve.mjs (helper module, all no-db). NAMES ARE
//! CONTRACT: normType · detectDispatches · detectHandlers · extractFlow · resolveFlowFromExtracts · resolveFlow.
// QUALITY: 80/100 — dispatch/reducer edges by normalized action-type key (regex); deterministic join
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;
use regex::Regex;

fn js_str(v: &Value) -> String { match v { Value::String(s) => s.clone(), Value::Null => String::new(), o => o.to_string() } }
fn line_of(src: &str, idx: usize) -> i64 {
    src.as_bytes()[..idx.min(src.len())].iter().filter(|&&b| b == b'\n').count() as i64 + 1
}
fn norm_type_str(raw: &str) -> String {
    static STR: OnceLock<Regex> = OnceLock::new();
    static SEG: OnceLock<Regex> = OnceLock::new();
    let s = raw.trim();
    let str_re = STR.get_or_init(|| Regex::new(r#"^['"`]([^'"`]+)['"`]$"#).unwrap());
    if let Some(c) = str_re.captures(s) { return c.get(1).unwrap().as_str().to_string(); }
    let seg = SEG.get_or_init(|| Regex::new(r"([A-Za-z_$][\w$]*)$").unwrap());
    if let Some(c) = seg.captures(s) { return c.get(1).unwrap().as_str().to_string(); }
    s.to_string()
}

pub fn normType(raw: &Value) -> Value { json!(norm_type_str(&js_str(raw))) }

fn detect_dispatches(source: &str) -> Vec<Value> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r#"(?s)\bdispatch\s*\(\s*\{.{0,300}?\btype\s*:\s*([A-Za-z_$][\w$.]*|['"`][^'"`]+['"`])"#).unwrap());
    re.captures_iter(source).map(|c| json!({ "type": norm_type_str(c.get(1).unwrap().as_str()), "line": line_of(source, c.get(0).unwrap().start()) })).collect()
}
pub fn detectDispatches(source: &Value) -> Value { json!(detect_dispatches(&js_str(source))) }

fn detect_handlers(source: &str) -> Vec<Value> {
    static SW: OnceLock<Regex> = OnceLock::new();
    static CASE: OnceLock<Regex> = OnceLock::new();
    static THUNK: OnceLock<Regex> = OnceLock::new();
    let mut out = Vec::new();
    if SW.get_or_init(|| Regex::new(r"\bswitch\s*\(\s*[\w.$]*\.type\s*\)").unwrap()).is_match(source) {
        let case = CASE.get_or_init(|| Regex::new(r#"\bcase\s+([A-Za-z_$][\w$.]*|['"`][^'"`]+['"`])\s*:"#).unwrap());
        for c in case.captures_iter(source) {
            out.push(json!({ "type": norm_type_str(c.get(1).unwrap().as_str()), "line": line_of(source, c.get(0).unwrap().start()) }));
        }
    }
    let thunk = THUNK.get_or_init(|| Regex::new(r#"\bcreateAsyncThunk\s*\(\s*['"`]([^'"`]+)['"`]"#).unwrap());
    for t in thunk.captures_iter(source) {
        let base = norm_type_str(&format!("'{}'", t.get(1).unwrap().as_str()));
        let line = line_of(source, t.get(0).unwrap().start());
        for suffix in ["", "/pending", "/fulfilled", "/rejected"] {
            out.push(json!({ "type": format!("{base}{suffix}"), "line": line }));
        }
    }
    out
}
pub fn detectHandlers(source: &Value) -> Value { json!(detect_handlers(&js_str(source))) }

fn extract_flow(path: &Value, source: &Value) -> Value {
    let src = js_str(source);
    json!({
        "path": path,
        "dispatches": detect_dispatches(&src).iter().map(|d| d.get("type").cloned().unwrap_or(Value::Null)).collect::<Vec<_>>(),
        "handlers": detect_handlers(&src).iter().map(|h| h.get("type").cloned().unwrap_or(Value::Null)).collect::<Vec<_>>(),
    })
}
pub fn extractFlow(path: &Value, source: &Value) -> Value { extract_flow(path, source) }

pub fn resolveFlowFromExtracts(extracts: &Value) -> Value {
    let mut handlers: HashMap<String, Vec<String>> = HashMap::new();
    let mut dispatchers: Vec<(String, String)> = Vec::new();
    let empty = vec![];
    for e in extracts.as_array().unwrap_or(&empty) {
        let path = js_str(e.get("path").unwrap_or(&Value::Null));
        for t in e.get("dispatches").and_then(|v| v.as_array()).unwrap_or(&empty) { dispatchers.push((path.clone(), js_str(t))); }
        for t in e.get("handlers").and_then(|v| v.as_array()).unwrap_or(&empty) {
            let v = handlers.entry(js_str(t)).or_default();
            if !v.contains(&path) { v.push(path.clone()); }
        }
    }
    let mut edges = Vec::new();
    let mut seen = HashSet::new();
    for (dpath, dtype) in &dispatchers {
        if let Some(tos) = handlers.get(dtype) {
            for to in tos {
                if to == dpath { continue; }
                if seen.insert(format!("{dpath}|{to}|{dtype}")) {
                    edges.push(json!({ "from_node": dpath, "to_node": to, "type": "dispatches", "output": dtype }));
                }
            }
        }
    }
    json!(edges)
}

pub fn resolveFlow(files: &Value) -> Value {
    let empty = vec![];
    let extracts: Vec<Value> = files.as_array().unwrap_or(&empty).iter()
        .map(|f| extract_flow(f.get("path").unwrap_or(&Value::Null), f.get("source").unwrap_or(&Value::Null))).collect();
    resolveFlowFromExtracts(&json!(extracts))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn resolves_flow() {
        assert_eq!(normType(&json!("actionTypes.SEARCH")), json!("SEARCH"));
        assert_eq!(normType(&json!("'products/search'")), json!("products/search"));
        let ds = detectDispatches(&json!("dispatch({ type: actionTypes.SEARCH, x: 1 });\ndispatch({ type: 'SET_ID' });"));
        let types: Vec<String> = ds.as_array().unwrap().iter().map(|d| js_str(d.get("type").unwrap())).collect();
        assert!(types.contains(&"SEARCH".to_string()) && types.contains(&"SET_ID".to_string()));
        let files = json!([
            { "path": "a.js", "source": "dispatch({ type: actionTypes.SEARCH });" },
            { "path": "reducer.js", "source": "switch (action.type) { case actionType.SEARCH: return s; }" }
        ]);
        let edges = resolveFlow(&files);
        assert_eq!(edges.as_array().unwrap().len(), 1);
        assert_eq!(edges[0]["from_node"], json!("a.js"));
        assert_eq!(edges[0]["to_node"], json!("reducer.js"));
    }
}
