//! cost-settings.rs — twin of skills/system/cost-settings.mjs (db lane, get/set). NAMES ARE CONTRACT:
//! `costSettings` (named default). Operator cost caps in a kv table; DEFAULTS merged over stored rows.
// QUALITY: 80/100 — DEFAULTS merge, nullable tier caps ('' → null), typed set validation, kv upsert
use rusqlite::Connection;
use serde_json::{json, Map, Value};

const NULLABLE: [&str; 4] = ["hard_stop_pct", "cap_low_usd", "cap_mid_usd", "cap_high_usd"];
const TIER_CAPS: [&str; 3] = ["cap_low_usd", "cap_mid_usd", "cap_high_usd"];

fn defaults() -> Map<String, Value> {
    let mut m = Map::new();
    m.insert("daily_cap_usd".into(), json!(50));
    m.insert("alert_threshold_pct".into(), json!(80));
    m.insert("hard_stop_pct".into(), Value::Null);
    m.insert("cap_low_usd".into(), Value::Null);
    m.insert("cap_mid_usd".into(), Value::Null);
    m.insert("cap_high_usd".into(), Value::Null);
    m
}
fn parse_num(s: &str) -> Value {
    if let Ok(i) = s.parse::<i64>() { json!(i) } else if let Ok(f) = s.parse::<f64>() { json!(f) } else { Value::Null }
}
fn js_num_str(v: &Value) -> String { // String(v) for a number
    if let Some(i) = v.as_i64() { i.to_string() } else if let Some(f) = v.as_f64() { format!("{f}") } else { v.as_str().map(|s| s.to_string()).unwrap_or_else(|| v.to_string()) }
}
fn js_typeof(v: &Value) -> &'static str {
    match v { Value::Null => "object", Value::Bool(_) => "boolean", Value::Number(_) => "number", Value::String(_) => "string", _ => "object" }
}
fn ensure_table(db: &Connection) { let _ = db.execute_batch("CREATE TABLE IF NOT EXISTS cost_settings (key TEXT PRIMARY KEY, value TEXT)"); }

fn read(db: &Connection) -> Value {
    ensure_table(db);
    let mut out = defaults();
    let rows = crate::db_json::query_rows(db, "SELECT key, value FROM cost_settings", &[]).unwrap_or(json!([]));
    for r in rows.as_array().unwrap() {
        let key = r.get("key").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let value = r.get("value");
        if value.map(|v| v.is_null()).unwrap_or(true) { continue; }
        if !out.contains_key(&key) { continue; }
        let vs = value.unwrap().as_str().unwrap_or("");
        let parsed = if NULLABLE.contains(&key.as_str()) {
            if vs.is_empty() { Value::Null } else { parse_num(vs) }
        } else { parse_num(vs) };
        out.insert(key, parsed);
    }
    Value::Object(out)
}
fn write(db: &Connection, patch: &Map<String, Value>) {
    ensure_table(db);
    for (k, v) in patch {
        let vstr = if v.is_null() { String::new() } else { js_num_str(v) };
        let _ = crate::db_json::run_write(db, "INSERT INTO cost_settings (key, value) VALUES (?, ?) ON CONFLICT(key) DO UPDATE SET value = excluded.value", &[json!(k), json!(vstr)]);
    }
}

pub fn costSettings(input: &Value, db: &Connection) -> Result<Value, String> {
    let op = input.get("op");
    if op.and_then(|v| v.as_str()) == Some("get") || op.map(|v| v.is_null()).unwrap_or(true) {
        return Ok(read(db));
    }
    if op.and_then(|v| v.as_str()) == Some("set") {
        let mut patch: Map<String, Value> = Map::new();
        if let Some(d) = input.get("daily_cap_usd") {
            if !d.is_number() || d.as_f64().unwrap_or(f64::NAN) <= 0.0 {
                return Err(format!("cost-settings: daily_cap_usd must be positive number (got {})", js_typeof(d)));
            }
            patch.insert("daily_cap_usd".into(), d.clone());
        }
        if let Some(a) = input.get("alert_threshold_pct") { patch.insert("alert_threshold_pct".into(), a.clone()); }
        if let Some(h) = input.get("hard_stop_pct") { patch.insert("hard_stop_pct".into(), h.clone()); }
        for k in TIER_CAPS {
            if let Some(v) = input.get(k) {
                if v.is_null() || v.as_str() == Some("") { patch.insert(k.into(), Value::Null); continue; }
                if !v.is_number() || v.as_f64().unwrap_or(f64::NAN) < 0.0 {
                    return Err(format!("cost-settings: {k} must be non-negative number or null (got {})", js_typeof(v)));
                }
                patch.insert(k.into(), v.clone());
            }
        }
        write(db, &patch);
        return Ok(read(db));
    }
    Err(format!("cost-settings: op must be one of: get, set (got '{}')", op.and_then(|v| v.as_str()).unwrap_or("")))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn get_set_defaults() {
        let c = Connection::open_in_memory().unwrap();
        let g = costSettings(&json!({ "op": "get" }), &c).unwrap();
        assert_eq!(g["daily_cap_usd"], json!(50));
        assert_eq!(g["hard_stop_pct"], Value::Null);
        let s = costSettings(&json!({ "op": "set", "daily_cap_usd": 25, "cap_low_usd": 5 }), &c).unwrap();
        assert_eq!(s["daily_cap_usd"], json!(25));
        assert_eq!(s["cap_low_usd"], json!(5));
        assert!(costSettings(&json!({ "op": "set", "daily_cap_usd": -1 }), &c).unwrap_err().contains("positive number"));
        assert!(costSettings(&json!({ "op": "bogus" }), &c).unwrap_err().contains("op must be one of"));
    }
}
