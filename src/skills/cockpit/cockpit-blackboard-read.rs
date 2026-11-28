//! cockpit-blackboard-read.rs — twin of skills/cockpit/cockpit-blackboard-read.mjs (db READ lane).
//! NAMES ARE CONTRACT: `cockpitBlackboardRead` (default). EPIC-VIBE-COCKPIT P2 — the three memory
//! slices fed to a pane: shared mission brief + shared coordination log + THIS pane's private scratch.
// QUALITY: 78/100 — self-heals schema, session lookup (not-found return), coordination/context/scratch reads
use rusqlite::Connection;
use serde_json::{json, Map, Value};

fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) }

// JS Number(v): the coercions this skill can hit (number / numeric-string / null / undefined).
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
// node:sqlite binds an integral JS number as INTEGER, a fractional one as REAL — mirror that so the
// bound param + any echoed value render identically across lanes.
fn num_param(n: f64) -> Value {
    if n.is_finite() && n.fract() == 0.0 && n.abs() < 9.007199254740992e15 { json!(n as i64) } else { json!(n) }
}
// JS `${sessionId}` where sessionId is Number(...): integral → no decimal, else shortest repr.
fn js_num_str(n: f64) -> String {
    if n.is_finite() && n.fract() == 0.0 { format!("{}", n as i64) } else { n.to_string() }
}

pub fn cockpitBlackboardRead(input: &Value, db: &Connection) -> Result<Value, String> {
    // context.db guard is not reachable here (db always present); self-heal the cockpit schema.
    crate::cockpit_cockpit_schema::ensureCockpitSchema(db, &json!(""));

    let session_id = js_number(input.get("session_id"));
    if session_id == 0.0 || session_id.is_nan() {
        return Err("cockpit-blackboard-read: session_id is required".into());
    }
    let sid = num_param(session_id);

    let sess = crate::db_json::query_rows(db, "SELECT mission_md FROM cockpit_sessions WHERE id = ?", &[sid.clone()])?;
    let sess_arr = sess.as_array().cloned().unwrap_or_default();
    if sess_arr.is_empty() {
        let mut o = Map::new();
        o.insert("ok".into(), json!(false));
        o.insert("error".into(), json!(format!("session {} not found", js_num_str(session_id))));
        return Ok(Value::Object(o));
    }
    let session_row = &sess_arr[0];

    let coordination = crate::db_json::query_rows(db,
        "SELECT id, pane_id, kind, body_md, created_at FROM cockpit_blackboard WHERE session_id = ? AND pane_id IS NULL AND kind != 'context' ORDER BY created_at",
        &[sid.clone()])?;
    let context_docs = crate::db_json::query_rows(db,
        "SELECT id, body_md, created_at FROM cockpit_blackboard WHERE session_id = ? AND kind = 'context' ORDER BY created_at",
        &[sid.clone()])?;

    // paneId = pane_id != null && pane_id !== '' ? Number(pane_id) : null
    let pane_present = match input.get("pane_id") {
        None | Some(Value::Null) => false,
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    };
    let scratch = if pane_present {
        let pid = num_param(js_number(input.get("pane_id")));
        crate::db_json::query_rows(db,
            "SELECT id, kind, body_md, created_at FROM cockpit_blackboard WHERE session_id = ? AND pane_id = ? ORDER BY created_at",
            &[sid.clone(), pid])?
    } else {
        json!([])
    };

    let mission = nz(session_row.get("mission_md")).cloned().unwrap_or_else(|| json!(""));

    let mut out = Map::new();
    out.insert("ok".into(), json!(true));
    out.insert("mission_md".into(), mission);
    out.insert("coordination".into(), coordination);
    out.insert("scratch".into(), scratch);
    out.insert("context_docs".into(), context_docs);
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seeded() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        crate::cockpit_cockpit_schema::ensureCockpitSchema(&c, &json!(""));
        c.execute_batch(
            "INSERT INTO cockpit_sessions (id,name,mission_md,created_at) VALUES (1,'s1','Ship it','2025-12-01 00:00:00'); \
             INSERT INTO cockpit_sessions (id,name,created_at) VALUES (2,'s2','2025-12-01 00:00:00'); \
             INSERT INTO cockpit_blackboard (id,session_id,pane_id,kind,body_md,created_at) VALUES \
               (1,1,NULL,'note','coord one','2026-01-01 09:00:00'), \
               (2,1,NULL,'decision','coord two','2026-01-01 09:01:00'), \
               (3,1,NULL,'context','ctx doc','2026-01-01 09:02:00'), \
               (4,1,5,'scratch','scratch A','2026-01-01 09:03:00'), \
               (5,1,5,'todo','scratch B','2026-01-01 09:04:00'), \
               (6,1,7,'scratch','other pane','2026-01-01 09:05:00');",
        ).unwrap();
        c
    }

    #[test]
    fn reads_all_three_slices() {
        let c = seeded();
        let r = cockpitBlackboardRead(&json!({ "session_id": 1, "pane_id": 5 }), &c).unwrap();
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["mission_md"], json!("Ship it"));
        // coordination: pane_id IS NULL AND kind != 'context', ordered by created_at
        let coord = r["coordination"].as_array().unwrap();
        assert_eq!(coord.len(), 2);
        assert_eq!(coord[0]["id"], json!(1));
        assert_eq!(coord[0]["pane_id"], Value::Null);
        assert_eq!(coord[1]["kind"], json!("decision"));
        // scratch: only pane 5, ordered
        let scr = r["scratch"].as_array().unwrap();
        assert_eq!(scr.len(), 2);
        assert_eq!(scr[0]["id"], json!(4));
        assert_eq!(scr[1]["kind"], json!("todo"));
        // context_docs: kind='context'
        let ctx = r["context_docs"].as_array().unwrap();
        assert_eq!(ctx.len(), 1);
        assert_eq!(ctx[0]["id"], json!(3));
        // key order preserved
        let keys: Vec<&str> = r.as_object().unwrap().keys().map(|s| s.as_str()).collect();
        assert_eq!(keys, vec!["ok", "mission_md", "coordination", "scratch", "context_docs"]);
    }

    #[test]
    fn no_pane_gives_empty_scratch() {
        let c = seeded();
        let r = cockpitBlackboardRead(&json!({ "session_id": 1 }), &c).unwrap();
        assert_eq!(r["scratch"], json!([]));
        assert_eq!(r["coordination"].as_array().unwrap().len(), 2);
        // empty-string pane_id also yields empty scratch
        let r2 = cockpitBlackboardRead(&json!({ "session_id": 1, "pane_id": "" }), &c).unwrap();
        assert_eq!(r2["scratch"], json!([]));
    }

    #[test]
    fn missing_mission_is_empty_string() {
        let c = seeded();
        let r = cockpitBlackboardRead(&json!({ "session_id": 2 }), &c).unwrap();
        assert_eq!(r["mission_md"], json!(""));
        assert_eq!(r["coordination"], json!([]));
        assert_eq!(r["context_docs"], json!([]));
    }

    #[test]
    fn session_not_found_returns_error() {
        let c = seeded();
        let r = cockpitBlackboardRead(&json!({ "session_id": 999 }), &c).unwrap();
        assert_eq!(r, json!({ "ok": false, "error": "session 999 not found" }));
    }
}
