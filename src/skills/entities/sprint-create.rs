//! sprint-create.rs — twin of skills/entities/sprint-create.mjs (db WRITE lane). NAMES ARE
//! CONTRACT: `sprintCreate` (named default). INSERT + UNIQUE → already-exists.
// QUALITY: 87/100 — write with a constraint branch (UNIQUE → VALIDATION_LOGIC message)
use rusqlite::{params, Connection};
use serde_json::{json, Value};

pub fn sprintCreate(input: &Value, db: &Connection) -> Result<Value, String> {
    let slug = match input.get("slug").and_then(|v| v.as_str()).filter(|s| !s.is_empty()) {
        Some(s) => s,
        None => return Err("sprint-create: slug is required".into()),
    };
    let goal_md = input.get("goal_md").cloned().unwrap_or(Value::Null);
    let capacity = input.get("capacity_pts").and_then(|v| v.as_f64()).unwrap_or(0.0);
    if capacity < 0.0 {
        return Err("sprint-create: capacity_pts (non-negative) is required".into());
    }
    let goal_param: Option<String> = match &goal_md { Value::Null => None, Value::String(s) => Some(s.clone()), o => Some(o.to_string()) };
    match db.execute("INSERT INTO sprints (slug, goal_md, capacity_pts) VALUES (?1, ?2, ?3)", params![slug, goal_param, capacity]) {
        Ok(_) => Ok(json!({ "id": db.last_insert_rowid(), "slug": slug, "goal_md": goal_md, "capacity_pts": capacity })),
        Err(e) => {
            if e.to_string().contains("UNIQUE") {
                Err(format!("sprint-create: sprint '{slug}' already exists"))
            } else {
                Err(e.to_string())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn creates_and_rejects_dupe() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE sprints (id INTEGER PRIMARY KEY, slug TEXT UNIQUE, goal_md TEXT, capacity_pts REAL)").unwrap();
        assert_eq!(sprintCreate(&json!({ "slug": "s1", "capacity_pts": 5 }), &c).unwrap()["id"], json!(1));
        assert!(sprintCreate(&json!({ "slug": "s1" }), &c).is_err()); // dup
        assert!(sprintCreate(&json!({}), &c).is_err());
    }
}
