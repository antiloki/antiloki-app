//! card-lint.rs — twin of skills/system/card-lint.mjs (standard db lane; default export
//! `cardLint(input, context)`). NAMES ARE CONTRACT: `cardLint`.
//!
//! Read-only scan of every `cards.body_json` against the canonical schema at
//! `schemas/card-body.schema.json`. Reports per-bucket violation counts, capped violation lists,
//! and a top-30 key-frequency summary. Powers the EPIC-CARD-SCHEMA-NORMALIZATION cleanup pass.
//!
//! BESPOKE-NATIVE (crossing playbook VET): same fs seam as
//! `skills/audit/law-hook-card-body-schema.rs` — the JS resolves
//! `resolve(__dirname, '..', '..', 'schemas', 'card-body.schema.json')`, a path HARDCODED
//! relative to the module's own on-disk location (DEFERRED.json lists this file under builtins
//! `fs`/`path`/`url`, not DB-derived). There is no seam a conform-db fixture can drive, so this
//! unit carries NO fixture and NO frontier entry (matches the god-driver-halt.rs /
//! law-hook-card-body-schema.rs precedent) — proven instead by the `#[cfg(test)]` block below,
//! which builds an in-memory `cards` table AND reads the REAL schema file committed at
//! `antiloki-core-rust/schemas/card-body.schema.json` (mirrored from antiloki-core per
//! MIRROR.json's `copied_files` list — byte-identical to the JS source's copy, verified via
//! `diff`).
//!
//! Path: JS `__dirname` (this module's own directory, `antiloki-core/skills/system/`) →
//! `../../schemas/card-body.schema.json` resolves to `antiloki-core/schemas/card-body.schema.json`.
//! The twin anchors on `CARGO_MANIFEST_DIR` (compile-time constant, the Rust analog of a
//! compile-time `__dirname`) — same convention as law-hook-card-body-schema.rs /
//! sandbox-resolve-path.rs / stack-facts.rs. JS has no env-var override for this path, so neither
//! does the twin — the test reads the one real committed file rather than injecting a fake
//! location.
//!
//! `context.db` missing → JS throws `err.contextMissing('card-lint','db')`; unreachable here
//! (Rust's `db: &Connection` is a required, non-nullable reference) — same "guard-throws are not
//! exercised" convention used by every sibling twin.
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
// = JS Number(v)
fn js_number(v: Option<&Value>) -> f64 {
    match v {
        None => f64::NAN,
        Some(Value::Null) => 0.0,
        Some(Value::Bool(b)) => {
            if *b {
                1.0
            } else {
                0.0
            }
        }
        Some(Value::Number(n)) => n.as_f64().unwrap_or(f64::NAN),
        Some(Value::String(s)) => {
            let t = s.trim();
            if t.is_empty() {
                0.0
            } else {
                t.parse().unwrap_or(f64::NAN)
            }
        }
        Some(_) => f64::NAN,
    }
}

fn schema_path() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("schemas")
        .join("card-body.schema.json")
}

fn load_schema() -> Result<Value, String> {
    let text = std::fs::read_to_string(schema_path()).map_err(|e| e.to_string())?;
    serde_json::from_str(&text).map_err(|e| e.to_string())
}

fn string_list(v: Option<&Value>) -> Vec<String> {
    v.and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
        .unwrap_or_default()
}

// PLACEHOLDER_RE, case-insensitive (JS `/i` flag → `(?i)`).
fn placeholder_re() -> Regex {
    Regex::new(r"(?i)\(no .*? recorded\)|\(operator to fill in\)|\(unknown\)|\bTODO\b|\bTBD\b|\bFIXME\b|needs human content").unwrap()
}

// placeholderFields: present-but-empty / placeholder-stub field names, in the parsed body's own
// key order (Object.entries iteration order == insertion order, mirrored by serde_json's
// preserve_order feature).
fn placeholder_fields(parsed: &Map<String, Value>, re: &Regex) -> Vec<String> {
    let mut bad = Vec::new();
    for (k, v) in parsed.iter() {
        match v {
            Value::String(s) => {
                if s.trim().is_empty() || re.is_match(s) {
                    bad.push(k.clone());
                }
            }
            Value::Array(a) if a.is_empty() => bad.push(k.clone()),
            _ => {}
        }
    }
    bad
}

fn push(bucket: &mut Vec<Value>, cap: usize, item: Value) {
    if bucket.len() < cap {
        bucket.push(item);
    }
}

pub fn cardLint(input: &Value, db: &Connection) -> Result<Value, String> {
    let limit_violations = input.get("limit_violations");
    let slug_glob = input.get("slug_glob");

    // const cap = Math.min(Math.max(1, Number(limit_violations) || 1000), 10000);
    let n = js_number(limit_violations);
    let n = if n == 0.0 || n.is_nan() { 1000.0 } else { n };
    let cap = n.max(1.0).min(10000.0) as usize;

    let schema = load_schema()?;
    let allowed_keys: std::collections::HashSet<String> = schema
        .get("properties")
        .and_then(Value::as_object)
        .map(|m| m.keys().cloned().collect())
        .unwrap_or_default();
    let required_keys: Vec<String> = string_list(schema.get("required"));
    let forbidden_keys: std::collections::HashSet<String> = string_list(
        schema.get("$defs").and_then(|d| d.get("forbiddenKeys")).and_then(|f| f.get("keys")),
    )
    .into_iter()
    .collect();
    let rename_rules: Map<String, Value> = schema
        .get("$defs")
        .and_then(|d| d.get("renameRules"))
        .and_then(|r| r.get("rules"))
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let kind_enum: Vec<String> =
        string_list(schema.get("properties").and_then(|p| p.get("kind")).and_then(|k| k.get("enum")));
    let size_enum: Vec<String> =
        string_list(schema.get("properties").and_then(|p| p.get("size")).and_then(|k| k.get("enum")));

    let rows_val = if truthy(slug_glob) {
        crate::db_json::query_rows(
            db,
            "SELECT slug, body_json FROM cards WHERE slug LIKE ?",
            &[slug_glob.cloned().unwrap()],
        )?
    } else {
        crate::db_json::query_rows(db, "SELECT slug, body_json FROM cards", &[])?
    };
    let rows = rows_val.as_array().cloned().unwrap_or_default();

    let (mut n_conformant, mut n_empty_body, mut n_invalid_json, mut n_non_object): (i64, i64, i64, i64) =
        (0, 0, 0, 0);
    let (mut n_has_violations, mut n_has_placeholder): (i64, i64) = (0, 0);

    let mut b_missing_required: Vec<Value> = Vec::new();
    let mut b_forbidden_keys: Vec<Value> = Vec::new();
    let mut b_unknown_keys: Vec<Value> = Vec::new();
    let mut b_invalid_enum: Vec<Value> = Vec::new();
    let mut b_empty_body: Vec<Value> = Vec::new();
    let mut b_non_object: Vec<Value> = Vec::new();
    let mut b_placeholder: Vec<Value> = Vec::new();

    // Map insertion order == first-seen key order (JS Map iteration order).
    let mut key_freq_order: Vec<String> = Vec::new();
    let mut key_freq: std::collections::HashMap<String, i64> = std::collections::HashMap::new();

    let placeholder_re = placeholder_re();

    for r in &rows {
        let slug = r.get("slug").cloned().unwrap_or(Value::Null);
        let body_json = r.get("body_json");
        if !truthy(body_json) {
            n_empty_body += 1;
            push(&mut b_empty_body, cap, json!({ "slug": slug }));
            continue;
        }
        let body_str = body_json.and_then(Value::as_str).unwrap_or("");
        let parsed: Value = match serde_json::from_str(body_str) {
            Ok(v) => v,
            Err(_) => {
                n_invalid_json += 1;
                continue;
            }
        };
        // `!parsed || typeof parsed !== 'object' || Array.isArray(parsed)` — a JS object literal
        // (even `{}`) is the ONLY truthy, typeof==='object', non-array value; every other JSON
        // shape (null/bool/number/string/array) fails at least one clause.
        let parsed_map = match &parsed {
            Value::Object(m) => m,
            other => {
                n_non_object += 1;
                let got_type = match other {
                    Value::Array(_) => "array",
                    Value::Null => "object", // typeof null === 'object'
                    Value::Bool(_) => "boolean",
                    Value::Number(_) => "number",
                    Value::String(_) => "string",
                    Value::Object(_) => unreachable!(),
                };
                push(&mut b_non_object, cap, json!({ "slug": slug, "got_type": got_type }));
                continue;
            }
        };

        let keys: Vec<String> = parsed_map.keys().cloned().collect();
        for k in &keys {
            if !key_freq.contains_key(k) {
                key_freq_order.push(k.clone());
            }
            *key_freq.entry(k.clone()).or_insert(0) += 1;
        }

        let card_forbidden: Vec<String> =
            keys.iter().filter(|k| forbidden_keys.contains(*k)).cloned().collect();
        let card_unknown: Vec<String> = keys
            .iter()
            .filter(|k| !forbidden_keys.contains(*k) && !allowed_keys.contains(*k))
            .cloned()
            .collect();
        let card_missing: Vec<String> =
            required_keys.iter().filter(|k| !parsed_map.contains_key(*k)).cloned().collect();

        let mut has_violation = false;
        if !card_missing.is_empty() {
            has_violation = true;
            push(&mut b_missing_required, cap, json!({ "slug": slug, "missing": card_missing }));
        }
        if !card_forbidden.is_empty() {
            has_violation = true;
            let mut rename_to = Map::new();
            for k in &card_forbidden {
                if let Some(v) = rename_rules.get(k) {
                    rename_to.insert(k.clone(), v.clone());
                }
            }
            let mut item = Map::new();
            item.insert("slug".into(), slug.clone());
            item.insert("keys".into(), json!(card_forbidden));
            if !rename_to.is_empty() {
                item.insert("rename_to".into(), Value::Object(rename_to));
            }
            push(&mut b_forbidden_keys, cap, Value::Object(item));
        }
        if !card_unknown.is_empty() {
            has_violation = true;
            push(&mut b_unknown_keys, cap, json!({ "slug": slug, "keys": card_unknown }));
        }
        let kind_v = parsed_map.get("kind");
        // JS: `kind != null` — a nullish check (excludes ONLY null/undefined), not truthy. A
        // falsy-but-present value ("", 0, false) still enters the enum-membership check in JS.
        if kind_v.is_some_and(|v| !v.is_null()) {
            let matches = kind_v.and_then(Value::as_str).is_some_and(|s| kind_enum.iter().any(|e| e.as_str() == s));
            if !matches {
                has_violation = true;
                push(
                    &mut b_invalid_enum,
                    cap,
                    json!({ "slug": slug, "field": "kind", "got": kind_v.unwrap().clone(), "allowed": kind_enum }),
                );
            }
        }
        let size_v = parsed_map.get("size");
        // JS: `size != null` — see kind's identical nullish-vs-truthy note above.
        if size_v.is_some_and(|v| !v.is_null()) {
            let matches = size_v.and_then(Value::as_str).is_some_and(|s| size_enum.iter().any(|e| e.as_str() == s));
            if !matches {
                has_violation = true;
                push(
                    &mut b_invalid_enum,
                    cap,
                    json!({ "slug": slug, "field": "size", "got": size_v.unwrap().clone(), "allowed": size_enum }),
                );
            }
        }

        if has_violation {
            n_has_violations += 1;
        } else {
            n_conformant += 1;
        }

        let stub_fields = placeholder_fields(parsed_map, &placeholder_re);
        if !stub_fields.is_empty() {
            n_has_placeholder += 1;
            push(&mut b_placeholder, cap, json!({ "slug": slug, "fields": stub_fields }));
        }
    }

    let summary_top: Vec<Value> = {
        let mut entries: Vec<(String, i64)> =
            key_freq_order.iter().map(|k| (k.clone(), key_freq[k])).collect();
        entries.sort_by(|a, b| b.1.cmp(&a.1)); // stable: ties keep first-seen order
        entries.truncate(30);
        entries.into_iter().map(|(key, count)| json!({ "key": key, "count": count })).collect()
    };

    let mut stats = Map::new();
    stats.insert("conformant".into(), json!(n_conformant));
    stats.insert("empty_body".into(), json!(n_empty_body));
    stats.insert("invalid_json".into(), json!(n_invalid_json));
    stats.insert("non_object".into(), json!(n_non_object));
    stats.insert("has_violations".into(), json!(n_has_violations));
    stats.insert("has_placeholder".into(), json!(n_has_placeholder));

    let mut violations_by_kind = Map::new();
    violations_by_kind.insert("missing_required".into(), json!(b_missing_required));
    violations_by_kind.insert("forbidden_keys".into(), json!(b_forbidden_keys));
    violations_by_kind.insert("unknown_keys".into(), json!(b_unknown_keys));
    violations_by_kind.insert("invalid_enum".into(), json!(b_invalid_enum));
    violations_by_kind.insert("empty_body".into(), json!(b_empty_body));
    violations_by_kind.insert("non_object".into(), json!(b_non_object));
    violations_by_kind.insert("placeholder".into(), json!(b_placeholder));

    let mut out = Map::new();
    out.insert("total_cards".into(), json!(rows.len() as i64));
    out.insert("stats".into(), Value::Object(stats));
    out.insert("violations_by_kind".into(), Value::Object(violations_by_kind));
    out.insert("summary_top_keys".into(), json!(summary_top));
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE cards (slug TEXT PRIMARY KEY, body_json TEXT)").unwrap();
        c
    }
    fn seed(c: &Connection, slug: &str, body_json: Option<&str>) {
        c.execute(
            "INSERT INTO cards (slug, body_json) VALUES (?1, ?2)",
            rusqlite::params![slug, body_json],
        )
        .unwrap();
    }
    fn call(c: &Connection, i: Value) -> Value {
        cardLint(&i, c).unwrap()
    }

    #[test]
    fn conformant_card_counts_as_conformant() {
        let c = db();
        seed(&c, "FEAT-A", Some(r#"{"kind":"feat","size":"M","motivation":"because"}"#));
        let r = call(&c, json!({}));
        assert_eq!(r["total_cards"], json!(1));
        assert_eq!(r["stats"]["conformant"], json!(1));
        assert_eq!(r["stats"]["has_violations"], json!(0));
        assert_eq!(r["violations_by_kind"]["missing_required"], json!([]));
    }

    #[test]
    fn empty_body_bucket() {
        let c = db();
        seed(&c, "EMPTY-1", None);
        seed(&c, "EMPTY-2", Some(""));
        let r = call(&c, json!({}));
        assert_eq!(r["stats"]["empty_body"], json!(2));
        assert_eq!(
            r["violations_by_kind"]["empty_body"],
            json!([{ "slug": "EMPTY-1" }, { "slug": "EMPTY-2" }])
        );
    }

    #[test]
    fn invalid_json_is_counted_but_not_bucketed() {
        let c = db();
        seed(&c, "BAD-1", Some("{not json"));
        let r = call(&c, json!({}));
        assert_eq!(r["stats"]["invalid_json"], json!(1));
        assert_eq!(r["stats"]["conformant"], json!(0));
    }

    #[test]
    fn non_object_bodies_report_js_typeof() {
        let c = db();
        seed(&c, "ARR", Some("[1,2,3]"));
        seed(&c, "STR", Some("\"hello\""));
        seed(&c, "NUM", Some("42"));
        seed(&c, "NUL", Some("null"));
        let r = call(&c, json!({}));
        assert_eq!(r["stats"]["non_object"], json!(4));
        let bucket = r["violations_by_kind"]["non_object"].as_array().unwrap();
        assert_eq!(bucket[0], json!({ "slug": "ARR", "got_type": "array" }));
        assert_eq!(bucket[1], json!({ "slug": "STR", "got_type": "string" }));
        assert_eq!(bucket[2], json!({ "slug": "NUM", "got_type": "number" }));
        assert_eq!(bucket[3], json!({ "slug": "NUL", "got_type": "object" }));
    }

    #[test]
    fn missing_required_field() {
        let c = db();
        seed(&c, "NOSIZE", Some(r#"{"kind":"feat"}"#));
        let r = call(&c, json!({}));
        assert_eq!(
            r["violations_by_kind"]["missing_required"],
            json!([{ "slug": "NOSIZE", "missing": ["size"] }])
        );
        assert_eq!(r["stats"]["has_violations"], json!(1));
    }

    #[test]
    fn forbidden_key_with_and_without_rename_rule() {
        let c = db();
        // "summary" is forbidden AND has a rename rule (-> motivation).
        seed(&c, "S1", Some(r#"{"kind":"feat","size":"M","summary":"x"}"#));
        // "parent" is forbidden with NO rename rule.
        seed(&c, "S2", Some(r#"{"kind":"feat","size":"M","parent":"E-1"}"#));
        let r = call(&c, json!({}));
        let bucket = r["violations_by_kind"]["forbidden_keys"].as_array().unwrap();
        assert_eq!(bucket[0], json!({ "slug": "S1", "keys": ["summary"], "rename_to": { "summary": "motivation" } }));
        assert_eq!(bucket[1], json!({ "slug": "S2", "keys": ["parent"] }));
        assert!(bucket[1].as_object().unwrap().get("rename_to").is_none());
    }

    #[test]
    fn unknown_key_bucket() {
        let c = db();
        seed(&c, "U1", Some(r#"{"kind":"feat","size":"M","totally_bogus":1}"#));
        let r = call(&c, json!({}));
        assert_eq!(
            r["violations_by_kind"]["unknown_keys"],
            json!([{ "slug": "U1", "keys": ["totally_bogus"] }])
        );
    }

    #[test]
    fn invalid_enum_kind_and_size() {
        let c = db();
        seed(&c, "K1", Some(r#"{"kind":"bogus-kind","size":"M"}"#));
        seed(&c, "Z1", Some(r#"{"kind":"feat","size":"XL"}"#));
        let r = call(&c, json!({}));
        let bucket = r["violations_by_kind"]["invalid_enum"].as_array().unwrap();
        assert_eq!(bucket[0]["slug"], json!("K1"));
        assert_eq!(bucket[0]["field"], json!("kind"));
        assert_eq!(bucket[0]["got"], json!("bogus-kind"));
        assert!(bucket[0]["allowed"].as_array().unwrap().contains(&json!("epic")));
        assert_eq!(bucket[1]["slug"], json!("Z1"));
        assert_eq!(bucket[1]["field"], json!("size"));
        assert_eq!(bucket[1]["allowed"], json!(["L", "M", "S"]));
    }

    #[test]
    fn placeholder_fields_detected_independently_of_conformance() {
        let c = db();
        // Schema-conformant (kind/size present + valid) but motivation is a placeholder stub and
        // acceptance_criteria is present-but-empty.
        seed(
            &c,
            "P1",
            Some(r#"{"kind":"feat","size":"M","motivation":"(unknown)","acceptance_criteria":[]}"#),
        );
        let r = call(&c, json!({}));
        assert_eq!(r["stats"]["conformant"], json!(1)); // still schema-conformant
        assert_eq!(r["stats"]["has_placeholder"], json!(1));
        assert_eq!(
            r["violations_by_kind"]["placeholder"],
            json!([{ "slug": "P1", "fields": ["motivation", "acceptance_criteria"] }])
        );
    }

    #[test]
    fn slug_glob_filters_scanned_rows() {
        let c = db();
        seed(&c, "FEAT-A", Some(r#"{"kind":"feat","size":"M"}"#));
        seed(&c, "FIX-B", Some(r#"{"kind":"fix","size":"S"}"#));
        let r = call(&c, json!({ "slug_glob": "FEAT-%" }));
        assert_eq!(r["total_cards"], json!(1));
        assert_eq!(r["stats"]["conformant"], json!(1));
    }

    #[test]
    fn limit_violations_caps_bucket_but_not_stats_counters() {
        let c = db();
        seed(&c, "M1", Some(r#"{"kind":"feat"}"#));
        seed(&c, "M2", Some(r#"{"kind":"feat"}"#));
        seed(&c, "M3", Some(r#"{"kind":"feat"}"#));
        let r = call(&c, json!({ "limit_violations": 1 }));
        assert_eq!(r["stats"]["has_violations"], json!(3));
        assert_eq!(r["violations_by_kind"]["missing_required"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn summary_top_keys_sorted_desc_ties_keep_first_seen_order() {
        let c = db();
        seed(&c, "A", Some(r#"{"kind":"feat","size":"M","motivation":"a"}"#));
        seed(&c, "B", Some(r#"{"kind":"feat","size":"M"}"#));
        let r = call(&c, json!({}));
        let top = r["summary_top_keys"].as_array().unwrap();
        // kind/size appear twice each (first-seen order preserved among ties); motivation once, last.
        assert_eq!(top[0], json!({ "key": "kind", "count": 2 }));
        assert_eq!(top[1], json!({ "key": "size", "count": 2 }));
        assert_eq!(top[2], json!({ "key": "motivation", "count": 1 }));
    }

    #[test]
    fn reads_the_real_committed_schema_file() {
        // Sanity check the bespoke-native fs seam itself: schema_path() must resolve to the
        // mirrored copy actually committed in this crate (MIRROR.json copied_files), and it must
        // be byte-identical to the JS source's copy (checked separately via `diff` — not
        // re-verified here since that would require reading outside CARGO_MANIFEST_DIR).
        let schema = load_schema().unwrap();
        assert_eq!(schema["required"], json!(["kind", "size"]));
        assert!(schema["properties"]["kind"]["enum"].as_array().unwrap().contains(&json!("epic")));
        assert_eq!(schema["$defs"]["renameRules"]["rules"]["summary"], json!("motivation"));
    }
}
