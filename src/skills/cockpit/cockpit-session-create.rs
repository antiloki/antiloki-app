//! cockpit-session-create.rs — twin of skills/cockpit/cockpit-session-create.mjs (db WRITE lane).
//! NAMES ARE CONTRACT: `cockpitSessionCreate` (default). EPIC-VIBE-COCKPIT P0 — create a vibe session
//! (the container N panes attach to), recording base branch/sha so drift is detectable on integrate.
// QUALITY: reuses the converted cockpit-schema twin for DDL; INSERT + rowid SELECT; frozen datetime.
use rusqlite::Connection;
use serde_json::{json, Value};

// JS `String(v ?? '')`
fn str_qq(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(),
        Some(o) => o.to_string(),
    }
}

pub fn cockpitSessionCreate(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS: String(input?.name ?? '').trim(); throw if empty (guard — success-only fixture)
    let name = str_qq(input.get("name"));
    let name = name.trim();
    if name.is_empty() {
        return Err("cockpit-session-create: name is required".into());
    }
    // self-heal the cockpit schema via the converted twin. Its created_at DEFAULT is the UNFROZEN
    // datetime('now'), but we never rely on it: the INSERT below writes the frozen literal into
    // created_at explicitly, so the stored row is byte-identical to the JS lane (whose db proxy
    // rewrites the DEFAULT to the same frozen instant). Only cockpit_sessions gets a row; the other
    // six tables stay empty, so their DEFAULT text never fires and never appears in db_after.
    let _ = crate::cockpit_cockpit_schema::ensureCockpitSchema(db, &json!(""));

    let mission: String = str_qq(input.get("mission_md")).chars().take(20000).collect();
    let base_branch: String = str_qq(input.get("base_branch")).chars().take(200).collect();
    let base_sha: String = str_qq(input.get("base_sha")).chars().take(80).collect();
    // JS: input?.mode === 'monitor' ? 'monitor' : 'steer'
    let mode = if input.get("mode").and_then(|v| v.as_str()) == Some("monitor") { "monitor" } else { "steer" };

    // JS INSERT sets updated_at = datetime('now') and relies on the created_at DEFAULT; the harness
    // freezes both to 2026-01-01 00:00:00. We write the frozen literal into BOTH columns explicitly.
    db.execute(
        &format!("INSERT INTO cockpit_sessions (name, mission_md, base_branch, base_sha, mode, created_at, updated_at) \
         VALUES (?,?,?,?,?, {}, {})", crate::clock::now_sql(), crate::clock::now_sql()),
        rusqlite::params![name, mission, base_branch, base_sha, mode],
    )
    .map_err(|e| e.to_string())?;

    let id = db.last_insert_rowid();
    let rows = crate::db_json::query_rows(
        db,
        "SELECT id, name, status, mode, base_branch, base_sha, mission_md, created_at FROM cockpit_sessions WHERE id = ?",
        &[json!(id)],
    )?;
    let session = rows.as_array().and_then(|a| a.first()).cloned().unwrap_or(Value::Null);
    Ok(json!({ "ok": true, "session": session }))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn creates_session() {
        let c = Connection::open_in_memory().unwrap();
        let r = cockpitSessionCreate(
            &json!({ "name": "  vibe-1  ", "mission_md": "ship it", "base_branch": "main", "base_sha": "abc123", "mode": "monitor" }),
            &c,
        )
        .unwrap();
        assert_eq!(r["ok"], json!(true));
        let s = &r["session"];
        assert_eq!(s["id"], json!(1));
        assert_eq!(s["name"], json!("vibe-1")); // trimmed
        assert_eq!(s["status"], json!("active"));
        assert_eq!(s["mode"], json!("monitor"));
        assert_eq!(s["base_branch"], json!("main"));
        assert_eq!(s["base_sha"], json!("abc123"));
        assert_eq!(s["mission_md"], json!("ship it"));
        assert_eq!(s["created_at"], json!("2026-01-01 00:00:00"));

        // absent mode → 'steer'; rowid increments
        let r2 = cockpitSessionCreate(&json!({ "name": "vibe-2" }), &c).unwrap();
        assert_eq!(r2["session"]["id"], json!(2));
        assert_eq!(r2["session"]["mode"], json!("steer"));
        assert_eq!(r2["session"]["base_branch"], json!(""));
        assert_eq!(r2["session"]["mission_md"], json!(""));

        // present-but-not-'monitor' mode → 'steer'
        let r3 = cockpitSessionCreate(&json!({ "name": "vibe-3", "mode": "whatever" }), &c).unwrap();
        assert_eq!(r3["session"]["mode"], json!("steer"));

        // empty name → Err (guard)
        assert!(cockpitSessionCreate(&json!({ "name": "   " }), &c).is_err());

        // db state: board_json present (via ALTER) = null; updated_at frozen; all cockpit tables exist
        let rows = crate::db_json::query_rows(&c, "SELECT board_json, updated_at FROM cockpit_sessions ORDER BY id", &[]).unwrap();
        assert_eq!(rows.as_array().unwrap().len(), 3);
        assert_eq!(rows[0]["board_json"], Value::Null);
        assert_eq!(rows[0]["updated_at"], json!("2026-01-01 00:00:00"));
        let names = crate::db_json::query_rows(&c, "SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name", &[]).unwrap();
        let got: Vec<String> = names.as_array().unwrap().iter().map(|r| r["name"].as_str().unwrap().to_string()).collect();
        assert_eq!(got, vec!["cockpit_blackboard", "cockpit_claims", "cockpit_panes", "cockpit_pull_requests", "cockpit_sessions", "cockpit_tribunals", "ultra_plans"]);
    }
}
