//! codebase-context.rs — twin of skills/codebase/codebase-context.mjs (db lane). NAMES ARE CONTRACT:
//! `codebaseContext` (named default). Wave G grounding pack: atlas slice → AI-sized markdown.
// QUALITY: 80/100 — CALLS converted atlasSubgraph + symbolFind (cross-module), per-file md, blast radius
use rusqlite::Connection;
use serde_json::{json, Value};
use std::collections::HashSet;

fn empty(reason: &str) -> Value {
    json!({ "grounded": false, "context_md": "", "files": [], "file_count": 0, "neighbor_count": 0, "node_count": 0, "reason": reason })
}
fn js_str(v: &Value) -> String { match v { Value::String(s) => s.clone(), Value::Null => String::new(), o => o.to_string() } }
// dedupe preserving first-seen order
fn dedup(v: Vec<String>) -> Vec<String> {
    let mut seen = HashSet::new(); let mut out = Vec::new();
    for s in v { if seen.insert(s.clone()) { out.push(s); } }
    out
}

pub fn codebaseContext(input: &Value, db: &Connection) -> Result<Value, String> {
    let origin = input.get("origin").and_then(|v| v.as_str()).unwrap_or("atlas").to_string();
    let hops = input.get("hops").and_then(|v| v.as_f64()).filter(|f| f.is_finite()).unwrap_or(1.0);
    let max_files = (1.0_f64).max(input.get("max_files").and_then(|v| v.as_f64()).unwrap_or(40.0)) as usize;
    let max_symbols = (1.0_f64).max(input.get("max_symbols").and_then(|v| v.as_f64()).unwrap_or(30.0)) as i64;

    let mut paths: Vec<String> = input.get("paths").and_then(|v| v.as_array()).map(|a|
        a.iter().map(|p| js_str(p).replace('\\', "/")).filter(|s| !s.is_empty()).collect()).unwrap_or_default();
    // no explicit paths → resolve from card terms via symbolFind (screening grounding)
    if paths.is_empty() {
        if let Some(terms) = input.get("terms").and_then(|v| v.as_array()).filter(|a| !a.is_empty()) {
            let mut found: Vec<String> = Vec::new();
            let mut seen = HashSet::new();
            for t in terms.iter().take(8) {
                if let Ok(r) = crate::codebase_symbol_find::symbolFind(&json!({ "query": js_str(t), "limit": 5, "origin": origin }), db) {
                    for m in r.get("matches").and_then(|v| v.as_array()).unwrap_or(&vec![]) {
                        let p = js_str(m.get("node").and_then(|n| n.get("path")).unwrap_or(&Value::Null));
                        if seen.insert(p.clone()) { found.push(p); }
                    }
                }
            }
            paths = found.into_iter().take(max_files).collect();
        }
    }
    if paths.is_empty() { return Ok(empty("no target paths or terms matched the atlas")); }

    let sub = crate::codebase_atlas_subgraph::atlasSubgraph(&json!({ "paths": paths, "hops": hops, "origin": origin }), db)?;
    if sub.get("node_count").and_then(|v| v.as_i64()).unwrap_or(0) == 0 {
        return Ok(empty("atlas has no nodes for these targets (not ingested?)"));
    }
    let nodes = sub.get("nodes").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    let edges = sub.get("edges").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    let node_count = sub.get("node_count").and_then(|v| v.as_i64()).unwrap_or(0);
    let by_id: std::collections::HashMap<String, Value> = nodes.iter().map(|n| (js_str(n.get("id").unwrap_or(&Value::Null)), n.clone())).collect();
    let seed_set: HashSet<String> = sub.get("seeds").and_then(|v| v.as_array()).map(|a| a.iter().map(js_str).collect()).unwrap_or_default();
    let seeds: Vec<&Value> = nodes.iter().filter(|n| seed_set.contains(&js_str(n.get("id").unwrap_or(&Value::Null))) && n.get("type").and_then(|v| v.as_str()) == Some("file")).collect();

    let header = "## Existing code context (atlas — the real code these files live in)";
    let mut file_sections: Vec<Value> = Vec::new();
    let (mut shown, mut truncated) = (0usize, 0usize);
    for f in &seeds {
        if shown >= max_files { truncated = seeds.len() - shown; break; }
        shown += 1;
        let fid = js_str(f.get("id").unwrap_or(&Value::Null));
        let path = js_str(f.get("path").unwrap_or(&Value::Null));
        let edge_paths = |from_is_f: bool| -> Vec<String> {
            edges.iter().filter(|e| {
                let matches = if from_is_f { e.get("from_node").map(js_str) == Some(fid.clone()) } else { e.get("to_node").map(js_str) == Some(fid.clone()) };
                matches && e.get("type").and_then(|v| v.as_str()) == Some("imports")
            }).filter_map(|e| {
                let other = if from_is_f { js_str(e.get("to_node").unwrap_or(&Value::Null)) } else { js_str(e.get("from_node").unwrap_or(&Value::Null)) };
                by_id.get(&other).and_then(|n| n.get("path")).map(js_str).filter(|s| !s.is_empty())
            }).collect()
        };
        let imports = dedup(edge_paths(true));
        let imported_by = dedup(edge_paths(false));
        let children = crate::db_json::query_rows(db,
            "SELECT name, type, signature FROM code_nodes WHERE origin = ? AND parent_id = ? AND type <> 'file' ORDER BY rowid LIMIT ?",
            &[json!(origin), json!(fid), json!(max_symbols)])?;
        let child_strs: Vec<String> = children.as_array().unwrap().iter().map(|c| {
            let name = js_str(c.get("name").unwrap_or(&Value::Null));
            match c.get("signature").and_then(|v| v.as_str()).filter(|s| !s.is_empty()) {
                Some(sig) => format!("{name} `{sig}`"),
                None => format!("{name} ({})", js_str(c.get("type").unwrap_or(&Value::Null))),
            }
        }).collect();
        let mut fl: Vec<String> = vec![format!("### {path}")];
        fl.push(if !child_strs.is_empty() { format!("- defines: {}", child_strs.join(", ")) } else { "- defines: (no extracted symbols)".to_string() });
        if !imports.is_empty() { fl.push(format!("- imports: {}", imports.join(", "))); }
        if !imported_by.is_empty() { fl.push(format!("- imported by (blast radius — changing the signatures here affects these): {}", imported_by.join(", "))); }
        file_sections.push(json!({ "path": path, "md": fl.join("\n") }));
    }

    let mut parts: Vec<String> = vec![header.to_string()];
    parts.extend(file_sections.iter().map(|s| js_str(s.get("md").unwrap_or(&Value::Null))));
    if truncated > 0 { parts.push(format!("- … {truncated} more file(s) in scope (truncated)")); }
    Ok(json!({
        "grounded": true, "context_md": parts.join("\n\n"), "files": file_sections,
        "file_count": seeds.len(), "neighbor_count": node_count - seeds.len() as i64, "node_count": node_count,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn grounds_or_empty() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE code_nodes (id TEXT, origin TEXT, path TEXT, type TEXT, name TEXT, signature TEXT, intent TEXT, parent_id TEXT); CREATE TABLE code_edges (origin TEXT, from_node TEXT, to_node TEXT, type TEXT, kind TEXT);").unwrap();
        // greenfield → empty
        assert_eq!(codebaseContext(&json!({ "paths": ["nope.ts"] }), &c).unwrap()["grounded"], json!(false));
        c.execute_batch("INSERT INTO code_nodes (id,origin,path,type,name) VALUES ('a@','atlas','a.ts','file','a'),('af','atlas','a.ts','function','f'); UPDATE code_nodes SET parent_id='a@', signature='f(x)' WHERE id='af';").unwrap();
        let r = codebaseContext(&json!({ "paths": ["a.ts"] }), &c).unwrap();
        assert_eq!(r["grounded"], json!(true));
        assert!(r["context_md"].as_str().unwrap().contains("### a.ts"));
        assert!(r["context_md"].as_str().unwrap().contains("f `f(x)`"));
    }
}
