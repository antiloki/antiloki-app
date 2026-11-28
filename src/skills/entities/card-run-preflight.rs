//! card-run-preflight.rs — twin of skills/entities/card-run-preflight.mjs (async default export
//! `cardRunPreflight(input, context)`, now sync per the async-lane convention — the JS `await`s
//! only sync SQLite reads through the two composed checks, so the resolved value is what this
//! twin returns directly). NAMES ARE CONTRACT: `cardRunPreflight`. The RUN-button gate: composes
//! card-deps-check + card-fields-check (both already-converted twins) plus an accuracy-score
//! hard-block.
//!
//! `context.db` missing → JS throws `err.contextMissing('card-run-preflight','db')`. Unreachable
//! here: `db: &Connection` is a required, non-nullable reference (same convention every sibling
//! db-lane twin uses).
//!
//! `slug` missing/falsy → JS throws `err.required('card-run-preflight','slug')`
//! (`"card-run-preflight: slug is required"`) — the one reachable throw path, modeled as `Err`.
//!
//! deps/fields sub-checks: this twin calls the ALREADY-CONVERTED
//! `entities_card_deps_check::cardDepsCheck` / `entities_card_fields_check::cardFieldsCheck`
//! directly and propagates their `Err` verbatim via `?` — the JS doesn't catch either call
//! either, so an uncaught throw from `cardDepsCheck`/`cardFieldsCheck` propagates straight out
//! of `cardRunPreflight` too (e.g. "card '<slug>' not found").
//!
//! GAP DISCLOSED — accuracy branch (`skip_accuracy:false`, the default): the JS `await`s
//! `cardAccuracyScore(...)`, an AI-touching sub-check (calls `context.provider_fn` / the real
//! LLM route) that has NO Rust twin — it is fundamentally non-deterministic and out of scope for
//! byte-identical conform-db testing (the same reasoning that keeps every AI-route skill off the
//! frontier). This twin's `!skip_accuracy` branch therefore returns
//! `Err("card-run-preflight: accuracy scoring is not available in the native lane (unconverted
//! AI dependency card-accuracy-score)")` rather than silently fabricating a score. Per the
//! playbook's skippable-branch escape hatch (lesson 5), the fixture ONLY exercises
//! `skip_accuracy:true` — the deterministic path this gate is actually used for programmatically
//! (e.g. a pure dependency/field-completeness re-check) — and this gap is disclosed here plus in
//! the conversion manifest, not silently.

use rusqlite::Connection;
use serde_json::{json, Map, Value};

const ACCURACY_MIN: i64 = 70;

fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}

pub fn cardRunPreflight(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS `if (!slug) throw ...` is a falsy-only gate: a truthy non-string slug (number/bool/
    // array/object) is NOT rejected here and gets forwarded as-is to cardDepsCheck/
    // cardFieldsCheck. This twin narrows the gate to "must be a non-empty JSON string" instead —
    // a disclosed divergence, not chased further: real callers always pass a string card_slug (a
    // TEXT PRIMARY KEY value), and reproducing the truthy-non-string path faithfully would require
    // threading a raw Value (not &str) through both already-converted sub-checks with no realistic
    // caller ever exercising it.
    let slug = match input.get("slug").and_then(Value::as_str) {
        Some(s) if !s.is_empty() => s,
        _ => return Err("card-run-preflight: slug is required".into()),
    };
    let skip_accuracy = truthy(input.get("skip_accuracy"));

    let mut blockers: Vec<Value> = Vec::new();

    let deps = crate::entities_card_deps_check::cardDepsCheck(&json!({ "slug": slug }), db)?;
    if let Some(arr) = deps.get("blockers").and_then(Value::as_array) {
        blockers.extend(arr.iter().cloned());
    }

    let fields = crate::entities_card_fields_check::cardFieldsCheck(&json!({ "slug": slug }), db)?;
    let issues = fields.get("issues").and_then(Value::as_array).cloned().unwrap_or_default();
    for i in &issues {
        if i.get("severity") == Some(&json!("error")) {
            let field = i.get("field").and_then(Value::as_str).unwrap_or("");
            let problem = i.get("problem").and_then(Value::as_str).unwrap_or("");
            blockers.push(json!({ "code": "FIELD_INCOMPLETE", "message": format!("{field}: {problem}") }));
        }
    }

    let accuracy_score = Value::Null;
    let accuracy_summary = Value::Null;
    let field_scores: Vec<Value> = Vec::new();
    if !skip_accuracy {
        return Err(
            "card-run-preflight: accuracy scoring is not available in the native lane (unconverted AI dependency card-accuracy-score)"
                .into(),
        );
    }

    let mut out = Map::new();
    out.insert("ok".into(), json!(blockers.is_empty()));
    out.insert("blockers".into(), Value::Array(blockers));
    out.insert("field_issues".into(), Value::Array(issues));
    out.insert("accuracy_score".into(), accuracy_score);
    out.insert("accuracy_summary".into(), accuracy_summary);
    out.insert("field_scores".into(), Value::Array(field_scores));
    out.insert("accuracy_min".into(), json!(ACCURACY_MIN));
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seeded(rows: &[(&str, &str, Option<&str>, &str)]) -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE cards (slug TEXT PRIMARY KEY, status TEXT, parent_slug TEXT, body_json TEXT)",
        )
        .unwrap();
        for (slug, status, parent, body_json) in rows {
            c.execute(
                "INSERT INTO cards (slug, status, parent_slug, body_json) VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![slug, status, parent, body_json],
            )
            .unwrap();
        }
        c
    }

    const READY_BODY: &str = r#"{"kind":"feat","size":"M","motivation":"Operators keep playing cards that are not ready, wasting driver cycles on under-specified work.","acceptance_criteria":["RUN refuses when accuracy below threshold","blockers shown inline"],"context":["driver picks by priority","no blocked_by table"]}"#;

    #[test]
    fn ready_card_with_skip_accuracy_passes() {
        let c = seeded(&[("X", "backlog", None, READY_BODY)]);
        let r = cardRunPreflight(&json!({ "slug": "X", "skip_accuracy": true }), &c).unwrap();
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["blockers"], json!([]));
        assert_eq!(r["accuracy_score"], json!(null));
        assert_eq!(r["accuracy_min"], json!(70));
    }

    #[test]
    fn blocked_status_hard_blocks_even_with_skip_accuracy() {
        let c = seeded(&[("X", "blocked", None, READY_BODY)]);
        let r = cardRunPreflight(&json!({ "slug": "X", "skip_accuracy": true }), &c).unwrap();
        assert_eq!(r["ok"], json!(false));
        assert!(r["blockers"].as_array().unwrap().iter().any(|b| b["code"] == json!("STATUS_BLOCKED")));
    }

    #[test]
    fn incomplete_fields_produce_field_incomplete_blockers() {
        let c = seeded(&[("X", "backlog", None, r#"{"kind":"feat","size":"S"}"#)]);
        let r = cardRunPreflight(&json!({ "slug": "X", "skip_accuracy": true }), &c).unwrap();
        assert_eq!(r["ok"], json!(false));
        assert!(r["blockers"].as_array().unwrap().iter().any(|b| b["code"] == json!("FIELD_INCOMPLETE")));
    }

    #[test]
    fn missing_slug_and_not_found_error() {
        let c = seeded(&[("X", "backlog", None, READY_BODY)]);
        assert_eq!(
            cardRunPreflight(&json!({ "skip_accuracy": true }), &c),
            Err("card-run-preflight: slug is required".to_string())
        );
        assert_eq!(
            cardRunPreflight(&json!({ "slug": "GHOST", "skip_accuracy": true }), &c),
            Err("card-deps-check: card 'GHOST' not found".to_string())
        );
    }

    #[test]
    fn accuracy_branch_is_not_available_natively() {
        let c = seeded(&[("X", "backlog", None, READY_BODY)]);
        let r = cardRunPreflight(&json!({ "slug": "X" }), &c);
        assert!(r.is_err());
    }
}
