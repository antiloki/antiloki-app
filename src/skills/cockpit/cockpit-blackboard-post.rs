//! cockpit-blackboard-post.rs — twin of skills/cockpit/cockpit-blackboard-post.mjs (db WRITE lane).
//! NAMES ARE CONTRACT: `cockpitBlackboardPost` (default). Append/update/delete the partitioned
//! blackboard; op:'mission'/'board' persist session-level UI state. Success-path only (guards throw).
use rusqlite::Connection;
use serde_json::{json, Map, Value};

// The kinds an entry may carry; anything else falls back to 'note'. Mirrors the JS Set.
const KINDS: &[&str] = &[
    "claim", "note", "handoff", "scratch", "decision", "context", "todo", "kanban", "sketch", "plan",
    "swarmq", "shape", "baseplan",
    // THE STUDIO TOOLS' OWN KINDS (2026-10-11: "when trying to add a new issue on the issues widget it is not working"). A kind that is not in this list is stored as a plain 'note', so the Issues widget's rows came back as notes and it
    // saw nothing. These are what the tools write: an issue, the Issues list/board choice, a Brainstorm message and its chosen options. (The front end also recognises them by shape, for a serve that has not been restarted.)
    "issue", "pref", "brainstorm", "brainstorm-chips",
];

// JS `String(v ?? '')`: null/undefined → "", else the JS string of v.
fn str_qq(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(),
        Some(o) => o.to_string(),
    }
}

// JS `Number(v)`.
fn js_number(v: Option<&Value>) -> f64 {
    match v {
        None => f64::NAN,
        Some(Value::Null) => 0.0,
        Some(Value::Bool(b)) => if *b { 1.0 } else { 0.0 },
        Some(Value::Number(n)) => n.as_f64().unwrap_or(f64::NAN),
        Some(Value::String(s)) => {
            let t = s.trim();
            if t.is_empty() { 0.0 } else { t.parse().unwrap_or(f64::NAN) }
        }
        Some(_) => f64::NAN,
    }
}

// JS Number → JSON: an integer-valued number renders without a decimal point (5, not 5.0).
fn int_json(f: f64) -> Value {
    if f.is_finite() && f.fract() == 0.0 && f.abs() < 9.007199254740992e15 { json!(f as i64) } else { json!(f) }
}

// JS String.prototype.slice(0, n) over BMP-safe fixtures (UTF-16 code units ≈ chars).
fn take(s: String, n: usize) -> String { s.chars().take(n).collect() }

// JS `!x` for x = Number(...): falsy at 0, -0, NaN. True here means the guard throws.
fn falsy_num(f: f64) -> bool { f == 0.0 || f.is_nan() }

pub fn cockpitBlackboardPost(input: &Value, db: &Connection) -> Result<Value, String> {
    crate::cockpit_cockpit_schema::ensureCockpitSchema(db, &json!(""));

    let op = input.get("op").filter(|v| !v.is_null()).and_then(|v| v.as_str()).unwrap_or("post");

    if op == "mission" {
        let sid = js_number(input.get("session_id"));
        if falsy_num(sid) { return Err("cockpit-blackboard-post: session_id is required".into()); }
        let mission = take(str_qq(input.get("mission_md")), 20000);
        crate::db_json::run_write(db, "UPDATE cockpit_sessions SET mission_md = ? WHERE id = ?", &[json!(mission), json!(sid as i64)])?;
        let mut out = Map::new();
        out.insert("ok".into(), json!(true));
        out.insert("session_id".into(), int_json(sid));
        return Ok(Value::Object(out));
    }

    if op == "board" {
        let sid = js_number(input.get("session_id"));
        if falsy_num(sid) { return Err("cockpit-blackboard-post: session_id is required".into()); }
        // JS: typeof board_json === 'string' ? board_json : JSON.stringify(board ?? [])
        let board = match input.get("board_json") {
            Some(Value::String(s)) => s.clone(),
            _ => {
                let b = input.get("board").filter(|v| !v.is_null()).cloned().unwrap_or(json!([]));
                serde_json::to_string(&b).unwrap_or_else(|_| "null".into())
            }
        };
        let board = take(board, 16000);
        crate::db_json::run_write(db, "UPDATE cockpit_sessions SET board_json = ? WHERE id = ?", &[json!(board), json!(sid as i64)])?;
        let mut out = Map::new();
        out.insert("ok".into(), json!(true));
        out.insert("session_id".into(), int_json(sid));
        return Ok(Value::Object(out));
    }

    if op == "update" {
        let id = js_number(input.get("id"));
        if falsy_num(id) { return Err("cockpit-blackboard-post: id is required".into()); }
        let body = take(str_qq(input.get("body_md")), 20000);
        if body.is_empty() { return Err("cockpit-blackboard-post: body_md is required".into()); }
        crate::db_json::run_write(db, "UPDATE cockpit_blackboard SET body_md = ? WHERE id = ?", &[json!(body), json!(id as i64)])?;
        let mut out = Map::new();
        out.insert("ok".into(), json!(true));
        out.insert("id".into(), int_json(id));
        return Ok(Value::Object(out));
    }

    if op == "delete" {
        let id = js_number(input.get("id"));
        if falsy_num(id) { return Err("cockpit-blackboard-post: id is required".into()); }
        crate::db_json::run_write(db, "DELETE FROM cockpit_blackboard WHERE id = ?", &[json!(id as i64)])?;
        let mut out = Map::new();
        out.insert("ok".into(), json!(true));
        out.insert("id".into(), int_json(id));
        out.insert("deleted".into(), json!(true));
        return Ok(Value::Object(out));
    }

    // op = 'post' (default) — append a blackboard entry
    let session_id = js_number(input.get("session_id"));
    if falsy_num(session_id) { return Err("cockpit-blackboard-post: session_id is required".into()); }
    let body = take(str_qq(input.get("body_md")), 20000);
    if body.is_empty() { return Err("cockpit-blackboard-post: body_md is required".into()); }
    let kind = match input.get("kind").and_then(|v| v.as_str()) {
        Some(k) if KINDS.contains(&k) => k,
        _ => "note",
    };
    // JS: pane_id != null && pane_id !== '' ? Number(pane_id) : null
    let pane_id: Value = {
        let pv = input.get("pane_id");
        let not_null = pv.map_or(false, |v| !v.is_null());
        let not_empty = pv.map_or(true, |v| v.as_str() != Some(""));
        if not_null && not_empty { json!(js_number(pv) as i64) } else { Value::Null }
    };

    let (_changes, rowid) = crate::db_json::run_write(
        db,
        "INSERT INTO cockpit_blackboard (session_id, pane_id, kind, body_md) VALUES (?,?,?,?)",
        &[json!(session_id as i64), pane_id, json!(kind), json!(body)],
    )?;
    let mut out = Map::new();
    out.insert("ok".into(), json!(true));
    out.insert("id".into(), json!(rowid));
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mkdb() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        // The real skill self-heals the schema via ensureCockpitSchema; here we mirror the fixture
        // lane's simplified tables (the two the skill actually touches, no datetime DEFAULTs).
        c.execute_batch(
            "CREATE TABLE cockpit_sessions (id INTEGER PRIMARY KEY, mission_md TEXT, board_json TEXT); \
             CREATE TABLE cockpit_blackboard (id INTEGER PRIMARY KEY, session_id INTEGER, pane_id INTEGER, kind TEXT, body_md TEXT);",
        )
        .unwrap();
        c
    }

    #[test]
    fn the_studio_tools_own_kinds_are_kept_and_an_unknown_kind_is_still_a_note() {
        let c = mkdb();
        for (i, k) in ["issue", "pref", "brainstorm", "brainstorm-chips", "todo", "sketch"].iter().enumerate() {
            cockpitBlackboardPost(&json!({ "session_id": 1, "pane_id": 3, "kind": k, "body_md": "{}" }), &c).unwrap();
            let row = crate::db_json::query_rows(&c, "SELECT kind FROM cockpit_blackboard WHERE id = ?", &[json!(i as i64 + 1)]).unwrap();
            assert_eq!(row[0]["kind"], json!(k), "{k} must be stored as itself, not as a note");
        }
        cockpitBlackboardPost(&json!({ "session_id": 1, "pane_id": 3, "kind": "not-a-kind", "body_md": "x" }), &c).unwrap();
        let row = crate::db_json::query_rows(&c, "SELECT kind FROM cockpit_blackboard WHERE id = 7", &[]).unwrap();
        assert_eq!(row[0]["kind"], json!("note"));
    }

    #[test]
    fn posts_updates_deletes() {
        // post: valid kind + explicit pane_id → INSERT, returns new id
        let c = mkdb();
        let r = cockpitBlackboardPost(&json!({ "session_id": 7, "pane_id": 2, "kind": "claim", "body_md": "hello" }), &c).unwrap();
        assert_eq!(r, json!({ "ok": true, "id": 1 }));
        let row = crate::db_json::query_rows(&c, "SELECT session_id, pane_id, kind, body_md FROM cockpit_blackboard WHERE id = 1", &[]).unwrap();
        assert_eq!(row[0], json!({ "session_id": 7, "pane_id": 2, "kind": "claim", "body_md": "hello" }));

        // post: unknown kind → 'note'; empty pane_id → NULL
        let c = mkdb();
        cockpitBlackboardPost(&json!({ "op": "post", "session_id": 3, "pane_id": "", "kind": "bogus", "body_md": "x" }), &c).unwrap();
        let row = crate::db_json::query_rows(&c, "SELECT pane_id, kind FROM cockpit_blackboard WHERE id = 1", &[]).unwrap();
        assert_eq!(row[0], json!({ "pane_id": null, "kind": "note" }));

        // mission: UPDATE mission_md, return { ok, session_id }
        let c = mkdb();
        c.execute("INSERT INTO cockpit_sessions (id) VALUES (1)", []).unwrap();
        let r = cockpitBlackboardPost(&json!({ "op": "mission", "session_id": 1, "mission_md": "go" }), &c).unwrap();
        assert_eq!(r, json!({ "ok": true, "session_id": 1 }));
        let row = crate::db_json::query_rows(&c, "SELECT mission_md FROM cockpit_sessions WHERE id = 1", &[]).unwrap();
        assert_eq!(row[0]["mission_md"], json!("go"));

        // board: array branch → JSON.stringify
        let c = mkdb();
        c.execute("INSERT INTO cockpit_sessions (id) VALUES (2)", []).unwrap();
        let r = cockpitBlackboardPost(&json!({ "op": "board", "session_id": 2, "board": [{ "id": "g2", "x": 10 }] }), &c).unwrap();
        assert_eq!(r, json!({ "ok": true, "session_id": 2 }));
        let row = crate::db_json::query_rows(&c, "SELECT board_json FROM cockpit_sessions WHERE id = 2", &[]).unwrap();
        assert_eq!(row[0]["board_json"], json!("[{\"id\":\"g2\",\"x\":10}]"));

        // board: string branch passes through; absent board → "[]"
        let c = mkdb();
        c.execute("INSERT INTO cockpit_sessions (id) VALUES (3)", []).unwrap();
        cockpitBlackboardPost(&json!({ "op": "board", "session_id": 3, "board_json": "[{\"id\":\"g1\"}]" }), &c).unwrap();
        let row = crate::db_json::query_rows(&c, "SELECT board_json FROM cockpit_sessions WHERE id = 3", &[]).unwrap();
        assert_eq!(row[0]["board_json"], json!("[{\"id\":\"g1\"}]"));

        // update: UPDATE body_md
        let c = mkdb();
        c.execute("INSERT INTO cockpit_blackboard (id, session_id, kind, body_md) VALUES (1, 7, 'note', 'old')", []).unwrap();
        let r = cockpitBlackboardPost(&json!({ "op": "update", "id": 1, "body_md": "new" }), &c).unwrap();
        assert_eq!(r, json!({ "ok": true, "id": 1 }));
        let row = crate::db_json::query_rows(&c, "SELECT body_md FROM cockpit_blackboard WHERE id = 1", &[]).unwrap();
        assert_eq!(row[0]["body_md"], json!("new"));

        // delete: DELETE the row
        let c = mkdb();
        c.execute("INSERT INTO cockpit_blackboard (id, session_id, kind, body_md) VALUES (1, 7, 'note', 'bye')", []).unwrap();
        let r = cockpitBlackboardPost(&json!({ "op": "delete", "id": 1 }), &c).unwrap();
        assert_eq!(r, json!({ "ok": true, "id": 1, "deleted": true }));
        let rows = crate::db_json::query_rows(&c, "SELECT * FROM cockpit_blackboard", &[]).unwrap();
        assert_eq!(rows.as_array().unwrap().len(), 0);
    }
}
