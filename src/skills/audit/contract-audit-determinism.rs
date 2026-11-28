//! contract-audit-determinism.rs — twin of skills/audit/contract-audit-determinism.mjs.
//! NAMES ARE CONTRACT: `contractAuditDeterminism` (default).
//!
//! Runs contract.draft.auto + gate.tl.sign over a synthetic card set, times every phase, counts
//! SQL calls, and reports per-field determinism. Answers three questions: is the pipeline
//! deterministic (llm_calls == 0), how long does each phase take, and which fields still lean on
//! RAG rather than a deterministic template lookup.
//!
//! ## MIRRORED, not translated: how SQL is counted
//! JS monkeypatches `db.prepare` — it replaces the method, counts, delegates, and restores on
//! teardown. Rust has no method to reassign, and this module was previously listed as untwinnable
//! for exactly that reason. But the *purpose* — "observe every statement this connection runs" —
//! is a first-class rusqlite feature: `Connection::trace` (feature `trace`), the callback SQLite
//! itself invokes per prepared statement. That is the native mirror, and it is strictly better
//! than the monkeypatch: it cannot be bypassed by code that holds a reference to the original
//! `prepare`, and there is no teardown to forget.
//!
//! `trace` takes a bare `fn(&str)`, not a closure, so the counter is a process-global
//! `AtomicUsize` rather than a captured variable. That is the one shape difference from the JS,
//! and it is why `SQL_COUNTER` is reset at the start of each measured window instead of
//! snapshotted-and-subtracted.
//!
//! ## Timing
//! JS uses `performance.now()`. The conform harness freezes it to a constant so every delta is
//! exactly 0; this twin uses a real `Instant` in production and reports 0.0 under the frozen lane,
//! matching that contract (see the crossing playbook's performance.now() rule).
use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::sync::atomic::{AtomicUsize, Ordering};

static SQL_COUNTER: AtomicUsize = AtomicUsize::new(0);

/// The `Connection::trace` callback. A bare fn (SQLite's contract), so it reports into a global.
fn count_sql(_sql: &str) {
    SQL_COUNTER.fetch_add(1, Ordering::Relaxed);
}

/// JS `FIXTURES` — transcribed verbatim, order preserved.
fn default_fixtures() -> Value {
    json!([
        { "slug": "AUDIT-LAW-HOOK",    "title": "Add law-hook for X",              "body": "law-hook constitutional enforce" },
        { "slug": "AUDIT-HTML-RENDER", "title": "Visualize the foo as HTML",       "body": "render html page svg" },
        { "slug": "AUDIT-MIGRATION",   "title": "Add migration for new column",    "body": "schema migration add column" },
        { "slug": "AUDIT-ACTION-ADD",  "title": "Register new action",             "body": "new action add action" },
        { "slug": "AUDIT-NOVEL",       "title": "Wholly novel architectural shift","body": "nothing matches any template" }
    ])
}

fn round2(v: f64) -> Value {
    let r: f64 = format!("{v:.2}").parse().unwrap_or(v);
    json!(r)
}
fn round3(v: f64) -> Value {
    let r: f64 = format!("{v:.3}").parse().unwrap_or(v);
    json!(r)
}
fn round1(v: f64) -> Value {
    let r: f64 = format!("{v:.1}").parse().unwrap_or(v);
    json!(r)
}

/// JS `aggregate(runs)` — phase percentiles, per-field mean confidence, template hit rate,
/// tier distribution, and the two all-* booleans.
fn aggregate(runs: &[Value]) -> Value {
    let mut phase_stats = Map::new();
    for k in ["draft_ms", "sign_ms", "get_ms"] {
        let mut vals: Vec<f64> = runs
            .iter()
            .filter_map(|r| r.get("phases").and_then(|p| p.get(k)).and_then(Value::as_f64))
            .filter(|v| v.is_finite())
            .collect();
        vals.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let n = vals.len();
        let sum: f64 = vals.iter().sum();
        let mut m = Map::new();
        // JS indexes vals[...] which is `undefined` on an empty list -> +undefined -> NaN -> the
        // key is present with value null after JSON.stringify. Empty here yields null too.
        let at = |i: usize| -> Value { vals.get(i).map(|v| round2(*v)).unwrap_or(Value::Null) };
        m.insert("min".into(), at(0));
        m.insert("median".into(), at(n / 2));
        m.insert("mean".into(), if n == 0 { round2(f64::NAN) } else { round2(sum / n.max(1) as f64) });
        m.insert("p95".into(), at(((n as f64) * 0.95).floor() as usize));
        m.insert("max".into(), if n == 0 { Value::Null } else { at(n - 1) });
        phase_stats.insert(k.to_string(), Value::Object(m));
    }

    // fields come from the FIRST run's confidence object (JS: Object.keys(runs[0]?.confidence ?? {}))
    let fields: Vec<String> = runs
        .first()
        .and_then(|r| r.get("confidence"))
        .and_then(Value::as_object)
        .map(|o| o.keys().cloned().collect())
        .unwrap_or_default();
    let mut mean_conf = Map::new();
    for f in &fields {
        let vals: Vec<f64> = runs
            .iter()
            .filter_map(|r| r.get("confidence").and_then(|c| c.get(f)).and_then(Value::as_f64))
            .filter(|v| v.is_finite())
            .collect();
        let sum: f64 = vals.iter().sum();
        mean_conf.insert(f.clone(), round3(sum / vals.len().max(1) as f64));
    }

    let with_template = runs.iter().filter(|r| !r.get("template_kind").unwrap_or(&Value::Null).is_null()).count();
    let mut tier_dist = Map::new();
    for r in runs {
        let tier = match r.get("tier") {
            Some(Value::String(s)) => s.clone(),
            Some(v) if !v.is_null() => v.to_string(),
            _ => "null".to_string(),
        };
        let cur = tier_dist.get(&tier).and_then(Value::as_i64).unwrap_or(0);
        tier_dist.insert(tier, json!(cur + 1));
    }

    json!({
        "iterations": runs.len(),
        "template_hit_rate": round3(with_template as f64 / runs.len().max(1) as f64),
        "tier_distribution": Value::Object(tier_dist),
        "phase_timings_ms": Value::Object(phase_stats),
        "mean_confidence_per_field": Value::Object(mean_conf),
        "all_persisted": runs.iter().all(|r| r.get("persisted") == Some(&json!(true))),
        "all_fetched_tl_approved": runs.iter().all(|r| r.get("fetched_status") == Some(&json!("tl_approved"))),
    })
}

fn elapsed_ms(start: std::time::Instant) -> f64 {
    // performance.now() is frozen to a constant in the conform lane, so every delta is exactly 0.
    if crate::clock::is_frozen() {
        return 0.0;
    }
    start.elapsed().as_secs_f64() * 1000.0
}

fn run_one_iteration(db: &Connection, fixture: &Value, prefix: &str) -> Result<Value, String> {
    let base = fixture.get("slug").and_then(Value::as_str).unwrap_or("");
    let slug = format!("{base}-{prefix}");
    let title = fixture.get("title").cloned().unwrap_or(Value::Null);
    let body = fixture.get("body").cloned().unwrap_or(Value::Null);

    db.execute(
        "INSERT INTO cards (slug, title, status, body_json) VALUES (?, ?, ?, ?)",
        rusqlite::params![slug, title.as_str().unwrap_or(""), "backlog", serde_json::to_string(&body).unwrap_or_default()],
    )
    .map_err(|e| format!("contract-audit-determinism: {e}"))?;

    let mut phases = Map::new();

    let t = std::time::Instant::now();
    let drafted = crate::contract_contract_draft_auto::contractDraftAuto(&json!({ "card_slug": slug }), db)?;
    phases.insert("draft_ms".into(), json!(elapsed_ms(t)));

    let contract = drafted.get("contract").cloned().unwrap_or(Value::Null);
    let t = std::time::Instant::now();
    let signed = crate::agents_tl_contract_gate_tl_sign::contractGateTlSign(
        &json!({ "contract": contract, "decision": "accept", "signed_by": "audit:bench" }),
        db,
    )?;
    phases.insert("sign_ms".into(), json!(elapsed_ms(t)));

    let contract_slug = contract.get("slug").cloned().unwrap_or(Value::Null);
    let t = std::time::Instant::now();
    let fetched = crate::contract_contract_get::contractGet(&contract_slug, db).unwrap_or(Value::Null);
    phases.insert("get_ms".into(), json!(elapsed_ms(t)));

    let rag = drafted.get("sources");
    Ok(json!({
        "card_slug": slug,
        "fixture_kind": fixture.get("slug").cloned().unwrap_or(Value::Null),
        "contract_slug": contract_slug,
        "template_kind": contract.get("template_kind").cloned().unwrap_or(Value::Null),
        "tier": contract.get("tier").cloned().unwrap_or(Value::Null),
        "confidence": contract.get("confidence").cloned().unwrap_or(Value::Null),
        "rag_refs": rag.and_then(|s| s.get("rag_references")).and_then(|r| r.get("suggestions"))
                       .and_then(Value::as_array).map(|a| a.len()).unwrap_or(0),
        "rag_laws": rag.and_then(|s| s.get("rag_soft_laws")).and_then(Value::as_array).map(|a| a.len()).unwrap_or(0),
        "persisted": signed.get("persisted") == Some(&json!(true)),
        "fetched_status": fetched.get("status").cloned().unwrap_or(Value::Null),
        "phases": Value::Object(phases),
    }))
}

/// `export default async function contractAuditDeterminism(input, _context)`.
pub fn contractAuditDeterminism(input: &Value, _db: &Connection) -> Result<Value, String> {
    let iterations = input.get("iterations").and_then(Value::as_i64).unwrap_or(10).max(0);
    let fixtures = match input.get("fixtures") {
        Some(v) if v.is_array() => v.clone(),
        Some(_) => return Err("contract-audit-determinism: fixtures is required".to_string()),
        None => default_fixtures(),
    };
    let fx = fixtures.as_array().cloned().unwrap_or_default();
    if fx.is_empty() {
        return Err("contract-audit-determinism: fixtures is required".to_string());
    }

    // A dedicated in-memory substrate so the audit cannot contaminate the caller's state — JS does
    // `openDb(':memory:')` for the same reason, and the caller's `db` is deliberately unused.
    let mut db = crate::engine_open_db::openDb(&json!(":memory:"), &json!({}))?;
    db.trace(Some(count_sql));

    let total_start = std::time::Instant::now();
    let mut runs: Vec<Value> = Vec::new();
    let mut idx = 0usize;
    for i in 0..iterations {
        for f in &fx {
            SQL_COUNTER.store(0, Ordering::Relaxed);
            let mut result = run_one_iteration(&db, f, &format!("r{i}-{idx}"))?;
            idx += 1;
            let n = SQL_COUNTER.load(Ordering::Relaxed);
            if let Some(o) = result.as_object_mut() {
                o.insert("sql_queries".into(), json!(n));
            }
            runs.push(result);
        }
    }
    db.trace(None); // JS's instr.restore()

    let total_ms = elapsed_ms(total_start);
    let summary = aggregate(&runs);
    let mean_sql = runs
        .iter()
        .filter_map(|r| r.get("sql_queries").and_then(Value::as_f64))
        .sum::<f64>()
        / runs.len().max(1) as f64;

    // JS spreads `...summary` into `determinism` AFTER the three fixed keys.
    let mut determinism = Map::new();
    determinism.insert("llm_calls".into(), json!(0));
    determinism.insert("mean_sql_queries_per_run".into(), round1(mean_sql));
    determinism.insert("mean_rag_calls_per_run".into(), json!(2));
    if let Some(o) = summary.as_object() {
        for (k, v) in o {
            determinism.insert(k.clone(), v.clone());
        }
    }

    let throughput = if total_ms > 0.0 { (runs.len() as f64 / total_ms) * 1000.0 } else { 0.0 };
    Ok(json!({
        "config": { "iterations": iterations, "fixtures_count": fx.len(), "total_runs": runs.len() },
        "determinism": Value::Object(determinism),
        "timing": { "wall_time_total_ms": round2(total_ms), "throughput_runs_per_sec": round2(throughput) },
        "per_run": runs.iter().take(5).cloned().collect::<Vec<_>>(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c0() -> Connection {
        Connection::open_in_memory().unwrap()
    }

    #[test]
    fn the_trace_hook_counts_every_statement() {
        // This is the mirror of JS's db.prepare monkeypatch — prove it actually observes.
        let mut c = Connection::open_in_memory().unwrap();
        SQL_COUNTER.store(0, Ordering::Relaxed);
        c.trace(Some(count_sql));
        c.execute_batch("CREATE TABLE t (a);").unwrap();
        c.execute("INSERT INTO t VALUES (1)", []).unwrap();
        let _: i64 = c.query_row("SELECT COUNT(*) FROM t", [], |r| r.get(0)).unwrap();
        c.trace(None);
        let counted = SQL_COUNTER.load(Ordering::Relaxed);
        assert!(counted >= 3, "expected >=3 traced statements, got {counted}");

        // and after restore it stops counting — the JS teardown contract
        let before = SQL_COUNTER.load(Ordering::Relaxed);
        c.execute("INSERT INTO t VALUES (2)", []).unwrap();
        assert_eq!(SQL_COUNTER.load(Ordering::Relaxed), before, "trace(None) must stop counting");
    }

    #[test]
    fn empty_or_non_array_fixtures_are_refused() {
        assert!(contractAuditDeterminism(&json!({ "fixtures": [] }), &c0()).unwrap_err().contains("fixtures is required"));
        assert!(contractAuditDeterminism(&json!({ "fixtures": "nope" }), &c0()).unwrap_err().contains("fixtures is required"));
    }

    #[test]
    fn default_fixtures_are_the_five_transcribed_kinds_in_order() {
        let f = default_fixtures();
        let slugs: Vec<&str> = f.as_array().unwrap().iter().map(|x| x["slug"].as_str().unwrap()).collect();
        assert_eq!(slugs, vec!["AUDIT-LAW-HOOK", "AUDIT-HTML-RENDER", "AUDIT-MIGRATION", "AUDIT-ACTION-ADD", "AUDIT-NOVEL"]);
    }

    #[test]
    fn aggregate_computes_percentiles_and_the_all_flags() {
        let runs = vec![
            json!({ "phases": { "draft_ms": 1.0, "sign_ms": 2.0, "get_ms": 3.0 }, "confidence": { "a": 0.5 },
                    "template_kind": "x", "tier": "S", "persisted": true, "fetched_status": "tl_approved" }),
            json!({ "phases": { "draft_ms": 3.0, "sign_ms": 4.0, "get_ms": 5.0 }, "confidence": { "a": 1.0 },
                    "template_kind": Value::Null, "tier": "M", "persisted": true, "fetched_status": "tl_approved" }),
        ];
        let a = aggregate(&runs);
        assert_eq!(a["iterations"], json!(2));
        assert_eq!(a["phase_timings_ms"]["draft_ms"]["min"], json!(1.0));
        assert_eq!(a["phase_timings_ms"]["draft_ms"]["max"], json!(3.0));
        assert_eq!(a["phase_timings_ms"]["draft_ms"]["mean"], json!(2.0));
        assert_eq!(a["mean_confidence_per_field"]["a"], json!(0.75));
        assert_eq!(a["template_hit_rate"], json!(0.5), "one of two had a template_kind");
        assert_eq!(a["tier_distribution"]["S"], json!(1));
        assert_eq!(a["all_persisted"], json!(true));
        assert_eq!(a["all_fetched_tl_approved"], json!(true));
    }

    #[test]
    fn all_flags_go_false_on_a_single_bad_run() {
        let runs = vec![
            json!({ "phases": {}, "confidence": {}, "persisted": true,  "fetched_status": "tl_approved" }),
            json!({ "phases": {}, "confidence": {}, "persisted": false, "fetched_status": "draft" }),
        ];
        let a = aggregate(&runs);
        assert_eq!(a["all_persisted"], json!(false));
        assert_eq!(a["all_fetched_tl_approved"], json!(false));
    }

    #[test]
    fn zero_iterations_yields_an_empty_but_well_shaped_report() {
        let r = contractAuditDeterminism(&json!({ "iterations": 0 }), &c0()).unwrap();
        assert_eq!(r["config"]["total_runs"], json!(0));
        assert_eq!(r["determinism"]["llm_calls"], json!(0), "the pipeline is LLM-free by design");
        assert_eq!(r["determinism"]["mean_rag_calls_per_run"], json!(2));
        assert_eq!(r["per_run"], json!([]));
        // the three fixed determinism keys come BEFORE the spread summary keys
        let keys: Vec<&str> = r["determinism"].as_object().unwrap().keys().map(|k| k.as_str()).collect();
        assert_eq!(&keys[..3], &["llm_calls", "mean_sql_queries_per_run", "mean_rag_calls_per_run"]);
    }

    #[test]
    fn timings_are_zero_under_the_frozen_clock() {
        // performance.now() is frozen in the conform lane, so every delta must be exactly 0.
        assert!(crate::clock::is_frozen());
        assert_eq!(elapsed_ms(std::time::Instant::now()), 0.0);
    }
}
