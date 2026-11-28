//! blueprint-compose.rs — twin of skills/blueprint/blueprint-compose.mjs (db lane; db unused). NAMES
//! ARE CONTRACT: `blueprintCompose` (named default). Deterministic file-source composer from blocks.
// QUALITY: 82/100 — whole-file snippet path + per-child path; import hoist/dedupe (insertion order)
use rusqlite::Connection;
use serde_json::{json, Value};
use std::collections::HashSet;

fn js_str(v: &Value) -> String { match v { Value::String(s) => s.clone(), Value::Null => String::new(), o => o.to_string() } }
// dedupe trimmed non-empty imports, preserving first-seen order
fn collect_imports(imports: &Value, order: &mut Vec<String>, seen: &mut HashSet<String>) {
    if let Some(arr) = imports.as_array() {
        for i in arr {
            let t = js_str(i).trim().to_string();
            if !t.is_empty() && seen.insert(t.clone()) { order.push(t); }
        }
    }
}
// filter(Boolean).join(sep) over string parts
fn join_nonempty(parts: &[String], sep: &str) -> String {
    parts.iter().filter(|s| !s.is_empty()).cloned().collect::<Vec<_>>().join(sep)
}

pub fn blueprintCompose(input: &Value, _db: &Connection) -> Result<Value, String> {
    let block = input.get("block").filter(|v| !v.is_null()).cloned().unwrap_or_else(|| if input.is_object() { input.clone() } else { json!({}) });
    let authored = input.get("authored").filter(|v| v.is_object()).cloned().unwrap_or(json!({}));
    // JS `{ path: block?.path, ... }`: an absent block.path is `undefined` → JSON.stringify OMITS the
    // key entirely (a present-but-null path is kept). Mirror that: include `path` only when the key exists.
    let has_path = block.get("path").is_some();
    let path = block.get("path").cloned().unwrap_or(Value::Null);
    let finish = |mut o: Value| -> Value { if !has_path { o.as_object_mut().unwrap().remove("path"); } o };

    // WHOLE-FILE snippet: when a file-level snippet exists, IT is the source (no per-child concat).
    let file_key = js_str(block.get("id").filter(|v| !v.is_null()).or_else(|| block.get("path").filter(|v| !v.is_null())).unwrap_or(&Value::Null));
    if let Some(whole) = authored.get(&file_key) {
        let code_raw = whole.get("code").map(js_str).unwrap_or_default();
        if !code_raw.trim().is_empty() {
            let (mut order, mut seen) = (Vec::new(), HashSet::new());
            collect_imports(whole.get("imports").unwrap_or(&Value::Null), &mut order, &mut seen);
            let code = code_raw.trim_end().to_string();
            let source = join_nonempty(&[order.join("\n"), code], "\n\n") + "\n";
            return Ok(finish(json!({ "path": path, "source": source, "block_count": 1, "import_count": order.len(), "whole_file": true })));
        }
    }

    // per-child path
    let empty = vec![];
    let tops = block.get("children").and_then(|v| v.as_array()).unwrap_or(&empty);
    let (mut order, mut seen) = (Vec::new(), HashSet::new());
    let mut blocks: Vec<String> = Vec::new();
    for child in tops {
        let child_id = js_str(child.get("id").unwrap_or(&Value::Null));
        let fallback = json!({ "code": child.get("code").cloned().unwrap_or(Value::Null), "imports": child.get("imports").cloned().unwrap_or(Value::Null) });
        let out = authored.get(&child_id).cloned().unwrap_or(fallback);
        collect_imports(out.get("imports").unwrap_or(&Value::Null), &mut order, &mut seen);
        let code = out.get("code").map(js_str).unwrap_or_default().trim_end().to_string();
        if !code.is_empty() { blocks.push(code); }
    }
    let import_block = order.join("\n");
    let body = blocks.join("\n\n");
    let mut source = join_nonempty(&[import_block.clone(), body.clone()], "\n\n");
    if !import_block.is_empty() || !body.is_empty() { source.push('\n'); }
    Ok(finish(json!({ "path": path, "source": source, "block_count": tops.len(), "import_count": order.len() })))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn composes() {
        let c = Connection::open_in_memory().unwrap();
        let block = json!({ "path": "src/x.ts", "children": [{ "id": "fnA", "type": "function" }, { "id": "fnB", "type": "function" }] });
        let authored = json!({
            "fnA": { "code": "export function a() { return 1; }", "imports": ["import { User } from './types';"] },
            "fnB": { "code": "export function b() { return 2; }", "imports": ["import { User } from './types';"] }
        });
        let r = blueprintCompose(&json!({ "block": block, "authored": authored }), &c).unwrap();
        assert_eq!(r["block_count"], json!(2));
        assert_eq!(r["import_count"], json!(1)); // deduped
        assert!(r["source"].as_str().unwrap().starts_with("import { User } from './types';\n\nexport function a"));
        // whole-file path
        let w = blueprintCompose(&json!({ "block": { "id": "F", "path": "f.ts" }, "authored": { "F": { "code": "whole file\n\n", "imports": [] } } }), &c).unwrap();
        assert_eq!(w["whole_file"], json!(true));
        assert_eq!(w["source"], json!("whole file\n"));
    }
}
