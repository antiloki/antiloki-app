//! card-automerge-set.rs — twin of skills/entities/card-automerge-set.mjs (db WRITE lane).
//! NAMES ARE CONTRACT: `cardAutomergeSet` (default). Flip a card's auto_merge toggle (1/0),
//! stamp updated_at, return { card_slug, auto_merge }. datetime('now') frozen 2026-01-01.
use rusqlite::Connection;
use serde_json::{json, Map, Value};

fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}

pub fn cardAutomergeSet(input: &Value, db: &Connection) -> Result<Value, String> {
    let slug_opt = input.get("card_slug");
    if !truthy(slug_opt) {
        return Err("card-automerge-set: card_slug is required".into());
    }
    let slug = slug_opt.unwrap();
    let value: i64 = if truthy(input.get("auto_merge")) { 1 } else { 0 };

    let rows = crate::db_json::query_rows(db, "SELECT slug FROM cards WHERE slug = ?", &[slug.clone()])?;
    if rows.as_array().unwrap().is_empty() {
        let s = slug.as_str().map(|x| x.to_string()).unwrap_or_else(|| slug.to_string());
        return Err(format!("card-automerge-set: card '{s}' not found"));
    }

    // JS: UPDATE ... updated_at = datetime('now') — frozen-clock lane pins 'now' to the instant below.
    crate::db_json::run_write(
        db,
        &format!("UPDATE cards SET auto_merge = ?, updated_at = {} WHERE slug = ?", crate::clock::now_sql()),
        &[json!(value), slug.clone()],
    )?;

    let mut out = Map::new();
    out.insert("card_slug".into(), slug.clone());
    out.insert("auto_merge".into(), json!(value));
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn seeded() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE cards (slug TEXT PRIMARY KEY, auto_merge INTEGER NOT NULL DEFAULT 0, updated_at TEXT); \
             INSERT INTO cards VALUES ('C-OFF', 0, '2025-12-01 00:00:00'); \
             INSERT INTO cards VALUES ('C-ON', 1, '2025-12-01 00:00:00');",
        ).unwrap();
        c
    }

    #[test]
    fn turns_on_and_stamps() {
        let c = seeded();
        let r = cardAutomergeSet(&json!({ "card_slug": "C-OFF", "auto_merge": true }), &c).unwrap();
        assert_eq!(r, json!({ "card_slug": "C-OFF", "auto_merge": 1 }));
        let row = crate::db_json::query_rows(&c, "SELECT auto_merge, updated_at FROM cards WHERE slug = ?", &[json!("C-OFF")]).unwrap();
        assert_eq!(row[0]["auto_merge"], json!(1));
        assert_eq!(row[0]["updated_at"], json!("2026-01-01 00:00:00"));
    }

    #[test]
    fn turns_off_when_falsy() {
        let c = seeded();
        // false → 0
        let r = cardAutomergeSet(&json!({ "card_slug": "C-ON", "auto_merge": false }), &c).unwrap();
        assert_eq!(r, json!({ "card_slug": "C-ON", "auto_merge": 0 }));
        // absent → 0
        let r2 = cardAutomergeSet(&json!({ "card_slug": "C-OFF" }), &c).unwrap();
        assert_eq!(r2, json!({ "card_slug": "C-OFF", "auto_merge": 0 }));
    }

    #[test]
    fn missing_card_errors() {
        let c = seeded();
        assert!(cardAutomergeSet(&json!({ "card_slug": "NOPE", "auto_merge": true }), &c).is_err());
    }
}
