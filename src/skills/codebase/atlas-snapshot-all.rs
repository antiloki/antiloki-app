//! atlas-snapshot-all.rs — twin of skills/codebase/atlas-snapshot-all.mjs (db, cross-db
//! code.*/brain.* attach lane, via the already-converted `atlasFileSnapshot` twin). NAMES ARE
//! CONTRACT: default `atlasSnapshotAll`. The FE-facing BATCH driver: "snapshot all" loops this
//! call (each call does up to `limit` files lacking a FRESH snapshot) until `remaining=0` — no
//! background-job infra needed. A file is "fresh" if its LATEST snapshot's content_hash matches
//! the file node's current content_hash (or either side is missing/empty, per the JS's loose
//! equality-style guard).
//!
//! PARITY-DEFERRED (transitively): each per-file call goes through
//! `crate::codebase_atlas_file_snapshot::atlasFileSnapshot`, which itself bottoms out at the
//! deferred `crate::ai_ai_call::aiCall` stub (always `Err`) — so every dispatched file in THIS
//! twin's own fixture returns `{ ok: false, ... }`, driving `failed` up and `generated` staying
//! 0. That is the correct, faithful behavior for the deferred boundary (see
//! atlas-file-snapshot.rs's own module doc) — everything ABOVE that boundary in both twins
//! (fresh/stale scan, batch slicing, counters) is real, ported logic and IS what this file's
//! tests exercise.
//!
//! `context.db` missing → JS throws `err.contextMissing('atlas-snapshot-all', 'db')`.
//! Unreachable here: `db: &Connection` is a required, non-nullable reference (same convention as
//! every sibling db-lane twin).
// QUALITY: bespoke-native — real db-read/batch-scan surface (fully ported, fixture-reachable) +
// 1 real dispatch to an already-converted twin (`atlasFileSnapshot`) that itself bottoms out at
// the deferred aiCall boundary (no fixture/frontier for that inner leaf; proven at its own file).

use rusqlite::Connection;
use serde_json::{json, Value};

// ── standard helpers (playbook) ─────────────────────────────────────────────────────────────
fn nz(v: Option<&Value>) -> Option<&Value> {
    v.filter(|x| !x.is_null())
}
fn js_number(v: Option<&Value>) -> f64 {
    match v {
        None => f64::NAN,
        Some(Value::Null) => 0.0,
        Some(Value::Bool(b)) => if *b { 1.0 } else { 0.0 },
        Some(Value::Number(n)) => n.as_f64().unwrap_or(f64::NAN),
        Some(Value::String(s)) => { let t = s.trim(); if t.is_empty() { 0.0 } else { t.parse().unwrap_or(f64::NAN) } }
        Some(_) => f64::NAN,
    }
}
fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}

pub fn atlasSnapshotAll(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS: `const { db } = context ?? {}; if (!db) throw err.contextMissing('atlas-snapshot-all',
    // 'db');` — unreachable here, `db` is a required reference in every db-lane call site.
    let origin = nz(input.get("origin")).cloned().unwrap_or(json!("atlas"));
    let origin_s = origin.as_str().unwrap_or("atlas").to_string();

    // `Math.max(1, Math.min(20, Number(input?.limit ?? 6)))`
    let limit_raw = nz(input.get("limit")).cloned().unwrap_or(json!(6));
    let limit_n = js_number(Some(&limit_raw));
    let limit_i: i64 = if limit_n.is_nan() { 1 } else { (limit_n.min(20.0).max(1.0)) as i64 };

    let force = matches!(input.get("force"), Some(Value::Bool(true)));

    let files = crate::db_json::query_rows(
        db,
        "SELECT path, content_hash FROM code.code_nodes WHERE origin=? AND type='file' ORDER BY path",
        &[origin.clone()],
    )?;
    let files = files.as_array().cloned().unwrap_or_default();
    let total = files.len() as i64;

    // A file is "fresh" if its latest snapshot's content_hash matches the file node's current hash.
    let mut needs: Vec<Value> = Vec::new();
    for f in &files {
        if force {
            needs.push(f.clone());
            continue;
        }
        let snap_rows = crate::db_json::query_rows(
            db,
            "SELECT content_hash FROM code.code_file_snapshots WHERE origin=? AND path=? ORDER BY id DESC LIMIT 1",
            &[origin.clone(), f.get("path").cloned().unwrap_or(Value::Null)],
        )?;
        let snap = snap_rows.as_array().and_then(|a| a.first()).cloned();
        let f_hash = f.get("content_hash").cloned().unwrap_or(Value::Null);
        let fresh = match &snap {
            Some(s) => {
                let s_hash = s.get("content_hash").cloned().unwrap_or(Value::Null);
                !truthy(Some(&f_hash)) || !truthy(Some(&s_hash)) || s_hash == f_hash
            }
            None => false,
        };
        if !fresh {
            needs.push(f.clone());
        }
    }
    let with_snap_before = total - needs.len() as i64;

    let mut generated: i64 = 0;
    let mut failed: i64 = 0;
    let batch_len = (limit_i as usize).min(needs.len());
    for f in needs.iter().take(batch_len) {
        let file_input = json!({
            "path": f.get("path").cloned().unwrap_or(Value::Null),
            "origin": origin,
            "root": input.get("root").cloned().unwrap_or(Value::Null),
        });
        match crate::codebase_atlas_file_snapshot::atlasFileSnapshot(&file_input, db) {
            Ok(r) => {
                if truthy(r.get("ok")) { generated += 1; } else { failed += 1; }
            }
            Err(_) => { failed += 1; }
        }
    }

    // remaining counts files still without a fresh snapshot (failed ones included → the FE stops
    // the loop when a batch makes no progress, so a persistent provider failure can't spin forever).
    let remaining = (needs.len() as i64 - generated).max(0);
    let batch = (limit_i).min(needs.len() as i64);

    Ok(json!({
        "generated": generated,
        "failed": failed,
        "batch": batch,
        "total": total,
        "with_snapshot": with_snap_before + generated,
        "remaining": remaining,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seed_schema(c: &Connection) {
        c.execute_batch(
            "ATTACH ':memory:' AS code; ATTACH ':memory:' AS brain; \
             CREATE TABLE workspaces (root_path TEXT, is_active INTEGER); \
             CREATE TABLE code.code_nodes (id TEXT, origin TEXT, path TEXT, type TEXT, name TEXT, signature TEXT, parent_id TEXT, span_json TEXT, content_hash TEXT, intent TEXT); \
             CREATE TABLE code.code_edges (origin TEXT, from_node TEXT, to_node TEXT, type TEXT); \
             CREATE TABLE code.code_signals (origin TEXT, category TEXT, kind TEXT, severity TEXT, path TEXT, line INTEGER, detail TEXT); \
             CREATE TABLE code.code_file_snapshots ( id INTEGER PRIMARY KEY AUTOINCREMENT, origin TEXT NOT NULL, path TEXT NOT NULL, content_hash TEXT, snapshot_json TEXT NOT NULL, model_id TEXT, created_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')) ); \
             CREATE TABLE brain.prompt_templates ( id INTEGER PRIMARY KEY, name TEXT NOT NULL, version INTEGER NOT NULL, template_md TEXT NOT NULL, default_tier TEXT NOT NULL, default_temperature TEXT NOT NULL, max_output_tokens INTEGER NOT NULL DEFAULT 1024, superseded_by INTEGER, created_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')), cross_field_rules_json TEXT, judging_criteria_json TEXT, system_md TEXT, phase_flow TEXT NOT NULL DEFAULT 'single', UNIQUE (name, version) ); \
             CREATE TABLE brain.prompt_output_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, grounded_refs_json TEXT, UNIQUE (prompt_template_id, field_name) ); \
             CREATE TABLE brain.prompt_input_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, UNIQUE (prompt_template_id, field_name) );",
        ).unwrap();
    }

    fn workspace_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("atlas-snapshot-all-test-{}-{}", tag, std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn empty_atlas_returns_zeroed_counters() {
        let c = Connection::open_in_memory().unwrap();
        seed_schema(&c);
        let r = atlasSnapshotAll(&json!({}), &c).unwrap();
        assert_eq!(r["total"], json!(0));
        assert_eq!(r["batch"], json!(0));
        assert_eq!(r["remaining"], json!(0));
        assert_eq!(r["with_snapshot"], json!(0));
        assert_eq!(r["generated"], json!(0));
        assert_eq!(r["failed"], json!(0));
    }

    #[test]
    fn stale_and_fresh_files_are_distinguished_and_dispatched_through_the_deferred_boundary() {
        let c = Connection::open_in_memory().unwrap();
        seed_schema(&c);
        let dir = workspace_dir("stale");
        std::fs::write(dir.join("a.ts"), "export function f() {}\n").unwrap();
        std::fs::write(dir.join("b.ts"), "export function g() {}\n").unwrap();
        c.execute("INSERT INTO code.code_nodes (id,origin,path,type,content_hash) VALUES ('a.ts@atlas','atlas','a.ts','file','h1')", []).unwrap();
        c.execute("INSERT INTO code.code_nodes (id,origin,path,type,content_hash) VALUES ('b.ts@atlas','atlas','b.ts','file','h2')", []).unwrap();
        // a.ts has a FRESH snapshot (matching content_hash) — should be skipped.
        c.execute("INSERT INTO code.code_file_snapshots (origin,path,content_hash,snapshot_json) VALUES ('atlas','a.ts','h1','{}')", []).unwrap();
        // b.ts has a STALE snapshot (different content_hash) — needs regeneration.
        c.execute("INSERT INTO code.code_file_snapshots (origin,path,content_hash,snapshot_json) VALUES ('atlas','b.ts','OLDHASH','{}')", []).unwrap();

        let r = atlasSnapshotAll(&json!({ "root": dir.to_string_lossy() }), &c).unwrap();
        assert_eq!(r["total"], json!(2));
        assert_eq!(r["with_snapshot"], json!(1)); // a.ts was fresh going in
        assert_eq!(r["batch"], json!(1)); // only b.ts needed work
        // the dispatched call bottoms out at the deferred aiCall stub → ok:false → failed, not generated.
        assert_eq!(r["generated"], json!(0));
        assert_eq!(r["failed"], json!(1));
        assert_eq!(r["remaining"], json!(1));
    }

    #[test]
    fn force_reprocesses_even_fresh_files_and_batch_respects_limit() {
        let c = Connection::open_in_memory().unwrap();
        seed_schema(&c);
        let dir = workspace_dir("force");
        std::fs::write(dir.join("a.ts"), "export const a = 1;\n").unwrap();
        std::fs::write(dir.join("b.ts"), "export const b = 1;\n").unwrap();
        std::fs::write(dir.join("c.ts"), "export const c = 1;\n").unwrap();
        c.execute("INSERT INTO code.code_nodes (id,origin,path,type,content_hash) VALUES ('a.ts@atlas','atlas','a.ts','file','h1')", []).unwrap();
        c.execute("INSERT INTO code.code_nodes (id,origin,path,type,content_hash) VALUES ('b.ts@atlas','atlas','b.ts','file','h2')", []).unwrap();
        c.execute("INSERT INTO code.code_nodes (id,origin,path,type,content_hash) VALUES ('c.ts@atlas','atlas','c.ts','file','h3')", []).unwrap();
        c.execute("INSERT INTO code.code_file_snapshots (origin,path,content_hash,snapshot_json) VALUES ('atlas','a.ts','h1','{}')", []).unwrap();
        c.execute("INSERT INTO code.code_file_snapshots (origin,path,content_hash,snapshot_json) VALUES ('b.ts','b.ts','h2','{}')", []).unwrap(); // wrong origin, irrelevant
        c.execute("INSERT INTO code.code_file_snapshots (origin,path,content_hash,snapshot_json) VALUES ('atlas','c.ts','h3','{}')", []).unwrap();

        let r = atlasSnapshotAll(&json!({ "force": true, "limit": 2, "root": dir.to_string_lossy() }), &c).unwrap();
        assert_eq!(r["total"], json!(3));
        assert_eq!(r["batch"], json!(2)); // limit=2 caps the 3 forced files
        // remaining = needs.length - generated (the deferred boundary keeps generated at 0), NOT
        // scoped to the batch — the FE re-loops until this hits 0.
        assert_eq!(r["remaining"], json!(3));
    }

    #[test]
    fn limit_clamps_to_the_1_to_20_range() {
        let c = Connection::open_in_memory().unwrap();
        seed_schema(&c);
        let dir = workspace_dir("clamp");
        for i in 0..3 {
            let name = format!("f{i}.ts");
            std::fs::write(dir.join(&name), "export const x = 1;\n").unwrap();
            c.execute(
                "INSERT INTO code.code_nodes (id,origin,path,type,content_hash) VALUES (?,'atlas',?,'file','h')",
                rusqlite::params![format!("{name}@atlas"), name],
            ).unwrap();
        }
        // limit=0 clamps up to 1.
        let r = atlasSnapshotAll(&json!({ "limit": 0, "root": dir.to_string_lossy() }), &c).unwrap();
        assert_eq!(r["batch"], json!(1));
        // limit=999 clamps down to 20 (but only 3 files exist, so batch is capped by need count).
        let r2 = atlasSnapshotAll(&json!({ "limit": 999, "root": dir.to_string_lossy() }), &c).unwrap();
        assert_eq!(r2["batch"], json!(3));
    }
}
