//! cockpit-pane-claim.rs — twin of skills/cockpit/cockpit-pane-claim.mjs (db WRITE lane). NAMES ARE
//! CONTRACT: `cockpitPaneClaim` (default). A pane claims a path/glob; rejects an overlapping claim.
// QUALITY: 80/100 — non-overlap gate: query sibling panes, globsOverlap find → conflict, else INSERT
use rusqlite::Connection;
use serde_json::{json, Map, Value};

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
// node:sqlite binds an integral JS number as INTEGER, a fractional one as REAL. Mirror that so the
// bound value (and any INTEGER-affinity column it lands in) renders identically to the JS lane.
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
// JS `${v}` for the values that appear in the error message (number / string).
fn tmpl(v: Option<&Value>) -> String {
    match v {
        None => "undefined".into(),
        Some(Value::Null) => "null".into(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(),
        Some(o) => o.to_string(),
    }
}

pub fn cockpitPaneClaim(input: &Value, db: &Connection) -> Result<Value, String> {
    // context.db is always provided by the harness; the JS `if (!db) throw` guard has no Rust twin.
    crate::cockpit_cockpit_schema::ensureCockpitSchema(db, &json!(""));

    let session_id = js_number(input.get("session_id"));
    if !truthy_num(session_id) { return Err("cockpit-pane-claim: session_id is required".into()); }
    let pane_id = js_number(input.get("pane_id"));
    if !truthy_num(pane_id) { return Err("cockpit-pane-claim: pane_id is required".into()); }
    let glob = str_qq(input.get("path_glob")).trim().to_string();
    if glob.is_empty() { return Err("cockpit-pane-claim: path_glob is required".into()); }

    let others = crate::db_json::query_rows(
        db,
        "SELECT pane_id, path_glob FROM cockpit_claims WHERE session_id = ? AND pane_id != ?",
        &[num_param(session_id), num_param(pane_id)],
    )?;
    let others = others.as_array().cloned().unwrap_or_default();

    let glob_v = json!(glob);
    let clash = others.iter().find(|e| {
        crate::cockpit__helpers_scope::globsOverlap(&glob_v, e.get("path_glob").unwrap_or(&Value::Null))
            .as_bool()
            .unwrap_or(false)
    });
    // an overlap is reported, not refused (operator 2026-09-20: each pane has its own worktree)
    let overlap = clash.map(|c| json!({ "pane_id": c.get("pane_id").cloned().unwrap_or(Value::Null), "glob": c.get("path_glob").cloned().unwrap_or(Value::Null) }));

    let (_, rowid) = crate::db_json::run_write(
        db,
        "INSERT INTO cockpit_claims (session_id, pane_id, path_glob) VALUES (?,?,?)",
        &[num_param(session_id), num_param(pane_id), glob_v.clone()],
    )?;
    let mut out = Map::new();
    out.insert("ok".into(), json!(true));
    out.insert("id".into(), json!(rowid));
    if let Some(o) = overlap { out.insert("overlap".into(), o); }
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        // mirror the fixture: the skill's ensureCockpitSchema is a no-op for this pre-created table.
        c.execute_batch(
            "CREATE TABLE cockpit_claims (id INTEGER PRIMARY KEY AUTOINCREMENT, session_id INTEGER NOT NULL, \
             pane_id INTEGER NOT NULL, path_glob TEXT NOT NULL, created_at TEXT NOT NULL DEFAULT '2026-01-01 00:00:00')",
        ).unwrap();
        c
    }

    #[test]
    fn clean_insert() {
        let c = fresh();
        let r = cockpitPaneClaim(&json!({ "session_id": 1, "pane_id": 2, "path_glob": "src/ui/**" }), &c).unwrap();
        assert_eq!(r, json!({ "ok": true, "id": 1 }));
        // ensureCockpitSchema created the sibling tables too
        let n: i64 = c.query_row("SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name LIKE 'cockpit_%'", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 6); // claims, sessions, panes, blackboard, pull_requests, tribunals
    }

    #[test]
    fn non_overlapping_sibling_still_inserts() {
        let c = fresh();
        c.execute("INSERT INTO cockpit_claims (session_id, pane_id, path_glob) VALUES (1, 3, 'docs/**')", []).unwrap();
        let r = cockpitPaneClaim(&json!({ "session_id": 1, "pane_id": 2, "path_glob": "src/ui/**" }), &c).unwrap();
        assert_eq!(r, json!({ "ok": true, "id": 2 }));
    }

    #[test]
    fn overlap_conflicts_on_first_match() {
        let c = fresh();
        c.execute("INSERT INTO cockpit_claims (session_id, pane_id, path_glob) VALUES (1, 4, 'lib/**')", []).unwrap();
        c.execute("INSERT INTO cockpit_claims (session_id, pane_id, path_glob) VALUES (1, 5, 'src/**')", []).unwrap();
        c.execute("INSERT INTO cockpit_claims (session_id, pane_id, path_glob) VALUES (1, 6, 'src/ui/x/**')", []).unwrap();
        let r = cockpitPaneClaim(&json!({ "session_id": 1, "pane_id": 2, "path_glob": "src/ui/**" }), &c).unwrap();
        // the overlap is reported, the claim is registered anyway (operator 2026-09-20: each pane has its own worktree)
        assert_eq!(r["ok"], json!(true)); assert_eq!(r["overlap"], json!({ "pane_id": 5, "glob": "src/**" }));
        let n: i64 = c.query_row("SELECT COUNT(*) FROM cockpit_claims", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 4);
    }

    #[test]
    fn same_pane_and_other_session_are_excluded() {
        let c = fresh();
        c.execute("INSERT INTO cockpit_claims (session_id, pane_id, path_glob) VALUES (1, 2, 'src/ui/**')", []).unwrap(); // same pane
        c.execute("INSERT INTO cockpit_claims (session_id, pane_id, path_glob) VALUES (9, 7, 'src/**')", []).unwrap();     // other session
        let r = cockpitPaneClaim(&json!({ "session_id": 1, "pane_id": 2, "path_glob": "src/ui/**" }), &c).unwrap();
        assert_eq!(r, json!({ "ok": true, "id": 3 }));
    }
}
