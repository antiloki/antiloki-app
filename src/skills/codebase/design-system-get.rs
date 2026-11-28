//! design-system-get.rs — twin of skills/codebase/design-system-get.mjs (db READ lane). NAMES ARE
//! CONTRACT: `designSystemGet` (default). The Design screen's data source (palette + components + audit).
// QUALITY: 79/100 — palette grouped by category (declared/literals), component gallery, cross-module design-audit
use rusqlite::Connection;
use serde_json::{json, Map, Value};

fn safe_parse(v: Option<&Value>) -> Value {
    // JSON.parse(s ?? '[]') → [] on error/missing
    match v.and_then(|x| x.as_str()) { Some(s) => serde_json::from_str(s).unwrap_or(json!([])), None => json!([]) }
}
fn truthy(v: Option<&Value>) -> bool {
    match v { None | Some(Value::Null) => false, Some(Value::Bool(b)) => *b, Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()), Some(Value::String(s)) => !s.is_empty(), Some(_) => true }
}

pub fn designSystemGet(input: &Value, db: &Connection) -> Result<Value, String> {
    let origin = input.get("origin").and_then(|v| v.as_str()).unwrap_or("atlas").to_string();
    let tokens = crate::db_json::query_rows(db, "SELECT token, value, category, declared, usage_count, sources_json FROM code_design_tokens WHERE origin = ? ORDER BY declared DESC, category, usage_count DESC", &[json!(origin)])
        .ok().and_then(|r| r.as_array().cloned()).unwrap_or_default();
    let components = crate::db_json::query_rows(db, "SELECT name, path, line, props_json, renders_json, tokens_json FROM code_design_components WHERE origin = ? ORDER BY path, line", &[json!(origin)])
        .ok().and_then(|r| r.as_array().cloned()).unwrap_or_default();

    let mut palette = Map::new();
    for t in &tokens {
        let cat = t.get("category").cloned().unwrap_or(Value::Null);
        let cat_key = match &cat { Value::String(s) => s.clone(), Value::Null => "null".into(), o => o.to_string() };
        let entry = palette.entry(cat_key).or_insert_with(|| json!({ "declared": [], "literals": [] }));
        let row = json!({ "token": t.get("token").cloned().unwrap_or(Value::Null), "value": t.get("value").cloned().unwrap_or(Value::Null), "usage_count": t.get("usage_count").cloned().unwrap_or(Value::Null), "sources": safe_parse(t.get("sources_json")) });
        let bucket = if truthy(t.get("declared")) { "declared" } else { "literals" };
        entry.get_mut(bucket).unwrap().as_array_mut().unwrap().push(row);
    }
    let categories: Vec<Value> = palette.keys().map(|k| json!(k)).collect();

    let comps: Vec<Value> = components.iter().map(|c| json!({ "name": c.get("name").cloned().unwrap_or(Value::Null), "path": c.get("path").cloned().unwrap_or(Value::Null), "line": c.get("line").cloned().unwrap_or(Value::Null),
        "props": safe_parse(c.get("props_json")), "renders": safe_parse(c.get("renders_json")), "tokens": safe_parse(c.get("tokens_json")) })).collect();

    let audit = crate::codebase_design_audit::designAudit(&json!({ "origin": origin }), db)?;
    let findings_len = audit.get("findings").and_then(|v| v.as_array()).map_or(0, |a| a.len());
    let declared_n = tokens.iter().filter(|t| truthy(t.get("declared"))).count();

    Ok(json!({ "ok": true, "origin": origin, "palette": Value::Object(palette), "categories": categories, "components": comps,
        "audit": { "findings": audit.get("findings").cloned().unwrap_or(json!([])), "summary": audit.get("summary").cloned().unwrap_or(json!({})) },
        "summary": { "tokens": tokens.len(), "declared": declared_n, "literals": tokens.len() - declared_n, "components": comps.len(), "drift": findings_len } }))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn gets() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE code_design_tokens (origin TEXT, token TEXT, value TEXT, category TEXT, declared INTEGER, usage_count INTEGER, sources_json TEXT); \
          CREATE TABLE code_design_components (origin TEXT, name TEXT, path TEXT, line INTEGER, props_json TEXT, renders_json TEXT, tokens_json TEXT); \
          INSERT INTO code_design_tokens VALUES ('atlas','C.p','#fff','color',1,3,'[]'),('atlas','lit','#000','color',0,1,'[]'); \
          INSERT INTO code_design_components VALUES ('atlas','Button','b.tsx',5,'[\"onClick\"]','[]','[]');").unwrap();
        let r = designSystemGet(&json!({ "origin": "atlas" }), &c).unwrap();
        assert_eq!(r["categories"], json!(["color"]));
        assert_eq!(r["palette"]["color"]["declared"].as_array().unwrap().len(), 1);
        assert_eq!(r["palette"]["color"]["literals"].as_array().unwrap().len(), 1);
        assert_eq!(r["components"][0]["props"], json!(["onClick"]));
        assert_eq!(r["summary"]["tokens"], json!(2));
        assert!(r["audit"]["findings"].is_array());
    }
}
