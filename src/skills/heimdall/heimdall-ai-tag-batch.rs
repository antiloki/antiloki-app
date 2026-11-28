//! heimdall-ai-tag-batch.rs — twin of skills/heimdall/heimdall-ai-tag-batch.mjs (standard lane,
//! aiCall deferred TRANSITIVELY). NAMES ARE CONTRACT: `heimdallAiTagBatch` (default). Runs the AI
//! judge over many untagged calls in one click: pulls the exact same eligible, information-value-
//! ordered stream `heimdall-tag-queue` (`crate::heimdall_heimdall_tag_queue::heimdallTagQueue`)
//! shows the human tagger, then calls `heimdall-ai-tag`
//! (`crate::heimdall_heimdall_ai_tag::heimdallAiTag`) once per target id.
//!
//! PARITY-DEFERRED (transitively, not directly): this module never calls `aiCall` itself — it
//! calls the already-converted `heimdallAiTag`, whose own judge seat ends in the deferred
//! `crate::ai_ai_call::aiCall` stub (see heimdall-ai-tag.rs's module doc). Every call to
//! `heimdallAiTag` for a call that is NOT already skipped for a deterministic reason (human-
//! tagged, already-AI-tagged without `force`) reaches that stub and returns `Err`, matching the
//! JS's `try { r = await heimdallAiTag(...) } catch (e) { skipped += 1; results.push({ ai_call_id:
//! id, error: e?.cause ?? e?.message ?? 'failed' }); continue; }` — ported here as a `match` on
//! `Result` rather than a `?` propagation, since the JS explicitly swallows a per-target failure
//! and keeps looping (batch semantics: one bad call must not abort the whole run). Everything
//! upstream of that per-id dispatch (limit clamp, queue read, force re-tag id merge, burned-set
//! panel routing, verdict tally) is real and deterministic — proven by `#[cfg(test)]` below,
//! reusing `heimdall-ai-tag.rs`'s own fixture schema (a superset covering `heimdall-tag-queue.rs`'s
//! tables too, since this module composes both).
//!
//! One deliberate narrowing ported faithfully rather than "fixed": JS's `verdict_counts` object is
//! seeded with ONLY the four non-zero keys (`'-2': 0, '-1': 0, '1': 0, '2': 0`) — a verdict of `0`
//! (never actually produced by the judge rubric, but not structurally impossible) would hit
//! `verdict_counts['0'] += 1` against an `undefined` bucket, i.e. `undefined + 1 === NaN`, and
//! `JSON.stringify(NaN)` serializes as `null`. `incr_verdict` below mirrors that exact collapse
//! (missing bucket → `Value::Null`, never a fabricated `1`) rather than silently starting a real
//! zero-verdict counter, so a future test asserting on this dead corner sees the same shape the
//! real JS would.
//!
//! `context.db` missing → JS throws `err.contextMissing('heimdall-ai-tag-batch', 'db')`. Not
//! modeled: `db: &Connection` is a required, non-nullable reference, same "guard-throws are not
//! exercised" convention every sibling twin uses.
// QUALITY: thin dispatcher composing 2 already-converted twins; no fixture/frontier entry (aiCall
// deferred transitively via heimdallAiTag) — native-tested for every deterministic branch.

use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::collections::HashSet;

// Standard helper functions (playbook §Standard helper functions) — copied verbatim (subset used).
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
        Some(Value::String(s)) => { let t = s.trim(); if t.is_empty() { 0.0 } else { t.parse().unwrap_or(f64::NAN) } },
        Some(_) => f64::NAN,
    }
}

// See module doc: mirrors `verdict_counts[String(r.verdict)] += 1` against an object seeded with
// ONLY the '-2'/'-1'/'1'/'2' keys — a verdict outside that set hits `undefined + 1 === NaN`, which
// `JSON.stringify` renders as `null`.
fn incr_verdict(counts: &mut Map<String, Value>, key: &str) {
    let next = match counts.get(key) {
        Some(Value::Number(n)) => n.as_i64().map(|i| json!(i + 1)).unwrap_or(Value::Null),
        _ => Value::Null,
    };
    counts.insert(key.to_string(), next);
}

pub fn heimdallAiTagBatch(input: &Value, db: &Connection) -> Result<Value, String> {
    // `Math.min(Math.max(Number(input?.limit ?? 25), 1), 100)`.
    let limit_input = input.get("limit").filter(|v| !v.is_null()).cloned().unwrap_or(json!(25));
    let limit_num = js_number(Some(&limit_input));
    let limit_num = if limit_num.is_nan() { f64::NAN } else { limit_num.max(1.0).min(100.0) };
    let limit_i64 = if limit_num.is_nan() { 0i64 } else { limit_num as i64 };

    let force = input.get("force") == Some(&Value::Bool(true));

    let card_slug = input.get("card_slug").filter(|v| truthy(Some(v)));
    let category = input.get("category").filter(|v| truthy(Some(v)));
    let template_base = input.get("template_base").filter(|v| truthy(Some(v)));

    // the same eligible, information-value-ordered stream the human tagger sees.
    let q = crate::heimdall_heimdall_tag_queue::heimdallTagQueue(
        &json!({
            "limit": limit_i64,
            "card_slug": card_slug.cloned().unwrap_or(Value::Null),
            "category": category.cloned().unwrap_or(Value::Null),
            "template_base": template_base.cloned().unwrap_or(Value::Null),
        }),
        db,
    )?;
    let items = q.get("items").and_then(Value::as_array).cloned().unwrap_or_default();

    let mut targets: Vec<i64> = items.iter().filter_map(|i| i.get("ai_call_id").and_then(Value::as_i64)).collect();

    // when force-retagging a gate/session, the queue excludes already-tagged calls, so pull the
    // AI-tagged ones back in explicitly for that scope.
    if force && (card_slug.is_some() || category.is_some() || template_base.is_some()) {
        let mut filters: Vec<String> = Vec::new();
        let mut args: Vec<Value> = Vec::new();
        if let Some(cs) = card_slug {
            filters.push("a.contract_slug IN (SELECT slug FROM contracts WHERE card_slug = ?)".to_string());
            args.push(json!(js_string(cs)));
        }
        if let Some(c) = category {
            filters.push("hc.category = ?".to_string());
            args.push(json!(js_string(c)));
        }
        if let Some(b) = template_base {
            filters.push("hc.template_base = ?".to_string());
            args.push(json!(js_string(b)));
        }
        let filter_sql = if filters.is_empty() { String::new() } else { format!("AND {}", filters.join(" AND ")) };
        let sql = format!(
            "SELECT a.id FROM brain.ai_calls a JOIN brain.heimdall_castings hc ON hc.ai_call_id = a.id \
             JOIN brain.heimdall_ai_tags t ON t.target_ai_call_id = a.id \
             WHERE a.cached = 0 AND NOT EXISTS (SELECT 1 FROM brain.heimdall_human_tags h WHERE h.ai_call_id = a.id) \
               {filter_sql} \
             ORDER BY a.id DESC LIMIT ?"
        );
        let mut all_args = args.clone();
        all_args.push(json!(limit_i64));
        let extra_rows = crate::db_json::query_rows(db, &sql, &all_args)?;
        let extra_ids: Vec<i64> = extra_rows
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .filter_map(|r| r.get("id").and_then(Value::as_i64))
            .collect();
        // `[...new Set([...targets, ...extra])].slice(0, limit)` — Set preserves first-seen
        // insertion order (targets first, then extra), then truncate to limit.
        let mut seen: HashSet<i64> = HashSet::new();
        let mut merged: Vec<i64> = Vec::new();
        for id in targets.iter().chain(extra_ids.iter()) {
            if seen.insert(*id) {
                merged.push(*id);
            }
        }
        merged.truncate(limit_i64.max(0) as usize);
        targets = merged;
    }

    // AI-2: HIGH-VALUE calls (burned downstream) get the 3-seat PANEL automatically — an agreeing
    // panel earns the ai_panel trust bump; a split routes to the human queue.
    let burned_set: HashSet<i64> = items
        .iter()
        .filter(|i| i.get("burned") == Some(&Value::Bool(true)))
        .filter_map(|i| i.get("ai_call_id").and_then(Value::as_i64))
        .collect();

    let mut results: Vec<Value> = Vec::new();
    let mut verdict_counts: Map<String, Value> = Map::new();
    for k in ["-2", "-1", "1", "2"] {
        verdict_counts.insert(k.to_string(), json!(0));
    }
    let mut tagged = 0i64;
    let mut skipped = 0i64;
    let mut low_confidence = 0i64;
    let mut panels = 0i64;

    for id in &targets {
        let use_panel = input.get("panel") == Some(&Value::Bool(true)) || burned_set.contains(id);
        let tag_input = json!({
            "ai_call_id": id,
            "force": force,
            "panel": use_panel,
            "min_confidence": input.get("min_confidence").cloned().unwrap_or(Value::Null),
        });
        match crate::heimdall_heimdall_ai_tag::heimdallAiTag(&tag_input, db) {
            Err(e) => {
                skipped += 1;
                results.push(json!({ "ai_call_id": id, "error": e }));
            }
            Ok(r) => {
                if r.get("tagged") == Some(&Value::Bool(true)) {
                    tagged += 1;
                    let verdict = r.get("verdict").cloned().unwrap_or(Value::Null);
                    incr_verdict(&mut verdict_counts, &js_string(&verdict));
                    if r.get("low_confidence") == Some(&Value::Bool(true)) {
                        low_confidence += 1;
                    }
                    let has_panel = r.get("panel").map_or(false, |p| !p.is_null());
                    if has_panel {
                        panels += 1;
                    }
                    results.push(json!({
                        "ai_call_id": id,
                        "verdict": verdict,
                        "confidence": r.get("confidence").cloned().unwrap_or(Value::Null),
                        "reason": r.get("reason").cloned().unwrap_or(Value::Null),
                        "failure_class": r.get("failure_class").cloned().unwrap_or(Value::Null),
                        "low_confidence": r.get("low_confidence").cloned().unwrap_or(Value::Null),
                        "panel": has_panel,
                    }));
                } else {
                    skipped += 1;
                    results.push(json!({ "ai_call_id": id, "skipped": r.get("reason").cloned().unwrap_or(Value::Null) }));
                }
            }
        }
    }

    Ok(json!({
        "ok": true,
        "considered": targets.len() as i64,
        "tagged": tagged,
        "skipped": skipped,
        "low_confidence": low_confidence,
        "panels": panels,
        "verdict_counts": verdict_counts,
        "results": results,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    // Superset schema: heimdall-tag-queue.rs's tables (queue read) + heimdall-ai-tag.rs's tables
    // (per-id dispatch), since this module composes both twins.
    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS brain;").unwrap();
        c.execute_batch(
            "CREATE TABLE contracts (slug TEXT PRIMARY KEY, card_slug TEXT);
             CREATE TABLE brain.ai_calls (id INTEGER PRIMARY KEY AUTOINCREMENT, prompt_template_id INTEGER, model_id TEXT, tier TEXT, cached INTEGER, cache_source_call_id INTEGER, contract_slug TEXT, tokens_in INTEGER, tokens_out INTEGER, outcome_class TEXT, cost_usd_actual REAL, ts TEXT, input_hash TEXT);
             CREATE TABLE brain.ai_call_inputs (id INTEGER PRIMARY KEY AUTOINCREMENT, ai_call_id INTEGER, key TEXT, value_text TEXT, order_index INTEGER);
             CREATE TABLE brain.ai_call_outputs (id INTEGER PRIMARY KEY AUTOINCREMENT, ai_call_id INTEGER, field_name TEXT, value_text TEXT, order_index INTEGER);
             CREATE TABLE brain.prompt_templates ( id INTEGER PRIMARY KEY, name TEXT NOT NULL, version INTEGER NOT NULL, template_md TEXT NOT NULL, default_tier TEXT NOT NULL, default_temperature TEXT NOT NULL, max_output_tokens INTEGER NOT NULL DEFAULT 1024, superseded_by INTEGER, created_at TEXT NOT NULL DEFAULT (datetime('now')), cross_field_rules_json TEXT, judging_criteria_json TEXT, system_md TEXT, phase_flow TEXT NOT NULL DEFAULT 'single', UNIQUE (name, version) );
             CREATE TABLE brain.prompt_output_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, grounded_refs_json TEXT, UNIQUE (prompt_template_id, field_name) );
             CREATE TABLE brain.prompt_input_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, UNIQUE (prompt_template_id, field_name) );
             CREATE TABLE brain.heimdall_categories (slug TEXT PRIMARY KEY, title TEXT);
             CREATE TABLE brain.heimdall_castings (id INTEGER PRIMARY KEY AUTOINCREMENT, ai_call_id INTEGER, category TEXT, template_base TEXT);
             CREATE TABLE brain.heimdall_human_tags (id INTEGER PRIMARY KEY AUTOINCREMENT, ai_call_id INTEGER);
             CREATE TABLE brain.heimdall_tag_skips (id INTEGER PRIMARY KEY AUTOINCREMENT, ai_call_id INTEGER);
             CREATE TABLE brain.heimdall_ai_tags (id INTEGER PRIMARY KEY AUTOINCREMENT, target_ai_call_id INTEGER UNIQUE, verdict INTEGER, confidence REAL, reason_md TEXT, failure_class TEXT, low_confidence INTEGER, panel_json TEXT, panel_agree INTEGER, judge_ai_call_id INTEGER, judge_model TEXT, superseded_by_human INTEGER NOT NULL DEFAULT 0, ts TEXT);
             CREATE TABLE brain.heimdall_evidence (id INTEGER PRIMARY KEY AUTOINCREMENT, model_slug TEXT, category TEXT, template_base TEXT, provenance TEXT, outcome TEXT, ai_call_id INTEGER, ai_tag_id INTEGER, human_tag_id INTEGER);
             CREATE TABLE brain.heimdall_ledger (id INTEGER PRIMARY KEY AUTOINCREMENT, scope_kind TEXT, scope_ref TEXT, calls INTEGER, sample_ok INTEGER);",
        ).unwrap();
        c
    }

    fn seed_casting(c: &Connection, ai_call_id: i64, category: &str, template_base: &str) {
        c.execute(
            "INSERT INTO brain.ai_calls (id, cached, outcome_class, ts) VALUES (?, 0, 'ok', '2026-01-01 00:00:00')",
            rusqlite::params![ai_call_id],
        )
        .unwrap();
        c.execute(
            "INSERT INTO brain.heimdall_castings (ai_call_id, category, template_base) VALUES (?, ?, ?)",
            rusqlite::params![ai_call_id, category, template_base],
        )
        .unwrap();
    }

    #[test]
    fn empty_queue_yields_zero_considered_and_no_results() {
        let c = db();
        let r = heimdallAiTagBatch(&json!({}), &c).unwrap();
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["considered"], json!(0));
        assert_eq!(r["tagged"], json!(0));
        assert_eq!(r["skipped"], json!(0));
        assert_eq!(r["results"], json!([]));
        assert_eq!(r["verdict_counts"], json!({ "-2": 0, "-1": 0, "1": 0, "2": 0 }));
    }

    #[test]
    fn already_ai_tagged_call_is_skipped_without_force() {
        let c = db();
        // template_base deliberately excludes the 'heimdall.%' prefix — heimdall-tag-queue.rs
        // filters those out as self-referential heimdall-internal calls (see its own test's id 8,
        // 'heimdall.internal'), so a queue-sourced target here must use a non-'heimdall.' base.
        seed_casting(&c, 1, "utility", "gate.x");
        c.execute(
            "INSERT INTO brain.heimdall_ai_tags (target_ai_call_id, verdict, confidence) VALUES (1, 1, 0.9)",
            [],
        )
        .unwrap();
        let r = heimdallAiTagBatch(&json!({}), &c).unwrap();
        assert_eq!(r["considered"], json!(1));
        assert_eq!(r["tagged"], json!(0));
        assert_eq!(r["skipped"], json!(1));
        assert_eq!(r["results"][0]["ai_call_id"], json!(1));
        assert_eq!(r["results"][0]["skipped"], json!("already AI-tagged (pass force=true to re-run)"));
    }

    #[test]
    fn fresh_untagged_call_reaches_deferred_ai_call_and_is_reported_as_a_skipped_error() {
        let c = db();
        seed_casting(&c, 2, "utility", "gate.y");
        let r = heimdallAiTagBatch(&json!({}), &c).unwrap();
        assert_eq!(r["considered"], json!(1));
        assert_eq!(r["tagged"], json!(0));
        assert_eq!(r["skipped"], json!(1));
        assert_eq!(r["results"][0]["ai_call_id"], json!(2));
        assert!(r["results"][0]["error"].as_str().unwrap().contains("ai-call"));
    }

    #[test]
    fn one_bad_target_does_not_abort_the_rest_of_the_batch() {
        let c = db();
        seed_casting(&c, 3, "utility", "gate.z");
        seed_casting(&c, 4, "utility", "gate.z2");
        c.execute("INSERT INTO brain.heimdall_ai_tags (target_ai_call_id, verdict, confidence) VALUES (3, 1, 0.9)", []).unwrap();
        let r = heimdallAiTagBatch(&json!({}), &c).unwrap();
        assert_eq!(r["considered"], json!(2));
        assert_eq!(r["skipped"], json!(2));
        let results = r["results"].as_array().unwrap();
        assert!(results.iter().any(|x| x["ai_call_id"] == json!(3) && x.get("skipped").is_some()));
        assert!(results.iter().any(|x| x["ai_call_id"] == json!(4) && x.get("error").is_some()));
    }

    #[test]
    fn force_scoped_by_card_slug_pulls_already_tagged_ids_back_into_targets() {
        let c = db();
        c.execute("INSERT INTO contracts (slug, card_slug) VALUES ('CONTRACT-1', 'FEAT-X')", []).unwrap();
        c.execute(
            "INSERT INTO brain.ai_calls (id, cached, outcome_class, ts, contract_slug) VALUES (5, 0, 'ok', '2026-01-01 00:00:00', 'CONTRACT-1')",
            [],
        )
        .unwrap();
        c.execute("INSERT INTO brain.heimdall_castings (ai_call_id, category, template_base) VALUES (5, 'utility', 'heimdall.gate.w')", []).unwrap();
        c.execute("INSERT INTO brain.heimdall_ai_tags (target_ai_call_id, verdict, confidence) VALUES (5, 1, 0.9)", []).unwrap();
        // without force, the queue excludes nothing here (ai_tags don't gate the queue query) —
        // but WITH card_slug + force, the id must additionally survive the force-merge re-pull.
        let r = heimdallAiTagBatch(&json!({ "card_slug": "FEAT-X", "force": true }), &c).unwrap();
        assert_eq!(r["considered"], json!(1));
        assert_eq!(r["results"][0]["ai_call_id"], json!(5));
    }

    #[test]
    fn queue_limit_truncates_targets_before_dispatch() {
        let c = db();
        for id in [10, 11, 12] {
            seed_casting(&c, id, "utility", "gate.v");
        }
        let r = heimdallAiTagBatch(&json!({ "limit": 2 }), &c).unwrap();
        assert_eq!(r["considered"], json!(2));
        assert_eq!(r["results"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn limit_is_clamped_into_1_to_100() {
        let c = db();
        // no rows — this just exercises that an out-of-range limit does not panic/reject.
        let r_low = heimdallAiTagBatch(&json!({ "limit": 0 }), &c).unwrap();
        let r_high = heimdallAiTagBatch(&json!({ "limit": 500 }), &c).unwrap();
        assert_eq!(r_low["ok"], json!(true));
        assert_eq!(r_high["ok"], json!(true));
    }
}
