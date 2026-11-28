//! pl-session-start.rs — twin of skills/agents/pl/pl-session-start.mjs (db WRITE lane). NAMES ARE
//! CONTRACT: `plSessionStart` (default). Opens a PL session (status 'clarifying') from a raw_intent,
//! rejects decision-defer phrases, and promotes a linked backlog card to 'doing'.
// QUALITY: INSERT + defer-phrase guard (regex, guard-only) + optional card promotion (frozen clock)
use rusqlite::Connection;
use serde_json::{json, Map, Value};

// JS truthiness — used for the `if (approved_card_slug)` promotion guard.
fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}

// JS destructuring default: absent key → default; present (incl. null) → the value as-is.
fn destr(input: &Value, key: &str, default: Value) -> Value {
    match input.get(key) { None => default, Some(v) => v.clone() }
}

// DEFER_PHRASES — 1:1 port of the JS RegExp literals (case-insensitive, no lookaround/backrefs).
// Only reached on the guard-throw path, which the crossing does not conform (success-only fixtures).
const DEFER_PATTERNS: [&str; 7] = [
    r"(?i)\bappropriate(ly)?\b",
    r"(?i)\bas needed\b",
    r"(?i)\bif necessary\b",
    r"(?i)\bwhere applicable\b",
    r"(?i)\bsomehow\b",
    r"(?i)\bsome kind of\b",
    r"(?i)\betc\.?$",
];

pub fn plSessionStart(input: &Value, db: &Connection) -> Result<Value, String> {
    let started_by = destr(input, "started_by", json!("human:cli"));
    let budget_usd = destr(input, "budget_usd", json!(0.05));
    let parent_session_id = destr(input, "parent_session_id", Value::Null);
    let approved_card_slug = destr(input, "approved_card_slug", Value::Null);

    let raw_intent = input.get("raw_intent");
    let raw_intent_v = match raw_intent {
        Some(v @ Value::String(s)) if s.chars().count() >= 5 => v.clone(),
        _ => return Err("pl-session-start: raw_intent (≥5 chars) is required".into()),
    };
    let raw_intent_s = raw_intent_v.as_str().unwrap();

    if DEFER_PATTERNS.iter().copied().any(|p| regex::Regex::new(p).unwrap().is_match(raw_intent_s)) {
        return Err("pl-session-start: raw_intent contains decision-defer phrase(s); resolve them upfront".into());
    }

    let (_changes, rowid) = crate::db_json::run_write(
        db,
        "INSERT INTO pl_sessions (raw_intent, status, started_by, budget_usd, parent_session_id, approved_card_slug) VALUES (?, 'clarifying', ?, ?, ?, ?)",
        &[raw_intent_v.clone(), started_by.clone(), budget_usd.clone(), parent_session_id.clone(), approved_card_slug.clone()],
    )?;

    // FEAT-PROMOTE-DOING: backlog→doing the moment a session opens. datetime('now') is frozen to the
    // canonical instant by the JS lane; the twin writes that literal. try/catch → swallow (missing row).
    if truthy(Some(&approved_card_slug)) {
        let _ = crate::db_json::run_write(
            db,
            &format!("UPDATE cards SET status='doing', updated_at={} WHERE slug = ? AND status = 'backlog'", crate::clock::now_sql()),
            &[approved_card_slug.clone()],
        );
    }

    let mut out = Map::new();
    out.insert("session_id".into(), json!(rowid));
    out.insert("status".into(), json!("clarifying"));
    out.insert("raw_intent".into(), raw_intent_v);
    out.insert("started_by".into(), started_by);
    out.insert("budget_usd".into(), budget_usd);
    out.insert("parent_session_id".into(), parent_session_id);
    out.insert("approved_card_slug".into(), approved_card_slug);
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn db0() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE pl_sessions (id INTEGER PRIMARY KEY, raw_intent TEXT, status TEXT, started_by TEXT, budget_usd REAL, parent_session_id INTEGER, approved_card_slug TEXT); \
             CREATE TABLE cards (slug TEXT PRIMARY KEY, status TEXT, updated_at TEXT);",
        ).unwrap();
        c
    }

    #[test]
    fn opens_with_defaults() {
        let c = db0();
        let r = plSessionStart(&json!({ "raw_intent": "build the auth flow" }), &c).unwrap();
        assert_eq!(r["session_id"], json!(1));
        assert_eq!(r["status"], json!("clarifying"));
        assert_eq!(r["started_by"], json!("human:cli"));
        assert_eq!(r["budget_usd"], json!(0.05));
        assert_eq!(r["parent_session_id"], Value::Null);
        assert_eq!(r["approved_card_slug"], Value::Null);
        // key order is the JS object-literal order
        let keys: Vec<&String> = r.as_object().unwrap().keys().collect();
        assert_eq!(keys, ["session_id", "status", "raw_intent", "started_by", "budget_usd", "parent_session_id", "approved_card_slug"]);
    }

    #[test]
    fn promotes_backlog_card() {
        let c = db0();
        c.execute("INSERT INTO cards (slug, status, updated_at) VALUES ('FEAT-X', 'backlog', '2025-12-01 00:00:00')", []).unwrap();
        let r = plSessionStart(&json!({ "raw_intent": "ship the billing page", "started_by": "human:dani", "budget_usd": 0.25, "parent_session_id": 7, "approved_card_slug": "FEAT-X" }), &c).unwrap();
        assert_eq!(r["approved_card_slug"], json!("FEAT-X"));
        assert_eq!(r["parent_session_id"], json!(7));
        let status: String = c.query_row("SELECT status FROM cards WHERE slug='FEAT-X'", [], |row| row.get(0)).unwrap();
        let updated: String = c.query_row("SELECT updated_at FROM cards WHERE slug='FEAT-X'", [], |row| row.get(0)).unwrap();
        assert_eq!(status, "doing");
        assert_eq!(updated, "2026-01-01 00:00:00");
    }

    #[test]
    fn leaves_non_backlog_card() {
        let c = db0();
        c.execute("INSERT INTO cards (slug, status, updated_at) VALUES ('FEAT-Y', 'doing', '2025-12-15 00:00:00')", []).unwrap();
        plSessionStart(&json!({ "raw_intent": "extend the report view", "approved_card_slug": "FEAT-Y" }), &c).unwrap();
        let updated: String = c.query_row("SELECT updated_at FROM cards WHERE slug='FEAT-Y'", [], |row| row.get(0)).unwrap();
        assert_eq!(updated, "2025-12-15 00:00:00"); // WHERE status='backlog' matched nothing
    }

    #[test]
    fn rejects_short_and_defer() {
        let c = db0();
        assert!(plSessionStart(&json!({ "raw_intent": "hi" }), &c).is_err());
        assert!(plSessionStart(&json!({}), &c).is_err());
        assert!(plSessionStart(&json!({ "raw_intent": "resize the panel as needed" }), &c).is_err());
        assert!(plSessionStart(&json!({ "raw_intent": "handle errors etc" }), &c).is_err());
    }
}
