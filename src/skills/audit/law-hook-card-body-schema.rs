//! law-hook-card-body-schema.rs — twin of skills/audit/law-hook-card-body-schema.mjs. NAMES ARE
//! CONTRACT: `lawHookCardBodySchema` (default). Enforces CARD_BODY_MUST_MATCH_SCHEMA: validates
//! `input_data.body` (card.add / card.update payloads) against the canonical JSON Schema at
//! `schemas/card-body.schema.json`, refusing malformed bodies with a precise `reason` +
//! `field_path`.
//!
//! BESPOKE-NATIVE (crossing playbook VET): the JS reads a REAL file
//! (`resolve(__dirname, '..', '..', 'schemas', 'card-body.schema.json')`) whose root is a
//! HARDCODED path relative to the module's own on-disk location — not DB-derived (contrast
//! stack-facts.rs, whose fs root comes from a `workspaces.root_path` DB row and is therefore
//! fixture-controllable). There is no seam a conform-db fixture can drive, so this unit carries
//! NO fixture and NO frontier entry (matches the god-driver-halt.rs precedent) — proven instead
//! by the `#[cfg(test)]` block below, which reads the REAL schema file committed at
//! `antiloki-core-rust/schemas/card-body.schema.json` (mirrored from antiloki-core per
//! MIRROR.json's `copied_files` list, so both lanes read byte-identical schema text even though
//! this harness never diffs them directly).
//!
//! Path: JS `__dirname` (this module's own directory, `antiloki-core/skills/audit/`) → `../../
//! schemas/card-body.schema.json` resolves to `antiloki-core/schemas/card-body.schema.json`. The
//! Rust crate ships its own mirrored copy at its own root, so the twin anchors on
//! `CARGO_MANIFEST_DIR` (compile-time constant, the Rust analog of a compile-time `__dirname`) —
//! same convention as sandbox-resolve-path.rs / law-add.rs / stack-facts.rs's siblings. No
//! env-var override exists here (unlike god-driver-halt's `ANTILOKI_HOME`) because the JS source
//! itself has none — the path is unconditionally fixed, so the twin's test reads the one real
//! file rather than injecting a fake location.
//!
//! JS keeps a module-level `_cachedSchema` (perf only, not observable) — the twin re-reads the
//! file each call; output is identical either way.
//!
//! Untested JS edge case (not replicated as a crash): if `body` arrives as the STRING `"null"`,
//! `JSON.parse` yields real `null`, and `typeof null === 'object'` lets it slip past the
//! `typeof body !== 'object'` guard — the JS then throws inside `Object.keys(null)` a few lines
//! later (TypeError), a latent bug never exercised by the module's own test suite. This twin
//! instead refuses cleanly via the "body must be an object" branch for that input (see `got`
//! below) rather than reproducing the crash.
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
// = JS `${v}` (template literal coercion).
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

// board-keeper.card.add / board-keeper.card.update — the only actions the hook fires for.
const BODY_WRITING_ACTIONS: [&str; 2] = ["board-keeper.card.add", "board-keeper.card.update"];

fn schema_path() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("schemas")
        .join("card-body.schema.json")
}

fn load_schema() -> Result<Value, String> {
    let text = std::fs::read_to_string(schema_path()).map_err(|e| e.to_string())?;
    serde_json::from_str(&text).map_err(|e| e.to_string())
}

fn refused(reason: String, field_path: String) -> Value {
    let mut out = Map::new();
    out.insert("allowed".into(), json!(false));
    out.insert("reason".into(), json!(reason));
    out.insert(
        "suggested_path".into(),
        json!("see docs/card-schema.md for the canonical shape; if migrating legacy data, set ANTILOKI_ALLOW_SCHEMA_DRIFT=true for the one-off run"),
    );
    out.insert("field_path".into(), json!(field_path));
    Value::Object(out)
}

fn string_list(v: Option<&Value>) -> Vec<String> {
    v.and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
        .unwrap_or_default()
}

pub fn lawHookCardBodySchema(input: &Value, _db: &Connection) -> Result<Value, String> {
    // const { action_name, input_data } = input ?? {};
    let action_name = input.get("action_name");
    let input_data = input.get("input_data");
    let action_name_str = action_name.and_then(Value::as_str);
    let is_body_writing = action_name_str.is_some_and(|s| BODY_WRITING_ACTIONS.contains(&s));
    if !truthy(action_name) || !is_body_writing {
        return Ok(json!({ "allowed": true }));
    }

    if std::env::var("ANTILOKI_ALLOW_SCHEMA_DRIFT").ok().as_deref() == Some("true") {
        return Ok(json!({ "allowed": true, "escape_hatch": "ANTILOKI_ALLOW_SCHEMA_DRIFT=true" }));
    }

    // input_data?.body — optional chaining short-circuits to undefined when input_data is
    // missing/null; only a plain object carries a meaningful `.body` property here.
    let body_field = input_data.filter(|v| v.is_object()).and_then(|v| v.get("body"));
    let mut body: Value = match body_field {
        None => return Ok(json!({ "allowed": true, "reason": "no body in update payload" })),
        Some(Value::Null) => {
            if action_name_str == Some("board-keeper.card.add") {
                return Ok(refused("body required on card.add".into(), "body".into()));
            }
            return Ok(json!({ "allowed": true, "reason": "body=null is a clear-body update" }));
        }
        Some(v) => v.clone(),
    };

    if let Value::String(s) = &body {
        match serde_json::from_str::<Value>(s) {
            Ok(parsed) => body = parsed,
            // JS: `body is not valid JSON: ${e.message}` — V8's exact SyntaxError text is not
            // reproducible in Rust (and this branch carries no fixture to compare against; see
            // module doc), so the underlying serde_json parse error stands in for `e.message`.
            Err(e) => return Ok(refused(format!("body is not valid JSON: {e}"), "body".into())),
        }
    }

    if !body.is_object() {
        let got = match &body {
            Value::Array(_) => "array",
            Value::String(_) => "string",
            Value::Number(_) => "number",
            Value::Bool(_) => "boolean",
            _ => "object", // Null (typeof null === 'object' in JS) — see module doc
        };
        return Ok(refused(format!("body must be an object (got {got})"), "body".into()));
    }
    let body_map = body.as_object().unwrap();

    let schema = load_schema()?;
    let allowed_keys: Vec<String> = schema
        .get("properties")
        .and_then(Value::as_object)
        .map(|m| m.keys().cloned().collect())
        .unwrap_or_default();
    let required_keys: Vec<String> = string_list(schema.get("required"));
    let forbidden_keys: Vec<String> = string_list(
        schema.get("$defs").and_then(|d| d.get("forbiddenKeys")).and_then(|f| f.get("keys")),
    );
    let kind_enum: Vec<String> =
        string_list(schema.get("properties").and_then(|p| p.get("kind")).and_then(|k| k.get("enum")));
    let size_enum: Vec<String> =
        string_list(schema.get("properties").and_then(|p| p.get("size")).and_then(|k| k.get("enum")));

    // Forbidden keys first — clearest error (the user typed the wrong field name).
    for k in body_map.keys() {
        if forbidden_keys.iter().any(|f| f == k) {
            return Ok(refused(
                format!("forbidden key '{k}' — see docs/card-schema.md for the canonical key and renames (e.g. motivation → why)"),
                format!("body.{k}"),
            ));
        }
    }

    // Unknown keys.
    for k in body_map.keys() {
        if !allowed_keys.iter().any(|a| a == k) {
            return Ok(refused(
                format!("unknown key '{k}' — schema only permits: {}", allowed_keys.join(", ")),
                format!("body.{k}"),
            ));
        }
    }

    // Required keys.
    for k in &required_keys {
        if !body_map.contains_key(k) {
            return Ok(refused(format!("missing required field '{k}'"), format!("body.{k}")));
        }
        if k.as_str() == "why" {
            let why_ok = matches!(body_map.get("why"), Some(Value::String(s)) if !s.trim().is_empty());
            if !why_ok {
                return Ok(refused("'why' must be a non-empty string".into(), "body.why".into()));
            }
        }
    }

    // Enums — `body.kind && !kindEnum.includes(body.kind)`: truthy-gated, then strict-equality
    // membership (only a string body.kind can ever match, since enum entries are all strings).
    let kind_v = body_map.get("kind");
    if truthy(kind_v) {
        let matches_enum = kind_v.and_then(Value::as_str).is_some_and(|k| kind_enum.iter().any(|e| e.as_str() == k));
        if !matches_enum {
            return Ok(refused(
                format!("kind '{}' not allowed — must be one of: {}", tmpl(kind_v), kind_enum.join(", ")),
                "body.kind".into(),
            ));
        }
    }
    let size_v = body_map.get("size");
    if truthy(size_v) {
        let matches_enum = size_v.and_then(Value::as_str).is_some_and(|s| size_enum.iter().any(|e| e.as_str() == s));
        if !matches_enum {
            return Ok(refused(
                format!("size '{}' not allowed — must be one of: {}", tmpl(size_v), size_enum.join(", ")),
                "body.size".into(),
            ));
        }
    }

    Ok(json!({ "allowed": true }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        Connection::open_in_memory().unwrap()
    }
    fn call(i: Value) -> Value {
        lawHookCardBodySchema(&i, &db()).unwrap()
    }

    #[test]
    fn passes_through_actions_the_hook_does_not_gate() {
        assert_eq!(call(json!({})), json!({ "allowed": true }));
        assert_eq!(call(json!({ "action_name": "" })), json!({ "allowed": true }));
        assert_eq!(
            call(json!({ "action_name": "board-keeper.card.status.change", "input_data": { "body": {} } })),
            json!({ "allowed": true })
        );
    }

    #[test]
    fn no_body_key_in_update_payload_is_allowed_with_reason() {
        let r = call(json!({ "action_name": "board-keeper.card.update", "input_data": { "slug": "X" } }));
        assert_eq!(r, json!({ "allowed": true, "reason": "no body in update payload" }));
    }

    #[test]
    fn null_body_refused_on_add_allowed_on_update() {
        let add = call(json!({ "action_name": "board-keeper.card.add", "input_data": { "body": null } }));
        assert_eq!(add["allowed"], json!(false));
        assert_eq!(add["reason"], json!("body required on card.add"));
        assert_eq!(add["field_path"], json!("body"));

        let upd = call(json!({ "action_name": "board-keeper.card.update", "input_data": { "body": null } }));
        assert_eq!(upd, json!({ "allowed": true, "reason": "body=null is a clear-body update" }));
    }

    #[test]
    fn valid_object_body_is_allowed() {
        let r = call(json!({
            "action_name": "board-keeper.card.add",
            "input_data": { "body": { "kind": "feat", "size": "M", "why": "because" } }
        }));
        assert_eq!(r, json!({ "allowed": true }));
    }

    #[test]
    fn valid_stringified_body_is_parsed_and_allowed() {
        let r = call(json!({
            "action_name": "board-keeper.card.add",
            "input_data": { "body": "{\"kind\":\"fix\",\"size\":\"S\",\"why\":\"bug\"}" }
        }));
        assert_eq!(r, json!({ "allowed": true }));
    }

    #[test]
    fn invalid_json_string_body_is_refused() {
        let r = call(json!({
            "action_name": "board-keeper.card.add",
            "input_data": { "body": "{not json" }
        }));
        assert_eq!(r["allowed"], json!(false));
        assert!(r["reason"].as_str().unwrap().starts_with("body is not valid JSON: "));
        assert_eq!(r["field_path"], json!("body"));
    }

    #[test]
    fn array_body_is_refused_as_array() {
        let r = call(json!({
            "action_name": "board-keeper.card.add",
            "input_data": { "body": [1, 2, 3] }
        }));
        assert_eq!(r["allowed"], json!(false));
        assert_eq!(r["reason"], json!("body must be an object (got array)"));
    }

    #[test]
    fn string_body_that_parses_to_a_number_is_refused_as_number() {
        let r = call(json!({
            "action_name": "board-keeper.card.add",
            "input_data": { "body": "42" }
        }));
        assert_eq!(r["allowed"], json!(false));
        assert_eq!(r["reason"], json!("body must be an object (got number)"));
    }

    #[test]
    fn forbidden_key_refused_before_unknown_key_check() {
        let r = call(json!({
            "action_name": "board-keeper.card.add",
            "input_data": { "body": { "parent": "E-1", "bogus": "x", "kind": "feat", "size": "M", "why": "y" } }
        }));
        assert_eq!(r["allowed"], json!(false));
        assert!(r["reason"].as_str().unwrap().contains("forbidden key 'parent'"));
        assert_eq!(r["field_path"], json!("body.parent"));
    }

    #[test]
    fn unknown_key_refused_with_full_allowed_list() {
        let r = call(json!({
            "action_name": "board-keeper.card.add",
            "input_data": { "body": { "kind": "feat", "size": "M", "why": "y", "totally_unknown": 1 } }
        }));
        assert_eq!(r["allowed"], json!(false));
        assert_eq!(r["field_path"], json!("body.totally_unknown"));
        let reason = r["reason"].as_str().unwrap();
        assert!(reason.starts_with("unknown key 'totally_unknown' — schema only permits: "));
        assert!(reason.contains("kind"));
        assert!(reason.contains("motivation"));
    }

    #[test]
    fn missing_required_field_refused() {
        let r = call(json!({
            "action_name": "board-keeper.card.add",
            "input_data": { "body": { "kind": "feat" } }
        }));
        assert_eq!(r["allowed"], json!(false));
        assert_eq!(r["reason"], json!("missing required field 'size'"));
        assert_eq!(r["field_path"], json!("body.size"));
    }

    #[test]
    fn kind_enum_violation_lists_all_options() {
        let r = call(json!({
            "action_name": "board-keeper.card.update",
            "input_data": { "body": { "kind": "bogus-kind", "size": "M" } }
        }));
        assert_eq!(r["allowed"], json!(false));
        assert_eq!(r["field_path"], json!("body.kind"));
        let reason = r["reason"].as_str().unwrap();
        assert!(reason.starts_with("kind 'bogus-kind' not allowed — must be one of: "));
        assert!(reason.contains("epic"));
        assert!(reason.contains("phase"));
    }

    #[test]
    fn size_enum_violation_lists_all_options() {
        let r = call(json!({
            "action_name": "board-keeper.card.update",
            "input_data": { "body": { "kind": "feat", "size": "XL" } }
        }));
        assert_eq!(r["allowed"], json!(false));
        assert_eq!(r["field_path"], json!("body.size"));
        assert_eq!(r["reason"], json!("size 'XL' not allowed — must be one of: L, M, S"));
    }

    #[test]
    fn escape_hatch_env_var_bypasses_validation() {
        let _guard = crate::test_env_lock::ENV_LOCK.lock().unwrap();
        unsafe {
            std::env::set_var("ANTILOKI_ALLOW_SCHEMA_DRIFT", "true");
        }
        let r = call(json!({
            "action_name": "board-keeper.card.add",
            "input_data": { "body": { "totally": "not schema shaped" } }
        }));
        unsafe {
            std::env::remove_var("ANTILOKI_ALLOW_SCHEMA_DRIFT");
        }
        assert_eq!(r, json!({ "allowed": true, "escape_hatch": "ANTILOKI_ALLOW_SCHEMA_DRIFT=true" }));
    }

    #[test]
    fn reads_the_real_committed_schema_file() {
        // Sanity check the bespoke-native fs seam itself: the twin's schema_path() must resolve
        // to the mirrored copy actually committed in this crate (MIRROR.json copied_files).
        let schema = load_schema().unwrap();
        assert_eq!(schema["required"], json!(["kind", "size"]));
        assert!(schema["properties"]["kind"]["enum"].as_array().unwrap().contains(&json!("epic")));
    }
}
