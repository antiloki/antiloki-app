//! switcher-mode.rs — twin of skills/ai/switcher-mode.mjs (db lane, get/set). NAMES ARE CONTRACT:
//! `switcherMode` (default) + `readCookingMode` (named). The dataset-cooking toggle in cost_settings.
// QUALITY: 86/100 — kv get/set with CREATE-IF-NOT-EXISTS + UPSERT; missing/absent → default ON
use rusqlite::Connection;
use serde_json::{json, Value};

const KEY: &str = "dataset_cooking";

pub fn readCookingMode(db: &Connection) -> bool {
    // JS: db.prepare(...).get(KEY)?.value !== '0'  → any value but '0' (incl. missing/null) is ON.
    match crate::db_json::query_rows(db, "SELECT value FROM cost_settings WHERE key = ?", &[json!(KEY)]) {
        Ok(rows) => rows
            .as_array()
            .and_then(|a| a.first())
            .and_then(|r| r.get("value"))
            .map(|v| v.as_str() != Some("0"))
            .unwrap_or(true), // no row → undefined !== '0' → true
        Err(_) => true, // table absent → default ON
    }
}

pub fn switcherMode(input: &Value, db: &Connection) -> Result<Value, String> {
    let op = input.get("op").and_then(|v| v.as_str()).unwrap_or("get");
    if op == "get" {
        return Ok(json!({ "cooking": readCookingMode(db) }));
    }
    if op == "set" {
        let cooking = input.get("cooking") != Some(&Value::Bool(false)); // !== false
        db.execute("CREATE TABLE IF NOT EXISTS cost_settings (key TEXT PRIMARY KEY, value TEXT)", [])
            .map_err(|e| e.to_string())?;
        crate::db_json::run_write(
            db,
            "INSERT INTO cost_settings (key, value) VALUES (?, ?) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            &[json!(KEY), json!(if cooking { "1" } else { "0" })],
        )?;
        return Ok(json!({ "cooking": cooking }));
    }
    Err(format!("switcher-mode: op must be one of: get, set (got '{op}')"))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn get_set_roundtrip() {
        let c = Connection::open_in_memory().unwrap();
        assert_eq!(switcherMode(&json!({ "op": "get" }), &c).unwrap()["cooking"], json!(true)); // table absent → ON
        switcherMode(&json!({ "op": "set", "cooking": false }), &c).unwrap();
        assert_eq!(switcherMode(&json!({ "op": "get" }), &c).unwrap()["cooking"], json!(false));
        switcherMode(&json!({ "op": "set", "cooking": true }), &c).unwrap();
        assert_eq!(switcherMode(&json!({}), &c).unwrap()["cooking"], json!(true)); // default op=get
        assert!(switcherMode(&json!({ "op": "bad" }), &c).is_err());
    }
}
