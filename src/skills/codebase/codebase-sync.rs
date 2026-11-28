//! codebase-sync.rs — twin of skills/codebase/codebase-sync.mjs, the anti-rot incremental pass:
//! hash every in-scope file, re-parse only the ones whose content_hash changed (plus brand-new
//! files), drop the ones that disappeared, and leave the untouched majority alone. `mode:'closure'`
//! additionally re-parses the reverse-deps of the changed/deleted files (the ripple fix);
//! `mode:'full'` re-parses everything. NAMES ARE CONTRACT: `codebaseSync` (default). `deletePaths`
//! and `owningFile` are transcribed as private fns (JS module-private helpers, not exported).
//!
//! BESPOKE-NATIVE (real fs reads of a real root + real sqlite reads/writes across code_nodes /
//! code_edges / code_descriptions / code_parse_health), not conform-db: same class as
//! atlas-ingest.rs (its whole-repo sibling) / terminal-sandbox.rs / auth-sign.rs. Proven below via
//! `#[cfg(test)]` against a real temp dir + an in-memory sqlite db seeded via the already-converted
//! `codebase_code_schema::ensureCodeSchema` twin.
//!
//! Cross-module calls (every one an already-converted twin, reused not reimplemented):
//! `codebase_atlas_ingest::{discover, hashSource, readProjectConfig, extractCsNamespaces,
//! degraded_detail}`, `codebase_ast_parse::astParse`, `codebase_dep_resolve::depResolve`,
//! `codebase_code_describe_gather::codeDescribeGather`, `codebase_symbol_table_build::symbolTableBuild`,
//! `codebase_binding_resolve::bindingResolve`, `codebase_flow_resolve::resolveFlow`, `db_json::{run_write,
//! query_rows}`.
//!
//! ## PARITY-DEFERRED / disclosed divergences
//! 1. **Unsupported-language re-parse.** `ast-parse.rs` only natively parses javascript / typescript /
//!    tsx / python (see that module's doc); a call for any other extension returns `Ok(Value::Null)`
//!    instead of JS's always-populated symbol/import/call extraction. This twin treats that exactly
//!    like atlas-ingest.rs does: the file degrades to a bare file node with a `status: "skipped"`
//!    health row (`detail: "language not yet ported to the native parser"`), not a crash.
//! 2. **Thrown-error reclassification is coarser** — same as atlas-ingest.rs (4): the JS source's
//!    `e.parser_engine` tag (a tree-sitter WASM engine crash, distinct from a genuine parse failure)
//!    has no Rust equivalent since `astParse` here returns a plain `Err(String)`; that branch is
//!    narrower already per (1), so every `Err` is recorded as a real `fail`. (Until 2026-10-01 a
//!    Flow-typed file was reclassified `skipped`; ast-parse.rs now parses it via flow-strip.rs and a
//!    Flow file oxc still cannot read is a `fail` with lang `flow`.)
//! 3. **Directory walk / discover order** inherits `discover`'s own disclosed sort-by-name
//!    approximation (see atlas-ingest.rs doc (6)) when `input.files` is not supplied.
//! Everything else — hash-diff classification (changed/new/deleted/unchanged), the `mode` reparse-set
//! expansion (incremental/closure/full), the edge preserve/recompute/replace stitch, and the
//! code_descriptions + code_parse_health best-effort refresh — is ported faithfully.
use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;

use crate::codebase_atlas_ingest::{discover, extractCsNamespaces, hashSource, readProjectConfig};

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

// The file a node id belongs to: 'path#symbol' -> 'path'; a file id is its own path.
fn owning_file(id: &str) -> &str {
    match id.find('#') { Some(h) => &id[..h], None => id }
}

// Remove a set of file paths' Blocks + Bindings from one origin: every node at that path
// (the file node + its children share path) and every edge touching those node ids.
fn delete_paths(db: &Connection, origin: &str, paths: &[String]) -> Result<(), String> {
    if paths.is_empty() { return Ok(()); }
    for p in paths {
        let ids = crate::db_json::query_rows(db, "SELECT id FROM code_nodes WHERE origin = ? AND path = ?", &[json!(origin), json!(p)])?;
        if let Some(arr) = ids.as_array() {
            for row in arr {
                if let Some(id) = row.get("id").and_then(Value::as_str) {
                    crate::db_json::run_write(db, "DELETE FROM code_edges WHERE origin = ? AND (from_node = ? OR to_node = ?)", &[json!(origin), json!(id), json!(id)])?;
                }
            }
        }
        crate::db_json::run_write(db, "DELETE FROM code_nodes WHERE origin = ? AND path = ?", &[json!(origin), json!(p)])?;
    }
    Ok(())
}

pub fn codebaseSync(input: &Value, db: &Connection) -> Result<Value, String> {
    let root_v = nz(input.get("root"));
    if root_v.is_none() { return Err("codebase-sync: root is required".into()); }
    let root = str_qq(root_v);
    let origin_v = nz(input.get("origin")).cloned().unwrap_or(json!("atlas"));
    let origin = str_qq(Some(&origin_v));
    let mode = nz(input.get("mode")).and_then(Value::as_str).map(|s| s.to_string()).unwrap_or_else(|| "incremental".to_string());

    // ── 1. current in-scope files (+ hashes), via the SAME discovery as a full ingest ──
    let include: Vec<String> = input.get("include").and_then(|v| v.as_array()).map(|a| a.iter().map(|x| str_qq(Some(x))).collect()).unwrap_or_default();
    let exclude: Vec<String> = input.get("exclude").and_then(|v| v.as_array()).map(|a| a.iter().map(|x| str_qq(Some(x))).collect()).unwrap_or_default();
    let current_files: Vec<String> = match input.get("files").and_then(|v| v.as_array()) {
        Some(arr) if !arr.is_empty() => arr.iter().map(|f| str_qq(Some(f)).replace('\\', "/")).collect(),
        _ => discover(&root, &include, &exclude),
    };

    let mut current_hash: HashMap<String, Option<String>> = HashMap::new();
    let mut all_sources: Vec<(String, String)> = Vec::new();
    let mut ns_index: Map<String, Value> = Map::new();
    for p in &current_files {
        match fs::read_to_string(Path::new(&root).join(p)) {
            Ok(src) => {
                current_hash.insert(p.clone(), Some(hashSource(&src)));
                if p.to_lowercase().ends_with(".cs") {
                    for ns in extractCsNamespaces(&src) {
                        ns_index.entry(ns).or_insert_with(|| json!([])).as_array_mut().unwrap().push(json!(p));
                    }
                }
                all_sources.push((p.clone(), src));
            }
            Err(_) => { current_hash.insert(p.clone(), None); }
        }
    }

    // ── 2. the stored atlas: one row per file node (id == path), with its last hash ──
    let stored_rows = crate::db_json::query_rows(db, "SELECT path, content_hash FROM code_nodes WHERE origin = ? AND type = 'file'", &[json!(origin)])?;
    let mut stored_hash: HashMap<String, Option<String>> = HashMap::new();
    if let Some(arr) = stored_rows.as_array() {
        for r in arr {
            let path = r.get("path").and_then(Value::as_str).unwrap_or("").to_string();
            let hash = r.get("content_hash").and_then(Value::as_str).map(|s| s.to_string());
            stored_hash.insert(path, hash);
        }
    }

    // ── 3. classify into changed / new / deleted / unchanged ──
    let mut changed: Vec<String> = Vec::new();
    let mut added: Vec<String> = Vec::new();
    let mut unchanged: Vec<String> = Vec::new();
    for p in &current_files {
        match stored_hash.get(p) {
            None => added.push(p.clone()),
            Some(sh) => { if sh != current_hash.get(p).unwrap() { changed.push(p.clone()); } else { unchanged.push(p.clone()); } }
        }
    }
    let current_set: HashSet<&String> = current_files.iter().collect();
    let deleted: Vec<String> = stored_hash.keys().filter(|p| !current_set.contains(p)).cloned().collect();

    // ── reparse set: changed + new, EXPANDED by `mode`. (computed from the STORED edges, before
    //    any deletion.) closure adds the reverse-deps of the changed/deleted files. ──
    let mut reparse_set: Vec<String> = changed.iter().chain(added.iter()).cloned().collect();
    let mut closure_added: i64 = 0;
    if mode == "full" {
        reparse_set = current_files.clone();
        closure_added = (current_files.len() as i64 - changed.len() as i64 - added.len() as i64).max(0);
    } else if mode == "closure" {
        let target_files: HashSet<String> = changed.iter().chain(deleted.iter()).cloned().collect();
        if !target_files.is_empty() {
            let current_set2: HashSet<String> = current_files.iter().cloned().collect();
            let in_set: HashSet<String> = reparse_set.iter().cloned().collect();
            let mut rev_deps: HashSet<String> = HashSet::new();
            let edge_rows = crate::db_json::query_rows(db, "SELECT from_node, to_node FROM code_edges WHERE origin = ?", &[json!(origin)])?;
            if let Some(arr) = edge_rows.as_array() {
                for e in arr {
                    let to_node = e.get("to_node").and_then(Value::as_str).unwrap_or("");
                    if !target_files.contains(owning_file(to_node)) { continue; }
                    let from_node = e.get("from_node").and_then(Value::as_str).unwrap_or("");
                    let from_file = owning_file(from_node).to_string();
                    if !target_files.contains(&from_file) && current_set2.contains(&from_file) && !in_set.contains(&from_file) { rev_deps.insert(from_file); }
                }
            }
            closure_added = rev_deps.len() as i64;
            reparse_set.extend(rev_deps);
        }
    }

    // ── 4. re-parse ONLY the changed/new files (resilient: a parse failure is recorded) ──
    let mut fresh_nodes: Vec<Value> = Vec::new();
    let mut fresh_imports: Vec<Value> = Vec::new();
    let mut fresh_calls: Vec<Value> = Vec::new();
    let mut errors: Vec<Value> = Vec::new();
    let mut health_by_path: HashMap<String, Value> = HashMap::new();
    let mut skipped_flow: HashSet<String> = HashSet::new();

    for rel_path in &reparse_set {
        // THE SAME SIZE CAP AS THE ATLAS LANE (audit 2026-10-01, F-06): a file past it is recorded as skipped, never read into the parser
        if let Ok(md) = fs::metadata(Path::new(&root).join(rel_path)) {
            if md.len() as i64 > crate::codebase_atlas_ingest::DEFAULT_MAX_FILE_BYTES {
                let why = format!("skipped: {}B > max_file_bytes {}", md.len(), crate::codebase_atlas_ingest::DEFAULT_MAX_FILE_BYTES);
                errors.push(json!({ "path": rel_path, "error": why }));
                health_by_path.insert(rel_path.clone(), json!({ "lang": null, "status": "fail", "detail": why }));
                fresh_nodes.push(json!({ "id": rel_path, "path": rel_path, "type": "file", "content_hash": current_hash.get(rel_path).cloned().flatten() }));
                continue;
            }
        }
        let source = match fs::read_to_string(Path::new(&root).join(rel_path)) {
            Ok(s) => s,
            Err(e) => {
                errors.push(json!({ "path": rel_path, "error": e.to_string() }));
                health_by_path.insert(rel_path.clone(), json!({ "lang": null, "status": "fail", "detail": e.to_string() }));
                fresh_nodes.push(json!({ "id": rel_path, "path": rel_path, "type": "file", "content_hash": current_hash.get(rel_path).cloned().flatten() }));
                continue;
            }
        };
        match crate::codebase_ast_parse::astParse(&json!({ "path": rel_path, "source": source }), db) {
            Ok(Value::Null) => {
                // A grammar with no walk profile — every ADAPTERS grammar has one today (see parse-treesitter.rs "Scope").
                skipped_flow.insert(rel_path.clone());
                health_by_path.insert(rel_path.clone(), json!({ "lang": null, "status": "skipped", "detail": "no walk profile for this grammar (see parse-treesitter.rs 'Scope')" }));
                let h = current_hash.get(rel_path).cloned().flatten().unwrap_or_else(|| hashSource(&source));
                fresh_nodes.push(json!({ "id": rel_path, "path": rel_path, "type": "file", "content_hash": h }));
            }
            Ok(parsed) => {
                if truthy(parsed.get("has_error")) {
                    let lang = parsed.get("lang").cloned().unwrap_or(Value::Null);
                    health_by_path.insert(rel_path.clone(), json!({ "lang": if truthy(parsed.get("flow")) { json!("flow") } else { lang }, "status": "degraded", "detail": crate::codebase_atlas_ingest::degraded_detail(&parsed) }));
                }
                let mut nodes: Vec<Value> = parsed.get("nodes").and_then(|v| v.as_array()).cloned().unwrap_or_default();
                let h = current_hash.get(rel_path).cloned().flatten().unwrap_or_else(|| hashSource(&source));
                if let Some(file_node) = nodes.iter_mut().find(|n| n.get("type").and_then(Value::as_str) == Some("file")) {
                    if let Some(obj) = file_node.as_object_mut() { obj.insert("content_hash".into(), json!(h)); }
                }
                if let Some(imports) = parsed.get("imports").and_then(|v| v.as_array()) {
                    for imp in imports { fresh_imports.push(json!({ "from_path": rel_path, "specifier": imp.get("specifier").cloned().unwrap_or(Value::Null), "kind": imp.get("kind").cloned().unwrap_or(json!("value")) })); }
                }
                if let Some(calls) = parsed.get("calls").and_then(|v| v.as_array()) {
                    for c in calls { fresh_calls.push(json!({ "from_path": rel_path, "caller": c.get("caller").cloned().unwrap_or(Value::Null), "callee": c.get("callee").cloned().unwrap_or(Value::Null), "via": c.get("via").cloned().unwrap_or(Value::Null) })); }
                }
                if let Some(refs) = parsed.get("refs").and_then(|v| v.as_array()) {
                    for c in refs { fresh_calls.push(json!({ "from_path": rel_path, "caller": c.get("caller").cloned().unwrap_or(Value::Null), "callee": c.get("callee").cloned().unwrap_or(Value::Null), "via": "ref" })); }
                }
                fresh_nodes.extend(nodes);
            }
            Err(e) => {
                errors.push(json!({ "path": rel_path, "error": e }));
                health_by_path.insert(rel_path.clone(), json!({ "lang": if e.starts_with("flow:") { json!("flow") } else { Value::Null }, "status": "fail", "detail": e }));
                let h = current_hash.get(rel_path).cloned().flatten();
                fresh_nodes.push(json!({ "id": rel_path, "path": rel_path, "type": "file", "content_hash": h }));
            }
        }
    }
    // A file that failed to parse (or was Flow-skipped) keeps only its bare file node: drop both
    // from the reparsed set so neither counts as a successful re-parse.
    let failed: HashSet<String> = errors.iter().filter_map(|e| e.get("path").and_then(Value::as_str)).map(|s| s.to_string()).collect();
    let reparsed_ok: Vec<String> = reparse_set.iter().filter(|p| !failed.contains(*p) && !skipped_flow.contains(*p)).cloned().collect();

    // ── 5a. snapshot the UNTOUCHED files' edges BEFORE deleting anything. ──
    let recomputed_or_deleted: HashSet<String> = reparse_set.iter().chain(deleted.iter()).cloned().collect();
    let all_edges_rows = crate::db_json::query_rows(db, "SELECT from_node, to_node, type, output, input, kind FROM code_edges WHERE origin = ?", &[json!(origin)])?;
    let preserved_edges: Vec<Value> = all_edges_rows.as_array().cloned().unwrap_or_default().into_iter()
        .filter(|e| { let from = e.get("from_node").and_then(Value::as_str).unwrap_or(""); !recomputed_or_deleted.contains(owning_file(from)) })
        .collect();

    // ── 5b. drop the reparse-set + deleted files' old nodes + edges (new files have none) ──
    let mut drop_paths: Vec<String> = reparse_set.iter().chain(deleted.iter()).cloned().collect();
    drop_paths.sort();
    drop_paths.dedup();
    delete_paths(db, &origin, &drop_paths)?;

    // ── 5c. write the freshly-parsed nodes (nodes only; edges are recomputed in 5d) ──
    for n in &fresh_nodes {
        let id = str_qq(n.get("id"));
        let path = n.get("path").cloned().unwrap_or_else(|| json!(id.clone()));
        let span = match n.get("span") {
            Some(Value::Null) | None => n.get("span_json").cloned().unwrap_or(Value::Null),
            Some(s) => json!(s.to_string()),
        };
        crate::db_json::run_write(
            db,
            "INSERT OR REPLACE INTO code_nodes (id, origin, path, type, name, signature, intent, parent_id, span_json, content_hash, version) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            &[
                json!(id), json!(origin), path,
                n.get("type").cloned().unwrap_or(Value::Null),
                n.get("name").cloned().unwrap_or(Value::Null),
                n.get("signature").cloned().unwrap_or(Value::Null),
                n.get("intent").cloned().unwrap_or(Value::Null),
                n.get("parent_id").cloned().unwrap_or(Value::Null),
                span,
                n.get("content_hash").cloned().unwrap_or(Value::Null),
                n.get("version").cloned().unwrap_or(json!(1)),
            ],
        )?;
    }

    // ── 5d. recompute edges over the FULL current node set ──
    let all_node_rows = crate::db_json::query_rows(db, "SELECT id, path, type, name FROM code_nodes WHERE origin = ?", &[json!(origin)])?
        .as_array().cloned().unwrap_or_default();
    let all_file_ids: Vec<Value> = all_node_rows.iter().filter(|n| n.get("type").and_then(Value::as_str) == Some("file")).map(|n| n.get("id").cloned().unwrap_or(Value::Null)).collect();
    let live_ids: HashSet<String> = all_node_rows.iter().filter_map(|n| n.get("id").and_then(Value::as_str)).map(|s| s.to_string()).collect();

    let surviving_preserved: Vec<Value> = preserved_edges.into_iter()
        .filter(|e| {
            let from = e.get("from_node").and_then(Value::as_str).unwrap_or("");
            let to = e.get("to_node").and_then(Value::as_str).unwrap_or("");
            live_ids.contains(from) && live_ids.contains(to)
        })
        .collect();

    let mut config = readProjectConfig(&root);
    if let Value::Object(ref mut m) = config { m.insert("nsIndex".into(), Value::Object(ns_index)); }
    let dep_result = crate::codebase_dep_resolve::depResolve(&json!({ "files": all_file_ids, "imports": fresh_imports, "config": config }), db)?;
    let fresh_import_edges: Vec<Value> = dep_result.get("edges").and_then(|v| v.as_array()).cloned().unwrap_or_default();

    let symbols = crate::codebase_symbol_table_build::symbolTableBuild(&json!({ "nodes": all_node_rows }), db)?;
    let all_import_edges: Vec<Value> = surviving_preserved.iter().filter(|e| e.get("type").and_then(Value::as_str) == Some("imports")).cloned()
        .chain(fresh_import_edges.iter().cloned())
        .collect();
    let binding_result = crate::codebase_binding_resolve::bindingResolve(&json!({ "calls": fresh_calls, "symbols": symbols, "importEdges": all_import_edges }), db)?;
    let fresh_call_edges: Vec<Value> = binding_result.get("edges").and_then(|v| v.as_array()).cloned().unwrap_or_default();

    // FLOW edges: recomputed WHOLESALE over the full current source set — never preserved.
    let source_pairs: Vec<Value> = all_sources.iter().map(|(p, s)| json!({ "path": p, "source": s })).collect();
    let flow_edges: Vec<Value> = crate::codebase_flow_resolve::resolveFlow(&json!(source_pairs)).as_array().cloned().unwrap_or_default().into_iter()
        .filter(|e| {
            let from = e.get("from_node").and_then(Value::as_str).unwrap_or("");
            let to = e.get("to_node").and_then(Value::as_str).unwrap_or("");
            live_ids.contains(from) && live_ids.contains(to)
        })
        .collect();

    // replace the origin's edges with preserved (untouched non-flow) ∪ recomputed (changed/new) ∪ flow
    crate::db_json::run_write(db, "DELETE FROM code_edges WHERE origin = ?", &[json!(origin)])?;
    let final_edges: Vec<Value> = surviving_preserved.into_iter().filter(|e| e.get("type").and_then(Value::as_str) != Some("dispatches"))
        .chain(fresh_import_edges.into_iter())
        .chain(fresh_call_edges.into_iter())
        .chain(flow_edges.iter().cloned())
        .collect();
    for e in &final_edges {
        crate::db_json::run_write(
            db,
            "INSERT INTO code_edges (origin, from_node, to_node, type, output, input, kind) VALUES (?, ?, ?, ?, ?, ?, ?)",
            &[
                json!(origin),
                json!(str_qq(e.get("from_node"))),
                json!(str_qq(e.get("to_node"))),
                e.get("type").cloned().unwrap_or(Value::Null),
                e.get("output").cloned().unwrap_or(Value::Null),
                e.get("input").cloned().unwrap_or(Value::Null),
                e.get("kind").cloned().unwrap_or(Value::Null),
            ],
        )?;
    }

    // EPIC-CODE-DESCRIPTIONS: keep the per-file facts fresh (best-effort; never breaks sync).
    for path in changed.iter().chain(added.iter()) {
        let _ = crate::codebase_code_describe_gather::codeDescribeGather(&json!({ "path": path, "origin": origin }), db);
    }
    if !deleted.is_empty() {
        for p in &deleted { let _ = crate::db_json::run_write(db, "DELETE FROM code_descriptions WHERE origin = ? AND path = ?", &[json!(origin), json!(p)]); }
    }

    // EPIC-TYPECHECK P2: keep the parse-health snapshot current (best-effort).
    for p in &reparse_set {
        match health_by_path.get(p) {
            Some(h) => {
                let detail: String = str_qq(h.get("detail")).chars().take(500).collect();
                let _ = crate::db_json::run_write(
                    db,
                    "INSERT OR REPLACE INTO code_parse_health (origin, path, lang, status, detail) VALUES (?,?,?,?,?)",
                    &[json!(origin), json!(p), h.get("lang").cloned().unwrap_or(Value::Null), h.get("status").cloned().unwrap_or(Value::Null), json!(detail)],
                );
            }
            None => { let _ = crate::db_json::run_write(db, "DELETE FROM code_parse_health WHERE origin = ? AND path = ?", &[json!(origin), json!(p)]); }
        }
    }
    for p in &deleted { let _ = crate::db_json::run_write(db, "DELETE FROM code_parse_health WHERE origin = ? AND path = ?", &[json!(origin), json!(p)]); }

    let node_count = crate::db_json::query_rows(db, "SELECT COUNT(*) n FROM code_nodes WHERE origin = ?", &[json!(origin)])?
        .as_array().and_then(|a| a.first()).and_then(|r| r.get("n")).cloned().unwrap_or(json!(0));

    Ok(json!({
        "mode": mode,
        "changed": changed.len() as i64,
        "new": added.len() as i64,
        "deleted": deleted.len() as i64,
        "unchanged": unchanged.len() as i64,
        "closure_added": closure_added,
        "reparsed": reparsed_ok.len() as i64,
        "nodes": node_count,
        "edges": final_edges.len() as i64,
        "flow_edges": flow_edges.len() as i64,
        "errors": errors,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs as stdfs;
    use std::path::PathBuf;

    fn temp_dir(tag: &str) -> PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!("antiloki-codebase-sync-rs-{tag}-{}", std::process::id()));
        let _ = stdfs::remove_dir_all(&dir);
        stdfs::create_dir_all(&dir).unwrap();
        dir
    }

    fn db_with_schema() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        crate::codebase_code_schema::ensureCodeSchema(&c, &Value::Null);
        c
    }

    #[test]
    fn first_sync_is_all_new_files() {
        let dir = temp_dir("first");
        stdfs::write(dir.join("a.mjs"), "export function foo() { return 1; }\n").unwrap();
        let db = db_with_schema();
        let r = codebaseSync(&json!({ "root": dir.to_string_lossy() }), &db).unwrap();
        assert_eq!(r["new"], json!(1));
        assert_eq!(r["changed"], json!(0));
        assert_eq!(r["deleted"], json!(0));
        assert!(r["nodes"].as_i64().unwrap() >= 1);
    }

    #[test]
    fn unchanged_file_is_skipped_on_second_sync() {
        let dir = temp_dir("unchanged");
        stdfs::write(dir.join("a.mjs"), "export function foo() { return 1; }\n").unwrap();
        let db = db_with_schema();
        codebaseSync(&json!({ "root": dir.to_string_lossy() }), &db).unwrap();
        let r2 = codebaseSync(&json!({ "root": dir.to_string_lossy() }), &db).unwrap();
        assert_eq!(r2["new"], json!(0));
        assert_eq!(r2["changed"], json!(0));
        assert_eq!(r2["unchanged"], json!(1));
        assert_eq!(r2["reparsed"], json!(0));
    }

    #[test]
    fn changed_file_is_reparsed_and_deleted_file_drops_its_node() {
        let dir = temp_dir("changed");
        stdfs::write(dir.join("a.mjs"), "export function foo() { return 1; }\n").unwrap();
        stdfs::write(dir.join("b.mjs"), "export function bar() { return 2; }\n").unwrap();
        let db = db_with_schema();
        codebaseSync(&json!({ "root": dir.to_string_lossy() }), &db).unwrap();

        stdfs::write(dir.join("a.mjs"), "export function foo() { return 2; }\n").unwrap();
        stdfs::remove_file(dir.join("b.mjs")).unwrap();
        let r2 = codebaseSync(&json!({ "root": dir.to_string_lossy() }), &db).unwrap();
        assert_eq!(r2["changed"], json!(1));
        assert_eq!(r2["deleted"], json!(1));
        assert_eq!(r2["unchanged"], json!(0));

        let rows = crate::db_json::query_rows(&db, "SELECT path FROM code_nodes WHERE origin = 'atlas' AND type = 'file'", &[]).unwrap();
        let paths: Vec<String> = rows.as_array().unwrap().iter().map(|r| r.get("path").and_then(Value::as_str).unwrap().to_string()).collect();
        assert!(paths.contains(&"a.mjs".to_string()));
        assert!(!paths.contains(&"b.mjs".to_string()));
    }

    #[test]
    fn closure_mode_reparses_the_untouched_importer_of_a_changed_file() {
        let dir = temp_dir("closure");
        stdfs::write(dir.join("target.mjs"), "export function shared() { return 1; }\n").unwrap();
        stdfs::write(dir.join("importer.mjs"), "import { shared } from './target.mjs';\nshared();\n").unwrap();
        let db = db_with_schema();
        codebaseSync(&json!({ "root": dir.to_string_lossy() }), &db).unwrap();

        stdfs::write(dir.join("target.mjs"), "export function shared() { return 2; }\n").unwrap();
        let r2 = codebaseSync(&json!({ "root": dir.to_string_lossy(), "mode": "closure" }), &db).unwrap();
        assert_eq!(r2["changed"], json!(1));
        assert_eq!(r2["closure_added"], json!(1));
        assert_eq!(r2["reparsed"], json!(2));
    }

    #[test]
    fn full_mode_reparses_every_file() {
        let dir = temp_dir("full");
        stdfs::write(dir.join("a.mjs"), "export function foo() { return 1; }\n").unwrap();
        stdfs::write(dir.join("b.mjs"), "export function bar() { return 2; }\n").unwrap();
        let db = db_with_schema();
        codebaseSync(&json!({ "root": dir.to_string_lossy() }), &db).unwrap();
        let r2 = codebaseSync(&json!({ "root": dir.to_string_lossy(), "mode": "full" }), &db).unwrap();
        // `unchanged` is the changed/new/unchanged classification (unaffected by `mode`); both
        // files are untouched between the two syncs, so both classify as unchanged. `mode: full`
        // only widens the REPARSE set (reparsed == every current file), tracked separately via
        // closure_added/reparsed — mirrors JS truth (codebase-sync.mjs `unchanged: unchanged.length`,
        // never zeroed for `full`; see codebase-sync.test.mjs's own full-mode test, which asserts
        // reparsed/node/edge parity, never unchanged === 0).
        assert_eq!(r2["unchanged"], json!(2));
        assert_eq!(r2["closure_added"], json!(2));
        assert_eq!(r2["reparsed"], json!(2));
    }

    #[test]
    fn root_is_required() {
        let db = db_with_schema();
        let e = codebaseSync(&json!({}), &db).unwrap_err();
        assert!(e.contains("root"));
    }

    // F-06 (audit 2026-10-01): the sync lane read any file into the parser; the atlas lane has capped a file at 1 MiB all along.
    #[test]
    fn a_file_over_the_atlas_cap_is_recorded_as_skipped_and_never_read_into_the_parser() {
        let dir = temp_dir("oversize");
        stdfs::write(dir.join("ok.mjs"), "export function foo() { return 1; }\n").unwrap();
        stdfs::write(dir.join("big.mjs"), "// filler line\n".repeat(80_000)).unwrap();   // 1 200 000 bytes
        assert!(stdfs::metadata(dir.join("big.mjs")).unwrap().len() as i64 > crate::codebase_atlas_ingest::DEFAULT_MAX_FILE_BYTES);
        let db = db_with_schema();
        let r = codebaseSync(&json!({ "root": dir.to_string_lossy() }), &db).unwrap();
        let errs = r["errors"].as_array().unwrap();
        assert_eq!(errs.len(), 1, "{errs:?}");
        assert_eq!(errs[0]["path"], json!("big.mjs"));
        assert!(errs[0]["error"].as_str().unwrap().contains("skipped") && errs[0]["error"].as_str().unwrap().contains("max_file_bytes"), "{errs:?}");
        let health = crate::db_json::query_rows(&db, "SELECT path, status FROM code_parse_health ORDER BY path", &[]).unwrap();
        assert!(health.as_array().unwrap().iter().any(|h| h["path"] == json!("big.mjs") && h["status"] == json!("fail")), "{health}");
        assert_eq!(r["reparsed"], json!(1), "the ordinary file still goes through");
    }

    #[test]
    fn a_plain_file_the_parser_gave_up_on_is_a_failed_parse_in_the_sync_lane_too() {
        let dir = temp_dir("panicked");
        stdfs::write(dir.join("broken.mjs"), "}\n").unwrap();
        let db = db_with_schema();
        let r = codebaseSync(&json!({ "root": dir.to_string_lossy() }), &db).unwrap();
        let errs = r["errors"].as_array().unwrap();
        assert_eq!(errs.len(), 1, "{errs:?}");
        assert!(errs[0]["error"].as_str().unwrap().contains("gave up"), "{errs:?}");
    }
}
