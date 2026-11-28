//! code-rebuild-settings.rs — twin of skills/system/code-rebuild-settings.mjs (db read+write lane).
//! NAMES ARE CONTRACT: codeRebuildSettings (default) · readRebuildSettings (db-first). Atlas rebuild caps.
// QUALITY: 80/100 — lazy kv table, clamp-on-read (floors), floored-positive-number set, get/set dispatch
use rusqlite::Connection;
use serde_json::{json, Value};

// DEFAULTS (insertion order preserved in output): max_tokens · max_file_bytes · max_files
const DEF_MAX_TOKENS: f64 = 250000.0;
const DEF_MAX_FILE_BYTES: f64 = 1048576.0;
const DEF_MAX_FILES: f64 = 400.0;
// FLOORS
const FLOOR_MAX_TOKENS: f64 = 1000.0;
const FLOOR_MAX_FILE_BYTES: f64 = 1024.0;
const FLOOR_MAX_FILES: f64 = 1.0;

fn js_number(v: Option<&Value>) -> f64 {
    match v {
        None => f64::NAN, Some(Value::Null) => 0.0,
        Some(Value::Bool(b)) => if *b { 1.0 } else { 0.0 },
        Some(Value::Number(n)) => n.as_f64().unwrap_or(f64::NAN),
        Some(Value::String(s)) => { let t = s.trim(); if t.is_empty() { 0.0 } else { t.parse::<f64>().unwrap_or(f64::NAN) } },
        Some(_) => f64::NAN,
    }
}
// byte-identical to the JS db.exec string so sqlite_master matches in both lanes
fn ensure_table(db: &Connection) {
    let _ = db.execute_batch("\n    CREATE TABLE IF NOT EXISTS code_settings (\n      key   TEXT PRIMARY KEY,\n      value TEXT\n    )\n  ");
}

pub fn readRebuildSettings(db: &Connection) -> Value {
    ensure_table(db);
    let (mut mt, mut mfb, mut mf) = (DEF_MAX_TOKENS, DEF_MAX_FILE_BYTES, DEF_MAX_FILES);
    let rows = crate::db_json::query_rows(db, "SELECT key, value FROM code_settings", &[]).ok().and_then(|r| r.as_array().cloned()).unwrap_or_default();
    for r in &rows {
        let key = r.get("key").and_then(|v| v.as_str()).unwrap_or("");
        let val = r.get("value");
        if val.map_or(true, |v| v.is_null()) { continue; } // value == null
        if !matches!(key, "max_tokens" | "max_file_bytes" | "max_files") { continue; } // !(key in DEFAULTS)
        let n = js_number(val);
        if n.is_finite() {
            match key { "max_tokens" => mt = n, "max_file_bytes" => mfb = n, _ => mf = n }
        }
    }
    mt = mt.floor().max(FLOOR_MAX_TOKENS);
    mfb = mfb.floor().max(FLOOR_MAX_FILE_BYTES);
    mf = mf.floor().max(FLOOR_MAX_FILES);
    json!({ "max_tokens": mt as i64, "max_file_bytes": mfb as i64, "max_files": mf as i64 })
}

fn write(db: &Connection, patch: &[(&str, String)]) {
    ensure_table(db);
    for (k, v) in patch {
        let _ = db.execute("INSERT INTO code_settings (key, value) VALUES (?, ?) ON CONFLICT(key) DO UPDATE SET value = excluded.value", rusqlite::params![k, v]);
    }
}

pub fn codeRebuildSettings(input: &Value, db: &Connection) -> Result<Value, String> {
    let op = input.get("op");
    let op_str = op.and_then(|v| v.as_str());
    if op_str == Some("get") || op.map_or(true, |v| v.is_null()) { return Ok(readRebuildSettings(db)); }
    if op_str == Some("set") {
        let mut patch: Vec<(&str, String)> = Vec::new();
        for k in ["max_tokens", "max_file_bytes", "max_files"] {
            match input.get(k) {
                None => continue, // input[k] === undefined
                Some(v) => {
                    let n = js_number(Some(v));
                    if !v.is_number() || !n.is_finite() || n <= 0.0 {
                        return Err(format!("code-rebuild-settings: {} must be a positive number", k));
                    }
                    patch.push((k, format!("{}", n.floor() as i64)));
                }
            }
        }
        write(db, &patch);
        return Ok(readRebuildSettings(db));
    }
    Err(format!("code-rebuild-settings: op must be one of get, set"))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn get_set_clamp() {
        let c = Connection::open_in_memory().unwrap();
        // defaults when empty
        assert_eq!(readRebuildSettings(&c), json!({ "max_tokens": 250000, "max_file_bytes": 1048576, "max_files": 400 }));
        // set floors + floor
        let r = codeRebuildSettings(&json!({ "op": "set", "max_tokens": 5000.9, "max_files": 40 }), &c).unwrap();
        assert_eq!(r["max_tokens"], json!(5000)); // floored
        assert_eq!(r["max_files"], json!(40));
        // clamp: a stored below-floor value reads as the floor
        c.execute("INSERT INTO code_settings (key,value) VALUES ('max_files','0') ON CONFLICT(key) DO UPDATE SET value=excluded.value", []).unwrap();
        assert_eq!(readRebuildSettings(&c)["max_files"], json!(1)); // floor
        // get dispatch (op:'get' == op absent)
        assert_eq!(codeRebuildSettings(&json!({ "op": "get" }), &c).unwrap()["max_tokens"], codeRebuildSettings(&json!({}), &c).unwrap()["max_tokens"]);
        // invalid set throws
        assert!(codeRebuildSettings(&json!({ "op": "set", "max_tokens": -5 }), &c).is_err());
    }
}
