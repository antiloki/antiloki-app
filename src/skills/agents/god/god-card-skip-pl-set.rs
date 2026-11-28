//! god-card-skip-pl-set.rs — twin of skills/agents/god/god-card-skip-pl-set.mjs (db WRITE lane).
//! NAMES ARE CONTRACT: `godCardSkipPlSet` (default). Flips cards.skip_pl_flag to 0/1 for one card,
//! returning the previous value and whether it changed. datetime('now') → frozen-clock literal.
use rusqlite::Connection;
use serde_json::{json, Map, Value};

// JS `!x` truthiness — used for the `if (!slug)` guard.
fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}

// JS `typeof v` for the boolean-guard error message (typeof null === "object").
fn js_typeof(v: Option<&Value>) -> &'static str {
    match v {
        None => "undefined",
        Some(Value::Null) => "object",
        Some(Value::Bool(_)) => "boolean",
        Some(Value::Number(_)) => "number",
        Some(Value::String(_)) => "string",
        Some(_) => "object",
    }
}

pub fn godCardSkipPlSet(input: &Value, db: &Connection) -> Result<Value, String> {
    let slug = input.get("slug");
    let skip_pl = input.get("skip_pl");

    if !truthy(slug) {
        return Err("god-card-skip-pl-set: slug is required".into());
    }
    let skip_pl_bool = match skip_pl {
        Some(Value::Bool(b)) => *b,
        other => {
            return Err(format!(
                "god-card-skip-pl-set: skip_pl must be a boolean; got {}",
                js_typeof(other)
            ))
        }
    };
    let slug_v = slug.cloned().unwrap_or(Value::Null);

    let rows = crate::db_json::query_rows(
        db,
        "SELECT slug, skip_pl_flag FROM cards WHERE slug = ?",
        &[slug_v.clone()],
    )?;
    let card = match rows.as_array().and_then(|a| a.first()) {
        Some(c) => c.clone(),
        None => return Err("god-card-skip-pl-set: card not found".into()),
    };
    let previous = card.get("skip_pl_flag").cloned().unwrap_or(Value::Null);

    let new_val: i64 = if skip_pl_bool { 1 } else { 0 };
    crate::db_json::run_write(
        db,
        &format!("UPDATE cards SET skip_pl_flag = ?, updated_at = {} WHERE slug = ?", crate::clock::now_sql()),
        &[json!(new_val), slug_v.clone()],
    )?;

    let changed = previous.as_i64() != Some(new_val);

    let mut out = Map::new();
    out.insert("slug".into(), slug_v);
    out.insert("skip_pl_flag".into(), json!(new_val));
    out.insert("previous".into(), previous);
    out.insert("changed".into(), json!(changed));
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn db0() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE cards (slug TEXT PRIMARY KEY, skip_pl_flag INTEGER NOT NULL DEFAULT 0, updated_at TEXT);",
        )
        .unwrap();
        c
    }

    #[test]
    fn flips_0_to_1_changed() {
        let c = db0();
        c.execute(
            "INSERT INTO cards (slug, skip_pl_flag, updated_at) VALUES ('FLIP-1', 0, '2025-12-01 00:00:00')",
            [],
        )
        .unwrap();
        let r = godCardSkipPlSet(&json!({ "slug": "FLIP-1", "skip_pl": true }), &c).unwrap();
        assert_eq!(r["slug"], json!("FLIP-1"));
        assert_eq!(r["skip_pl_flag"], json!(1));
        assert_eq!(r["previous"], json!(0));
        assert_eq!(r["changed"], json!(true));
        // key order is the JS object-literal order
        let keys: Vec<&String> = r.as_object().unwrap().keys().collect();
        assert_eq!(keys, ["slug", "skip_pl_flag", "previous", "changed"]);
        // db_after: flag flipped, updated_at frozen literal
        let flag: i64 = c
            .query_row("SELECT skip_pl_flag FROM cards WHERE slug='FLIP-1'", [], |row| row.get(0))
            .unwrap();
        let updated: String = c
            .query_row("SELECT updated_at FROM cards WHERE slug='FLIP-1'", [], |row| row.get(0))
            .unwrap();
        assert_eq!(flag, 1);
        assert_eq!(updated, "2026-01-01 00:00:00");
    }

    #[test]
    fn idempotent_0_to_0_unchanged() {
        let c = db0();
        c.execute("INSERT INTO cards (slug, skip_pl_flag, updated_at) VALUES ('KEEP-0', 0, '2025-12-01 00:00:00')", []).unwrap();
        let r = godCardSkipPlSet(&json!({ "slug": "KEEP-0", "skip_pl": false }), &c).unwrap();
        assert_eq!(r["skip_pl_flag"], json!(0));
        assert_eq!(r["previous"], json!(0));
        assert_eq!(r["changed"], json!(false));
    }

    #[test]
    fn already_1_set_1_unchanged() {
        let c = db0();
        c.execute("INSERT INTO cards (slug, skip_pl_flag, updated_at) VALUES ('KEEP-1', 1, '2025-12-01 00:00:00')", []).unwrap();
        let r = godCardSkipPlSet(&json!({ "slug": "KEEP-1", "skip_pl": true }), &c).unwrap();
        assert_eq!(r["skip_pl_flag"], json!(1));
        assert_eq!(r["previous"], json!(1));
        assert_eq!(r["changed"], json!(false));
    }

    #[test]
    fn flips_1_to_0_changed() {
        let c = db0();
        c.execute("INSERT INTO cards (slug, skip_pl_flag, updated_at) VALUES ('FLIP-0', 1, '2025-12-01 00:00:00')", []).unwrap();
        let r = godCardSkipPlSet(&json!({ "slug": "FLIP-0", "skip_pl": false }), &c).unwrap();
        assert_eq!(r["skip_pl_flag"], json!(0));
        assert_eq!(r["previous"], json!(1));
        assert_eq!(r["changed"], json!(true));
    }

    #[test]
    fn guards_reject_bad_input() {
        let c = db0();
        c.execute("INSERT INTO cards (slug, skip_pl_flag, updated_at) VALUES ('X', 0, '2025-12-01 00:00:00')", []).unwrap();
        assert!(godCardSkipPlSet(&json!({ "skip_pl": true }), &c).is_err()); // missing slug
        assert!(godCardSkipPlSet(&json!({ "slug": "X", "skip_pl": "yes" }), &c).is_err()); // non-boolean
        assert!(godCardSkipPlSet(&json!({ "slug": "NOPE", "skip_pl": true }), &c).is_err()); // not found
    }
}
