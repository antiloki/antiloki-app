//! sprint-add-item.rs — twin of skills/entities/sprint-add-item.mjs (db WRITE lane). NAMES ARE
//! CONTRACT: `sprintAddItem` (named default). Capacity-validated add of a card to a sprint.
// QUALITY: 82/100 — two NOT_FOUND guards, capacity SUM check, UNIQUE→already-in-sprint branch
use rusqlite::Connection;
use serde_json::{json, Value};

fn jsnum(v: &Value) -> String {
    if let Some(i) = v.as_i64() { i.to_string() }
    else if let Some(f) = v.as_f64() { format!("{f}") }
    else { v.to_string() }
}

pub fn sprintAddItem(input: &Value, db: &Connection) -> Result<Value, String> {
    let sprint_slug = input.get("sprint_slug").and_then(|v| v.as_str()).filter(|s| !s.is_empty());
    let sprint_slug = match sprint_slug { Some(s) => s.to_string(), None => return Err("sprint-add-item: sprint_slug is required".into()) };
    let card_slug = input.get("card_slug").and_then(|v| v.as_str()).filter(|s| !s.is_empty());
    let card_slug = match card_slug { Some(s) => s.to_string(), None => return Err("sprint-add-item: card_slug is required".into()) };
    let est_pts = input.get("est_pts").cloned().unwrap_or(json!(1));
    let est_f = est_pts.as_f64().unwrap_or(1.0);
    let phase = input.get("phase").and_then(|v| v.as_str()).unwrap_or("impl").to_string();

    let sprint = crate::db_json::query_rows(db, "SELECT slug, capacity_pts FROM sprints WHERE slug = ?", &[json!(sprint_slug)])?;
    let sprint = match sprint.as_array().and_then(|a| a.first()) {
        Some(r) => r.clone(),
        None => return Err(format!("sprint-add-item: sprint '{sprint_slug}' not found")),
    };
    let card = crate::db_json::query_rows(db, "SELECT slug FROM cards WHERE slug = ?", &[json!(card_slug)])?;
    if card.as_array().map(|a| a.is_empty()).unwrap_or(true) {
        return Err(format!("sprint-add-item: card '{card_slug}' not found"));
    }

    let cap_val = sprint.get("capacity_pts").cloned().unwrap_or(Value::Null);
    let cap = cap_val.as_f64().unwrap_or(0.0);
    if cap > 0.0 {
        let used_row = crate::db_json::query_rows(db, "SELECT COALESCE(SUM(est_pts),0) AS s FROM sprint_items WHERE sprint_slug = ?", &[json!(sprint_slug)])?;
        let used_val = used_row.as_array().and_then(|a| a.first()).and_then(|r| r.get("s")).cloned().unwrap_or(json!(0));
        let used = used_val.as_f64().unwrap_or(0.0);
        if used + est_f > cap {
            return Err(format!(
                "sprint-add-item: sprint '{sprint_slug}' over capacity: used={} +{} > cap={}",
                jsnum(&used_val), jsnum(&est_pts), jsnum(&cap_val)
            ));
        }
    }

    match crate::db_json::run_write(db, "INSERT INTO sprint_items (sprint_slug, card_slug, phase, est_pts) VALUES (?, ?, ?, ?)", &[json!(sprint_slug), json!(card_slug), json!(phase), est_pts.clone()]) {
        Ok((_, id)) => Ok(json!({ "id": id, "sprint_slug": sprint_slug, "card_slug": card_slug, "phase": phase, "est_pts": est_pts })),
        Err(e) if e.contains("UNIQUE") => Err(format!("sprint-add-item: card '{card_slug}' is already in sprint '{sprint_slug}'")),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn seed() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE sprints (slug TEXT, capacity_pts REAL); \
          CREATE TABLE cards (slug TEXT); \
          CREATE TABLE sprint_items (id INTEGER PRIMARY KEY, sprint_slug TEXT, card_slug TEXT, phase TEXT, est_pts REAL); \
          INSERT INTO sprints VALUES ('S', 5); INSERT INTO cards VALUES ('C'),('D');").unwrap();
        c
    }
    #[test]
    fn adds_and_guards() {
        let c = seed();
        assert_eq!(sprintAddItem(&json!({ "sprint_slug": "S", "card_slug": "C", "est_pts": 3 }), &c).unwrap()["id"], json!(1));
        // over capacity: 3 used + 3 > 5
        assert!(sprintAddItem(&json!({ "sprint_slug": "S", "card_slug": "D", "est_pts": 3 }), &c).unwrap_err().contains("over capacity"));
        assert!(sprintAddItem(&json!({ "sprint_slug": "X", "card_slug": "C" }), &c).unwrap_err().contains("sprint 'X' not found"));
        assert!(sprintAddItem(&json!({ "sprint_slug": "S", "card_slug": "ZZ" }), &c).unwrap_err().contains("card 'ZZ' not found"));
    }
}
