//! code-describe-gather.rs — twin of skills/codebase/code-describe-gather.mjs (db lane). NAMES ARE
//! CONTRACT: `codeDescribeGather` (default) + `gatherFacts` (named, db-first helper). STEP 1 of the
//! per-file description: aggregate atlas facts (symbols/imports/dependents/role) → facts_json UPSERT.
// QUALITY: 82/100 — decorator-or-node symbols, import/dependent resolve, symbol-role vote else path
// heuristic; persist ON CONFLICT with datetime('now') frozen to 2026-01-01, content-change essay invalidate
use regex::Regex;
use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::collections::HashMap;
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
fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) }
// JS String(v)
fn js_string(v: &Value) -> String {
    match v {
        Value::Null => "null".into(),
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Array(a) => a.iter().map(js_string).collect::<Vec<_>>().join(","),
        Value::Object(_) => "[object Object]".into(),
    }
}

const ROLE_BY_KIND: &[(&str, &str)] = &[
    ("component", "ui"), ("hook", "state"), ("store", "state"), ("class", "logic"),
    ("type", "data"), ("const", "data"), ("function", "logic"),
];
fn role_by_kind(kind: &str) -> Option<&'static str> {
    ROLE_BY_KIND.iter().find(|(k, _)| *k == kind).map(|(_, v)| *v)
}

fn role_of_path(p: &Value) -> &'static str {
    static UI: OnceLock<Regex> = OnceLock::new();
    static UI_EXT: OnceLock<Regex> = OnceLock::new();
    static STATE: OnceLock<Regex> = OnceLock::new();
    static DATA: OnceLock<Regex> = OnceLock::new();
    static ROUTE: OnceLock<Regex> = OnceLock::new();
    let ui = UI.get_or_init(|| Regex::new(r"(^|/)(components?|ui|views?|pages?|screens?)(/|$)").unwrap());
    let ui_ext = UI_EXT.get_or_init(|| Regex::new(r"\.(jsx|tsx|vue|svelte)$").unwrap());
    let state = STATE.get_or_init(|| Regex::new(r"(^|/)(stores?|state|redux|context|reducers?|atoms?|hooks?)(/|$)").unwrap());
    let data = DATA.get_or_init(|| Regex::new(r"(^|/)(models?|schemas?|entities|db|data|migrations?)(/|$)").unwrap());
    let route = ROUTE.get_or_init(|| Regex::new(r"(^|/)(routes?|controllers?|middlewares?|api|apis)(/|$)").unwrap());
    let s = js_string(p).replace('\\', "/").to_lowercase();
    if ui.is_match(&s) || ui_ext.is_match(&s) { return "ui"; }
    if state.is_match(&s) { return "state"; }
    if data.is_match(&s) { return "data"; }
    if route.is_match(&s) { return "route"; }
    "logic"
}

// The deterministic facts for ONE file from the live atlas tables. Pure read. (db-first helper)
pub fn gatherFacts(db: &Connection, origin: &Value, path: &Value) -> Value {
    // fileNode?.content_hash ?? null
    let file_row = crate::db_json::query_rows(
        db, "SELECT content_hash FROM code_nodes WHERE origin = ? AND path = ? AND type = 'file' LIMIT 1",
        &[origin.clone(), path.clone()],
    ).ok().and_then(|r| r.as_array().and_then(|a| a.first()).cloned());
    let content_hash = match &file_row {
        Some(row) => nz(row.get("content_hash")).cloned().unwrap_or(Value::Null),
        None => Value::Null,
    };

    // lang: String(path).match(/\.([a-z0-9]+)$/i)?.[1].toLowerCase() ?? null
    static EXT: OnceLock<Regex> = OnceLock::new();
    let ext = EXT.get_or_init(|| Regex::new(r"(?i)\.([a-z0-9]+)$").unwrap());
    let ps = js_string(path);
    let lang = match ext.captures(&ps) {
        Some(c) => json!(c[1].to_lowercase()),
        None => Value::Null,
    };

    // symbols: prefer the richer decorators; else the raw nodes. The decorators query is wrapped in a
    // try/catch in JS (missing table → []); .ok()...unwrap_or_default mirrors that exactly.
    let dec_rows = crate::db_json::query_rows(
        db, "SELECT name, kind, role, signature, summary FROM code_decorators WHERE origin = ? AND path = ? ORDER BY line",
        &[origin.clone(), path.clone()],
    ).ok().and_then(|r| r.as_array().cloned()).unwrap_or_default();
    let mut symbols: Vec<Value> = dec_rows.iter().map(|r| {
        let mut o = Map::new();
        o.insert("name".into(), r.get("name").cloned().unwrap_or(Value::Null));
        o.insert("kind".into(), r.get("kind").cloned().unwrap_or(Value::Null));
        o.insert("role".into(), r.get("role").cloned().unwrap_or(Value::Null));
        o.insert("signature".into(), r.get("signature").cloned().unwrap_or(Value::Null));
        o.insert("summary".into(), r.get("summary").cloned().unwrap_or(Value::Null));
        Value::Object(o)
    }).collect();
    if symbols.is_empty() {
        let node_rows = crate::db_json::query_rows(
            db, "SELECT name, type AS kind, signature FROM code_nodes WHERE origin = ? AND path = ? AND type != 'file' ORDER BY id",
            &[origin.clone(), path.clone()],
        ).ok().and_then(|r| r.as_array().cloned()).unwrap_or_default();
        symbols = node_rows.iter().map(|r| {
            let kind = r.get("kind").cloned().unwrap_or(Value::Null);
            let role = match kind.as_str().and_then(role_by_kind) { Some(rr) => json!(rr), None => json!("logic") };
            let mut o = Map::new();
            o.insert("name".into(), r.get("name").cloned().unwrap_or(Value::Null));
            o.insert("kind".into(), kind);
            o.insert("role".into(), role);
            o.insert("signature".into(), r.get("signature").cloned().unwrap_or(Value::Null));
            o.insert("summary".into(), Value::Null);
            Value::Object(o)
        }).collect();
    }

    // imports (this file → others) + dependents (others → this file), resolved node-id → path.
    let imports_rows = crate::db_json::query_rows(
        db, "SELECT DISTINCT tn.path AS p FROM code_edges e JOIN code_nodes fn ON e.from_node = fn.id JOIN code_nodes tn ON e.to_node = tn.id \
             WHERE e.origin = ? AND e.type = 'imports' AND fn.path = ? AND tn.path != ?",
        &[origin.clone(), path.clone(), path.clone()],
    ).ok().and_then(|r| r.as_array().cloned()).unwrap_or_default();
    let dependent_rows = crate::db_json::query_rows(
        db, "SELECT DISTINCT fn.path AS p FROM code_edges e JOIN code_nodes fn ON e.from_node = fn.id JOIN code_nodes tn ON e.to_node = tn.id \
             WHERE e.origin = ? AND e.type = 'imports' AND tn.path = ? AND fn.path != ?",
        &[origin.clone(), path.clone(), path.clone()],
    ).ok().and_then(|r| r.as_array().cloned()).unwrap_or_default();
    // .map(r => r.p).filter(Boolean)
    let imports: Vec<Value> = imports_rows.iter().map(|r| r.get("p").cloned().unwrap_or(Value::Null)).filter(|p| truthy(Some(p))).collect();
    let dependents: Vec<Value> = dependent_rows.iter().map(|r| r.get("p").cloned().unwrap_or(Value::Null)).filter(|p| truthy(Some(p))).collect();

    // primary role: the most common symbol role (ties → first-seen), else the path heuristic.
    let mut role_keys: Vec<String> = Vec::new();
    let mut role_counts: HashMap<String, i64> = HashMap::new();
    for s in &symbols {
        let role_v = s.get("role");
        if truthy(role_v) {
            let key = js_string(role_v.unwrap());
            if !role_counts.contains_key(&key) { role_keys.push(key.clone()); }
            *role_counts.entry(key).or_insert(0) += 1;
        }
    }
    let mut entries: Vec<(String, i64)> = role_keys.iter().map(|k| (k.clone(), role_counts[k])).collect();
    entries.sort_by(|a, b| b.1.cmp(&a.1)); // b[1]-a[1] descending, stable → ties keep insertion order
    let role: Value = match entries.first() {
        Some((k, _)) => json!(k),
        None => json!(role_of_path(path)),
    };

    let mut facts = Map::new();
    facts.insert("path".into(), path.clone());
    facts.insert("origin".into(), origin.clone());
    facts.insert("lang".into(), lang);
    facts.insert("role".into(), role);
    facts.insert("content_hash".into(), content_hash);
    facts.insert("symbols".into(), json!(symbols));
    facts.insert("imports".into(), json!(imports));
    facts.insert("dependents".into(), json!(dependents));
    facts.insert("n_symbols".into(), json!(symbols.len() as i64));
    facts.insert("n_imports".into(), json!(imports.len() as i64));
    facts.insert("n_dependents".into(), json!(dependents.len() as i64));
    Value::Object(facts)
}

fn persist(db: &Connection, origin: &Value, path: &Value, facts: &Value) -> Result<(), String> {
    let facts_json = serde_json::to_string(facts).map_err(|e| e.to_string())?;
    let content_hash = facts.get("content_hash").cloned().unwrap_or(Value::Null);
    // JS: gathered_at = datetime('now') (×2) — frozen-clock lane pins 'now' to the instant below.
    crate::db_json::run_write(
        db,
        &format!("INSERT INTO code_descriptions (origin, path, facts_json, content_hash, gathered_at) \
         VALUES (?, ?, ?, ?, {}) \
         ON CONFLICT(origin, path) DO UPDATE SET \
           facts_json = excluded.facts_json, \
           gathered_at = {}, \
           summary_md = CASE WHEN IFNULL(code_descriptions.content_hash, '') != IFNULL(excluded.content_hash, '') THEN NULL ELSE code_descriptions.summary_md END, \
           generated_at = CASE WHEN IFNULL(code_descriptions.content_hash, '') != IFNULL(excluded.content_hash, '') THEN NULL ELSE code_descriptions.generated_at END, \
           content_hash = excluded.content_hash", crate::clock::now_sql(), crate::clock::now_sql()),
        &[origin.clone(), path.clone(), json!(facts_json), content_hash],
    )?;
    Ok(())
}

pub fn codeDescribeGather(input: &Value, db: &Connection) -> Result<Value, String> {
    let origin = nz(input.get("origin")).cloned().unwrap_or(json!("atlas"));

    if truthy(input.get("all")) {
        let files = crate::db_json::query_rows(db, "SELECT DISTINCT path FROM code_nodes WHERE origin = ? AND type = 'file'", &[origin.clone()])?;
        let paths: Vec<Value> = files.as_array().cloned().unwrap_or_default().iter()
            .map(|r| r.get("path").cloned().unwrap_or(Value::Null)).collect();
        for path in &paths {
            let facts = gatherFacts(db, &origin, path);
            persist(db, &origin, path, &facts)?;
        }
        return Ok(json!({ "gathered": paths.len() as i64, "origin": origin }));
    }

    let path = input.get("path");
    if !truthy(path) { return Err("code-describe-gather: path is required".into()); }
    let path = path.unwrap().clone();
    let facts = gatherFacts(db, &origin, &path);
    persist(db, &origin, &path, &facts)?;
    let content_hash = facts.get("content_hash").cloned().unwrap_or(Value::Null);
    Ok(json!({ "path": path, "origin": origin, "facts": facts, "content_hash": content_hash }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seeded() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE code_nodes (id TEXT, origin TEXT, name TEXT, path TEXT, type TEXT, signature TEXT, content_hash TEXT); \
             CREATE TABLE code_edges (origin TEXT, from_node TEXT, to_node TEXT, type TEXT, kind TEXT); \
             CREATE TABLE code_decorators (origin TEXT, path TEXT, name TEXT, kind TEXT, role TEXT, signature TEXT, summary TEXT, line INTEGER); \
             CREATE TABLE code_descriptions (origin TEXT, path TEXT, facts_json TEXT, content_hash TEXT, summary_md TEXT, generated_at TEXT, gathered_at TEXT, PRIMARY KEY (origin, path)); \
             INSERT INTO code_nodes (id,origin,name,path,type,content_hash) VALUES \
               ('f1','atlas','Widget','src/components/Widget.jsx','file','hashW'), \
               ('f2','atlas','util','src/lib/util.js','file','hashU'), \
               ('f3','atlas','Home','src/pages/Home.jsx','file','hashH'); \
             INSERT INTO code_decorators (origin,path,name,kind,role,signature,summary,line) VALUES \
               ('atlas','src/components/Widget.jsx','Widget','component','ui','() => JSX','renders',1), \
               ('atlas','src/components/Widget.jsx','useThing','hook','state','()','the hook',2), \
               ('atlas','src/components/Widget.jsx','helper','function','ui','(x)',NULL,3); \
             INSERT INTO code_edges (origin,from_node,to_node,type) VALUES \
               ('atlas','f1','f2','imports'), \
               ('atlas','f3','f1','imports');",
        ).unwrap();
        c
    }

    #[test]
    fn gathers_decorator_symbols_imports_and_role() {
        let c = seeded();
        let f = gatherFacts(&c, &json!("atlas"), &json!("src/components/Widget.jsx"));
        assert_eq!(f["lang"], json!("jsx"));
        assert_eq!(f["role"], json!("ui")); // ui:2 beats state:1
        assert_eq!(f["content_hash"], json!("hashW"));
        assert_eq!(f["n_symbols"], json!(3));
        assert_eq!(f["symbols"][0], json!({ "name": "Widget", "kind": "component", "role": "ui", "signature": "() => JSX", "summary": "renders" }));
        assert_eq!(f["symbols"][2]["summary"], Value::Null);
        assert_eq!(f["imports"], json!(["src/lib/util.js"]));
        assert_eq!(f["dependents"], json!(["src/pages/Home.jsx"]));
    }

    #[test]
    fn falls_back_to_nodes_then_path_heuristic() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE code_nodes (id TEXT, origin TEXT, name TEXT, path TEXT, type TEXT, signature TEXT, content_hash TEXT); \
             CREATE TABLE code_edges (origin TEXT, from_node TEXT, to_node TEXT, type TEXT, kind TEXT); \
             CREATE TABLE code_decorators (origin TEXT, path TEXT, name TEXT, kind TEXT, role TEXT, signature TEXT, summary TEXT, line INTEGER); \
             INSERT INTO code_nodes (id,origin,name,path,type,signature,content_hash) VALUES \
               ('g1','atlas','models','src/db/models.ts','file',NULL,'hashM'), \
               ('g2','atlas','User','src/db/models.ts','class','class User',NULL), \
               ('g3','atlas','save','src/db/models.ts','function','save()',NULL);",
        ).unwrap();
        // node fallback: role voted from symbols (logic:2), NOT the path heuristic (which would be 'data')
        let f = gatherFacts(&c, &json!("atlas"), &json!("src/db/models.ts"));
        assert_eq!(f["role"], json!("logic"));
        assert_eq!(f["symbols"][0], json!({ "name": "User", "kind": "class", "role": "logic", "signature": "class User", "summary": Value::Null }));
        assert_eq!(f["n_symbols"], json!(2));
        // no symbols at all → path heuristic; missing file node → content_hash null
        let e = gatherFacts(&c, &json!("atlas"), &json!("src/api/routes.js"));
        assert_eq!(e["role"], json!("route"));
        assert_eq!(e["content_hash"], Value::Null);
        assert_eq!(e["symbols"], json!([]));
        assert_eq!(e["lang"], json!("js"));
        // no extension → lang null; roleOfPath default 'logic'
        let m = gatherFacts(&c, &json!("atlas"), &json!("Makefile"));
        assert_eq!(m["lang"], Value::Null);
        assert_eq!(m["role"], json!("logic"));
    }

    #[test]
    fn default_persists_and_returns() {
        let c = seeded();
        let r = codeDescribeGather(&json!({ "path": "src/components/Widget.jsx" }), &c).unwrap();
        assert_eq!(r["path"], json!("src/components/Widget.jsx"));
        assert_eq!(r["origin"], json!("atlas"));
        assert_eq!(r["content_hash"], json!("hashW"));
        assert_eq!(r["facts"]["role"], json!("ui"));
        let gathered_at: String = c.query_row("SELECT gathered_at FROM code_descriptions WHERE path='src/components/Widget.jsx'", [], |r| r.get(0)).unwrap();
        assert_eq!(gathered_at, "2026-01-01 00:00:00");
        let facts_json: String = c.query_row("SELECT facts_json FROM code_descriptions WHERE path='src/components/Widget.jsx'", [], |r| r.get(0)).unwrap();
        assert_eq!(facts_json, serde_json::to_string(&r["facts"]).unwrap());
    }

    #[test]
    fn upsert_invalidates_essay_on_content_change() {
        let c = seeded();
        c.execute(
            "INSERT INTO code_descriptions (origin,path,facts_json,content_hash,summary_md,generated_at,gathered_at) \
             VALUES ('atlas','src/components/Widget.jsx','{\"old\":1}','stale','cached essay','2025-01-01 00:00:00','2025-01-01 00:00:00')",
            [],
        ).unwrap();
        codeDescribeGather(&json!({ "path": "src/components/Widget.jsx", "origin": "atlas" }), &c).unwrap();
        let (summary, hash): (Option<String>, String) = c.query_row(
            "SELECT summary_md, content_hash FROM code_descriptions WHERE path='src/components/Widget.jsx'", [], |r| Ok((r.get(0)?, r.get(1)?)),
        ).unwrap();
        assert_eq!(summary, None); // stale != hashW → essay nulled
        assert_eq!(hash, "hashW");
    }

    #[test]
    fn all_branch_gathers_every_file() {
        let c = seeded();
        let r = codeDescribeGather(&json!({ "all": true }), &c).unwrap();
        assert_eq!(r["gathered"], json!(3));
        assert_eq!(r["origin"], json!("atlas"));
        let n: i64 = c.query_row("SELECT COUNT(*) FROM code_descriptions", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 3);
    }
}
