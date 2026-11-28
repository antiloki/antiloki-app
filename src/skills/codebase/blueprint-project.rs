//! blueprint-project.rs — twin of skills/codebase/blueprint-project.mjs (db WRITE lane). NAMES ARE
//! CONTRACT: `blueprintProject` (default). Projects a designed blueprint's Blocks into the code graph
//! as origin='blueprint:<slug>@<version>' code_nodes (+ dataflow code_edges), so conformance-verify
//! can diff "designed" vs "exists". Best-effort by design: internal failures (bad JSON, a write that
//! somehow throws) are caught and turned into {projected:false,reason} rather than propagating, so a
//! projection hiccup never breaks the PLAN stage — mirrored here via an inner closure whose Err is
//! converted to that same graceful shape. Delegates the walk to the already-converted block-shape twin
//! (walkBlocks/normalizeBlueprint) and the actual write to the already-converted atlas-build twin.
use rusqlite::Connection;
use serde_json::{json, Map, Value};

// Transcribed verbatim from skills/codebase/code-schema.mjs (data const, not a skill call).
const NODE_TYPES: [&str; 11] = [
    "file", "function", "class", "method", "component", "hook",
    "condition", "loop", "import", "type", "const",
];

fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) } // JS `??` right operand
// = JS String(v) where v may be an absent field (not just null): absent -> "undefined".
fn js_string(v: Option<&Value>) -> String {
    match v {
        None => "undefined".into(),
        Some(Value::Null) => "null".into(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(),
        Some(o) => o.to_string(),
    }
}
fn tmpl(v: Option<&Value>) -> String { js_string(v) } // = JS `${v}`

pub fn blueprintProject(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS: const { db } = context ?? {}; if (!db) throw err.contextMissing(...) — the db lane always
    // injects a live connection, so that guard is unreachable here (guard-throw, not fixture-exercised).
    let slug = match input.get("contract_slug").and_then(|v| v.as_str()) {
        Some(s) if !s.is_empty() => s.to_string(),
        _ => return Err("blueprint-project: contract_slug is required".into()), // guard-throw, not fixture-exercised
    };

    let rows = crate::db_json::query_rows(
        db,
        "SELECT version, blueprint_json FROM blueprints
     WHERE contract_slug = ? AND superseded_by IS NULL
     ORDER BY version DESC LIMIT 1",
        &[json!(slug)],
    )?;
    let row = match rows.as_array().and_then(|a| a.first()) {
        Some(r) => r.clone(),
        None => return Ok(json!({ "projected": false, "reason": "no active blueprint for contract" })),
    };

    // ── try { ... } catch (e) { return {projected:false, reason:`projection error: ${e.message}`} } ──
    let result: Result<Value, String> = (|| -> Result<Value, String> {
        let parsed: Value = match row.get("blueprint_json") {
            Some(Value::String(s)) => serde_json::from_str(s).unwrap_or_else(|_| json!({})),
            _ => Value::Null, // SQL NULL -> JS null -> JSON.parse(null) parses "null" -> null (no catch)
        };
        let bp = crate::blueprint_block_shape::normalizeBlueprint(&parsed);
        let version = row.get("version").cloned().unwrap_or(Value::Null);
        let origin = format!("blueprint:{}@{}", slug, tmpl(Some(&version)));

        // Flatten blocks -> nodes (see block-shape.mjs walkBlocks for the recursion contract).
        let mut nodes: Vec<Value> = Vec::new();
        let mut id_map: std::collections::HashMap<String, String> = std::collections::HashMap::new();
        let blocks_v = bp.get("blocks").cloned().unwrap_or_else(|| json!([]));
        {
            let mut visit = |block: &Value, file_id: &Value| {
                let path = {
                    let base: Value = if !file_id.is_null() {
                        file_id.clone()
                    } else if let Some(p) = block.get("path").filter(|v| !v.is_null()) {
                        p.clone()
                    } else if let Some(i) = block.get("id").filter(|v| !v.is_null()) {
                        i.clone()
                    } else {
                        json!("unknown")
                    };
                    js_string(Some(&base)).replace('\\', "/")
                };
                let name_v = block.get("name");
                let named = matches!(name_v, Some(Value::String(s)) if !s.trim().is_empty());
                let block_type = block.get("type").and_then(|v| v.as_str());
                // A non-file block WITHOUT a real symbol name promises nothing nameable — skip it.
                if block_type != Some("file") && !named { return; }

                let name_or_intent: Option<Value> = if let Some(n) = block.get("name").filter(|v| !v.is_null()) {
                    Some(n.clone())
                } else if let Some(i) = block.get("intent").filter(|v| !v.is_null()) {
                    Some(i.clone())
                } else {
                    None
                };
                let inner_key = format!("{}#{}", path, match &name_or_intent { Some(v) => tmpl(Some(v)), None => String::new() });
                let key = match block.get("id").filter(|v| !v.is_null()) {
                    Some(v) => js_string(Some(v)),
                    None => inner_key,
                };
                let node_id = format!("{}::{}", origin, key);
                id_map.insert(key, node_id.clone());

                let type_str = match block_type {
                    Some(t) if NODE_TYPES.contains(&t) => t.to_string(),
                    _ => "function".to_string(),
                };
                let name_str = if named {
                    match name_v { Some(Value::String(s)) => s.trim().to_string(), _ => path.clone() }
                } else {
                    path.clone()
                };
                let parent_id = if block_type == Some("file") || file_id.is_null() {
                    Value::Null
                } else {
                    json!(format!("{}::{}", origin, js_string(Some(file_id))))
                };

                let mut node = Map::new();
                node.insert("id".into(), json!(node_id));
                node.insert("path".into(), json!(path));
                node.insert("type".into(), json!(type_str));
                node.insert("name".into(), json!(name_str));
                node.insert("signature".into(), nz(block.get("signature")).cloned().unwrap_or(Value::Null));
                node.insert("intent".into(), nz(block.get("intent")).cloned().unwrap_or(Value::Null));
                node.insert("parent_id".into(), parent_id);
                nodes.push(Value::Object(node));
            };
            crate::blueprint_block_shape::walkBlocks(&blocks_v, &mut visit, &Value::Null);
        }

        // Bindings -> edges, only where BOTH endpoints are projected blocks.
        let mut edges: Vec<Value> = Vec::new();
        if let Some(bindings) = bp.get("bindings").and_then(|v| v.as_array()) {
            for b in bindings {
                let from_key = js_string(b.get("from"));
                let to_key = js_string(b.get("to"));
                if let (Some(from_id), Some(to_id)) = (id_map.get(&from_key), id_map.get(&to_key)) {
                    let mut edge = Map::new();
                    edge.insert("from_node".into(), json!(from_id));
                    edge.insert("to_node".into(), json!(to_id));
                    edge.insert("type".into(), json!("dataflow"));
                    edge.insert("output".into(), nz(b.get("output")).cloned().unwrap_or(Value::Null));
                    edge.insert("input".into(), nz(b.get("input")).cloned().unwrap_or(Value::Null));
                    edges.push(Value::Object(edge));
                }
            }
        }

        // Re-project: drop ALL prior blueprint partitions for this contract, then write the current one.
        crate::db_json::run_write(db, "DELETE FROM code_edges WHERE origin LIKE 'blueprint:' || ? || '@%'", &[json!(slug)])?;
        crate::db_json::run_write(db, "DELETE FROM code_nodes WHERE origin LIKE 'blueprint:' || ? || '@%'", &[json!(slug)])?;
        crate::codebase_atlas_build::atlasBuild(&json!({ "origin": origin, "nodes": nodes, "edges": edges }), db)?;

        Ok(json!({
            "projected": true,
            "origin": origin,
            "version": version,
            "nodes": nodes.len() as i64,
            "edges": edges.len() as i64,
        }))
    })();

    match result {
        Ok(v) => Ok(v),
        Err(e) => Ok(json!({ "projected": false, "reason": format!("projection error: {}", e) })),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCHEMA: &str = "
        CREATE TABLE blueprints (
          id INTEGER PRIMARY KEY AUTOINCREMENT, contract_slug TEXT NOT NULL, version INTEGER NOT NULL DEFAULT 1,
          blueprint_json TEXT NOT NULL DEFAULT '{}', superseded_by INTEGER
        );
        CREATE TABLE code_nodes (
          id TEXT PRIMARY KEY, origin TEXT, path TEXT, type TEXT, name TEXT, signature TEXT,
          intent TEXT, parent_id TEXT, span_json TEXT, content_hash TEXT, version INTEGER, exported INTEGER
        );
        CREATE TABLE code_edges (
          id INTEGER PRIMARY KEY AUTOINCREMENT, origin TEXT, from_node TEXT, to_node TEXT,
          type TEXT, output TEXT, input TEXT
        , kind TEXT);
    ";

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(SCHEMA).unwrap();
        c
    }

    fn seed(c: &Connection, slug: &str, version: i64, bp: &Value, superseded_by: Option<i64>) {
        c.execute(
            "INSERT INTO blueprints (contract_slug, version, blueprint_json, superseded_by) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![slug, version, serde_json::to_string(bp).unwrap(), superseded_by],
        ).unwrap();
    }

    #[test]
    fn flattens_a_blueprint_into_code_nodes() {
        let c = db();
        let bp = json!({
            "blocks": [{
                "id": "src/auth.ts", "type": "file", "path": "src/auth.ts",
                "children": [{ "id": "validate", "type": "function", "name": "validate", "intent": "reject an empty/expired token", "signature": "(token)" }]
            }],
            "bindings": []
        });
        seed(&c, "FOO", 1, &bp, None);

        let r = blueprintProject(&json!({ "contract_slug": "FOO" }), &c).unwrap();
        assert_eq!(r["projected"], json!(true));
        assert_eq!(r["origin"], json!("blueprint:FOO@1"));
        assert_eq!(r["nodes"], json!(2)); // the file + validate

        let fn_node = crate::db_json::query_rows(&c, "SELECT name, path, type FROM code_nodes WHERE origin='blueprint:FOO@1' AND type='function'", &[]).unwrap();
        assert_eq!(fn_node[0], json!({ "name": "validate", "path": "src/auth.ts", "type": "function" }));
    }

    #[test]
    fn no_active_blueprint_is_a_graceful_skip() {
        let c = db();
        let r = blueprintProject(&json!({ "contract_slug": "NONE" }), &c).unwrap();
        assert_eq!(r, json!({ "projected": false, "reason": "no active blueprint for contract" }));
    }

    #[test]
    fn nameless_module_wrapper_projects_no_phantom_function() {
        let c = db();
        let bp = json!({
            "blocks": [{
                "id": "src/components/ClickMeButton.jsx", "type": "file", "path": "src/components/ClickMeButton.jsx",
                "intent": "centered click-me button with localStorage count",
                "children": [{ "id": "src/components/ClickMeButton.jsx.main", "type": "module", "intent": "the whole component", "children": [] }]
            }],
            "bindings": []
        });
        seed(&c, "R2", 1, &bp, None);
        let r = blueprintProject(&json!({ "contract_slug": "R2" }), &c).unwrap();
        assert_eq!(r["projected"], json!(true));
        assert_eq!(r["nodes"], json!(1)); // only the file node projects
        let n = crate::db_json::query_rows(&c, "SELECT COUNT(*) n FROM code_nodes WHERE origin='blueprint:R2@1' AND type='function'", &[]).unwrap();
        assert_eq!(n[0]["n"], json!(0));
    }

    #[test]
    fn named_child_projects_under_its_symbol_name() {
        let c = db();
        let bp = json!({
            "blocks": [{
                "id": "src/x.jsx", "type": "file", "path": "src/x.jsx",
                "children": [{ "id": "src/x.jsx.ClickMeButton", "type": "component", "name": "ClickMeButton", "signature": "()", "intent": "the button", "children": [] }]
            }],
            "bindings": []
        });
        seed(&c, "NAMED", 1, &bp, None);
        blueprintProject(&json!({ "contract_slug": "NAMED" }), &c).unwrap();
        let node = crate::db_json::query_rows(&c, "SELECT name, type FROM code_nodes WHERE origin='blueprint:NAMED@1' AND type='component'", &[]).unwrap();
        assert_eq!(node[0], json!({ "name": "ClickMeButton", "type": "component" }));
    }

    #[test]
    fn reproject_supersedes_prior_version_and_drops_old_projection() {
        let c = db();
        let bp1 = json!({ "blocks": [{ "id": "a.js", "type": "file", "path": "a.js", "children": [] }], "bindings": [] });
        let bp2 = json!({ "blocks": [{ "id": "b.js", "type": "file", "path": "b.js", "children": [] }], "bindings": [] });
        seed(&c, "BAZ", 1, &bp1, None);
        blueprintProject(&json!({ "contract_slug": "BAZ" }), &c).unwrap();
        c.execute("UPDATE blueprints SET superseded_by = id WHERE contract_slug='BAZ' AND version=1", []).unwrap();
        seed(&c, "BAZ", 2, &bp2, None);

        let r = blueprintProject(&json!({ "contract_slug": "BAZ" }), &c).unwrap();
        assert_eq!(r["origin"], json!("blueprint:BAZ@2"));
        let v1 = crate::db_json::query_rows(&c, "SELECT COUNT(*) n FROM code_nodes WHERE origin='blueprint:BAZ@1'", &[]).unwrap();
        assert_eq!(v1[0]["n"], json!(0), "v1 projection dropped");
        let v2 = crate::db_json::query_rows(&c, "SELECT COUNT(*) n FROM code_nodes WHERE origin='blueprint:BAZ@2'", &[]).unwrap();
        assert!(v2[0]["n"].as_i64().unwrap() > 0);
    }

    #[test]
    fn invalid_json_blueprint_falls_back_to_empty_projection() {
        let c = db();
        c.execute(
            "INSERT INTO blueprints (contract_slug, version, blueprint_json, superseded_by) VALUES ('BADJSON', 1, 'not valid json{', NULL)",
            [],
        ).unwrap();
        let r = blueprintProject(&json!({ "contract_slug": "BADJSON" }), &c).unwrap();
        assert_eq!(r, json!({ "projected": true, "origin": "blueprint:BADJSON@1", "version": 1, "nodes": 0, "edges": 0 }));
    }

    #[test]
    fn dropped_binding_when_one_endpoint_unresolved_and_id_absent_key_fallback() {
        let c = db();
        let bp = json!({
            "blocks": [
                { "path": "src/nofile-id.ts", "type": "file", "children": [] },
                { "type": "function", "name": "orphan" }
            ],
            // the file block has no "id", so its idMap key falls back to `${path}#${name??intent??''}`
            // = "src/nofile-id.ts#" (block-shape key-template branch) — "from" resolves against THAT key;
            // "to" never resolves (no node carries that key) so the binding is dropped either way.
            "bindings": [{ "from": "src/nofile-id.ts#", "to": "does-not-exist" }]
        });
        seed(&c, "ORPHAN", 1, &bp, None);
        let r = blueprintProject(&json!({ "contract_slug": "ORPHAN" }), &c).unwrap();
        assert_eq!(r["nodes"], json!(2));
        assert_eq!(r["edges"], json!(0));
        let orphan = crate::db_json::query_rows(&c, "SELECT path, parent_id FROM code_nodes WHERE origin='blueprint:ORPHAN@1' AND name='orphan'", &[]).unwrap();
        assert_eq!(orphan[0], json!({ "path": "unknown", "parent_id": Value::Null }));
    }
}
