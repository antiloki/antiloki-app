//! notebook-scope.rs — twin of skills/notebooks/notebook-scope.mjs (db READ lane). NAMES ARE
//! CONTRACT: `resolveScope` (named, reused by notebook-corpus) · `notebookScope` (default, the
//! FE scope-preview action). Resolves a notebook's scope (paths/folders/node_ids, optionally
//! expanded by deps via atlas-subgraph or dependents via blast-radius) into the in-scope atlas
//! nodes + a compact code-context digest. Best-effort: every expansion step is a JS try/catch —
//! a query failure (atlas not onboarded) silently degrades rather than aborting the whole call.
use rusqlite::Connection;
use serde_json::{json, Value};
use std::collections::HashSet;
use std::sync::OnceLock;
use regex::Regex;

const ORIGIN: &str = "atlas";

// JS truthiness.
fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}
// JS `String(v ?? '')`.
fn str_qq(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(),
        Some(o) => o.to_string(),
    }
}
// JS `${v}` template interpolation.
fn tmpl(v: Option<&Value>) -> String {
    match v {
        None => "undefined".into(),
        Some(Value::Null) => "null".into(),
        Some(x) => str_qq(Some(x)),
    }
}
// JS `String(v)` (no `??`) — used for `.map(String)` id coercion and Set membership keys.
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
// `.replace(/\s+/g, ' ')` — collapses whitespace runs but does NOT trim (unlike grounding.mjs's
// helper): notebook-scope only collapses before `.slice(0, 120)`, so a leading/trailing run
// survives as a single space.
fn ws_collapse(s: &str) -> String {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\s+").unwrap()).replace_all(s, " ").to_string()
}
// `const isFile = (p) => /\.\w{1,8}$/.test(p)`.
fn is_file(p: &str) -> bool {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\.\w{1,8}$").unwrap()).is_match(p)
}
// Set-membership key for a node row's `id` field (mirrors `n.id` used directly in a JS Set —
// ids are always TEXT in code_nodes, so a String key is exact).
fn id_key(n: &Value) -> String { js_string(n.get("id").unwrap_or(&Value::Null)) }

// `function fetchByIds(db, ids, origin)`.
fn fetch_by_ids(db: &Connection, ids: &[String], origin: &Value) -> Result<Vec<Value>, String> {
    if ids.is_empty() { return Ok(vec![]); }
    let mut seen: HashSet<String> = HashSet::new();
    let mut uniq: Vec<String> = Vec::new();
    for id in ids { if seen.insert(id.clone()) { uniq.push(id.clone()); } }
    let ph = uniq.iter().map(|_| "?").collect::<Vec<_>>().join(",");
    let mut args: Vec<Value> = vec![origin.clone()];
    args.extend(uniq.iter().map(|s| json!(s)));
    let sql = format!("SELECT id, path, type, name, intent FROM code_nodes WHERE origin = ? AND id IN ({ph}) ORDER BY path LIMIT 400");
    let rows = crate::db_json::query_rows(db, &sql, &args)?;
    Ok(rows.as_array().cloned().unwrap_or_default())
}

// `function baseNodes(db, s, origin)`.
fn base_nodes(db: &Connection, s: &Value, origin: &Value) -> Result<Vec<Value>, String> {
    let ids: Vec<String> = s.get("node_ids").and_then(|v| v.as_array())
        .map(|a| a.iter().map(js_string).collect())
        .unwrap_or_default();
    if !ids.is_empty() { return fetch_by_ids(db, &ids, origin); }

    let paths: Vec<String> = s.get("paths").and_then(|v| v.as_array())
        .map(|a| a.iter().map(|p| {
            let raw = js_string(p).replace('\\', "/");
            let stripped = raw.strip_prefix("./").map(|r| r.to_string()).unwrap_or(raw);
            stripped.trim().to_string()
        }).filter(|x| !x.is_empty()).collect())
        .unwrap_or_default();
    if paths.is_empty() { return Ok(vec![]); }

    let exact: Vec<&String> = paths.iter().filter(|p| is_file(p)).collect();
    let folders: Vec<&String> = paths.iter().filter(|p| !is_file(p)).collect();
    let mut parts: Vec<String> = Vec::new();
    let mut args: Vec<Value> = vec![origin.clone()];
    if !exact.is_empty() {
        let ph = exact.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        parts.push(format!("path IN ({ph})"));
        for e in &exact { args.push(json!(e.as_str())); }
    }
    for f in &folders {
        parts.push("path LIKE ?".to_string());
        args.push(json!(format!("{}/%", f.trim_end_matches('/'))));
    }
    if parts.is_empty() { return Ok(vec![]); }
    let sql = format!(
        "SELECT id, path, type, name, intent FROM code_nodes WHERE origin = ? AND ({}) ORDER BY path LIMIT 400",
        parts.join(" OR ")
    );
    let rows = crate::db_json::query_rows(db, &sql, &args)?;
    Ok(rows.as_array().cloned().unwrap_or_default())
}

/// `export async function resolveScope(scope, context)` — reused by notebook-corpus. Every
/// expansion step below is a JS try/catch (`if let Ok(...)` here): a failure leaves `nodes`
/// exactly as it was before that step, never propagates.
pub fn resolveScope(input: &Value, db: &Connection) -> Result<Value, String> {
    let s = input;
    let origin: Value = if truthy(s.get("origin")) { s.get("origin").cloned().unwrap() } else { json!(ORIGIN) };

    let mut nodes: Vec<Value> = base_nodes(db, s, &origin).unwrap_or_default();

    if truthy(s.get("deps")) && !nodes.is_empty() {
        let node_ids: Vec<Value> = nodes.iter().map(|n| n.get("id").cloned().unwrap_or(Value::Null)).collect();
        let sub_input = json!({ "node_ids": node_ids, "hops": 1, "origin": origin.clone() });
        if let Ok(sub) = crate::codebase_atlas_subgraph::atlasSubgraph(&sub_input, db) {
            let mut have: HashSet<String> = nodes.iter().map(id_key).collect();
            if let Some(sub_nodes) = sub.get("nodes").and_then(|v| v.as_array()) {
                for n in sub_nodes {
                    let k = id_key(n);
                    if !have.contains(&k) { nodes.push(n.clone()); have.insert(k); }
                }
            }
        }
    }

    if truthy(s.get("dependents")) && !nodes.is_empty() {
        let node_ids: Vec<Value> = nodes.iter().map(|n| n.get("id").cloned().unwrap_or(Value::Null)).collect();
        let r_input = json!({ "node_ids": node_ids });
        if let Ok(r) = crate::codebase_blast_radius::blastRadius(&r_input, db) {
            let have: HashSet<String> = nodes.iter().map(id_key).collect();
            let fresh: Vec<String> = r.get("affected").and_then(|v| v.as_array()).cloned().unwrap_or_default()
                .iter().map(js_string).filter(|x| !have.contains(x)).take(200).collect();
            if let Ok(fetched) = fetch_by_ids(db, &fresh, &origin) {
                let mut have2 = have;
                for n in fetched {
                    let k = id_key(&n);
                    if !have2.contains(&k) { nodes.push(n); have2.insert(k); }
                }
            }
        }
    }

    nodes.truncate(400);

    let mut seen_paths: HashSet<String> = HashSet::new();
    let mut paths: Vec<Value> = Vec::new();
    for n in &nodes {
        let p = n.get("path").cloned().unwrap_or(Value::Null);
        if seen_paths.insert(js_string(&p)) { paths.push(p); }
    }

    let code_context_md = if nodes.is_empty() {
        String::new()
    } else {
        nodes.iter().take(120).map(|n| {
            let path = tmpl(n.get("path"));
            let name = n.get("name");
            let name_part = if truthy(name) { format!("  \u{b7}  {}", tmpl(name)) } else { String::new() };
            let typ = tmpl(n.get("type"));
            let intent = n.get("intent");
            let intent_part = if truthy(intent) {
                let collapsed = ws_collapse(&js_string(intent.unwrap()));
                let capped: String = collapsed.chars().take(120).collect();
                format!(" \u{2014} {capped}")
            } else { String::new() };
            format!("- {path}{name_part} ({typ}){intent_part}")
        }).collect::<Vec<_>>().join("\n")
    };

    let dbs = s.get("dbs").filter(|v| v.is_array()).cloned().unwrap_or_else(|| json!([]));

    Ok(json!({ "ok": true, "nodes": nodes, "paths": paths, "code_context_md": code_context_md, "dbs": dbs }))
}

/// `export default async function notebookScope(input, context)` — the FE scope-preview action.
pub fn notebookScope(input: &Value, db: &Connection) -> Result<Value, String> {
    crate::notebooks_notebook_schema::ensureNotebookSchema(db, &json!("nb."));
    let mut scope: Option<Value> = input.get("scope").cloned();
    if !truthy(scope.as_ref()) && truthy(input.get("id")) {
        let nb = crate::notebooks_notebook_store::getNotebook(db, input.get("id").unwrap());
        scope = nb.get("scope").cloned();
    }
    let final_scope = match &scope {
        Some(v) if truthy(Some(v)) => v.clone(),
        _ => json!({}),
    };
    resolveScope(&final_scope, db)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE code_nodes (id TEXT, origin TEXT, path TEXT, type TEXT, name TEXT, signature TEXT, intent TEXT, parent_id TEXT);
             CREATE TABLE code_edges (origin TEXT, from_node TEXT, to_node TEXT, type TEXT, kind TEXT);",
        ).unwrap();
        let rows: [(&str, &str, &str, &str, Value, Value); 8] = [
            ("a@", "atlas", "a.ts", "file", json!("a"), Value::Null),
            ("af", "atlas", "a.ts", "function", json!("f"), json!("   multi   space   text   ")),
            ("b@", "atlas", "b.ts", "file", Value::Null, Value::Null),
            ("c@", "atlas", "folder/c.ts", "file", json!("c"), Value::Null),
            ("d@", "atlas", "folder/sub/d.ts", "file", json!("d"), Value::Null),
            ("e@", "atlas", "e.ts", "file", json!("e"), Value::Null),
            ("g@", "atlas", "g.ts", "file", json!("g"), Value::Null),
            ("x@", "other", "a.ts", "file", json!("x"), Value::Null),
        ];
        for (id, origin, path, typ, name, intent) in rows {
            crate::db_json::run_write(
                &c,
                "INSERT INTO code_nodes (id,origin,path,type,name,signature,intent,parent_id) VALUES (?,?,?,?,?,NULL,?,NULL)",
                &[json!(id), json!(origin), json!(path), json!(typ), name, intent],
            ).unwrap();
        }
        crate::db_json::run_write(&c, "INSERT INTO code_edges (origin,from_node,to_node,type) VALUES ('atlas','a@','e@','imports')", &[]).unwrap();
        crate::db_json::run_write(&c, "INSERT INTO code_edges (origin,from_node,to_node,type) VALUES ('atlas','g@','a@','imports')", &[]).unwrap();
        c
    }

    #[test]
    fn exact_file_paths() {
        let c = fresh();
        let r = resolveScope(&json!({ "paths": ["a.ts"] }), &c).unwrap();
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["nodes"].as_array().unwrap().len(), 2);
        assert_eq!(r["nodes"][0]["id"], json!("a@"));
        assert_eq!(r["nodes"][1]["id"], json!("af"));
        assert_eq!(r["paths"], json!(["a.ts"]));
        assert_eq!(
            r["code_context_md"],
            json!("- a.ts  \u{b7}  a (file)\n- a.ts  \u{b7}  f (function) \u{2014}  multi space text ")
        );
        assert_eq!(r["dbs"], json!([]));
    }

    #[test]
    fn folder_paths() {
        let c = fresh();
        let r = resolveScope(&json!({ "paths": ["folder"] }), &c).unwrap();
        let ids: Vec<Value> = r["nodes"].as_array().unwrap().iter().map(|n| n["id"].clone()).collect();
        assert_eq!(ids, vec![json!("c@"), json!("d@")]);
        assert_eq!(r["paths"], json!(["folder/c.ts", "folder/sub/d.ts"]));
    }

    #[test]
    fn node_ids_lookup() {
        let c = fresh();
        let r = resolveScope(&json!({ "node_ids": ["b@", "e@"] }), &c).unwrap();
        let ids: Vec<Value> = r["nodes"].as_array().unwrap().iter().map(|n| n["id"].clone()).collect();
        assert_eq!(ids, vec![json!("b@"), json!("e@")]); // ordered by path (b.ts < e.ts)
        assert_eq!(r["code_context_md"], json!("- b.ts (file)\n- e.ts  \u{b7}  e (file)")); // no name for b@
    }

    #[test]
    fn empty_scope_degrades_to_empty() {
        let c = fresh();
        let r = resolveScope(&json!({}), &c).unwrap();
        assert_eq!(r, json!({ "ok": true, "nodes": [], "paths": [], "code_context_md": "", "dbs": [] }));
    }

    #[test]
    fn deps_expands_bidirectionally_via_atlas_subgraph() {
        let c = fresh();
        // atlas-subgraph hops BOTH directions from a@/af — picks up e@ (a@->e@) AND g@ (g@->a@).
        let r = resolveScope(&json!({ "paths": ["a.ts"], "deps": true }), &c).unwrap();
        let ids: Vec<Value> = r["nodes"].as_array().unwrap().iter().map(|n| n["id"].clone()).collect();
        assert_eq!(ids, vec![json!("a@"), json!("af"), json!("e@"), json!("g@")]);
        assert_eq!(r["paths"], json!(["a.ts", "e.ts", "g.ts"]));
    }

    #[test]
    fn dependents_expands_via_blast_radius_reverse_closure() {
        let c = fresh();
        let r = resolveScope(&json!({ "paths": ["a.ts"], "dependents": true }), &c).unwrap();
        let ids: Vec<Value> = r["nodes"].as_array().unwrap().iter().map(|n| n["id"].clone()).collect();
        assert_eq!(ids, vec![json!("a@"), json!("af"), json!("g@")]); // g@ depends on a@; e@ NOT included (deps false)
    }

    #[test]
    fn origin_filters_the_base_query() {
        let c = fresh();
        let r = resolveScope(&json!({ "paths": ["a.ts"], "origin": "other" }), &c).unwrap();
        let ids: Vec<Value> = r["nodes"].as_array().unwrap().iter().map(|n| n["id"].clone()).collect();
        assert_eq!(ids, vec![json!("x@")]); // only the origin='other' row at a.ts
    }

    #[test]
    fn notebook_scope_default_uses_input_scope_or_notebook_by_id() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS nb;").unwrap();
        c.execute_batch(
            "CREATE TABLE code_nodes (id TEXT, origin TEXT, path TEXT, type TEXT, name TEXT, signature TEXT, intent TEXT, parent_id TEXT);
             CREATE TABLE code_edges (origin TEXT, from_node TEXT, to_node TEXT, type TEXT, kind TEXT);",
        ).unwrap();
        crate::db_json::run_write(&c, "INSERT INTO code_nodes (id,origin,path,type,name) VALUES ('a@','atlas','a.ts','file','a')", &[]).unwrap();

        let a = notebookScope(&json!({ "scope": { "paths": ["a.ts"] } }), &c).unwrap();
        assert_eq!(a["nodes"].as_array().unwrap().len(), 1);

        crate::db_json::run_write(
            &c,
            "INSERT INTO nb.notebooks (id, slug, title, scope_json) VALUES (1, 's1', 'T1', ?)",
            &[json!(serde_json::to_string(&json!({ "paths": ["a.ts"] })).unwrap())],
        ).unwrap();
        let b = notebookScope(&json!({ "id": 1 }), &c).unwrap();
        assert_eq!(b["nodes"].as_array().unwrap().len(), 1);

        let no_id = notebookScope(&json!({}), &c).unwrap();
        assert_eq!(no_id, json!({ "ok": true, "nodes": [], "paths": [], "code_context_md": "", "dbs": [] }));

        let missing_id = notebookScope(&json!({ "id": 999 }), &c).unwrap();
        assert_eq!(missing_id, json!({ "ok": true, "nodes": [], "paths": [], "code_context_md": "", "dbs": [] }));
    }
}
