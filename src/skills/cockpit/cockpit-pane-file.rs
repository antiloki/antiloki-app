//! cockpit-pane-file.rs — twin of skills/cockpit/cockpit-pane-file.mjs. NAMES ARE CONTRACT:
//! `cockpitPaneFile` (default). The pane's file tools (`read`/`write`) on the chat engine — edits
//! become DISPATCHED ACTIONS through the SAME `processEdits` boundary
//! (`crate::cockpit_cockpit_pane_message::processEdits`) the legacy reply-parsing loop uses.
//!
//! BESPOKE-NATIVE (dispatches to the already-twinned `cockpit-pane-message.rs`'s `processEdits`,
//! plus a real fs read fallback for the `read` op), not conform-db: proven below via
//! `#[cfg(test)]` against a real temp dir + an in-memory sqlite db.
use rusqlite::Connection;
use serde_json::{json, Value};

// JS Number(v): the subset that appears here (missing/null/number/string).
fn js_number(v: Option<&Value>) -> f64 {
    match v {
        None => f64::NAN,
        Some(Value::Null) => 0.0,
        Some(Value::Bool(b)) => if *b { 1.0 } else { 0.0 },
        Some(Value::Number(n)) => n.as_f64().unwrap_or(f64::NAN),
        Some(Value::String(s)) => { let t = s.trim(); if t.is_empty() { 0.0 } else { t.parse().unwrap_or(f64::NAN) } },
        Some(_) => f64::NAN,
    }
}
// JS `!n` on a Number: falsy for 0 / -0 / NaN.
fn truthy_num(n: f64) -> bool { n != 0.0 && !n.is_nan() }
// node:sqlite binds an integral JS number as INTEGER, a fractional one as REAL.
fn num_param(n: f64) -> Value { if n.is_finite() && n == n.trunc() { json!(n as i64) } else { json!(n) } }
// JS String(v ?? '')
fn str_qq(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(),
        Some(o) => o.to_string(),
    }
}
fn take_chars(s: &str, n: usize) -> String { s.chars().take(n).collect() }
// JSON.parse(s ?? '[]') catch → [] ; non-array → []
fn parse_arr(s: Option<&Value>) -> Vec<String> {
    let raw = match s { Some(Value::String(t)) => t.clone(), None | Some(Value::Null) => "[]".to_string(), Some(o) => o.to_string() };
    match serde_json::from_str::<Value>(&raw) {
        Ok(Value::Array(a)) => a.iter().map(|v| str_qq(Some(v))).collect(),
        _ => Vec::new(),
    }
}
fn parse_arr_json(s: Option<&Value>) -> Vec<Value> {
    let raw = match s { Some(Value::String(t)) => t.clone(), None | Some(Value::Null) => "[]".to_string(), Some(o) => o.to_string() };
    match serde_json::from_str::<Value>(&raw) {
        Ok(Value::Array(a)) => a,
        _ => Vec::new(),
    }
}

const MAX_READ_CHARS: usize = 48000;

pub fn cockpitPaneFile(input: &Value, db: &Connection) -> Result<Value, String> {
    // context.db is always provided by the harness; the JS `if (!db) throw` guard has no Rust twin.
    crate::cockpit_cockpit_schema::ensureCockpitSchema(db, &json!(""));

    let op = str_qq(input.get("op"));
    let pane_id_n = js_number(input.get("pane_id"));
    if !truthy_num(pane_id_n) { return Err("cockpit-pane-file: pane_id is required".into()); }
    let pane_id = num_param(pane_id_n);

    let rows = crate::db_json::query_rows(db, "SELECT * FROM cockpit_panes WHERE id = ?", &[pane_id.clone()])?;
    let pane = match rows.as_array().and_then(|a| a.first()) {
        Some(p) => p.clone(),
        None => return Ok(json!({ "ok": false, "error": format!("pane {} not found", pane_id_n as i64) })),
    };
    let pane_kind = pane.get("pane_kind").and_then(Value::as_str).unwrap_or("agent");
    if pane_kind != "agent" {
        return Ok(json!({ "ok": false, "error": format!("pane {} is not an agent pane", pane_id_n as i64) }));
    }

    let path = str_qq(input.get("path"));
    if path.is_empty() { return Err("cockpit-pane-file: path is required".into()); }

    let scope = parse_arr(pane.get("scope_json"));
    let grants = parse_arr(pane.get("grants_json"));
    let can_edit = grants.iter().any(|g| g == "edit" || g == "write");

    if op == "read" {
        // Same boundary minus the edit grant — reading its slice is how the agent sees.
        // apply:false returns the CURRENT content as plan[0].old_content.
        let probe = crate::cockpit_cockpit_pane_message::processEdits(&json!({
            "pane": pane, "edits": [{ "path": path, "content": "" }], "scope": scope, "canEdit": true, "apply": false,
        }));
        let rejected = probe.get("rejected").and_then(Value::as_array).cloned().unwrap_or_default();
        if !rejected.is_empty() {
            let reason = rejected[0].get("reason").cloned().unwrap_or(Value::Null);
            return Ok(json!({ "ok": false, "pane_id": pane_id_n as i64, "path": path, "error": reason }));
        }
        let plan0 = probe.get("plan").and_then(Value::as_array).and_then(|a| a.first()).cloned().unwrap_or(json!({}));
        let rel = plan0.get("path").and_then(Value::as_str).unwrap_or("").to_string();
        let mut content = plan0.get("old_content").and_then(Value::as_str).map(|s| s.to_string());
        if content.is_none() {
            // plan read failed / new file — distinguish via a direct existence probe
            let worktree = pane.get("worktree_path").and_then(Value::as_str).unwrap_or("");
            content = std::fs::read_to_string(std::path::Path::new(worktree).join(&rel)).ok();
        }
        let content = match content {
            None => return Ok(json!({ "ok": true, "pane_id": pane_id_n as i64, "path": rel, "exists": false, "content": Value::Null })),
            Some(c) => c,
        };
        let truncated = content.chars().count() > MAX_READ_CHARS;
        return Ok(json!({
            "ok": true, "pane_id": pane_id_n as i64, "path": rel, "exists": true,
            "content": take_chars(&content, MAX_READ_CHARS), "truncated": truncated,
        }));
    }

    if op == "write" {
        let content = str_qq(input.get("content"));
        let auto = js_number(pane.get("auto_apply")) == 1.0;
        let session_id = pane.get("session_id").cloned().unwrap_or(Value::Null);
        let r = crate::cockpit_cockpit_pane_message::processEdits(&json!({
            "pane": pane, "edits": [{ "path": path, "content": content }], "scope": scope, "canEdit": can_edit, "apply": auto,
        }));
        let rejected = r.get("rejected").and_then(Value::as_array).cloned().unwrap_or_default();
        if !rejected.is_empty() {
            // NEVER silent (operator 2026-07-06): the refusal reason lands in the pane thread.
            let list = rejected.iter()
                .map(|x| format!("{} ({})", x.get("path").and_then(Value::as_str).unwrap_or("?"), x.get("reason").and_then(Value::as_str).unwrap_or("")))
                .collect::<Vec<_>>().join(" · ");
            crate::db_json::run_write(
                db,
                "INSERT INTO cockpit_blackboard (session_id, pane_id, kind, body_md) VALUES (?,?,?,?)",
                &[session_id.clone(), pane_id.clone(), json!("note"), json!(format!("⚠ rejected: {list}"))],
            )?;
            return Ok(json!({
                "ok": false, "pane_id": pane_id_n as i64, "applied": [], "rejected": rejected,
                "mode": if auto { "auto" } else { "plan" },
            }));
        }
        let applied = r.get("applied").and_then(Value::as_array).cloned().unwrap_or_default();
        if auto {
            let applied_list = applied.iter().map(|v| str_qq(Some(v))).collect::<Vec<_>>().join(", ");
            crate::db_json::run_write(
                db,
                "INSERT INTO cockpit_blackboard (session_id, pane_id, kind, body_md) VALUES (?,?,?,?)",
                &[session_id.clone(), pane_id.clone(), json!("note"), json!(format!("✅ applied → {applied_list} (auto ⚡)"))],
            )?;
            let idx = str_qq(pane.get("idx"));
            let subagent_name = str_qq(pane.get("subagent_name"));
            crate::db_json::run_write(
                db,
                "INSERT INTO cockpit_blackboard (session_id, pane_id, kind, body_md) VALUES (?, NULL, ?, ?)",
                &[session_id.clone(), json!("handoff"), json!(format!("pane {idx} ({subagent_name}) edited: {applied_list}"))],
            )?;
            crate::db_json::run_write(db, "UPDATE cockpit_panes SET last_activity = datetime('now') WHERE id = ?", &[pane_id.clone()])?;
            return Ok(json!({ "ok": true, "pane_id": pane_id_n as i64, "applied": applied, "rejected": [], "mode": "auto" }));
        }
        // Manual: MERGE into the pending plan (same-path entries replaced) — the existing
        // apply_pending/discard_pending ops + the FE's diff/Apply button stay the gate.
        let plan0 = r.get("plan").and_then(Value::as_array).and_then(|a| a.first()).cloned().unwrap_or(json!({}));
        let new_path = plan0.get("path").cloned().unwrap_or(Value::Null);
        let mut pending = parse_arr_json(pane.get("pending_edits_json"));
        pending.retain(|p| p.get("path") != Some(&new_path));
        pending.push(plan0);
        crate::db_json::run_write(
            db,
            "UPDATE cockpit_panes SET pending_edits_json = ?, last_activity = datetime('now') WHERE id = ?",
            &[json!(serde_json::to_string(&pending).unwrap_or_default()), pane_id.clone()],
        )?;
        let paths_list = pending.iter().map(|p| str_qq(p.get("path"))).collect::<Vec<_>>().join(", ");
        crate::db_json::run_write(
            db,
            "INSERT INTO cockpit_blackboard (session_id, pane_id, kind, body_md) VALUES (?,?,?,?)",
            &[session_id, pane_id.clone(), json!("note"), json!(format!("📋 plan pending — {} file(s): {paths_list} (Apply to write)", pending.len()))],
        )?;
        return Ok(json!({ "ok": true, "pane_id": pane_id_n as i64, "applied": [], "rejected": [], "pending": pending, "mode": "plan" }));
    }

    Err(format!("cockpit-pane-file: op must be one of: read, write (got '{op}')"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn temp_dir(tag: &str) -> PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!("antiloki-cockpit-pane-file-rs-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn seeded() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        crate::cockpit_cockpit_schema::ensureCockpitSchema(&c, &json!(""));
        c.execute("INSERT INTO cockpit_sessions (id, name, created_at) VALUES (1, 's1', datetime('now'))", []).unwrap();
        c
    }

    fn insert_pane(c: &Connection, worktree: &str, scope: &str, grants: &str, auto_apply: i64, pane_kind: &str) -> i64 {
        crate::db_json::run_write(
            c,
            "INSERT INTO cockpit_panes (session_id, idx, subagent_name, worktree_path, scope_json, grants_json, auto_apply, pane_kind) VALUES (1, 0, 'dev', ?, ?, ?, ?, ?)",
            &[json!(worktree), json!(scope), json!(grants), json!(auto_apply), json!(pane_kind)],
        ).unwrap().1
    }

    #[test]
    fn pane_id_required() {
        let c = seeded();
        let err = cockpitPaneFile(&json!({ "op": "read" }), &c).unwrap_err();
        assert_eq!(err, "cockpit-pane-file: pane_id is required");
    }

    #[test]
    fn pane_not_found() {
        let c = seeded();
        let r = cockpitPaneFile(&json!({ "op": "read", "pane_id": 999, "path": "a.txt" }), &c).unwrap();
        assert_eq!(r, json!({ "ok": false, "error": "pane 999 not found" }));
    }

    #[test]
    fn non_agent_pane_rejected() {
        let c = seeded();
        let dir = temp_dir("nonagent");
        let pane_id = insert_pane(&c, &dir.to_string_lossy(), "[]", "[]", 0, "human");
        let r = cockpitPaneFile(&json!({ "op": "read", "pane_id": pane_id, "path": "a.txt" }), &c).unwrap();
        assert_eq!(r, json!({ "ok": false, "error": format!("pane {pane_id} is not an agent pane") }));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn path_required() {
        let c = seeded();
        let dir = temp_dir("pathreq");
        let pane_id = insert_pane(&c, &dir.to_string_lossy(), "[]", "[]", 0, "agent");
        let err = cockpitPaneFile(&json!({ "op": "read", "pane_id": pane_id }), &c).unwrap_err();
        assert_eq!(err, "cockpit-pane-file: path is required");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn read_existing_file_returns_content() {
        let c = seeded();
        let dir = temp_dir("readexist");
        std::fs::write(dir.join("a.txt"), "hello world").unwrap();
        let pane_id = insert_pane(&c, &dir.to_string_lossy(), "[\"*.txt\"]", "[]", 0, "agent");
        let r = cockpitPaneFile(&json!({ "op": "read", "pane_id": pane_id, "path": "a.txt" }), &c).unwrap();
        assert_eq!(r, json!({ "ok": true, "pane_id": pane_id, "path": "a.txt", "exists": true, "content": "hello world", "truncated": false }));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn read_missing_file_returns_exists_false() {
        let c = seeded();
        let dir = temp_dir("readmissing");
        let pane_id = insert_pane(&c, &dir.to_string_lossy(), "[\"*.txt\"]", "[]", 0, "agent");
        let r = cockpitPaneFile(&json!({ "op": "read", "pane_id": pane_id, "path": "missing.txt" }), &c).unwrap();
        assert_eq!(r, json!({ "ok": true, "pane_id": pane_id, "path": "missing.txt", "exists": false, "content": null }));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn read_out_of_scope_is_rejected() {
        let c = seeded();
        let dir = temp_dir("readoos");
        std::fs::write(dir.join("a.txt"), "hi").unwrap();
        let pane_id = insert_pane(&c, &dir.to_string_lossy(), "[\"src/**\"]", "[]", 0, "agent");
        let r = cockpitPaneFile(&json!({ "op": "read", "pane_id": pane_id, "path": "a.txt" }), &c).unwrap();
        assert_eq!(r, json!({ "ok": false, "pane_id": pane_id, "path": "a.txt", "error": "out of pane scope" }));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn write_auto_apply_writes_and_posts_notes() {
        let c = seeded();
        let dir = temp_dir("writeauto");
        let pane_id = insert_pane(&c, &dir.to_string_lossy(), "[\"src/**\"]", "[\"edit\"]", 1, "agent");
        let r = cockpitPaneFile(&json!({ "op": "write", "pane_id": pane_id, "path": "src/App.jsx", "content": "hello" }), &c).unwrap();
        assert_eq!(r, json!({ "ok": true, "pane_id": pane_id, "applied": ["src/App.jsx"], "rejected": [], "mode": "auto" }));
        assert_eq!(std::fs::read_to_string(dir.join("src/App.jsx")).unwrap(), "hello");
        let notes = crate::db_json::query_rows(&c, "SELECT kind, body_md FROM cockpit_blackboard ORDER BY id", &[]).unwrap();
        let arr = notes.as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert!(arr[0]["body_md"].as_str().unwrap().contains("✅ applied → src/App.jsx (auto ⚡)"));
        assert_eq!(arr[1]["kind"], json!("handoff"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn write_no_grant_rejected_and_notes_posted() {
        let c = seeded();
        let dir = temp_dir("writenogrant");
        let pane_id = insert_pane(&c, &dir.to_string_lossy(), "[\"src/**\"]", "[]", 1, "agent");
        let r = cockpitPaneFile(&json!({ "op": "write", "pane_id": pane_id, "path": "src/App.jsx", "content": "hello" }), &c).unwrap();
        assert_eq!(r["ok"], json!(false));
        assert_eq!(r["rejected"][0]["reason"], json!("pane has no edit grant"));
        let notes = crate::db_json::query_rows(&c, "SELECT body_md FROM cockpit_blackboard", &[]).unwrap();
        assert_eq!(notes.as_array().unwrap().len(), 1);
        assert!(notes[0]["body_md"].as_str().unwrap().starts_with("⚠ rejected:"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn write_manual_stages_into_pending_and_merges_same_path() {
        let c = seeded();
        let dir = temp_dir("writemanual");
        let pane_id = insert_pane(&c, &dir.to_string_lossy(), "[\"src/**\"]", "[\"edit\"]", 0, "agent");
        let r1 = cockpitPaneFile(&json!({ "op": "write", "pane_id": pane_id, "path": "src/App.jsx", "content": "v1" }), &c).unwrap();
        assert_eq!(r1["mode"], json!("plan"));
        assert_eq!(r1["pending"].as_array().unwrap().len(), 1);
        assert!(!dir.join("src/App.jsx").exists());
        let r2 = cockpitPaneFile(&json!({ "op": "write", "pane_id": pane_id, "path": "src/App.jsx", "content": "v2" }), &c).unwrap();
        let pending = r2["pending"].as_array().unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0]["content"], json!("v2"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn unknown_op_errors() {
        let c = seeded();
        let dir = temp_dir("unknownop");
        let pane_id = insert_pane(&c, &dir.to_string_lossy(), "[]", "[]", 0, "agent");
        let err = cockpitPaneFile(&json!({ "op": "delete", "pane_id": pane_id, "path": "a.txt" }), &c).unwrap_err();
        assert_eq!(err, "cockpit-pane-file: op must be one of: read, write (got 'delete')");
        std::fs::remove_dir_all(&dir).ok();
    }
}
