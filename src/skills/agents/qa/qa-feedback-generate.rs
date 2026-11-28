//! qa-feedback-generate.rs — twin of skills/agents/qa/qa-feedback-generate.mjs (db WRITE lane).
//! NAMES ARE CONTRACT: `qaFeedbackGenerate` (default), `loadJson` (internal — not exported by the
//! JS either), `ensurePrompt` → `ensure_prompt`, `buildAmendmentPayload` → `build_amendment_payload`,
//! `defensiveWidenFilesTouched` → `defensive_widen_files_touched`, `loadScopeOffenders` →
//! `load_scope_offenders`. QA's remediation-loop generator: reads a failing qa_run + its contract +
//! DEV's finished microtasks + scope offenders, asks the LLM for a structured retry plan
//! (feedback_md + suggested_microtasks), persists it onto the qa_runs row, and is idempotent (a
//! row that already carries feedback_md+suggested_microtasks_json returns it without a fresh call).
//!
//! PARITY-DEFERRED (single call site): the `await aiCall(...)` dispatch is deferred to
//! `crate::ai_ai_call::aiCall` (see that module's doc for why network dispatch is out of scope).
//! Everything AROUND it is ported faithfully and is the deterministic surface this file's tests
//! exercise: guards (qa_run_id required, qa_run/contract existence, all_passed short-circuit,
//! idempotent-cache return), failing-AC + microtasks-done + scope-offenders assembly
//! (`loadScopeOffenders`'s regex extraction verbatim), prompt bootstrap (`ensure_prompt`, reusing
//! the already-converted `ai_prompt_find`/`ai_prompt_add` twins — same pattern as book-run.rs's
//! `ensure_faculty`), and the POST-aiCall output-shaping helpers (`build_amendment_payload`,
//! `defensive_widen_files_touched`) which are ported byte-for-byte from the JS but — like
//! book-run.rs's post-wave staging logic — are unreachable by THIS file's own fixture because they
//! run only after a successful aiCall response, which the deferred stub never produces;
//! correctness there is by inspection + covered directly with canned `output_kv`-shaped values in
//! the `#[cfg(test)]` block below (the JS reads `aiResult.output_kv` — a flat parent/child row list
//! — not a nested object, so these helpers are tested against that exact shape).
// QUALITY: mixed unit — deterministic guards/assembly/persistence (fixture-reachable) + PARITY-
// DEFERRED aiCall boundary + output-shaping helpers (unreachable past the stub, unit-tested directly)

use rusqlite::Connection;
use serde_json::{json, Map, Value};

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
fn str_qq(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(),
        Some(o) => o.to_string(),
    }
}
fn nz(v: Option<&Value>) -> Option<&Value> {
    v.filter(|x| !x.is_null())
}
fn first(rows: &Value) -> Option<Value> {
    rows.as_array().and_then(|a| a.first()).cloned()
}

const PROMPT_NAME: &str = "qa.feedback.remediate.v4";

// The template/output-fields are transcribed verbatim only for `ensure_prompt`'s bootstrap insert
// — the LLM never runs here (deferred), so their exact prose has no bearing on this twin's tested
// surface, but the SHAPE (name + max_output_tokens + output_fields) must match so a real prompt
// bootstrap performed by this twin is byte-identical to the JS's for db_after comparison.
const PROMPT_TEMPLATE_MD: &str = r#"You are QA. A contract just failed. You are the THINKER; DEV is a pure EXECUTOR who acts only on the microtasks you produce. You MUST decide the retry plan now.

Your job:
  1. Read goal + failing acceptance + scope offenders + what DEV already wrote.
  2. Diagnose whether the failure is fixable WITHIN the contract scope (DEV can do it) or needs a SCOPE CHANGE (TL must amend the contract first).
  3. Emit a structured plan.

Microtask kinds — pick precisely:
  - code        : DEV edits source files INSIDE files_touched
  - test        : DEV adds/fixes tests INSIDE files_touched
  - migration   : DEV writes a new migration file
  - docs        : DEV writes docs INSIDE files_touched
  - contract-amendment : SCOPE PROBLEM — TL must amend the contract before DEV can retry.
                        Use this when the fix requires editing a file NOT in files_touched, or
                        when an acceptance criterion is missing/wrong, or when the goal needs
                        refinement. Each amendment microtask MUST include amendment_type +
                        amendment_payload_json.

amendment_type values + their REQUIRED scalar fields:
  - widen-files-touched      : amendment_paths_csv = "src/foo.ts,src/bar.ts"
  - add-acceptance-criterion : amendment_text = "X should also Y" + amendment_ac_kind = "behavioral|file_exists|grep"
  - refine-goal              : amendment_text = "<new goal_md>"
  - add-microtask-step       : amendment_microtask_intent = "...", amendment_microtask_kind = "code|test|...", amendment_microtask_hint = "..."

═══ CONTRACT GOAL ═══
{{goal_md}}

═══ BRIEFING (full context) ═══
{{briefing_md}}

═══ MICROTASKS DEV ALREADY FINISHED ═══
{{microtasks_done}}

═══ FAILING ACCEPTANCE ITEMS ═══
{{failing_ac}}

═══ SCOPE OFFENDERS (DEV touched a file NOT in files_touched) ═══
{{scope_offenders}}

Return JSON of this exact shape (all values are PLAIN STRINGS — never JSON-encoded):
{
  "feedback_md": "<≤200 word narrative naming what's wrong and what the plan does>",
  "suggested_microtasks": [
    {
      "microtask_id": "<kebab-id>",
      "intent": "<imperative single sentence>",
      "kind": "code|migration|test|docs|contract-amendment",
      "hint": "<concrete pointer e.g. file path + what to change>",
      "amendment_type": "<widen-files-touched|add-acceptance-criterion|refine-goal|add-microtask-step — only when kind=contract-amendment>",
      "amendment_paths_csv": "<comma-separated paths — only for widen-files-touched>",
      "amendment_text": "<the new text — only for add-acceptance-criterion or refine-goal>",
      "amendment_ac_kind": "<behavioral|file_exists|grep — only for add-acceptance-criterion>",
      "amendment_microtask_intent": "<single sentence — only for add-microtask-step>",
      "amendment_microtask_kind": "<code|test|migration|docs — only for add-microtask-step>",
      "amendment_microtask_hint": "<concrete pointer — only for add-microtask-step>"
    }
  ],
  "escalation_reason": "<set ONLY if no microtask can fix this (e.g. flaky external infra). Otherwise omit. If you set this, suggested_microtasks MAY be empty.>"
}

Hard rules:
  - GROUND EVERY CLAIM IN THE EVIDENCE. Each diagnostic statement in feedback_md
    MUST quote or cite the specific text in FAILING ACCEPTANCE ITEMS above (the
    real build/test/tsc output). Name the file, the error code, the missing
    symbol — whatever the evidence actually says.
  - NO SPECULATION when evidence is present. Do NOT write "likely", "probably",
    "the likely issues are", "might be", or list hypothetical causes. If the
    evidence shows the error, state the error. Guessing causes the same generic
    feedback every retry and the loop never converges — that is the failure mode
    this rule exists to kill.
  - IF THE EVIDENCE IS EMPTY OR DOES NOT EXPLAIN THE FAILURE: do NOT invent a
    speculative remediation. Set escalation_reason (e.g. "evidence insufficient:
    AC#N failed but captured output is empty/uninformative — needs operator
    eyes") and leave suggested_microtasks empty. An honest escalate beats a
    confident-but-wrong microtask set that re-fails identically.
  - If scope_offenders is non-empty, emit a contract-amendment microtask with amendment_type=widen-files-touched and amendment_paths_csv listing every offender.
  - If a failing AC says "file X should exist" and X is outside files_touched, emit widen-files-touched first, then a follow-up code microtask.
  - NEVER return both an empty suggested_microtasks AND no escalation_reason. That is the one forbidden output.

═══ EXAMPLE 1 — grounded contract-amendment (evidence quoted verbatim) ═══
Given a failing AC whose evidence reads "TS2307: Cannot find module './dto/turn-response.dto' — src/api/turn.controller.ts(4,28)" and files_touched does not contain the DTO file:
{
  "feedback_md": "AC#0 failed with \"TS2307: Cannot find module './dto/turn-response.dto'\" at src/api/turn.controller.ts(4,28). The controller imports a DTO that was never created, and src/api/dto/turn-response.dto.ts is outside files_touched — DEV cannot create it without a scope change. Plan: widen scope to include the DTO, then create it with the response shape the controller expects.",
  "suggested_microtasks": [
    { "microtask_id": "widen-for-dto", "intent": "Widen files_touched to include the missing DTO file", "kind": "contract-amendment", "hint": "TS2307 names the unresolved module path", "amendment_type": "widen-files-touched", "amendment_paths_csv": "src/api/dto/turn-response.dto.ts" },
    { "microtask_id": "create-dto", "intent": "Create src/api/dto/turn-response.dto.ts exporting the TurnResponse class the controller imports", "kind": "code", "hint": "src/api/turn.controller.ts line 4 shows the expected import shape" }
  ]
}

═══ EXAMPLE 2 — honest escalation (evidence empty/uninformative) ═══
Given a failing AC whose evidence is "(no evidence)" or empty:
{
  "feedback_md": "AC#2 (behavioral) failed but the captured evidence is empty — there is no build/test output to diagnose from. Inventing a remediation would re-fail identically, so this escalates to an operator instead.",
  "suggested_microtasks": [],
  "escalation_reason": "evidence insufficient: AC#2 failed but captured output is empty/uninformative — needs operator eyes"
}"#;

fn prompt_output_fields() -> Value {
    json!([
        { "field_name": "feedback_md", "field_type": "string", "max_length": 2400 },
        { "field_name": "suggested_microtasks", "field_type": "array_of_object", "required": false, "description": "each: {microtask_id, intent, kind, hint, amendment_type?, amendment_payload_json?}. Empty allowed only when escalation_reason is set." },
        { "field_name": "escalation_reason", "field_type": "string", "required": false, "description": "optional — set when no retry can fix this" },
    ])
}

// `ensurePrompt(db)` — try promptFind; on miss, promptAdd then promptFind again (JS try/catch).
fn ensure_prompt(db: &Connection) -> Result<(), String> {
    if crate::ai_prompt_find::promptFind(&json!({ "name": PROMPT_NAME }), db).is_ok() {
        return Ok(());
    }
    crate::ai_prompt_add::promptAdd(
        &json!({
            "name": PROMPT_NAME,
            "template_md": PROMPT_TEMPLATE_MD,
            "default_tier": "mid",
            "default_temperature": "cold",
            "max_output_tokens": 1500,
            "output_fields": prompt_output_fields(),
        }),
        db,
    )?;
    crate::ai_prompt_find::promptFind(&json!({ "name": PROMPT_NAME }), db)?;
    Ok(())
}

// `loadJson(s, fallback)`: falsy s → fallback; JSON.parse then Array?arr:fallback; parse error →
// fallback. Only ever called here with `fallback = []`.
fn load_json(s: Option<&Value>) -> Vec<Value> {
    match s {
        Some(Value::String(x)) if !x.is_empty() => match serde_json::from_str::<Value>(x) {
            Ok(Value::Array(a)) => a,
            _ => Vec::new(),
        },
        _ => Vec::new(),
    }
}

// `loadScopeOffenders(db, qa_run_id, files_touched_json)` — scans failing kind='scope' evidence
// for file-path-looking tokens, returns the ones NOT already in files_touched. `try {} catch {
// return [] }` in JS wraps the whole query — mirrored by treating a query error as empty (best
// effort, never blocks the feedback generation it feeds into).
fn load_scope_offenders(db: &Connection, qa_run_id: i64, files_touched_json: Option<&Value>) -> Vec<String> {
    let rows = match crate::db_json::query_rows(
        db,
        "SELECT evidence_md FROM qa_run_results WHERE qa_run_id = ? AND passed = 0 AND kind = 'scope'",
        &[json!(qa_run_id)],
    ) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let re = RE.get_or_init(|| {
        regex::Regex::new(r"[\w./\\-]+\.(?:ts|tsx|js|mjs|cjs|json|md|sql|py|go|rs|css|html)\b").unwrap()
    });
    let mut offenders: Vec<String> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let empty: Vec<Value> = Vec::new();
    for r in rows.as_array().unwrap_or(&empty) {
        let ev = js_string(nz(r.get("evidence_md")).unwrap_or(&json!("")));
        for m in re.find_iter(&ev) {
            let s = m.as_str().to_string();
            if seen.insert(s.clone()) {
                offenders.push(s);
            }
        }
    }
    let known: std::collections::HashSet<String> = load_json(files_touched_json)
        .into_iter()
        .filter_map(|v| v.as_str().map(|s| s.to_string()))
        .collect();
    offenders.into_iter().filter(|p| !known.contains(p)).collect()
}

fn defensive_widen_files_touched(offenders: &[String]) -> Value {
    json!({
        "microtask_id": "qa-defensive-widen-files-touched",
        "intent": "QA detected out-of-scope edits but the LLM did not propose a remediation plan; widen the contract scope to include them so DEV retry can land cleanly",
        "kind": "contract-amendment",
        "hint": format!("Widen files_touched_json to include: {}", offenders.join(", ")),
        "amendment_type": "widen-files-touched",
        "amendment_payload": { "paths": offenders },
        "consumed_at": Value::Null,
    })
}

fn build_amendment_payload(amendment_type: &str, cell: &dyn Fn(&str) -> Option<Value>) -> Value {
    match amendment_type {
        "widen-files-touched" => {
            let csv = cell("amendment_paths_csv");
            let paths: Vec<Value> = match csv.as_ref().and_then(Value::as_str) {
                Some(s) => s.split(',').map(|x| x.trim()).filter(|x| !x.is_empty()).map(|x| json!(x)).collect(),
                None => Vec::new(),
            };
            json!({ "paths": paths })
        }
        "add-acceptance-criterion" => {
            json!({
                "text": cell("amendment_text").unwrap_or(Value::Null),
                "ac_kind": cell("amendment_ac_kind").unwrap_or_else(|| json!("behavioral")),
            })
        }
        "refine-goal" => json!({ "new_goal_md": cell("amendment_text").unwrap_or(Value::Null) }),
        "add-microtask-step" => json!({
            "microtask": {
                "microtask_id": cell("microtask_id").unwrap_or(Value::Null),
                "intent": cell("amendment_microtask_intent").unwrap_or(Value::Null),
                "kind": cell("amendment_microtask_kind").unwrap_or_else(|| json!("code")),
                "hint": cell("amendment_microtask_hint").unwrap_or(Value::Null),
            }
        }),
        _ => json!({}),
    }
}

pub fn qaFeedbackGenerate(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS: `const { db, provider_fn = null } = context ?? {}; if (!db) throw err.contextMissing`
    // — unreachable here (db is never absent in the Rust signature).
    let qa_run_id_v = nz(input.get("qa_run_id"));
    if qa_run_id_v.map_or(true, |v| str_qq(Some(v)).is_empty()) {
        return Err("qa-feedback-generate: qa_run_id is required".to_string());
    }
    let qa_run_id = qa_run_id_v.and_then(Value::as_i64).unwrap_or_else(|| {
        str_qq(qa_run_id_v).parse::<i64>().unwrap_or(0)
    });

    let qa_run_rows = crate::db_json::query_rows(
        db,
        "SELECT id, contract_slug, all_passed, feedback_md, suggested_microtasks_json, escalation_reason FROM qa_runs WHERE id = ?",
        &[json!(qa_run_id)],
    )?;
    let qa_run = match first(&qa_run_rows) {
        Some(r) => r,
        None => return Err(format!("qa-feedback-generate: qa_run {qa_run_id} not found")),
    };
    let all_passed = qa_run.get("all_passed").and_then(Value::as_i64).unwrap_or(0);
    if all_passed == 1 {
        return Err(format!("qa-feedback-generate: qa_run {qa_run_id} passed — no feedback needed"));
    }

    // Idempotency: already has feedback → return it without a fresh AI call.
    let existing_feedback = nz(qa_run.get("feedback_md")).filter(|v| !str_qq(Some(v)).is_empty());
    let existing_mt = nz(qa_run.get("suggested_microtasks_json")).filter(|v| !str_qq(Some(v)).is_empty());
    if let (Some(fb), Some(mt)) = (existing_feedback, existing_mt) {
        let mut out = Map::new();
        out.insert("qa_run_id".into(), json!(qa_run_id));
        out.insert("cached".into(), json!(true));
        out.insert("feedback_md".into(), fb.clone());
        out.insert("suggested_microtasks".into(), json!(load_json(Some(mt))));
        return Ok(Value::Object(out));
    }

    let contract_slug = js_string(qa_run.get("contract_slug").unwrap_or(&Value::Null));
    let contract_rows = crate::db_json::query_rows(
        db,
        "SELECT slug, goal_md, briefing_md, microtasks_json, dev_microtask_progress, files_touched_json FROM contracts WHERE slug = ?",
        &[json!(contract_slug)],
    )?;
    let contract = match first(&contract_rows) {
        Some(c) => c,
        None => return Err(format!("qa-feedback-generate: contract '{contract_slug}' not found")),
    };

    let failing_ac_rows = crate::db_json::query_rows(
        db,
        "SELECT ac_index, kind, evidence_md FROM qa_run_results WHERE qa_run_id = ? AND passed = 0 ORDER BY ac_index",
        &[json!(qa_run_id)],
    )?;
    let empty: Vec<Value> = Vec::new();
    let failing_ac = failing_ac_rows.as_array().cloned().unwrap_or(empty.clone());
    let failing_ac_lines = if failing_ac.is_empty() {
        "(no per-AC failures recorded — likely scope offender)".to_string()
    } else {
        failing_ac
            .iter()
            .map(|r| {
                let idx = js_string(r.get("ac_index").unwrap_or(&Value::Null));
                let kind = js_string(r.get("kind").unwrap_or(&Value::Null));
                let ev = nz(r.get("evidence_md")).map(|v| js_string(v)).unwrap_or_else(|| "(no evidence)".to_string());
                format!("- AC#{idx} ({kind}): {ev}")
            })
            .collect::<Vec<_>>()
            .join("\n")
    };

    let all_microtasks = load_json(contract.get("microtasks_json"));
    let done_count = contract.get("dev_microtask_progress").and_then(Value::as_i64).unwrap_or(0).max(0) as usize;
    let microtasks_done: Vec<Value> = all_microtasks.into_iter().take(done_count).collect();
    let microtasks_done_str = if microtasks_done.is_empty() {
        "(none yet — DEV did no work before this QA run)".to_string()
    } else {
        microtasks_done
            .iter()
            .enumerate()
            .map(|(i, m)| {
                let id = nz(m.get("microtask_id")).map(|v| js_string(v)).unwrap_or_else(|| "?".to_string());
                let intent = nz(m.get("intent")).map(|v| js_string(v)).unwrap_or_else(|| "(no intent)".to_string());
                format!("{}. [{id}] {intent}", i + 1)
            })
            .collect::<Vec<_>>()
            .join("\n")
    };

    let scope_offenders = load_scope_offenders(db, qa_run_id, contract.get("files_touched_json"));
    let scope_offenders_str = if scope_offenders.is_empty() {
        "(none — DEV stayed inside files_touched)".to_string()
    } else {
        scope_offenders.iter().map(|p| format!("- {p}")).collect::<Vec<_>>().join("\n")
    };

    ensure_prompt(db)?;

    let goal_md = nz(contract.get("goal_md")).map(|v| js_string(v)).unwrap_or_else(|| "(no goal)".to_string());
    let briefing_full = nz(contract.get("briefing_md")).map(|v| js_string(v)).unwrap_or_default();
    let briefing_md: String = briefing_full.chars().take(4000).collect();
    let briefing_md = if briefing_md.is_empty() { "(no briefing)".to_string() } else { briefing_md };

    let ai_input = json!({
        "prompt_name": PROMPT_NAME,
        "input_kv": [
            { "key": "goal_md", "value": goal_md },
            { "key": "briefing_md", "value": briefing_md },
            { "key": "microtasks_done", "value": microtasks_done_str },
            { "key": "failing_ac", "value": failing_ac_lines },
            { "key": "scope_offenders", "value": scope_offenders_str },
        ],
        "strict_feed_check": true,
        "provider_fn": input.get("provider_fn").cloned().unwrap_or(Value::Null),
    });

    // PARITY-DEFERRED past this point: crate::ai_ai_call::aiCall always returns Err (see its own
    // module doc). Every real invocation of this twin stops here; the shaping code below
    // (unreachable past the stub) is ported for inspection + unit-tested directly with canned
    // output_kv values, mirroring book-run.rs's precedent for its post-fac_call staging logic.
    let ai_result = crate::ai_ai_call::aiCall(&ai_input, db)?;

    let outputs = ai_result.get("output_kv").and_then(Value::as_array).cloned().unwrap_or_default();
    let shaped = shape_feedback_output(&outputs, &scope_offenders);

    crate::db_json::run_write(
        db,
        "UPDATE qa_runs SET feedback_md = ?, suggested_microtasks_json = ?, escalation_reason = ? WHERE id = ?",
        &[
            json!(shaped.feedback_md),
            json!(serde_json::to_string(&shaped.suggested).map_err(|e| e.to_string())?),
            shaped.escalation_reason.clone().map(Value::String).unwrap_or(Value::Null),
            json!(qa_run_id),
        ],
    )?;

    let mut out = Map::new();
    out.insert("qa_run_id".into(), json!(qa_run_id));
    out.insert("cached".into(), json!(false));
    out.insert("feedback_md".into(), json!(shaped.feedback_md));
    out.insert("suggested_microtasks".into(), json!(shaped.suggested));
    out.insert("escalation_reason".into(), shaped.escalation_reason.map(Value::String).unwrap_or(Value::Null));
    out.insert("ai_call_id".into(), ai_result.get("ai_call_id").cloned().unwrap_or(Value::Null));
    out.insert("cost_usd".into(), ai_result.get("cost_usd").cloned().unwrap_or(json!(0)));
    Ok(Value::Object(out))
}

struct Shaped {
    feedback_md: String,
    suggested: Vec<Value>,
    escalation_reason: Option<String>,
}

// Ported from qaFeedbackGenerate's post-aiCall body (JS lines ~243-293). `outputs` is the flat
// `ai_call.output_kv` row list (parent rows for `field_name='suggested_microtasks'` carry no
// scalar value + an `order_index`; child rows carry `parent_output_id` pointing at the parent's
// `id` + their own `field_name`/value column). Unreachable via THIS file's fixture (aiCall is
// deferred) — exercised directly in `#[cfg(test)]` with canned rows.
fn field_name_of(o: &Value) -> Option<&str> {
    o.get("field_name").and_then(Value::as_str)
}
fn is_scalar_null(o: &Value) -> bool {
    o.get("value_text").map_or(true, Value::is_null)
        && o.get("value_number").map_or(true, Value::is_null)
        && o.get("value_boolean").map_or(true, Value::is_null)
}
fn cell_for(children: &[&Value], key: &str) -> Option<Value> {
    let c = children.iter().find(|c| field_name_of(c) == Some(key))?;
    nz(c.get("value_text")).or_else(|| nz(c.get("value_number"))).or_else(|| nz(c.get("value_boolean"))).cloned()
}

fn shape_feedback_output(outputs: &[Value], scope_offenders: &[String]) -> Shaped {
    let feedback_md = outputs
        .iter()
        .find(|o: &&Value| field_name_of(o) == Some("feedback_md"))
        .and_then(|o| nz(o.get("value_text")))
        .and_then(Value::as_str)
        .map(|s| s.to_string())
        .unwrap_or_else(|| "(AI returned empty feedback)".to_string());

    let escalation_from_llm = outputs
        .iter()
        .find(|o: &&Value| field_name_of(o) == Some("escalation_reason"))
        .and_then(|o| nz(o.get("value_text")))
        .and_then(Value::as_str)
        .map(|s| s.to_string());

    let mut parent_rows: Vec<&Value> = outputs
        .iter()
        .filter(|o: &&Value| field_name_of(o) == Some("suggested_microtasks") && is_scalar_null(o))
        .collect();
    parent_rows.sort_by_key(|o| o.get("order_index").and_then(Value::as_i64).unwrap_or(0));

    let mut suggested: Vec<Value> = Vec::new();
    for parent in &parent_rows {
        let parent_id = parent.get("id").cloned();
        let children: Vec<&Value> = outputs
            .iter()
            .filter(|o: &&Value| parent_id.is_some() && o.get("parent_output_id").cloned() == parent_id)
            .collect();
        let cell = |k: &str| cell_for(&children, k);
        let intent = cell("intent");
        let Some(intent) = intent else { continue };
        let kind = cell("kind").unwrap_or_else(|| json!("code"));
        let kind_str = kind.as_str().unwrap_or("code").to_string();
        let amendment_type = cell("amendment_type");
        let mut mt = Map::new();
        mt.insert("microtask_id".into(), cell("microtask_id").unwrap_or(Value::Null));
        mt.insert("intent".into(), intent);
        mt.insert("kind".into(), kind);
        mt.insert("hint".into(), cell("hint").unwrap_or(Value::Null));
        mt.insert("amendment_type".into(), amendment_type.clone().unwrap_or(Value::Null));
        mt.insert("consumed_at".into(), Value::Null);
        if kind_str == "contract-amendment" {
            let atype = amendment_type.as_ref().and_then(Value::as_str).unwrap_or("");
            let cell_dyn: &dyn Fn(&str) -> Option<Value> = &cell;
            mt.insert("amendment_payload".into(), build_amendment_payload(atype, cell_dyn));
        }
        suggested.push(Value::Object(mt));
    }

    let escalation_reason = escalation_from_llm.filter(|s| !s.trim().is_empty()).map(|s| s.trim().to_string());
    let (suggested, escalation_reason) = if suggested.is_empty() && escalation_reason.is_none() {
        if !scope_offenders.is_empty() {
            (vec![defensive_widen_files_touched(scope_offenders)], None)
        } else {
            (
                Vec::new(),
                Some("QA could not produce a structured remediation plan (LLM returned empty suggested_microtasks with no escalation_reason).".to_string()),
            )
        }
    } else {
        (suggested, escalation_reason)
    };

    Shaped { feedback_md, suggested, escalation_reason }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        // ensure_prompt() delegates to promptAdd/promptFind, which address these three tables
        // as `brain.*` (they live in the attached `brain` schema in the real substrate) — mirror
        // that here with an in-memory ATTACH, same convention as prompt-add.rs's/prompt-find.rs's
        // own test `db()` helpers.
        c.execute_batch("ATTACH ':memory:' AS brain").unwrap();
        c.execute_batch(
            "CREATE TABLE contracts (slug TEXT PRIMARY KEY, goal_md TEXT, briefing_md TEXT, microtasks_json TEXT, dev_microtask_progress INTEGER NOT NULL DEFAULT 0, files_touched_json TEXT, last_qa_consumed_run_id INTEGER);
             CREATE TABLE qa_runs (id INTEGER PRIMARY KEY AUTOINCREMENT, contract_slug TEXT NOT NULL, all_passed INTEGER NOT NULL DEFAULT 0, feedback_md TEXT, suggested_microtasks_json TEXT, escalation_reason TEXT);
             CREATE TABLE qa_run_results (id INTEGER PRIMARY KEY AUTOINCREMENT, qa_run_id INTEGER NOT NULL, ac_index INTEGER, kind TEXT, passed INTEGER NOT NULL DEFAULT 0, evidence_md TEXT);
             CREATE TABLE brain.prompt_templates ( id INTEGER PRIMARY KEY, name TEXT NOT NULL, version INTEGER NOT NULL, template_md TEXT NOT NULL, default_tier TEXT NOT NULL, default_temperature TEXT NOT NULL, max_output_tokens INTEGER NOT NULL DEFAULT 1024, superseded_by INTEGER, created_at TEXT NOT NULL DEFAULT (datetime('now')), cross_field_rules_json TEXT, judging_criteria_json TEXT, system_md TEXT, phase_flow TEXT NOT NULL DEFAULT 'single', UNIQUE (name, version) );
             CREATE TABLE brain.prompt_output_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, grounded_refs_json TEXT, UNIQUE (prompt_template_id, field_name) );
             CREATE TABLE brain.prompt_input_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, UNIQUE (prompt_template_id, field_name) );",
        ).unwrap();
        c
    }

    fn a_contract(c: &Connection, slug: &str) -> () {
        c.execute(
            "INSERT INTO contracts (slug, goal_md, briefing_md, microtasks_json, dev_microtask_progress, files_touched_json) VALUES (?, 'goal', 'brief', '[{\"microtask_id\":\"mt1\",\"intent\":\"do x\"}]', 1, '[\"src/a.ts\"]')",
            [slug],
        ).unwrap();
    }

    #[test]
    fn qa_run_id_required() {
        let c = setup();
        let e = qaFeedbackGenerate(&json!({}), &c).unwrap_err();
        assert!(e.contains("qa_run_id is required"), "{e}");
    }

    #[test]
    fn qa_run_not_found() {
        let c = setup();
        let e = qaFeedbackGenerate(&json!({ "qa_run_id": 999 }), &c).unwrap_err();
        assert!(e.contains("qa_run 999 not found"), "{e}");
    }

    #[test]
    fn all_passed_short_circuits() {
        let c = setup();
        a_contract(&c, "s1");
        c.execute("INSERT INTO qa_runs (contract_slug, all_passed) VALUES ('s1', 1)", []).unwrap();
        let rid = c.last_insert_rowid();
        let e = qaFeedbackGenerate(&json!({ "qa_run_id": rid }), &c).unwrap_err();
        assert!(e.contains("passed — no feedback needed"), "{e}");
    }

    #[test]
    fn idempotent_cached_return_skips_ai_call() {
        let c = setup();
        a_contract(&c, "s1");
        c.execute(
            "INSERT INTO qa_runs (contract_slug, all_passed, feedback_md, suggested_microtasks_json) VALUES ('s1', 0, 'already diagnosed', '[{\"microtask_id\":\"mt2\",\"intent\":\"fix\"}]')",
            [],
        ).unwrap();
        let rid = c.last_insert_rowid();
        let r = qaFeedbackGenerate(&json!({ "qa_run_id": rid }), &c).unwrap();
        assert_eq!(r["cached"], json!(true));
        assert_eq!(r["feedback_md"], json!("already diagnosed"));
        assert_eq!(r["suggested_microtasks"][0]["microtask_id"], json!("mt2"));
    }

    #[test]
    fn contract_not_found_errors() {
        let c = setup();
        c.execute("INSERT INTO qa_runs (contract_slug, all_passed) VALUES ('ghost', 0)", []).unwrap();
        let rid = c.last_insert_rowid();
        let e = qaFeedbackGenerate(&json!({ "qa_run_id": rid }), &c).unwrap_err();
        assert!(e.contains("contract 'ghost' not found"), "{e}");
    }

    #[test]
    fn reaches_the_parity_deferred_aicall_boundary_after_ensuring_the_prompt() {
        let c = setup();
        a_contract(&c, "s1");
        c.execute("INSERT INTO qa_runs (contract_slug, all_passed) VALUES ('s1', 0)", []).unwrap();
        let rid = c.last_insert_rowid();
        c.execute(
            "INSERT INTO qa_run_results (qa_run_id, ac_index, kind, passed, evidence_md) VALUES (?, 0, 'behavioral', 0, 'TS2307 boom')",
            [rid],
        ).unwrap();
        let e = qaFeedbackGenerate(&json!({ "qa_run_id": rid }), &c).unwrap_err();
        assert!(e.contains("ai-call"), "unexpected error: {e}");
        let n: i64 = c.query_row("SELECT COUNT(*) FROM brain.prompt_templates WHERE name = 'qa.feedback.remediate.v4'", [], |row| row.get(0)).unwrap();
        assert_eq!(n, 1, "ensure_prompt ran before the deferred call");
    }

    // ── load_scope_offenders ─────────────────────────────────────────────
    #[test]
    fn load_scope_offenders_extracts_paths_not_in_files_touched() {
        let c = setup();
        a_contract(&c, "s1");
        c.execute("INSERT INTO qa_runs (contract_slug, all_passed) VALUES ('s1', 0)", []).unwrap();
        let rid = c.last_insert_rowid();
        c.execute(
            "INSERT INTO qa_run_results (qa_run_id, ac_index, kind, passed, evidence_md) VALUES (?, NULL, 'scope', 0, 'DEV touched src/a.ts and src/b.ts unexpectedly')",
            [rid],
        ).unwrap();
        let offenders = load_scope_offenders(&c, rid, Some(&json!(r#"["src/a.ts"]"#)));
        assert_eq!(offenders, vec!["src/b.ts".to_string()]);
    }

    #[test]
    fn load_scope_offenders_empty_on_query_error_or_no_rows() {
        let c = setup();
        a_contract(&c, "s1");
        c.execute("INSERT INTO qa_runs (contract_slug, all_passed) VALUES ('s1', 0)", []).unwrap();
        let rid = c.last_insert_rowid();
        assert_eq!(load_scope_offenders(&c, rid, None), Vec::<String>::new());
    }

    // ── shape_feedback_output (post-aiCall shaping, canned output_kv) ────
    #[test]
    fn shape_feedback_output_happy_path_with_nested_amendment() {
        let outputs = vec![
            json!({ "id": 1, "field_name": "feedback_md", "value_text": "AC#0 failed because X" }),
            json!({ "id": 2, "field_name": "suggested_microtasks", "order_index": 0 }),
            json!({ "id": 3, "parent_output_id": 2, "field_name": "microtask_id", "value_text": "mt1" }),
            json!({ "id": 4, "parent_output_id": 2, "field_name": "intent", "value_text": "widen scope" }),
            json!({ "id": 5, "parent_output_id": 2, "field_name": "kind", "value_text": "contract-amendment" }),
            json!({ "id": 6, "parent_output_id": 2, "field_name": "hint", "value_text": "TS2307" }),
            json!({ "id": 7, "parent_output_id": 2, "field_name": "amendment_type", "value_text": "widen-files-touched" }),
            json!({ "id": 8, "parent_output_id": 2, "field_name": "amendment_paths_csv", "value_text": "src/dto.ts, src/other.ts" }),
        ];
        let shaped = shape_feedback_output(&outputs, &[]);
        assert_eq!(shaped.feedback_md, "AC#0 failed because X");
        assert_eq!(shaped.suggested.len(), 1);
        assert_eq!(shaped.suggested[0]["microtask_id"], json!("mt1"));
        assert_eq!(shaped.suggested[0]["amendment_payload"]["paths"], json!(["src/dto.ts", "src/other.ts"]));
        assert_eq!(shaped.escalation_reason, None);
    }

    #[test]
    fn shape_feedback_output_no_feedback_field_falls_back_to_default_text() {
        let shaped = shape_feedback_output(&[], &[]);
        assert_eq!(shaped.feedback_md, "(AI returned empty feedback)");
    }

    #[test]
    fn shape_feedback_output_empty_suggestions_no_scope_synthesizes_escalation() {
        let outputs = vec![json!({ "id": 1, "field_name": "feedback_md", "value_text": "unclear" })];
        let shaped = shape_feedback_output(&outputs, &[]);
        assert!(shaped.suggested.is_empty());
        assert_eq!(
            shaped.escalation_reason,
            Some("QA could not produce a structured remediation plan (LLM returned empty suggested_microtasks with no escalation_reason).".to_string())
        );
    }

    #[test]
    fn shape_feedback_output_empty_suggestions_with_scope_offenders_synthesizes_defensive_widen() {
        let outputs = vec![json!({ "id": 1, "field_name": "feedback_md", "value_text": "unclear" })];
        let shaped = shape_feedback_output(&outputs, &["src/x.ts".to_string()]);
        assert_eq!(shaped.suggested.len(), 1);
        assert_eq!(shaped.suggested[0]["microtask_id"], json!("qa-defensive-widen-files-touched"));
        assert_eq!(shaped.suggested[0]["amendment_payload"]["paths"], json!(["src/x.ts"]));
        assert_eq!(shaped.escalation_reason, None);
    }

    #[test]
    fn shape_feedback_output_honors_explicit_escalation_reason_even_with_no_suggestions() {
        let outputs = vec![
            json!({ "id": 1, "field_name": "feedback_md", "value_text": "no evidence" }),
            json!({ "id": 2, "field_name": "escalation_reason", "value_text": "  evidence insufficient  " }),
        ];
        let shaped = shape_feedback_output(&outputs, &["ignored.ts".to_string()]);
        assert!(shaped.suggested.is_empty());
        assert_eq!(shaped.escalation_reason, Some("evidence insufficient".to_string()));
    }

    #[test]
    fn shape_feedback_output_skips_microtasks_missing_intent() {
        let outputs = vec![
            json!({ "id": 1, "field_name": "feedback_md", "value_text": "x" }),
            json!({ "id": 2, "field_name": "suggested_microtasks", "order_index": 0 }),
            json!({ "id": 3, "parent_output_id": 2, "field_name": "microtask_id", "value_text": "mt1" }),
            // no "intent" child row → filtered out (JS `.filter((m) => m.intent)`)
        ];
        let shaped = shape_feedback_output(&outputs, &[]);
        assert!(shaped.suggested.is_empty());
    }
}
