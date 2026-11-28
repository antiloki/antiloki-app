//! card-normalize.rs — twin of skills/system/card-normalize.mjs (db WRITE lane; composes the
//! entities_card_update twin). NAMES ARE CONTRACT: `cardNormalize` (default). Rewrites every
//! card's body_json into the canonical shape via deterministic rules: rename-rule merges, drop
//! forbidden/unknown keys, infer kind from slug prefix, infer size from kind/tree-position,
//! synthesize a placeholder `why` from the title when missing.
//!
//! BESPOKE-NATIVE (crossing playbook VET): same fs seam as `card-lint.rs` / `card-body-generate.rs`
//! / `law-hook-card-body-schema.rs` — the JS resolves `resolve(__dirname, '..', '..', 'schemas',
//! 'card-body.schema.json')`, a path HARDCODED relative to the module's own on-disk location
//! (DEFERRED.json lists this file under builtins `fs`/`path`/`url`, not DB-derived). There is no
//! seam a conform-db fixture can drive, so this unit carries NO fixture and NO frontier entry —
//! proven instead by the `#[cfg(test)]` block below, which builds an in-memory `cards` table AND
//! reads the REAL schema file committed at `antiloki-core-rust/schemas/card-body.schema.json`
//! (mirrored from antiloki-core per MIRROR.json's `copied_files` list — byte-identical to the JS
//! source's copy). Anchors on `CARGO_MANIFEST_DIR` (compile-time constant, the Rust analog of a
//! compile-time `__dirname`) — same convention as its siblings above.
//!
//! NOT idempotent, despite the JS module header's claim ("second run produces zero audit rows"):
//! the schema's `$defs.renameRules.rules` maps `why -> motivation`, so step 6's synthesized `why`
//! becomes a rename-rule SOURCE key on the very next run — the rename loop deletes it and merges
//! it into `motivation` again, and step 6 re-synthesizes `why` from the title again. `motivation`
//! grows a new `--- legacy why ---` block every pass. This twin reproduces that real behavior
//! faithfully rather than the header's aspirational one (verified against the real schema file by
//! `skills/system/card-normalize.test.mjs`'s "second run is NOT a no-op" case).
//!
//! GOTCHA: `serde_json::Map::remove` under `preserve_order` is `swap_remove` (moves the LAST key
//! into the deleted slot) — NOT what JS `delete obj[k]` does (relative order of the remaining keys
//! is preserved). Every key drop below uses `.shift_remove()` instead.
//!
//! `context.db` missing → JS throws `err.contextMissing('card-normalize','db')`; unreachable here
//! (Rust's `db: &Connection` is a required, non-nullable reference) — same "guard-throws are not
//! exercised" convention used by every sibling twin. `requiredKeys` is computed in the JS source
//! but never referenced anywhere in the module (dead code) — this twin does not compute it either
//! since it has zero effect on behavior.
use regex::Regex;
use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::collections::HashSet;

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
// = JS String(v) (only reached once truthy(v) is already known true here).
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
// = JS Number(v).
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

fn string_list(v: Option<&Value>) -> Vec<String> {
    v.and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
        .unwrap_or_default()
}

fn schema_path() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("schemas").join("card-body.schema.json")
}

fn load_schema() -> Result<Value, String> {
    let text = std::fs::read_to_string(schema_path()).map_err(|e| e.to_string())?;
    serde_json::from_str(&text).map_err(|e| e.to_string())
}

const KIND_FROM_PREFIX: &[(&str, &str)] = &[
    ("EPIC", "epic"), ("FEAT", "feat"), ("FIX", "fix"), ("PREVENT", "prevent"),
    ("DOCS", "docs"), ("DECISION", "decision"), ("GAP", "gap"), ("PHASE", "phase"),
    ("TUI", "feat"), ("MCP", "feat"), ("ROLE", "feat"), ("LOKI", "feat"),
    ("CARD", "feat"), ("SLICE", "feat"), ("POLICY", "decision"), ("ADR", "decision"),
    ("TASK", "feat"),
];

const SIZE_FROM_KIND: &[(&str, &str)] = &[
    ("epic", "L"), ("feat", "M"), ("phase", "M"),
    ("fix", "S"), ("prevent", "S"), ("docs", "S"), ("decision", "S"), ("gap", "S"),
];

const SIZE_COERCE: &[(&str, &str)] = &[
    ("L", "L"), ("M", "M"), ("S", "S"),
    ("l", "L"), ("m", "M"), ("s", "S"),
    ("large", "L"), ("big", "L"), ("xl", "L"),
    ("medium", "M"), ("med", "M"),
    ("small", "S"), ("xs", "S"), ("extra-small", "S"), ("tiny", "S"),
    ("small-medium", "M"), ("medium-small", "M"), ("medium-large", "L"),
];

const CANONICAL_KINDS: &[&str] = &["epic", "feat", "fix", "prevent", "docs", "decision", "gap", "phase"];

const PREFERRED_ORDER: [&str; 16] = [
    "kind", "size", "why",
    "scope", "out_of_scope", "deliverables", "acceptance",
    "pre_conditions", "risks", "files_touched", "verify", "notes",
    "diagnosis", "trigger", "mitigation", "alternatives_considered",
];

fn slug_prefix(slug: &str) -> Option<String> {
    let re = Regex::new(r"^([A-Z]+)-").unwrap();
    re.captures(slug).map(|c| c[1].to_string())
}

fn infer_kind(slug: &str, existing: Option<&Value>) -> Option<String> {
    if let Some(Value::String(s)) = existing {
        if !s.is_empty() {
            let lower = s.to_lowercase();
            if CANONICAL_KINDS.contains(&lower.as_str()) {
                return Some(lower);
            }
        }
    }
    let p = slug_prefix(slug)?;
    KIND_FROM_PREFIX.iter().find(|(pre, _)| *pre == p.as_str()).map(|(_, k)| (*k).to_string())
}

fn infer_size(existing_size: Option<&Value>, kind: &str, has_children: bool) -> String {
    if let Some(v) = existing_size {
        if truthy(Some(v)) {
            if let Value::String(s) = v {
                if s == "L" || s == "M" || s == "S" {
                    return s.clone();
                }
            }
            let coerce_key = js_string(v);
            let coerce_key = coerce_key.trim();
            if let Some((_, coerced)) = SIZE_COERCE.iter().find(|(k, _)| *k == coerce_key) {
                return (*coerced).to_string();
            }
        }
    }
    if kind == "epic" {
        return "L".to_string();
    }
    if has_children {
        return "M".to_string();
    }
    SIZE_FROM_KIND.iter().find(|(k, _)| *k == kind).map(|(_, s)| (*s).to_string()).unwrap_or_else(|| "M".to_string())
}

fn order_keys(obj: &Map<String, Value>, preferred: &[&str]) -> Map<String, Value> {
    let mut out = Map::new();
    for k in preferred {
        if let Some(v) = obj.get(*k) {
            out.insert((*k).to_string(), v.clone());
        }
    }
    for (k, v) in obj.iter() {
        if !out.contains_key(k) {
            out.insert(k.clone(), v.clone());
        }
    }
    out
}

// Returns (newBody, changed, flagReasons) — the JS computeNormalizedBody().
fn compute_normalized_body(
    slug: &str,
    title: Option<&Value>,
    body_json: Option<&Value>,
    has_children: bool,
    allowed_keys: &HashSet<String>,
    forbidden_keys: &HashSet<String>,
    rename_rules: &Map<String, Value>,
) -> (Value, bool, Vec<String>) {
    let mut flag_reasons: Vec<String> = Vec::new();
    let body_truthy = truthy(body_json);
    let original: Option<Value> = if body_truthy {
        body_json.and_then(Value::as_str).and_then(|s| serde_json::from_str::<Value>(s).ok())
    } else {
        None
    };
    let is_obj = matches!(original, Some(Value::Object(_)));
    let mut base: Map<String, Value> = match original {
        Some(Value::Object(m)) => m,
        _ => Map::new(),
    };

    if body_truthy && !is_obj {
        flag_reasons.push("non-object body replaced with synthesized object".to_string());
    }
    if !body_truthy {
        flag_reasons.push("empty body synthesized from slug".to_string());
    }

    // 1. Apply rename rules (schema.$defs.renameRules.rules order = Object.entries order, since
    //    serde_json's preserve_order mirrors V8's own object-key insertion order for JSON.parse).
    //    Both old+new present: concatenate (string) or merge (array) and flag; else move the value.
    let rename_pairs: Vec<(String, String)> = rename_rules
        .iter()
        .filter_map(|(k, v)| v.as_str().map(|nk| (k.clone(), nk.to_string())))
        .collect();
    for (old_key, new_key) in rename_pairs {
        if !base.contains_key(&old_key) {
            continue;
        }
        let old_val = base.shift_remove(&old_key).unwrap();
        let old_nullish = matches!(&old_val, Value::Null) || matches!(&old_val, Value::String(s) if s.is_empty());
        if old_nullish {
            continue;
        }
        let existing_present = match base.get(&new_key) {
            None | Some(Value::Null) => false,
            Some(Value::String(s)) => !s.is_empty(),
            Some(_) => true,
        };
        if existing_present {
            let existing = base.get(&new_key).cloned().unwrap();
            match (&existing, &old_val) {
                (Value::String(es), Value::String(os)) => {
                    let merged = format!("{es}\n\n--- legacy {old_key} ---\n{os}");
                    base.insert(new_key.clone(), Value::String(merged));
                    flag_reasons.push(format!("merged legacy '{old_key}' into '{new_key}'"));
                }
                (Value::Array(ea), Value::Array(oa)) => {
                    let mut merged = ea.clone();
                    merged.extend(oa.iter().cloned());
                    base.insert(new_key.clone(), Value::Array(merged));
                    flag_reasons.push(format!("merged legacy '{old_key}' array into '{new_key}'"));
                }
                _ => {
                    flag_reasons.push(format!("unmergable legacy '{old_key}' dropped (type mismatch with '{new_key}')"));
                }
            }
        } else {
            base.insert(new_key.clone(), old_val);
        }
    }

    // 2. Drop forbidden keys that don't have a rename rule (true noise, no flag).
    let snapshot_forbidden: Vec<String> = base.keys().cloned().collect();
    for k in snapshot_forbidden {
        if forbidden_keys.contains(&k) {
            base.shift_remove(&k);
        }
    }

    // 3. Drop unknown keys (flagged so the operator can recover content if needed).
    let snapshot_unknown: Vec<String> = base.keys().cloned().collect();
    for k in snapshot_unknown {
        if !allowed_keys.contains(&k) {
            flag_reasons.push(format!("dropped unknown key '{k}'"));
            base.shift_remove(&k);
        }
    }

    // 4. Ensure kind. inferKind() re-derives when existing is non-canonical.
    let existing_kind = base.get("kind").cloned();
    let inferred_kind = infer_kind(slug, existing_kind.as_ref());
    let take_first_branch = match &inferred_kind {
        None => false,
        Some(ik) => match &existing_kind {
            Some(Value::String(ek)) => ik != ek,
            _ => true,
        },
    };
    if take_first_branch {
        let ik = inferred_kind.unwrap();
        let kind_nonnull_present = matches!(&existing_kind, Some(v) if !v.is_null());
        if kind_nonnull_present {
            let old_kind_str = tmpl(existing_kind.as_ref());
            flag_reasons.push(format!("kind '{old_kind_str}' coerced to '{ik}' (was non-canonical)"));
        }
        base.insert("kind".into(), Value::String(ik));
    } else if !truthy(existing_kind.as_ref()) {
        base.insert("kind".into(), Value::String("feat".into()));
        flag_reasons.push("kind defaulted to 'feat' (slug prefix unknown)".to_string());
    }
    // Final guard: inferKind may have returned None AND base.kind was already non-canonical.
    let now_kind = base.get("kind").cloned().unwrap_or(Value::Null);
    let is_canonical = now_kind.as_str().is_some_and(|s| CANONICAL_KINDS.contains(&s));
    if !is_canonical {
        let old_str = tmpl(Some(&now_kind));
        flag_reasons.push(format!("kind '{old_str}' not canonical; replaced with 'feat'"));
        base.insert("kind".into(), Value::String("feat".into()));
    }

    // 5. Ensure size.
    let existing_size = base.get("size").cloned();
    let kind_now = base.get("kind").and_then(Value::as_str).unwrap_or("").to_string();
    let new_size = infer_size(existing_size.as_ref(), &kind_now, has_children);
    base.insert("size".into(), Value::String(new_size.clone()));
    if let Some(v) = existing_size.as_ref() {
        if !v.is_null() {
            let same = matches!(v, Value::String(s) if s == &new_size);
            if !same {
                let old_str = tmpl(Some(v));
                flag_reasons.push(format!("size '{old_str}' overridden by inference to '{new_size}'"));
            }
        }
    }

    // 6. Ensure why. Already populated via rename rules from problem/summary/(why itself, which
    //    the schema's renameRules also treats as a legacy source of 'motivation') — if still
    //    missing, synthesize from title.
    let why_ok = matches!(base.get("why"), Some(Value::String(s)) if !s.is_empty());
    if !why_ok {
        let title_str = tmpl(title);
        base.insert("why".into(), Value::String(format!("(no motivation recorded) {title_str}")));
        flag_reasons.push("why synthesized from title — needs human content".to_string());
    }

    // 7. Order keys deterministically so the JSON is stable across runs.
    let ordered = order_keys(&base, &PREFERRED_ORDER);

    let old_str = match body_json {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(other) => js_string(other),
    };
    let new_str = serde_json::to_string(&ordered).unwrap();
    let changed = old_str != new_str;

    (Value::Object(ordered), changed, flag_reasons)
}

pub fn cardNormalize(input: &Value, db: &Connection) -> Result<Value, String> {
    let dry_run_out = match input.get("dry_run") {
        Some(v) => v.clone(),
        None => json!(false),
    };
    let write = !truthy(input.get("dry_run"));

    // cap = Math.min(Math.max(1, Number(limit) || 1000), 10000)
    let n = js_number(input.get("limit"));
    let n = if n == 0.0 || n.is_nan() { 1000.0 } else { n };
    let cap = n.max(1.0).min(10000.0) as i64;

    let schema = load_schema()?;
    let allowed_keys: HashSet<String> = schema
        .get("properties")
        .and_then(Value::as_object)
        .map(|m| m.keys().cloned().collect())
        .unwrap_or_default();
    let forbidden_keys: HashSet<String> = string_list(
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

    let slug_glob = input.get("slug_glob");
    let status = input.get("status");
    let mut clauses: Vec<&str> = Vec::new();
    let mut args: Vec<Value> = Vec::new();
    if truthy(slug_glob) {
        clauses.push("slug LIKE ?");
        args.push(slug_glob.unwrap().clone());
    }
    if truthy(status) {
        clauses.push("status = ?");
        args.push(status.unwrap().clone());
    }
    let where_clause = if clauses.is_empty() { String::new() } else { format!("WHERE {}", clauses.join(" AND ")) };
    let sql = format!("SELECT slug, title, body_json, parent_slug FROM cards {where_clause} LIMIT {cap}");
    let rows = crate::db_json::query_rows(db, &sql, &args)?.as_array().cloned().unwrap_or_default();

    // Precompute hasChildren for size inference.
    let child_rows = crate::db_json::query_rows(
        db,
        "SELECT parent_slug, COUNT(*) AS n FROM cards WHERE parent_slug IS NOT NULL GROUP BY parent_slug",
        &[],
    )?;
    let mut has_children_set: HashSet<String> = HashSet::new();
    for r in child_rows.as_array().cloned().unwrap_or_default() {
        if let Some(p) = r.get("parent_slug").and_then(Value::as_str) {
            has_children_set.insert(p.to_string());
        }
    }

    let mut processed: i64 = 0;
    let mut fixed: i64 = 0;
    let mut unchanged: i64 = 0;
    let mut flagged: Vec<Value> = Vec::new();
    let mut errors: Vec<Value> = Vec::new();
    let mut samples: Vec<Value> = Vec::new();

    for r in &rows {
        processed += 1;
        let slug_v = r.get("slug").cloned().unwrap_or(Value::Null);
        let slug_str = slug_v.as_str().unwrap_or("").to_string();
        let title_v = r.get("title").cloned();
        let body_json_v = r.get("body_json").cloned();
        let has_children = has_children_set.contains(&slug_str);

        let (new_body, changed, flag_reasons) = compute_normalized_body(
            &slug_str,
            title_v.as_ref(),
            body_json_v.as_ref(),
            has_children,
            &allowed_keys,
            &forbidden_keys,
            &rename_rules,
        );

        if !flag_reasons.is_empty() {
            flagged.push(json!({ "slug": slug_v.clone(), "reasons": flag_reasons }));
        }
        if !changed {
            unchanged += 1;
            continue;
        }

        let parsed_old: Option<Value> = if truthy(body_json_v.as_ref()) {
            body_json_v.as_ref().and_then(Value::as_str).and_then(|s| serde_json::from_str::<Value>(s).ok())
        } else {
            None
        };
        let old_keys: Vec<String> = match &parsed_old {
            Some(Value::Object(m)) => m.keys().cloned().collect(),
            _ => Vec::new(),
        };
        let new_keys: Vec<String> = new_body.as_object().unwrap().keys().cloned().collect();
        if samples.len() < 10 {
            let inferred = json!({
                "kind": new_body.get("kind").cloned().unwrap_or(Value::Null),
                "size": new_body.get("size").cloned().unwrap_or(Value::Null),
            });
            samples.push(json!({
                "slug": slug_v.clone(),
                "before_keys": old_keys,
                "after_keys": new_keys,
                "inferred": inferred,
            }));
        }

        if write {
            // Persist via card-update — composes with-audit at the action layer.
            let up = json!({ "slug": slug_v.clone(), "body": new_body.clone() });
            if let Err(e) = crate::entities_card_update::cardUpdate(&up, db) {
                let truncated: String = e.chars().take(240).collect();
                errors.push(json!({ "slug": slug_v.clone(), "error": truncated }));
                continue;
            }
        }
        fixed += 1;
    }

    let mut out = Map::new();
    out.insert("dry_run".into(), dry_run_out);
    out.insert("processed".into(), json!(processed));
    out.insert("fixed".into(), json!(fixed));
    out.insert("unchanged".into(), json!(unchanged));
    out.insert("flagged".into(), Value::Array(flagged));
    out.insert("errors".into(), Value::Array(errors));
    out.insert("samples".into(), Value::Array(samples));
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE cards (slug TEXT PRIMARY KEY, title TEXT, body_json TEXT, parent_slug TEXT, status TEXT, updated_at TEXT);",
        )
        .unwrap();
        c
    }
    fn seed(c: &Connection, slug: &str, title: &str, body_json: Option<&str>, parent_slug: Option<&str>, status: &str) {
        c.execute(
            "INSERT INTO cards (slug, title, body_json, parent_slug, status, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, '2025-01-01 00:00:00')",
            rusqlite::params![slug, title, body_json, parent_slug, status],
        )
        .unwrap();
    }
    fn body_of(c: &Connection, slug: &str) -> Value {
        let s: String = c.query_row("SELECT body_json FROM cards WHERE slug = ?1", [slug], |r| r.get(0)).unwrap();
        serde_json::from_str(&s).unwrap()
    }

    #[test]
    fn why_always_synthesized_because_rename_moves_why_to_motivation() {
        let c = db();
        seed(&c, "FEAT-A", "A title", Some(r#"{"kind":"feat","size":"M","why":"original why text"}"#), None, "backlog");
        let r = cardNormalize(&json!({}), &c).unwrap();
        assert_eq!(r["fixed"], json!(1));
        let body = body_of(&c, "FEAT-A");
        assert_eq!(body["motivation"], json!("original why text"));
        assert_eq!(body["why"], json!("(no motivation recorded) A title"));
        let flagged = r["flagged"].as_array().unwrap().iter().find(|f| f["slug"] == json!("FEAT-A")).unwrap();
        assert!(flagged["reasons"].as_array().unwrap().contains(&json!("why synthesized from title — needs human content")));
    }

    #[test]
    fn rename_rule_merge_concatenates_when_both_keys_present() {
        let c = db();
        seed(&c, "FEAT-B", "t", Some(r#"{"kind":"feat","size":"M","motivation":"existing","summary":"legacy summary"}"#), None, "backlog");
        let r = cardNormalize(&json!({}), &c).unwrap();
        let body = body_of(&c, "FEAT-B");
        assert_eq!(body["motivation"], json!("existing\n\n--- legacy summary ---\nlegacy summary"));
        assert!(body.get("summary").is_none());
        let flagged = r["flagged"].as_array().unwrap().iter().find(|f| f["slug"] == json!("FEAT-B")).unwrap();
        assert!(flagged["reasons"].as_array().unwrap().contains(&json!("merged legacy 'summary' into 'motivation'")));
    }

    #[test]
    fn forbidden_key_without_rename_rule_dropped_silently() {
        let c = db();
        seed(&c, "FEAT-C", "t", Some(r#"{"kind":"feat","size":"M","motivation":"m","parent":"EPIC-X"}"#), None, "backlog");
        let r = cardNormalize(&json!({}), &c).unwrap();
        let body = body_of(&c, "FEAT-C");
        assert!(body.get("parent").is_none());
        let flagged = r["flagged"].as_array().unwrap().iter().find(|f| f["slug"] == json!("FEAT-C")).unwrap();
        assert!(!flagged["reasons"].as_array().unwrap().iter().any(|x| x.as_str().unwrap().contains("parent")));
    }

    #[test]
    fn unknown_key_dropped_and_flagged() {
        let c = db();
        seed(&c, "FEAT-D", "t", Some(r#"{"kind":"feat","size":"M","motivation":"m","totally_bogus":1}"#), None, "backlog");
        let r = cardNormalize(&json!({}), &c).unwrap();
        let body = body_of(&c, "FEAT-D");
        assert!(body.get("totally_bogus").is_none());
        let flagged = r["flagged"].as_array().unwrap().iter().find(|f| f["slug"] == json!("FEAT-D")).unwrap();
        assert!(flagged["reasons"].as_array().unwrap().contains(&json!("dropped unknown key 'totally_bogus'")));
    }

    #[test]
    fn kind_inferred_from_slug_prefix_alias_map() {
        let c = db();
        seed(&c, "MCP-1", "t", Some(r#"{"size":"M","motivation":"m"}"#), None, "backlog");
        cardNormalize(&json!({}), &c).unwrap();
        assert_eq!(body_of(&c, "MCP-1")["kind"], json!("feat"));
    }

    #[test]
    fn unknown_slug_prefix_defaults_kind_to_feat_and_flags() {
        let c = db();
        seed(&c, "ZZZZ-1", "t", Some(r#"{"size":"M","motivation":"m"}"#), None, "backlog");
        let r = cardNormalize(&json!({}), &c).unwrap();
        assert_eq!(body_of(&c, "ZZZZ-1")["kind"], json!("feat"));
        let flagged = r["flagged"].as_array().unwrap().iter().find(|f| f["slug"] == json!("ZZZZ-1")).unwrap();
        assert!(flagged["reasons"].as_array().unwrap().contains(&json!("kind defaulted to 'feat' (slug prefix unknown)")));
    }

    #[test]
    fn non_canonical_existing_kind_coerced_via_slug_prefix() {
        let c = db();
        seed(&c, "FIX-1", "t", Some(r#"{"kind":"benchmark","size":"S","motivation":"m"}"#), None, "backlog");
        let r = cardNormalize(&json!({}), &c).unwrap();
        assert_eq!(body_of(&c, "FIX-1")["kind"], json!("fix"));
        let flagged = r["flagged"].as_array().unwrap().iter().find(|f| f["slug"] == json!("FIX-1")).unwrap();
        assert!(flagged["reasons"].as_array().unwrap().contains(&json!("kind 'benchmark' coerced to 'fix' (was non-canonical)")));
    }

    #[test]
    fn size_kept_as_is_when_already_canonical() {
        let c = db();
        seed(&c, "FEAT-E", "t", Some(r#"{"kind":"feat","size":"L","motivation":"m"}"#), None, "backlog");
        let r = cardNormalize(&json!({}), &c).unwrap();
        assert_eq!(body_of(&c, "FEAT-E")["size"], json!("L"));
        let flagged = r["flagged"].as_array().unwrap().iter().find(|f| f["slug"] == json!("FEAT-E")).unwrap();
        assert!(!flagged["reasons"].as_array().unwrap().iter().any(|x| x.as_str().unwrap().starts_with("size ")));
    }

    #[test]
    fn legacy_size_string_coerced() {
        let c = db();
        seed(&c, "FEAT-F", "t", Some(r#"{"kind":"feat","size":"large","motivation":"m"}"#), None, "backlog");
        let r = cardNormalize(&json!({}), &c).unwrap();
        assert_eq!(body_of(&c, "FEAT-F")["size"], json!("L"));
        let flagged = r["flagged"].as_array().unwrap().iter().find(|f| f["slug"] == json!("FEAT-F")).unwrap();
        assert!(flagged["reasons"].as_array().unwrap().contains(&json!("size 'large' overridden by inference to 'L'")));
    }

    #[test]
    fn missing_size_infers_l_for_epic_and_m_when_has_children() {
        let c = db();
        seed(&c, "EPIC-G", "t", Some(r#"{"kind":"epic","motivation":"m"}"#), None, "backlog");
        seed(&c, "FEAT-H", "t", Some(r#"{"kind":"feat","motivation":"m"}"#), None, "backlog");
        seed(&c, "FEAT-H-CHILD", "t", Some(r#"{"kind":"feat","motivation":"m"}"#), Some("FEAT-H"), "backlog");
        cardNormalize(&json!({}), &c).unwrap();
        assert_eq!(body_of(&c, "EPIC-G")["size"], json!("L"));
        assert_eq!(body_of(&c, "FEAT-H")["size"], json!("M"));
    }

    #[test]
    fn output_body_keys_follow_preferred_order() {
        let c = db();
        seed(&c, "FEAT-I", "t", Some(r#"{"notes":"n","kind":"feat","verify":"v","size":"M","motivation":"m"}"#), None, "backlog");
        cardNormalize(&json!({}), &c).unwrap();
        let body = body_of(&c, "FEAT-I");
        let keys: Vec<&str> = body.as_object().unwrap().keys().map(|s| s.as_str()).collect();
        assert_eq!(keys, vec!["kind", "size", "why", "verify", "notes", "motivation"]);
    }

    #[test]
    fn non_object_body_replaced_and_flagged() {
        let c = db();
        seed(&c, "FEAT-J", "array body card", Some("[1,2,3]"), None, "backlog");
        let r = cardNormalize(&json!({}), &c).unwrap();
        let body = body_of(&c, "FEAT-J");
        assert_eq!(body["kind"], json!("feat"));
        assert_eq!(body["why"], json!("(no motivation recorded) array body card"));
        let flagged = r["flagged"].as_array().unwrap().iter().find(|f| f["slug"] == json!("FEAT-J")).unwrap();
        assert!(flagged["reasons"].as_array().unwrap().contains(&json!("non-object body replaced with synthesized object")));
    }

    #[test]
    fn null_body_synthesized_and_flagged() {
        let c = db();
        seed(&c, "FEAT-K", "no body card", None, None, "backlog");
        let r = cardNormalize(&json!({}), &c).unwrap();
        let body = body_of(&c, "FEAT-K");
        assert_eq!(body["kind"], json!("feat"));
        let flagged = r["flagged"].as_array().unwrap().iter().find(|f| f["slug"] == json!("FEAT-K")).unwrap();
        assert!(flagged["reasons"].as_array().unwrap().contains(&json!("empty body synthesized from slug")));
    }

    #[test]
    fn dry_run_computes_without_writing_then_real_run_persists() {
        let c = db();
        seed(&c, "FEAT-L", "t", Some(r#"{"kind":"feat","size":"M","motivation":"m"}"#), None, "backlog");

        let dry = cardNormalize(&json!({ "dry_run": true }), &c).unwrap();
        assert_eq!(dry["dry_run"], json!(true));
        assert_eq!(dry["fixed"], json!(1));
        let untouched: String = c.query_row("SELECT body_json FROM cards WHERE slug='FEAT-L'", [], |r| r.get(0)).unwrap();
        assert_eq!(untouched, r#"{"kind":"feat","size":"M","motivation":"m"}"#);

        let real = cardNormalize(&json!({}), &c).unwrap();
        assert_eq!(real["dry_run"], json!(false));
        assert_eq!(real["fixed"], json!(1));
        assert_eq!(
            body_of(&c, "FEAT-L"),
            json!({ "kind": "feat", "size": "M", "why": "(no motivation recorded) t", "motivation": "m" })
        );
    }

    #[test]
    fn second_run_is_not_a_no_op_synthesized_why_remerges_each_pass() {
        let c = db();
        seed(&c, "FEAT-L2", "a title", Some(r#"{"kind":"feat","size":"M","motivation":"m"}"#), None, "backlog");

        let first = cardNormalize(&json!({}), &c).unwrap();
        assert_eq!(first["fixed"], json!(1));
        assert_eq!(
            body_of(&c, "FEAT-L2"),
            json!({ "kind": "feat", "size": "M", "why": "(no motivation recorded) a title", "motivation": "m" })
        );

        let second = cardNormalize(&json!({}), &c).unwrap();
        assert_eq!(second["fixed"], json!(1));
        assert_eq!(second["unchanged"], json!(0));
        let body2 = body_of(&c, "FEAT-L2");
        assert_eq!(body2["motivation"], json!("m\n\n--- legacy why ---\n(no motivation recorded) a title"));
        assert_eq!(body2["why"], json!("(no motivation recorded) a title"));
        let flagged = second["flagged"].as_array().unwrap().iter().find(|f| f["slug"] == json!("FEAT-L2")).unwrap();
        assert!(flagged["reasons"].as_array().unwrap().contains(&json!("merged legacy 'why' into 'motivation'")));
        assert!(flagged["reasons"].as_array().unwrap().contains(&json!("why synthesized from title — needs human content")));
    }

    #[test]
    fn slug_glob_and_status_filter_processed_cards() {
        let c = db();
        seed(&c, "FEAT-M1", "t", Some(r#"{"kind":"feat","size":"M","motivation":"m"}"#), None, "backlog");
        seed(&c, "FEAT-M2", "t", Some(r#"{"kind":"feat","size":"M","motivation":"m"}"#), None, "done");
        seed(&c, "FIX-N1", "t", Some(r#"{"kind":"fix","size":"S","motivation":"m"}"#), None, "backlog");

        let r1 = cardNormalize(&json!({ "slug_glob": "FEAT-%", "dry_run": true }), &c).unwrap();
        assert_eq!(r1["processed"], json!(2));

        let r2 = cardNormalize(&json!({ "status": "backlog", "dry_run": true }), &c).unwrap();
        assert_eq!(r2["processed"], json!(2));

        let r3 = cardNormalize(&json!({ "slug_glob": "FEAT-%", "status": "backlog", "dry_run": true }), &c).unwrap();
        assert_eq!(r3["processed"], json!(1));
    }

    #[test]
    fn limit_caps_processed_count() {
        let c = db();
        for i in 0..5 {
            seed(&c, &format!("FEAT-CAP-{i}"), "t", Some(r#"{"kind":"feat","size":"M","motivation":"m"}"#), None, "backlog");
        }
        let r = cardNormalize(&json!({ "limit": 2, "dry_run": true }), &c).unwrap();
        assert_eq!(r["processed"], json!(2));
    }

    #[test]
    fn samples_include_before_after_keys_and_inferred_kind_size() {
        let c = db();
        seed(&c, "FEAT-O", "t", Some(r#"{"kind":"feat","motivation":"m"}"#), None, "backlog");
        let r = cardNormalize(&json!({}), &c).unwrap();
        let sample = r["samples"].as_array().unwrap().iter().find(|s| s["slug"] == json!("FEAT-O")).unwrap();
        assert_eq!(sample["before_keys"], json!(["kind", "motivation"]));
        assert_eq!(sample["after_keys"], json!(["kind", "size", "why", "motivation"]));
        assert_eq!(sample["inferred"], json!({ "kind": "feat", "size": "M" }));
    }

    #[test]
    fn reads_the_real_committed_schema_file() {
        let schema = load_schema().unwrap();
        assert_eq!(schema["required"], json!(["kind", "size"]));
        assert!(schema["properties"]["kind"]["enum"].as_array().unwrap().contains(&json!("epic")));
        assert_eq!(schema["$defs"]["renameRules"]["rules"]["why"], json!("motivation"));
    }
}
