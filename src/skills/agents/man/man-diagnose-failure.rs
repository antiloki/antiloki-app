//! man-diagnose-failure.rs — twin of skills/agents/man/man-diagnose-failure.mjs (db WRITE lane,
//! brain-attached `driver_diagnoses` + unqualified `inbox_messages`). NAMES ARE CONTRACT:
//! `manDiagnoseFailure` (default), `interpretPending` (named export).
//!
//! P1 (deterministic, no AI, fully ported): on a FAILURE outcome tick (`failed`/`escalated`/
//! `blocked`), gathers the forensic snapshot via the already-converted
//! `driver_driver_inspect::driverInspect` twin, computes a failure SIGNATURE
//! (`decided_action :: error_class`, see `error_class` below — a literal port of the JS
//! `reduceRight` "rightmost non-null error_code, else a normalized+capped error_message slice"
//! logic), and records ONE deduped row per signature in `brain.driver_diagnoses` —
//! incrementing `recurrences` + refreshing `tick_id`/`card_slug`/`gather_json` when the same
//! signature fires again (the AI is deliberately NOT re-run on a recurrence).
//!
//! P2 (LOW AI, PARITY-DEFERRED): `interpret()` below faithfully ports every deterministic step —
//! `ensureDiagnosePrompt` (idempotent prompt registration via the already-converted
//! `ai_prompt_find::promptFind` / `ai_prompt_add::promptAdd` twins), transcript assembly
//! (`gather.render` joined + capped at `TRANSCRIPT_CAP`, with the JS one-line fallback when
//! `render` is empty), and response-parsing (`pick_t`/`pick_n`, classification-enum validation,
//! confidence clamping, the "nothing usable → stay P1-only" degrade rule) — but the actual AI
//! dispatch calls `crate::ai_ai_call::aiCall`, which is itself PARITY-DEFERRED (see its own module
//! doc: it ALWAYS returns `Err`, a real network call being non-deterministic and unfixturable in
//! principle). Net effect: `interpret()` in this twin always registers the prompt (real,
//! observable DB side effect) and then always degrades to P1-only (`None`) — exactly the JS
//! best-effort `catch { return null }` branch for "AI unavailable", just permanently so. The
//! `provider_fn` JS uses to stub the network call in ITS tests has no Rust equivalent (the
//! deferred `aiCall` stub takes no such parameter), so the classified-happy-path branch of P2
//! cannot be exercised here — that is the deferred boundary, not a gap in this port.
//!
//! P5b (`maybeProposeDrift`, fully ported): on a `substrate-gap` classification, raises ONE
//! deduped `inbox_messages` proposal (kind `substrate-gap-diagnosed`) — best-effort, non-blocking,
//! never creates a card. Ported as a private fn exercised directly by native tests (its trigger
//! condition — a real `substrate-gap` classification — is unreachable through the public API
//! here because `interpret()` never returns one, per the deferred boundary above).
//!
//! `context.db` missing → JS throws `err.contextMissing('man-diagnose-failure', 'db')`.
//! Unreachable here: `db: &Connection` is a required, non-nullable reference (same convention as
//! every sibling db-lane twin).
//!
//! Throw fidelity: missing `tick_id` → `"man-diagnose-failure: tick_id is required"`; unknown
//! `tick_id` → `"man-diagnose-failure: driver_tick '<id>' not found"` (both verified against
//! `err.required`/`err.notFound`'s message-join format in engine/error.mjs).
// QUALITY: mixed unit — P1 dedup/signature + P5b drift-proposal fully ported and native-tested;
// P2's deterministic surface (prompt registration, transcript assembly, response parsing) ported
// but its network leg is the same class of deferred boundary as ai-call.rs's own `aiCall`.

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
fn str_qq(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(),
        Some(o) => o.to_string(),
    }
}
fn tmpl(v: Option<&Value>) -> String {
    match v {
        None => "undefined".into(),
        Some(Value::Null) => "null".into(),
        Some(x) => str_qq(Some(x)),
    }
}
// = JS `.length` (UTF-16 code units) / `.slice(0,n)` — chars-based approximation, exact for the
// ASCII content this unit ever renders (same convention as driver-inspect.rs's js_len/js_slice).
fn js_len(s: &str) -> usize { s.encode_utf16().count() }
fn js_slice(s: &str, n: usize) -> String { s.chars().take(n).collect() }

const FAILURE_OUTCOMES: [&str; 3] = ["failed", "escalated", "blocked"];
const GATHER_CAP: usize = 64 * 1024;
const TRANSCRIPT_CAP: usize = 12 * 1024;

const DPROMPT: &str = "man.diagnose.failure.v1";
const DSYSTEM: &str = "You are MAN, the failure-diagnosis auditor for an autonomous coding driver. Given a forensic readout of ONE failed driver tick — the decision it made, the FACTS it saw, the error, and the events/AI calls in its time window — produce a SHORT, concrete root-cause diagnosis. Ground every claim STRICTLY in the readout; never invent state you cannot see. Classify the failure as exactly one of: card-bug (the card/contract/specs are wrong), substrate-gap (the driver/engine itself is at fault — a missing capability or a harness bug), config (environment/setup — missing build command, deps, keys, paths), transient (a flake/timeout/race likely to pass on retry).";
const DTEMPLATE: &str = "Diagnose this FAILED driver tick from the forensic readout below.\n\n{{transcript}}\n\nProduce:\n- classification: EXACTLY one of  card-bug | substrate-gap | config | transient\n- root_cause_md: 1–3 sentences — WHY it failed, citing the specific fact / error_code / event above.\n- suggested_fix_md: 1–2 sentences — the concrete next action that would fix or unblock it.\n- confidence: a number 0.0–1.0 — how strongly the readout supports this diagnosis.";
const VALID_CLASS: [&str; 4] = ["card-bug", "substrate-gap", "config", "transient"];

fn doutput_fields() -> Value {
    json!([
        { "field_name": "classification", "field_type": "string" },
        { "field_name": "root_cause_md", "field_type": "string" },
        { "field_name": "suggested_fix_md", "field_type": "string" },
        { "field_name": "confidence", "field_type": "number" },
    ])
}

/// `function errorClass(tick, gather)` — the deepest (rightmost, per the JS `reduceRight` "sticky
/// first non-null wins" quirk) `error_code` in `gather.events`, else a normalized+capped slice of
/// `tick.error_message`, else `'no-error'`.
fn error_class(tick: &Value, gather: &Value) -> String {
    let code = gather
        .get("events")
        .and_then(Value::as_array)
        .and_then(|evs| evs.iter().rev().find_map(|e| e.get("error_code").filter(|v| !v.is_null())));
    if let Some(c) = code {
        return str_qq(Some(c));
    }
    let msg = str_qq(tick.get("error_message"));
    let collapsed = msg.split_whitespace().collect::<Vec<&str>>().join(" ");
    if collapsed.is_empty() { "no-error".to_string() } else { js_slice(&collapsed, 60) }
}

/// `function ensureDiagnosePrompt(db)` — idempotent: no-op when an active row already has the
/// exact current template_md; else registers/bumps via `promptAdd`.
fn ensure_diagnose_prompt(db: &Connection) {
    let existing = crate::ai_prompt_find::promptFind(&json!({ "name": DPROMPT }), db).ok();
    if let Some(row) = &existing {
        if row.get("template_md").and_then(Value::as_str) == Some(DTEMPLATE) {
            return;
        }
    }
    let _ = crate::ai_prompt_add::promptAdd(
        &json!({
            "name": DPROMPT,
            "template_md": DTEMPLATE,
            "system_md": DSYSTEM,
            "default_tier": "low",
            "default_temperature": "cold",
            "phase_flow": "single_only",
            "max_output_tokens": 800,
            "output_fields": doutput_fields(),
        }),
        db,
    );
}

fn pick_t(res: &Value, f: &str) -> Option<String> {
    res.get("output_kv")?.as_array()?.iter()
        .find(|o| o.get("field_name").and_then(Value::as_str) == Some(f) && o.get("parent_output_id").map_or(true, Value::is_null))
        .and_then(|o| o.get("value_text")).and_then(Value::as_str).map(|s| s.to_string())
}
fn pick_n(res: &Value, f: &str) -> Option<f64> {
    res.get("output_kv")?.as_array()?.iter()
        .find(|o| o.get("field_name").and_then(Value::as_str) == Some(f) && o.get("parent_output_id").map_or(true, Value::is_null))
        .and_then(|o| o.get("value_number")).and_then(Value::as_f64)
}

/// `async function interpret(db, gather, tick, provider_fn)` — see module doc: always registers
/// the prompt (real side effect), always degrades to `None` here because `ai_ai_call::aiCall` is
/// itself an always-`Err` deferred stub (no `provider_fn` injection point exists in this twin).
fn interpret(db: &Connection, gather: &Value, tick: &Value) -> Option<Value> {
    ensure_diagnose_prompt(db);
    let joined = gather
        .get("render")
        .and_then(Value::as_array)
        .map(|a| a.iter().map(|v| str_qq(Some(v))).collect::<Vec<_>>().join("\n"))
        .unwrap_or_default();
    let capped = js_slice(&joined, TRANSCRIPT_CAP);
    let transcript = if capped.is_empty() {
        format!(
            "tick {} ({}) → {}: {}",
            tmpl(tick.get("id")),
            tick.get("decided_action").and_then(Value::as_str).unwrap_or("?"),
            tmpl(tick.get("outcome")),
            tick.get("error_message").and_then(Value::as_str).unwrap_or("(no message)"),
        )
    } else {
        capped
    };
    let call_input = json!({
        "prompt_name": DPROMPT,
        "input_kv": [{ "key": "transcript", "value": transcript }],
        "tier": "low", "temperature": "cold", "phase_flow": "single_only",
    });
    match crate::ai_ai_call::aiCall(&call_input, db) {
        Err(_) => None,
        Ok(res) => {
            let mut classification = pick_t(&res, "classification").unwrap_or_default().trim().to_lowercase();
            if !VALID_CLASS.contains(&classification.as_str()) { classification = String::new(); }
            let root_cause_md = pick_t(&res, "root_cause_md");
            let suggested_fix_md = pick_t(&res, "suggested_fix_md");
            let confidence = pick_n(&res, "confidence").map(|c| c.max(0.0).min(1.0));
            if classification.is_empty() && root_cause_md.is_none() && suggested_fix_md.is_none() {
                return None;
            }
            Some(json!({
                "classification": if classification.is_empty() { Value::Null } else { json!(classification) },
                "root_cause_md": root_cause_md,
                "suggested_fix_md": suggested_fix_md,
                "confidence": confidence,
                "model_id": res.get("model_id").cloned().unwrap_or(Value::Null),
            }))
        }
    }
}

/// `function maybeProposeDrift(db, row)` — best-effort; failures inside are swallowed (JS
/// `try {...} catch {}`), mirrored here via `.ok()`/`let _ =` on every fallible step.
fn maybe_propose_drift(db: &Connection, row: &Value) {
    if row.get("classification").and_then(Value::as_str) != Some("substrate-gap") { return; }
    let signature = str_qq(row.get("signature"));
    let subject = format!("Substrate-gap diagnosed: {}", js_slice(&signature, 120));
    let dup = crate::db_json::query_rows(
        db,
        "SELECT 1 FROM inbox_messages WHERE kind = 'substrate-gap-diagnosed' AND subject = ? LIMIT 1",
        &[json!(subject)],
    );
    if let Ok(rows) = &dup {
        if rows.as_array().map_or(false, |a| !a.is_empty()) { return; }
    }
    let recurrences = row.get("recurrences").and_then(Value::as_i64).unwrap_or(1);
    let mut body = format!(
        "## MAN diagnosed a substrate-gap\nA driver failure was classified **substrate-gap** — the substrate/engine is at fault, not the card.\n\n**Signature:** `{signature}`  (seen {recurrences}×)\n\n"
    );
    if truthy(row.get("root_cause_md")) {
        body.push_str(&format!("## Root cause\n{}\n\n", str_qq(row.get("root_cause_md"))));
    }
    if truthy(row.get("suggested_fix_md")) {
        body.push_str(&format!("## Suggested fix\n{}\n\n", str_qq(row.get("suggested_fix_md"))));
    }
    body.push_str("## What to do\nIf this is a real substrate defect, open a drift card to fix it. MAN raised this proposal automatically from its failure diagnosis — **no card was created**.");
    let card_slug = row.get("card_slug").cloned().unwrap_or(Value::Null);
    let payload = json!({
        "signature": signature,
        "diagnosis_id": row.get("id").cloned().unwrap_or(Value::Null),
        "classification": row.get("classification").cloned().unwrap_or(Value::Null),
        "confidence": row.get("confidence").cloned().unwrap_or(Value::Null),
    });
    let payload_str = serde_json::to_string(&payload).unwrap_or_default();
    let _ = crate::db_json::run_write(
        db,
        "INSERT INTO inbox_messages (kind, subject, body_md, severity, related_card_slug, suggested_card_action, payload_json) VALUES ('substrate-gap-diagnosed', ?, ?, 'warn', ?, 'create-drift', ?)",
        &[json!(subject), json!(body), card_slug, json!(payload_str)],
    );
}

/// `export async function interpretPending(db, sel, provider_fn = null)` — JIT re-interpretation
/// off the hot path. Cached: an already-classified row returns as-is (`interpret` never called).
#[allow(non_snake_case)]
pub fn interpretPending(db: &Connection, sel: &Value) -> Result<Value, String> {
    let row = if let Some(did) = sel.get("diagnosis_id").filter(|v| !v.is_null()) {
        crate::db_json::query_rows(db, "SELECT * FROM brain.driver_diagnoses WHERE id = ?", &[did.clone()])?
            .as_array().and_then(|a| a.first()).cloned()
    } else if let Some(cs) = sel.get("card_slug").filter(|v| !v.is_null()) {
        crate::db_json::query_rows(db, "SELECT * FROM brain.driver_diagnoses WHERE card_slug = ? ORDER BY id DESC LIMIT 1", &[cs.clone()])?
            .as_array().and_then(|a| a.first()).cloned()
    } else {
        None
    };
    let row = match row {
        Some(r) => r,
        None => return Ok(Value::Null),
    };
    if truthy(row.get("classification")) {
        return Ok(row); // already interpreted → cached
    }
    let gather: Value = row.get("gather_json").and_then(Value::as_str)
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or(Value::Null);
    let tick_id = row.get("tick_id").cloned().unwrap_or(Value::Null);
    let tick_rows = crate::db_json::query_rows(db, "SELECT id, decided_action, outcome, error_message FROM brain.driver_ticks WHERE id = ?", &[tick_id.clone()])?;
    let tick = tick_rows.as_array().and_then(|a| a.first()).cloned()
        .unwrap_or_else(|| json!({ "id": tick_id, "decided_action": Value::Null, "outcome": "failed", "error_message": Value::Null }));

    let diag = interpret(db, &gather, &tick);
    if let Some(d) = diag {
        crate::db_json::run_write(
            db,
            "UPDATE brain.driver_diagnoses SET classification=?, root_cause_md=?, suggested_fix_md=?, confidence=?, model_id=?, updated_at=datetime('now') WHERE id=?",
            &[
                d.get("classification").cloned().unwrap_or(Value::Null),
                d.get("root_cause_md").cloned().unwrap_or(Value::Null),
                d.get("suggested_fix_md").cloned().unwrap_or(Value::Null),
                d.get("confidence").cloned().unwrap_or(Value::Null),
                d.get("model_id").cloned().unwrap_or(Value::Null),
                row.get("id").cloned().unwrap_or(Value::Null),
            ],
        )?;
        let updated = crate::db_json::query_rows(db, "SELECT * FROM brain.driver_diagnoses WHERE id = ?", &[row.get("id").cloned().unwrap_or(Value::Null)])?
            .as_array().and_then(|a| a.first()).cloned().unwrap_or(Value::Null);
        maybe_propose_drift(db, &updated); // P5b
        Ok(updated)
    } else {
        Ok(row) // still P1-only (AI unavailable)
    }
}

#[allow(non_snake_case)]
pub fn manDiagnoseFailure(input: &Value, db: &Connection) -> Result<Value, String> {
    let tick_id = input.get("tick_id").cloned().unwrap_or(Value::Null);
    let gather_only = input.get("gather_only").and_then(Value::as_bool).unwrap_or(false);
    if tick_id.is_null() {
        return Err("man-diagnose-failure: tick_id is required".to_string());
    }

    let tick_rows = crate::db_json::query_rows(db, "SELECT * FROM brain.driver_ticks WHERE id = ?", &[tick_id.clone()])?;
    let tick = match tick_rows.as_array().and_then(|a| a.first()) {
        Some(t) => t.clone(),
        None => return Err(format!("man-diagnose-failure: driver_tick '{}' not found", tmpl(Some(&tick_id)))),
    };
    let outcome = tick.get("outcome").and_then(Value::as_str).unwrap_or("");
    if !FAILURE_OUTCOMES.contains(&outcome) {
        return Ok(json!({
            "skipped": true,
            "reason": format!("tick {} outcome='{}' is not a failure", tmpl(Some(&tick_id)), outcome),
            "tick_id": tick_id,
        }));
    }

    // Gather the forensic snapshot (the existing read; pure, never mutates).
    let gather = crate::driver_driver_inspect::driverInspect(&json!({ "tick_id": tick_id }), db).unwrap_or(Value::Null);
    let signature = format!(
        "{} :: {}",
        tick.get("decided_action").and_then(Value::as_str).unwrap_or("unknown"),
        error_class(&tick, &gather)
    );
    let mut gather_json = serde_json::to_string(&gather).ok();
    if let Some(gj) = &gather_json {
        if js_len(gj) > GATHER_CAP { gather_json = Some(js_slice(gj, GATHER_CAP)); }
    }
    let card_slug = tick.get("card_slug").cloned().unwrap_or(Value::Null);

    let existing = crate::db_json::query_rows(db, "SELECT id, recurrences, classification FROM brain.driver_diagnoses WHERE signature = ?", &[json!(signature)])?
        .as_array().and_then(|a| a.first()).cloned();
    if let Some(e) = existing {
        let id = e.get("id").cloned().unwrap_or(Value::Null);
        crate::db_json::run_write(
            db,
            "UPDATE brain.driver_diagnoses SET recurrences = recurrences + 1, tick_id = ?, card_slug = ?, gather_json = ?, updated_at = datetime('now') WHERE id = ?",
            &[tick_id.clone(), card_slug.clone(), gather_json.clone().map(|s| json!(s)).unwrap_or(Value::Null), id.clone()],
        )?;
        let recurrences = e.get("recurrences").and_then(Value::as_i64).unwrap_or(0) + 1;
        return Ok(json!({
            "diagnosis_id": id,
            "signature": signature,
            "recurrences": recurrences,
            "reused": true,
            "classification": e.get("classification").cloned().unwrap_or(Value::Null),
        }));
    }

    let (_c, diagnosis_id) = crate::db_json::run_write(
        db,
        "INSERT INTO brain.driver_diagnoses (signature, tick_id, card_slug, gather_json) VALUES (?,?,?,?)",
        &[json!(signature), tick_id.clone(), card_slug.clone(), gather_json.map(|s| json!(s)).unwrap_or(Value::Null)],
    )?;

    // P2: MAN interprets the gather (LOW AI) — once per signature, here on first sighting.
    // Skipped under gather_only (the Zero-LLM hot path) → the row stays P1-only.
    let diag = if gather_only { None } else { interpret(db, &gather, &tick) };
    if let Some(d) = &diag {
        crate::db_json::run_write(
            db,
            "UPDATE brain.driver_diagnoses SET classification = ?, root_cause_md = ?, suggested_fix_md = ?, confidence = ?, model_id = ?, updated_at = datetime('now') WHERE id = ?",
            &[
                d.get("classification").cloned().unwrap_or(Value::Null),
                d.get("root_cause_md").cloned().unwrap_or(Value::Null),
                d.get("suggested_fix_md").cloned().unwrap_or(Value::Null),
                d.get("confidence").cloned().unwrap_or(Value::Null),
                d.get("model_id").cloned().unwrap_or(Value::Null),
                json!(diagnosis_id),
            ],
        )?;
        maybe_propose_drift(db, &json!({
            "id": diagnosis_id, "signature": signature, "card_slug": card_slug, "recurrences": 1,
            "classification": d.get("classification").cloned().unwrap_or(Value::Null),
            "root_cause_md": d.get("root_cause_md").cloned().unwrap_or(Value::Null),
            "suggested_fix_md": d.get("suggested_fix_md").cloned().unwrap_or(Value::Null),
            "confidence": d.get("confidence").cloned().unwrap_or(Value::Null),
        }));
    }
    Ok(json!({
        "diagnosis_id": diagnosis_id,
        "signature": signature,
        "recurrences": 1,
        "reused": false,
        "classification": diag.as_ref().and_then(|d| d.get("classification")).cloned().unwrap_or(Value::Null),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS brain").unwrap();
        c.execute_batch(
            "CREATE TABLE brain.driver_ticks (id INTEGER PRIMARY KEY, started_at TEXT, ended_at TEXT, \
             card_slug TEXT, decided_agent TEXT, decided_action TEXT, decided_reason TEXT, outcome TEXT, \
             duration_ms INTEGER, error_message TEXT, cost_usd REAL, facts_json TEXT);
             CREATE TABLE runtime_events (id INTEGER PRIMARY KEY, ts TEXT, actor TEXT, action TEXT, skill TEXT, \
             outcome TEXT, error_code TEXT, duration_ms REAL, context_json TEXT, parent_event_id INTEGER);
             CREATE TABLE sacred_timeline (id INTEGER PRIMARY KEY, ts TEXT, actor TEXT, operation TEXT, entity_type TEXT, entity_id TEXT);
             CREATE TABLE contracts (id INTEGER PRIMARY KEY AUTOINCREMENT, slug TEXT UNIQUE, title TEXT, status TEXT, tier TEXT, signed_by TEXT, \
             signed_at TEXT, sandbox_path TEXT, created_at TEXT, updated_at TEXT, current_attempt INTEGER, \
             files_touched_json TEXT, briefing_md TEXT, acceptance_json TEXT, goal_md TEXT, card_slug TEXT, superseded_by TEXT);
             CREATE TABLE qa_runs (id INTEGER PRIMARY KEY, contract_slug TEXT, all_passed INTEGER, scope_passed INTEGER, \
             acceptance_pass INTEGER, acceptance_fail INTEGER, ts TEXT);
             CREATE TABLE brain.ai_calls (id INTEGER PRIMARY KEY, ts TEXT, prompt_template_id INTEGER, tier TEXT, \
             temperature TEXT, provider TEXT, model_id TEXT, outcome TEXT, tokens_in INTEGER, tokens_out INTEGER, \
             duration_ms REAL, cost_usd_actual REAL, cached INTEGER, parent_event_id INTEGER);
             CREATE TABLE brain.ai_call_outputs (id INTEGER PRIMARY KEY, ai_call_id INTEGER, field_name TEXT, \
             value_text TEXT, value_number REAL, value_boolean INTEGER, order_index INTEGER, parent_output_id INTEGER);
             CREATE TABLE brain.prompt_templates ( id INTEGER PRIMARY KEY, name TEXT NOT NULL, version INTEGER NOT NULL, template_md TEXT NOT NULL, default_tier TEXT NOT NULL CHECK (default_tier IN ('low','mid','high')), default_temperature TEXT NOT NULL CHECK (default_temperature IN ('freezed','cold','warm','hot','hell')), max_output_tokens INTEGER NOT NULL DEFAULT 1024, superseded_by INTEGER, created_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')), cross_field_rules_json TEXT, judging_criteria_json TEXT, system_md TEXT, phase_flow TEXT NOT NULL DEFAULT 'single', UNIQUE (name, version) );
             CREATE TABLE brain.prompt_output_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL CHECK (field_type IN ('string','number','boolean','enum','array_of_string','array_of_object')), enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, grounded_refs_json TEXT, UNIQUE (prompt_template_id, field_name) );
             CREATE TABLE brain.prompt_input_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL CHECK (field_type IN ('string','number','boolean','enum','array_of_string','array_of_object')), enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, UNIQUE (prompt_template_id, field_name) );
             CREATE TABLE brain.driver_diagnoses ( id INTEGER PRIMARY KEY AUTOINCREMENT, signature TEXT NOT NULL UNIQUE, tick_id INTEGER, card_slug TEXT, classification TEXT, root_cause_md TEXT, suggested_fix_md TEXT, confidence REAL, gather_json TEXT, recurrences INTEGER NOT NULL DEFAULT 1, model_id TEXT, created_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')), updated_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')) );
             CREATE TABLE inbox_messages ( id INTEGER PRIMARY KEY AUTOINCREMENT, ts TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')), kind TEXT, subject TEXT, body_md TEXT, severity TEXT, related_card_slug TEXT, related_contract_slug TEXT, suggested_reconciler TEXT, suggested_card_action TEXT, payload_json TEXT );"
        ).unwrap();
        c
    }

    fn seed_tick(c: &Connection, card_slug: &str, action: &str, outcome: &str, error_message: Option<&str>) -> i64 {
        c.execute(
            "INSERT INTO brain.driver_ticks (started_at, ended_at, card_slug, decided_action, outcome, error_message) VALUES ('2026-01-01 10:00:00','2026-01-01 10:00:05', ?, ?, ?, ?)",
            rusqlite::params![card_slug, action, outcome, error_message],
        ).unwrap();
        c.last_insert_rowid()
    }

    #[test]
    fn missing_tick_id_returns_exact_message() {
        let c = db();
        let e = manDiagnoseFailure(&json!({}), &c).unwrap_err();
        assert_eq!(e, "man-diagnose-failure: tick_id is required");
    }

    #[test]
    fn unknown_tick_id_returns_exact_not_found_message() {
        let c = db();
        let e = manDiagnoseFailure(&json!({ "tick_id": 999 }), &c).unwrap_err();
        assert_eq!(e, "man-diagnose-failure: driver_tick '999' not found");
    }

    #[test]
    fn non_failure_outcome_is_skipped_and_records_nothing() {
        let c = db();
        let tick_id = seed_tick(&c, "CARD-X", "blueprint.apply", "fired", None);
        let r = manDiagnoseFailure(&json!({ "tick_id": tick_id }), &c).unwrap();
        assert_eq!(r["skipped"], json!(true));
        assert!(r["reason"].as_str().unwrap().contains("outcome='fired'"));
        let n: i64 = c.query_row("SELECT COUNT(*) FROM brain.driver_diagnoses", [], |row| row.get(0)).unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn p1_failure_tick_creates_a_deduped_row_with_signature_and_gather() {
        let c = db();
        let tick_id = seed_tick(&c, "CARD-X", "blueprint.apply", "failed", Some("boom: ENOENT"));
        let r = manDiagnoseFailure(&json!({ "tick_id": tick_id }), &c).unwrap();
        assert_eq!(r["reused"], json!(false));
        assert_eq!(r["recurrences"], json!(1));
        assert!(r["signature"].as_str().unwrap().starts_with("blueprint.apply :: "));
        // aiCall is a deferred stub (always Err) → this twin always degrades to P1-only.
        assert_eq!(r["classification"], Value::Null);
        let gather_json: String = c.query_row("SELECT gather_json FROM brain.driver_diagnoses WHERE id = ?", [r["diagnosis_id"].as_i64().unwrap()], |row| row.get(0)).unwrap();
        assert!(gather_json.contains("Tick"));
    }

    #[test]
    fn recurrence_bumps_count_and_does_not_create_a_second_row() {
        let c = db();
        let t1 = seed_tick(&c, "CARD-X", "blueprint.apply", "failed", Some("boom: ENOENT"));
        let r1 = manDiagnoseFailure(&json!({ "tick_id": t1 }), &c).unwrap();
        let t2 = seed_tick(&c, "CARD-X", "blueprint.apply", "failed", Some("boom: ENOENT"));
        let r2 = manDiagnoseFailure(&json!({ "tick_id": t2 }), &c).unwrap();
        assert_eq!(r2["reused"], json!(true));
        assert_eq!(r2["recurrences"], json!(2));
        assert_eq!(r2["diagnosis_id"], r1["diagnosis_id"]);
        let n: i64 = c.query_row("SELECT COUNT(*) FROM brain.driver_diagnoses", [], |row| row.get(0)).unwrap();
        assert_eq!(n, 1);
        let tick_id: i64 = c.query_row("SELECT tick_id FROM brain.driver_diagnoses WHERE id = ?", [r1["diagnosis_id"].as_i64().unwrap()], |row| row.get(0)).unwrap();
        assert_eq!(tick_id, t2); // refreshed to latest occurrence
    }

    #[test]
    fn different_error_message_produces_a_distinct_signature_and_a_new_row() {
        let c = db();
        let t1 = seed_tick(&c, "CARD-X", "blueprint.apply", "failed", Some("boom: ENOENT"));
        manDiagnoseFailure(&json!({ "tick_id": t1 }), &c).unwrap();
        let t2 = seed_tick(&c, "CARD-X", "blueprint.apply", "failed", Some("totally different failure"));
        manDiagnoseFailure(&json!({ "tick_id": t2 }), &c).unwrap();
        let n: i64 = c.query_row("SELECT COUNT(*) FROM brain.driver_diagnoses", [], |row| row.get(0)).unwrap();
        assert_eq!(n, 2);
    }

    #[test]
    fn gather_only_skips_interpret_entirely_no_prompt_registered() {
        let c = db();
        let tick_id = seed_tick(&c, "CARD-X", "blueprint.apply", "failed", Some("boom"));
        let r = manDiagnoseFailure(&json!({ "tick_id": tick_id, "gather_only": true }), &c).unwrap();
        assert_eq!(r["classification"], Value::Null);
        let n: i64 = c.query_row("SELECT COUNT(*) FROM brain.prompt_templates", [], |row| row.get(0)).unwrap();
        assert_eq!(n, 0, "gather_only must never call interpret() → no prompt registration side effect");
    }

    #[test]
    fn non_gather_only_still_registers_the_prompt_even_though_ai_is_deferred() {
        let c = db();
        let tick_id = seed_tick(&c, "CARD-X", "blueprint.apply", "failed", Some("boom"));
        manDiagnoseFailure(&json!({ "tick_id": tick_id }), &c).unwrap();
        let name: String = c.query_row("SELECT name FROM brain.prompt_templates WHERE id = 1", [], |row| row.get(0)).unwrap();
        assert_eq!(name, DPROMPT);
    }

    #[test]
    fn error_class_normalizes_whitespace_and_caps_at_60_chars() {
        let c = db();
        let long_msg = format!("boom:   {}", "x".repeat(100)); // collapses whitespace, then caps to 60
        let tick_id = seed_tick(&c, "CARD-X", "blueprint.apply", "failed", Some(&long_msg));
        let r = manDiagnoseFailure(&json!({ "tick_id": tick_id }), &c).unwrap();
        let sig = r["signature"].as_str().unwrap();
        let (_, tail) = sig.split_once(" :: ").unwrap();
        assert_eq!(tail.len(), 60);
        assert!(!tail.contains("   ")); // collapsed
    }

    #[test]
    fn interpret_pending_returns_null_when_no_diagnosis_exists() {
        let c = db();
        let r = interpretPending(&c, &json!({ "card_slug": "NOPE" })).unwrap();
        assert_eq!(r, Value::Null);
    }

    #[test]
    fn interpret_pending_reinterprets_a_pending_row_and_stays_p1_only() {
        let c = db();
        let tick_id = seed_tick(&c, "CARD-X", "blueprint.apply", "failed", Some("boom"));
        let rec = manDiagnoseFailure(&json!({ "tick_id": tick_id, "gather_only": true }), &c).unwrap();
        assert_eq!(rec["classification"], Value::Null);
        let d = interpretPending(&c, &json!({ "card_slug": "CARD-X" })).unwrap();
        assert_eq!(d["classification"], Value::Null); // deferred aiCall → still P1-only
        assert_eq!(d["id"], rec["diagnosis_id"]);
        // interpret() DOES run this time (gather_only was NOT set on interpretPending) → prompt registered
        let n: i64 = c.query_row("SELECT COUNT(*) FROM brain.prompt_templates", [], |row| row.get(0)).unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn interpret_pending_is_cached_for_an_already_classified_row() {
        let c = db();
        let tick_id = seed_tick(&c, "CARD-X", "blueprint.apply", "failed", Some("boom"));
        let rec = manDiagnoseFailure(&json!({ "tick_id": tick_id, "gather_only": true }), &c).unwrap();
        c.execute("UPDATE brain.driver_diagnoses SET classification = 'card-bug' WHERE id = ?", [rec["diagnosis_id"].as_i64().unwrap()]).unwrap();
        let d = interpretPending(&c, &json!({ "card_slug": "CARD-X" })).unwrap();
        assert_eq!(d["classification"], json!("card-bug"));
        // cached path never calls interpret() → no prompt registration
        let n: i64 = c.query_row("SELECT COUNT(*) FROM brain.prompt_templates", [], |row| row.get(0)).unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn maybe_propose_drift_raises_one_deduped_inbox_message_on_substrate_gap() {
        let c = db();
        let row = json!({ "id": 1, "signature": "blueprint.apply :: boom", "card_slug": "CARD-X", "recurrences": 3, "classification": "substrate-gap", "root_cause_md": "the gate had no build_cmd", "suggested_fix_md": "set contract.build_cmd", "confidence": 0.7 });
        maybe_propose_drift(&c, &row);
        let n: i64 = c.query_row("SELECT COUNT(*) FROM inbox_messages WHERE kind='substrate-gap-diagnosed'", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1);
        let (subject, body, action): (String, String, String) = c.query_row(
            "SELECT subject, body_md, suggested_card_action FROM inbox_messages WHERE kind='substrate-gap-diagnosed'",
            [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        ).unwrap();
        assert!(subject.contains("Substrate-gap diagnosed"));
        assert!(body.contains("Suggested fix"));
        assert_eq!(action, "create-drift");
        // same signature/subject again → still ONE proposal (deduped)
        maybe_propose_drift(&c, &row);
        let n2: i64 = c.query_row("SELECT COUNT(*) FROM inbox_messages WHERE kind='substrate-gap-diagnosed'", [], |r| r.get(0)).unwrap();
        assert_eq!(n2, 1);
    }

    #[test]
    fn maybe_propose_drift_raises_nothing_for_non_substrate_gap() {
        let c = db();
        let row = json!({ "id": 1, "signature": "x :: y", "classification": "card-bug" });
        maybe_propose_drift(&c, &row);
        let n: i64 = c.query_row("SELECT COUNT(*) FROM inbox_messages", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 0);
    }
}
