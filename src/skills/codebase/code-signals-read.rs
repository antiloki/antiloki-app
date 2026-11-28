//! code-signals-read.rs — twin of skills/codebase/code-signals-read.mjs (db READ lane). NAMES ARE
//! CONTRACT: `codeSignalsRead` (named default). Grouped signal counts + filtered rows, sev-first.
// QUALITY: 80/100 — dynamic WHERE, nested grouped counts (preserve_order), stable severity re-sort
use rusqlite::Connection;
use serde_json::{json, Map, Value};

fn sev_rank(s: &str) -> i64 {
    match s { "high" => 0, "medium" => 1, "low" => 2, _ => 3 }
}

pub fn codeSignalsRead(input: &Value, db: &Connection) -> Result<Value, String> {
    let origin = input.get("origin").and_then(|v| v.as_str()).unwrap_or("atlas").to_string();
    // JS: Number.isInteger(limit) && limit>0 ? limit : 500
    let limit = match input.get("limit") {
        Some(Value::Number(n)) if n.is_i64() && n.as_i64().unwrap() > 0 => n.as_i64().unwrap(),
        _ => 500,
    };

    // rows (dynamic filter); table-absent → []
    let mut wh = vec!["origin = ?".to_string()];
    let mut args: Vec<Value> = vec![json!(origin)];
    if let Some(cat) = input.get("category").filter(|v| truthy(v)) {
        wh.push("category = ?".into());
        args.push(json!(jstr(cat)));
    }
    if let Some(kind) = input.get("kind").filter(|v| truthy(v)) {
        wh.push("kind = ?".into());
        args.push(json!(jstr(kind)));
    }
    let sql = format!(
        "SELECT category, kind, severity, path, line, detail FROM code_signals WHERE {} \
         ORDER BY CASE severity WHEN 'high' THEN 0 WHEN 'medium' THEN 1 ELSE 2 END, category, kind, path LIMIT ?",
        wh.join(" AND ")
    );
    args.push(json!(limit));
    let mut rows: Vec<Value> = crate::db_json::query_rows(db, &sql, &args)
        .ok()
        .and_then(|r| r.as_array().cloned())
        .unwrap_or_default();

    // full grouped counts over ALL rows for this origin (independent of the row filter/limit)
    let mut by_category = Map::new();
    let mut counts = json!({ "high": 0, "medium": 0, "low": 0 });
    let mut total = 0i64;
    if let Ok(g) = crate::db_json::query_rows(
        db,
        "SELECT category, kind, severity, COUNT(*) n FROM code_signals WHERE origin = ? GROUP BY category, kind, severity",
        &[json!(origin)],
    ) {
        for r in g.as_array().unwrap() {
            let cat = jstr(r.get("category").unwrap_or(&Value::Null));
            let kind = jstr(r.get("kind").unwrap_or(&Value::Null));
            let sev = jstr(r.get("severity").unwrap_or(&Value::Null));
            let n = r.get("n").and_then(|v| v.as_i64()).unwrap_or(0);
            let cat_obj = by_category.entry(cat).or_insert_with(|| json!({})).as_object_mut().unwrap();
            let kind_obj = cat_obj.entry(kind).or_insert_with(|| json!({})).as_object_mut().unwrap();
            kind_obj.insert(sev.clone(), json!(n));
            let counts_obj = counts.as_object_mut().unwrap();
            let prev = counts_obj.get(&sev).and_then(|v| v.as_i64()).unwrap_or(0);
            counts_obj.insert(sev.clone(), json!(prev + n));
            total += n;
        }
    }

    // stable re-sort of rows by severity rank only (JS Array.sort is stable)
    rows.sort_by_key(|r| sev_rank(r.get("severity").and_then(|v| v.as_str()).unwrap_or("")));
    Ok(json!({ "origin": origin, "total": total, "by_category": by_category, "counts": counts, "rows": rows }))
}

fn truthy(v: &Value) -> bool {
    match v {
        Value::Null | Value::Bool(false) => false,
        Value::String(s) => !s.is_empty(),
        Value::Number(n) => n.as_f64().map(|f| f != 0.0).unwrap_or(true),
        _ => true,
    }
}
fn jstr(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => "null".into(),
        o => o.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn groups_and_sorts() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE code_signals (origin TEXT, category TEXT, kind TEXT, severity TEXT, path TEXT, line INTEGER, detail TEXT); \
          INSERT INTO code_signals VALUES ('atlas','safety','x','low','a',1,'d'),('atlas','safety','x','high','b',2,'d');").unwrap();
        let r = codeSignalsRead(&json!({}), &c).unwrap();
        assert_eq!(r["total"], json!(2));
        assert_eq!(r["counts"]["high"], json!(1));
        assert_eq!(r["rows"][0]["severity"], json!("high")); // high sorts first
        assert_eq!(r["by_category"]["safety"]["x"]["low"], json!(1));
    }
}
