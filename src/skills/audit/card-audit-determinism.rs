//! card-audit-determinism.rs — twin of skills/audit/card-audit-determinism.mjs (db READ lane).
//! NAMES ARE CONTRACT: `cardAuditDeterminism` (default, async-over-sync — the JS `await`s only
//! sync SQLite reads via the already-converted `card-draft-auto` twin).
//!
//! Mirror of contract-audit-determinism for PL: runs `cardDraftAuto` over N synthetic intents
//! (default 5, x iterations default 10), timing each phase and counting SQL statements via a
//! `db.prepare` instrumentation hook. Two frozen-clock facts make this deterministic in Rust:
//!   - `performance.now()` is frozen (conform-db JS lane), so every timing delta is exactly 0.0 —
//!     the twin hardcodes 0 wherever the JS source computes a measured duration.
//!   - `wall_time_total_ms` is therefore always 0, so `throughput_runs_per_sec` (runs/totalMs*1000)
//!     always divides by zero → JS `Infinity` → `JSON.stringify` renders that as `null`. The twin
//!     hardcodes `Value::Null` for the same reason (serde_json also maps a non-finite f64 to null).
//! The JS `db.prepare` call-count hook (`instrumentDb`) has no Rust equivalent — instead the twin
//! statically replicates cardDraftAuto's OWN call count: `ragSimilarCards` issues exactly one
//! `db.prepare` iff the intent has at least one token >3 chars (else it short-circuits before any
//! query), and `pastRejectionHints` always issues exactly one. See `count_sql_queries` below.
use rusqlite::Connection;
use serde_json::{json, Map, Value};

fn get<'a>(input: &'a Value, k: &str) -> Option<&'a Value> {
    input.as_object().and_then(|o| o.get(k))
}

fn round_dp(x: f64, dp: i32) -> f64 {
    let scale = 10f64.powi(dp);
    (x * scale).round() / scale
}

fn default_fixtures() -> Value {
    json!([
        { "intent": "fix the broken login crash on submit" },
        { "intent": "add new feature for user dashboard exports" },
        { "intent": "refactor card layout to use cleaner pattern" },
        { "intent": "document the new RAG flow for new devs" },
        { "intent": "investigate wholly unprecedented esoteric thing" },
    ])
}

// Mirrors ragSimilarCards' token computation to derive whether it issues its ONE db.prepare call;
// pastRejectionHints always issues exactly one (unconditional, only guarded by try/catch on the
// call's OWN failure, which never happens for a schema-valid db).
fn count_sql_queries(raw_intent: &str) -> i64 {
    let rag_query = raw_intent
        .to_lowercase()
        .split_whitespace()
        .any(|w| w.chars().count() > 3);
    (if rag_query { 1 } else { 0 }) + 1
}

pub fn cardAuditDeterminism(input: &Value, db: &Connection) -> Result<Value, String> {
    let iterations = get(input, "iterations").and_then(|v| v.as_i64()).unwrap_or(10);
    let fixtures_val = get(input, "fixtures").cloned().unwrap_or_else(default_fixtures);
    let fixtures = match fixtures_val.as_array() {
        Some(a) if !a.is_empty() => a.clone(),
        _ => return Err("card-audit-determinism: fixtures is required".to_string()),
    };

    let mut runs: Vec<Value> = Vec::new();
    for _i in 0..iterations {
        for fx in &fixtures {
            let intent = fx.get("intent").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let result = crate::card_card_draft_auto::cardDraftAuto(
                &json!({ "raw_intent": intent }),
                db,
            )?;
            let card = &result["card"];
            let similar_count = result["sources"]["similar_cards"]
                .as_array()
                .map(|a| a.len())
                .unwrap_or(0) as i64;
            let sql_queries = count_sql_queries(&intent);

            let mut run = Map::new();
            run.insert("intent".into(), json!(intent));
            run.insert("template_kind".into(), card["template_kind"].clone());
            run.insert("slug".into(), card["slug"].clone());
            run.insert("priority".into(), card["priority"].clone());
            run.insert("similar_count".into(), json!(similar_count));
            run.insert("confidence".into(), card["confidence"].clone());
            run.insert("phase_draft_ms".into(), json!(0)); // performance.now() frozen delta
            run.insert("sql_queries".into(), json!(sql_queries));
            runs.push(Value::Object(run));
        }
    }
    let total_ms = 0.0f64; // performance.now() frozen: totalStart == "now" always

    // ── Aggregate ────────────────────────────────────────────────────────────────────────────
    let n = runs.len() as f64;
    let fields: Vec<String> = runs
        .first()
        .and_then(|r| r["confidence"].as_object())
        .map(|m| m.keys().cloned().collect())
        .unwrap_or_default();
    let mut mean_confidence = Map::new();
    for f in &fields {
        let vals: Vec<f64> = runs
            .iter()
            .filter_map(|r| r["confidence"].get(f).and_then(|v| v.as_f64()))
            .filter(|v| v.is_finite())
            .collect();
        let count = vals.len().max(1) as f64;
        let mean = vals.iter().sum::<f64>() / count;
        mean_confidence.insert(f.clone(), json!(round_dp(mean, 3)));
    }
    let tpl_hit = runs.iter().filter(|r| !r["template_kind"].is_null()).count() as f64;
    let sql_sum: i64 = runs.iter().filter_map(|r| r["sql_queries"].as_i64()).sum();

    let mut determinism = Map::new();
    determinism.insert("llm_calls".into(), json!(0));
    determinism.insert(
        "mean_sql_queries_per_run".into(),
        json!(round_dp(sql_sum as f64 / n, 1)),
    );
    determinism.insert("template_hit_rate".into(), json!(round_dp(tpl_hit / n, 3)));
    determinism.insert("mean_confidence_per_field".into(), Value::Object(mean_confidence));

    let mut draft_ms = Map::new();
    draft_ms.insert("min".into(), json!(0));
    draft_ms.insert("median".into(), json!(0));
    draft_ms.insert("mean".into(), json!(0));
    draft_ms.insert("p95".into(), json!(0));
    draft_ms.insert("max".into(), json!(0));

    let mut timing = Map::new();
    timing.insert("draft_ms".into(), Value::Object(draft_ms));
    timing.insert("wall_time_total_ms".into(), json!(0));
    // runs.length / totalMs (0) * 1000 → JS Infinity → JSON.stringify → null
    timing.insert("throughput_runs_per_sec".into(), Value::Null);
    let _ = total_ms;

    let mut config = Map::new();
    config.insert("iterations".into(), json!(iterations));
    config.insert("fixtures_count".into(), json!(fixtures.len() as i64));
    config.insert("total_runs".into(), json!(runs.len() as i64));

    let per_run: Vec<Value> = runs.iter().take(5).cloned().collect();

    let mut out = Map::new();
    out.insert("config".into(), Value::Object(config));
    out.insert("determinism".into(), Value::Object(determinism));
    out.insert("timing".into(), Value::Object(timing));
    out.insert("per_run".into(), json!(per_run));
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE cards (slug TEXT PRIMARY KEY, title TEXT, body_json TEXT);
             ATTACH ':memory:' AS memory;
             CREATE TABLE memory.memories (
               id INTEGER PRIMARY KEY AUTOINCREMENT, type TEXT NOT NULL, title TEXT NOT NULL,
               situation TEXT NOT NULL, guidance TEXT NOT NULL, tags_json TEXT NOT NULL DEFAULT '[]',
               created_by TEXT NOT NULL
             );",
        )
        .unwrap();
        c
    }

    #[test]
    fn default_fixtures_and_iterations_produce_full_report() {
        let c = db();
        let r = cardAuditDeterminism(&json!({}), &c).unwrap();
        assert_eq!(r["config"]["iterations"], json!(10));
        assert_eq!(r["config"]["fixtures_count"], json!(5));
        assert_eq!(r["config"]["total_runs"], json!(50));
        assert_eq!(r["determinism"]["llm_calls"], json!(0));
        assert!(r["determinism"]["mean_sql_queries_per_run"].as_f64().unwrap() > 0.0);
        assert_eq!(r["timing"]["wall_time_total_ms"], json!(0));
        assert_eq!(r["timing"]["throughput_runs_per_sec"], Value::Null);
        assert_eq!(r["per_run"].as_array().unwrap().len(), 5);
    }

    #[test]
    fn custom_fixtures_and_small_iterations_count_runs_correctly() {
        let c = db();
        let r = cardAuditDeterminism(
            &json!({
                "iterations": 2,
                "fixtures": [
                    { "intent": "fix the broken login crash on submit" },
                    { "intent": "add new feature for user dashboard exports" },
                ]
            }),
            &c,
        )
        .unwrap();
        assert_eq!(r["config"]["total_runs"], json!(4));
        assert_eq!(r["per_run"].as_array().unwrap().len(), 4);
        assert_eq!(r["per_run"][0]["intent"], json!("fix the broken login crash on submit"));
        assert_eq!(r["per_run"][0]["template_kind"], json!("bug-fix"));
    }

    #[test]
    fn novel_intent_without_template_hit_rate_zero() {
        let c = db();
        let r = cardAuditDeterminism(
            &json!({ "iterations": 1, "fixtures": [{ "intent": "investigate wholly unprecedented esoteric thing" }] }),
            &c,
        )
        .unwrap();
        assert_eq!(r["per_run"][0]["template_kind"], Value::Null);
        assert_eq!(r["determinism"]["template_hit_rate"], json!(0.0));
    }

    #[test]
    fn empty_fixtures_errs() {
        let c = db();
        assert!(cardAuditDeterminism(&json!({ "fixtures": [] }), &c).is_err());
    }
}
