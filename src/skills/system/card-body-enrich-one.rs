//! card-body-enrich-one.rs — twin of skills/system/card-body-enrich-one.mjs (db). NAMES ARE
//! CONTRACT: default `cardBodyEnrichOne`.
//!
//! Operator-triggered single-card enrichment: runs `card-body-enrich` for ONE card (optionally
//! steered by `operator_guidance`), supersedes prior PENDING proposals for that card (a direct
//! non-audited staging-table write, same as the JS — `card_enrichment_proposals` is not an
//! audited entity), then persists the fresh proposal.
//!
//! `context.db` missing → JS throws `err.contextMissing('card-body-enrich-one','db')`; unreachable
//! here (Rust's `db: &Connection` is a required, non-nullable reference) — same convention as
//! every sibling twin (see card-body-enrich.rs).
//!
//! `randomUUID()` → `random_uuid()` below, NOT byte-identical to node:crypto's RNG (same
//! precedent as brain-session-start.rs / atlas-smell-to-card.rs) — native tests assert structure
//! (format `manual-<millis>-<8 hex chars>`), not the exact value.
use rusqlite::Connection;
use serde_json::{json, Value};

fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}
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
fn js_number(v: Option<&Value>) -> f64 {
    match v {
        None => f64::NAN,
        Some(Value::Null) => 0.0,
        Some(Value::Bool(b)) => if *b { 1.0 } else { 0.0 },
        Some(Value::Number(n)) => n.as_f64().unwrap_or(f64::NAN),
        Some(Value::String(s)) => { let t = s.trim(); if t.is_empty() { 0.0 } else { t.parse().unwrap_or(f64::NAN) } }
        Some(_) => f64::NAN,
    }
}

// NOT byte-identical to node:crypto's randomUUID/Date.now (see module doc) — never exercised by
// conform fixtures; native tests assert shape only.
fn random_uuid8() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos();
    let mut x = (nanos as u64) ^ 0x9E3779B97F4A7C15u64;
    let mut hex = String::new();
    for _ in 0..8 {
        x ^= x << 13; x ^= x >> 7; x ^= x << 17;
        hex.push(std::char::from_digit((x % 16) as u32, 16).unwrap());
    }
    hex
}
fn now_millis() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as i64
}

/// `export default async function cardBodyEnrichOne(input, context)`
pub fn cardBodyEnrichOne(input: &Value, db: &Connection) -> Result<Value, String> {
    let slug_val = match input.get("slug").filter(|v| truthy(Some(v))) {
        Some(v) => v.clone(),
        None => return Err("card-body-enrich-one: slug is required".into()),
    };
    let slug = js_string(&slug_val);
    let operator_guidance = input.get("operator_guidance").cloned();
    let confidence_floor = match input.get("confidence_floor") {
        None | Some(Value::Null) => 0.7,
        Some(v) => js_number(Some(v)),
    };

    let mut enrich_input = json!({ "slug": slug_val, "confidence_floor": confidence_floor });
    if let Some(og) = operator_guidance {
        enrich_input["operator_guidance"] = og;
    }
    let proposal = crate::system_card_body_enrich::cardBodyEnrich(&enrich_input, db)?;

    // Supersede prior pending proposals for this card — direct write is allowed, the staging
    // table is not an audited entity (see module doc).
    crate::db_json::run_write(
        db,
        "UPDATE card_enrichment_proposals SET decision = 'rejected', decided_at = datetime('now') WHERE card_slug = ? AND decided_at IS NULL",
        &[json!(slug)],
    )?;

    let batch_id = format!("manual-{}-{}", now_millis(), random_uuid8());

    let proposed_body = proposal.get("proposed_body").cloned().unwrap_or(json!({}));
    let confidence_overall = js_number(proposal.get("confidence_overall"));
    let confidence_overall = if confidence_overall.is_nan() { 0.0 } else { confidence_overall };
    let dropped_fields = proposal.get("dropped_fields").cloned().unwrap_or(json!([]));
    let status = proposal.get("status").cloned().unwrap_or(Value::Null);
    let ai_call_id = proposal.get("ai_call_id").cloned().unwrap_or(Value::Null);
    let cost_usd = { let n = js_number(proposal.get("cost_usd")); if n.is_nan() { 0.0 } else { n } };
    let reasoning = proposal.get("reasoning").cloned().unwrap_or(Value::Null);
    let proposal_slug = proposal.get("slug").cloned().unwrap_or(json!(slug));

    let (_changes, inserted_id) = crate::db_json::run_write(
        db,
        "INSERT INTO card_enrichment_proposals \
           (card_slug, proposed_body_json, confidence_overall, dropped_fields_json, status, ai_call_id, cost_usd, reasoning, batch_id) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        &[
            proposal_slug,
            json!(proposed_body.to_string()),
            json!(confidence_overall),
            json!(dropped_fields.to_string()),
            status,
            ai_call_id,
            json!(cost_usd),
            reasoning,
            json!(batch_id.clone()),
        ],
    )?;

    let mut out = proposal.as_object().cloned().unwrap_or_default();
    out.insert("proposal_id".to_string(), json!(inserted_id));
    out.insert("batch_id".to_string(), json!(batch_id));
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE cards (slug TEXT PRIMARY KEY, title TEXT, body_json TEXT, \
             risk INTEGER, complexity INTEGER, priority INTEGER, score REAL, parent_slug TEXT);
             CREATE TABLE card_enrichment_proposals (
               id INTEGER PRIMARY KEY AUTOINCREMENT,
               card_slug TEXT NOT NULL,
               proposed_body_json TEXT NOT NULL,
               confidence_overall REAL NOT NULL,
               dropped_fields_json TEXT,
               status TEXT NOT NULL,
               ai_call_id INTEGER,
               cost_usd REAL DEFAULT 0,
               reasoning TEXT,
               created_at TEXT NOT NULL DEFAULT (datetime('now')),
               decided_at TEXT,
               decision TEXT,
               accepted_body_json TEXT,
               batch_id TEXT
             );",
        )
        .unwrap();
        c.execute_batch("ATTACH ':memory:' AS brain").unwrap();
        c.execute_batch(
            "CREATE TABLE brain.prompt_templates ( id INTEGER PRIMARY KEY, name TEXT NOT NULL, version INTEGER NOT NULL, template_md TEXT NOT NULL, default_tier TEXT NOT NULL CHECK (default_tier IN ('low','mid','high')), default_temperature TEXT NOT NULL CHECK (default_temperature IN ('freezed','cold','warm','hot','hell')), max_output_tokens INTEGER NOT NULL DEFAULT 1024, superseded_by INTEGER, created_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')), cross_field_rules_json TEXT, judging_criteria_json TEXT, system_md TEXT, phase_flow TEXT NOT NULL DEFAULT 'single', UNIQUE (name, version) );
             CREATE TABLE brain.prompt_output_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL CHECK (field_type IN ('string','number','boolean','enum','array_of_string','array_of_object')), enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, grounded_refs_json TEXT, UNIQUE (prompt_template_id, field_name) );
             CREATE TABLE brain.prompt_input_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL CHECK (field_type IN ('string','number','boolean','enum','array_of_string','array_of_object')), enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, UNIQUE (prompt_template_id, field_name) );",
        )
        .unwrap();
        c
    }
    fn ins_card(c: &Connection, slug: &str, title: &str, body_json: &str) {
        c.execute(
            "INSERT INTO cards (slug, title, body_json, priority) VALUES (?1, ?2, ?3, 3)",
            rusqlite::params![slug, title, body_json],
        )
        .unwrap();
    }

    #[test]
    fn missing_slug_errors() {
        let c = db();
        let err = cardBodyEnrichOne(&json!({}), &c).unwrap_err();
        assert!(err.contains("slug is required"));
    }

    #[test]
    fn propagates_underlying_enrich_error() {
        // no ANTILOKI_SUBSTRATE_ROOT set / unknown card → cardBodyEnrich errors, propagates via `?`.
        let c = db();
        let err = cardBodyEnrichOne(&json!({ "slug": "NOPE-1" }), &c).unwrap_err();
        assert!(!err.is_empty());
    }

    #[test]
    fn rich_card_persists_no_change_needed_proposal_and_supersedes_prior_pending() {
        let root = std::env::temp_dir().join(format!("antiloki-cbeo-rs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let c = db();
        let body = json!({
            "kind": "feat", "size": "M", "why": "Real motivation text here.",
            "scope": ["a"], "acceptance": ["b"], "verify": "run tests", "notes": "n",
        })
        .to_string();
        ins_card(&c, "FEAT-RICH-1", "Rich card", &body);

        // seed a prior pending proposal for the same card
        c.execute(
            "INSERT INTO card_enrichment_proposals (card_slug, proposed_body_json, confidence_overall, status, batch_id) VALUES ('FEAT-RICH-1', '{}', 0.5, 'ok', 'prior-batch')",
            [],
        )
        .unwrap();

        let _guard = crate::test_env_lock::ENV_LOCK.lock().unwrap();
        unsafe { std::env::set_var("ANTILOKI_SUBSTRATE_ROOT", root.to_str().unwrap()); }
        let out = cardBodyEnrichOne(&json!({ "slug": "FEAT-RICH-1" }), &c).unwrap();
        unsafe { std::env::remove_var("ANTILOKI_SUBSTRATE_ROOT"); }
        drop(_guard);

        assert_eq!(out["status"], json!("no_change_needed"));
        assert!(out["proposal_id"].as_i64().unwrap() > 0);
        assert!(out["batch_id"].as_str().unwrap().starts_with("manual-"));

        let prior_decision: String = c
            .query_row("SELECT decision FROM card_enrichment_proposals WHERE batch_id = 'prior-batch'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(prior_decision, "rejected");

        let count: i64 = c
            .query_row("SELECT COUNT(*) FROM card_enrichment_proposals WHERE card_slug = 'FEAT-RICH-1'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 2);
    }
}
