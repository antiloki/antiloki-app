//! card-body-generate.rs — twin of skills/system/card-body-generate.mjs. NAMES ARE CONTRACT:
//! `cardBodyGenerate` (default). v1 deterministic card-body-json generator: fills kind/size/why
//! from rough operator input (explicit values, slug-prefix inference, kind→size defaults),
//! passes optional fields through, validates the assembled body against the canonical schema,
//! returns `{ body, warnings }`.
//!
//! BESPOKE-NATIVE (crossing playbook VET): validates against a REAL file
//! (JS: `resolve(__dirname, '..', '..', 'schemas', 'card-body.schema.json')`) whose root is a
//! HARDCODED path relative to the module's own on-disk location — not DB-derived (contrast
//! stack-facts.rs, whose fs root is DB-derived and therefore fixture-controllable). There is no
//! seam a conform-db fixture can drive, so this unit carries NO fixture and NO frontier entry
//! (same precedent as `audit_law_hook_card_body_schema`, the sibling twin that reads the
//! identical schema file — this twin repeats that same `schema_path()`/`load_schema()` shape
//! rather than reaching across modules for a private helper). Proven instead by the
//! `#[cfg(test)]` block below, which reads the real schema mirrored at
//! `antiloki-core-rust/schemas/card-body.schema.json` (MIRROR.json `copied_files`), and anchors
//! via `CARGO_MANIFEST_DIR` (compile-time constant, the Rust analog of JS's compile-time-fixed
//! `__dirname`) — no env-var override exists because the JS source has none either.
//!
//! Errors: JS throws `AntilokiError{code:VALIDATION_LOGIC, cause}` in five places (missing
//! why/rough_description, plus four "generator bug" schema-validation guards that are dead in
//! practice since every passthrough field is already schema-shaped). This twin returns
//! `Err(cause)` for each — matches gotcha #9 (only the success path would ever be fixture-worthy,
//! and there is no fixture at all here).
use regex::Regex;
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
// = JS `${v}` interpolation.
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
// = JS String(v) (no ?? — only reached once truthy(v) is already known true, so v is Some+non-null).
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

const KIND_FROM_PREFIX: &[(&str, &str)] = &[
    ("EPIC", "epic"), ("FEAT", "feat"), ("FIX", "fix"), ("PREVENT", "prevent"),
    ("DOCS", "docs"), ("DECISION", "decision"), ("GAP", "gap"), ("PHASE", "phase"),
    ("TUI", "feat"), ("MCP", "feat"), ("ROLE", "feat"), ("LOKI", "feat"),
    ("CARD", "feat"), ("SLICE", "feat"), ("POLICY", "decision"), ("ADR", "decision"),
    ("TASK", "feat"),
];

const SIZE_COERCE: &[(&str, &str)] = &[
    ("L", "L"), ("M", "M"), ("S", "S"),
    ("l", "L"), ("m", "M"), ("s", "S"),
    ("large", "L"), ("big", "L"), ("xl", "L"),
    ("medium", "M"), ("med", "M"),
    ("small", "S"), ("xs", "S"), ("extra-small", "S"), ("tiny", "S"),
    ("small-medium", "M"), ("medium-small", "M"), ("medium-large", "L"),
];

const SIZE_FROM_KIND: &[(&str, &str)] = &[
    ("epic", "L"), ("feat", "M"), ("phase", "M"),
    ("fix", "S"), ("prevent", "S"), ("docs", "S"), ("decision", "S"), ("gap", "S"),
];

const CANONICAL_KINDS: &[&str] = &["epic", "feat", "fix", "prevent", "docs", "decision", "gap", "phase"];
const CANONICAL_SIZES: &[&str] = &["L", "M", "S"];

// Array-typed optional fields pass an array-through-filter; the three string-typed ones
// (diagnosis/trigger/mitigation) additionally accept a raw string. Order matters: it is the
// output insertion order for whichever of these keys get set.
const PASSTHROUGH_FIELDS: &[&str] = &[
    "scope", "out_of_scope", "deliverables", "acceptance",
    "pre_conditions", "risks", "files_touched",
    "diagnosis", "trigger", "mitigation", "alternatives_considered",
];
const STRING_TYPED_PASSTHROUGH: &[&str] = &["diagnosis", "trigger", "mitigation"];

fn schema_path() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("schemas").join("card-body.schema.json")
}

fn load_schema() -> Result<Value, String> {
    let text = std::fs::read_to_string(schema_path()).map_err(|e| e.to_string())?;
    serde_json::from_str(&text).map_err(|e| e.to_string())
}

fn string_list(v: Option<&Value>) -> Vec<&str> {
    v.and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).collect()).unwrap_or_default()
}

fn validate_against_schema(body: &Map<String, Value>, schema: &Value) -> Result<(), String> {
    let allowed: Vec<&str> = schema
        .get("properties")
        .and_then(Value::as_object)
        .map(|m| m.keys().map(String::as_str).collect())
        .unwrap_or_default();
    let required = string_list(schema.get("required"));
    let forbidden = string_list(schema.get("$defs").and_then(|d| d.get("forbiddenKeys")).and_then(|f| f.get("keys")));
    let kind_enum = string_list(schema.get("properties").and_then(|p| p.get("kind")).and_then(|k| k.get("enum")));
    let size_enum = string_list(schema.get("properties").and_then(|p| p.get("size")).and_then(|k| k.get("enum")));

    for k in body.keys() {
        if forbidden.contains(&k.as_str()) {
            return Err(format!("generated body contains forbidden key '{k}' — generator bug"));
        }
        if !allowed.contains(&k.as_str()) {
            return Err(format!("generated body contains unknown key '{k}' — generator bug"));
        }
    }
    for k in &required {
        if !body.contains_key(*k) {
            return Err(format!("generated body missing required '{k}' — generator bug"));
        }
    }
    let body_kind = body.get("kind").and_then(Value::as_str).unwrap_or("");
    if !kind_enum.contains(&body_kind) {
        return Err(format!("generated kind '{body_kind}' not in enum"));
    }
    let body_size = body.get("size").and_then(Value::as_str).unwrap_or("");
    if !size_enum.contains(&body_size) {
        return Err(format!("generated size '{body_size}' not in enum"));
    }
    Ok(())
}

pub fn cardBodyGenerate(input: &Value, _db: &Connection) -> Result<Value, String> {
    let mut warnings: Vec<String> = Vec::new();

    // ── kind ────────────────────────────────────────────────────────────
    let kind_v = input.get("kind");
    let mut resolved_kind: Option<String> = None;
    if truthy(kind_v) {
        let lower = js_string(kind_v.unwrap()).to_lowercase();
        if CANONICAL_KINDS.contains(&lower.as_str()) {
            resolved_kind = Some(lower);
        } else {
            warnings.push(format!("kind '{}' not canonical — falling back to slug prefix / 'feat'", tmpl(kind_v)));
        }
    }
    if resolved_kind.is_none() {
        let slug_v = input.get("slug");
        if truthy(slug_v) {
            let slug_str = js_string(slug_v.unwrap());
            let re = Regex::new(r"^([A-Z]+)-").unwrap();
            if let Some(caps) = re.captures(&slug_str) {
                let p = caps.get(1).unwrap().as_str();
                if let Some((_, k)) = KIND_FROM_PREFIX.iter().find(|(pre, _)| *pre == p) {
                    resolved_kind = Some((*k).to_string());
                }
            }
        }
    }
    if resolved_kind.is_none() {
        resolved_kind = Some("feat".to_string());
        warnings.push("kind defaulted to 'feat'".to_string());
    }
    let resolved_kind = resolved_kind.unwrap();

    // ── size ────────────────────────────────────────────────────────────
    let size_v = input.get("size");
    let mut resolved_size: Option<String> = None;
    if truthy(size_v) {
        if let Some(s) = size_v.and_then(Value::as_str) {
            if CANONICAL_SIZES.contains(&s) {
                resolved_size = Some(s.to_string());
            }
        }
        if resolved_size.is_none() {
            let coerce_key = js_string(size_v.unwrap());
            let coerce_key = coerce_key.trim();
            if let Some((_, coerced)) = SIZE_COERCE.iter().find(|(k, _)| *k == coerce_key) {
                resolved_size = Some((*coerced).to_string());
                warnings.push(format!("size '{}' coerced to '{}'", tmpl(size_v), coerced));
            }
        }
    }
    if resolved_size.is_none() {
        let from_kind = SIZE_FROM_KIND.iter().find(|(k, _)| *k == resolved_kind).map(|(_, s)| *s).unwrap_or("M");
        resolved_size = Some(from_kind.to_string());
        warnings.push(format!("size defaulted from kind '{resolved_kind}' to '{from_kind}'"));
    }
    let resolved_size = resolved_size.unwrap();

    // ── why ─────────────────────────────────────────────────────────────
    let why_str = input.get("why").and_then(Value::as_str).filter(|s| !s.trim().is_empty());
    let rough_str = input.get("rough_description").and_then(Value::as_str).filter(|s| !s.trim().is_empty());
    let resolved_why = why_str
        .or(rough_str)
        .map(str::to_string)
        .ok_or_else(|| "either why or rough_description is required".to_string())?;

    // ── assemble ────────────────────────────────────────────────────────
    let mut body: Map<String, Value> = Map::new();
    body.insert("kind".into(), json!(resolved_kind));
    body.insert("size".into(), json!(resolved_size));
    body.insert("why".into(), json!(resolved_why));

    for k in PASSTHROUGH_FIELDS {
        match input.get(*k) {
            Some(Value::Array(arr)) if !arr.is_empty() => {
                let filtered: Vec<Value> =
                    arr.iter().filter(|x| matches!(x, Value::String(s) if !s.is_empty())).cloned().collect();
                body.insert((*k).to_string(), Value::Array(filtered));
            }
            Some(Value::String(s)) if !s.is_empty() => {
                if STRING_TYPED_PASSTHROUGH.contains(k) {
                    body.insert((*k).to_string(), json!(s));
                }
            }
            _ => {}
        }
    }

    for k in ["verify", "notes"] {
        if let Some(Value::String(s)) = input.get(k) {
            if !s.is_empty() {
                body.insert(k.to_string(), json!(s));
            }
        }
    }

    // ── validate against the canonical schema ───────────────────────────
    let schema = load_schema()?;
    validate_against_schema(&body, &schema)?;

    Ok(json!({ "body": Value::Object(body), "warnings": warnings }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        Connection::open_in_memory().unwrap()
    }
    fn call(i: Value) -> Value {
        cardBodyGenerate(&i, &db()).unwrap()
    }

    #[test]
    fn explicit_canonical_kind_lowercased_used_verbatim_no_warning() {
        let r = call(json!({ "kind": "FIX", "size": "S", "why": "bug happened" }));
        assert_eq!(r["body"]["kind"], json!("fix"));
        assert_eq!(r["warnings"], json!([]));
    }

    #[test]
    fn non_canonical_kind_falls_back_to_slug_prefix_and_warns() {
        let r = call(json!({ "kind": "bogus", "slug": "FEAT-THING", "why": "w" }));
        assert_eq!(r["body"]["kind"], json!("feat"));
        assert!(r["warnings"].as_array().unwrap().contains(&json!(
            "kind 'bogus' not canonical — falling back to slug prefix / 'feat'"
        )));
    }

    #[test]
    fn kind_inferred_from_slug_prefix() {
        let r = call(json!({ "slug": "PREVENT-REGRESSION", "why": "w" }));
        assert_eq!(r["body"]["kind"], json!("prevent"));
    }

    #[test]
    fn kind_defaults_to_feat_when_slug_prefix_unmapped() {
        let r = call(json!({ "slug": "WEIRD-999", "why": "w" }));
        assert_eq!(r["body"]["kind"], json!("feat"));
        assert!(r["warnings"].as_array().unwrap().contains(&json!("kind defaulted to 'feat'")));
    }

    #[test]
    fn size_legacy_alias_coerced_with_warning() {
        let r = call(json!({ "kind": "feat", "size": "large", "why": "w" }));
        assert_eq!(r["body"]["size"], json!("L"));
        assert!(r["warnings"].as_array().unwrap().contains(&json!("size 'large' coerced to 'L'")));
    }

    #[test]
    fn size_unrecognized_string_falls_through_to_kind_default() {
        let r = call(json!({ "kind": "fix", "size": "gigantic", "why": "w" }));
        assert_eq!(r["body"]["size"], json!("S"));
        assert!(r["warnings"].as_array().unwrap().contains(&json!("size defaulted from kind 'fix' to 'S'")));
    }

    #[test]
    fn size_omitted_defaults_from_kind_table() {
        let r = call(json!({ "kind": "epic", "why": "w" }));
        assert_eq!(r["body"]["size"], json!("L"));
    }

    #[test]
    fn why_explicit_wins_over_rough_description() {
        let r = call(json!({ "kind": "feat", "size": "M", "why": "real", "rough_description": "ignored" }));
        assert_eq!(r["body"]["why"], json!("real"));
    }

    #[test]
    fn why_blank_falls_back_to_rough_description() {
        let r = call(json!({ "kind": "feat", "size": "M", "why": "   ", "rough_description": "rough text" }));
        assert_eq!(r["body"]["why"], json!("rough text"));
    }

    #[test]
    fn neither_why_nor_rough_description_errs() {
        let err = cardBodyGenerate(&json!({ "kind": "feat", "size": "M" }), &db()).unwrap_err();
        assert_eq!(err, "either why or rough_description is required");
    }

    #[test]
    fn array_passthrough_keeps_only_non_empty_strings() {
        let r = call(json!({
            "kind": "feat", "size": "M", "why": "w",
            "scope": ["a", "", 42, "b", null],
        }));
        assert_eq!(r["body"]["scope"], json!(["a", "b"]));
    }

    #[test]
    fn array_present_but_entirely_non_string_still_sets_empty_array() {
        let r = call(json!({ "kind": "feat", "size": "M", "why": "w", "scope": [42, null] }));
        assert_eq!(r["body"]["scope"], json!([]));
    }

    #[test]
    fn empty_array_not_set_at_all() {
        let r = call(json!({ "kind": "feat", "size": "M", "why": "w", "scope": [] }));
        assert!(r["body"].get("scope").is_none());
    }

    #[test]
    fn string_typed_optional_field_passes_through() {
        let r = call(json!({ "kind": "fix", "size": "S", "why": "w", "diagnosis": "root cause" }));
        assert_eq!(r["body"]["diagnosis"], json!("root cause"));
    }

    #[test]
    fn string_given_for_array_typed_field_is_ignored() {
        let r = call(json!({ "kind": "feat", "size": "M", "why": "w", "scope": "hint only" }));
        assert!(r["body"].get("scope").is_none());
    }

    #[test]
    fn output_key_order_matches_js_verify_before_notes() {
        let r = call(json!({ "kind": "feat", "size": "M", "why": "w", "notes": "n", "verify": "v" }));
        let keys: Vec<&String> = r["body"].as_object().unwrap().keys().collect();
        assert_eq!(keys, ["kind", "size", "why", "verify", "notes"]);
    }

    #[test]
    fn full_happy_path_validates_cleanly() {
        let r = call(json!({
            "kind": "epic", "why": "because it matters",
            "scope": ["do this"], "deliverables": ["ship it"],
        }));
        assert_eq!(r["body"]["kind"], json!("epic"));
        assert_eq!(r["body"]["size"], json!("L"));
        assert_eq!(r["body"]["scope"], json!(["do this"]));
        assert_eq!(r["body"]["deliverables"], json!(["ship it"]));
    }

    #[test]
    fn reads_the_real_committed_schema_file() {
        // Sanity check the bespoke-native fs seam itself: schema_path() must resolve to the
        // mirrored copy actually committed in this crate (MIRROR.json copied_files).
        let schema = load_schema().unwrap();
        assert_eq!(schema["required"], json!(["kind", "size"]));
        assert!(schema["properties"]["kind"]["enum"].as_array().unwrap().contains(&json!("epic")));
    }
}
