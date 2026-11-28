//! code-scope-set.rs — twin of skills/codebase/code-scope-set.mjs (db WRITE lane). NAMES ARE
//! CONTRACT: `codeScopeSet` (default). UPSERT one code_scope row per origin (exclude/include globs).
// QUALITY: 82/100 — strArr normalize (drop non-string / blank, trim), datetime('now') frozen, ON CONFLICT upsert
use rusqlite::{params, Connection};
use serde_json::{json, Map, Value};

fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) }

// JS: Array.isArray(v) ? v.filter(x => typeof x === 'string' && x.trim()).map(x => x.trim()) : []
fn str_arr(v: Option<&Value>) -> Vec<String> {
    match v.and_then(|x| x.as_array()) {
        Some(arr) => arr.iter()
            .filter_map(|x| x.as_str())
            .filter(|s| !s.trim().is_empty())
            .map(|s| s.trim().to_string())
            .collect(),
        None => Vec::new(),
    }
}

pub fn codeScopeSet(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS: input?.origin ?? 'atlas'  (null/undefined → 'atlas')
    let origin = nz(input.get("origin")).cloned().unwrap_or_else(|| json!("atlas"));
    let exclude = str_arr(input.get("exclude"));
    let include = str_arr(input.get("include"));

    // JS: JSON.stringify(exclude / include)  (compact, no spaces — matches serde_json)
    let exclude_json = serde_json::to_string(&json!(exclude)).unwrap();
    let include_json = serde_json::to_string(&json!(include)).unwrap();
    // origin binds as its string form ('atlas' default / caller string); PK is TEXT.
    let origin_bind = match &origin { Value::String(s) => s.clone(), other => other.to_string() };

    // datetime('now') → frozen instant 2026-01-01 00:00:00 (conform-db clock is pinned)
    db.execute(
        &format!("INSERT INTO code_scope (origin, exclude_json, include_json, updated_at) \
         VALUES (?1, ?2, ?3, {}) \
         ON CONFLICT(origin) DO UPDATE SET \
           exclude_json = excluded.exclude_json, include_json = excluded.include_json, updated_at = {}", crate::clock::now_sql(), crate::clock::now_sql()),
        params![origin_bind, exclude_json, include_json],
    ).map_err(|e| e.to_string())?;

    let mut out = Map::new();
    out.insert("origin".into(), origin);
    out.insert("exclude".into(), json!(exclude));
    out.insert("include".into(), json!(include));
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn db0() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE code_scope (origin TEXT PRIMARY KEY, exclude_json TEXT NOT NULL DEFAULT '[]', include_json TEXT NOT NULL DEFAULT '[]', updated_at TEXT NOT NULL DEFAULT (datetime('now')))").unwrap();
        c
    }
    #[test]
    fn upserts_and_normalizes() {
        let c = db0();
        // fresh insert: drop non-string (42) + blank ('  '), trim survivors
        let r = codeScopeSet(&json!({ "origin": "custom", "exclude": ["node_modules", "  dist  ", 42, "", "src"], "include": ["*.js"] }), &c).unwrap();
        assert_eq!(r, json!({ "origin": "custom", "exclude": ["node_modules", "dist", "src"], "include": ["*.js"] }));
        let (ex, up): (String, String) = c.query_row("SELECT exclude_json, updated_at FROM code_scope WHERE origin='custom'", [], |row| Ok((row.get(0)?, row.get(1)?))).unwrap();
        assert_eq!(ex, "[\"node_modules\",\"dist\",\"src\"]");
        assert_eq!(up, "2026-01-01 00:00:00"); // frozen

        // default origin + non-array inputs → []
        let r2 = codeScopeSet(&json!({ "exclude": "nope", "include": null }), &c).unwrap();
        assert_eq!(r2, json!({ "origin": "atlas", "exclude": [], "include": [] }));

        // ON CONFLICT DO UPDATE overwrites the existing atlas row
        let r3 = codeScopeSet(&json!({ "exclude": ["only"] }), &c).unwrap();
        assert_eq!(r3["origin"], json!("atlas"));
        let ex2: String = c.query_row("SELECT exclude_json FROM code_scope WHERE origin='atlas'", [], |row| row.get(0)).unwrap();
        assert_eq!(ex2, "[\"only\"]");
        let n: i64 = c.query_row("SELECT COUNT(*) FROM code_scope", [], |row| row.get(0)).unwrap();
        assert_eq!(n, 2); // atlas + custom, upsert did not duplicate
    }
}
