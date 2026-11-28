//! card-deps-check.rs — twin of skills/entities/card-deps-check.mjs. THE DB LANE PILOT.
//! NAMES ARE CONTRACT: `cardDepsCheck` (named default). The JS reads context.db; here the db is
//! a rusqlite Connection seeded identically on both lanes, so the same queries return the same
//! rows — that equality is the whole point of the db lane.
// QUALITY: 85/100 — faithful clone of the queries + blocker logic; rusqlite is mechanism, not name
use rusqlite::{Connection, OptionalExtension};
use serde_json::{json, Value};

pub fn cardDepsCheck(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS throws err.contextMissing when no db — the db lane always injects one, so that path is
    // unreachable here (and unrepresentable: we hold a &Connection).
    let slug = match input.get("slug").and_then(Value::as_str) {
        Some(s) if !s.is_empty() => s,
        _ => return Err("card-deps-check: slug is required".into()),
    };

    let status: Option<String> = db
        .prepare("SELECT status FROM cards WHERE slug = ?1").map_err(|e| e.to_string())?
        .query_row([slug], |r| r.get::<_, String>(0))
        .optional().map_err(|e| e.to_string())?;
    let status = match status {
        // JS AntilokiError message = "<skill>: <cause>" — mirror it exactly for faithfulness
        Some(s) => s,
        None => return Err(format!("card-deps-check: card '{slug}' not found")),
    };

    let mut blockers: Vec<Value> = Vec::new();
    if status == "blocked" {
        blockers.push(json!({ "code": "STATUS_BLOCKED", "message": "Card status is blocked — resolve the block before running." }));
    }
    // JS TERMINAL = { done: 'already done', archived: 'archived' }
    let terminal = match status.as_str() {
        "done" => Some("already done"),
        "archived" => Some("archived"),
        _ => None,
    };
    if let Some(t) = terminal {
        blockers.push(json!({ "code": "STATUS_TERMINAL", "message": format!("Card is {t} — nothing left to run.") }));
    }

    // parent-awaits-children: open (non-terminal) children block the parent
    let children: Vec<String> = {
        let mut stmt = db.prepare(
            "SELECT slug FROM cards WHERE parent_slug = ?1 AND status NOT IN ('done','blocked','archived')",
        ).map_err(|e| e.to_string())?;
        let rows = stmt.query_map([slug], |r| r.get::<_, String>(0)).map_err(|e| e.to_string())?;
        rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())?
    };
    if !children.is_empty() {
        blockers.push(json!({
            "code": "AWAITING_CHILDREN",
            "message": format!("{} child card(s) still open — finish them first: {}", children.len(), children.join(", ")),
        }));
    }

    Ok(json!({ "ok": blockers.is_empty(), "blockers": blockers }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seeded(rows: &[(&str, &str, Option<&str>)]) -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE cards (slug TEXT PRIMARY KEY, status TEXT, parent_slug TEXT)").unwrap();
        for (slug, status, parent) in rows {
            c.execute("INSERT INTO cards (slug, status, parent_slug) VALUES (?1, ?2, ?3)", rusqlite::params![slug, status, parent]).unwrap();
        }
        c
    }

    #[test]
    fn eligible_card_has_no_blockers() {
        let c = seeded(&[("C1", "backlog", None)]);
        let r = cardDepsCheck(&json!({ "slug": "C1" }), &c).unwrap();
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["blockers"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn blocked_terminal_and_children() {
        let c = seeded(&[("B", "blocked", None), ("D", "done", None), ("P", "backlog", None), ("K", "doing", Some("P"))]);
        assert_eq!(cardDepsCheck(&json!({ "slug": "B" }), &c).unwrap()["blockers"][0]["code"], json!("STATUS_BLOCKED"));
        assert_eq!(cardDepsCheck(&json!({ "slug": "D" }), &c).unwrap()["blockers"][0]["code"], json!("STATUS_TERMINAL"));
        let p = cardDepsCheck(&json!({ "slug": "P" }), &c).unwrap();
        assert_eq!(p["ok"], json!(false));
        assert_eq!(p["blockers"][0]["code"], json!("AWAITING_CHILDREN"));
    }

    #[test]
    fn missing_slug_and_missing_card_error() {
        let c = seeded(&[("C1", "backlog", None)]);
        assert!(cardDepsCheck(&json!({}), &c).is_err());
        assert!(cardDepsCheck(&json!({ "slug": "ghost" }), &c).is_err());
    }
}
