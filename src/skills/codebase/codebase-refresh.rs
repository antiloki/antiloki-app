//! codebase-refresh.rs — twin of skills/codebase/codebase-refresh.mjs. NAMES ARE CONTRACT:
//! `codebaseRefresh` (default). The "code changed → keep the derived world coherent" orchestrator:
//! EAGER closure-sync (codebase-sync) + entry-points re-extract, HISTORY-append (entry-point
//! snapshot, inside atlasEntryPoints), LAZY-verify (re-check every data-story vs the live graph),
//! and a best-effort RAG re-sweep when anything moved. `detect_only:true` reports the drift set
//! WITHOUT mutating (the FE "code changed outside antiloki — refresh?" banner).
//!
//! BESPOKE-NATIVE (real fs hashing of a real root + real sqlite reads across code_nodes /
//! code_file_snapshots, orchestrating other already-converted twins), not conform-db — same class
//! as its own codebaseSync dependency. Proven below via `#[cfg(test)]` against a real temp dir + an
//! in-memory sqlite db seeded via the already-converted `codebase_code_schema::ensureCodeSchema` twin.
//!
//! PARITY-DEFERRED BOUNDARY: the real embed call lives behind `rag_rag_sweep::ragSweep` (its own
//! `external_embed::embed` dependency always Errs per row — see that module's doc). This file calls
//! `ragSweep({ implicit: true }, context)` exactly once, best-effort (JS wraps it in a `catch {}`
//! that leaves `rag = null`); everything else here — the changed/added/deleted hash-diff, the
//! `codebase-sync` + `atlas-entry-points` orchestration, the per-story broken-vs-ok tally, and the
//! `snapshots_stale` count — is fully deterministic and ported faithfully.
use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::collections::HashSet;
use std::fs;
use std::path::Path;

use crate::codebase_atlas_ingest::{discover, hashSource};

fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) }
fn str_qq(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(),
        Some(o) => o.to_string(),
    }
}
fn is_truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}
fn to_vec_str(v: &Value) -> Vec<String> {
    v.as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default()
}

// Mirror of codebase-sync's own hash-diff classification (so detect + sync always agree): hash the
// current in-scope files, compare to the content_hash stamped on the atlas's file nodes.
fn detect_changes(db: &Connection, root: &str, origin: &str, include: &[String], exclude: &[String]) -> Result<(Vec<String>, Vec<String>, Vec<String>), String> {
    let current = discover(root, include, exclude);
    let stored_rows = crate::db_json::query_rows(db, "SELECT path, content_hash FROM code_nodes WHERE origin=? AND type='file'", &[json!(origin)])?;
    let mut stored: std::collections::HashMap<String, Option<String>> = std::collections::HashMap::new();
    if let Some(arr) = stored_rows.as_array() {
        for r in arr {
            let path = r.get("path").and_then(Value::as_str).unwrap_or("").to_string();
            let hash = r.get("content_hash").and_then(Value::as_str).map(|s| s.to_string());
            stored.insert(path, hash);
        }
    }

    let mut changed: Vec<String> = Vec::new();
    let mut added: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for p in &current {
        seen.insert(p.clone());
        let h: Option<String> = match fs::read_to_string(Path::new(root).join(p)) {
            Ok(src) => Some(hashSource(&src)),
            Err(_) => None,
        };
        match stored.get(p) {
            None => added.push(p.clone()),
            Some(sh) => { if sh != &h { changed.push(p.clone()); } }
        }
    }
    let deleted: Vec<String> = stored.keys().filter(|p| !seen.contains(*p)).cloned().collect();
    Ok((changed, added, deleted))
}

fn snapshots_stale_count(db: &Connection, origin: &str) -> i64 {
    db.query_row(
        "SELECT COUNT(DISTINCT s.path) n FROM code_file_snapshots s \
         JOIN code_nodes f ON f.origin = s.origin AND f.path = s.path AND f.type = 'file' \
         WHERE s.origin = ? AND (f.content_hash IS NULL OR s.content_hash IS NULL OR f.content_hash <> s.content_hash)",
        rusqlite::params![origin],
        |r| r.get(0),
    ).unwrap_or(0)
}

pub fn codebaseRefresh(input: &Value, context: &Value, db: &Connection) -> Result<Value, String> {
    let origin_v = nz(input.get("origin")).cloned().unwrap_or(json!("atlas"));
    let origin = str_qq(Some(&origin_v));
    let root_v = match nz(input.get("root")) {
        Some(r) => r.clone(),
        None => crate::engine_workspace_root::getWorkspaceRoot(db),
    };
    let root = str_qq(Some(&root_v));

    let scope = crate::codebase_code_scope_get::loadScope(db, &origin_v);
    let include: Vec<String> = match nz(input.get("include")) { Some(v) => to_vec_str(v), None => to_vec_str(&scope["include"]) };
    let exclude: Vec<String> = match nz(input.get("exclude")) { Some(v) => to_vec_str(v), None => to_vec_str(&scope["exclude"]) };

    let (changed, added, deleted) = detect_changes(db, &root, &origin, &include, &exclude)?;
    let snapshots_stale = snapshots_stale_count(db, &origin);

    // DETECT-ONLY: the banner case — report what's drifted, change nothing.
    if is_truthy(input.get("detect_only")) {
        return Ok(json!({
            "detect_only": true,
            "changed": changed, "added": added, "deleted": deleted,
            "dirty": (changed.len() + added.len() + deleted.len()) as i64,
            "snapshots_stale": snapshots_stale,
        }));
    }

    // ── EAGER: atlas + neighbourhood + flow (closure sync), then the external surface ──
    let mode = nz(input.get("mode")).and_then(Value::as_str).map(|s| s.to_string()).unwrap_or_else(|| "closure".to_string());
    let sync = crate::codebase_codebase_sync::codebaseSync(
        &json!({ "root": root, "origin": origin, "mode": mode, "include": include, "exclude": exclude }),
        db,
    )?;

    // `input?.capture !== false` — anything but the literal boolean false.
    let capture = !matches!(input.get("capture"), Some(Value::Bool(false)));
    let entry: Option<Value> = crate::codebase_atlas_entry_points::atlasEntryPoints(
        &json!({ "origin": origin, "root": root, "capture": capture }),
        db,
    ).ok();

    // ── LAZY-verify: re-check every story vs the live graph; flag the broken ones ──
    let mut ok: i64 = 0;
    let mut broken: i64 = 0;
    let mut broken_ids: Vec<Value> = Vec::new();
    if let Ok(list) = crate::codebase_data_story_list::dataStoryList(&json!({ "origin": origin }), db) {
        if let Some(stories) = list.get("stories").and_then(Value::as_array) {
            for s in stories {
                let id = s.get("id").cloned().unwrap_or(Value::Null);
                if let Ok(v) = crate::codebase_data_story_get::dataStoryGet(&json!({ "id": id, "origin": origin }), db) {
                    if v.get("status").and_then(Value::as_str) == Some("broken") {
                        broken += 1;
                        broken_ids.push(id);
                    } else {
                        ok += 1;
                    }
                }
            }
        }
    }

    // ── RAG: the semantic index is part of the derived world — re-sweep after the graph moved.
    // Best-effort: an embed-model hiccup never fails the refresh. Skipped when nothing changed.
    let mut rag: Option<Value> = None;
    if changed.len() + added.len() + deleted.len() > 0 {
        rag = crate::rag_rag_sweep::ragSweep(&json!({ "implicit": true }), context, db).ok();
    }

    let entry_points: Value = match &entry {
        Some(e) => {
            let mut m: Map<String, Value> = e.get("counts").and_then(Value::as_object).cloned().unwrap_or_default();
            let added_ep = e.get("snapshot").and_then(|s| s.get("added")).and_then(Value::as_i64).unwrap_or(0);
            let removed_ep = e.get("snapshot").and_then(|s| s.get("removed")).and_then(Value::as_i64).unwrap_or(0);
            m.insert("added".into(), json!(added_ep));
            m.insert("removed".into(), json!(removed_ep));
            Value::Object(m)
        }
        None => Value::Null,
    };

    let rag_out: Value = match &rag {
        Some(r) if !is_truthy(r.get("skipped_implicit")) => json!({
            "embedded": r.get("embedded").cloned().unwrap_or(Value::Null),
            "skipped": r.get("skipped").cloned().unwrap_or(Value::Null),
            "deleted": r.get("deleted").cloned().unwrap_or(Value::Null),
        }),
        _ => Value::Null,
    };

    Ok(json!({
        "detect_only": false,
        "changed": changed.len() as i64,
        "added": added.len() as i64,
        "deleted": deleted.len() as i64,
        "reparsed": sync.get("reparsed").cloned().unwrap_or(Value::Null),
        "closure_added": sync.get("closure_added").cloned().unwrap_or(Value::Null),
        "edges": sync.get("edges").cloned().unwrap_or(Value::Null),
        "flow_edges": sync.get("flow_edges").and_then(Value::as_i64).unwrap_or(0),
        "entry_points": entry_points,
        "stories": { "ok": ok, "broken": broken, "broken_ids": broken_ids },
        "snapshots_stale": snapshots_stale,
        "rag": rag_out,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs as stdfs;
    use std::path::PathBuf;

    fn temp_dir(tag: &str) -> PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!("antiloki-codebase-refresh-rs-{tag}-{}", std::process::id()));
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
    fn detect_only_reports_added_files_without_mutating() {
        let dir = temp_dir("detect-only");
        stdfs::write(dir.join("a.mjs"), "export function foo() { return 1; }\n").unwrap();
        let db = db_with_schema();
        let r = codebaseRefresh(&json!({ "root": dir.to_string_lossy(), "detect_only": true }), &Value::Null, &db).unwrap();
        assert_eq!(r["detect_only"], json!(true));
        assert_eq!(r["added"], json!(["a.mjs"]));
        assert_eq!(r["changed"], json!([]));
        assert_eq!(r["deleted"], json!([]));
        assert_eq!(r["dirty"], json!(1));
        // no mutation happened: the atlas is still empty.
        let count: i64 = db.query_row("SELECT COUNT(*) FROM code_nodes", [], |row| row.get(0)).unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn full_refresh_syncs_the_atlas_and_reports_entry_points() {
        let dir = temp_dir("full");
        stdfs::write(dir.join("a.mjs"), "export function foo() { return 1; }\n").unwrap();
        let db = db_with_schema();
        let r = codebaseRefresh(&json!({ "root": dir.to_string_lossy() }), &Value::Null, &db).unwrap();
        assert_eq!(r["detect_only"], json!(false));
        assert_eq!(r["added"], json!(1));
        assert_eq!(r["changed"], json!(0));
        assert!(r["entry_points"].is_object());
        assert_eq!(r["stories"]["ok"], json!(0));
        assert_eq!(r["stories"]["broken"], json!(0));
        // the atlas now has the file node.
        let count: i64 = db.query_row("SELECT COUNT(*) FROM code_nodes WHERE origin='atlas' AND type='file'", [], |row| row.get(0)).unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn second_refresh_with_no_changes_reports_zero_and_rag_stays_null() {
        let dir = temp_dir("second");
        stdfs::write(dir.join("a.mjs"), "export function foo() { return 1; }\n").unwrap();
        let db = db_with_schema();
        codebaseRefresh(&json!({ "root": dir.to_string_lossy() }), &Value::Null, &db).unwrap();
        let r2 = codebaseRefresh(&json!({ "root": dir.to_string_lossy() }), &Value::Null, &db).unwrap();
        assert_eq!(r2["added"], json!(0));
        assert_eq!(r2["changed"], json!(0));
        assert_eq!(r2["deleted"], json!(0));
        // nothing changed -> ragSweep is never even attempted.
        assert_eq!(r2["rag"], Value::Null);
    }

    #[test]
    fn broken_story_is_flagged_and_ok_story_is_not() {
        let dir = temp_dir("stories");
        stdfs::write(dir.join("a.mjs"), "export function foo() { return 1; }\nfoo();\n").unwrap();
        let db = db_with_schema();
        codebaseRefresh(&json!({ "root": dir.to_string_lossy() }), &Value::Null, &db).unwrap();
        // a story hopping through a node id that doesn't exist in the live graph -> broken.
        db.execute(
            "INSERT INTO code_data_stories (origin, name, hops_json) VALUES ('atlas', 'ghost', '[{\"node_id\":\"nowhere.js#missing\"}]')",
            [],
        ).unwrap();
        let r = codebaseRefresh(&json!({ "root": dir.to_string_lossy() }), &Value::Null, &db).unwrap();
        assert_eq!(r["stories"]["broken"], json!(1));
        assert_eq!(r["stories"]["broken_ids"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn snapshots_stale_counts_diverged_content_hashes() {
        let dir = temp_dir("stale");
        stdfs::write(dir.join("a.mjs"), "export function foo() { return 1; }\n").unwrap();
        let db = db_with_schema();
        codebaseRefresh(&json!({ "root": dir.to_string_lossy() }), &Value::Null, &db).unwrap();
        db.execute(
            "INSERT INTO code_file_snapshots (origin, path, content_hash, snapshot_json) VALUES ('atlas', 'a.mjs', 'stale-hash', '{}')",
            [],
        ).unwrap();
        let r = codebaseRefresh(&json!({ "root": dir.to_string_lossy(), "detect_only": true }), &Value::Null, &db).unwrap();
        assert_eq!(r["snapshots_stale"], json!(1));
    }

    #[test]
    fn deleted_file_is_detected_after_removal() {
        let dir = temp_dir("deleted");
        stdfs::write(dir.join("a.mjs"), "export function foo() { return 1; }\n").unwrap();
        let db = db_with_schema();
        codebaseRefresh(&json!({ "root": dir.to_string_lossy() }), &Value::Null, &db).unwrap();
        stdfs::remove_file(dir.join("a.mjs")).unwrap();
        let r = codebaseRefresh(&json!({ "root": dir.to_string_lossy(), "detect_only": true }), &Value::Null, &db).unwrap();
        assert_eq!(r["deleted"], json!(["a.mjs"]));
    }

    #[test]
    fn scope_defaults_come_from_the_saved_code_scope_row() {
        let dir = temp_dir("scope");
        stdfs::write(dir.join("a.mjs"), "export function foo() { return 1; }\n").unwrap();
        stdfs::write(dir.join("b.ts"), "export const bar = 1;\n").unwrap();
        let db = db_with_schema();
        db.execute("INSERT INTO code_scope (origin, exclude_json, include_json) VALUES ('atlas', '[]', '[\"*.mjs\"]')", []).unwrap();
        let r = codebaseRefresh(&json!({ "root": dir.to_string_lossy(), "detect_only": true }), &Value::Null, &db).unwrap();
        // only the .mjs file matches the saved include glob.
        assert_eq!(r["added"], json!(["a.mjs"]));
    }
}
