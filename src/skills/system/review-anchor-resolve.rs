//! review-anchor-resolve.rs — twin of skills/system/review-anchor-resolve.mjs (no-db, pure). NAMES ARE
//! CONTRACT: reviewAnchorResolve (default) · acLabel · findAddedToken · matchFile. Map each AC to a diff spot.
// QUALITY: 82/100 — per-verifier-kind anchoring (added-token line / first hunk) vs behavioral fallback
use rusqlite::Connection;
use serde_json::{json, Value};

fn truthy(v: Option<&Value>) -> bool {
    match v { None | Some(Value::Null) => false, Some(Value::Bool(b)) => *b, Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()), Some(Value::String(s)) => !s.is_empty(), Some(_) => true }
}
fn str_qq(v: Option<&Value>) -> String {
    match v { None | Some(Value::Null) => String::new(), Some(Value::String(s)) => s.clone(), Some(Value::Bool(b)) => b.to_string(), Some(n @ Value::Number(_)) => n.to_string(), Some(o) => o.to_string() }
}
fn js_string(v: &Value) -> String { match v { Value::Null => "null".into(), Value::String(s) => s.clone(), Value::Number(n) => n.to_string(), Value::Bool(b) => b.to_string(), o => o.to_string() } }
fn take80(s: &str) -> String { s.chars().take(80).collect() }
fn q_or(v: Option<&Value>, dflt: &str) -> String { v.filter(|x| !x.is_null()).map(|x| str_qq(Some(x))).unwrap_or_else(|| dflt.to_string()) }

pub fn acLabel(item: &Value) -> Value {
    let kind = item.get("kind").filter(|v| !v.is_null()).cloned().unwrap_or(json!("unknown"));
    match kind.as_str() {
        Some("file_exists") | Some("file-exists") => json!(format!("file: {}", q_or(item.get("path"), "?"))),
        Some(k @ "test") | Some(k @ "cli") => json!(format!("{}: {}", k, take80(&str_qq(item.get("cmd"))))),
        Some("sql") => json!(format!("sql: {}", take80(&str_qq(item.get("query"))))),
        Some("no_law_violation") => json!(format!("law: {}", q_or(item.get("law_slug"), "?"))),
        Some("skill_returns") => json!(format!("action: {}", q_or(item.get("action"), "?"))),
        Some("diff_in_paths") => json!("diff scoped to allowed paths"),
        _ => kind,
    }
}

pub fn findAddedToken(file: &Value, token: &Value) -> Value {
    if !truthy(Some(file)) { return Value::Null; }
    let tok = str_qq(Some(token));
    let empty = vec![];
    for hunk in file.get("hunks").and_then(|v| v.as_array()).unwrap_or(&empty) {
        for ln in hunk.get("lines").and_then(|v| v.as_array()).unwrap_or(&empty) {
            if ln.get("sign").and_then(|v| v.as_str()) == Some("+") && ln.get("new_line").map_or(false, |v| !v.is_null()) && str_qq(ln.get("content")).contains(&tok) {
                return json!({ "new_line": ln.get("new_line").cloned().unwrap_or(Value::Null), "content": ln.get("content").cloned().unwrap_or(Value::Null) });
            }
        }
    }
    Value::Null
}

fn first_hunk_line(file: &Value) -> Value {
    let empty = vec![];
    let h = match file.get("hunks").and_then(|v| v.as_array()).unwrap_or(&empty).first() { Some(h) => h, None => return Value::Null };
    let first_added = h.get("lines").and_then(|v| v.as_array()).unwrap_or(&empty).iter().find(|l| l.get("sign").and_then(|v| v.as_str()) == Some("+") && l.get("new_line").map_or(false, |v| !v.is_null()));
    match first_added.and_then(|l| l.get("new_line")).filter(|v| !v.is_null()) {
        Some(nl) => nl.clone(),
        None => h.get("new_start").filter(|v| !v.is_null()).cloned().unwrap_or(Value::Null),
    }
}

pub fn matchFile(diff_files: &Value, path: &Value) -> Value {
    if !truthy(Some(path)) { return Value::Null; }
    let ps = str_qq(Some(path));
    let empty = vec![];
    let files = diff_files.as_array().unwrap_or(&empty);
    if let Some(f) = files.iter().find(|f| f.get("path") == Some(path)) { return f.clone(); }
    if let Some(f) = files.iter().find(|f| { let fp = str_qq(f.get("path")); fp.ends_with(&ps) || ps.ends_with(&fp) }) { return f.clone(); }
    Value::Null
}

pub fn reviewAnchorResolve(input: &Value, _db: &Connection) -> Result<Value, String> {
    let empty: Vec<Value> = vec![];
    let acceptance = input.get("acceptance").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    let qa_results = input.get("qa_results").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    let diff_files = input.get("diff").and_then(|d| d.get("files")).filter(|v| v.is_array()).cloned().unwrap_or(json!([]));
    let mut qa_by: std::collections::HashMap<String, Value> = std::collections::HashMap::new();
    for r in &qa_results { qa_by.insert(js_string(r.get("ac_index").unwrap_or(&Value::Null)), r.clone()); }

    let mut anchors: Vec<Value> = Vec::new();
    let mut behavioral: Vec<Value> = Vec::new();
    for (i, item_raw) in acceptance.iter().enumerate() {
        let item = if item_raw.is_null() { json!({}) } else { item_raw.clone() };
        let label = acLabel(&item);
        let qa = qa_by.get(&js_string(&json!(i)));
        let passed = match qa { Some(q) => json!(truthy(q.get("passed"))), None => Value::Null };
        let evidence_md = qa.and_then(|q| q.get("evidence_md")).filter(|v| !v.is_null()).cloned().unwrap_or(Value::Null);
        let kind_val = item.get("kind").cloned().unwrap_or(Value::Null);
        let kind = kind_val.as_str();
        let behav = json!({ "ac_index": i, "label": label, "kind": kind_val, "passed": passed, "evidence_md": evidence_md });

        if kind == Some("file_exists") || kind == Some("file-exists") {
            let file = matchFile(&diff_files, item.get("path").unwrap_or(&Value::Null));
            let tokens = item.get("must_contain").and_then(|v| v.as_array()).cloned().unwrap_or_default();
            let mut anchored = false;
            if truthy(Some(&file)) && !tokens.is_empty() {
                for token in &tokens {
                    let hit = findAddedToken(&file, token);
                    if !hit.is_null() {
                        anchors.push(json!({ "ac_index": i, "label": label, "file": file.get("path").cloned().unwrap_or(Value::Null), "new_line": hit.get("new_line").cloned().unwrap_or(Value::Null), "token": token, "passed": passed, "evidence_md": evidence_md }));
                        anchored = true;
                    }
                }
            }
            if !anchored && truthy(Some(&file)) {
                let ln = first_hunk_line(&file);
                if !ln.is_null() {
                    anchors.push(json!({ "ac_index": i, "label": label, "file": file.get("path").cloned().unwrap_or(Value::Null), "new_line": ln, "token": Value::Null, "passed": passed, "evidence_md": evidence_md }));
                    anchored = true;
                }
            }
            if !anchored { behavioral.push(behav); }
            continue;
        }
        if kind == Some("test") || kind == Some("cli") {
            let cmd = str_qq(item.get("cmd"));
            let file = diff_files.as_array().and_then(|files| files.iter().find(|f| cmd.contains(&str_qq(f.get("path")))).cloned());
            match file {
                Some(f) => { let ln = first_hunk_line(&f); anchors.push(json!({ "ac_index": i, "label": label, "file": f.get("path").cloned().unwrap_or(Value::Null), "new_line": ln, "token": Value::Null, "passed": passed, "evidence_md": evidence_md })); }
                None => behavioral.push(behav),
            }
            continue;
        }
        behavioral.push(behav);
    }
    Ok(json!({ "anchors": anchors, "behavioral": behavioral }))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn diff() -> Value { json!({ "files": [{ "path": "a.mjs", "status": "modified", "hunks": [{ "new_start": 1, "lines": [
        { "sign": " ", "content": "const x = 1;", "new_line": 1 },
        { "sign": "+", "content": "export function doThing() {}", "new_line": 2 },
        { "sign": "+", "content": "const TOKEN_HERE = 42;", "new_line": 3 }] }] }] }) }
    #[test]
    fn resolves() {
        assert_eq!(acLabel(&json!({ "kind": "file-exists", "path": "a.mjs" })), json!("file: a.mjs"));
        assert_eq!(acLabel(&json!({ "kind": "sql", "query": "SELECT 1" })), json!("sql: SELECT 1"));
        assert_eq!(acLabel(&json!({ "kind": "weird" })), json!("weird"));
        assert_eq!(findAddedToken(&diff()["files"][0], &json!("TOKEN_HERE"))["new_line"], json!(3));
        assert!(findAddedToken(&diff()["files"][0], &json!("const x")).is_null()); // context line, not added
        assert_eq!(matchFile(&diff()["files"], &json!("x/a.mjs"))["path"], json!("a.mjs")); // suffix
        let r = reviewAnchorResolve(&json!({ "acceptance": [
            { "kind": "file-exists", "path": "a.mjs", "must_contain": ["doThing"] },
            { "kind": "sql", "query": "x" }], "qa_results": [{ "ac_index": 0, "passed": true }], "diff": diff() }), &Connection::open_in_memory().unwrap()).unwrap();
        assert_eq!(r["anchors"][0]["new_line"], json!(2));
        assert_eq!(r["anchors"][0]["passed"], json!(true));
        assert_eq!(r["behavioral"][0]["kind"], json!("sql"));
    }
}
