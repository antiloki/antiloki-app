//! anatomy-latest.rs — twin of skills/entities/anatomy-latest.mjs (db READ lane, attached `memory`
//! db). NAMES ARE CONTRACT: `anatomyLatest` (default). Most recent anatomy snapshot (parsed
//! payload_json), optionally diffed against an older snapshot by id (added/removed/changed skills,
//! added/removed tables + actions, counts_delta).
use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::collections::{HashMap, HashSet};

// JS String(v) coercion — used only to turn a `name`/dict key into a Rust-hashable key.
fn js_str(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        o => o.to_string(),
    }
}
// JS `${v}` interpolation (only feeds an error message).
fn tmpl(v: Option<&Value>) -> String {
    match v {
        None => "undefined".into(),
        Some(Value::Null) => "null".into(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(),
        Some(o) => o.to_string(),
    }
}
fn name_of(x: &Value, key: &str) -> String { js_str(x.get(key).unwrap_or(&Value::Null)) }

// nameSet(arr, key): dedup by x[key], first-occurrence insertion order (mirrors `new Set(...)`).
fn name_set(arr: &[Value], key: &str) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for x in arr {
        let n = name_of(x, key);
        if seen.insert(n.clone()) { out.push(n); }
    }
    out
}

// nameMap(arr, key): Map keyed by x[key] → x (last value wins per key, but a Map.set on an
// EXISTING key keeps its original position — so we track insertion order separately).
fn name_map(arr: &[Value], key: &str) -> (Vec<String>, HashMap<String, Value>) {
    let mut order = Vec::new();
    let mut map: HashMap<String, Value> = HashMap::new();
    for x in arr {
        let n = name_of(x, key);
        if !map.contains_key(&n) { order.push(n.clone()); }
        map.insert(n, x.clone());
    }
    (order, map)
}

fn diff_payloads(latest: &Value, older: &Value) -> Value {
    let latest_skills: Vec<Value> = latest.get("skills").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    let older_skills: Vec<Value> = older.get("skills").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    let (old_order, sub_old) = name_map(&older_skills, "name");
    let (new_order, sub_new) = name_map(&latest_skills, "name");

    let added: Vec<Value> = new_order.iter().filter(|n| !sub_old.contains_key(n.as_str())).map(|n| json!(n)).collect();
    let removed: Vec<Value> = old_order.iter().filter(|n| !sub_new.contains_key(n.as_str())).map(|n| json!(n)).collect();
    let changed: Vec<Value> = new_order
        .iter()
        .filter(|n| match sub_old.get(n.as_str()) {
            Some(o) => o.get("sha256") != sub_new.get(n.as_str()).and_then(|v| v.get("sha256")),
            None => false,
        })
        .map(|n| json!(n))
        .collect();

    let latest_tables: Vec<Value> = latest.get("tables").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    let older_tables: Vec<Value> = older.get("tables").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    let table_old = name_set(&older_tables, "name");
    let table_new = name_set(&latest_tables, "name");

    let latest_actions: Vec<Value> = latest.get("actions").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    let older_actions: Vec<Value> = older.get("actions").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    let action_old = name_set(&older_actions, "name");
    let action_new = name_set(&latest_actions, "name");

    let tables_added: Vec<Value> = table_new.iter().filter(|n| !table_old.contains(*n)).map(|n| json!(n)).collect();
    let tables_removed: Vec<Value> = table_old.iter().filter(|n| !table_new.contains(*n)).map(|n| json!(n)).collect();
    let actions_added: Vec<Value> = action_new.iter().filter(|n| !action_old.contains(*n)).map(|n| json!(n)).collect();
    let actions_removed: Vec<Value> = action_old.iter().filter(|n| !action_new.contains(*n)).map(|n| json!(n)).collect();

    // counts_delta = Object.fromEntries(Object.keys(latest.counts).map(k => [k, latest.counts[k] - (older.counts[k] ?? 0)]))
    let mut counts_delta = Map::new();
    if let Some(latest_counts) = latest.get("counts").and_then(|v| v.as_object()) {
        let older_counts = older.get("counts").and_then(|v| v.as_object());
        for (k, lv) in latest_counts {
            let lnum = lv.as_f64().unwrap_or(0.0);
            let onum = older_counts
                .and_then(|oc| oc.get(k))
                .filter(|v| !v.is_null())
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0);
            counts_delta.insert(k.clone(), json!((lnum - onum) as i64));
        }
    }

    json!({
        "skills": { "added": added, "removed": removed, "changed": changed },
        "tables": { "added": tables_added, "removed": tables_removed },
        "actions": { "added": actions_added, "removed": actions_removed },
        "counts_delta": Value::Object(counts_delta),
    })
}

pub fn anatomyLatest(input: &Value, db: &Connection) -> Result<Value, String> {
    let rows = crate::db_json::query_rows(
        db,
        "SELECT * FROM memory.anatomy_snapshots ORDER BY id DESC LIMIT 1",
        &[],
    )?;
    let latest_row = match rows.as_array().and_then(|a| a.first()) {
        Some(r) => r.clone(),
        None => return Ok(Value::Null),
    };

    let payload_json = latest_row.get("payload_json").and_then(|v| v.as_str()).unwrap_or("");
    let payload: Value = serde_json::from_str(payload_json).map_err(|e| e.to_string())?;

    let mut result = Map::new();
    result.insert("id".into(), latest_row.get("id").cloned().unwrap_or(Value::Null));
    result.insert("taken_at".into(), latest_row.get("taken_at").cloned().unwrap_or(Value::Null));
    result.insert("sha256".into(), latest_row.get("sha256").cloned().unwrap_or(Value::Null));
    result.insert("taken_by".into(), latest_row.get("taken_by").cloned().unwrap_or(Value::Null));
    result.insert("git_sha".into(), latest_row.get("git_sha").cloned().unwrap_or(Value::Null));
    result.insert("payload".into(), payload.clone());

    let diff_against = input.get("diff_against").cloned().unwrap_or(Value::Null);
    if !diff_against.is_null() {
        let is_int = diff_against.is_number()
            && diff_against.as_f64().map_or(false, |f| f.is_finite() && f.fract() == 0.0);
        if !is_int {
            return Err("anatomy-latest: diff_against must be an integer snapshot id".into());
        }
        let older_rows = crate::db_json::query_rows(
            db,
            "SELECT payload_json FROM memory.anatomy_snapshots WHERE id = ?",
            &[diff_against.clone()],
        )?;
        let older_row = match older_rows.as_array().and_then(|a| a.first()) {
            Some(r) => r,
            None => {
                return Err(format!(
                    "anatomy-latest: snapshot id '{}' not found",
                    tmpl(Some(&diff_against))
                ))
            }
        };
        let older_payload_json = older_row.get("payload_json").and_then(|v| v.as_str()).unwrap_or("");
        let older_payload: Value = serde_json::from_str(older_payload_json).map_err(|e| e.to_string())?;
        result.insert("diff".into(), diff_payloads(&payload, &older_payload));
        result.insert("diff_against".into(), diff_against);
    }

    Ok(Value::Object(result))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "ATTACH ':memory:' AS memory;
            CREATE TABLE memory.anatomy_snapshots (
              id           INTEGER PRIMARY KEY AUTOINCREMENT,
              taken_at     TEXT    NOT NULL DEFAULT (datetime('now')),
              payload_json TEXT    NOT NULL,
              sha256       TEXT    NOT NULL UNIQUE,
              taken_by     TEXT    NOT NULL,
              git_sha      TEXT
            );",
        )
        .unwrap();
        c
    }

    #[test]
    fn returns_null_when_no_snapshots() {
        let c = db();
        assert_eq!(anatomyLatest(&json!({}), &c).unwrap(), Value::Null);
    }

    #[test]
    fn returns_latest_with_parsed_payload_and_no_diff() {
        let c = db();
        c.execute(
            "INSERT INTO memory.anatomy_snapshots (id, taken_at, payload_json, sha256, taken_by, git_sha) VALUES (1, '2025-12-01 00:00:00', ?, 'h1', 'tester', NULL)",
            rusqlite::params![r#"{"counts":{"skills":1},"skills":[{"name":"x","sha256":"h"}],"tables":[],"views":[],"agents":[],"actions":[]}"#],
        )
        .unwrap();
        let r = anatomyLatest(&json!({}), &c).unwrap();
        assert_eq!(r["id"], json!(1));
        assert_eq!(r["taken_at"], json!("2025-12-01 00:00:00"));
        assert_eq!(r["sha256"], json!("h1"));
        assert_eq!(r["taken_by"], json!("tester"));
        assert_eq!(r["git_sha"], Value::Null);
        assert_eq!(r["payload"]["skills"][0]["name"], json!("x"));
        assert!(r.get("diff").is_none());
        assert!(r.get("diff_against").is_none());
    }

    #[test]
    fn diff_against_reports_added_removed_changed_and_counts_delta() {
        let c = db();
        let older = r#"{"counts":{"skills":2,"tables":1,"views":0,"agents":1,"actions":2},"skills":[{"name":"skill-a","sha256":"hash-a-old"},{"name":"skill-b","sha256":"hash-b"},{"name":"skill-d","sha256":"hash-d"}],"tables":[{"name":"table_old"},{"name":"table_gone"}],"views":[],"agents":[{"name":"dev"}],"actions":[{"name":"action-old"},{"name":"action-gone"}]}"#;
        let latest = r#"{"counts":{"skills":3,"tables":2,"views":0,"agents":1,"actions":2,"cards":5,"sacred_timeline":1,"memories":10,"embeddings":10,"memory_links":3},"skills":[{"name":"skill-a","sha256":"hash-a-new"},{"name":"skill-b","sha256":"hash-b"},{"name":"skill-c","sha256":"hash-c"}],"tables":[{"name":"table_old"},{"name":"table_new"}],"views":[],"agents":[{"name":"dev"}],"actions":[{"name":"action-old"},{"name":"action-new"}]}"#;
        c.execute(
            "INSERT INTO memory.anatomy_snapshots (id, taken_at, payload_json, sha256, taken_by, git_sha) VALUES (1, '2025-12-01 00:00:00', ?, 'h1', 't1', NULL)",
            rusqlite::params![older],
        )
        .unwrap();
        c.execute(
            "INSERT INTO memory.anatomy_snapshots (id, taken_at, payload_json, sha256, taken_by, git_sha) VALUES (2, '2026-01-01 00:00:00', ?, 'h2', 't2', 'sha123')",
            rusqlite::params![latest],
        )
        .unwrap();

        let r = anatomyLatest(&json!({ "diff_against": 1 }), &c).unwrap();
        assert_eq!(r["id"], json!(2));
        assert_eq!(r["diff_against"], json!(1));
        assert_eq!(r["diff"]["skills"]["added"], json!(["skill-c"]));
        assert_eq!(r["diff"]["skills"]["removed"], json!(["skill-d"]));
        assert_eq!(r["diff"]["skills"]["changed"], json!(["skill-a"]));
        assert_eq!(r["diff"]["tables"]["added"], json!(["table_new"]));
        assert_eq!(r["diff"]["tables"]["removed"], json!(["table_gone"]));
        assert_eq!(r["diff"]["actions"]["added"], json!(["action-new"]));
        assert_eq!(r["diff"]["actions"]["removed"], json!(["action-gone"]));
        assert_eq!(r["diff"]["counts_delta"]["skills"], json!(1));
        assert_eq!(r["diff"]["counts_delta"]["tables"], json!(1));
        assert_eq!(r["diff"]["counts_delta"]["views"], json!(0));
        assert_eq!(r["diff"]["counts_delta"]["agents"], json!(0));
        assert_eq!(r["diff"]["counts_delta"]["actions"], json!(0));
        assert_eq!(r["diff"]["counts_delta"]["cards"], json!(5));
        assert_eq!(r["diff"]["counts_delta"]["memories"], json!(10));
    }
}
