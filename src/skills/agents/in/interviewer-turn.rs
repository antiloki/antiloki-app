//! interviewer-turn.rs — twin of skills/agents/in/interviewer-turn.mjs (db WRITE lane; the
//! SINGLE entry point for user input). NAMES ARE CONTRACT: `interviewerTurn` (default, only pub
//! fn). Composes ~20 already-converted twins by their real pub fn signatures (session/turn
//! bookkeeping, the destructive-verb floor, the greeting/pl-clarify floors, the AI triage +
//! validate-suggestion + anchor-fill, route + confirm-gate + card-proposal-normalize, the
//! auto-dispatch loop through `engine_action_run::skillRunAs`, the deterministic inline
//! capabilities (logic-spec / code-sample / code-transform), the clarify-questionnaire screen,
//! the phase-2 presenter, the deterministic view-intent fallback, the widget offers, and the
//! final telemetry persist).
//!
//! `context` collapses to `db: &Connection` (this crate's uniform db-lane signature). JS's
//! `context.actor` (default `'human:cli'`) has no carrier here — it is read ONLY as the default
//! for `user_identifier` in the real JS body (grepped: no other read site), so it is hardcoded as
//! a local `ACTOR` constant, same convention `workflow_workflow_run::workflowRun` uses for its own
//! single-use context default. `context.provider_fn` (JS test-injection hook for a mock AI
//! provider) has no Rust equivalent and is dropped the same way every LLM-PARTIAL twin in this
//! crate drops it (interviewer-chat.rs, interviewer-present.rs, atlas-code-sample.rs, …) — it only
//! ever reaches the deferred `ai_ai_call::aiCall` boundary inside those twins' own bodies.
//! `context.gitRunner` has no carrier through `engine_action_run::skillRunAs` either (documented
//! gap in interviewer-dispatch.rs's own module doc, same class of drop here).
//!
//! PARITY-DEFERRED (transitively): `agents_in_interviewer_chat::interviewerChat`,
//! `agents_in_interviewer_present::interviewerPresent`,
//! `agents_in_interviewer_clarify_questionnaire::interviewerClarifyQuestionnaire`, and
//! `codebase_atlas_logic_spec::atlasLogicSpec` all end in the deferred `ai_ai_call::aiCall`
//! network stub, which always returns `Err` in this crate — so in REAL (non-stub) execution every
//! call this file makes to those four twins resolves to their Err/degraded path. This file mirrors
//! the JS's own try/catch (or Ok(ok:false) check) around each of those calls exactly, so the
//! deterministic surface around them — session/turn bookkeeping, the destructive floor, the
//! greeting/pl-clarify floors, prebuilt_plan handling, route/gate/normalize, auto-dispatch,
//! deterministic view-intent inference, widget offers, and telemetry persistence — is real,
//! reachable, ported code, exercised directly by this file's own `#[cfg(test)]` tests.
//! `agents_in_interviewer_embed_turn::interviewerEmbedTurn` is likewise PARITY-DEFERRED
//! transitively (the ONNX embed model has no Rust twin) but never throws — matches JS's own
//! best-effort, always-non-fatal treatment.
//!
//! `crate::codebase_code_transform::codeTransform` is a SIBLING conversion landing in this same
//! wave (an AST-guided JS/ESM source-edit skill with no Rust twin at the time this file was
//! written). It never throws in JS (`export default function codeTransform(...)` always returns a
//! plain `{ok, ...}` object, no throw path) — so, matching this crate's established convention for
//! never-throwing JS defaults (e.g. `interviewerGreetingFloor` returns plain `Value`, not
//! `Result`), this twin calls it as `fn codeTransform(input: &Value, db: &Connection) -> Value`.
//! If the sibling conversion lands with a different signature, the central build step reconciles
//! the two — the call site here is written to the established crate convention, not guessed ad hoc.
//!
//! Raw SQL against `interviewer_sessions`/`interviewer_turns`/`runtime_events` (session
//! bookkeeping, turn persistence, best-effort tracing) mirrors the JS's own direct
//! `db.prepare(...).run(...)` calls — these tables are NOT in the NEVER_BYPASS_WITH_AUDIT audited
//! set (cards/agents/actions/laws/memories), matching every sibling interviewer-* twin's identical
//! raw-SQL treatment of the same tables (interviewer-dispatch.rs, interviewer-session-stale-end.rs).
//!
//! Frozen-clock literal `2026-01-01 00:00:00` stands in for the JS's `datetime('now')` writes
//! (cold-start `last_active_at` bump), per this crate's established conform-harness convention.
use regex::Regex;
use rusqlite::Connection;
use serde_json::{json, Value};
use std::sync::OnceLock;

// ── standard helpers (playbook) ─────────────────────────────────────────────────────────────
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
fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
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
fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) }
// JS `String(x ?? '')` — nullish (not falsy) coalesces to '' BEFORE String() runs. Distinct from
// js_string(Null) == "null" (gotcha #2 in the crossing playbook).
fn or_empty(v: &Value) -> String { if v.is_null() { String::new() } else { js_string(v) } }
// JS `a ?? b` — nullish coalescing (only null/undefined fall through, not falsy values).
fn nullish(a: &Value, b: &Value) -> Value { if a.is_null() { b.clone() } else { a.clone() } }

const ACTOR: &str = "human:cli"; // context.actor default — only read site is user_identifier's own default (see module doc)

fn re_want_sample_1() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"(?i)\b(before\s*[/&]*\s*(and\s*)?after|sample|example|snippet|how\s+would\s+(it|the\s+code|this))\b").unwrap())
}
fn re_want_sample_2() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"(?i)\b(code|refactor|improve|improvement|rewrite|implementation|function|method|class|file|module)\b").unwrap())
}
fn re_want_improvements() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"(?i)\b(improve|improvements?|better|refactor|messy|cleaner|what'?s?\s+(wrong|bad|off)|not\s+(great|good)|issues?|problems?|smell|tech(nical)?\s*debt|quality|weakness|concerns?)\b").unwrap())
}
fn re_logic_1() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"(?i)\b(design|plan|sketch|outline|structure)\b").unwrap())
}
fn re_logic_2() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"(?i)\b(logic|function|module|feature|flow|algorithm|component|method|the\s+code)\b").unwrap())
}
fn re_logic_3() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"(?i)\blogic\s+(design|spec|for|of)\b").unwrap())
}
fn re_logic_4() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"(?i)\bhow\s+(would|should)\b[^?]*\bwork\b").unwrap())
}
fn re_rename() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"(?i)\brename\s+([A-Za-z_$][\w$]*)\s+(?:to|->|\u{2192}|into)\s+([A-Za-z_$][\w$]*)").unwrap())
}
fn re_import() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r#"(?i)\b(?:rewire|repoint|change|update|switch)\s+(?:the\s+)?import\s+(?:of\s+)?[`'"]?([^`'"\s]+)[`'"]?\s+(?:to|->|\u{2192})\s+[`'"]?([^`'"\s]+)[`'"]?"#).unwrap())
}
fn re_file() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r#"(?i)\bin\s+[`'"]?([\w./-]+\.(?:js|mjs|cjs))[`'"]?"#).unwrap())
}

fn action_slug_field(action: &str) -> Option<&'static str> {
    match action {
        "board-keeper.card.children" => Some("parent_slug"),
        "board-keeper.card.get" => Some("slug"),
        "board-keeper.card.update" => Some("slug"),
        "board-keeper.card.status.change" => Some("slug"),
        "board-keeper.card.html" => Some("slug"),
        "board-keeper.card.score" => Some("slug"),
        "board-keeper.card.draft.auto" => Some("slug"),
        "git-manager.contract.load" => Some("card_slug"),
        "pl.session.start" => Some("approved_card_slug"),
        "system.card.skip-pl.set" => Some("slug"),
        "system.card.force-status" => Some("slug"),
        "system.work.start" => Some("slug"),
        _ => None,
    }
}

fn trace_event(db: &Connection, turn_id: i64, skill: &str, detail: Option<Value>) {
    let ctx = detail.map(|d| json!(serde_json::to_string(&d).unwrap_or_default())).unwrap_or(Value::Null);
    let _ = crate::db_json::run_write(
        db,
        "INSERT INTO runtime_events (actor, agent, skill, outcome, user_goal_id, source, context_json) VALUES ('interviewer', 'in', ?, 'success', ?, 'chat', ?)",
        &[json!(skill), json!(turn_id), ctx],
    );
}

fn get_or_create_session(db: &Connection, user_identifier: &Value) -> Result<i64, String> {
    let rows = crate::db_json::query_rows(
        db,
        "SELECT id FROM interviewer_sessions WHERE user_identifier IS ? AND status='active' ORDER BY id DESC LIMIT 1",
        &[user_identifier.clone()],
    )?;
    if let Some(id) = rows.as_array().and_then(|a| a.first()).and_then(|r| r.get("id")).and_then(Value::as_i64) {
        return Ok(id);
    }
    let (_changes, rowid) = crate::db_json::run_write(
        db,
        "INSERT INTO interviewer_sessions (user_identifier, status) VALUES (?, 'active')",
        &[user_identifier.clone()],
    )?;
    Ok(rowid)
}

fn next_turn_index(db: &Connection, session_id: i64) -> Result<i64, String> {
    let rows = crate::db_json::query_rows(
        db,
        "SELECT COALESCE(MAX(turn_index), -1) + 1 AS next FROM interviewer_turns WHERE session_id = ?",
        &[json!(session_id)],
    )?;
    Ok(rows.as_array().and_then(|a| a.first()).and_then(|r| r.get("next")).and_then(Value::as_i64).unwrap_or(0))
}

pub fn interviewerTurn(input: &Value, db: &Connection) -> Result<Value, String> {
    let obj = input.as_object();
    let get = |k: &str| obj.and_then(|o| o.get(k));

    let user_text_v = get("user_text").cloned().unwrap_or(Value::Null);
    let user_text: String = match &user_text_v {
        Value::String(s) if !s.is_empty() => s.clone(),
        _ => return Err("interviewer-turn: user_text is required".into()),
    };

    let user_identifier = get("user_identifier").cloned().unwrap_or(json!(ACTOR));
    let explicit_session_id = get("session_id").cloned().unwrap_or(Value::Null);
    let prebuilt_plan_v = get("prebuilt_plan").cloned().unwrap_or(Value::Null);
    let references = get("references").cloned().unwrap_or(json!([]));
    let attachments = get("attachments").cloned().unwrap_or(json!([]));
    let model_id = get("model_id").cloned().unwrap_or(Value::Null);
    let pane_overlay = get("pane_overlay").cloned().unwrap_or(Value::Null);
    let screen_md = get("screen_md").cloned().unwrap_or(Value::Null);
    let episode_prior_md = get("episode_prior_md").cloned().unwrap_or(Value::Null);

    // FEAT-INTERVIEWER-PREBUILT-PLAN
    let has_prebuilt_plan = !prebuilt_plan_v.is_null();
    let prebuilt_plan: Vec<Value> = if has_prebuilt_plan {
        let arr = match prebuilt_plan_v.as_array() {
            Some(a) if !a.is_empty() => a.clone(),
            _ => return Err("interviewer-turn: prebuilt_plan (must be non-empty array when set) is required".into()),
        };
        for p in &arr {
            let action_ok = p.get("action").and_then(Value::as_str).map_or(false, |s| !s.is_empty());
            if !action_ok {
                return Err("interviewer-turn: prebuilt_plan[].action is required".into());
            }
        }
        arr
    } else {
        Vec::new()
    };

    // ── 0. Stale-end pass (non-fatal) ────────────────────────────────────
    let _ = crate::agents_in_interviewer_session_stale_end::interviewerSessionStaleEnd(&json!({}), db);

    // ── 1. Session ────────────────────────────────────────────────────
    let session_id: i64 = if !explicit_session_id.is_null() {
        let sid = js_number(Some(&explicit_session_id));
        let mut resolved: Option<i64> = None;
        if sid.is_finite() && sid.fract() == 0.0 && sid > 0.0 {
            let sid_i = sid as i64;
            let exists = crate::db_json::query_rows(db, "SELECT id FROM interviewer_sessions WHERE id = ?", &[json!(sid_i)])?;
            if exists.as_array().map_or(false, |a| !a.is_empty()) {
                resolved = Some(sid_i);
            }
        }
        match resolved {
            Some(s) => s,
            None => get_or_create_session(db, &user_identifier)?,
        }
    } else {
        get_or_create_session(db, &user_identifier)?
    };
    let turn_index = next_turn_index(db, session_id)?;

    // ── 2. Insert placeholder turn ──────────────────────────────────────
    let (_changes, turn_id) = crate::db_json::run_write(
        db,
        "INSERT INTO interviewer_turns (session_id, turn_index, user_text, classified_kind) VALUES (?, ?, ?, 'clarify')",
        &[json!(session_id), json!(turn_index), json!(user_text.clone())],
    )?;

    // FEAT-COLD-START-BOOTSTRAP (best-effort; column may not exist on very old DBs)
    let _ = crate::db_json::run_write(
        db,
        "UPDATE interviewer_sessions SET last_active_at = datetime('2026-01-01 00:00:00') WHERE id = ?",
        &[json!(session_id)],
    );

    // FEAT-CROSS-SESSION-RAG (best-effort; embed model has no Rust twin)
    if std::env::var("ANTILOKI_DISABLE_TURN_EMBED").ok().as_deref() != Some("1") {
        let _ = crate::agents_in_interviewer_embed_turn::interviewerEmbedTurn(
            &json!({ "turn_id": turn_id, "user_text": user_text.clone() }),
            db,
        );
    }

    // ── 3. Destructive-verb safety floor (deterministic, pre-AI) ────────
    let det_classification = crate::agents_in_interviewer_classify::interviewerClassify(&json!({ "user_text": user_text.clone() }), db)?;

    let mut response_md: Value = Value::Null;
    let classification: Value;
    let mut suggested_action_name: Value = Value::Null;
    let mut suggested_args: Value = json!({});
    let mut routing: Value = Value::Null;
    let mut intent_summary: Value = json!("");
    let mut ui_commands: Value = json!([]);
    let mut delegated_to_agent: Value = json!("in");
    let mut next_steps: Value = json!([]);
    let mut context_bundle: Value = Value::Null;
    let mut ai_telemetry: Vec<Value> = Vec::new();
    let mut turn_model_id: Value = Value::Null;
    let mut turn_ai_call_id: Value = Value::Null;
    let mut turn_ai_tier: Value = Value::Null;
    let mut turn_ai_cost_usd: Value = Value::Null;
    let mut turn_ai_duration_ms: Value = Value::Null;
    let mut turn_ai_tokens_in: Value = Value::Null;
    let mut turn_ai_tokens_out: Value = Value::Null;

    // ── 3a. Pre-built plan path ─────────────────────────────────────────
    let greeting: Value = if has_prebuilt_plan {
        json!({ "matched": false })
    } else {
        crate::agents_in_interviewer_greeting_floor::interviewerGreetingFloor(
            &json!({ "user_text": user_text.clone(), "turn_index": turn_index }),
            Some(db),
        )
    };

    let code_sample_enabled = std::env::var("ANTILOKI_INTERVIEWER_CODE_SAMPLE").ok().as_deref() == Some("1");
    let wants_sample = !has_prebuilt_plan
        && re_want_sample_1().is_match(&user_text)
        && re_want_sample_2().is_match(&user_text);
    let wants_improvements = !has_prebuilt_plan
        && re_want_improvements().is_match(&user_text)
        && crate::codebase_smart_context_bundle::isCodeQuestion(&json!(user_text.clone()));
    let wants_code_answer = wants_sample || wants_improvements;

    if has_prebuilt_plan {
        let kind = if prebuilt_plan.iter().any(|p| p.get("risk_tier").and_then(Value::as_str) == Some("critical")) {
            "destructive"
        } else if prebuilt_plan.iter().any(|p| p.get("risk_tier").and_then(Value::as_str) == Some("high")) {
            "autonomous"
        } else if prebuilt_plan.iter().any(|p| p.get("risk_tier").and_then(Value::as_str) == Some("medium")) {
            "mutate"
        } else {
            "curate"
        };
        let suggested_actions: Vec<Value> = prebuilt_plan.iter().map(|p| json!({
            "name": p.get("action").cloned().unwrap_or(Value::Null),
            "args": p.get("args").cloned().unwrap_or(json!({})),
            "risk_tier": p.get("risk_tier").cloned().unwrap_or(Value::Null),
        })).collect();
        classification = json!({
            "kind": kind,
            "confidence": 1.0,
            "reason": "prebuilt-plan: caller supplied actions directly (no AI classification)",
            "suggested_actions": suggested_actions,
            "response_md": Value::Null,
        });
        let action_names: Vec<String> = prebuilt_plan.iter()
            .map(|p| p.get("action").and_then(Value::as_str).unwrap_or("").to_string())
            .collect();
        let n = prebuilt_plan.len();
        intent_summary = json!(format!("prebuilt plan ({} action{}): {}", n, if n == 1 { "" } else { "s" }, action_names.join(", ")));
        delegated_to_agent = json!("in");
        response_md = json!(format!("Pre-built plan accepted: {}.", action_names.join(", ")));
    } else if truthy(greeting.get("matched")) {
        classification = json!({
            "kind": "clarify",
            "confidence": 1.0,
            "reason": "greeting-floor: canned reply, no AI call",
            "suggested_actions": [],
            "response_md": greeting.get("response_md").cloned().unwrap_or(Value::Null),
        });
        response_md = greeting.get("response_md").cloned().unwrap_or(Value::Null);
        intent_summary = greeting.get("intent_summary").cloned().unwrap_or(json!(""));
        next_steps = greeting.get("next_steps").cloned().unwrap_or(json!([]));
        delegated_to_agent = json!("in");
    } else if det_classification.get("kind").and_then(Value::as_str) == Some("destructive") {
        classification = det_classification.clone();
        response_md = json!("That looks like a destructive request \u{2014} I won't auto-dispatch it. Type the exact slug shown to confirm, or say 'cancel'.");
        intent_summary = json!("User issued a destructive request");
        delegated_to_agent = json!("in");
    } else if code_sample_enabled && wants_code_answer {
        // ── 3z. CODE-SAMPLE FLOOR ────────────────────────────────────────
        classification = json!({
            "kind": "read",
            "confidence": 1.0,
            "reason": "code-sample-floor: deterministic before/after request \u{2014} chat triage skipped",
            "suggested_actions": [],
            "response_md": Value::Null,
        });
        intent_summary = json!("concrete before/after code improvements");
        delegated_to_agent = json!("in");
        // response_md stays null — §6b sets presentation_md
    } else {
        let pl_floor = crate::agents_in_interviewer_pl_clarify_floor::interviewerPlClarifyFloor(&json!({ "user_text": user_text.clone() }), db)?;
        if truthy(pl_floor.get("matched")) {
            classification = json!({
                "kind": "clarify",
                "confidence": 1.0,
                "reason": "pl-clarify-floor: substrate has pending PL questions, surfacing without AI call",
                "suggested_actions": [],
                "response_md": pl_floor.get("response_md").cloned().unwrap_or(Value::Null),
            });
            response_md = pl_floor.get("response_md").cloned().unwrap_or(Value::Null);
            intent_summary = pl_floor.get("intent_summary").cloned().unwrap_or(json!(""));
            next_steps = pl_floor.get("next_steps").cloned().unwrap_or(json!([]));
            delegated_to_agent = json!("in");
        } else {
            // ── 3a. Topic anchor ─────────────────────────────────────────
            let topic_anchor = crate::agents_in_interviewer_topic_anchor::interviewerTopicAnchor(
                &json!({ "session_id": session_id, "current_turn_index": turn_index }),
                db,
            )?;

            // ── 3b. AI chat (primary path) ────────────────────────────────
            trace_event(db, turn_id, "triage-start", Some(json!({ "phase": "thinking" })));
            let chat_input = json!({
                "user_text": user_text.clone(),
                "session_id": session_id,
                "topic_anchor": topic_anchor.get("prompt_block_md").cloned().unwrap_or(Value::Null),
                "references": references.clone(),
                "attachments": attachments.clone(),
                "model_id": model_id.clone(),
                "pane_overlay": pane_overlay.clone(),
                "screen_md": screen_md.clone(),
                "episode_prior_md": episode_prior_md.clone(),
            });
            let ai: Value = match crate::agents_in_interviewer_chat::interviewerChat(&chat_input, db) {
                Ok(v) => v,
                Err(e) => {
                    let msg: String = e.chars().take(100).collect();
                    json!({
                        "response_md": format!("(interviewer brain unavailable: {}). Try again, or use a substrate action directly via 'antiloki <action-name>'.", msg),
                        "kind": "clarify",
                        "suggested_action_name": Value::Null,
                        "suggested_args": {},
                    })
                }
            };
            response_md = ai.get("response_md").cloned().unwrap_or(Value::Null);
            suggested_action_name = ai.get("suggested_action_name").cloned().unwrap_or(Value::Null);
            suggested_args = nz(ai.get("suggested_args")).cloned().unwrap_or(json!({}));
            routing = nz(ai.get("routing")).cloned().unwrap_or(Value::Null);
            intent_summary = nz(ai.get("intent_summary")).cloned().unwrap_or(json!(""));
            delegated_to_agent = nz(ai.get("delegated_to_agent")).cloned().unwrap_or(json!("in"));
            next_steps = nz(ai.get("next_steps")).cloned().unwrap_or(json!([]));
            context_bundle = nz(ai.get("context_bundle")).cloned().unwrap_or(Value::Null);
            ui_commands = match ai.get("ui_commands") { Some(v @ Value::Array(_)) => v.clone(), _ => json!([]) };
            if let Some(t) = nz(ai.get("ai_telemetry")) { ai_telemetry.push(t.clone()); }

            let ai_tel = ai.get("ai_telemetry");
            trace_event(db, turn_id, "triage", Some(json!({
                "phase": "triage",
                "kind": if truthy(Some(&suggested_action_name)) { "route" } else { "answer" },
                "model": ai_tel.and_then(|t| t.get("model_id")).cloned().unwrap_or(Value::Null),
                "tier": ai_tel.and_then(|t| t.get("tier")).cloned().unwrap_or(Value::Null),
                "cost_usd": ai_tel.and_then(|t| t.get("cost_usd")).cloned().unwrap_or(Value::Null),
                "tokens_in": ai_tel.and_then(|t| t.get("tokens_in")).cloned().unwrap_or(Value::Null),
                "tokens_out": ai_tel.and_then(|t| t.get("tokens_out")).cloned().unwrap_or(Value::Null),
                "cached": ai_tel.and_then(|t| t.get("cached")).cloned().unwrap_or(Value::Null),
            })));

            turn_ai_call_id = nz(ai.get("ai_call_id")).cloned().unwrap_or(Value::Null);
            turn_model_id = ai_tel.and_then(|t| nz(t.get("model_id"))).cloned().unwrap_or(Value::Null);
            turn_ai_tier = ai_tel.and_then(|t| nz(t.get("tier"))).cloned().unwrap_or(Value::Null);
            turn_ai_cost_usd = ai_tel.and_then(|t| nz(t.get("cost_usd"))).cloned().unwrap_or(Value::Null);
            turn_ai_duration_ms = ai_tel.and_then(|t| nz(t.get("duration_ms"))).cloned().unwrap_or(Value::Null);
            turn_ai_tokens_in = ai_tel.and_then(|t| nz(t.get("tokens_in"))).cloned().unwrap_or(Value::Null);
            turn_ai_tokens_out = ai_tel.and_then(|t| nz(t.get("tokens_out"))).cloned().unwrap_or(Value::Null);

            // validate the AI's suggestion against the real action catalog
            if truthy(Some(&suggested_action_name)) {
                let v = crate::agents_in_interviewer_validate_suggestion::interviewerValidateSuggestion(&json!({
                    "suggested_action_name": suggested_action_name.clone(),
                    "delegated_to_agent": delegated_to_agent.clone(),
                    "suggested_args": suggested_args.clone(),
                    "user_text": user_text.clone(),
                }), db)?;
                if !truthy(v.get("valid")) {
                    suggested_action_name = Value::Null;
                    suggested_args = json!({});
                    if let Some(ca) = nz(v.get("corrected_agent")) { delegated_to_agent = ca.clone(); }
                }
            }

            // FIX-INTERVIEWER-ARGS-FROM-TOPIC-ANCHOR
            let topic_slug = nz(topic_anchor.get("slug")).cloned();
            let mut anchor_fill_note: Option<String> = None;
            if truthy(Some(&suggested_action_name)) {
                if let Some(slug) = &topic_slug {
                    if let Some(field) = action_slug_field(suggested_action_name.as_str().unwrap_or("")) {
                        let missing = suggested_args.get(field).map_or(true, Value::is_null);
                        if missing {
                            if let Some(o) = suggested_args.as_object_mut() {
                                o.insert(field.to_string(), slug.clone());
                            } else {
                                let mut m = serde_json::Map::new();
                                m.insert(field.to_string(), slug.clone());
                                suggested_args = Value::Object(m);
                            }
                            anchor_fill_note = Some(format!(" (anchor-slug fill: {}={})", field, js_string(slug)));
                        }
                    }
                }
            }

            let ai_kind = ai.get("kind").cloned().unwrap_or(json!("clarify"));
            let ai_conf_num = js_number(ai.get("confidence"));
            let confidence: f64 = if ai_conf_num.is_finite() {
                ai_conf_num
            } else if truthy(Some(&suggested_action_name)) {
                0.85
            } else {
                0.5
            };
            let mut reason = format!("interviewer-chat: {}{}", js_string(&ai_kind), anchor_fill_note.clone().unwrap_or_default());

            let suggested_actions = if truthy(Some(&suggested_action_name)) {
                json!([{ "name": suggested_action_name.clone(), "args": suggested_args.clone() }])
            } else {
                json!([])
            };

            let mut final_confidence = confidence;

            // FIX-INTERVIEWER-CONTEXT-GROUNDING-FROM-RECENT-TURNS: topic-anchor confidence floor
            if let Some(slug) = &topic_slug {
                if truthy(Some(&crate::agents_in_interviewer_topic_anchor::isShortFollowUp(&json!(user_text.clone())))) {
                    if final_confidence < 0.85 {
                        final_confidence = 0.85;
                        reason = format!("{} (topic-anchor floor: {})", reason, js_string(slug));
                    }
                }
            }

            classification = json!({
                "kind": ai_kind,
                "confidence": final_confidence,
                "reason": reason,
                "suggested_actions": suggested_actions,
                "response_md": response_md.clone(),
            });
        }
    }

    // FEAT-AGENT-REASONING-LOG
    let ai_used = det_classification.get("kind").and_then(Value::as_str) != Some("destructive");
    let _ = crate::engine_log_decision::logDecision(&json!({
        "agent": "in",
        "decision": "classify",
        "rationale": nz(classification.get("reason")).cloned().unwrap_or_else(|| json!(format!("kind={}", js_string(classification.get("kind").unwrap_or(&Value::Null))))),
        "inputs": {
            "user_text": user_text.clone(),
            "kind": classification.get("kind").cloned().unwrap_or(Value::Null),
            "confidence": classification.get("confidence").cloned().unwrap_or(Value::Null),
            "suggested_actions": classification.get("suggested_actions").cloned().unwrap_or(Value::Null),
            "ai_used": ai_used,
        },
    }), db);

    // ── 4. Route + confirm-gate ──────────────────────────────────────────
    let mut route = crate::agents_in_interviewer_route::interviewerRoute(&json!({ "classification": classification.clone() }), db)?;

    if classification.get("kind").and_then(Value::as_str) == Some("clarify") {
        let plan_arr = route.get("plan").and_then(Value::as_array).cloned().unwrap_or_default();
        if !plan_arr.is_empty() {
            let mut ns_arr = next_steps.as_array().cloned().unwrap_or_default();
            for p in &plan_arr {
                let action_name = p.get("action").cloned().unwrap_or(Value::Null);
                let already = ns_arr.iter().any(|ns| ns.get("action_name") == Some(&action_name));
                if !already {
                    ns_arr.push(json!({ "hint": format!("proposed earlier \u{2014} {}", js_string(&action_name)), "action_name": action_name }));
                }
            }
            next_steps = json!(ns_arr);
            route["plan"] = json!([]);
            route["requires_confirmation"] = json!(false);
        }
    }

    // EPIC-VIBE-CHAT-UNIFICATION M3: pane file tools are SELF-GATING
    if let Some(pane_id) = nz(pane_overlay.get("pane_id")).cloned() {
        if let Some(plan_arr) = route.get_mut("plan").and_then(Value::as_array_mut) {
            for p in plan_arr.iter_mut() {
                let is_pane_file = p.get("action").and_then(Value::as_str).map_or(false, |a| a.starts_with("cockpit.pane.file."));
                if is_pane_file {
                    let op = if p.get("action").and_then(Value::as_str).map_or(false, |a| a.ends_with(".read")) { "read" } else { "write" };
                    let mut args = p.get("args").cloned().unwrap_or(json!({}));
                    if let Some(o) = args.as_object_mut() {
                        o.insert("pane_id".into(), pane_id.clone());
                        o.insert("op".into(), json!(op));
                    } else {
                        args = json!({ "pane_id": pane_id.clone(), "op": op });
                    }
                    p["args"] = args;
                    p["risk_tier"] = json!("low");
                }
            }
            let all_pane = !plan_arr.is_empty() && plan_arr.iter().all(|p| p.get("action").and_then(Value::as_str).map_or(false, |a| a.starts_with("cockpit.pane.file.")));
            if all_pane {
                route["risk_tier"] = json!("low");
                route["requires_confirmation"] = json!(false);
            }
        }
    }

    let mut gate_input = route.as_object().cloned().unwrap_or_default();
    gate_input.insert("classification".into(), classification.clone());
    let gate = crate::agents_in_interviewer_confirm_gate::interviewerConfirmGate(&Value::Object(gate_input), db)?;

    // FEAT-INTERVIEWER-CARD-PROPOSAL-NORMALIZE
    let normalized = crate::agents_in_interviewer_card_proposal_normalize::interviewerCardProposalNormalize(
        &json!({ "plan": route.get("plan").cloned().unwrap_or(json!([])) }),
        db,
    )?;
    route["plan"] = normalized.get("plan").cloned().unwrap_or(json!([]));

    // ── 5. Update turn row with classification + proposed plan ──────────
    crate::db_json::run_write(
        db,
        "UPDATE interviewer_turns SET classified_kind = ?, classification_confidence = ?, classified_reason = ?, proposed_actions_json = ?, interviewer_response_md = ?, model_id = ?, ai_call_id = ?, ai_tier = ?, ai_cost_usd = ?, ai_duration_ms = ?, ai_tokens_in = ?, ai_tokens_out = ? WHERE id = ?",
        &[
            classification.get("kind").cloned().unwrap_or(Value::Null),
            nz(classification.get("confidence")).cloned().unwrap_or(Value::Null),
            nz(classification.get("reason")).cloned().unwrap_or(Value::Null),
            json!(serde_json::to_string(route.get("plan").unwrap_or(&json!([]))).unwrap_or_default()),
            response_md.clone(),
            turn_model_id.clone(), turn_ai_call_id.clone(), turn_ai_tier.clone(), turn_ai_cost_usd.clone(), turn_ai_duration_ms.clone(),
            turn_ai_tokens_in.clone(), turn_ai_tokens_out.clone(),
            json!(turn_id),
        ],
    )?;

    // ── 6. Auto-dispatch low-risk (bypass=true) ──────────────────────────
    let mut dispatched_ids: Value = Value::Null;
    let mut dispatch_results: Option<Vec<Value>> = None;
    let bypass = truthy(gate.get("bypass"));
    let plan_arr = route.get("plan").and_then(Value::as_array).cloned().unwrap_or_default();
    if bypass && !plan_arr.is_empty() {
        let mut results: Vec<Value> = Vec::new();
        for p in &plan_arr {
            let action_name = p.get("action").and_then(Value::as_str).unwrap_or("").to_string();
            let args = p.get("args").cloned().unwrap_or(json!({}));

            if action_name == "system.investigate" {
                let question = match args.get("question") { Some(v) if !v.is_null() => v.clone(), _ => json!(user_text.clone()) };
                if crate::codebase_smart_context_bundle::isCodeQuestion(&question) {
                    match crate::codebase_smart_context_bundle::smartContextBundle(&json!({
                        "question": question.clone(), "token_budget": 16000, "trace_turn_id": turn_id,
                    }), db) {
                        Ok(bundle) => {
                            results.push(json!({ "action": "system.codebase.context.bundle", "ok": true, "result": bundle.clone() }));
                            if let Some(sources) = bundle.get("sources").and_then(Value::as_array) {
                                if !sources.is_empty() {
                                    let _ = crate::db_json::run_write(
                                        db,
                                        "UPDATE interviewer_turns SET sources_json = ? WHERE id = ?",
                                        &[json!(serde_json::to_string(&bundle["sources"]).unwrap_or_default()), json!(turn_id)],
                                    );
                                }
                            }
                            let q_trunc: String = js_string(&question).chars().take(80).collect();
                            trace_event(db, turn_id, "rag", Some(json!({
                                "phase": "search", "query": q_trunc,
                                "files": bundle.get("sources").and_then(Value::as_array).map_or(0, |a| a.len()),
                                "scope": bundle.get("scope").cloned().unwrap_or(Value::Null),
                            })));
                        }
                        Err(e) => results.push(json!({ "action": action_name.clone(), "ok": false, "error": e })),
                    }
                    continue;
                }
            }

            match crate::engine_action_run::skillRunAs(
                &json!({ "action": action_name.clone(), "input": args.clone() }),
                db, "interviewer:auto", Some("interviewer:confirmed"), &json!(turn_id),
            ) {
                Ok(r) => results.push(json!({ "action": action_name.clone(), "ok": true, "result": r })),
                Err(e) => results.push(json!({ "action": action_name.clone(), "ok": false, "error": e })),
            }
        }

        let any_ok = results.iter().any(|r| r.get("ok") == Some(&Value::Bool(true)));
        if any_ok {
            let max_rows = crate::db_json::query_rows(db, "SELECT MAX(id) AS m FROM sacred_timeline", &[])?;
            let last_id = max_rows.as_array().and_then(|a| a.first()).and_then(|r| r.get("m")).cloned().unwrap_or(Value::Null);
            dispatched_ids = json!([last_id]);
            let _ = crate::db_json::run_write(
                db,
                "UPDATE interviewer_turns SET confirmed=1, dispatched_action_ids_json=? WHERE id = ?",
                &[json!(serde_json::to_string(&dispatched_ids).unwrap_or_default()), json!(turn_id)],
            );
        }
        dispatch_results = Some(results);
    }

    // ── 7. Phase 2: PRESENTER ────────────────────────────────────────────
    let mut presentation_md: Value = Value::Null;
    let mut view_intent: Value = Value::Null;
    let mut view_data: Value = Value::Null;
    let mut presenter_notes: Value = Value::Null;
    let mut clarify_questionnaire: Value = Value::Null;
    let presenter_enabled = std::env::var("ANTILOKI_INTERVIEWER_PRESENTER").ok().as_deref() != Some("0");
    let greeting_matched = classification.get("reason").and_then(Value::as_str).map_or(false, |r| r.contains("greeting-floor"));
    let mut code_sample_handled = false;
    let classification_kind_str = classification.get("kind").and_then(Value::as_str).unwrap_or("").to_string();

    // ── 6a. LOGIC DESIGN (mini-AST) ───────────────────────────────────────
    let logic_spec_enabled = std::env::var("ANTILOKI_INTERVIEWER_LOGIC_SPEC").ok().as_deref() == Some("1");
    let wants_logic_design = (re_logic_1().is_match(&user_text) && re_logic_2().is_match(&user_text))
        || re_logic_3().is_match(&user_text)
        || re_logic_4().is_match(&user_text);
    if logic_spec_enabled && wants_logic_design && classification_kind_str != "destructive" && !greeting_matched {
        if let Ok(r) = crate::codebase_atlas_logic_spec::atlasLogicSpec(&json!({ "intent": user_text.clone() }), db) {
            if let Some(t) = nz(r.get("ai_telemetry")) { ai_telemetry.push(t.clone()); }
            if truthy(r.get("ok")) {
                if let Some(spec) = nz(r.get("spec")) {
                    view_intent = json!("tabs");
                    let name = spec.get("name").cloned().unwrap_or(Value::Null);
                    view_data = json!({ "tabs": [{ "label": "logic", "kind": "logic-spec", "payload": { "spec": spec } }], "label": "logic design", "hint": name });
                    let suffix = if truthy(Some(&name)) { format!(" for `{}`", js_string(&name)) } else { String::new() };
                    presentation_md = json!(format!("Here's the logic design{} \u{2014} review the shape before writing the code.", suffix));
                    code_sample_handled = true;
                }
            }
        }
        // non-fatal: fall through to the normal flow (Err path — atlasLogicSpec propagates the
        // deferred aiCall's Err in real execution)
    }

    // ── 6b. CODE SAMPLE (before/after) ────────────────────────────────────
    if code_sample_enabled && wants_code_answer && !code_sample_handled && classification_kind_str != "destructive" && !greeting_matched {
        trace_event(db, turn_id, "improvements", Some(json!({ "phase": "analyze" })));
        if let Ok(sample) = crate::codebase_atlas_code_sample::atlasCodeSample(&json!({ "ask": user_text.clone() }), db) {
            match sample.get("ai_telemetry") {
                Some(Value::Array(arr)) => { for t in arr { ai_telemetry.push(t.clone()); } }
                Some(t) if !t.is_null() => ai_telemetry.push(t.clone()),
                _ => {}
            }
            let tel0: Option<Value> = match sample.get("ai_telemetry") {
                Some(Value::Array(arr)) => arr.first().cloned(),
                Some(t) if !t.is_null() => Some(t.clone()),
                _ => None,
            };
            let files_len = sample.get("files").and_then(Value::as_array).map(|a| a.len() as i64);
            let suggestions_len = sample.get("improvements").and_then(Value::as_array).map_or(0, |a| a.len() as i64);
            trace_event(db, turn_id, "improvements-done", Some(json!({
                "phase": "analyze",
                "files": files_len,
                "suggestions": suggestions_len,
                "model": tel0.as_ref().and_then(|t| t.get("model_id")).cloned().unwrap_or(Value::Null),
                "cost_usd": tel0.as_ref().and_then(|t| t.get("cost_usd")).cloned().unwrap_or(Value::Null),
            })));
            let improvements = sample.get("improvements").and_then(Value::as_array).cloned().unwrap_or_default();
            if truthy(sample.get("ok")) && !improvements.is_empty() {
                let file_count = sample.get("files").and_then(Value::as_array).map_or(1, |a| a.len().max(1));
                let n_imp = improvements.len();
                view_intent = json!("improvements");
                view_data = json!({
                    "improvements": improvements,
                    "summary": sample.get("summary").cloned().unwrap_or(Value::Null),
                    "diagram": sample.get("diagram").cloned().unwrap_or(Value::Null),
                    "files": sample.get("files").cloned().unwrap_or(Value::Null),
                });
                presentation_md = json!(format!(
                    "Here {} {} concrete improvement{}{} \u{2014} overview and before/after below.",
                    if n_imp == 1 { "is" } else { "are" }, n_imp, if n_imp == 1 { "" } else { "s" },
                    if file_count > 1 { format!(" across {} files", file_count) } else { String::new() },
                ));
                code_sample_handled = true;
            } else if let Some(err) = nz(sample.get("error")) {
                let err_s = js_string(err);
                response_md = if err_s.contains("no_atlas") {
                    json!("I can show a before/after, but this workspace's codebase isn't indexed yet \u{2014} build the Atlas (Code screen) and ask again.")
                } else {
                    json!(format!("I tried to pull a before/after sample but couldn't read the source ({}).", err_s))
                };
                code_sample_handled = true;
                let _ = crate::db_json::run_write(db, "UPDATE interviewer_turns SET interviewer_response_md = ? WHERE id = ?", &[response_md.clone(), json!(turn_id)]);
            }
        }
    }

    // ── 6c. CODE TRANSFORM (deterministic, AST-guided) ────────────────────
    let code_transform_enabled = std::env::var("ANTILOKI_INTERVIEWER_CODE_TRANSFORM").ok().as_deref() != Some("0");
    let rename_m = re_rename().captures(&user_text);
    let import_m = re_import().captures(&user_text);
    let has_transform_match = rename_m.is_some() || import_m.is_some();
    if code_transform_enabled && has_transform_match && !code_sample_handled && classification_kind_str != "destructive" && !greeting_matched {
        let caps = rename_m.as_ref().or(import_m.as_ref()).unwrap();
        let is_rename = rename_m.is_some();
        let from = caps.get(1).map(|m| m.as_str().to_string()).unwrap_or_default();
        let to = caps.get(2).map(|m| m.as_str().to_string()).unwrap_or_default();

        let file_m = re_file().captures(&user_text);
        let mut xfile: Option<String> = None;
        if let Some(fc) = &file_m {
            let f1 = fc.get(1).map(|m| m.as_str().to_string()).unwrap_or_default();
            match crate::db_json::query_rows(
                db,
                "SELECT path FROM code.code_nodes WHERE origin='atlas' AND type='file' AND path LIKE ? ORDER BY length(path) ASC LIMIT 1",
                &[json!(format!("%{}", f1))],
            ) {
                Ok(rows) => {
                    xfile = rows.as_array().and_then(|a| a.first()).and_then(|r| r.get("path")).and_then(Value::as_str).map(String::from);
                    if xfile.is_none() { xfile = Some(f1); }
                }
                Err(_) => { xfile = Some(f1); }
            }
        }

        if xfile.is_none() {
            response_md = json!(format!(
                "Which file? Say it like \"rename {} to {} in path/to/file.js\" \u{2014} I apply it deterministically (JS/ESM, AST-guided).",
                if is_rename { from.clone() } else { "OLD".to_string() },
                if is_rename { to.clone() } else { "NEW".to_string() },
            ));
            code_sample_handled = true;
            let _ = crate::db_json::run_write(db, "UPDATE interviewer_turns SET interviewer_response_md = ? WHERE id = ?", &[response_md.clone(), json!(turn_id)]);
        } else {
            let op = if is_rename { "rename" } else { "rewire_import" };
            let xfile_s = xfile.unwrap();
            // codeTransform returns status objects in JS (never throws); its Rust twin is Result —
            // fold Err into the same {ok:false, error} shape the JS caller checks via .ok/.error.
            let tr = crate::codebase_code_transform::codeTransform(&json!({
                "file": xfile_s, "op": op, "args": { "from": from.clone(), "to": to.clone() },
            }), db).unwrap_or_else(|e| json!({ "ok": false, "error": e }));
            if truthy(tr.get("ok")) {
                let file_v = tr.get("file").cloned().unwrap_or(Value::Null);
                let changes = tr.get("changes").and_then(Value::as_i64).unwrap_or(0);
                view_intent = json!("tabs");
                view_data = json!({
                    "tabs": [{ "label": "diff", "kind": "diff", "payload": { "before": tr.get("before"), "after": tr.get("after"), "file": file_v } }],
                    "label": "transform",
                    "hint": format!("{} \u{b7} {} edit{}", op, changes, if changes == 1 { "" } else { "s" }),
                });
                presentation_md = json!(format!(
                    "Done \u{2014} {} in `{}` ({} edit{}, deterministic \u{2014} review the diff).",
                    if is_rename { format!("renamed `{}` \u{2192} `{}`", from, to) } else { format!("rewired import `{}` \u{2192} `{}`", from, to) },
                    js_string(&file_v), changes, if changes == 1 { "" } else { "s" },
                ));
                code_sample_handled = true;
            } else if let Some(err) = nz(tr.get("error")) {
                response_md = json!(format!("Couldn't apply that transform: {}", js_string(err)));
                code_sample_handled = true;
                let _ = crate::db_json::run_write(db, "UPDATE interviewer_turns SET interviewer_response_md = ? WHERE id = ?", &[response_md.clone(), json!(turn_id)]);
            }
        }
    }

    // CODE QUESTIONS the triage classified `read` (not `investigate`) — deterministic bundle fallback
    let dr_empty = dispatch_results.as_ref().map_or(true, |d| d.is_empty());
    if !code_sample_handled && !has_prebuilt_plan && dr_empty && crate::codebase_smart_context_bundle::isCodeQuestion(&json!(user_text.clone())) {
        if let Ok(bundle) = crate::codebase_smart_context_bundle::smartContextBundle(&json!({
            "question": user_text.clone(), "token_budget": 16000, "trace_turn_id": turn_id,
        }), db) {
            if truthy(bundle.get("bundle_md")) {
                dispatch_results = Some(vec![json!({ "action": "system.codebase.context.bundle", "ok": true, "result": bundle.clone() })]);
            }
            let q_trunc: String = user_text.chars().take(80).collect();
            trace_event(db, turn_id, "rag", Some(json!({
                "phase": "search", "query": q_trunc,
                "files": bundle.get("sources").and_then(Value::as_array).map_or(0, |a| a.len()),
                "scope": bundle.get("scope").cloned().unwrap_or(Value::Null),
            })));
        }
    }

    let has_something_to_present = dispatch_results.as_ref().map_or(false, |d| !d.is_empty())
        || route.get("plan").and_then(Value::as_array).map_or(false, |a| !a.is_empty());
    let should_present = presenter_enabled
        && classification_kind_str != "destructive"
        && !greeting_matched
        && !code_sample_handled
        && has_something_to_present;

    // FEAT-INTERVIEWER-CLARIFY-QUESTIONNAIRE
    let low_confidence = classification.get("confidence").and_then(Value::as_f64).unwrap_or(0.0) < 0.7;
    let questionnaire_enabled = std::env::var("ANTILOKI_INTERVIEWER_QUESTIONNAIRE").ok().as_deref() != Some("0");

    let mut prev_turn_had_questionnaire = false;
    if let Ok(rows) = crate::db_json::query_rows(
        db,
        "SELECT proposed_actions_json, classified_kind, classified_reason, clarify_questionnaire_json FROM interviewer_turns WHERE session_id = ? AND turn_index = ?",
        &[json!(session_id), json!(turn_index - 1)],
    ) {
        if let Some(prev) = rows.as_array().and_then(|a| a.first()) {
            if prev.get("classified_kind").and_then(Value::as_str) == Some("clarify") {
                let prev_plan: Value = prev.get("proposed_actions_json").and_then(Value::as_str)
                    .and_then(|s| serde_json::from_str(s).ok()).unwrap_or(json!([]));
                if prev_plan.as_array().map_or(true, |a| a.is_empty()) {
                    prev_turn_had_questionnaire = true;
                }
                if prev_turn_had_questionnaire {
                    if let Some(pqs) = prev.get("clarify_questionnaire_json").and_then(Value::as_str) {
                        if let Ok(pq) = serde_json::from_str::<Value>(pqs) {
                            let opts = pq.get("options").and_then(Value::as_array).cloned().unwrap_or_default();
                            let ut = user_text.trim().to_lowercase();
                            let answered = opts.iter().any(|o| {
                                let label = o.get("label").and_then(Value::as_str).unwrap_or("").trim().to_lowercase();
                                let key = o.get("key").and_then(Value::as_str).unwrap_or("").trim().to_lowercase();
                                label == ut || key == ut
                            });
                            if answered { prev_turn_had_questionnaire = false; }
                        }
                    }
                }
            }
        }
    }

    let has_proposal = truthy(Some(&suggested_action_name)) || route.get("plan").and_then(Value::as_array).map_or(false, |a| !a.is_empty());
    let should_question = questionnaire_enabled
        && low_confidence
        && !greeting_matched
        && !prev_turn_had_questionnaire
        && dispatch_results.is_none()
        && !code_sample_handled
        && classification_kind_str != "destructive"
        && classification.get("reason").and_then(Value::as_str) != Some("greeting-floor: canned reply, no AI call")
        && (classification_kind_str == "clarify" || has_proposal);

    if should_question {
        let recent_turns = context_bundle.get("recent_turns").cloned().unwrap_or(json!(""));
        if let Ok(cq) = crate::agents_in_interviewer_clarify_questionnaire::interviewerClarifyQuestionnaire(&json!({
            "user_text": user_text.clone(), "intent_summary": intent_summary.clone(), "recent_turns": recent_turns,
        }), db) {
            if truthy(cq.get("should_render")) {
                if let Some(questions) = cq.get("questions").and_then(Value::as_array).filter(|a| !a.is_empty()) {
                    clarify_questionnaire = json!({ "should_render": true, "questions": questions });
                    response_md = if questions.len() == 1 {
                        questions[0].get("question_md").cloned().unwrap_or(Value::Null)
                    } else {
                        json!("A few quick questions so I can answer well:")
                    };
                }
            }
            if let Some(t) = nz(cq.get("ai_telemetry")) { ai_telemetry.push(t.clone()); }
        }
        // non-fatal: interviewerClarifyQuestionnaire propagates the deferred aiCall's Err in real execution
    }

    // Persist the screening fork so a reloaded session re-renders it.
    if !clarify_questionnaire.is_null() {
        crate::db_json::run_write(
            db,
            "UPDATE interviewer_turns SET clarify_questionnaire_json = ?, interviewer_response_md = ? WHERE id = ?",
            &[json!(serde_json::to_string(&clarify_questionnaire).unwrap_or_default()), response_md.clone(), json!(turn_id)],
        )?;
    }

    if should_present {
        trace_event(db, turn_id, "present-start", Some(json!({ "phase": "writing" })));
        let dispatch_results_for_present: Value = match &dispatch_results { Some(v) => Value::Array(v.clone()), None => Value::Null };
        if let Ok(pres) = crate::agents_in_interviewer_present::interviewerPresent(&json!({
            "user_text": user_text.clone(),
            "intent_summary": intent_summary.clone(),
            "classification_kind": classification.get("kind").cloned().unwrap_or(Value::Null),
            "dispatch_results": dispatch_results_for_present,
            "session_id": session_id,
        }), db) {
            presentation_md = nz(pres.get("presentation_md")).filter(|v| truthy(Some(*v))).cloned().unwrap_or(Value::Null);
            view_intent = nz(pres.get("view_intent")).filter(|v| truthy(Some(*v))).cloned().unwrap_or(Value::Null);
            view_data = nz(pres.get("view_data")).filter(|v| truthy(Some(*v))).cloned().unwrap_or(Value::Null);
            presenter_notes = nz(pres.get("notes_to_self")).filter(|v| truthy(Some(*v))).cloned().unwrap_or(Value::Null);
            if let Some(t) = nz(pres.get("ai_telemetry")) { ai_telemetry.push(t.clone()); }
            let tel = pres.get("ai_telemetry");
            trace_event(db, turn_id, "present", Some(json!({
                "phase": "present",
                "model": tel.and_then(|t| t.get("model_id")).cloned().unwrap_or(Value::Null),
                "tier": tel.and_then(|t| t.get("tier")).cloned().unwrap_or(Value::Null),
                "cost_usd": tel.and_then(|t| t.get("cost_usd")).cloned().unwrap_or(Value::Null),
                "tokens_in": tel.and_then(|t| t.get("tokens_in")).cloned().unwrap_or(Value::Null),
                "tokens_out": tel.and_then(|t| t.get("tokens_out")).cloned().unwrap_or(Value::Null),
            })));
        }
        // Presenter is non-fatal: fall back to deterministic inference below (propagates the
        // deferred aiCall's Err in real execution).
    }

    // FIX-INTERVIEWER-AUTODISPATCH-RESPONSE-RENDERING: deterministic fallback
    let dr_nonempty = dispatch_results.as_ref().map_or(false, |d| !d.is_empty());
    if dr_nonempty && (!truthy(Some(&presentation_md)) || !truthy(Some(&view_intent))) {
        let dr_val = Value::Array(dispatch_results.clone().unwrap_or_default());
        if let Ok(inferred) = crate::agents_in_interviewer_view_intent_infer::interviewerViewIntentInfer(&json!({
            "dispatch_results": dr_val, "user_text": user_text.clone(),
        }), db) {
            if !truthy(Some(&view_intent)) {
                if let Some(v) = nz(inferred.get("view_intent")).filter(|v| truthy(Some(*v))) { view_intent = v.clone(); }
            }
            if !truthy(Some(&view_data)) {
                if let Some(v) = nz(inferred.get("view_data")).filter(|v| truthy(Some(*v))) { view_data = v.clone(); }
            }
            if !truthy(Some(&presentation_md)) {
                if let Some(v) = nz(inferred.get("presentation_md")).filter(|v| truthy(Some(*v))) { presentation_md = v.clone(); }
            }
        }
    }

    // FIX-INLINE-ANSWER-PERSISTED
    let pm_trim = or_empty(&presentation_md).trim().to_string();
    if code_sample_handled && !pm_trim.is_empty() && presentation_md != response_md {
        response_md = presentation_md.clone();
        let _ = crate::db_json::run_write(db, "UPDATE interviewer_turns SET interviewer_response_md = ? WHERE id = ?", &[response_md.clone(), json!(turn_id)]);
    }

    // FINAL FALLBACK — never render a blank turn.
    let rendered_something = !or_empty(&presentation_md).trim().is_empty()
        || truthy(Some(&view_intent))
        || dispatch_results.as_ref().map_or(false, |d| d.iter().any(|r| truthy(r.get("ok"))))
        || !or_empty(&response_md).trim().is_empty()
        || truthy(Some(&clarify_questionnaire));
    if !rendered_something {
        let steps: Vec<Value> = next_steps.as_array().cloned().unwrap_or_default().into_iter().take(4).collect();
        response_md = if !steps.is_empty() {
            let lines: Vec<String> = steps.iter().map(|s| {
                let label = nz(s.get("hint")).or_else(|| s.get("action_name")).cloned().unwrap_or(Value::Null);
                format!("- **{}**", js_string(&label))
            }).collect();
            json!(format!("Here are a few ways I can help \u{2014} pick one:\n{}", lines.join("\n")))
        } else {
            json!("I'm not sure what you'd like to do yet \u{2014} could you say a bit more about what you're after?")
        };
        let _ = crate::db_json::run_write(db, "UPDATE interviewer_turns SET interviewer_response_md = ? WHERE id = ?", &[response_md.clone(), json!(turn_id)]);
    }

    // DETERMINISTIC WIDGET OFFERS
    let dispatch_results_value: Value = match &dispatch_results { Some(v) => Value::Array(v.clone()), None => Value::Null };
    let mut offered_widgets: Value = json!([]);
    {
        let answer_src = nullish(&presentation_md, &response_md);
        let answer_md = or_empty(&answer_src);
        if let Ok(off) = crate::agents_in_interviewer_widget_offers::interviewerWidgetOffers(&json!({
            "user_text": user_text.clone(),
            "dispatch_results": dispatch_results_value.clone(),
            "answer_md": answer_md,
        }), db) {
            offered_widgets = off.get("offers").cloned().unwrap_or(json!([]));
        }
    }

    // WIDGET + TELEMETRY DURABILITY (best-effort)
    {
        let tel: Vec<Value> = ai_telemetry.iter().filter(|t| !t.is_null()).cloned().collect();
        let sum = |k: &str| -> f64 { tel.iter().map(|t| t.get(k).and_then(Value::as_f64).unwrap_or(0.0)).sum() };
        let primary = tel.iter().find(|t| nz(t.get("ai_call_id")).is_some()).or_else(|| tel.first());
        let offered_arr = offered_widgets.as_array().cloned().unwrap_or_default();
        let _ = crate::db_json::run_write(
            db,
            "UPDATE interviewer_turns SET view_intent = ?, view_data_json = ?, ai_telemetry_json = ?, offered_widgets_json = ?, ai_cost_usd = ?, ai_tokens_in = ?, ai_tokens_out = ?, model_id = COALESCE(model_id, ?), ai_call_id = COALESCE(ai_call_id, ?), ai_tier = COALESCE(ai_tier, ?) WHERE id = ?",
            &[
                view_intent.clone(),
                if view_data.is_null() { Value::Null } else { json!(serde_json::to_string(&view_data).unwrap_or_default()) },
                if tel.is_empty() { Value::Null } else { json!(serde_json::to_string(&json!(tel)).unwrap_or_default()) },
                if offered_arr.is_empty() { Value::Null } else { json!(serde_json::to_string(&offered_widgets).unwrap_or_default()) },
                if tel.is_empty() { turn_ai_cost_usd.clone() } else { json!(sum("cost_usd")) },
                if tel.is_empty() { turn_ai_tokens_in.clone() } else { json!(sum("tokens_in")) },
                if tel.is_empty() { turn_ai_tokens_out.clone() } else { json!(sum("tokens_out")) },
                primary.and_then(|p| p.get("model_id")).cloned().unwrap_or(Value::Null),
                primary.and_then(|p| p.get("ai_call_id")).cloned().unwrap_or(Value::Null),
                primary.and_then(|p| p.get("tier")).cloned().unwrap_or(Value::Null),
                json!(turn_id),
            ],
        );
    }

    let dispatch_failed = dispatch_results.as_ref().map_or(false, |d| !d.is_empty() && !d.iter().any(|r| truthy(r.get("ok"))));

    Ok(json!({
        "turn_id": turn_id,
        "session_id": session_id,
        "turn_index": turn_index,
        "ui_commands": ui_commands,
        "model_id": turn_model_id,
        "ai_call_id": turn_ai_call_id,
        "ai_tier": turn_ai_tier,
        "ai_cost_usd": turn_ai_cost_usd,
        "ai_duration_ms": turn_ai_duration_ms,
        "classification": classification,
        "plan": route.get("plan").cloned().unwrap_or(json!([])),
        "risk_tier": route.get("risk_tier").cloned().unwrap_or(Value::Null),
        "gate": gate,
        "auto_dispatched": dispatch_results.is_some(),
        "dispatch_results": dispatch_results_value,
        "confirmed": !dispatched_ids.is_null(),
        "dispatch_failed": dispatch_failed,
        "response_md": response_md.clone(),
        "ai_fallback_response": response_md,
        "routing": routing,
        "intent_summary": intent_summary,
        "delegated_to_agent": delegated_to_agent,
        "next_steps": next_steps,
        "context_bundle": context_bundle,
        "ai_telemetry": ai_telemetry,
        "presentation_md": presentation_md,
        "view_intent": view_intent,
        "view_data": view_data,
        "presenter_notes": presenter_notes,
        "clarify_questionnaire": clarify_questionnaire,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    const ACTIONS_SCHEMA: &str = "CREATE TABLE actions (name TEXT UNIQUE, description TEXT, owner_agent TEXT, steps_json TEXT, returns TEXT, on_failure TEXT, kind TEXT)";
    const AGENT_ACTIONS_SCHEMA: &str = "CREATE TABLE agent_actions (agent_name TEXT, action_name TEXT, revoked_at TEXT)";
    const RUNTIME_EVENTS_SCHEMA: &str = "CREATE TABLE runtime_events (id INTEGER PRIMARY KEY AUTOINCREMENT, ts TEXT NOT NULL DEFAULT '2026-01-01 00:00:00', parent_event_id INTEGER, actor TEXT NOT NULL, agent TEXT, action TEXT, skill TEXT, duration_ms REAL, outcome TEXT NOT NULL CHECK (outcome IN ('success','error','blocked','skipped','in_flight')), error_code TEXT, context_json TEXT, user_goal_id INTEGER, source TEXT)";
    const CARDS_SCHEMA: &str = "CREATE TABLE cards (slug TEXT PRIMARY KEY, title TEXT, status TEXT, score REAL, updated_at TEXT NOT NULL DEFAULT '2026-01-01 00:00:00')";
    const LAWS_SCHEMA: &str = "CREATE TABLE laws.laws (id INTEGER PRIMARY KEY, slug TEXT, kind TEXT, domain TEXT, statement TEXT, enforcement TEXT, enforcement_hook TEXT, superseded_by INTEGER)";
    const LAW_VIOLATIONS_SCHEMA: &str = "CREATE TABLE laws.law_violations (id INTEGER PRIMARY KEY, law_id INTEGER NOT NULL, attempted_at TEXT NOT NULL DEFAULT '2026-01-01 00:00:00', attempted_by TEXT NOT NULL, agent_name TEXT, action_name TEXT, context_json TEXT, outcome TEXT NOT NULL, CHECK (outcome IN ('blocked', 'warned', 'escaped', 'allowed_exception')))";
    const SACRED_TIMELINE_SCHEMA: &str = "CREATE TABLE sacred_timeline (id INTEGER PRIMARY KEY AUTOINCREMENT, entity_id TEXT)";
    const DECISIONS_SCHEMA: &str = "CREATE TABLE brain.brain_decisions ( id INTEGER PRIMARY KEY AUTOINCREMENT, session_id TEXT NOT NULL, decided_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')), card_slug TEXT, decision TEXT NOT NULL, rationale TEXT NOT NULL, inputs_json TEXT, fire_id INTEGER, CHECK (decision IN ('fire','wait','rollback','escalate','done','pause','session_start','session_end','goal_set','classify','gate_verdict','rule_match','sandbox_create','review_pass','schedule_and_dispatch','agent_step')) )";
    const SESSIONS_SCHEMA: &str = "CREATE TABLE interviewer_sessions (id INTEGER PRIMARY KEY AUTOINCREMENT, started_at TEXT NOT NULL DEFAULT '2026-01-01 00:00:00', user_identifier TEXT, status TEXT NOT NULL DEFAULT 'active', ended_at TEXT, context_summary_md TEXT, last_active_at TEXT)";
    const TURNS_SCHEMA: &str = "CREATE TABLE interviewer_turns (id INTEGER PRIMARY KEY AUTOINCREMENT, session_id INTEGER NOT NULL, ts TEXT NOT NULL DEFAULT '2026-01-01 00:00:00', turn_index INTEGER NOT NULL, user_text TEXT NOT NULL, classified_kind TEXT NOT NULL, classification_confidence REAL, classified_reason TEXT, proposed_actions_json TEXT NOT NULL DEFAULT '[]', confirmed INTEGER NOT NULL DEFAULT 0, dispatched_action_ids_json TEXT, interviewer_response_md TEXT, model_id TEXT, ai_call_id TEXT, ai_tier TEXT, ai_cost_usd REAL, ai_duration_ms REAL, ai_tokens_in INTEGER, ai_tokens_out INTEGER, sources_json TEXT, clarify_questionnaire_json TEXT, view_intent TEXT, view_data_json TEXT, ai_telemetry_json TEXT, offered_widgets_json TEXT, user_text_embedding BLOB)";

    fn seeded_db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS laws; ATTACH ':memory:' AS brain; ATTACH ':memory:' AS memory; ATTACH ':memory:' AS knowledge;").unwrap();
        c.execute_batch(&format!(
            "{ACTIONS_SCHEMA}; {AGENT_ACTIONS_SCHEMA}; {RUNTIME_EVENTS_SCHEMA}; {CARDS_SCHEMA}; {LAWS_SCHEMA}; {LAW_VIOLATIONS_SCHEMA};
             {SACRED_TIMELINE_SCHEMA}; {DECISIONS_SCHEMA}; {SESSIONS_SCHEMA}; {TURNS_SCHEMA};
             CREATE TABLE memory.memories (id INTEGER PRIMARY KEY, title TEXT NOT NULL, guidance TEXT NOT NULL, type TEXT NOT NULL, rationale TEXT, validated INTEGER, superseded_by INTEGER);
             CREATE TABLE memory.embeddings (id INTEGER PRIMARY KEY AUTOINCREMENT, memory_id INTEGER NOT NULL, vector BLOB);
             CREATE TABLE interviewer_session_widgets (id INTEGER PRIMARY KEY AUTOINCREMENT, session_id TEXT, type TEXT, kind TEXT, payload_json TEXT);
             CREATE TABLE workspaces (name TEXT, root_path TEXT, vcs TEXT, is_active INTEGER);
             CREATE TABLE knowledge.skill_library (slug TEXT, name TEXT, description TEXT, enabled INTEGER);
             CREATE TABLE brain.ai_calls (id INTEGER PRIMARY KEY, tokens_in INTEGER, tokens_out INTEGER);
             CREATE TABLE brain.prompt_templates ( id INTEGER PRIMARY KEY, name TEXT NOT NULL, version INTEGER NOT NULL, template_md TEXT NOT NULL, default_tier TEXT NOT NULL, default_temperature TEXT NOT NULL, max_output_tokens INTEGER NOT NULL DEFAULT 1024, superseded_by INTEGER, created_at TEXT NOT NULL DEFAULT (datetime('now')), cross_field_rules_json TEXT, judging_criteria_json TEXT, system_md TEXT, phase_flow TEXT NOT NULL DEFAULT 'single', UNIQUE (name, version) );
             CREATE TABLE brain.prompt_output_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, grounded_refs_json TEXT, UNIQUE (prompt_template_id, field_name) );
             CREATE TABLE brain.prompt_input_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, UNIQUE (prompt_template_id, field_name) );
             INSERT INTO actions (name, description, owner_agent, steps_json, returns, kind) VALUES
               ('board-keeper.card.list', 'list cards', 'board-keeper', '[{{\"skill\":\"card-list\",\"input\":{{}},\"as\":\"cards\"}}]', '$.results.cards', 'atomic');"
        )).unwrap();
        c
    }

    #[test]
    fn requires_user_text() {
        let c = seeded_db();
        let e = interviewerTurn(&json!({}), &c).unwrap_err();
        assert_eq!(e, "interviewer-turn: user_text is required");
        let e2 = interviewerTurn(&json!({ "user_text": "" }), &c).unwrap_err();
        assert_eq!(e2, "interviewer-turn: user_text is required");
    }

    #[test]
    fn prebuilt_plan_must_be_non_empty_array() {
        let c = seeded_db();
        let e = interviewerTurn(&json!({ "user_text": "do it", "prebuilt_plan": [] }), &c).unwrap_err();
        assert_eq!(e, "interviewer-turn: prebuilt_plan (must be non-empty array when set) is required");
    }

    #[test]
    fn prebuilt_plan_entries_require_action() {
        let c = seeded_db();
        let e = interviewerTurn(&json!({ "user_text": "do it", "prebuilt_plan": [{ "args": {} }] }), &c).unwrap_err();
        assert_eq!(e, "interviewer-turn: prebuilt_plan[].action is required");
    }

    #[test]
    fn greeting_floor_short_circuits_with_no_ai_call() {
        let c = seeded_db();
        let r = interviewerTurn(&json!({ "user_text": "hi" }), &c).unwrap();
        assert_eq!(r["classification"]["kind"], json!("clarify"));
        assert_eq!(r["classification"]["reason"], json!("greeting-floor: canned reply, no AI call"));
        assert!(truthy(r.get("response_md")));
        assert_eq!(r["turn_index"], json!(0));

        let stored = crate::db_json::query_rows(&c, "SELECT classified_kind, interviewer_response_md FROM interviewer_turns WHERE id = ?", &[r["turn_id"].clone()]).unwrap();
        assert_eq!(stored[0]["classified_kind"], json!("clarify"));
        assert_eq!(stored[0]["interviewer_response_md"], r["response_md"]);
    }

    #[test]
    fn destructive_verb_floor_blocks_before_any_ai_call() {
        let c = seeded_db();
        let r = interviewerTurn(&json!({ "user_text": "DELETE the stale memory entries" }), &c).unwrap();
        assert_eq!(r["classification"]["kind"], json!("destructive"));
        assert!(r["response_md"].as_str().unwrap().contains("destructive"));
        // Verified against real JS (interviewer-classify + interviewer-route + interviewer-confirm-gate):
        // "DELETE the stale memory entries" hits the destructive rule but ACTION_HINTS has no
        // match for it, so suggested_actions=[] and route.plan=[]. interviewer-route sets
        // requires_confirmation=false whenever plan.length===0 (even though risk_tier='critical'
        // via KIND_DEFAULT_RISK). interviewer-confirm-gate then short-circuits on
        // `!requires_confirmation` and returns only { bypass: true, risk_tier }, WITHOUT a
        // hard_gate key at all — so gate.hard_gate is absent (Null), never true.
        assert_eq!(r["gate"]["hard_gate"], Value::Null);
        assert_eq!(r["gate"]["bypass"], json!(true));
        assert_eq!(r["auto_dispatched"], json!(false));
    }

    #[test]
    fn ai_chat_failure_falls_back_gracefully() {
        // Not a greeting, not destructive, not a code question — falls to the AI chat branch,
        // which always Errs in real (non-stub) execution (the deferred ai-call boundary).
        let c = seeded_db();
        let r = interviewerTurn(&json!({ "user_text": "tell me about the weather forecast systems" }), &c).unwrap();
        assert_eq!(r["classification"]["kind"], json!("clarify"));
        assert!(r["response_md"].as_str().unwrap().contains("interviewer brain unavailable"));
    }

    #[test]
    fn second_turn_increments_turn_index_in_the_same_session() {
        let c = seeded_db();
        let r1 = interviewerTurn(&json!({ "user_text": "hi", "session_id": Value::Null }), &c).unwrap();
        let sid = r1["session_id"].clone();
        let r2 = interviewerTurn(&json!({ "user_text": "hi again", "session_id": sid.clone() }), &c).unwrap();
        assert_eq!(r2["session_id"], sid);
        assert_eq!(r2["turn_index"], json!(1));
    }

    #[test]
    fn prebuilt_plan_low_risk_bypasses_and_dispatches() {
        let c = seeded_db();
        let r = interviewerTurn(&json!({
            "user_text": "bulk import",
            "prebuilt_plan": [{ "action": "board-keeper.card.list", "args": {} }],
        }), &c).unwrap();
        assert_eq!(r["classification"]["kind"], json!("curate"));
        assert_eq!(r["risk_tier"], json!("low"));
        assert_eq!(r["gate"]["bypass"], json!(true));
        assert_eq!(r["auto_dispatched"], json!(true));
        assert_eq!(r["dispatch_results"][0]["ok"], json!(true));
        assert_eq!(r["confirmed"], json!(true));
    }

    #[test]
    fn explicit_session_id_reused_when_it_exists() {
        let c = seeded_db();
        let r1 = interviewerTurn(&json!({ "user_text": "hi" }), &c).unwrap();
        let sid = r1["session_id"].as_i64().unwrap();
        let r2 = interviewerTurn(&json!({ "user_text": "hi again", "session_id": sid }), &c).unwrap();
        assert_eq!(r2["session_id"], json!(sid));
        assert_eq!(r2["turn_index"], json!(1));
    }

    #[test]
    fn nonexistent_explicit_session_id_falls_back_to_identifier_lookup() {
        let c = seeded_db();
        let r = interviewerTurn(&json!({ "user_text": "hi", "session_id": 999999, "user_identifier": "human:test" }), &c).unwrap();
        assert_ne!(r["session_id"], json!(999999));
    }

    #[test]
    fn action_slug_field_map_covers_documented_actions() {
        assert_eq!(action_slug_field("board-keeper.card.get"), Some("slug"));
        assert_eq!(action_slug_field("git-manager.contract.load"), Some("card_slug"));
        assert_eq!(action_slug_field("pl.session.start"), Some("approved_card_slug"));
        assert_eq!(action_slug_field("system.work.start"), Some("slug"));
        assert_eq!(action_slug_field("unknown.action"), None);
    }

    #[test]
    fn helper_semantics() {
        assert_eq!(js_string(&Value::Null), "null");
        assert_eq!(or_empty(&Value::Null), "");
        assert_eq!(or_empty(&json!("x")), "x");
        assert_eq!(nullish(&Value::Null, &json!("b")), json!("b"));
        assert_eq!(nullish(&json!(""), &json!("b")), json!(""));
        assert!(!truthy(Some(&Value::Null)));
        assert!(truthy(Some(&json!("x"))));
    }
}
