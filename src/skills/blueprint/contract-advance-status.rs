//! contract-advance-status.rs — twin of skills/blueprint/contract-advance-status.mjs (db WRITE lane).
//! NAMES ARE CONTRACT: `contractAdvanceStatus` (default). Guarded + idempotent contract status flip:
//! UPDATE only fires when the row is in the expected `from` state; advanced = changes>0.
// QUALITY: enum guard (from/to) + NOT_FOUND when contract absent + guarded UPDATE with frozen clock
use rusqlite::{params, Connection};
use serde_json::{json, Value};

const VALID: [&str; 9] = [
    "proposed", "pl_ratified", "tl_approved", "human_approved", "impl", "qa", "done", "rejected", "superseded",
];

fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}

pub fn contractAdvanceStatus(input: &Value, db: &Connection) -> Result<Value, String> {
    let contract_slug = input.get("contract_slug").cloned().unwrap_or(Value::Null);
    if !truthy(Some(&contract_slug)) {
        return Err("contract-advance-status: contract_slug is required".into());
    }
    let from = input.get("from").cloned().unwrap_or(Value::Null);
    let to = input.get("to").cloned().unwrap_or(Value::Null);
    let from_s = from.as_str().unwrap_or("");
    let to_s = to.as_str().unwrap_or("");
    if !VALID.contains(&from_s) {
        return Err(format!("contract-advance-status: from must be one of: {} (got '{}')", VALID.join(", "), from_s));
    }
    if !VALID.contains(&to_s) {
        return Err(format!("contract-advance-status: to must be one of: {} (got '{}')", VALID.join(", "), to_s));
    }

    let slug_s = contract_slug.as_str().unwrap_or("");
    let rows = crate::db_json::query_rows(db, "SELECT status FROM contracts WHERE slug = ?", &[json!(slug_s)])?;
    if rows.as_array().map_or(true, |a| a.is_empty()) {
        return Err(format!("contract-advance-status: contract '{slug_s}' not found"));
    }

    // Guarded: flip ONLY from the expected state (idempotent — already-advanced no-ops).
    // datetime('now') is pinned to the frozen instant to match the conform-db clock lane.
    let changes = db
        .execute(
            &format!("UPDATE contracts SET status = ?1, updated_at = {} WHERE slug = ?2 AND status = ?3", crate::clock::now_sql()),
            params![to_s, slug_s, from_s],
        )
        .map_err(|e| e.to_string())?;

    Ok(json!({ "contract_slug": contract_slug, "from": from, "to": to, "advanced": changes > 0 }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup(status: &str) -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE contracts (slug TEXT PRIMARY KEY, card_slug TEXT, title TEXT, status TEXT, updated_at TEXT);").unwrap();
        c.execute("INSERT INTO contracts (slug, card_slug, title, status) VALUES ('CT-X','CARD-X','x',?1)", params![status]).unwrap();
        c
    }
    fn status_of(c: &Connection) -> String {
        c.query_row("SELECT status FROM contracts WHERE slug='CT-X'", [], |r| r.get(0)).unwrap()
    }

    #[test]
    fn flips_guards_and_rejects() {
        // flips from the expected state
        let c = setup("pl_ratified");
        let r = contractAdvanceStatus(&json!({ "contract_slug": "CT-X", "from": "pl_ratified", "to": "tl_approved" }), &c).unwrap();
        assert_eq!(r, json!({ "contract_slug": "CT-X", "from": "pl_ratified", "to": "tl_approved", "advanced": true }));
        assert_eq!(status_of(&c), "tl_approved");

        // guarded idempotent no-op when not in the expected state
        let r2 = contractAdvanceStatus(&json!({ "contract_slug": "CT-X", "from": "pl_ratified", "to": "tl_approved" }), &c).unwrap();
        assert_eq!(r2["advanced"], json!(false));
        assert_eq!(status_of(&c), "tl_approved");

        // apply transition tl_approved → impl
        assert_eq!(contractAdvanceStatus(&json!({ "contract_slug": "CT-X", "from": "tl_approved", "to": "impl" }), &c).unwrap()["advanced"], json!(true));
        assert_eq!(status_of(&c), "impl");

        // rejects unknown status + missing contract
        assert!(contractAdvanceStatus(&json!({ "contract_slug": "CT-X", "from": "bogus", "to": "impl" }), &c).is_err());
        assert!(contractAdvanceStatus(&json!({ "contract_slug": "NOPE", "from": "pl_ratified", "to": "impl" }), &setup("pl_ratified")).is_err());
    }
}
