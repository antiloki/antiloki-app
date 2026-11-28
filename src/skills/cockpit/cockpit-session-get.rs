//! cockpit-session-get.rs — twin of skills/cockpit/cockpit-session-get.mjs (db READ lane). NAMES
//! ARE CONTRACT: `cockpitSessionGet` (async default → sync twin). EPIC-VIBE-COCKPIT P0: read a vibe
//! session (session + panes + shared coordination + pane notes + file claims), else list recent.
// QUALITY: 80/100 — Number(id) list/read fork; panes spread + scope/grants JSON-parse with default
use rusqlite::Connection;
use serde_json::{json, Map, Value};

// = JS Number(v) for the id coercion (only the paths that appear: absent/null/bool/number/string).
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

// JS `${id}` where id is a Number: an integer-valued number renders with no decimal point
// (fixtures keep ids integral; the fractional fallback is Rust's default, unexercised).
fn num_tmpl(n: f64) -> String {
    if n.is_finite() && n.fract() == 0.0 && n.abs() < 9_007_199_254_740_992.0 { format!("{}", n as i64) } else { n.to_string() }
}

// = JS `parse = (s, d) => { try { return JSON.parse(s ?? 'null') ?? d; } catch { return d; } }`.
// s is a TEXT column (String, or defensively Null → 'null'). A parsed `null`, invalid JSON, or an
// absent value all yield the default d; any non-null parse result is returned as-is.
fn parse(s: Option<&Value>, d: Value) -> Value {
    let src = match s {
        None | Some(Value::Null) => "null".to_string(),
        Some(Value::String(t)) => t.clone(),
        Some(other) => other.to_string(),
    };
    match serde_json::from_str::<Value>(&src) {
        Ok(v) if !v.is_null() => v,
        _ => d,
    }
}

// JS binds Number(id): an integer-valued number binds as SQLite INTEGER, otherwise REAL. Match that
// bind type (numeric affinity makes the WHERE compare equal either way, but we stay faithful).
fn id_param(n: f64) -> Value {
    if n.is_finite() && n.fract() == 0.0 && n >= i64::MIN as f64 && n <= i64::MAX as f64 { json!(n as i64) } else { json!(n) }
}

pub fn cockpitSessionGet(input: &Value, db: &Connection) -> Result<Value, String> {
    crate::cockpit_cockpit_schema::ensureCockpitSchema(db, &json!(""));

    let id = js_number(input.get("id"));
    if id == 0.0 || id.is_nan() {
        let sessions = crate::db_json::query_rows(
            db,
            "SELECT id, name, status, mode, created_at FROM cockpit_sessions ORDER BY id DESC LIMIT 50",
            &[],
        )?;
        let mut out = Map::new();
        out.insert("ok".into(), json!(true));
        out.insert("sessions".into(), sessions);
        return Ok(Value::Object(out));
    }

    let p = id_param(id);
    let session_rows = crate::db_json::query_rows(db, "SELECT * FROM cockpit_sessions WHERE id = ?", &[p.clone()])?;
    let session = match session_rows.as_array().and_then(|a| a.first()) {
        Some(s) => s.clone(),
        None => {
            let mut out = Map::new();
            out.insert("ok".into(), json!(false));
            out.insert("error".into(), json!(format!("session {} not found", num_tmpl(id))));
            return Ok(Value::Object(out));
        }
    };

    // panes: spread each row, then append scope/grants parsed from their *_json columns (default []).
    let panes_raw = crate::db_json::query_rows(db, "SELECT * FROM cockpit_panes WHERE session_id = ? ORDER BY idx", &[p.clone()])?;
    let panes: Vec<Value> = panes_raw
        .as_array()
        .map(|a| {
            a.iter()
                .map(|pane| {
                    let mut m = pane.as_object().cloned().unwrap_or_default();
                    let scope = parse(m.get("scope_json"), json!([]));
                    let grants = parse(m.get("grants_json"), json!([]));
                    m.insert("scope".into(), scope);
                    m.insert("grants".into(), grants);
                    Value::Object(m)
                })
                .collect()
        })
        .unwrap_or_default();

    let coordination = crate::db_json::query_rows(
        db,
        "SELECT id, pane_id, kind, body_md, created_at FROM cockpit_blackboard WHERE session_id = ? AND pane_id IS NULL ORDER BY created_at DESC LIMIT 100",
        &[p.clone()],
    )?;
    // pane-scoped notes (each pane's chat thread), oldest-first for rendering
    let notes = crate::db_json::query_rows(
        db,
        "SELECT id, pane_id, kind, body_md, created_at FROM cockpit_blackboard WHERE session_id = ? AND pane_id IS NOT NULL ORDER BY created_at",
        &[p.clone()],
    )?;
    let claims = crate::db_json::query_rows(db, "SELECT id, pane_id, path_glob FROM cockpit_claims WHERE session_id = ?", &[p.clone()])?;

    let mut out = Map::new();
    out.insert("ok".into(), json!(true));
    out.insert("session".into(), session);
    out.insert("panes".into(), json!(panes));
    out.insert("coordination".into(), coordination);
    out.insert("notes".into(), notes);
    out.insert("claims".into(), claims);
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seeded() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        crate::cockpit_cockpit_schema::ensureCockpitSchema(&c, &json!(""));
        c.execute_batch(
            "INSERT INTO cockpit_sessions (id,name,status,mode,created_at) VALUES (1,'alpha','active','steer','2026-01-01 00:00:01'),(2,'beta','merged','monitor','2026-01-01 00:00:02');
             INSERT INTO cockpit_panes (id,session_id,idx,scope_json,grants_json) VALUES (10,1,0,'[\"src/ui/**\"]','[\"card.add\"]'),(11,1,1,'null','bad');
             INSERT INTO cockpit_blackboard (id,session_id,pane_id,kind,body_md,created_at) VALUES (1,1,NULL,'note','brief','2026-01-01 00:00:10'),(2,1,NULL,'decision','x','2026-01-01 00:00:20'),(3,1,10,'scratch','u','2026-01-01 00:00:05'),(4,1,10,'note','a','2026-01-01 00:00:06');
             INSERT INTO cockpit_claims (id,session_id,pane_id,path_glob) VALUES (1,1,10,'src/ui/**'),(2,1,11,'src/lib/**');",
        )
        .unwrap();
        c
    }

    #[test]
    fn lists_when_no_id() {
        let c = seeded();
        let r = cockpitSessionGet(&json!({}), &c).unwrap();
        assert_eq!(r["ok"], json!(true));
        let sess = r["sessions"].as_array().unwrap();
        assert_eq!(sess.len(), 2);
        assert_eq!(sess[0]["id"], json!(2)); // ORDER BY id DESC
        assert_eq!(sess[1]["id"], json!(1));
        // id=0 and non-numeric strings coerce to falsy → still list mode
        assert_eq!(cockpitSessionGet(&json!({"id": 0}), &c).unwrap()["sessions"].as_array().unwrap().len(), 2);
        assert_eq!(cockpitSessionGet(&json!({"id": "abc"}), &c).unwrap()["sessions"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn reads_full_session() {
        let c = seeded();
        let r = cockpitSessionGet(&json!({"id": 1}), &c).unwrap();
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["session"]["name"], json!("alpha"));
        let panes = r["panes"].as_array().unwrap();
        assert_eq!(panes.len(), 2);
        assert_eq!(panes[0]["scope"], json!(["src/ui/**"]));
        assert_eq!(panes[0]["grants"], json!(["card.add"]));
        assert_eq!(panes[1]["scope"], json!([])); // 'null' → parsed null → default []
        assert_eq!(panes[1]["grants"], json!([])); // 'bad' → parse throws → default []
        let coord: Vec<i64> = r["coordination"].as_array().unwrap().iter().map(|c| c["id"].as_i64().unwrap()).collect();
        assert_eq!(coord, vec![2, 1]); // created_at DESC
        let notes: Vec<i64> = r["notes"].as_array().unwrap().iter().map(|n| n["id"].as_i64().unwrap()).collect();
        assert_eq!(notes, vec![3, 4]); // created_at ASC, pane-scoped
        assert_eq!(r["claims"].as_array().unwrap().len(), 2);
        let keys: Vec<&str> = r.as_object().unwrap().keys().map(|k| k.as_str()).collect();
        assert_eq!(keys, vec!["ok", "session", "panes", "coordination", "notes", "claims"]);
    }

    #[test]
    fn not_found() {
        let c = seeded();
        let r = cockpitSessionGet(&json!({"id": 999}), &c).unwrap();
        assert_eq!(r, json!({"ok": false, "error": "session 999 not found"}));
    }
}
