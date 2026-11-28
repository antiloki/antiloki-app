//! entry-points-get.rs — twin of skills/codebase/entry-points-get.mjs (db lane). NAMES ARE
//! CONTRACT: `entryPointsGet` (named default). Filtered list + kind/path grouping + snapshot.
// QUALITY: 84/100 — where-building + grouping into counts/by_kind/by_path + latest snapshot parse
use rusqlite::Connection;
use serde_json::{json, Map, Value};

pub fn entryPointsGet(input: &Value, db: &Connection) -> Result<Value, String> {
    let origin = input.get("origin").and_then(|v| v.as_str()).unwrap_or("atlas");
    let mut where_ = String::from("origin = ?");
    let mut args: Vec<Value> = vec![json!(origin)];
    if let Some(p) = input.get("path").and_then(|v| v.as_str()).filter(|s| !s.is_empty()) {
        where_.push_str(" AND path = ?");
        args.push(json!(p.replace('\\', "/")));
    } else if let Some(pre) = input.get("prefix").and_then(|v| v.as_str()).filter(|s| !s.is_empty()) {
        let norm = pre.replace('\\', "/");
        where_.push_str(" AND path LIKE ?");
        args.push(json!(format!("{}/%", norm.trim_end_matches('/'))));
    }
    let items = crate::db_json::query_rows(db,
        &format!("SELECT kind, subtype, node_id, path, label, http_method, detail, confidence, line, framework FROM code_entry_points WHERE {where_} ORDER BY kind, path, line"),
        &args).unwrap_or_else(|_| json!([]));
    let iarr = items.as_array().unwrap();

    let mut counts = Map::new();
    for k in ["route", "ui", "data"] { counts.insert(k.into(), json!(0)); }
    let mut by_kind: Map<String, Value> = Map::new();
    for k in ["route", "ui", "data"] { by_kind.insert(k.into(), json!([])); }
    let mut by_path: Map<String, Value> = Map::new();
    for e in iarr {
        let kind = e.get("kind").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let cur = counts.get(&kind).and_then(|v| v.as_i64()).unwrap_or(0);
        counts.insert(kind.clone(), json!(cur + 1));
        by_kind.entry(kind).or_insert_with(|| json!([])).as_array_mut().unwrap().push(e.clone());
        let path = e.get("path").and_then(|v| v.as_str()).unwrap_or("").to_string();
        by_path.entry(path).or_insert_with(|| json!([])).as_array_mut().unwrap().push(e.clone());
    }

    // latest snapshot — best-effort
    let mut latest_snapshot = Value::Null;
    if let Ok(s) = crate::db_json::query_rows(db,
        "SELECT captured_at, counts_json, diff_json FROM code_entry_point_snapshots WHERE origin = ? ORDER BY id DESC LIMIT 1",
        &[json!(origin)]) {
        if let Some(row) = s.as_array().unwrap().first() {
            let parse = |v: Option<&Value>, dflt: &str| -> Value {
                v.and_then(|x| x.as_str()).map(|str_| serde_json::from_str::<Value>(str_).unwrap_or(Value::Null))
                    .unwrap_or_else(|| serde_json::from_str::<Value>(dflt).unwrap())
            };
            latest_snapshot = json!({
                "captured_at": row.get("captured_at").cloned().unwrap_or(Value::Null),
                "counts": parse(row.get("counts_json"), "{}"),
                "diff": parse(row.get("diff_json"), "null"),
            });
        }
    }

    Ok(json!({
        "origin": origin, "total": iarr.len(), "counts": counts,
        "by_kind": by_kind, "by_path": by_path, "items": items, "latest_snapshot": latest_snapshot,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn groups_and_counts() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE code_entry_points (origin TEXT, kind TEXT, subtype TEXT, node_id INTEGER, path TEXT, label TEXT, http_method TEXT, detail TEXT, confidence REAL, line INTEGER, framework TEXT);
            INSERT INTO code_entry_points (origin,kind,path,line) VALUES ('atlas','route','/a',1),('atlas','ui','/b',2),('atlas','route','/a',3);").unwrap();
        let r = entryPointsGet(&json!({}), &c).unwrap();
        assert_eq!(r["total"], json!(3));
        assert_eq!(r["counts"]["route"], json!(2));
        assert_eq!(r["by_path"]["/a"].as_array().unwrap().len(), 2);
    }
}
