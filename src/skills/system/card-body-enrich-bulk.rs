//! card-body-enrich-bulk.rs — twin of skills/system/card-body-enrich-bulk.mjs (db, dispatcher).
//! NAMES ARE CONTRACT: default `cardBodyEnrichBulk`.
//!
//! Full sweep over thin-bodied cards. Refuses to run unless a canary batch was operator-approved
//! within the last 24h (>=8/10 accepted/edited, no pending decisions). For each candidate slug it
//! dispatches to the already-converted `card_body_enrich::cardBodyEnrich` twin (reused, real
//! effect) and inserts a `card_enrichment_proposals` row on success; a per-slug error (e.g. the
//! deferred `aiCall` inside `cardBodyEnrich`, which is expected here since every real candidate
//! is thin/placeholder and therefore reaches the AI call) is caught and pushed into `errors`,
//! mirroring the JS `try { ... } catch (e) { out.errors.push(...) }` loop body.
//!
//! `batch_id = "bulk-" + Date.now() + "-" + randomUUID().slice(0,8)` is NOT byte-identical to
//! JS's clock/RNG — same "non-deterministic id, ported for real usage, asserted on shape not
//! exact value" precedent as brain-session-start.rs / codebase-onboard-start.rs.
//!
//! Frozen clock 2026-01-01 for the canary-window `datetime('now', '-24 hours')` guard, per
//! playbook: production SQL uses the literal frozen instant, test fixtures seed relative to it.
//!
//! `context.db` missing → JS throws `err.contextMissing('card-body-enrich-bulk','db')`;
//! unreachable here (Rust's `db: &Connection` is a required, non-nullable reference) — same
//! convention as every sibling twin.
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

const CANARY_APPROVAL_WINDOW_HOURS: i64 = 24;
const CANARY_MIN_ACCEPT_FRACTION: f64 = 0.8;

// Keep in sync with card-body-enrich.rs's `placeholder_why_re` — same JS regex source
// (PLACEHOLDER_WHY_RE), duplicated per module the same way the JS duplicates it in-file.
fn placeholder_why_re() -> regex::Regex {
    regex::Regex::new(r"(?i)\(no .*? recorded\)|\(operator to fill in\)|\(unknown\)|\(no motivation\)|\bTODO\b|\bTBD\b|\bFIXME\b|needs human content").unwrap()
}
fn has_placeholder_why(parsed: &Value) -> bool {
    let why = parsed.get("why");
    let s = match why {
        Some(Value::String(s)) => s,
        _ => return true,
    };
    let t = s.trim();
    t.is_empty() || placeholder_why_re().is_match(t)
}

// Fallback for `randomUUID()`. NOT byte-identical to node:crypto's RNG — never exercised by a
// fixture that asserts an exact batch_id (see module doc).
fn random_uuid_short() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos();
    let mut x = (nanos as u64) ^ 0x9E3779B97F4A7C15u64;
    let mut hex = String::new();
    for _ in 0..8 {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        hex.push(std::char::from_digit((x % 16) as u32, 16).unwrap());
    }
    hex
}

/// `export default async function cardBodyEnrichBulk(input, context)`
pub fn cardBodyEnrichBulk(input: &Value, db: &Connection) -> Result<Value, String> {
    let confidence_floor = match input.get("confidence_floor") {
        None | Some(Value::Null) => 0.7,
        Some(v) => js_number(Some(v)),
    };
    let max_budget_usd = match input.get("max_budget_usd") {
        None | Some(Value::Null) => 5.0,
        Some(v) => js_number(Some(v)),
    };
    // JS: `Number(limit) || 1000` — NaN AND 0 are both falsy, so either falls back to 1000.
    let limit_raw = js_number(input.get("limit"));
    let limit: i64 = if limit_raw.is_nan() || limit_raw == 0.0 { 1000 } else { limit_raw as i64 };
    let limit_clamped = limit.max(1).min(10000);

    // ── canary gate ──────────────────────────────────────────────────────
    let recent = crate::db_json::query_rows(
        db,
        &format!(
            "SELECT batch_id, MAX(created_at) AS created_at, COUNT(*) AS total, \
             SUM(CASE WHEN decision IN ('accepted','edited') THEN 1 ELSE 0 END) AS accepted, \
             SUM(CASE WHEN decided_at IS NULL THEN 1 ELSE 0 END) AS pending \
             FROM card_enrichment_proposals \
             WHERE batch_id LIKE 'canary-%' \
               AND created_at > datetime({NOW}, '-{CANARY_APPROVAL_WINDOW_HOURS} hours') \
             GROUP BY batch_id ORDER BY created_at DESC LIMIT 1",
            NOW = crate::clock::now_sql_inner()
        ),
        &[],
    )?;
    let recent_row = recent.as_array().and_then(|a| a.first()).cloned();
    let recent_row = match recent_row {
        Some(r) => r,
        None => {
            return Err(format!(
                "no canary batch within last {CANARY_APPROVAL_WINDOW_HOURS}h — run system.card.body.enrich.canary first"
            ));
        }
    };
    let batch_id_seen = recent_row.get("batch_id").and_then(Value::as_str).unwrap_or("").to_string();
    let pending = js_number(recent_row.get("pending"));
    if pending > 0.0 {
        return Err(format!(
            "canary batch {batch_id_seen} has {} undecided proposal(s); review them first",
            pending as i64
        ));
    }
    let total = js_number(recent_row.get("total"));
    let accepted = js_number(recent_row.get("accepted"));
    let accept_rate = accepted / total.max(1.0);
    if accept_rate < CANARY_MIN_ACCEPT_FRACTION {
        return Err(format!(
            "canary accept rate {:.0}% below floor {:.0}% — tune the prompt or grep filter before bulk",
            accept_rate * 100.0,
            CANARY_MIN_ACCEPT_FRACTION * 100.0
        ));
    }

    // ── thin-bodied candidates (excluding ones already in flight) ────────
    let rows = crate::db_json::query_rows(
        db,
        &format!(
            "SELECT slug, body_json FROM cards \
             WHERE body_json IS NOT NULL \
               AND slug NOT IN ( \
                 SELECT DISTINCT card_slug FROM card_enrichment_proposals \
                 WHERE decision IS NULL OR decision='accepted' \
               ) \
             LIMIT {limit_clamped}"
        ),
        &[],
    )?;

    let mut candidates: Vec<String> = Vec::new();
    if let Some(arr) = rows.as_array() {
        for r in arr {
            let body_json = r.get("body_json");
            let raw = match body_json {
                Some(Value::String(s)) => s.clone(),
                _ => continue,
            };
            let mut parsed: Value = match serde_json::from_str(&raw) {
                Ok(v) => v,
                Err(_) => continue,
            };
            if let Value::String(s) = &parsed {
                match serde_json::from_str::<Value>(s) {
                    Ok(v) => parsed = v,
                    Err(_) => continue,
                }
            }
            let obj = match parsed.as_object() {
                Some(o) if !parsed.is_array() => o,
                _ => continue,
            };
            if obj.len() > 3 && !has_placeholder_why(&parsed) {
                continue;
            }
            if let Some(slug) = r.get("slug").and_then(Value::as_str) {
                candidates.push(slug.to_string());
            }
        }
    }

    let batch_id = format!("bulk-{}-{}", chrono_now_millis(), random_uuid_short());

    let mut enriched: i64 = 0;
    let mut total_cost_usd: f64 = 0.0;
    let mut errors: Vec<Value> = Vec::new();
    let mut stopped_reason: Option<String> = None;

    for slug in &candidates {
        if total_cost_usd >= max_budget_usd {
            stopped_reason = Some(format!("budget cap reached (${max_budget_usd:.2})"));
            break;
        }
        let result = crate::system_card_body_enrich::cardBodyEnrich(
            &json!({ "slug": slug, "confidence_floor": confidence_floor }),
            db,
        );
        match result {
            Ok(proposal) => {
                let proposed_body = proposal.get("proposed_body").cloned().unwrap_or(json!({}));
                let confidence = proposal.get("confidence_overall").cloned().unwrap_or(json!(0));
                let dropped_fields = proposal.get("dropped_fields").cloned().unwrap_or(json!([]));
                let status = proposal.get("status").and_then(Value::as_str).unwrap_or("").to_string();
                let ai_call_id = proposal.get("ai_call_id").cloned().unwrap_or(Value::Null);
                let cost = proposal.get("cost_usd").cloned().unwrap_or(json!(0));
                let reasoning = proposal.get("reasoning").cloned().unwrap_or(Value::Null);
                let insert = crate::db_json::run_write(
                    db,
                    "INSERT INTO card_enrichment_proposals \
                     (card_slug, proposed_body_json, confidence_overall, dropped_fields_json, status, ai_call_id, cost_usd, reasoning, batch_id) \
                     VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
                    &[
                        proposal.get("slug").cloned().unwrap_or(json!(slug)),
                        json!(proposed_body.to_string()),
                        confidence.clone(),
                        json!(dropped_fields.to_string()),
                        json!(status),
                        ai_call_id,
                        cost.clone(),
                        reasoning,
                        json!(batch_id.clone()),
                    ],
                );
                // JS wraps both the cardBodyEnrich call AND insert.run(...) in the same
                // try/catch — an insert failure (e.g. a CHECK constraint) is caught and pushed
                // into `errors` too, not propagated. Mirror that here instead of using `?`.
                match insert {
                    Ok(_) => {
                        enriched += 1;
                        let cost_n = js_number(Some(&cost));
                        total_cost_usd += if cost_n.is_nan() { 0.0 } else { cost_n };
                    }
                    Err(e) => {
                        let msg = e.chars().take(240).collect::<String>();
                        errors.push(json!({ "slug": slug, "error": msg }));
                    }
                }
            }
            Err(e) => {
                let msg = e.chars().take(240).collect::<String>();
                errors.push(json!({ "slug": slug, "error": msg }));
            }
        }
    }
    let stopped_reason = stopped_reason.unwrap_or_else(|| "complete".to_string());

    Ok(json!({
        "batch_id": batch_id,
        "candidates_total": candidates.len(),
        "enriched": enriched,
        "total_cost_usd": total_cost_usd,
        "errors": errors,
        "stopped_reason": stopped_reason,
    }))
}

// Fallback for `Date.now()`. NOT byte-identical to JS's wall clock — same non-deterministic-id
// precedent as random_uuid_short above; ported for real usage, asserted on shape not exact value.
fn chrono_now_millis() -> u128 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    // Isolates the real `git log` calls inside cardGroundingCollect (via cardBodyEnrich) from
    // this checkout's actual history — same precedent as card-body-enrich.rs / card-grounding-
    // collect.rs. Only needed for tests that reach a real candidate (git evidence content is
    // never asserted on here; this is purely about not depending on the surrounding repo state).
    fn with_substrate_root<T>(root: &str, f: impl FnOnce() -> T) -> T {
        let _guard = crate::test_env_lock::ENV_LOCK.lock().unwrap();
        unsafe { std::env::set_var("ANTILOKI_SUBSTRATE_ROOT", root); }
        let result = f();
        unsafe { std::env::remove_var("ANTILOKI_SUBSTRATE_ROOT"); }
        result
    }
    fn temp_dir(tag: &str) -> PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!("antiloki-card-body-enrich-bulk-rs-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    // cardBodyEnrich (dispatched per-candidate) calls ensure_prompt, which needs the same
    // brain.prompt_* schema as card-body-enrich.rs's own fixture — without it, a candidate
    // errors with "no such table: brain.prompt_templates" instead of reaching the deferred
    // aiCall PARITY-DEFERRED error the "errors via deferred ai call" tests assert on.
    const BRAIN_SCHEMA: &str = "
        CREATE TABLE brain.prompt_templates ( id INTEGER PRIMARY KEY, name TEXT NOT NULL, version INTEGER NOT NULL, template_md TEXT NOT NULL, default_tier TEXT NOT NULL CHECK (default_tier IN ('low','mid','high')), default_temperature TEXT NOT NULL CHECK (default_temperature IN ('freezed','cold','warm','hot','hell')), max_output_tokens INTEGER NOT NULL DEFAULT 1024, superseded_by INTEGER, created_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')), cross_field_rules_json TEXT, judging_criteria_json TEXT, system_md TEXT, phase_flow TEXT NOT NULL DEFAULT 'single', UNIQUE (name, version) );
        CREATE TABLE brain.prompt_output_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL CHECK (field_type IN ('string','number','boolean','enum','array_of_string','array_of_object')), enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, grounded_refs_json TEXT, UNIQUE (prompt_template_id, field_name) );
        CREATE TABLE brain.prompt_input_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL CHECK (field_type IN ('string','number','boolean','enum','array_of_string','array_of_object')), enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, UNIQUE (prompt_template_id, field_name) );
    ";

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
               created_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')),
               decided_at TEXT,
               decision TEXT,
               accepted_body_json TEXT,
               batch_id TEXT
             );",
        )
        .unwrap();
        c.execute_batch("ATTACH ':memory:' AS brain").unwrap();
        c.execute_batch(BRAIN_SCHEMA).unwrap();
        c
    }
    fn ins_card(c: &Connection, slug: &str, body_json: &str) {
        c.execute(
            "INSERT INTO cards (slug, title, body_json, priority) VALUES (?1, ?1, ?2, 3)",
            rusqlite::params![slug, body_json],
        )
        .unwrap();
    }
    fn ins_canary(c: &Connection, batch_id: &str, total: i64, accepted: i64, pending: i64, created_at: &str) {
        for i in 0..total {
            let decision: Option<&str> = if i < pending { None } else if i < pending + accepted { Some("accepted") } else { Some("rejected") };
            let decided_at: Option<&str> = if decision.is_some() { Some("2026-01-01 00:00:00") } else { None };
            c.execute(
                "INSERT INTO card_enrichment_proposals (card_slug, proposed_body_json, confidence_overall, status, batch_id, created_at, decision, decided_at) \
                 VALUES (?, '{}', 0.9, 'ok', ?, ?, ?, ?)",
                rusqlite::params![format!("SLUG-{i}"), batch_id, created_at, decision, decided_at],
            )
            .unwrap();
        }
    }

    #[test]
    fn no_canary_batch_errors() {
        let c = db();
        let err = cardBodyEnrichBulk(&json!({}), &c).unwrap_err();
        assert!(err.contains("no canary batch within last 24h"), "got: {err}");
    }

    #[test]
    fn canary_batch_outside_window_treated_as_absent() {
        let c = db();
        ins_canary(&c, "canary-old", 10, 9, 0, "2025-01-01 00:00:00");
        let err = cardBodyEnrichBulk(&json!({}), &c).unwrap_err();
        assert!(err.contains("no canary batch within last 24h"), "got: {err}");
    }

    #[test]
    fn canary_batch_with_pending_errors() {
        let c = db();
        ins_canary(&c, "canary-1", 10, 8, 2, "2026-01-01 00:00:00");
        let err = cardBodyEnrichBulk(&json!({}), &c).unwrap_err();
        assert!(err.contains("undecided proposal"), "got: {err}");
    }

    #[test]
    fn canary_batch_below_accept_floor_errors() {
        let c = db();
        ins_canary(&c, "canary-2", 10, 5, 0, "2026-01-01 00:00:00");
        let err = cardBodyEnrichBulk(&json!({}), &c).unwrap_err();
        assert!(err.contains("below floor"), "got: {err}");
    }

    #[test]
    fn approved_canary_no_candidates_returns_empty_sweep() {
        let c = db();
        ins_canary(&c, "canary-3", 10, 9, 0, "2026-01-01 00:00:00");
        let out = cardBodyEnrichBulk(&json!({}), &c).unwrap();
        assert_eq!(out["candidates_total"], json!(0));
        assert_eq!(out["enriched"], json!(0));
        assert_eq!(out["stopped_reason"], json!("complete"));
        assert_eq!(out["errors"], json!([]));
    }

    #[test]
    fn thin_body_card_is_a_candidate_and_errors_via_deferred_ai_call() {
        let root = temp_dir("thin-body");
        let c = db();
        ins_canary(&c, "canary-4", 10, 9, 0, "2026-01-01 00:00:00");
        ins_card(&c, "FEAT-THIN-1", &json!({ "kind": "feat", "size": "S", "why": "Some real why." }).to_string());
        with_substrate_root(root.to_str().unwrap(), || {
            let out = cardBodyEnrichBulk(&json!({}), &c).unwrap();
            assert_eq!(out["candidates_total"], json!(1));
            assert_eq!(out["enriched"], json!(0));
            let errs = out["errors"].as_array().unwrap();
            assert_eq!(errs.len(), 1);
            assert_eq!(errs[0]["slug"], json!("FEAT-THIN-1"));
            assert!(errs[0]["error"].as_str().unwrap().contains("ai-call"), "{}", errs[0]["error"]);
        });
    }

    #[test]
    fn rich_body_with_real_why_is_excluded_from_candidates() {
        let c = db();
        ins_canary(&c, "canary-5", 10, 9, 0, "2026-01-01 00:00:00");
        let body = json!({
            "kind": "feat", "size": "M", "why": "Real motivation.",
            "scope": ["a"], "acceptance": ["b"], "verify": "run tests", "notes": "n",
        })
        .to_string();
        ins_card(&c, "FEAT-RICH-1", &body);
        let out = cardBodyEnrichBulk(&json!({}), &c).unwrap();
        assert_eq!(out["candidates_total"], json!(0));
    }

    #[test]
    fn rich_body_with_placeholder_why_is_still_a_candidate() {
        let root = temp_dir("rich-placeholder");
        let c = db();
        ins_canary(&c, "canary-6", 10, 9, 0, "2026-01-01 00:00:00");
        let body = json!({
            "kind": "feat", "size": "M", "why": "(no motivation recorded)",
            "scope": ["a"], "acceptance": ["b"], "verify": "run tests", "notes": "n",
        })
        .to_string();
        ins_card(&c, "FEAT-RICH-PH", &body);
        with_substrate_root(root.to_str().unwrap(), || {
            let out = cardBodyEnrichBulk(&json!({}), &c).unwrap();
            assert_eq!(out["candidates_total"], json!(1));
        });
    }

    #[test]
    fn zero_budget_stops_immediately_on_first_candidate() {
        let c = db();
        ins_canary(&c, "canary-7", 10, 9, 0, "2026-01-01 00:00:00");
        ins_card(&c, "FEAT-THIN-2", &json!({ "kind": "feat", "size": "S", "why": "x" }).to_string());
        // Zero budget breaks BEFORE the dispatch to cardBodyEnrich, so no git call happens here —
        // no substrate-root isolation needed.
        let out = cardBodyEnrichBulk(&json!({ "max_budget_usd": 0.0 }), &c).unwrap();
        assert_eq!(out["candidates_total"], json!(1));
        assert_eq!(out["enriched"], json!(0));
        assert_eq!(out["errors"], json!([]));
        assert!(out["stopped_reason"].as_str().unwrap().starts_with("budget cap reached"));
    }

    #[test]
    fn in_flight_slug_excluded_from_candidates() {
        let c = db();
        ins_canary(&c, "canary-8", 10, 9, 0, "2026-01-01 00:00:00");
        ins_card(&c, "FEAT-INFLIGHT", &json!({ "kind": "feat", "size": "S" }).to_string());
        c.execute(
            "INSERT INTO card_enrichment_proposals (card_slug, proposed_body_json, confidence_overall, status, batch_id) \
             VALUES ('FEAT-INFLIGHT', '{}', 0.5, 'ok', 'bulk-earlier')",
            [],
        )
        .unwrap();
        let out = cardBodyEnrichBulk(&json!({}), &c).unwrap();
        assert_eq!(out["candidates_total"], json!(0));
    }

    #[test]
    fn has_placeholder_why_matches_stub_patterns() {
        assert!(has_placeholder_why(&json!({ "why": "TODO" })));
        assert!(has_placeholder_why(&json!({})));
        assert!(!has_placeholder_why(&json!({ "why": "A real explanation." })));
    }
}
