//! prompt-find.rs — twin of skills/ai/prompt-find.mjs (db READ lane, brain-attached). NAMES ARE
//! CONTRACT: `promptFind` (default). Resolves a prompt template by name (active version) or by
//! {name, version}, returning the row + parsed output/input field schemas + cross_field_rules.
use rusqlite::Connection;
use serde_json::{json, Map, Value};

fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}
// JS `${v}` interpolation (error-message path only).
fn tmpl(v: Option<&Value>) -> String {
    match v {
        None => "undefined".into(),
        Some(Value::Null) => "null".into(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(),
        Some(o) => o.to_string(),
    }
}

// JS `if (col) { try { x = JSON.parse(col) } catch { x = null } }` — falsy column → null,
// parse failure → null. JSON.parse("null") → null either way (same result).
fn parse_json_or_null(v: Option<&Value>) -> Value {
    if !truthy(v) {
        return Value::Null;
    }
    match v {
        Some(Value::String(s)) => serde_json::from_str(s).unwrap_or(Value::Null),
        // JSON.parse coerces its argument to string; a numeric cell parses as itself.
        Some(n @ Value::Number(_)) => serde_json::from_str(&n.to_string()).unwrap_or(Value::Null),
        _ => Value::Null,
    }
}

// Shared row mapper: JS `{ ...f, enum_values, required, min_length, max_length,
// required_sections, pattern[, grounded_refs] }`. The spread keeps the SELECT column
// positions; overrides replace in place; required_sections (and grounded_refs for
// output rows) are NEW keys and append at the end — insert order below mirrors that.
fn map_field(f: &Value, with_grounded_refs: bool) -> Value {
    let mut o: Map<String, Value> = f.as_object().cloned().unwrap_or_default();
    let enum_values = match f.get("enum_values") {
        // `f.enum_values ? f.enum_values.split('|') : null` — "" is falsy → null
        Some(Value::String(s)) if !s.is_empty() => json!(s.split('|').collect::<Vec<&str>>()),
        _ => Value::Null,
    };
    o.insert("enum_values".into(), enum_values);
    o.insert("required".into(), json!(truthy(f.get("required"))));
    // `typeof f.min_length === 'number' ? f.min_length : null` (0 is a number → kept)
    o.insert(
        "min_length".into(),
        match f.get("min_length") { Some(n @ Value::Number(_)) => n.clone(), _ => Value::Null },
    );
    o.insert(
        "max_length".into(),
        match f.get("max_length") { Some(n @ Value::Number(_)) => n.clone(), _ => Value::Null },
    );
    o.insert("required_sections".into(), parse_json_or_null(f.get("required_sections_json")));
    // `typeof f.pattern === 'string' ? f.pattern : null` ("" is a string → kept)
    o.insert(
        "pattern".into(),
        match f.get("pattern") { Some(s @ Value::String(_)) => s.clone(), _ => Value::Null },
    );
    if with_grounded_refs {
        o.insert("grounded_refs".into(), parse_json_or_null(f.get("grounded_refs_json")));
    }
    Value::Object(o)
}

pub fn promptFind(input: &Value, db: &Connection) -> Result<Value, String> {
    // `typeof input === 'string' ? input : input?.name` / version null when string input
    let name = if input.is_string() { Some(input.clone()) } else { input.get("name").cloned() };
    let version = if input.is_string() { Some(Value::Null) } else { input.get("version").cloned() };
    if !truthy(name.as_ref()) {
        return Err("prompt-find: name is required".into());
    }
    let name_v = name.unwrap_or(Value::Null);
    let by_version = truthy(version.as_ref()); // JS `version ?` — 0/null/undefined → active path

    let rows = if by_version {
        crate::db_json::query_rows(
            db,
            "SELECT * FROM brain.prompt_templates WHERE name = ? AND version = ?",
            &[name_v.clone(), version.clone().unwrap_or(Value::Null)],
        )?
    } else {
        crate::db_json::query_rows(
            db,
            "SELECT * FROM brain.prompt_templates WHERE name = ? AND superseded_by IS NULL",
            &[name_v.clone()],
        )?
    };
    let tpl = match rows.as_array().and_then(|a| a.first()) {
        Some(t) => t.clone(),
        None => {
            return Err(format!(
                "prompt-find: prompt '{}'{} not found",
                tmpl(Some(&name_v)),
                if by_version { format!(" v{}", tmpl(version.as_ref())) } else { " (active)".to_string() }
            ))
        }
    };
    let tpl_id = tpl.get("id").cloned().unwrap_or(Value::Null);

    let out_rows = crate::db_json::query_rows(
        db,
        "SELECT field_name, field_type, enum_values, required, description, order_index, \
         min_length, max_length, required_sections_json, pattern, grounded_refs_json \
         FROM brain.prompt_output_fields WHERE prompt_template_id = ? ORDER BY order_index ASC",
        &[tpl_id.clone()],
    )?;
    let output_fields: Vec<Value> = out_rows
        .as_array()
        .map(|a| a.iter().map(|f| map_field(f, true)).collect())
        .unwrap_or_default();

    let in_rows = crate::db_json::query_rows(
        db,
        "SELECT field_name, field_type, enum_values, required, description, order_index, \
         min_length, max_length, required_sections_json, pattern \
         FROM brain.prompt_input_fields WHERE prompt_template_id = ? ORDER BY order_index ASC",
        &[tpl_id],
    )?;
    let input_fields: Vec<Value> = in_rows
        .as_array()
        .map(|a| a.iter().map(|f| map_field(f, false)).collect())
        .unwrap_or_default();

    let cross_field_rules = parse_json_or_null(tpl.get("cross_field_rules_json"));

    // `{ ...tpl, output_fields, input_fields, cross_field_rules }` — three appended keys
    let mut out: Map<String, Value> = tpl.as_object().cloned().unwrap_or_default();
    out.insert("output_fields".into(), json!(output_fields));
    out.insert("input_fields".into(), json!(input_fields));
    out.insert("cross_field_rules".into(), cross_field_rules);
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db0() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "ATTACH ':memory:' AS brain; \
             CREATE TABLE brain.prompt_templates ( id INTEGER PRIMARY KEY, name TEXT NOT NULL, version INTEGER NOT NULL, template_md TEXT NOT NULL, default_tier TEXT NOT NULL, default_temperature TEXT NOT NULL, max_output_tokens INTEGER NOT NULL DEFAULT 1024, superseded_by INTEGER, created_at TEXT NOT NULL, cross_field_rules_json TEXT, judging_criteria_json TEXT, system_md TEXT, phase_flow TEXT NOT NULL DEFAULT 'single', UNIQUE (name, version) ); \
             CREATE TABLE brain.prompt_output_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, grounded_refs_json TEXT, UNIQUE (prompt_template_id, field_name) ); \
             CREATE TABLE brain.prompt_input_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, UNIQUE (prompt_template_id, field_name) );",
        )
        .unwrap();
        c
    }

    fn seed(c: &Connection) {
        c.execute_batch(
            "INSERT INTO brain.prompt_templates (id, name, version, template_md, default_tier, default_temperature, max_output_tokens, superseded_by, created_at, cross_field_rules_json, judging_criteria_json, system_md, phase_flow) VALUES \
               (1, 'gate.review', 1, 'Old body v1.', 'mid', 'cold', 1024, 2, '2025-12-01 10:00:00', NULL, NULL, NULL, 'single'), \
               (2, 'gate.review', 2, 'Review {{diff}} carefully.', 'high', 'cold', 2048, NULL, '2025-12-10 10:00:00', '[{\"id\":\"r1\",\"expression\":\"true\",\"message\":\"ok\"}]', '[\"clear verdict\"]', 'You are a reviewer.', 'single'); \
             INSERT INTO brain.prompt_output_fields (id, prompt_template_id, field_name, field_type, enum_values, required, description, order_index, min_length, max_length, required_sections_json, pattern, grounded_refs_json) VALUES \
               (3, 2, 'notes',   'string', NULL,             0, NULL,          2, NULL, 200,  '{broken',                        NULL,               '[\"card_slug\"]'), \
               (1, 2, 'verdict', 'enum',   'pass|fail|skip', 1, 'the verdict', 0, NULL, NULL, NULL,                             '^(pass|fail|skip)$', NULL), \
               (2, 2, 'summary', 'string', '',               1, NULL,          1, 10,   NULL, '[\"## Findings\",\"## Verdict\"]', NULL,             '{oops'), \
               (4, 1, 'old',     'string', NULL,             1, NULL,          0, NULL, NULL, NULL,                             NULL,               NULL); \
             INSERT INTO brain.prompt_input_fields (id, prompt_template_id, field_name, field_type, enum_values, required, description, order_index, min_length, max_length, required_sections_json, pattern) VALUES \
               (1, 2, 'diff', 'string', NULL, 1, 'unified diff', 0, 1, NULL, NULL, NULL);",
        )
        .unwrap();
    }

    #[test]
    fn string_input_resolves_active_and_maps_fields() {
        let c = db0();
        seed(&c);
        let r = promptFind(&json!("gate.review"), &c).unwrap();
        assert_eq!(r["version"], json!(2));
        assert_eq!(r["superseded_by"], Value::Null);
        // output_fields ordered by order_index despite shuffled insert order
        let fields = r["output_fields"].as_array().unwrap();
        assert_eq!(fields.len(), 3);
        assert_eq!(fields[0]["field_name"], json!("verdict"));
        assert_eq!(fields[0]["enum_values"], json!(["pass", "fail", "skip"]));
        assert_eq!(fields[0]["required"], json!(true));
        assert_eq!(fields[0]["pattern"], json!("^(pass|fail|skip)$"));
        assert_eq!(fields[0]["min_length"], Value::Null);
        // "" enum_values is falsy → null; valid sections JSON parsed; broken grounded refs → null
        assert_eq!(fields[1]["enum_values"], Value::Null);
        assert_eq!(fields[1]["min_length"], json!(10));
        assert_eq!(fields[1]["required_sections"], json!(["## Findings", "## Verdict"]));
        assert_eq!(fields[1]["grounded_refs"], Value::Null);
        // broken sections JSON → null; valid grounded refs parsed; required 0 → false
        assert_eq!(fields[2]["required"], json!(false));
        assert_eq!(fields[2]["required_sections"], Value::Null);
        assert_eq!(fields[2]["grounded_refs"], json!(["card_slug"]));
        assert_eq!(fields[2]["max_length"], json!(200));
        // spread key order + appended required_sections/grounded_refs
        let keys: Vec<&String> = fields[0].as_object().unwrap().keys().collect();
        assert_eq!(
            keys,
            ["field_name", "field_type", "enum_values", "required", "description", "order_index",
             "min_length", "max_length", "required_sections_json", "pattern", "grounded_refs_json",
             "required_sections", "grounded_refs"]
        );
        // input_fields present, no grounded_refs key
        let inf = r["input_fields"].as_array().unwrap();
        assert_eq!(inf.len(), 1);
        assert_eq!(inf[0]["field_name"], json!("diff"));
        assert!(inf[0].get("grounded_refs").is_none());
        // cross_field_rules parsed
        assert_eq!(r["cross_field_rules"], json!([{"id": "r1", "expression": "true", "message": "ok"}]));
        // template keys: SELECT * column order + three appended keys
        let tkeys: Vec<&String> = r.as_object().unwrap().keys().collect();
        assert_eq!(
            tkeys,
            ["id", "name", "version", "template_md", "default_tier", "default_temperature",
             "max_output_tokens", "superseded_by", "created_at", "cross_field_rules_json",
             "judging_criteria_json", "system_md", "phase_flow", "output_fields", "input_fields",
             "cross_field_rules"]
        );
    }

    #[test]
    fn name_version_resolves_superseded_row() {
        let c = db0();
        seed(&c);
        let r = promptFind(&json!({ "name": "gate.review", "version": 1 }), &c).unwrap();
        assert_eq!(r["version"], json!(1));
        assert_eq!(r["superseded_by"], json!(2));
        // only template 1's field comes back
        let fields = r["output_fields"].as_array().unwrap();
        assert_eq!(fields.len(), 1);
        assert_eq!(fields[0]["field_name"], json!("old"));
        assert_eq!(r["input_fields"], json!([]));
        assert_eq!(r["cross_field_rules"], Value::Null);
    }

    #[test]
    fn version_zero_is_falsy_and_takes_active_path() {
        let c = db0();
        seed(&c);
        let r = promptFind(&json!({ "name": "gate.review", "version": 0 }), &c).unwrap();
        assert_eq!(r["version"], json!(2));
    }

    #[test]
    fn guards_and_not_found() {
        let c = db0();
        seed(&c);
        assert!(promptFind(&json!({}), &c).is_err()); // missing name
        assert!(promptFind(&json!("nope"), &c).is_err()); // active not found
        assert!(promptFind(&json!({ "name": "gate.review", "version": 9 }), &c).is_err()); // version not found
    }
}
