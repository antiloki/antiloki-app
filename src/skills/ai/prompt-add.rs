//! prompt-add.rs — twin of skills/ai/prompt-add.mjs (db WRITE lane, attached `brain` db).
//! NAMES ARE CONTRACT: `promptAdd` (default). Registers a prompt template + normalized
//! output/input field rows; bumps version when the name already has an active row (the old
//! active row gets superseded_by = new id). Returns { id, name, version, output_field_count,
//! input_field_count }. The JS savepoint fallback (FIX-PROMPT-ADD-NESTED-TXN, named via
//! process.hrtime — nondeterministic but unreachable in the lane, where BEGIN always succeeds
//! on a fresh connection) collapses to a fixed-name savepoint.
use rusqlite::Connection;
use serde_json::{json, Value};

const VALID_TIER: [&str; 3] = ["low", "mid", "high"];
const VALID_TEMP: [&str; 5] = ["freezed", "cold", "warm", "hot", "hell"];
const VALID_FIELD_TYPE: [&str; 6] = ["string", "number", "boolean", "enum", "array_of_string", "array_of_object"];
const VALID_PHASE_FLOW: [&str; 3] = ["single", "reason_then_format", "single_only"];

fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}
// = JS `${v}` (feeds the enumVal `(got '…')` interpolation only).
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
// = JS String(v) (judging_criteria map).
fn js_string(v: &Value) -> String {
    match v {
        Value::Null => "null".into(),
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Array(a) => a.iter().map(js_string).collect::<Vec<_>>().join(","),
        Value::Object(_) => "[object Object]".into(),
    }
}
fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) }

// JS `.length` for the template_md guard: strings/arrays have one; anything else compares
// `undefined < 5` → false, so the guard passes for truthy non-string values (JS parity).
fn js_length(v: &Value) -> Option<i64> {
    match v {
        Value::String(s) => Some(s.chars().count() as i64),
        Value::Array(a) => Some(a.len() as i64),
        _ => None,
    }
}
// JS Array.prototype.join('|'): null/undefined elements render as '' (NOT "null").
fn join_pipe(a: &[Value]) -> String {
    a.iter().map(|v| if v.is_null() { String::new() } else { js_string(v) }).collect::<Vec<_>>().join("|")
}
// JS `Number.isInteger(v) ? v : null` for the min_length/max_length lint columns.
fn js_integer(v: Option<&Value>) -> Value {
    match v {
        Some(Value::Number(n)) => n
            .as_i64()
            .map(|i| json!(i))
            .or_else(|| n.as_f64().filter(|f| f.is_finite() && f.fract() == 0.0).map(|f| json!(f as i64)))
            .unwrap_or(Value::Null),
        _ => Value::Null,
    }
}
fn in_set(v: Option<&Value>, set: &[&str]) -> bool {
    v.and_then(|x| x.as_str()).map_or(false, |s| set.contains(&s))
}
// enumSrc: Array f.enum wins, else Array f.enum_values, else f.enum_values ?? null;
// arrays join('|'), non-arrays pass through unchanged.
fn resolve_enum(f: &Value) -> Value {
    if let Some(Value::Array(a)) = f.get("enum") { return json!(join_pipe(a)); }
    if let Some(Value::Array(a)) = f.get("enum_values") { return json!(join_pipe(a)); }
    nz(f.get("enum_values")).cloned().unwrap_or(Value::Null)
}
// JSON.stringify(v) when v is an array, else null (required_sections / grounded_refs).
fn stringify_if_array(v: Option<&Value>) -> Value {
    match v {
        Some(a @ Value::Array(_)) => json!(serde_json::to_string(a).unwrap_or_default()),
        _ => Value::Null,
    }
}

// One normalized-field loop (mechanism identical for output/input; input skips grounded_refs).
fn insert_fields(db: &Connection, template_id: i64, fields: &[Value], label: &str, sql: &str, grounded: bool) -> Result<i64, String> {
    let mut order: i64 = 0;
    for f in fields {
        if !truthy(f.get("field_name")) {
            return Err(format!("prompt-add: {label}[].field_name is required"));
        }
        if !in_set(f.get("field_type"), &VALID_FIELD_TYPE) {
            return Err(format!(
                "prompt-add: {label}[].field_type must be one of: string, number, boolean, enum, array_of_string, array_of_object (got '{}')",
                tmpl(f.get("field_type"))
            ));
        }
        let required: i64 = if f.get("required") == Some(&Value::Bool(false)) { 0 } else { 1 }; // strict === false
        let pattern = match f.get("pattern") { Some(Value::String(s)) => json!(s.clone()), _ => Value::Null };
        let mut params = vec![
            json!(template_id),
            f.get("field_name").cloned().unwrap_or(Value::Null),
            f.get("field_type").cloned().unwrap_or(Value::Null),
            resolve_enum(f),
            json!(required),
            nz(f.get("description")).cloned().unwrap_or(Value::Null),
            json!(order),
            js_integer(f.get("min_length")),
            js_integer(f.get("max_length")),
            stringify_if_array(f.get("required_sections")),
            pattern,
        ];
        if grounded { params.push(stringify_if_array(f.get("grounded_refs"))); }
        crate::db_json::run_write(db, sql, &params)?;
        order += 1;
    }
    Ok(order)
}

pub fn promptAdd(input: &Value, db: &Connection) -> Result<Value, String> {
    // Destructure defaults fire only when the key is ABSENT (JS undefined); explicit null stays.
    let name = input.get("name").cloned().unwrap_or(Value::Null);
    let template_md = input.get("template_md").cloned().unwrap_or(Value::Null);
    let default_tier = input.get("default_tier").cloned().unwrap_or(json!("mid"));
    let default_temperature = input.get("default_temperature").cloned().unwrap_or(json!("cold"));
    let max_output_tokens = input.get("max_output_tokens").cloned().unwrap_or(json!(1024));
    let output_fields = input.get("output_fields").cloned().unwrap_or(json!([]));
    let input_fields = input.get("input_fields").cloned().unwrap_or(json!([]));
    let cross_field_rules = input.get("cross_field_rules").cloned().unwrap_or(Value::Null);
    let system_md = input.get("system_md").cloned().unwrap_or(Value::Null);
    let phase_flow = input.get("phase_flow").cloned().unwrap_or(json!("single"));
    let judging_criteria = input.get("judging_criteria").cloned().unwrap_or(Value::Null);

    if !truthy(Some(&name)) { return Err("prompt-add: name is required".into()); }
    if !truthy(Some(&template_md)) || js_length(&template_md).map_or(false, |l| l < 5) {
        return Err("prompt-add: template_md is required".into());
    }
    if !in_set(Some(&default_tier), &VALID_TIER) {
        return Err(format!("prompt-add: default_tier must be one of: low, mid, high (got '{}')", tmpl(Some(&default_tier))));
    }
    if !in_set(Some(&default_temperature), &VALID_TEMP) {
        return Err(format!("prompt-add: default_temperature must be one of: freezed, cold, warm, hot, hell (got '{}')", tmpl(Some(&default_temperature))));
    }
    let out_fields: Vec<Value> = match output_fields.as_array() {
        Some(a) if !a.is_empty() => a.clone(),
        _ => return Err("prompt-add: output_fields (≥1) is required".into()),
    };
    if !in_set(Some(&phase_flow), &VALID_PHASE_FLOW) {
        return Err(format!("prompt-add: phase_flow must be one of: single, reason_then_format, single_only (got '{}')", tmpl(Some(&phase_flow))));
    }

    // Resolve version: bump if name already has an active row.
    let rows = crate::db_json::query_rows(
        db,
        "SELECT id, version FROM brain.prompt_templates WHERE name = ? AND superseded_by IS NULL",
        &[name.clone()],
    )?;
    let existing = rows.as_array().and_then(|a| a.first()).cloned();
    let version: i64 = match &existing {
        Some(e) => e.get("version").and_then(|v| v.as_i64()).unwrap_or(0) + 1,
        None => 1,
    };

    let nested = db.execute_batch("BEGIN").is_err();
    if nested { db.execute_batch("SAVEPOINT prompt_add_sp").map_err(|e| e.to_string())?; }
    let body = || -> Result<Value, String> {
        // Validate cross_field_rules shape early so a bad rule never lands in the DB.
        let mut cross_field_rules_json = Value::Null;
        if !cross_field_rules.is_null() {
            let rules = cross_field_rules
                .as_array()
                .ok_or_else(|| "prompt-add: cross_field_rules must be an array of {id, expression, message} objects".to_string())?;
            for r in rules {
                let ok = r.get("id").map_or(false, Value::is_string)
                    && r.get("expression").map_or(false, Value::is_string)
                    && r.get("message").map_or(false, Value::is_string);
                if !ok {
                    return Err("prompt-add: each cross_field_rule must have string id, expression, message".into());
                }
            }
            cross_field_rules_json = json!(serde_json::to_string(&cross_field_rules).unwrap_or_default());
        }
        let system_param = match &system_md {
            Value::String(s) if !s.trim().is_empty() => system_md.clone(), // untrimmed original
            _ => Value::Null,
        };
        let judging_param = match judging_criteria.as_array() {
            Some(a) if !a.is_empty() => {
                let mapped: Vec<String> = a.iter().map(|c| js_string(c).chars().take(240).collect()).take(8).collect();
                json!(serde_json::to_string(&mapped).unwrap_or_default())
            }
            _ => Value::Null,
        };
        let (_changes, id) = crate::db_json::run_write(
            db,
            "INSERT INTO brain.prompt_templates (name, version, template_md, default_tier, default_temperature, max_output_tokens, cross_field_rules_json, system_md, phase_flow, judging_criteria_json) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            &[
                name.clone(),
                json!(version),
                template_md.clone(),
                default_tier.clone(),
                default_temperature.clone(),
                max_output_tokens.clone(),
                cross_field_rules_json,
                system_param,
                phase_flow.clone(),
                judging_param,
            ],
        )?;

        if let Some(e) = &existing {
            crate::db_json::run_write(
                db,
                "UPDATE brain.prompt_templates SET superseded_by = ? WHERE id = ?",
                &[json!(id), e.get("id").cloned().unwrap_or(Value::Null)],
            )?;
        }

        let order = insert_fields(
            db, id, &out_fields, "output_fields",
            "INSERT INTO brain.prompt_output_fields (prompt_template_id, field_name, field_type, enum_values, required, description, order_index, min_length, max_length, required_sections_json, pattern, grounded_refs_json) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            true,
        )?;
        let input_order = match input_fields.as_array() {
            Some(a) if !a.is_empty() => insert_fields(
                db, id, a, "input_fields",
                "INSERT INTO brain.prompt_input_fields (prompt_template_id, field_name, field_type, enum_values, required, description, order_index, min_length, max_length, required_sections_json, pattern) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                false,
            )?,
            _ => 0,
        };
        if nested { db.execute_batch("RELEASE SAVEPOINT prompt_add_sp").map_err(|e| e.to_string())?; }
        else { db.execute_batch("COMMIT").map_err(|e| e.to_string())?; }
        Ok(json!({ "id": id, "name": name.clone(), "version": version, "output_field_count": order, "input_field_count": input_order }))
    };
    match body() {
        Ok(v) => Ok(v),
        Err(e) => {
            let _ = if nested {
                db.execute_batch("ROLLBACK TO SAVEPOINT prompt_add_sp; RELEASE SAVEPOINT prompt_add_sp")
            } else {
                db.execute_batch("ROLLBACK")
            };
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCHEMA: &str = "
        CREATE TABLE brain.prompt_templates ( id INTEGER PRIMARY KEY, name TEXT NOT NULL, version INTEGER NOT NULL, template_md TEXT NOT NULL, default_tier TEXT NOT NULL CHECK (default_tier IN ('low','mid','high')), default_temperature TEXT NOT NULL CHECK (default_temperature IN ('freezed','cold','warm','hot','hell')), max_output_tokens INTEGER NOT NULL DEFAULT 1024, superseded_by INTEGER, created_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')), cross_field_rules_json TEXT, judging_criteria_json TEXT, system_md TEXT, phase_flow TEXT NOT NULL DEFAULT 'single', UNIQUE (name, version) );
        CREATE TABLE brain.prompt_output_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL CHECK (field_type IN ('string','number','boolean','enum','array_of_string','array_of_object')), enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, grounded_refs_json TEXT, UNIQUE (prompt_template_id, field_name) );
        CREATE TABLE brain.prompt_input_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL CHECK (field_type IN ('string','number','boolean','enum','array_of_string','array_of_object')), enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, UNIQUE (prompt_template_id, field_name) );
    ";

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS brain").unwrap();
        c.execute_batch(SCHEMA).unwrap();
        c
    }
    fn minimal(name: &str) -> Value {
        json!({ "name": name, "template_md": "Say hello to {{who}}.", "output_fields": [{ "field_name": "message", "field_type": "string" }] })
    }

    #[test]
    fn registers_with_defaults() {
        let c = db();
        let r = promptAdd(&minimal("greet"), &c).unwrap();
        assert_eq!(r, json!({ "id": 1, "name": "greet", "version": 1, "output_field_count": 1, "input_field_count": 0 }));
        let (tier, temp, max, flow): (String, String, i64, String) = c
            .query_row("SELECT default_tier, default_temperature, max_output_tokens, phase_flow FROM brain.prompt_templates WHERE id = 1", [], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
            })
            .unwrap();
        assert_eq!((tier.as_str(), temp.as_str(), max, flow.as_str()), ("mid", "cold", 1024, "single"));
        let (req, ord): (i64, i64) = c
            .query_row("SELECT required, order_index FROM brain.prompt_output_fields WHERE field_name = 'message'", [], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap();
        assert_eq!((req, ord), (1, 0));
    }

    #[test]
    fn version_bump_supersedes_the_active_row() {
        let c = db();
        let first = promptAdd(&minimal("greet"), &c).unwrap();
        let second = promptAdd(&minimal("greet"), &c).unwrap();
        assert_eq!(second["version"], json!(2));
        let superseded_by: i64 = c
            .query_row("SELECT superseded_by FROM brain.prompt_templates WHERE id = ?", [first["id"].as_i64().unwrap()], |r| r.get(0))
            .unwrap();
        assert_eq!(superseded_by, second["id"].as_i64().unwrap());
    }

    #[test]
    fn full_options_serialize_like_js() {
        let c = db();
        let long: String = "abcdefghij".repeat(25); // 250 chars → sliced to 240
        let r = promptAdd(&json!({
            "name": "review-gate", "template_md": "Review {{diff}} and emit a verdict.",
            "default_tier": "low", "default_temperature": "freezed", "max_output_tokens": 800,
            "phase_flow": "reason_then_format", "system_md": "You are a strict reviewer.",
            "cross_field_rules": [{ "id": "r1", "expression": "output.score >= 0", "message": "bad score" }],
            "judging_criteria": ["c1", 42, long, "c4", "c5", "c6", "c7", "c8", "c9"],
            "output_fields": [
                { "field_name": "verdict", "field_type": "enum", "enum": ["pass", "fail"], "description": "overall" },
                { "field_name": "reason", "field_type": "string", "required": false, "min_length": 10, "max_length": 240,
                  "required_sections": ["Summary"], "pattern": "^[A-Z]", "grounded_refs": ["card_slug"] }
            ],
            "input_fields": [{ "field_name": "mode", "field_type": "enum", "enum_values": ["fast", "deep"] }]
        }), &c).unwrap();
        assert_eq!(r["output_field_count"], json!(2));
        assert_eq!(r["input_field_count"], json!(1));
        let jc: String = c.query_row("SELECT judging_criteria_json FROM brain.prompt_templates WHERE id = 1", [], |r| r.get(0)).unwrap();
        let arr: Vec<String> = serde_json::from_str(&jc).unwrap();
        assert_eq!(arr.len(), 8); // slice(0, 8)
        assert_eq!(arr[1], "42"); // String(42)
        assert_eq!(arr[2].len(), 240); // slice(0, 240)
        let ev: String = c.query_row("SELECT enum_values FROM brain.prompt_output_fields WHERE field_name = 'verdict'", [], |r| r.get(0)).unwrap();
        assert_eq!(ev, "pass|fail");
        let (req, min): (i64, i64) = c
            .query_row("SELECT required, min_length FROM brain.prompt_output_fields WHERE field_name = 'reason'", [], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap();
        assert_eq!((req, min), (0, 10));
        let iev: String = c.query_row("SELECT enum_values FROM brain.prompt_input_fields WHERE field_name = 'mode'", [], |r| r.get(0)).unwrap();
        assert_eq!(iev, "fast|deep");
    }

    #[test]
    fn edge_coercions_null_out_like_js() {
        let c = db();
        promptAdd(&json!({
            "name": "edge", "template_md": "12345", "phase_flow": "single_only", "system_md": "   ",
            "judging_criteria": [],
            "output_fields": [{ "field_name": "tags", "field_type": "array_of_string", "enum": "notanarray",
                                 "enum_values": "red|green|blue", "min_length": 3.7, "pattern": 123 }]
        }), &c).unwrap();
        let (sys, jc): (Option<String>, Option<String>) = c
            .query_row("SELECT system_md, judging_criteria_json FROM brain.prompt_templates WHERE id = 1", [], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap();
        assert_eq!((sys, jc), (None, None)); // whitespace system_md + empty judging → NULL
        let (ev, min, pat): (String, Option<i64>, Option<String>) = c
            .query_row("SELECT enum_values, min_length, pattern FROM brain.prompt_output_fields WHERE field_name = 'tags'", [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap();
        assert_eq!(ev, "red|green|blue"); // non-array `enum` ignored, string enum_values passes through
        assert_eq!(min, None); // Number.isInteger(3.7) → null
        assert_eq!(pat, None); // non-string pattern → null
    }

    #[test]
    fn throws_validate_and_roll_back() {
        let c = db();
        assert!(promptAdd(&json!({ "template_md": "valid enough", "output_fields": [{ "field_name": "x", "field_type": "string" }] }), &c).unwrap_err().contains("name is required"));
        assert!(promptAdd(&json!({ "name": "a", "template_md": "1234", "output_fields": [{ "field_name": "x", "field_type": "string" }] }), &c).unwrap_err().contains("template_md"));
        assert!(promptAdd(&json!({ "name": "a", "template_md": "valid enough", "default_tier": "xl", "output_fields": [{ "field_name": "x", "field_type": "string" }] }), &c).unwrap_err().contains("default_tier must be one of: low, mid, high (got 'xl')"));
        assert!(promptAdd(&json!({ "name": "a", "template_md": "valid enough", "output_fields": [] }), &c).unwrap_err().contains("output_fields"));
        assert!(promptAdd(&json!({ "name": "a", "template_md": "valid enough", "phase_flow": "dual", "output_fields": [{ "field_name": "x", "field_type": "string" }] }), &c).unwrap_err().contains("phase_flow must be one of"));
        assert!(promptAdd(&json!({ "name": "a", "template_md": "valid enough", "cross_field_rules": "nope", "output_fields": [{ "field_name": "x", "field_type": "string" }] }), &c).unwrap_err().contains("must be an array"));
        // mid-loop failure rolls the whole registration back
        let e = promptAdd(&json!({ "name": "rb", "template_md": "valid enough", "output_fields": [
            { "field_name": "good", "field_type": "string" }, { "field_name": "bad", "field_type": "nope" }
        ] }), &c).unwrap_err();
        assert!(e.contains("output_fields[].field_type must be one of"));
        let n: i64 = c.query_row("SELECT COUNT(*) FROM brain.prompt_templates", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 0);
        let nf: i64 = c.query_row("SELECT COUNT(*) FROM brain.prompt_output_fields", [], |r| r.get(0)).unwrap();
        assert_eq!(nf, 0);
    }
}
