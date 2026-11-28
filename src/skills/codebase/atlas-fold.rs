//! atlas-fold.rs — twin of skills/codebase/atlas-fold.mjs (db WRITE lane). NAMES ARE CONTRACT:
//! `atlasFold` (named default). Relinks verified blueprint nodes to the atlas nodes they became.
// QUALITY: 83/100 — per-link lookup + two UPDATEs; String()-coerced ids, notFound on either side
use rusqlite::Connection;
use serde_json::{json, Value};

fn s(v: Option<&Value>) -> String {
    match v {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Null) | None => "undefined".into(), // String(undefined)
        Some(o) => o.to_string(),
    }
}

pub fn atlasFold(input: &Value, db: &Connection) -> Result<Value, String> {
    let empty = vec![];
    let links = input.get("links").and_then(|v| v.as_array()).unwrap_or(&empty);
    if links.is_empty() {
        return Err("atlas-fold: links is required".into());
    }
    let node = |id: &str| -> Result<Option<Value>, String> {
        Ok(crate::db_json::query_rows(db, "SELECT id, origin FROM code_nodes WHERE id = ?", &[json!(id)])?
            .as_array()
            .and_then(|a| a.first())
            .cloned())
    };
    let mut folded = 0i64;
    for l in links {
        let bp_id = s(l.get("blueprint_node"));
        let bp = match node(&bp_id)? {
            Some(r) => r,
            None => return Err(format!("atlas-fold: blueprint_node '{bp_id}' not found")),
        };
        let at_id = s(l.get("atlas_node"));
        let at = match node(&at_id)? {
            Some(r) => r,
            None => return Err(format!("atlas-fold: atlas_node '{at_id}' not found")),
        };
        // blueprint node -> atlas node it became; atlas node -> the blueprint origin that produced it
        crate::db_json::run_write(db, "UPDATE code_nodes SET became = ? WHERE id = ?", &[at.get("id").cloned().unwrap_or(Value::Null), bp.get("id").cloned().unwrap_or(Value::Null)])?;
        crate::db_json::run_write(db, "UPDATE code_nodes SET from_blueprint = ? WHERE id = ?", &[bp.get("origin").cloned().unwrap_or(Value::Null), at.get("id").cloned().unwrap_or(Value::Null)])?;
        folded += 1;
    }
    Ok(json!({ "folded": folded }))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn folds_and_errors_on_missing() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE code_nodes (id TEXT, origin TEXT, became TEXT, from_blueprint TEXT); INSERT INTO code_nodes (id,origin) VALUES ('bp1','plan'),('at1','atlas');").unwrap();
        let r = atlasFold(&json!({ "links": [{ "blueprint_node": "bp1", "atlas_node": "at1" }] }), &c).unwrap();
        assert_eq!(r["folded"], json!(1));
        let became: String = c.query_row("SELECT became FROM code_nodes WHERE id='bp1'", [], |r| r.get(0)).unwrap();
        assert_eq!(became, "at1");
        assert!(atlasFold(&json!({ "links": [{ "blueprint_node": "nope", "atlas_node": "at1" }] }), &c).is_err());
        assert!(atlasFold(&json!({ "links": [] }), &c).is_err());
    }
}
