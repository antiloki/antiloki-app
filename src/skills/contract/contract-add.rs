//! contract-add.rs — twin of skills/contract/contract-add.mjs (db WRITE lane). NAMES ARE CONTRACT:
//! `contractAdd` (default). Persists a contract row from a draft, JSON-stringifying array/object
//! fields for the *_json columns, then one-shot flips the owning backlog card to 'doing'.
// QUALITY: dynamic-column INSERT (optional microtasks_json/goal_md via PRAGMA) + frozen-clock card flip
use rusqlite::Connection;
use serde_json::{json, Value};
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

// JS destructuring default: a MISSING key (undefined) takes the default; an explicit null is kept.
fn field(input: &Value, key: &str, default: Value) -> Value {
    match input.get(key) {
        None => default,
        Some(v) => v.clone(),
    }
}

// ensureJson: pre-serialized string → verbatim; null/missing → '{}' (object) or '[]' (array);
// else JSON.stringify. serde preserve_order + compact form matches JS JSON.stringify byte-for-byte.
fn ensure_json(input: &Value, key: &str, expected_object: bool) -> String {
    let empty = || -> String { if expected_object { "{}".into() } else { "[]".into() } };
    match input.get(key) {
        None | Some(Value::Null) => empty(),
        Some(Value::String(s)) => s.clone(),
        Some(other) => serde_json::to_string(other).unwrap_or_else(|_| empty()),
    }
}

pub fn contractAdd(input: &Value, db: &Connection) -> Result<Value, String> {
    let input = if input.is_object() { input.clone() } else { json!({}) };

    let slug = field(&input, "slug", Value::Null);
    let title = field(&input, "title", Value::Null);
    let status = field(&input, "status", json!("proposed"));
    let template_kind = field(&input, "template_kind", Value::Null);
    let size = field(&input, "size", Value::Null);
    let complexity = field(&input, "complexity", Value::Null);
    let risk = field(&input, "risk", Value::Null);
    let tier = field(&input, "tier", Value::Null);
    let card_slug = field(&input, "card_slug", Value::Null);
    let briefing_md = field(&input, "briefing_md", Value::Null);
    let base_sha = field(&input, "base_sha", Value::Null);
    let current_attempt = field(&input, "current_attempt", json!(1));
    let superseded_by = field(&input, "superseded_by", Value::Null);
    let signed_by = field(&input, "signed_by", Value::Null);
    let signed_at = field(&input, "signed_at", Value::Null);
    let tl_briefing_md = field(&input, "tl_briefing_md", Value::Null);
    let tl_evaluation_md = field(&input, "tl_evaluation_md", Value::Null);

    if !truthy(Some(&slug)) { return Err("contract-add: required field 'slug'".into()); }
    if !truthy(Some(&title)) { return Err("contract-add: required field 'title'".into()); }

    const VALID_STATUSES: [&str; 8] = ["proposed", "tl_approved", "human_approved", "impl", "qa", "done", "rejected", "superseded"];
    const VALID_SIZES: [&str; 4] = ["XS", "S", "M", "L"];
    const VALID_TIERS: [&str; 3] = ["trivial", "standard", "high-risk"];
    if !VALID_STATUSES.contains(&status.as_str().unwrap_or("")) {
        return Err(format!("contract-add: invalid status '{}'", status.as_str().unwrap_or("")));
    }
    if !size.is_null() && !VALID_SIZES.contains(&size.as_str().unwrap_or("")) {
        return Err(format!("contract-add: invalid size '{}'", size.as_str().unwrap_or("")));
    }
    if !tier.is_null() && !VALID_TIERS.contains(&tier.as_str().unwrap_or("")) {
        return Err(format!("contract-add: invalid tier '{}'", tier.as_str().unwrap_or("")));
    }
    if !complexity.is_null() {
        let c = complexity.as_f64().unwrap_or(f64::NAN);
        if c < 1.0 || c > 5.0 { return Err(format!("complexity must be 1..5, got {c}")); }
    }
    if !risk.is_null() {
        let r = risk.as_f64().unwrap_or(f64::NAN);
        if r < 1.0 || r > 5.0 { return Err(format!("risk must be 1..5, got {r}")); }
    }

    // Detect optional columns (added by later migrations). Older DBs fall back to the legacy shape.
    let pragma = crate::db_json::query_rows(db, "PRAGMA table_info(contracts)", &[])?;
    let col_set: HashSet<String> = pragma
        .as_array()
        .map(|a| a.iter().filter_map(|c| c.get("name").and_then(|n| n.as_str()).map(String::from)).collect())
        .unwrap_or_default();
    let has_microtasks = col_set.contains("microtasks_json");
    let has_goal_md = col_set.contains("goal_md");

    let mut cols: Vec<&str> = vec![
        "slug", "title", "status", "template_kind", "size", "complexity", "risk", "tier",
        "card_slug",
        "files_touched_json", "acceptance_json", "naming_json",
        "edge_cases_json", "must_obey_json", "out_of_scope_json", "reference_artifacts_json",
        "briefing_md", "base_sha", "current_attempt", "superseded_by",
        "signed_by", "signed_at", "tl_briefing_md", "tl_evaluation_md",
    ];
    let mut args: Vec<Value> = vec![
        slug.clone(), title, status, template_kind, size, complexity, risk, tier,
        card_slug.clone(),
        Value::String(ensure_json(&input, "files_touched", false)),
        Value::String(ensure_json(&input, "acceptance_json", false)),
        Value::String(ensure_json(&input, "naming", true)),
        Value::String(ensure_json(&input, "edge_cases", false)),
        Value::String(ensure_json(&input, "must_obey", false)),
        Value::String(ensure_json(&input, "out_of_scope", false)),
        Value::String(ensure_json(&input, "reference_artifacts", false)),
        briefing_md, base_sha, current_attempt, superseded_by,
        signed_by, signed_at, tl_briefing_md, tl_evaluation_md,
    ];
    if has_microtasks {
        cols.push("microtasks_json");
        args.push(Value::String(ensure_json(&input, "microtasks", false)));
    }
    if has_goal_md {
        cols.push("goal_md");
        args.push(field(&input, "goal_md", Value::Null));
    }
    let placeholders = vec!["?"; cols.len()].join(", ");
    let sql = format!("INSERT INTO contracts ({}) VALUES ({})", cols.join(", "), placeholders);
    crate::db_json::run_write(db, &sql, &args)?;

    // FEAT-CARD-STATUS-MIRRORS-WORK: first contract on a card flips it backlog → doing. Swallow
    // errors (card row/table may be absent in fixtures). datetime('now') pinned to the frozen clock.
    if truthy(Some(&card_slug)) {
        let _ = crate::db_json::run_write(
            db,
            &format!("UPDATE cards SET status = 'doing', updated_at = {} WHERE slug = ? AND status = 'backlog'", crate::clock::now_sql()),
            &[card_slug.clone()],
        );
    }

    let rows = crate::db_json::query_rows(db, "SELECT * FROM contracts WHERE slug = ?", &[slug])?;
    Ok(rows.as_array().and_then(|a| a.first()).cloned().unwrap_or(Value::Null))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE contracts (\
               id INTEGER PRIMARY KEY AUTOINCREMENT, slug TEXT NOT NULL UNIQUE, title TEXT NOT NULL, \
               status TEXT NOT NULL DEFAULT 'proposed', template_kind TEXT, size TEXT, complexity INTEGER, \
               risk INTEGER, tier TEXT, card_slug TEXT, files_touched_json TEXT NOT NULL DEFAULT '[]', \
               acceptance_json TEXT NOT NULL DEFAULT '[]', naming_json TEXT NOT NULL DEFAULT '{}', \
               edge_cases_json TEXT NOT NULL DEFAULT '[]', must_obey_json TEXT NOT NULL DEFAULT '[]', \
               out_of_scope_json TEXT NOT NULL DEFAULT '[]', reference_artifacts_json TEXT NOT NULL DEFAULT '[]', \
               briefing_md TEXT, base_sha TEXT, current_attempt INTEGER NOT NULL DEFAULT 1, superseded_by TEXT, \
               signed_by TEXT, signed_at TEXT, tl_briefing_md TEXT, tl_evaluation_md TEXT, \
               microtasks_json TEXT NOT NULL DEFAULT '[]', goal_md TEXT); \
             CREATE TABLE cards (slug TEXT PRIMARY KEY, status TEXT, updated_at TEXT);",
        )
        .unwrap();
        c
    }

    #[test]
    fn inserts_and_flips_backlog_card() {
        let c = db();
        c.execute("INSERT INTO cards (slug, status, updated_at) VALUES ('CARD-1','backlog','2025-12-01 00:00:00')", []).unwrap();
        let r = contractAdd(
            &json!({ "slug": "CT-1", "title": "First", "card_slug": "CARD-1",
                     "files_touched": ["a.js"], "naming": { "symbol": "foo" }, "complexity": 3 }),
            &c,
        )
        .unwrap();
        assert_eq!(r["slug"], json!("CT-1"));
        assert_eq!(r["status"], json!("proposed"));
        assert_eq!(r["files_touched_json"], json!("[\"a.js\"]"));
        assert_eq!(r["naming_json"], json!("{\"symbol\":\"foo\"}"));
        assert_eq!(r["complexity"], json!(3));
        assert_eq!(r["current_attempt"], json!(1));
        assert_eq!(r["id"], json!(1));
        let st: String = c.query_row("SELECT status FROM cards WHERE slug='CARD-1'", [], |r| r.get(0)).unwrap();
        assert_eq!(st, "doing");
    }

    #[test]
    fn defaults_and_passthrough_string() {
        let c = db();
        let r = contractAdd(&json!({ "slug": "CT-2", "title": "Second", "acceptance_json": "[\"pre\"]" }), &c).unwrap();
        assert_eq!(r["acceptance_json"], json!("[\"pre\"]"));
        assert_eq!(r["files_touched_json"], json!("[]"));
        assert_eq!(r["naming_json"], json!("{}"));
        assert!(r["card_slug"].is_null());
    }

    #[test]
    fn does_not_flip_non_backlog_card() {
        let c = db();
        c.execute("INSERT INTO cards (slug, status, updated_at) VALUES ('CARD-2','doing','2025-12-01 00:00:00')", []).unwrap();
        contractAdd(&json!({ "slug": "CT-3", "title": "Third", "card_slug": "CARD-2" }), &c).unwrap();
        let st: String = c.query_row("SELECT status FROM cards WHERE slug='CARD-2'", [], |r| r.get(0)).unwrap();
        assert_eq!(st, "doing");
    }
}
