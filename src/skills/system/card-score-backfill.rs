//! card-score-backfill.rs — twin of skills/system/card-score-backfill.mjs (db, dispatcher).
//! NAMES ARE CONTRACT: default `cardScoreBackfill`.
//!
//! Walks every card with a null priority/risk/complexity facet and dispatches each to the
//! already-converted `entities_card_score::cardScore` twin (reused, real effect / deferred
//! `aiCall`). Idempotent + budget-capped, mirroring the JS loop byte-for-byte: `processed`
//! increments before the budget check, `dry_run` short-circuits to `scored++` without dispatching,
//! and a per-slug `cardScore` error is caught into `errors` rather than propagated (same shape as
//! `card-body-enrich-bulk.rs`'s loop body).
//!
//! Because `cardScore`'s only AI leg is the deferred `aiCall` stub (always `Err`, see
//! card-score.rs's module doc), every non-dry-run candidate that passes `cardScore`'s guards
//! (slug found, rubric seeded, prompt bootstrapped) still ends up in `errors` here — the
//! `total_cost_usd` accumulation / `samples` push on the success branch is ported faithfully but
//! unreachable by this file's own fixture for that reason, same PARITY-DEFERRED convention as its
//! sibling dispatcher twins.
//!
//! `context.db` missing → JS throws `err.contextMissing('card-score-backfill', 'db')`;
//! unreachable here (`db: &Connection` is a required, non-nullable reference), same convention as
//! every sibling db-lane twin.
use rusqlite::Connection;
use serde_json::{json, Value};

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

#[allow(non_snake_case)]
pub fn cardScoreBackfill(input: &Value, db: &Connection) -> Result<Value, String> {
    let dry_run = input.get("dry_run").and_then(Value::as_bool).unwrap_or(false);

    // JS: `Number(limit) || 1000` — NaN AND 0 are both falsy, so either falls back to 1000.
    let limit_raw = js_number(input.get("limit"));
    let limit: i64 = if limit_raw.is_nan() || limit_raw == 0.0 { 1000 } else { limit_raw as i64 };
    let cap = limit.max(1).min(10000);

    let budget_raw = js_number(input.get("budget_usd_cap"));
    let budget_usd_cap = if budget_raw.is_nan() || budget_raw == 0.0 { 5.0 } else { budget_raw };
    let budget_cap = budget_usd_cap.max(0.0);

    let slug_glob = input.get("slug_glob").and_then(Value::as_str).filter(|s| !s.is_empty());

    let (where_sql, params): (String, Vec<Value>) = match slug_glob {
        Some(g) => (
            "(priority IS NULL OR risk IS NULL OR complexity IS NULL) AND slug LIKE ?".to_string(),
            vec![json!(g)],
        ),
        None => ("(priority IS NULL OR risk IS NULL OR complexity IS NULL)".to_string(), vec![]),
    };

    let rows = crate::db_json::query_rows(
        db,
        &format!("SELECT slug, title FROM cards WHERE {where_sql} ORDER BY slug LIMIT {cap}"),
        &params,
    )?;
    let rows = rows.as_array().cloned().unwrap_or_default();

    let mut processed: i64 = 0;
    let mut scored: i64 = 0;
    let skipped_already_scored: i64 = 0;
    let mut errors: Vec<Value> = Vec::new();
    let mut total_cost_usd: f64 = 0.0;
    let mut samples: Vec<Value> = Vec::new();
    let mut stopped_reason: Option<String> = None;

    for r in &rows {
        processed += 1;

        if total_cost_usd >= budget_cap {
            stopped_reason = Some(format!("budget cap reached (${budget_cap:.2})"));
            break;
        }

        let slug = r.get("slug").cloned().unwrap_or(Value::Null);

        if dry_run {
            scored += 1;
            continue;
        }

        match crate::entities_card_score::cardScore(&json!({ "slug": slug }), db) {
            Ok(score_result) => {
                let mut cost = 0.0;
                if let Some(ai_call_id) = score_result.get("ai_call_id").filter(|v| !v.is_null()) {
                    if let Ok(cost_rows) = crate::db_json::query_rows(
                        db,
                        "SELECT cost_usd_actual FROM brain.ai_calls WHERE id = ?",
                        &[ai_call_id.clone()],
                    ) {
                        if let Some(row) = cost_rows.as_array().and_then(|a| a.first()) {
                            let n = js_number(row.get("cost_usd_actual"));
                            cost = if n.is_nan() { 0.0 } else { n };
                        }
                    }
                    // Cost lookup is best-effort — a lookup failure (e.g. no `brain` ATTACH)
                    // still means the score landed; don't report it as an error.
                }
                total_cost_usd += cost;
                scored += 1;
                if samples.len() < 10 {
                    samples.push(json!({
                        "slug": slug,
                        "risk": score_result.get("risk").cloned().unwrap_or(Value::Null),
                        "complexity": score_result.get("complexity").cloned().unwrap_or(Value::Null),
                        "priority": score_result.get("priority").cloned().unwrap_or(Value::Null),
                        "score": score_result.get("score").cloned().unwrap_or(Value::Null),
                        "cost_usd": cost,
                    }));
                }
            }
            Err(e) => {
                let msg = e.chars().take(240).collect::<String>();
                errors.push(json!({ "slug": slug, "error": msg }));
            }
        }
    }

    let budget_remaining_usd = (budget_cap - total_cost_usd).max(0.0);
    let stopped_reason = stopped_reason.unwrap_or_else(|| {
        if processed == 0 { "no cards needed scoring".to_string() } else { "complete".to_string() }
    });

    Ok(json!({
        "dry_run": dry_run,
        "processed": processed,
        "scored": scored,
        "skipped_already_scored": skipped_already_scored,
        "errors": errors,
        "total_cost_usd": total_cost_usd,
        "budget_remaining_usd": budget_remaining_usd,
        "samples": samples,
        "stopped_reason": stopped_reason,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db0() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS brain; ATTACH ':memory:' AS laws").unwrap();
        c.execute_batch(
            "CREATE TABLE cards ( slug TEXT PRIMARY KEY, title TEXT, body_json TEXT, risk INTEGER, complexity INTEGER, priority INTEGER, score INTEGER, updated_at TEXT );
             CREATE TABLE laws.scoring_rubric ( facet TEXT NOT NULL, level INTEGER NOT NULL, definition TEXT NOT NULL );
             CREATE TABLE brain.ai_calls ( id INTEGER PRIMARY KEY, cost_usd_actual REAL );
             CREATE TABLE brain.prompt_templates ( id INTEGER PRIMARY KEY, name TEXT NOT NULL, version INTEGER NOT NULL, template_md TEXT NOT NULL, default_tier TEXT NOT NULL, default_temperature TEXT NOT NULL, max_output_tokens INTEGER NOT NULL DEFAULT 1024, superseded_by INTEGER, created_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')), cross_field_rules_json TEXT, judging_criteria_json TEXT, system_md TEXT, phase_flow TEXT NOT NULL DEFAULT 'single', UNIQUE (name, version) );
             CREATE TABLE brain.prompt_output_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, grounded_refs_json TEXT, UNIQUE (prompt_template_id, field_name) );
             CREATE TABLE brain.prompt_input_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, UNIQUE (prompt_template_id, field_name) );",
        ).unwrap();
        c
    }

    fn seed_rubric(c: &Connection) {
        c.execute_batch(
            "INSERT INTO laws.scoring_rubric (facet, level, definition) VALUES
               ('risk', 1, 'trivial'), ('risk', 5, 'critical'),
               ('complexity', 1, 'trivial'), ('complexity', 5, 'epic'),
               ('priority', 1, 'can wait'), ('priority', 5, 'do now');",
        )
        .unwrap();
    }

    fn ins_card(c: &Connection, slug: &str, title: &str) {
        c.execute("INSERT INTO cards (slug, title) VALUES (?, ?)", rusqlite::params![slug, title]).unwrap();
    }

    #[test]
    fn no_cards_needed_scoring() {
        let c = db0();
        let out = cardScoreBackfill(&json!({}), &c).unwrap();
        assert_eq!(out["processed"], json!(0));
        assert_eq!(out["stopped_reason"], json!("no cards needed scoring"));
    }

    #[test]
    fn dry_run_counts_without_dispatch() {
        let c = db0();
        ins_card(&c, "FEAT-A", "A");
        ins_card(&c, "FEAT-B", "B");
        let out = cardScoreBackfill(&json!({ "dry_run": true }), &c).unwrap();
        assert_eq!(out["processed"], json!(2));
        assert_eq!(out["scored"], json!(2));
        assert_eq!(out["errors"], json!([]));
        assert_eq!(out["stopped_reason"], json!("complete"));
    }

    #[test]
    fn candidate_hits_deferred_ai_boundary_and_lands_in_errors() {
        let c = db0();
        seed_rubric(&c);
        ins_card(&c, "FEAT-X", "Do the thing");
        let out = cardScoreBackfill(&json!({}), &c).unwrap();
        assert_eq!(out["processed"], json!(1));
        assert_eq!(out["scored"], json!(0));
        let errs = out["errors"].as_array().unwrap();
        assert_eq!(errs.len(), 1);
        assert_eq!(errs[0]["slug"], json!("FEAT-X"));
        assert!(errs[0]["error"].as_str().unwrap().contains("ai-call"), "{}", errs[0]["error"]);
        assert_eq!(out["stopped_reason"], json!("complete"));
    }

    #[test]
    fn zero_budget_stops_before_first_dispatch() {
        let c = db0();
        seed_rubric(&c);
        ins_card(&c, "FEAT-X", "Do the thing");
        let out = cardScoreBackfill(&json!({ "budget_usd_cap": 0.0 }), &c).unwrap();
        // budget_usd_cap 0 || 5.0 falls back to default 5.0 in JS (`Number(0) || 5.0` → 5.0
        // because 0 is falsy) — so the loop still dispatches and errors via the deferred call.
        assert_eq!(out["processed"], json!(1));
        let errs = out["errors"].as_array().unwrap();
        assert_eq!(errs.len(), 1);
    }

    #[test]
    fn slug_glob_filters_candidates() {
        let c = db0();
        ins_card(&c, "FEAT-A", "A");
        ins_card(&c, "EPIC-B", "B");
        let out = cardScoreBackfill(&json!({ "dry_run": true, "slug_glob": "EPIC-%" }), &c).unwrap();
        assert_eq!(out["processed"], json!(1));
    }

    #[test]
    fn already_scored_cards_are_excluded() {
        let c = db0();
        c.execute(
            "INSERT INTO cards (slug, title, priority, risk, complexity) VALUES ('FEAT-DONE', 'Done', 3, 3, 3)",
            [],
        )
        .unwrap();
        let out = cardScoreBackfill(&json!({}), &c).unwrap();
        assert_eq!(out["processed"], json!(0));
    }

    #[test]
    fn negative_limit_clamps_to_1() {
        let c = db0();
        for i in 0..3 {
            ins_card(&c, &format!("FEAT-{i}"), "x");
        }
        // JS: `Math.min(Math.max(1, Number(limit) || 1000), 10000)` — Number(-5) is truthy
        // (nonzero), so -5 survives the `||` and gets clamped up to 1, not defaulted to 1000.
        let out = cardScoreBackfill(&json!({ "dry_run": true, "limit": -5 }), &c).unwrap();
        assert_eq!(out["processed"], json!(1));
    }
}
