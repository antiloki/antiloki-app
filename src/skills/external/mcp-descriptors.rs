//! mcp-descriptors.rs — twin of skills/external/mcp-descriptors.mjs. NAMES ARE CONTRACT:
//! `mcpDescriptors` (default), `mcpName`, `findInputReceiver` (both also called directly by the JS
//! test suite as named exports).
//!
//! BESPOKE-NATIVE: builds MCP tool descriptors from the `actions` table by dispatching per-action to
//! the already-converted `extract-input-schema` and `extract-jsdoc-description` twins, both of which
//! do a real `fs::read_to_string` of the receiver skill's source file (via `findSkillPath`) when the
//! receiver isn't resolvable from an inline `source` — the same fs-touching dependency that makes
//! those two twins themselves bespoke-native. `extract-input-schema`'s twin takes `&Connection` as
//! its context parameter (mirroring the JS skill's `context` object, unused internally beyond the fs
//! read), so this skill takes the same `db: &Connection` and forwards it unchanged.
use rusqlite::Connection;
use serde_json::{json, Map, Value};

/// mcpName(actionName) — named export, standard lane. Convert a dotted/dashed action name into a
/// valid MCP tool name (snake_case). e.g. 'board-keeper.card.add' -> 'board_keeper_card_add'
#[allow(non_snake_case)]
pub fn mcpName(action_name: &str) -> String {
    action_name
        .chars()
        .map(|c| if c == '.' || c == '-' { '_' } else { c })
        .collect()
}

/// findInputReceiver(steps) — named export, standard lane. Trace an action's steps to find the
/// skill that ultimately receives the user input ($.input).
#[allow(non_snake_case)]
pub fn findInputReceiver(steps: &Value) -> Option<String> {
    let arr = steps.as_array()?;
    for step in arr {
        let input = step.get("input");
        if let Some(Value::String(s)) = input {
            if s == "$.input" {
                return step.get("skill").and_then(|v| v.as_str()).map(|s| s.to_string());
            }
        }
        if let Some(Value::Object(obj)) = input {
            if obj.get("wrappedInput").and_then(|v| v.as_str()) == Some("$.input") {
                return obj.get("skill").and_then(|v| v.as_str()).map(|s| s.to_string());
            }
        }
    }
    None
}

fn default_to_json_schema(field: &Value) -> Value {
    let def = field.get("default");
    match def {
        None => json!({}),
        Some(Value::Null) => json!({ "default": Value::Null }),
        Some(Value::String(s)) => json!({ "type": "string", "default": s }),
        Some(Value::Number(n)) => json!({ "type": "number", "default": n }),
        Some(Value::Bool(b)) => json!({ "type": "boolean", "default": b }),
        Some(Value::Array(a)) => json!({ "type": "array", "default": a }),
        Some(Value::Object(o)) => json!({ "type": "object", "default": o }),
    }
}

fn fields_to_json_schema(fields: &[Value]) -> Value {
    let mut properties = Map::new();
    let mut required: Vec<Value> = Vec::new();
    for f in fields {
        let name = f.get("name").and_then(|v| v.as_str()).unwrap_or_default().to_string();
        properties.insert(name.clone(), default_to_json_schema(f));
        if f.get("required").and_then(|v| v.as_bool()).unwrap_or(false) {
            required.push(json!(name));
        }
    }
    let mut schema = Map::new();
    schema.insert("type".into(), json!("object"));
    schema.insert("properties".into(), Value::Object(properties));
    if !required.is_empty() {
        schema.insert("required".into(), Value::Array(required));
    }
    Value::Object(schema)
}

/// mcpDescriptors(input, context) — default export, standard lane.
#[allow(non_snake_case)]
pub fn mcpDescriptors(input: &Value, db: &Connection) -> Result<Value, String> {
    let only_names: Option<Vec<String>> = input
        .as_object()
        .and_then(|o| o.get("only"))
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect());

    let mut stmt = db
        .prepare("SELECT name, description, steps_json FROM actions ORDER BY name")
        .map_err(|e| e.to_string())?;
    let rows: Vec<(String, Option<String>, String)> = stmt
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?, row.get::<_, String>(2)?))
        })
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;

    let actions: Vec<(String, Option<String>, String)> = match &only_names {
        Some(names) => rows.into_iter().filter(|(name, _, _)| names.contains(name)).collect(),
        None => rows,
    };

    let mut tools: Vec<Value> = Vec::new();
    let mut mapping = Map::new();
    let mut warnings: Vec<Value> = Vec::new();

    for (name, description, steps_json) in actions {
        let steps: Value = serde_json::from_str(&steps_json).map_err(|e| e.to_string())?;
        let receiver = findInputReceiver(&steps);

        let input_schema: Value = match &receiver {
            None => {
                warnings.push(json!({
                    "action": name,
                    "warning": "no step receives $.input; inputSchema will be empty",
                }));
                json!({ "type": "object", "properties": {} })
            }
            Some(r) => match crate::audit_extract_input_schema::extractInputSchema(&json!(r), db) {
                Ok(schema) => {
                    let fields = schema.get("fields").and_then(|v| v.as_array()).cloned().unwrap_or_default();
                    fields_to_json_schema(&fields)
                }
                Err(e) => {
                    warnings.push(json!({
                        "action": name,
                        "warning": format!("cannot extract schema from '{r}': {e}"),
                    }));
                    fields_to_json_schema(&[])
                }
            },
        };

        let mut final_description: Value = match &description {
            Some(d) => json!(d),
            None => Value::Null,
        };
        if let Some(r) = &receiver {
            if let Ok(jsdoc) = crate::audit_extract_jsdoc_description::extractJsdocDescription(&json!(r), &Value::Null) {
                if let Some(jd) = jsdoc.get("description").and_then(|v| v.as_str()) {
                    let base = description.clone().unwrap_or_default();
                    final_description = json!(format!("{base}\n\n{jd}"));
                }
            }
        }

        let tool_name = mcpName(&name);
        tools.push(json!({
            "name": tool_name,
            "description": final_description,
            "inputSchema": input_schema,
        }));
        mapping.insert(tool_name, json!(name));
    }

    Ok(json!({
        "tools": tools,
        "mapping": Value::Object(mapping),
        "warnings": warnings,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup_db() -> Connection {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(
            "CREATE TABLE actions (name TEXT PRIMARY KEY, description TEXT, steps_json TEXT);",
        )
        .unwrap();
        db
    }

    #[test]
    fn mcp_name_replaces_dots_and_dashes() {
        assert_eq!(mcpName("board-keeper.card.add"), "board_keeper_card_add");
    }

    #[test]
    fn find_input_receiver_direct_pattern() {
        let steps = json!([{ "skill": "card-add", "input": "$.input" }]);
        assert_eq!(findInputReceiver(&steps), Some("card-add".to_string()));
    }

    #[test]
    fn find_input_receiver_wrapped_pattern() {
        let steps = json!([
            { "skill": "with-audit", "input": { "skill": "card-add", "wrappedInput": "$.input" } }
        ]);
        assert_eq!(findInputReceiver(&steps), Some("card-add".to_string()));
    }

    #[test]
    fn find_input_receiver_none() {
        let steps = json!([{ "skill": "x", "input": "$.other" }]);
        assert_eq!(findInputReceiver(&steps), None);
    }

    #[test]
    fn no_receiver_produces_warning_and_empty_schema() {
        let db = setup_db();
        db.execute(
            "INSERT INTO actions (name, description, steps_json) VALUES (?1, ?2, ?3)",
            rusqlite::params![
                "test.noop",
                "does nothing",
                json!([{ "skill": "x", "input": "$.other" }]).to_string()
            ],
        )
        .unwrap();
        let out = mcpDescriptors(&json!({}), &db).unwrap();
        assert_eq!(out["tools"][0]["name"], json!("test_noop"));
        assert_eq!(out["tools"][0]["inputSchema"], json!({ "type": "object", "properties": {} }));
        assert_eq!(out["warnings"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn only_filter_restricts_actions() {
        let db = setup_db();
        db.execute(
            "INSERT INTO actions (name, description, steps_json) VALUES (?1, ?2, ?3)",
            rusqlite::params!["a.one", "one", json!([]).to_string()],
        )
        .unwrap();
        db.execute(
            "INSERT INTO actions (name, description, steps_json) VALUES (?1, ?2, ?3)",
            rusqlite::params!["a.two", "two", json!([]).to_string()],
        )
        .unwrap();
        let out = mcpDescriptors(&json!({ "only": ["a.one"] }), &db).unwrap();
        let tools = out["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["name"], json!("a_one"));
    }

    #[test]
    fn mapping_maps_tool_name_back_to_action_name() {
        let db = setup_db();
        db.execute(
            "INSERT INTO actions (name, description, steps_json) VALUES (?1, ?2, ?3)",
            rusqlite::params!["board-keeper.card.add", "adds a card", json!([]).to_string()],
        )
        .unwrap();
        let out = mcpDescriptors(&json!({}), &db).unwrap();
        assert_eq!(out["mapping"]["board_keeper_card_add"], json!("board-keeper.card.add"));
    }
}
