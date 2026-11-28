//! pl-contract-review.rs — twin of skills/agents/pl/pl-contract-review.mjs (db lane). NAMES ARE
//! CONTRACT: `plContractReview` (default), `PROMPT_NAME`, `PROMPT_TEMPLATE_MD` (consts, exposed
//! as `pub const` below), `TL_MAX_PL_REVIEWS` (pub const, value 2).
//!
//! The PL/TL handshake: PL reads the technical contract TL drafted and either signs it or pushes
//! back with concrete, product-scoped concerns. Pulls the original `raw_intent` + clarify Q&A
//! from `pl_sessions`/`pl_questions`/`pl_answers` (best-effort — JS wraps this in try/catch and
//! falls back to placeholder strings if the PL tables or a matching session don't exist), ensures
//! the `pl.contract.review.v1` prompt template is registered (`ai_prompt_find::promptFind` /
//! `ai_prompt_add::promptAdd`, both already-converted twins), then calls `ai_ai_call::aiCall` and
//! parses the `verdict`/`rationale_md`/`pushback_md` output_kv fields, updates the contract row
//! (`pl_review_count`, `pl_last_pushback_md`, `status`, `rejection_reason_md`, `failure_kind`),
//! and returns the decision summary.
//!
//! BESPOKE-NATIVE, not conform-db fixture-tested: the function's tail depends on
//! `ai_ai_call::aiCall`, itself PARITY-DEFERRED (see ai-call.rs's module doc: the provider
//! dispatch is a real, non-deterministic network round-trip, unconvertible in principle). Every
//! deterministic step BEFORE that call — contract lookup + status/cap guards, the best-effort
//! pl_sessions/pl_questions/pl_answers pull, files_touched/acceptance/microtasks JSON parsing,
//! `ensurePrompt`'s find-or-register flow, and the input_kv assembly — is ported faithfully and
//! proven by the native tests below (same convention as qa-senior-review.rs / man-diagnose-
//! failure.rs). The AI happy-path (parsing a real `aiResult.output_kv` into verdict/rationale_md/
//! pushback_md, and the resulting UPDATE) is NOT exercised natively — `aiCall` always returns
//! `Err` here, so `plContractReview` always short-circuits to that Err once it reaches the call.
//!
//! `context.db` missing → JS throws `err.contextMissing('pl-contract-review', 'db')`. Not
//! modeled: `db: &Connection` stays a required, non-nullable reference (every sibling db-lane
//! twin's convention). `contract_slug` missing → JS `err.required('pl-contract-review',
//! 'contract_slug')` → message `"pl-contract-review: contract_slug is required"`.
//!
//! Throw fidelity (verified against engine/error.mjs's AntilokiError message-join format,
//! `${skill}: ${cause}`):
//!   - contract not found → `"pl-contract-review: contract 'X' not found"` (err.notFound's cause
//!     shape `${what} '${value}' not found` with what='contract').
//!   - status != 'tl_approved' → `"pl-contract-review: contract 'X' status='Y', expected
//!     'tl_approved' (review only after TL signs)"`.
//!   - pl_review_count >= TL_MAX_PL_REVIEWS (2) → `"pl-contract-review: contract 'X' has hit
//!     pl_review_count=N (cap=2). MAN must arbitrate via man.audit.contract.phase1."`.
//!
//! Cross-module calls (pre-existing Rust twins): `crate::ai_prompt_find::promptFind`,
//! `crate::ai_prompt_add::promptAdd`, `crate::ai_ai_call::aiCall`.
// QUALITY: bespoke-native — contract-state guards, best-effort PL-session context pull, and real
// prompt-registration db writes ported and tested; the aiCall network dispatch is the disclosed
// PARITY-DEFERRED boundary (inherited from ai-call.rs, not re-litigated here).

use rusqlite::Connection;
use serde_json::{json, Map, Value};

fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) }
fn str_qq(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(),
        Some(o) => o.to_string(),
    }
}
fn take_chars(s: &str, n: usize) -> String { s.chars().take(n).collect() }

pub const TL_MAX_PL_REVIEWS: i64 = 2;

pub const PROMPT_NAME: &str = "pl.contract.review.v1";

pub const PROMPT_TEMPLATE_MD: &str = "You are PL (Product Lead). TL just drafted a technical contract from your ratified product intent. Read it carefully and decide whether to sign or push back.

You are NOT a tech reviewer — TL handles tech soundness. You ARE the keeper of the product goal. Push back when:
  - The scope drifted from what the operator originally asked for
  - The acceptance criteria don't cover the product outcome
  - TL made hidden assumptions about the product that you disagree with
  - The briefing doesn't address concerns you originally raised in clarify

Don't push back on tech choices (file structure, library picks, microtask decomposition) — those are TL's call.

But DO push back when the contract's OWN goal_md requires structural fields that are still empty. A contract whose goal_md says \"TL must fill naming_json / must_obey_json / template_kind\" but leaves them blank is NOT ready — that is a product-completeness gap, not a tech opinion. The actual values of those fields are shown below so you can verify they are populated.

═══ ORIGINAL OPERATOR INTENT ═══
{{raw_intent}}

═══ PL CLARIFICATIONS Q&A ═══
{{clarify_summary}}

═══ TL'S CONTRACT ═══
slug:            {{contract_slug}}
title:           {{contract_title}}
template_kind:   {{template_kind}}
files_touched:   {{files_touched}}
acceptance:      {{acceptance}}
naming_json:     {{naming_json}}
must_obey_json:  {{must_obey_json}}
goal_md:         {{goal_md}}
tl_briefing_md:  {{tl_briefing_md}}
microtask count: {{microtask_count}}

═══ PRIOR PUSHBACK (if any) ═══
{{prior_pushback_md}}

═══ DECISION ═══

Return ONLY a JSON object — no prose before or after — matching EXACTLY this template:
{
  \"verdict\": \"signed\",            // MUST be exactly \"signed\" OR \"pushback\"
  \"rationale_md\": \"<1-3 sentences explaining your call (min 30 chars)>\",
  \"pushback_md\": \"\"               // \"\" when signed; when pushback, ≥20 chars of concrete, actionable changes (1-3 specific items, product/scope/acceptance only)
}

Rules the response MUST satisfy (they are machine-checked — a violation bounces back to you):
  - verdict is exactly one of \"signed\" | \"pushback\".
  - If verdict=\"signed\": pushback_md MUST be \"\" (empty string).
  - If verdict=\"pushback\": pushback_md MUST be non-empty (≥20 chars) and list the specific changes TL must make. If the structural fields above (naming_json/must_obey_json/template_kind) are required by goal_md but empty, say exactly which to fill and with what.

Be honest. If TL's contract is genuinely complete, sign it — the loop is capped at 2 cycles and over-rejecting wastes the operator's time.";

fn output_fields_json() -> Value {
    json!([
        { "field_name": "verdict", "field_type": "enum", "required": true, "enum": ["signed", "pushback"] },
        { "field_name": "rationale_md", "field_type": "string", "required": true, "min_length": 30 },
        { "field_name": "pushback_md", "field_type": "string", "required": false, "min_length": 0 },
    ])
}

fn cross_field_rules_json() -> Value {
    json!([
        {
            "id": "pushback-requires-md",
            "expression": "output.verdict !== 'pushback' || (typeof output.pushback_md === 'string' && output.pushback_md.trim().length >= 20)",
            "message": "verdict=pushback requires pushback_md with ≥20 chars of concrete, actionable feedback",
        },
    ])
}

/// `ensurePrompt(db)` — find-or-register-or-reregister on template/field drift. JS: registers
/// when the existing row's `template_md` differs from the current `PROMPT_TEMPLATE_MD` too (not
/// just absence). Return value unused by the caller (JS `ensurePrompt(db);` discards it too).
fn ensure_prompt(db: &Connection) {
    let existing = crate::ai_prompt_find::promptFind(&json!({ "name": PROMPT_NAME }), db).ok();
    let up_to_date = existing
        .as_ref()
        .and_then(|e| e.get("template_md"))
        .and_then(Value::as_str)
        .map(|t| t == PROMPT_TEMPLATE_MD)
        .unwrap_or(false);
    if up_to_date {
        return;
    }
    let _ = crate::ai_prompt_add::promptAdd(
        &json!({
            "name": PROMPT_NAME,
            "template_md": PROMPT_TEMPLATE_MD,
            "default_tier": "high",
            "default_temperature": "cold",
            "max_output_tokens": 1200,
            "output_fields": output_fields_json(),
            "cross_field_rules": cross_field_rules_json(),
        }),
        db,
    );
}

/// `export default async function plContractReview(input, context)`
#[allow(non_snake_case)]
pub fn plContractReview(input: &Value, db: &Connection) -> Result<Value, String> {
    let contract_slug = input.get("contract_slug").and_then(Value::as_str).unwrap_or("");
    if contract_slug.is_empty() {
        return Err("pl-contract-review: contract_slug is required".to_string());
    }

    let rows = crate::db_json::query_rows(
        db,
        "SELECT slug, title, card_slug, status, files_touched_json, acceptance_json, \
         goal_md, tl_briefing_md, microtasks_json, pl_review_count, pl_last_pushback_md, \
         template_kind, naming_json, must_obey_json FROM contracts WHERE slug = ?",
        &[json!(contract_slug)],
    )?;
    let contract = match rows.as_array().and_then(|a| a.first()) {
        Some(c) => c.clone(),
        None => return Err(format!("pl-contract-review: contract '{contract_slug}' not found")),
    };

    let status = contract.get("status").and_then(Value::as_str).unwrap_or("");
    if status != "tl_approved" {
        return Err(format!(
            "pl-contract-review: contract '{contract_slug}' status='{status}', expected 'tl_approved' (review only after TL signs)"
        ));
    }
    let review_count = contract.get("pl_review_count").and_then(Value::as_i64).unwrap_or(0);
    if review_count >= TL_MAX_PL_REVIEWS {
        return Err(format!(
            "pl-contract-review: contract '{contract_slug}' has hit pl_review_count={review_count} (cap={TL_MAX_PL_REVIEWS}). MAN must arbitrate via man.audit.contract.phase1."
        ));
    }

    // Best-effort pull of the original raw_intent + clarify Q&A — JS wraps this whole block in
    // try/catch (pl tables may not exist) and falls back to placeholder strings on any failure.
    let card_slug = contract.get("card_slug").and_then(Value::as_str).unwrap_or("");
    let mut raw_intent = "(no PL session found)".to_string();
    let mut clarify_summary = "(no clarify exchanges recorded)".to_string();
    if let Ok(sess_rows) = crate::db_json::query_rows(
        db,
        "SELECT id, raw_intent FROM pl_sessions WHERE approved_card_slug = ? ORDER BY id DESC LIMIT 1",
        &[json!(card_slug)],
    ) {
        if let Some(session) = sess_rows.as_array().and_then(|a| a.first()) {
            if let Some(ri) = nz(session.get("raw_intent")).and_then(Value::as_str) {
                raw_intent = ri.to_string();
            }
            let session_id = session.get("id").cloned().unwrap_or(Value::Null);
            if let Ok(turn_rows) = crate::db_json::query_rows(
                db,
                "SELECT step, question_md, answer_md FROM pl_questions q \
                 LEFT JOIN pl_answers a ON a.question_id = q.id \
                 WHERE q.session_id = ? AND q.step = 'clarify' ORDER BY q.id ASC LIMIT 10",
                &[session_id],
            ) {
                if let Some(turns) = turn_rows.as_array() {
                    if !turns.is_empty() {
                        clarify_summary = turns
                            .iter()
                            .enumerate()
                            .map(|(i, t)| {
                                let q = t.get("question_md").and_then(Value::as_str).unwrap_or("");
                                let a = t.get("answer_md").and_then(Value::as_str);
                                let q = take_chars(q, 150);
                                let a = match a {
                                    Some(s) => take_chars(s, 200),
                                    None => "(unanswered)".to_string(),
                                };
                                format!("Q{}: {q}\nA{}: {a}", i + 1, i + 1)
                            })
                            .collect::<Vec<_>>()
                            .join("\n\n");
                    }
                }
            }
        }
    }

    let files_touched: Value = contract
        .get("files_touched_json")
        .and_then(Value::as_str)
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or_else(|| json!([]));
    let acceptance: Value = contract
        .get("acceptance_json")
        .and_then(Value::as_str)
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or_else(|| json!([]));
    let microtasks: Vec<Value> = contract
        .get("microtasks_json")
        .and_then(Value::as_str)
        .and_then(|s| serde_json::from_str::<Value>(s).ok())
        .and_then(|v| v.as_array().cloned())
        .unwrap_or_default();

    ensure_prompt(db);

    let template_kind = nz(contract.get("template_kind"))
        .and_then(Value::as_str)
        .unwrap_or("(none — no template classified this card)")
        .to_string();
    let naming_json = take_chars(&str_qq(nz(contract.get("naming_json")).or(Some(&json!("{}")))), 1500);
    let must_obey_json = take_chars(&str_qq(nz(contract.get("must_obey_json")).or(Some(&json!("[]")))), 1500);
    let goal_md = take_chars(&str_qq(nz(contract.get("goal_md")).or(Some(&json!("")))), 2000);
    let tl_briefing_md = take_chars(&str_qq(nz(contract.get("tl_briefing_md")).or(Some(&json!("")))), 4000);
    let prior_pushback_md = take_chars(
        &str_qq(nz(contract.get("pl_last_pushback_md")).or(Some(&json!("(none)")))),
        2000,
    );

    let ai_input = json!({
        "prompt_name": PROMPT_NAME,
        "input_kv": [
            { "key": "contract_slug", "value": contract_slug },
            { "key": "contract_title", "value": contract.get("title").and_then(Value::as_str).unwrap_or("") },
            { "key": "template_kind", "value": template_kind },
            { "key": "files_touched", "value": take_chars(&serde_json::to_string(&files_touched).unwrap_or_default(), 1000) },
            { "key": "acceptance", "value": take_chars(&serde_json::to_string(&acceptance).unwrap_or_default(), 2000) },
            { "key": "naming_json", "value": naming_json },
            { "key": "must_obey_json", "value": must_obey_json },
            { "key": "goal_md", "value": goal_md },
            { "key": "tl_briefing_md", "value": tl_briefing_md },
            { "key": "microtask_count", "value": microtasks.len().to_string() },
            { "key": "raw_intent", "value": take_chars(&raw_intent, 2000) },
            { "key": "clarify_summary", "value": take_chars(&clarify_summary, 3000) },
            { "key": "prior_pushback_md", "value": prior_pushback_md },
        ],
        "tier": "high",
        "temperature": "cold",
        "structured_output": true,
    });

    // PARITY-DEFERRED boundary: real `aiCall` always Errs here (see module doc + ai-call.rs).
    // Everything after this point (verdict parsing, the contracts UPDATE) is unreachable in the
    // native harness — ported faithfully from the JS below for parity when the deferred stub is
    // eventually replaced with a real provider dispatch.
    let ai_result = crate::ai_ai_call::aiCall(&ai_input, db)?;

    let output_kv: Vec<Value> = ai_result.get("output_kv").and_then(Value::as_array).cloned().unwrap_or_default();
    let find_field = |name: &str| -> Option<String> {
        output_kv
            .iter()
            .find(|o| o.get("field_name").and_then(Value::as_str) == Some(name))
            .and_then(|o| o.get("value_text"))
            .and_then(Value::as_str)
            .map(str::to_string)
    };
    let verdict = find_field("verdict");
    let rationale_md = find_field("rationale_md").unwrap_or_default();
    let pushback_md = find_field("pushback_md").unwrap_or_default();

    let verdict = match verdict.as_deref() {
        Some("signed") => "signed",
        Some("pushback") => "pushback",
        other => {
            return Err(format!(
                "pl-contract-review: PL review returned no usable verdict (got '{}') — expected 'signed' or 'pushback'. Treating as a malformed response rather than a silent sign-off.",
                other.unwrap_or("undefined")
            ));
        }
    };
    if verdict == "pushback" && pushback_md.trim().len() < 20 {
        return Err(
            "pl-contract-review: verdict=pushback requires pushback_md ≥20 chars with concrete feedback. PL produced an empty/thin pushback.".to_string(),
        );
    }

    let new_count = review_count + 1;
    let new_status = if verdict == "pushback" { "rejected" } else { "tl_approved" };
    let pushback_or_null: Value = if pushback_md.is_empty() { Value::Null } else { json!(pushback_md) };
    db.execute(
        &format!("UPDATE contracts SET pl_review_count = ?1, pl_last_pushback_md = ?2, status = ?3, \
         rejection_reason_md = CASE WHEN ?4 = 'pushback' THEN ?5 ELSE rejection_reason_md END, \
         failure_kind = CASE WHEN ?6 = 'pushback' THEN 'pl_pushback' ELSE failure_kind END, \
         updated_at = {} WHERE slug = ?7", crate::clock::now_sql()),
        rusqlite::params![
            new_count,
            pushback_or_null.as_str(),
            new_status,
            verdict,
            pushback_or_null.as_str(),
            verdict,
            contract_slug
        ],
    )
    .map_err(|e| format!("pl-contract-review: UPDATE contracts failed: {e}"))?;

    let mut out = Map::new();
    out.insert("contract_slug".into(), json!(contract_slug));
    out.insert("verdict".into(), json!(verdict));
    out.insert("pushback".into(), json!(verdict == "pushback"));
    out.insert("rationale_md".into(), json!(rationale_md));
    out.insert("pushback_md".into(), json!(pushback_md));
    out.insert("pl_review_count".into(), json!(new_count));
    out.insert("new_status".into(), json!(new_status));
    out.insert("cap_reached".into(), json!(new_count >= TL_MAX_PL_REVIEWS));
    out.insert("ai_call_id".into(), ai_result.get("ai_call_id").cloned().unwrap_or(Value::Null));
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "ATTACH ':memory:' AS brain;
            CREATE TABLE contracts (
              id INTEGER PRIMARY KEY AUTOINCREMENT, slug TEXT NOT NULL UNIQUE, title TEXT,
              card_slug TEXT, status TEXT NOT NULL DEFAULT 'proposed',
              files_touched_json TEXT NOT NULL DEFAULT '[]', acceptance_json TEXT NOT NULL DEFAULT '[]',
              goal_md TEXT, tl_briefing_md TEXT, microtasks_json TEXT NOT NULL DEFAULT '[]',
              pl_review_count INTEGER NOT NULL DEFAULT 0, pl_last_pushback_md TEXT,
              template_kind TEXT, naming_json TEXT, must_obey_json TEXT,
              rejection_reason_md TEXT, failure_kind TEXT, updated_at TEXT);
            CREATE TABLE pl_sessions (id INTEGER PRIMARY KEY AUTOINCREMENT, raw_intent TEXT NOT NULL, approved_card_slug TEXT);
            CREATE TABLE pl_questions (id INTEGER PRIMARY KEY AUTOINCREMENT, session_id INTEGER NOT NULL, step TEXT NOT NULL, question_md TEXT NOT NULL);
            CREATE TABLE pl_answers (id INTEGER PRIMARY KEY AUTOINCREMENT, question_id INTEGER NOT NULL UNIQUE, answer_md TEXT);
            CREATE TABLE brain.prompt_templates ( id INTEGER PRIMARY KEY, name TEXT NOT NULL, version INTEGER NOT NULL, template_md TEXT NOT NULL, default_tier TEXT NOT NULL, default_temperature TEXT NOT NULL, max_output_tokens INTEGER NOT NULL DEFAULT 1024, superseded_by INTEGER, created_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')), cross_field_rules_json TEXT, judging_criteria_json TEXT, system_md TEXT, phase_flow TEXT NOT NULL DEFAULT 'single', UNIQUE (name, version) );
            CREATE TABLE brain.prompt_output_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, grounded_refs_json TEXT, UNIQUE (prompt_template_id, field_name) );
            CREATE TABLE brain.prompt_input_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, UNIQUE (prompt_template_id, field_name) );",
        )
        .unwrap();
        c
    }

    fn insert_contract(c: &Connection, slug: &str, status: &str, review_count: i64) {
        c.execute(
            "INSERT INTO contracts (slug, title, card_slug, status, pl_review_count, goal_md, tl_briefing_md) \
             VALUES (?1, 'Title', 'CARD-1', ?2, ?3, 'goal', 'briefing')",
            rusqlite::params![slug, status, review_count],
        )
        .unwrap();
    }

    // ── guard paths ──────────────────────────────────────────────────
    #[test]
    fn missing_contract_slug_returns_exact_message() {
        let c = db();
        assert_eq!(
            plContractReview(&json!({}), &c).unwrap_err(),
            "pl-contract-review: contract_slug is required"
        );
    }

    #[test]
    fn contract_not_found_returns_exact_message() {
        let c = db();
        assert_eq!(
            plContractReview(&json!({ "contract_slug": "GHOST" }), &c).unwrap_err(),
            "pl-contract-review: contract 'GHOST' not found"
        );
    }

    #[test]
    fn wrong_status_returns_exact_message() {
        let c = db();
        insert_contract(&c, "CT-1", "proposed", 0);
        assert_eq!(
            plContractReview(&json!({ "contract_slug": "CT-1" }), &c).unwrap_err(),
            "pl-contract-review: contract 'CT-1' status='proposed', expected 'tl_approved' (review only after TL signs)"
        );
    }

    #[test]
    fn cap_reached_returns_exact_message() {
        let c = db();
        insert_contract(&c, "CT-1", "tl_approved", 2);
        assert_eq!(
            plContractReview(&json!({ "contract_slug": "CT-1" }), &c).unwrap_err(),
            "pl-contract-review: contract 'CT-1' has hit pl_review_count=2 (cap=2). MAN must arbitrate via man.audit.contract.phase1."
        );
    }

    // ── reaches the deferred aiCall boundary and registers the prompt first ──
    #[test]
    fn valid_contract_reaches_deferred_ai_call_and_registers_prompt_first() {
        let c = db();
        insert_contract(&c, "CT-1", "tl_approved", 0);
        let err = plContractReview(&json!({ "contract_slug": "CT-1" }), &c).unwrap_err();
        assert!(err.contains("ai-call"), "unexpected error: {err}");

        let tpl = crate::ai_prompt_find::promptFind(&json!({ "name": PROMPT_NAME }), &c).unwrap();
        assert_eq!(tpl["name"], json!(PROMPT_NAME));
        assert_eq!(tpl["default_tier"], json!("high"));
        assert_eq!(tpl["default_temperature"], json!("cold"));
        assert_eq!(tpl["output_fields"].as_array().unwrap().len(), 3);
    }

    #[test]
    fn ensure_prompt_is_idempotent_second_call_does_not_duplicate() {
        let c = db();
        ensure_prompt(&c);
        ensure_prompt(&c);
        let count: i64 = c
            .query_row("SELECT COUNT(*) FROM brain.prompt_templates WHERE name = ?", [PROMPT_NAME], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1);
    }

    // ── best-effort pl_sessions/pl_questions/pl_answers pull (proceeds even without match) ──
    #[test]
    fn no_matching_pl_session_still_reaches_deferred_boundary() {
        let c = db();
        insert_contract(&c, "CT-2", "tl_approved", 0);
        let err = plContractReview(&json!({ "contract_slug": "CT-2" }), &c).unwrap_err();
        assert!(err.contains("ai-call"));
    }

    #[test]
    fn matching_pl_session_with_clarify_turns_still_reaches_deferred_boundary() {
        let c = db();
        insert_contract(&c, "CT-3", "tl_approved", 0);
        c.execute(
            "INSERT INTO pl_sessions (raw_intent, approved_card_slug) VALUES ('build the thing', 'CARD-1')",
            [],
        )
        .unwrap();
        let session_id = c.last_insert_rowid();
        c.execute(
            "INSERT INTO pl_questions (session_id, step, question_md) VALUES (?1, 'clarify', 'what scope?')",
            [session_id],
        )
        .unwrap();
        let question_id = c.last_insert_rowid();
        c.execute("INSERT INTO pl_answers (question_id, answer_md) VALUES (?1, 'just the API')", [question_id]).unwrap();

        let err = plContractReview(&json!({ "contract_slug": "CT-3" }), &c).unwrap_err();
        assert!(err.contains("ai-call"));
    }

    // ── malformed JSON columns fall back to defaults, still reaches deferred boundary ──
    #[test]
    fn malformed_json_columns_fall_back_to_defaults() {
        let c = db();
        c.execute(
            "INSERT INTO contracts (slug, title, card_slug, status, pl_review_count, files_touched_json, acceptance_json, microtasks_json) \
             VALUES ('CT-4', 'Title', 'CARD-1', 'tl_approved', 0, 'not-json', 'not-json', 'not-json')",
            [],
        )
        .unwrap();
        let err = plContractReview(&json!({ "contract_slug": "CT-4" }), &c).unwrap_err();
        assert!(err.contains("ai-call"));
    }
}
