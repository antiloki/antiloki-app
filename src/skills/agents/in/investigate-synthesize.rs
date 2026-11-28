//! investigate-synthesize.rs — twin of skills/agents/in/investigate-synthesize.mjs (db READ+WRITE,
//! brain-attached; LLM-PARTIAL). NAMES ARE CONTRACT: `investigateSynthesize` (default),
//! `PROMPT_NAME`, `PROMPT_TEMPLATE_MD`, `PROMPT_OUTPUT_FIELDS`, `ensurePrompt`.
//!
//! PARITY-DEFERRED: the `aiCall(...)` leg is a real network call to a BYOK LLM provider — genuinely
//! non-deterministic, unfixturable in principle (see `crate::ai_ai_call::aiCall`'s own doc, and the
//! same pattern in interviewer-ai-fallback.rs). This twin calls that deferred stub, which always
//! returns `Err(..)`, so the real happy path (an actual model response reaching the output_kv
//! aggregation) cannot be exercised by a native test. Everything else is ported faithfully:
//!   - the `db` / `question` guards (`err.contextMissing`/`err.required` in JS → plain `Err(String)`
//!     here, matching this codebase's convention of dropping `context.db` optionality once the twin
//!     signature already requires `db: &Connection`),
//!   - the no-evidence cost-discipline fast path (empty/absent `findings` → canned response, no AI
//!     call, `no_evidence: true`),
//!   - the FEAT-INVESTIGATE-COST-CAP budget gate: pre-call rough token/cost estimate AND the
//!     post-call rate-limit refusal keyed off the most recent `brain.ai_calls` row for this
//!     `prompt_name` (both read `ANTILOKI_INVESTIGATE_BUDGET_USD`, default 0.05),
//!   - the `ANTILOKI_INVESTIGATE_SYNTHESIZE_STUB=1` deterministic test-mode stub,
//!   - `ensurePrompt` (find-or-create idempotency via `crate::ai_prompt_find`/`crate::ai_prompt_add`),
//!   - `fmtAge`/`renderFindingsBlock` (pure formatting helpers used both by the prompt-input assembly
//!     AND the pre-call cost estimate), and
//!   - the `output_kv` → `key_points`/`cited_refs` array aggregation + scalar-field collapse
//!     (`collapse_output_kv`, factored out as its own pure fn so the deterministic response-shaping
//!     surface is independently testable against a canned AI response).
//!
//! `provider_fn` (the JS test-injection hook for a mock provider) has no Rust equivalent — it only
//! ever reaches the deferred `aiCall` body, so it is dropped from this twin's signature the same way
//! interviewer-ai-fallback.rs drops it.
//!
//! `recent.ts + 'Z'` (JS treats the stored `brain.ai_calls.ts` as a naive UTC timestamp and appends
//! `Z` before `Date.parse`) uses the REAL wall clock (`SystemTime::now`), same convention as
//! contract-draft-auto.rs/auth-sign.rs's `now_epoch_ms` — this is a live rate-limit check, not a
//! fixtured-determinism concern. `ts` (`"YYYY-MM-DD HH:MM:SS"`, SQLite's `datetime()` format,
//! interpreted as UTC) is parsed by hand via the civil-calendar algorithm (`days_from_civil`, no
//! external date crate is a dependency of this workspace) rather than `chrono::NaiveDateTime`. A
//! malformed/absent `ts` is treated as "cannot parse" the same way JS `Number.isNaN(tsMs)` guards
//! it: the rate-limit branch is skipped, matching JS.
// QUALITY: mixed unit — guards + cost-gate + prompt-upsert + response-shaping ported/fixture-tested;
// aiCall leg PARITY-DEFERRED (network, unfixturable)

use rusqlite::{Connection, OptionalExtension};
use serde_json::{json, Map, Value};
use std::time::{SystemTime, UNIX_EPOCH};

/// Howard Hinnant's `days_from_civil` — proleptic-Gregorian (y, m, d) → days since 1970-01-01.
/// No external date crate; this workspace has no `chrono` dependency.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// Parses SQLite `datetime()` output (`"YYYY-MM-DD HH:MM:SS"`, interpreted as UTC) to epoch
/// milliseconds. `None` on any malformed input — mirrors JS `Number.isNaN(Date.parse(ts + 'Z'))`.
fn parse_sqlite_ts_to_epoch_ms(ts: &str) -> Option<i64> {
    let (date_part, time_part) = ts.split_once(' ')?;
    let mut dp = date_part.splitn(3, '-');
    let y: i64 = dp.next()?.parse().ok()?;
    let m: i64 = dp.next()?.parse().ok()?;
    let d: i64 = dp.next()?.parse().ok()?;
    let mut tp = time_part.splitn(3, ':');
    let hh: i64 = tp.next()?.parse().ok()?;
    let mm: i64 = tp.next()?.parse().ok()?;
    let ss: i64 = tp.next()?.parse().ok()?;
    let days = days_from_civil(y, m, d);
    Some((days * 86_400 + hh * 3600 + mm * 60 + ss) * 1000)
}

fn now_epoch_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

/// Inverse of `days_from_civil` (Howard Hinnant's `civil_from_days`) composed with the epoch-secs
/// → (y,m,d,hh,mm,ss) split. Test-only: builds a `datetime()`-shaped string from the real clock so
/// the rate-limit test doesn't hardcode a stale absolute timestamp.
#[cfg(test)]
fn epoch_secs_to_civil(secs: i64) -> (i64, i64, i64, i64, i64, i64) {
    let days = secs.div_euclid(86_400);
    let tod = secs.rem_euclid(86_400);
    let (hh, mm, ss) = (tod / 3600, (tod % 3600) / 60, tod % 60);
    let z = days + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d, hh, mm, ss)
}

pub const PROMPT_NAME: &str = "interviewer.investigate.synthesize.v3";

pub const PROMPT_TEMPLATE_MD: &str = "You are the INVESTIGATOR. The substrate's research phase fanned out across cards, memories, actions, skills, laws, the audit timeline, driver ticks, and runtime errors. Your job: read the findings, reason through them step by step, and produce a grounded answer.\n\nNON-NEGOTIABLE RULES:\n  1. CITE findings by their ref field. Every key claim in your answer must point at one or more refs. No ungrounded claims.\n  2. If findings are insufficient to answer the question, say so HONESTLY in summary_md. Do not fabricate. Set confidence accordingly.\n  3. Reasoning is explicit — your chain of thought goes in reasoning_md as numbered steps. The reader should see HOW you reached the conclusion.\n  4. Answer is structured: a short summary_md + key_points[] (3-7 bullets), each with cite-refs. Confidence is your honest 0..1 score.\n  5. TIME-AWARENESS: each finding has an age_seconds field. Findings ≤ 300s old (5 min) describe CURRENT state. Findings > 300s describe HISTORY. When reporting failures/errors, ALWAYS state explicitly whether they are recent or historical. Do NOT say \"X is happening\" about a finding > 5 min old — say \"X happened Nm ago\" instead.\n\nQUESTION:\n{{question}}\n\nFINDINGS ({{findings_count}} total; format: [ref] source/kind: snippet):\n{{findings_block}}\n\nOUTPUT — JSON of this exact shape:\n{\n  \"reasoning_md\": \"<numbered chain of thought — 4-8 short steps showing how findings combine into the answer>\",\n  \"summary_md\":   \"<2-4 sentences answering the question, with [ref] citations inline>\",\n  \"key_points\":   [\"<short point 1 with [ref]>\", \"<short point 2 with [ref]>\", ...3-7 items],\n  \"confidence\":   <0.0..1.0 — how grounded is this answer in the findings>,\n  \"cited_refs\":   [\"<ref-1>\", \"<ref-2>\", ...the unique refs you actually used]\n}\n\nNote: key_points and cited_refs are ARRAYS OF STRINGS — return real JSON arrays, not JSON-encoded strings.";

pub fn PROMPT_OUTPUT_FIELDS() -> Value {
    json!([
        { "field_name": "reasoning_md", "field_type": "string", "required": true },
        { "field_name": "summary_md",   "field_type": "string", "required": true },
        { "field_name": "key_points",   "field_type": "array_of_string", "required": true },
        { "field_name": "confidence",   "field_type": "number", "required": true },
        { "field_name": "cited_refs",   "field_type": "array_of_string", "required": false },
    ])
}

/// `function ensurePrompt(db)` — find-or-create idempotency: `promptFind` first, and only on
/// failure does `promptAdd` run, followed by a re-find. Mirrors the JS try/catch.
pub fn ensurePrompt(db: &Connection) -> Result<Value, String> {
    let find_input = json!({ "name": PROMPT_NAME });
    if let Ok(found) = crate::ai_prompt_find::promptFind(&find_input, db) {
        return Ok(found);
    }
    crate::ai_prompt_add::promptAdd(
        &json!({
            "name": PROMPT_NAME,
            "template_md": PROMPT_TEMPLATE_MD,
            "default_tier": "high",
            "default_temperature": "hot",
            "max_output_tokens": 1800,
            "output_fields": PROMPT_OUTPUT_FIELDS(),
        }),
        db,
    )?;
    crate::ai_prompt_find::promptFind(&find_input, db)
}

/// `function fmtAge(s)` — pure formatting helper. Pure.
pub fn fmtAge(s: Option<i64>) -> String {
    let s = match s { Some(s) => s, None => return "age:?".to_string() };
    if s < 60 { return format!("{s}s ago"); }
    if s < 3600 { return format!("{}m ago", (s as f64 / 60.0).round() as i64); }
    if s < 86400 { return format!("{}h ago", (s as f64 / 3600.0).round() as i64); }
    format!("{}d ago", (s as f64 / 86400.0).round() as i64)
}

/// `function renderFindingsBlock(findings)` — one line per finding: `[ref] (age) source/kind:
/// snippet`. Pure.
pub fn renderFindingsBlock(findings: &Value) -> String {
    let arr = match findings.as_array() { Some(a) => a, None => return String::new() };
    arr.iter()
        .map(|f| {
            let ref_ = f.get("ref").and_then(Value::as_str).unwrap_or("");
            let age = f.get("age_seconds").and_then(Value::as_i64);
            let source = f.get("source").and_then(Value::as_str).unwrap_or("");
            let kind = f.get("kind").and_then(Value::as_str).unwrap_or("");
            let snippet = f.get("snippet").and_then(Value::as_str).unwrap_or("");
            format!("[{ref_}] ({}) {source}/{kind}: {snippet}", fmtAge(age))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// `output_kv` → `key_points`/`cited_refs` array aggregation (grouped by `order_index`) plus scalar
/// `reasoning_md`/`summary_md`/`confidence` collapse. Mirrors the JS inline aggregation loop.
/// Factored out as its own pure fn — testable against a canned AI response since the real `aiCall`
/// leg is PARITY-DEFERRED.
pub fn collapse_output_kv(output_kv: &Value) -> Value {
    let mut scalars: Map<String, Value> = Map::new();
    let mut key_points: Vec<(i64, String)> = Vec::new();
    let mut cited_refs: Vec<(i64, String)> = Vec::new();

    if let Some(rows) = output_kv.as_array() {
        for row in rows {
            let Some(field_name) = row.get("field_name").and_then(Value::as_str) else { continue };
            let is_array_field = field_name == "key_points" || field_name == "cited_refs";
            if is_array_field {
                let idx = row.get("order_index").and_then(Value::as_i64).unwrap_or(0);
                let s = row.get("value_text").and_then(Value::as_str).unwrap_or("").to_string();
                if field_name == "key_points" { key_points.push((idx, s)); } else { cited_refs.push((idx, s)); }
                continue;
            }
            if let Some(v) = row.get("value_text") {
                if !v.is_null() { scalars.insert(field_name.to_string(), v.clone()); continue; }
            }
            if let Some(v) = row.get("value_number") {
                if !v.is_null() { scalars.insert(field_name.to_string(), v.clone()); continue; }
            }
            if let Some(v) = row.get("value_boolean") {
                if !v.is_null() { scalars.insert(field_name.to_string(), json!(v.as_bool().unwrap_or(false))); continue; }
            }
        }
    }
    key_points.sort_by_key(|(i, _)| *i);
    cited_refs.sort_by_key(|(i, _)| *i);

    json!({
        "reasoning_md": scalars.get("reasoning_md").and_then(Value::as_str).unwrap_or("").to_string(),
        "summary_md": scalars.get("summary_md").and_then(Value::as_str).unwrap_or("").to_string(),
        "key_points": key_points.into_iter().map(|(_, s)| json!(s)).collect::<Vec<_>>(),
        "confidence": scalars.get("confidence").and_then(Value::as_f64).unwrap_or(0.0),
        "cited_refs": cited_refs.into_iter().map(|(_, s)| json!(s)).collect::<Vec<_>>(),
    })
}

/// Rough pre-call token/cost estimate: `(findings_block.len() + question.len()) / 4` tokens at
/// $0.02/1k. Pure — shared by the pre-call refusal check.
fn estimate_cost_usd(findings_block: &str, question: &str) -> f64 {
    let approx_chars = (findings_block.chars().count() + question.chars().count()) as f64;
    let approx_tokens = approx_chars / 4.0;
    (approx_tokens / 1000.0) * 0.02
}

/// `export default async function investigateSynthesize(input, context)`. `context.db` →
/// `db: &Connection` (non-optional in this signature); `context.provider_fn` has no Rust equivalent
/// (see module doc) and is dropped.
pub fn investigateSynthesize(input: &Value, db: &Connection) -> Result<Value, String> {
    let question = input.get("question").and_then(Value::as_str).unwrap_or("");
    if question.is_empty() {
        return Err("investigate-synthesize: question is required".to_string());
    }
    let findings = input.get("findings").cloned().unwrap_or(json!([]));
    let findings_arr = findings.as_array().cloned().unwrap_or_default();

    // No-evidence fast path — never call the AI when there's nothing to synthesize from.
    if findings_arr.is_empty() {
        return Ok(json!({
            "reasoning_md": "(no findings to reason over)",
            "answer": {
                "summary_md": "I couldn't find anything in the substrate that bears on this question. The state, memory, audit trail, and recent driver activity all came up empty. Try rephrasing, or file a card to investigate further.",
                "key_points": [],
                "confidence": 0,
            },
            "citations": [],
            "ai_call_id": null,
            "ai_telemetry": null,
            "no_evidence": true,
        }));
    }

    // FEAT-INVESTIGATE-COST-CAP: hard budget gate.
    let budget_usd: f64 = std::env::var("ANTILOKI_INVESTIGATE_BUDGET_USD")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0.05);
    let hard_ceiling_usd = budget_usd * 2.0;
    const RATE_LIMIT_MS: i64 = 60_000;

    if let Ok(mut stmt) = db.prepare(
        "SELECT cost_usd_actual, ts FROM brain.ai_calls WHERE prompt_name = ? ORDER BY id DESC LIMIT 1",
    ) {
        let queried: Option<(Option<f64>, Option<String>)> = stmt
            .query_row([PROMPT_NAME], |r| Ok((r.get(0)?, r.get(1)?)))
            .optional()
            .unwrap_or(None);
        if let Some(row) = queried {
            if let (Some(cost), Some(ts)) = row {
                if cost > hard_ceiling_usd {
                    if let Some(ts_ms) = parse_sqlite_ts_to_epoch_ms(&ts) {
                        let elapsed = now_epoch_ms() - ts_ms;
                        if elapsed < RATE_LIMIT_MS {
                            let wait_s = ((RATE_LIMIT_MS - elapsed) as f64 / 1000.0).ceil() as i64;
                            return Ok(json!({
                                "reasoning_md": "(rate-limited: last investigate exceeded 2× budget)",
                                "answer": {
                                    "summary_md": format!("Investigation refused: the previous run cost ${:.4} (hard ceiling ${:.4}). Wait {wait_s}s, raise ANTILOKI_INVESTIGATE_BUDGET_USD, or scope the question more narrowly.", cost, hard_ceiling_usd),
                                    "key_points": [],
                                    "confidence": 0,
                                },
                                "citations": [],
                                "ai_call_id": null,
                                "ai_telemetry": null,
                                "cost_capped": true,
                            }));
                        }
                    }
                }
            }
        }
        // brain.ai_calls absent on bootstrap, or query error — skip, matches JS catch.
    }

    let findings_block = renderFindingsBlock(&findings);
    let est_cost_usd = estimate_cost_usd(&findings_block, question);
    if est_cost_usd > hard_ceiling_usd {
        return Ok(json!({
            "reasoning_md": format!("(pre-call estimate ${:.4} exceeds hard ceiling ${:.4})", est_cost_usd, hard_ceiling_usd),
            "answer": {
                "summary_md": format!("Investigation refused: pre-call cost estimate ${:.4} exceeds the hard ceiling ${:.4} (ANTILOKI_INVESTIGATE_BUDGET_USD={budget_usd}). Reduce findings (max_findings_per_source / budget_findings) or scope the question.", est_cost_usd, hard_ceiling_usd),
                "key_points": [],
                "confidence": 0,
            },
            "citations": [],
            "ai_call_id": null,
            "ai_telemetry": null,
            "cost_capped": true,
        }));
    }

    // Test-mode stub — deterministic canned synthesis.
    if std::env::var("ANTILOKI_INVESTIGATE_SYNTHESIZE_STUB").as_deref() == Ok("1") {
        let refs: Vec<String> = findings_arr.iter().take(3)
            .map(|f| f.get("ref").and_then(Value::as_str).unwrap_or("").to_string())
            .collect();
        let first_ref = refs.first().cloned().unwrap_or_default();
        return Ok(json!({
            "reasoning_md": "1. (stub) read findings.\n2. (stub) combined.\n3. (stub) concluded.",
            "answer": {
                "summary_md": format!("(stub) Found {} relevant findings. [{}]", findings_arr.len(), first_ref),
                "key_points": refs.iter().map(|r| json!(format!("Key point cites [{r}]"))).collect::<Vec<_>>(),
                "confidence": 0.7,
            },
            "citations": refs,
            "ai_call_id": null,
            "ai_telemetry": {
                "phase": "investigate_synthesize", "prompt_name": PROMPT_NAME,
                "model_id": "stub", "provider": "stub",
                "tier": "high", "temperature": "hot",
                "tokens_in": 0, "tokens_out": 0, "cost_usd": 0, "cached": false, "duration_ms": 0,
            },
        }));
    }

    ensurePrompt(db)?;

    let ai_input = json!({
        "prompt_name": PROMPT_NAME,
        "input_kv": [
            { "key": "question", "value": question },
            { "key": "findings_count", "value": findings_arr.len().to_string() },
            { "key": "findings_block", "value": findings_block },
        ],
        "tier": "high",
        "temperature": "hot",
    });
    // PARITY-DEFERRED boundary: crate::ai_ai_call::aiCall is a network stub that always returns
    // Err — see module doc. On a real provider response this would feed collapse_output_kv plus the
    // ai_call_id-keyed tokens_in/tokens_out lookup below.
    let ai_result = crate::ai_ai_call::aiCall(&ai_input, db)?;

    let collapsed = collapse_output_kv(ai_result.get("output_kv").unwrap_or(&Value::Null));

    let ai_call_id = ai_result.get("ai_call_id").cloned().unwrap_or(Value::Null);
    let (mut tokens_in, mut tokens_out) = (Value::Null, Value::Null);
    if !ai_call_id.is_null() {
        if let Some(id) = ai_call_id.as_i64() {
            if let Ok(mut stmt) = db.prepare("SELECT tokens_in, tokens_out FROM brain.ai_calls WHERE id = ?") {
                let row: Option<(Option<i64>, Option<i64>)> = stmt
                    .query_row([id], |r| Ok((r.get(0)?, r.get(1)?)))
                    .optional()
                    .unwrap_or(None);
                if let Some((ti, to)) = row {
                    tokens_in = ti.map(Value::from).unwrap_or(Value::Null);
                    tokens_out = to.map(Value::from).unwrap_or(Value::Null);
                }
            }
        }
    }

    Ok(json!({
        "reasoning_md": collapsed["reasoning_md"],
        "answer": {
            "summary_md": collapsed["summary_md"],
            "key_points": collapsed["key_points"],
            "confidence": collapsed["confidence"],
        },
        "citations": collapsed["cited_refs"],
        "ai_call_id": ai_call_id,
        "ai_telemetry": {
            "phase": "investigate_synthesize", "prompt_name": PROMPT_NAME,
            "model_id": ai_result.get("model_id").cloned().unwrap_or(Value::Null),
            "provider": ai_result.get("provider").cloned().unwrap_or(Value::Null),
            "tier": ai_result.get("tier_used").and_then(Value::as_str).unwrap_or("high"),
            "temperature": ai_result.get("temperature_used").and_then(Value::as_str).unwrap_or("hot"),
            "tokens_in": tokens_in, "tokens_out": tokens_out,
            "cost_usd": ai_result.get("cost_usd").cloned().unwrap_or(json!(0)),
            "cached": ai_result.get("cached").cloned().unwrap_or(json!(false)),
            "duration_ms": ai_result.get("duration_ms").cloned().unwrap_or(Value::Null),
        },
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_env_lock::ENV_LOCK;

    // Schema copied verbatim from prompt-add.rs's/prompt-find.rs's own test fixtures, plus a minimal
    // brain.ai_calls shape matching the columns this module reads.
    fn open_db() -> Connection {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch("ATTACH DATABASE ':memory:' AS brain").unwrap();
        db.execute_batch(
            "CREATE TABLE brain.prompt_templates ( id INTEGER PRIMARY KEY, name TEXT NOT NULL, version INTEGER NOT NULL, template_md TEXT NOT NULL, default_tier TEXT NOT NULL CHECK (default_tier IN ('low','mid','high')), default_temperature TEXT NOT NULL CHECK (default_temperature IN ('freezed','cold','warm','hot','hell')), max_output_tokens INTEGER NOT NULL DEFAULT 1024, superseded_by INTEGER, created_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')), cross_field_rules_json TEXT, judging_criteria_json TEXT, system_md TEXT, phase_flow TEXT NOT NULL DEFAULT 'single', UNIQUE (name, version) );
             CREATE TABLE brain.prompt_output_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL CHECK (field_type IN ('string','number','boolean','enum','array_of_string','array_of_object')), enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, grounded_refs_json TEXT, UNIQUE (prompt_template_id, field_name) );
             CREATE TABLE brain.prompt_input_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL CHECK (field_type IN ('string','number','boolean','enum','array_of_string','array_of_object')), enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, UNIQUE (prompt_template_id, field_name) );
             CREATE TABLE brain.ai_calls ( id INTEGER PRIMARY KEY, prompt_name TEXT, cost_usd_actual REAL, ts TEXT, tokens_in INTEGER, tokens_out INTEGER );",
        )
        .unwrap();
        db
    }

    fn clear_env() {
        std::env::remove_var("ANTILOKI_INVESTIGATE_BUDGET_USD");
        std::env::remove_var("ANTILOKI_INVESTIGATE_SYNTHESIZE_STUB");
    }

    #[test]
    fn missing_question_errors() {
        let _g = ENV_LOCK.lock().unwrap();
        clear_env();
        let db = open_db();
        let e = investigateSynthesize(&json!({}), &db).unwrap_err();
        assert!(e.contains("question is required"));
        let e2 = investigateSynthesize(&json!({ "question": "" }), &db).unwrap_err();
        assert!(e2.contains("question is required"));
    }

    #[test]
    fn no_findings_returns_no_evidence_fast_path_without_ai_call() {
        let _g = ENV_LOCK.lock().unwrap();
        clear_env();
        let db = open_db();
        let r = investigateSynthesize(&json!({ "question": "why is X failing?" }), &db).unwrap();
        assert_eq!(r["no_evidence"], json!(true));
        assert_eq!(r["ai_call_id"], Value::Null);
        assert_eq!(r["answer"]["confidence"], json!(0));
        assert!(r["answer"]["summary_md"].as_str().unwrap().contains("couldn't find anything"));

        let r2 = investigateSynthesize(&json!({ "question": "q", "findings": [] }), &db).unwrap();
        assert_eq!(r2["no_evidence"], json!(true));
    }

    #[test]
    fn stub_mode_returns_deterministic_canned_synthesis() {
        let _g = ENV_LOCK.lock().unwrap();
        clear_env();
        std::env::set_var("ANTILOKI_INVESTIGATE_SYNTHESIZE_STUB", "1");
        let db = open_db();
        let findings = json!([
            { "ref": "card-1", "source": "cards", "kind": "backlog", "snippet": "s1", "age_seconds": 30 },
            { "ref": "mem-2", "source": "memory", "kind": "decision", "snippet": "s2", "age_seconds": 900 },
        ]);
        let r = investigateSynthesize(&json!({ "question": "q", "findings": findings }), &db).unwrap();
        std::env::remove_var("ANTILOKI_INVESTIGATE_SYNTHESIZE_STUB");
        assert_eq!(r["ai_telemetry"]["model_id"], json!("stub"));
        assert_eq!(r["answer"]["confidence"], json!(0.7));
        assert_eq!(r["citations"], json!(["card-1", "mem-2"]));
        assert!(r["answer"]["summary_md"].as_str().unwrap().contains("Found 2 relevant findings"));
    }

    #[test]
    fn pre_call_cost_estimate_refuses_when_findings_block_too_large() {
        let _g = ENV_LOCK.lock().unwrap();
        clear_env();
        std::env::set_var("ANTILOKI_INVESTIGATE_BUDGET_USD", "0.0001");
        let db = open_db();
        let big_snippet = "x".repeat(50_000);
        let findings = json!([{ "ref": "r1", "source": "s", "kind": "k", "snippet": big_snippet, "age_seconds": 1 }]);
        let r = investigateSynthesize(&json!({ "question": "q", "findings": findings }), &db).unwrap();
        std::env::remove_var("ANTILOKI_INVESTIGATE_BUDGET_USD");
        assert_eq!(r["cost_capped"], json!(true));
        assert_eq!(r["ai_call_id"], Value::Null);
        assert!(r["answer"]["summary_md"].as_str().unwrap().contains("pre-call cost estimate"));
    }

    #[test]
    fn post_call_rate_limit_refuses_within_window_after_prior_overrun() {
        let _g = ENV_LOCK.lock().unwrap();
        clear_env();
        std::env::set_var("ANTILOKI_INVESTIGATE_BUDGET_USD", "0.01");
        let db = open_db();
        let now_ms = now_epoch_ms();
        let secs = now_ms / 1000;
        let (y, m, d, hh, mm, ss) = epoch_secs_to_civil(secs);
        let now = format!("{y:04}-{m:02}-{d:02} {hh:02}:{mm:02}:{ss:02}");
        db.execute(
            "INSERT INTO brain.ai_calls (prompt_name, cost_usd_actual, ts) VALUES (?, ?, ?)",
            rusqlite::params![PROMPT_NAME, 5.0, now],
        ).unwrap();
        let findings = json!([{ "ref": "r1", "source": "s", "kind": "k", "snippet": "small", "age_seconds": 1 }]);
        let r = investigateSynthesize(&json!({ "question": "q", "findings": findings }), &db).unwrap();
        std::env::remove_var("ANTILOKI_INVESTIGATE_BUDGET_USD");
        assert_eq!(r["cost_capped"], json!(true));
        assert!(r["answer"]["summary_md"].as_str().unwrap().contains("previous run cost"));
    }

    #[test]
    fn deferred_ai_call_leg_propagates_the_stub_error() {
        let _g = ENV_LOCK.lock().unwrap();
        clear_env();
        let db = open_db();
        let findings = json!([{ "ref": "r1", "source": "s", "kind": "k", "snippet": "small", "age_seconds": 1 }]);
        let e = investigateSynthesize(&json!({ "question": "q", "findings": findings }), &db).unwrap_err();
        assert!(e.contains("ai-call"), "propagates ai-call.rs's deferred-stub error: {e}");
    }

    #[test]
    fn ensure_prompt_creates_then_finds_idempotently() {
        let _g = ENV_LOCK.lock().unwrap();
        clear_env();
        let db = open_db();
        let first = ensurePrompt(&db).unwrap();
        assert_eq!(first["name"], json!(PROMPT_NAME));
        let second = ensurePrompt(&db).unwrap();
        assert_eq!(second["id"], first["id"], "second call finds, does not re-add");
    }

    // ── fmtAge / renderFindingsBlock (pure formatting) ──────────────────────────────────────
    #[test]
    fn fmt_age_buckets() {
        assert_eq!(fmtAge(None), "age:?");
        assert_eq!(fmtAge(Some(30)), "30s ago");
        assert_eq!(fmtAge(Some(120)), "2m ago");
        assert_eq!(fmtAge(Some(7200)), "2h ago");
        assert_eq!(fmtAge(Some(172800)), "2d ago");
    }

    #[test]
    fn render_findings_block_formats_each_line() {
        let findings = json!([
            { "ref": "a1", "source": "cards", "kind": "backlog", "snippet": "text1", "age_seconds": 30 },
            { "ref": "a2", "source": "memory", "kind": null, "snippet": "text2", "age_seconds": null },
        ]);
        let block = renderFindingsBlock(&findings);
        let lines: Vec<&str> = block.lines().collect();
        assert_eq!(lines[0], "[a1] (30s ago) cards/backlog: text1");
        assert_eq!(lines[1], "[a2] (age:?) memory/: text2");
    }

    // ── collapse_output_kv (pure response-shaping surface) ──────────────────────────────────
    #[test]
    fn collapse_aggregates_array_fields_by_order_index_and_scalars() {
        let output_kv = json!([
            { "field_name": "reasoning_md", "value_text": "1. step one" },
            { "field_name": "summary_md", "value_text": "the answer" },
            { "field_name": "confidence", "value_number": 0.85 },
            { "field_name": "key_points", "value_text": "second point", "order_index": 1 },
            { "field_name": "key_points", "value_text": "first point", "order_index": 0 },
            { "field_name": "cited_refs", "value_text": "ref-2", "order_index": 1 },
            { "field_name": "cited_refs", "value_text": "ref-1", "order_index": 0 },
        ]);
        let out = collapse_output_kv(&output_kv);
        assert_eq!(out["reasoning_md"], json!("1. step one"));
        assert_eq!(out["summary_md"], json!("the answer"));
        assert_eq!(out["confidence"], json!(0.85));
        assert_eq!(out["key_points"], json!(["first point", "second point"]));
        assert_eq!(out["cited_refs"], json!(["ref-1", "ref-2"]));
    }

    #[test]
    fn collapse_defaults_when_output_kv_missing_or_empty() {
        let out = collapse_output_kv(&Value::Null);
        assert_eq!(out["reasoning_md"], json!(""));
        assert_eq!(out["summary_md"], json!(""));
        assert_eq!(out["key_points"], json!([]));
        assert_eq!(out["cited_refs"], json!([]));
        assert_eq!(out["confidence"], json!(0.0));

        let out2 = collapse_output_kv(&json!([]));
        assert_eq!(out2["key_points"], json!([]));
    }
}
