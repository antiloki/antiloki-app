//! risk-tag.rs — twin of skills/entities/risk-tag.mjs (db read+write lane). NAMES ARE CONTRACT:
//! `riskTag` (default). Failure-class tags with a match signature (globs/langs/kw) + learned prevention.
// QUALITY: 82/100 — mint upsert, list, and deterministic match (glob/lang/kw signature) sorted by weight
use regex::Regex;
use rusqlite::{params, Connection};
use serde_json::{json, Value};
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
fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) }

fn glob_to_re(g: &str) -> Result<Regex, regex::Error> {
    static SP: OnceLock<Regex> = OnceLock::new();
    let sp = SP.get_or_init(|| Regex::new(r"[.+^${}()|\[\]\\]").unwrap());
    let escaped = sp.replace_all(g, |c: &regex::Captures| format!("\\{}", &c[0]));
    let s = escaped.replace("**", "<<G>>").replace('*', "[^/]*").replace("<<G>>", ".*");
    Regex::new(&format!("^{}$", s))
}
fn ext_of(p: &str) -> String {
    static R: OnceLock<Regex> = OnceLock::new();
    let re = R.get_or_init(|| Regex::new(r"(?i)\.([a-z0-9]+)$").unwrap());
    re.captures(p).map(|c| c[1].to_lowercase()).unwrap_or_default()
}

fn risk_rows(db: &Connection) -> Vec<Value> {
    let rows = crate::db_json::query_rows(db, "SELECT slug, description, applies_to_json, prevention_md, retryability, source_pattern_key, fix_commit, canary_test, weight, status FROM tags WHERE kind = 'risk'", &[])
        .ok().and_then(|r| r.as_array().cloned()).unwrap_or_default();
    rows.into_iter().map(|t| {
        let applies = t.get("applies_to_json").and_then(|v| v.as_str()).map(|s| serde_json::from_str::<Value>(s).unwrap_or(json!({}))).unwrap_or(json!({}));
        let mut m = t.as_object().cloned().unwrap_or_default();
        m.insert("applies_to".into(), applies);
        Value::Object(m)
    }).collect()
}

pub fn riskTag(input: &Value, db: &Connection) -> Result<Value, String> {
    let op = nz(input.get("op")).map(|v| str_qq(Some(v))).unwrap_or_else(|| "list".to_string());

    if op == "mint" {
        let slug = str_qq(input.get("slug"));
        let slug = slug.trim().to_string();
        if slug.is_empty() { return Err("risk-tag: slug is required".into()); }
        let a = nz(input.get("applies_to")).cloned().unwrap_or(json!({}));
        let desc: String = str_qq(input.get("description")).chars().take(400).collect();
        let _ = db.execute("INSERT INTO tags (slug, kind, description) VALUES (?, 'risk', ?) ON CONFLICT(slug) DO UPDATE SET kind='risk', description=excluded.description", params![slug, desc]);
        let globs: Vec<Value> = a.get("globs").and_then(|v| v.as_array()).map(|arr| arr.iter().map(|g| json!(str_qq(Some(g)))).collect()).unwrap_or_default();
        let langs: Vec<Value> = a.get("langs").and_then(|v| v.as_array()).map(|arr| arr.iter().map(|l| json!(str_qq(Some(l)))).collect()).unwrap_or_default();
        let applies_json = serde_json::to_string(&json!({ "globs": globs, "langs": langs })).unwrap();
        let prevention: String = str_qq(input.get("prevention_md")).chars().take(2000).collect();
        let retry: Option<String> = nz(input.get("retryability")).map(|v| str_qq(Some(v)));
        let spk: Option<String> = nz(input.get("source_pattern_key")).map(|v| str_qq(Some(v)));
        let fix: Option<String> = nz(input.get("fix_commit")).map(|v| str_qq(Some(v)));
        let canary: Option<String> = nz(input.get("canary_test")).map(|v| str_qq(Some(v)));
        let status: Option<String> = nz(input.get("status")).map(|v| str_qq(Some(v)));
        let _ = db.execute(
            "UPDATE tags SET applies_to_json=?, prevention_md=?, retryability=?, source_pattern_key=?, fix_commit=?, canary_test=?, status=COALESCE(?, status) WHERE slug=?",
            params![applies_json, prevention, retry, spk, fix, canary, status, slug]);
        return Ok(json!({ "ok": true, "slug": slug }));
    }

    if op == "list" { return Ok(json!({ "ok": true, "tags": risk_rows(db) })); }

    if op == "match" {
        let mut files: Vec<String> = input.get("files").and_then(|v| v.as_array()).map(|a| a.iter().map(|f| str_qq(Some(f))).collect()).unwrap_or_default();
        if files.is_empty() && truthy(input.get("card_slug")) {
            let cs = str_qq(input.get("card_slug"));
            let rows = crate::db_json::query_rows(db, "SELECT files_touched_json FROM contracts WHERE card_slug = ? ORDER BY id DESC LIMIT 1", &[json!(cs)]).ok().and_then(|r| r.as_array().cloned()).unwrap_or_default();
            let j: Value = rows.first().and_then(|c| c.get("files_touched_json")).and_then(|v| v.as_str()).map(|s| serde_json::from_str::<Value>(s).unwrap_or(json!([]))).unwrap_or(json!([]));
            files = j.as_array().map(|a| a.iter().map(|x| x.as_str().map(String::from).or_else(|| nz(x.get("path")).map(|v| str_qq(Some(v)))).unwrap_or_default()).filter(|s| !s.is_empty()).collect()).unwrap_or_default();
        }
        let norm: Vec<String> = files.iter().map(|f| f.replace('\\', "/")).collect();
        let intent = str_qq(input.get("intent")).to_lowercase();
        let mut matched: Vec<Value> = Vec::new();
        for t in risk_rows(db) {
            let ap = t.get("applies_to").cloned().unwrap_or(json!({}));
            let globs: Vec<String> = ap.get("globs").and_then(|v| v.as_array()).map(|a| a.iter().map(|g| str_qq(Some(g))).collect()).unwrap_or_default();
            let langs: Vec<String> = ap.get("langs").and_then(|v| v.as_array()).map(|a| a.iter().map(|l| str_qq(Some(l)).to_lowercase()).collect()).unwrap_or_default();
            let file_globs: Vec<&String> = globs.iter().filter(|g| !g.starts_with("kw:")).collect();
            let kws: Vec<String> = globs.iter().filter(|g| g.starts_with("kw:")).map(|g| g[3..].to_lowercase()).collect();
            let file_hit = norm.iter().any(|f| file_globs.iter().any(|g| glob_to_re(g).map_or(false, |re| re.is_match(f))) || langs.contains(&ext_of(f)));
            let kw_hit = kws.iter().any(|k| !k.is_empty() && intent.contains(k));
            if file_hit || kw_hit { matched.push(t); }
        }
        matched.sort_by(|x, y| {
            let wx = x.get("weight").and_then(|v| v.as_f64()).unwrap_or(1.0);
            let wy = y.get("weight").and_then(|v| v.as_f64()).unwrap_or(1.0);
            wy.partial_cmp(&wx).unwrap_or(std::cmp::Ordering::Equal)
        });
        return Ok(json!({ "ok": true, "matched": matched }));
    }

    Err(format!("risk-tag: op must be one of mint, list, match"))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn seed(c: &Connection) {
        c.execute_batch("CREATE TABLE tags (slug TEXT PRIMARY KEY, kind TEXT, description TEXT, applies_to_json TEXT, prevention_md TEXT, retryability TEXT, source_pattern_key TEXT, fix_commit TEXT, canary_test TEXT, weight INTEGER, status TEXT); \
          CREATE TABLE contracts (id INTEGER PRIMARY KEY, card_slug TEXT, files_touched_json TEXT);").unwrap();
    }
    #[test]
    fn mint_list_match() {
        let c = Connection::open_in_memory().unwrap(); seed(&c);
        let r = riskTag(&json!({ "op": "mint", "slug": "cohesive-file", "applies_to": { "globs": ["src/components/**"], "langs": ["jsx"] }, "prevention_md": "author whole", "retryability": "structural" }), &c).unwrap();
        assert_eq!(r, json!({ "ok": true, "slug": "cohesive-file" }));
        let listed = riskTag(&json!({ "op": "list" }), &c).unwrap();
        assert_eq!(listed["tags"].as_array().unwrap().len(), 1);
        assert_eq!(listed["tags"][0]["applies_to"]["globs"], json!(["src/components/**"]));
        // match by glob
        let m = riskTag(&json!({ "op": "match", "files": ["src/components/Button.jsx"] }), &c).unwrap();
        assert_eq!(m["matched"].as_array().unwrap().len(), 1);
        // match by lang ext
        assert_eq!(riskTag(&json!({ "op": "match", "files": ["other/x.jsx"] }), &c).unwrap()["matched"].as_array().unwrap().len(), 1);
        assert_eq!(riskTag(&json!({ "op": "match", "files": ["other/x.py"] }), &c).unwrap()["matched"].as_array().unwrap().len(), 0);
        // match by kw
        riskTag(&json!({ "op": "mint", "slug": "ls", "applies_to": { "globs": ["kw:localstorage"] }, "prevention_md": "x" }), &c).unwrap();
        assert_eq!(riskTag(&json!({ "op": "match", "files": [], "intent": "persist in localStorage" }), &c).unwrap()["matched"].as_array().unwrap().len(), 1);
    }
}
