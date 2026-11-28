//! db_json — SENTINEL-OWNED db-lane helper (like js_math, not a core twin). `db.prepare(sql).all()`
//! in JS returns an array of row objects; this runs the same query on rusqlite and returns the
//! same JSON shape (column name → value), so read-only list skills port to a one-liner and only
//! their SQL-building LOGIC is what conformance actually judges.
use rusqlite::types::ValueRef;
use rusqlite::Connection;
use serde_json::{json, Value};

fn cell(v: ValueRef) -> Value {
    match v {
        ValueRef::Null => Value::Null,
        ValueRef::Integer(i) => Value::from(i),
        ValueRef::Real(f) => Value::from(f),
        ValueRef::Text(t) => Value::from(String::from_utf8_lossy(t).into_owned()),
        // list skills don't select blobs; represent defensively rather than panic
        ValueRef::Blob(b) => Value::from(base64_like(b)),
    }
}
fn base64_like(b: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(b)
}

fn to_sql(v: &Value) -> rusqlite::types::Value {
    use rusqlite::types::Value as S;
    match v {
        Value::Null => S::Null,
        Value::Bool(b) => S::Integer(*b as i64),
        Value::Number(n) => n.as_i64().map(S::Integer).unwrap_or_else(|| S::Real(n.as_f64().unwrap())),
        Value::String(s) => S::Text(s.clone()),
        other => S::Text(other.to_string()),
    }
}

/// Mirror node:sqlite `prepare(sql).run(...params)`: execute a write and return
/// `(changes, lastInsertRowid)`. On a constraint violation returns the rusqlite error string
/// (which carries "UNIQUE constraint failed"), so twins can branch on `.contains("UNIQUE")`
/// exactly as the JS branches on `e.message.includes('UNIQUE')`.
pub fn run_write(db: &Connection, sql: &str, params: &[Value]) -> Result<(usize, i64), String> {
    let sql_params: Vec<rusqlite::types::Value> = params.iter().map(to_sql).collect();
    let changes = db.execute(sql, rusqlite::params_from_iter(sql_params)).map_err(|e| e.to_string())?;
    Ok((changes, db.last_insert_rowid()))
}

/// Run `sql` with positional `params`, return the rows as a JSON array of {col: value} objects —
/// the same shape node:sqlite's `.all(...params)` yields.
pub fn query_rows(db: &Connection, sql: &str, params: &[Value]) -> Result<Value, String> {
    let mut stmt = db.prepare(sql).map_err(|e| e.to_string())?;
    let cols: Vec<String> = (0..stmt.column_count()).map(|i| stmt.column_name(i).unwrap_or("").to_string()).collect();
    let sql_params: Vec<rusqlite::types::Value> = params.iter().map(to_sql).collect();
    let rows = stmt
        .query_map(rusqlite::params_from_iter(sql_params), |row| {
            let mut obj = serde_json::Map::new();
            for (i, name) in cols.iter().enumerate() {
                obj.insert(name.clone(), cell(row.get_ref(i).unwrap()));
            }
            Ok(Value::Object(obj))
        })
        .map_err(|e| e.to_string())?;
    let out: Vec<Value> = rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())?;
    Ok(json!(out))
}
