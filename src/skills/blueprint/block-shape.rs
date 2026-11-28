//! block-shape.rs — twin of skills/blueprint/block-shape.mjs (helper module). NAMES ARE CONTRACT:
//! walkBlocks · flattenBlocks · normalizeBlueprint. The canonical Block/Binding shape.
// QUALITY: 82/100 — normalizeBlueprint conforms directly; walkBlocks (callback) + flattenBlocks (Map)
//          are ported faithfully and verified TRANSITIVELY (operator-accepted) when dependents conform.
use serde_json::{json, Map, Value};

fn js_str(v: &Value) -> String { match v { Value::String(s) => s.clone(), o => o.to_string() } }

// TRANSITIVE: depth-first walk; f(block, fileId) per block; fileId = nearest enclosing file block's id.
pub fn walkBlocks(blocks: &Value, f: &mut dyn FnMut(&Value, &Value), file_id: &Value) {
    let empty = vec![];
    for b in blocks.as_array().unwrap_or(&empty) {
        if b.is_null() { continue; }
        let fid = if b.get("type").and_then(|v| v.as_str()) == Some("file") {
            b.get("id").filter(|v| !v.is_null())
                .or_else(|| b.get("path").filter(|v| !v.is_null()))
                .cloned().unwrap_or_else(|| file_id.clone())
        } else { file_id.clone() };
        f(b, &fid);
        if let Some(children) = b.get("children").filter(|v| v.is_array()) {
            walkBlocks(children, f, &fid);
        }
    }
}

// TRANSITIVE: Map(id → { block, fileId }) over every block carrying an id (serde object, own-key lookup).
pub fn flattenBlocks(blocks: &Value) -> Value {
    let mut map: Map<String, Value> = Map::new();
    {
        let mut collect = |b: &Value, file_id: &Value| {
            if let Some(id) = b.get("id").filter(|v| !v.is_null()) {
                map.insert(js_str(id), json!({ "block": b, "fileId": file_id }));
            }
        };
        walkBlocks(blocks, &mut collect, &Value::Null);
    }
    Value::Object(map)
}

// Conforms directly: ensure the canonical {blocks,bindings,coverage} shape.
pub fn normalizeBlueprint(bp: &Value) -> Value {
    let x = if bp.is_object() { bp.as_object().unwrap().clone() } else { Map::new() };
    let mut out = x.clone();
    let arr = |k: &str| x.get(k).filter(|v| v.is_array()).cloned().unwrap_or(json!([]));
    out.insert("blocks".into(), arr("blocks"));
    out.insert("bindings".into(), arr("bindings"));
    out.insert("coverage".into(), arr("coverage"));
    Value::Object(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn shape_and_walk() {
        assert_eq!(normalizeBlueprint(&json!({})), json!({ "blocks": [], "bindings": [], "coverage": [] }));
        assert_eq!(normalizeBlueprint(&Value::Null), json!({ "blocks": [], "bindings": [], "coverage": [] }));
        let bp = json!({ "blocks": [{ "id": "f", "type": "file", "path": "f", "children": [{ "id": "f.fn", "type": "function", "children": [] }] }], "bindings": [], "coverage": [] });
        assert_eq!(normalizeBlueprint(&bp), bp);
        // walkBlocks / flattenBlocks (transitive helpers): fileId is the enclosing file block
        let flat = flattenBlocks(&bp["blocks"]);
        assert_eq!(flat["f.fn"]["fileId"], json!("f"));
        assert_eq!(flat.as_object().unwrap().len(), 2);
    }
}
