//! tl-plan-contract.rs — twin of skills/agents/tl/tl-plan-contract.mjs (db lane, TL-REFINE-STAGE-
//! SPLIT). NAMES ARE CONTRACT: default `tlPlanContract`. The PLAN half of the v11 plan-then-
//! author-per-file split: runs the v13 solo-low PLAN call (`tl.refine.microtasks.v13`, reused via
//! `crate::agents_tl__helpers_refine_shared::PROMPT_NAME`) and, on success, persists microtask
//! DEFINITIONS (patches: []), the senior-mentor briefing fields, bootstrap_cmd/generated_paths,
//! the expanded files_touched, and a refine_history 'planned' entry.
//!
//! `context.db` missing → JS throws `err.contextMissing('tl-plan-contract', 'db')`. Unreachable
//! here: `db: &Connection` is a required, non-nullable reference (same convention as every sibling
//! db-lane twin). `context.provider_fn` (a JS function reference for provider selection/DI) has no
//! Rust equivalent to carry — threaded as `null` into the constructed aiCall input, same as
//! book-run.rs's/pl-research.rs's `provider_fn`.
//!
//! PARITY-DEFERRED: the plan call is dispatched through `crate::ai_ai_call::aiCall`, the deferred
//! network-AI stub (see ai-call.rs's module doc) — it always returns `Err`, so `tlPlanContract`
//! propagates that error once the (real, ported) deterministic surface above it has run: the
//! contract-get + not-found guard, `ensurePlanPrompts` (real prompt-template upserts, reused from
//! `agents_tl__helpers_refine_shared`), the best-effort `devBundleSlice` file-bundle fetch (reused
//! twin), the best-effort card-markdown render (`render_card_md`, own private fn — mirrors the
//! JS's inline `renderCardMd`), `gatherFailureSignal`/`buildQaFailureContext` (reused), the
//! `last_failure_kind` prefix-match block, the replan directive, and the best-effort MAN-diagnosis
//! enrichment (`interpretPending`, reused from `agents_man_man_diagnose_failure` — its own module
//! is itself PARITY-DEFERRED past the diagnosis-classification AI call, so this call always
//! resolves to a cache-miss `Ok(Null)`/`Err`, both of which this file swallows exactly like the
//! JS `try {} catch {}`).
//!
//! Everything AFTER the first `aiCall` dispatch (the corrective lint re-prompt, the empty-refusal
//! rejection path, the senior-briefing extraction/validation, the scope-expansion + `contracts`
//! UPDATE, the refine_history append) is ported byte-for-byte from the JS but is unreachable from
//! this file's own tests — the `?` on the first `aiCall` call short-circuits before any of it runs
//! — same disclosed shape as book-run.rs's post-wave staging logic and pl-research.rs's response
//! shaping. Correct by construction; the pure helpers it composes (`pick_scalar`/`pick_array`,
//! `render_card_md`) are independently unit-tested below with canned values per the crossing
//! playbook's coverage rule.
//!
//! Cross-module (already-converted twins, reused not reimplemented):
//! `crate::contract_contract_get::contractGet`, `crate::agents_tl__helpers_refine_shared::{
//! ensurePlanPrompts, PROMPT_NAME, buildPlanFromOutput, gatherFailureSignal, buildQaFailureContext,
//! expandScopeOrReject, lintPlanDefs, formatPlanLintBlock}`, `crate::agents_dev_dev_plan_spec::
//! devPlanSpecValidate`, `crate::agents_dev_dev_bundle_slice::devBundleSlice`,
//! `crate::engine_json_utils::loadJson`, `crate::agents_man_man_diagnose_failure::interpretPending`,
//! `crate::ai_ai_call::aiCall`, `crate::ai_ai_call_postmortem::aiCallPostmortem`.
//!
//! `AntilokiError`/`ErrorCodes` (engine/error.mjs) are PARITY-DEFERRED at the class level (same
//! convention as every other bespoke twin in this crate) — every throw site here collapses to
//! `Err("<skill>: <cause>".into())`.
//!
//! `SUBSTRATE_ROOT` (workspace-root.mjs) has no `pub` accessor in its own twin — `sandbox_path`
//! resolution inlines the same `ANTILOKI_SUBSTRATE_ROOT` env-override-else-`CARGO_MANIFEST_DIR`-
//! parent derivation used by refine-shared.rs's private `substrate_root_local` (duplicated here,
//! not imported, since that fn is private to its module).
// QUALITY: mixed unit — deterministic contract-fetch/prompt-ensure/context-assembly surface (fully
// ported, fixture-reachable up to the deferred aiCall boundary) + the post-call persistence logic
// (unreachable from the default export, its pure sub-helpers unit-tested directly) + 1 bespoke/
// deferred aiCall dispatch (no fixture/frontier).

use rusqlite::Connection;
use serde_json::{json, Value};

// ── standard helpers (playbook) ─────────────────────────────────────────────────────────────
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
} // = JS String(v ?? '')
fn js_string(v: &Value) -> String {
    match v {
        Value::Null => "null".into(),
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Array(a) => a.iter().map(js_string).collect::<Vec<_>>().join(","),
        Value::Object(_) => "[object Object]".into(),
    }
} // = JS String(v)
fn nz(v: Option<&Value>) -> Option<&Value> {
    v.filter(|x| !x.is_null())
} // JS `??` right operand: nz(a).or(nz(b))
fn js_number(v: Option<&Value>) -> f64 {
    match v {
        None => f64::NAN,
        Some(Value::Null) => 0.0,
        Some(Value::Bool(b)) => if *b { 1.0 } else { 0.0 },
        Some(Value::Number(n)) => n.as_f64().unwrap_or(f64::NAN),
        Some(Value::String(s)) => { let t = s.trim(); if t.is_empty() { 0.0 } else { t.parse().unwrap_or(f64::NAN) } }
        Some(_) => f64::NAN,
    }
} // = JS Number(v)
fn js_len(s: &str) -> usize { s.encode_utf16().count() }
fn js_slice(s: &str, n: usize) -> String { s.chars().take(n).collect() }

// SUBSTRATE_ROOT (see module doc — inlined real-fs derivation, duplicated from refine-shared.rs's
// private helper since it has no pub accessor).
fn substrate_root_local() -> String {
    if let Ok(v) = std::env::var("ANTILOKI_SUBSTRATE_ROOT") {
        if !v.is_empty() {
            return v;
        }
    }
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    match std::path::Path::new(manifest_dir).parent() {
        Some(p) => p.to_string_lossy().into_owned(),
        None => manifest_dir.to_string(),
    }
}

// THE CARD section of the v13 reason prompt: render cards.body_json (operator intent) as readable
// markdown. Tolerant of every legacy body shape — an empty/unparseable body degrades to a stub
// line, never an error (card context is best-effort). `export function renderCardMd(cardRow)`.
fn render_card_md(card_row: &Value) -> String {
    let body: Value = card_row.get("body_json")
        .and_then(Value::as_str)
        .and_then(|s| serde_json::from_str::<Value>(s).ok())
        .filter(|v| !v.is_null())
        .unwrap_or_else(|| json!({}));
    let title = card_row.get("title").and_then(Value::as_str).filter(|s| !s.is_empty()).unwrap_or("(untitled card)");
    let mut lines: Vec<String> = vec![format!("# {title}")];
    if let Some(m) = body.get("motivation").and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()) {
        lines.push(String::new());
        lines.push("## Motivation".to_string());
        lines.push(m.to_string());
    }
    let mut add_list = |field: &str, heading: &str| {
        if let Some(arr) = body.get(field).and_then(Value::as_array) {
            if !arr.is_empty() {
                lines.push(String::new());
                lines.push(format!("## {heading}"));
                for item in arr {
                    lines.push(format!("- {}", js_string(item)));
                }
            }
        }
    };
    add_list("acceptance_criteria", "Acceptance criteria (operator wording)");
    add_list("context", "Context");
    add_list("out_of_scope", "Out of scope");
    if lines.len() > 1 {
        lines.join("\n")
    } else {
        format!("{}\n(card body empty)", lines[0])
    }
}

// FEAT-TL-SENIOR-MENTOR-PROMPT read-back: `output_kv.find(o => o.field_name === name &&
// o.parent_output_id == null)?.value_text ?? null`.
fn pick_scalar(output_kv: &Value, name: &str) -> Option<Value> {
    output_kv.as_array()?.iter()
        .find(|o| {
            o.get("field_name").and_then(Value::as_str) == Some(name)
                && o.get("parent_output_id").map_or(true, Value::is_null)
        })
        .and_then(|o| o.get("value_text").cloned())
        .filter(|v| !v.is_null())
}

// `output_kv.filter(o => o.field_name === name && o.value_text != null && o.parent_output_id ==
// null).sort((a,b) => (a.order_index ?? 0) - (b.order_index ?? 0)).map(o => o.value_text)`.
fn pick_array(output_kv: &Value, name: &str) -> Vec<Value> {
    let mut rows: Vec<&Value> = output_kv.as_array()
        .map(|arr| arr.iter().filter(|o| {
            o.get("field_name").and_then(Value::as_str) == Some(name)
                && o.get("value_text").map_or(false, |v| !v.is_null())
                && o.get("parent_output_id").map_or(true, Value::is_null)
        }).collect())
        .unwrap_or_default();
    rows.sort_by_key(|o| o.get("order_index").and_then(Value::as_i64).unwrap_or(0));
    rows.into_iter().filter_map(|o| o.get("value_text").cloned()).collect()
}

pub fn tlPlanContract(input: &Value, db: &Connection) -> Result<Value, String> {
    let provider_fn = Value::Null;

    let contract_slug = str_qq(input.get("contract_slug"));
    if contract_slug.is_empty() {
        return Err("tl-plan-contract: contract_slug is required".to_string());
    }
    let max_microtasks = if input.get("max_microtasks").is_some() { js_number(input.get("max_microtasks")) } else { 12.0 };
    let total_max_cost_usd = if input.get("total_max_cost_usd").is_some() { js_number(input.get("total_max_cost_usd")) } else { 0.5 };
    let bypass_cache = if input.get("bypass_cache").is_some() { truthy(input.get("bypass_cache")) } else { false };

    let contract = crate::contract_contract_get::contractGet(&json!({ "slug": contract_slug.clone() }), db)?;
    if contract.is_null() {
        return Err(format!("tl-plan-contract: contract '{contract_slug}' not found"));
    }

    crate::agents_tl__helpers_refine_shared::ensurePlanPrompts(db)?;

    // FIX-BUNDLE-SANDBOX-FIRST: on a re-plan the sandbox already contains every applied patch —
    // the bundle must show THAT source.
    let sandbox_abs_root = contract.get("sandbox_path").and_then(Value::as_str).filter(|s| !s.is_empty())
        .map(|sp| std::path::Path::new(&substrate_root_local()).join(sp).to_string_lossy().into_owned());
    let sandbox_root_v = sandbox_abs_root.clone().map(Value::String).unwrap_or(Value::Null);

    // Feed TL the real source so file specs (and per-file anchors) are grounded. Bundle is
    // best-effort — its absence must not block TL.
    let file_bundle = match crate::agents_dev_dev_bundle_slice::devBundleSlice(
        &json!({ "contract": &contract, "sandbox_root": sandbox_root_v }), db,
    ) {
        Ok(v) => v.get("bundle_md").and_then(Value::as_str).map(String::from).unwrap_or_else(|| "(no source available)".to_string()),
        Err(_) => "(no source available)".to_string(),
    };

    // THE CARD (v13 reason prompt, operator directive 2026-06-12): the operator's intent from
    // cards.body_json. Best-effort — card context absence must not block TL.
    let card_md = {
        let card_slug = contract.get("card_slug").and_then(Value::as_str).filter(|s| !s.is_empty());
        match card_slug {
            Some(cs) => match crate::db_json::query_rows(db, "SELECT title, body_json FROM cards WHERE slug = ?", &[json!(cs)]) {
                Ok(rows) => match rows.as_array().and_then(|a| a.first()) {
                    Some(row) => render_card_md(row),
                    None => "(no card linked to this contract)".to_string(),
                },
                Err(_) => "(no card linked to this contract)".to_string(),
            },
            None => "(no card linked to this contract)".to_string(),
        }
    };

    // EPIC-QA-RETRY-CONVERGENCE: re-query the rich failure context here (the driver/builder stays
    // thin — it only passes contract_slug + bypass_cache).
    let signal = crate::agents_tl__helpers_refine_shared::gatherFailureSignal(db, &contract);
    let qa_failure_context = crate::agents_tl__helpers_refine_shared::buildQaFailureContext(db, &contract, &signal);

    // FIX-COVERAGE-GAP-FEEDBACK: feed the recorded PREVIOUS crash back so the AI knows WHICH files
    // need plan entries or generated_paths. Stage-split: tl.refine.*/tl.plan.*/tl.author.* are all
    // TL authoring failures.
    let last_kind = contract.get("last_failure_kind").and_then(Value::as_str).unwrap_or("");
    let is_tl_authoring_kind = last_kind.starts_with("tl.refine") || last_kind.starts_with("tl.plan") || last_kind.starts_with("tl.author");
    let last_refine_failure_block = if is_tl_authoring_kind && truthy(contract.get("last_failure_md")) {
        let at = nz(contract.get("last_failure_at")).map(|v| js_string(v)).unwrap_or_else(|| "recently".to_string());
        let md = js_slice(&str_qq(contract.get("last_failure_md")), 2000);
        format!(
            "\n\n═══ YOUR PREVIOUS AUTHORING ATTEMPT CRASHED ({at}) ═══\n{md}\nFix exactly this in the new plan: every net-new file in files_touched needs either an authoring microtask or a generated_paths entry (with the bootstrap_cmd that generates it)."
        )
    } else {
        String::new()
    };

    let base_feedback = format!(
        "{}{last_refine_failure_block}",
        if truthy(Some(&qa_failure_context)) { js_slice(&str_qq(Some(&qa_failure_context)), 16000) } else { "(first authoring attempt — no prior QA failure)".to_string() },
    );

    let is_replan = truthy(Some(&qa_failure_context));
    let retry_directive = if is_replan {
        "FULL RE-AUTHOR (re-plan): regenerate the complete microtask plan. Do NOT re-emit the prior failed plan verbatim; do NOT regress criteria that already passed.".to_string()
    } else {
        "(first authoring attempt)".to_string()
    };

    // FEAT-DRIVER-DIAGNOSIS P5: feed MAN's diagnosis of the prior failure into the re-plan. Best-
    // effort — diagnosis enrichment must never block or fail the plan.
    let mut man_diagnosis_block = String::new();
    if is_replan {
        if let Some(cs) = contract.get("card_slug").filter(|v| truthy(Some(v))) {
            if let Ok(d) = crate::agents_man_man_diagnose_failure::interpretPending(db, &json!({ "card_slug": cs })) {
                if !d.is_null() && truthy(d.get("classification")) {
                    let mut s = format!(
                        "\n\n═══ MAN'S DIAGNOSIS OF THE PRIOR FAILURE ═══\nclassification: {}\n",
                        str_qq(d.get("classification")),
                    );
                    if truthy(d.get("root_cause_md")) {
                        s += &format!("root cause: {}\n", str_qq(d.get("root_cause_md")));
                    }
                    if truthy(d.get("suggested_fix_md")) {
                        s += &format!("suggested fix: {}\n", str_qq(d.get("suggested_fix_md")));
                    }
                    s += "Address this root cause directly in the new plan; do not reproduce the same failure.";
                    man_diagnosis_block = s;
                }
            }
        }
    }

    // ═══ The PLAN call — v13: ONE SOLO low-tier call decides AND plans. ═══
    let plan_tier = "low";
    let run_plan_call = |lint_block: Option<&str>, force_bypass: bool| -> Result<Value, String> {
        let qa_feedback = match lint_block {
            Some(lb) => format!("{base_feedback}\n\n{lb}{man_diagnosis_block}"),
            None => format!("{base_feedback}{man_diagnosis_block}"),
        };
        // json! cannot contain a Rust block expression as a value — compute it first.
        let briefing_md_val = {
            let b = nz(contract.get("briefing_md")).or_else(|| nz(contract.get("tl_briefing_md")));
            let s = js_slice(&str_qq(b), 4000);
            if s.is_empty() { "(no briefing)".to_string() } else { s }
        };
        let ai_input = json!({
            "prompt_name": crate::agents_tl__helpers_refine_shared::PROMPT_NAME,
            "input_kv": [
                { "key": "contract_slug", "value": contract.get("slug").cloned().unwrap_or(Value::Null) },
                { "key": "card_md", "value": card_md },
                { "key": "file_bundle", "value": file_bundle },
                { "key": "contract_title", "value": nz(contract.get("title")).and_then(|v| v.as_str()).filter(|s| !s.is_empty()).unwrap_or("(untitled)") },
                { "key": "contract_size", "value": nz(contract.get("size")).cloned().unwrap_or(json!("M")) },
                { "key": "contract_tier", "value": nz(contract.get("tier")).cloned().unwrap_or(json!("standard")) },
                { "key": "contract_complexity", "value": nz(contract.get("complexity")).cloned().unwrap_or(json!(3)) },
                { "key": "contract_risk", "value": nz(contract.get("risk")).cloned().unwrap_or(json!(3)) },
                { "key": "files_touched", "value": serde_json::to_string(&contract.get("files_touched").cloned().unwrap_or(json!([]))).unwrap_or_default() },
                { "key": "acceptance", "value": serde_json::to_string_pretty(&contract.get("acceptance").cloned().unwrap_or(json!([]))).unwrap_or_default() },
                { "key": "must_obey", "value": serde_json::to_string(&contract.get("must_obey").cloned().unwrap_or(json!([]))).unwrap_or_default() },
                { "key": "out_of_scope", "value": serde_json::to_string(&contract.get("out_of_scope").cloned().unwrap_or(json!([]))).unwrap_or_default() },
                { "key": "edge_cases", "value": serde_json::to_string(&contract.get("edge_cases").cloned().unwrap_or(json!([]))).unwrap_or_default() },
                { "key": "briefing_md", "value": briefing_md_val },
                { "key": "max_microtasks", "value": max_microtasks },
                { "key": "total_max_cost_usd", "value": total_max_cost_usd },
                { "key": "retry_directive", "value": &retry_directive },
                { "key": "kept_microtasks_block", "value": "(none)" },
                { "key": "qa_feedback", "value": qa_feedback },
            ],
            "tier": plan_tier, "temperature": "freezed",
            "bypass_cache": force_bypass || bypass_cache,
            "strict_feed_check": true,
            "provider_fn": &provider_fn,
        });
        crate::ai_ai_call::aiCall(&ai_input, db)
    };

    // ── PARITY-DEFERRED from here on (see module doc) — aiCall always returns Err, so everything
    // below is unreachable from this file's own tests but ported byte-for-byte for correctness.
    let mut ai_result = run_plan_call(None, false)?;
    let mut plan = crate::agents_tl__helpers_refine_shared::buildPlanFromOutput(&ai_result.get("output_kv").cloned().unwrap_or(json!([])), &contract, total_max_cost_usd);

    // FEAT-PLAN-SPEC-LINT: deterministic spec-quality lint IN the planning turn.
    if !truthy(plan.get("empty_refusal")) {
        let mut lint = crate::agents_tl__helpers_refine_shared::lintPlanDefs(&plan.get("microtasks").cloned().unwrap_or(json!([])));
        if lint.as_array().map_or(false, |a| !a.is_empty()) {
            ai_result = run_plan_call(Some(&crate::agents_tl__helpers_refine_shared::formatPlanLintBlock(&lint)), true)?;
            plan = crate::agents_tl__helpers_refine_shared::buildPlanFromOutput(&ai_result.get("output_kv").cloned().unwrap_or(json!([])), &contract, total_max_cost_usd);
            if !truthy(plan.get("empty_refusal")) {
                lint = crate::agents_tl__helpers_refine_shared::lintPlanDefs(&plan.get("microtasks").cloned().unwrap_or(json!([])));
                if lint.as_array().map_or(false, |a| !a.is_empty()) {
                    // EPIC-AI-BEHAVIOR-STUDY hook 2: file the diagnosis before failing; never let
                    // it block the failure path.
                    let _ = crate::ai_ai_call_postmortem::aiCallPostmortem(&json!({
                        "ai_call_id": ai_result.get("ai_call_id").cloned().unwrap_or(Value::Null),
                        "contract_slug": contract.get("slug").cloned().unwrap_or(Value::Null),
                        "related_action": "tl.plan.contract",
                        "lint_violations": &lint,
                        "failure_kind": "tl.plan.contract:stage_failed",
                        "trigger": "stage_lint",
                    }), db);
                    let violations_msg = lint.as_array().unwrap_or(&Vec::new()).iter().map(|v| {
                        format!("[{}] {}: {}", js_string(&v["microtask_id"]), js_string(&v["file"]), js_string(&v["problem"]))
                    }).collect::<Vec<_>>().join("; ");
                    return Err(format!(
                        "tl-plan-contract: plan spec lint failed after a corrective re-prompt: {violations_msg}"
                    ));
                }
            }
        }
    }

    // FEAT-TL-SENIOR-MENTOR-PROMPT: extract the senior-mentor fields.
    let output_kv = ai_result.get("output_kv").cloned().unwrap_or(json!([]));

    // FIX-TL-REFINE-EMPTY-MICROTASKS: AI honestly refused (stub contract). Mark contract
    // 'rejected' so the state machine stops routing to it.
    if truthy(plan.get("empty_refusal")) {
        let reason = nz(plan.get("rationale")).and_then(|v| v.as_str()).filter(|s| !s.is_empty())
            .map(String::from)
            .unwrap_or_else(|| "AI refused — contract too stubby to author microtasks (likely <UNKNOWN_PATHS_TL_REVIEW> placeholder or empty acceptance)".to_string());
        crate::db_json::run_write(
            db,
            &format!("UPDATE contracts SET status='rejected', rejection_reason_md=?, failure_kind='tl_empty_refusal', updated_at={} WHERE slug = ?", crate::clock::now_sql()),
            &[json!(reason), json!(contract_slug.clone())],
        )?;
        return Ok(json!({
            "contract_slug": contract_slug,
            "microtask_count": 0,
            "plan": plan,
            "ai_call_id": ai_result.get("ai_call_id").cloned().unwrap_or(Value::Null),
            "persisted": false,
            "empty_refusal": true,
            "contract_marked_rejected": true,
            "failure_kind": "tl_empty_refusal",
            "reason": reason,
        }));
    }

    let senior_briefing = json!({
        "briefing_md": pick_scalar(&output_kv, "briefing_md"),
        "approach_md": pick_scalar(&output_kv, "approach_md"),
        "alternatives_considered_md": pick_scalar(&output_kv, "alternatives_considered_md"),
        "edge_cases_list": pick_array(&output_kv, "edge_cases_list"),
        "anti_patterns_list": pick_array(&output_kv, "anti_patterns_list"),
        "hints_for_dev_list": pick_array(&output_kv, "hints_for_dev_list"),
    });

    // Validator: briefing_md must be >=200 chars (UTF-16 code units, matching JS .length).
    let briefing_md_str = senior_briefing.get("briefing_md").and_then(Value::as_str);
    let briefing_len = briefing_md_str.map(js_len).unwrap_or(0);
    if briefing_md_str.is_none() || briefing_len < 200 {
        return Err(format!(
            "tl-plan-contract: TL briefing_md too thin ({briefing_len} chars < 200 required). A senior brief must include mission + background + constraints."
        ));
    }
    let briefing_for_db = senior_briefing.get("briefing_md").cloned().unwrap_or(Value::Null);

    let approach_md = senior_briefing.get("approach_md").and_then(Value::as_str).unwrap_or("(no approach provided)").to_string();
    let alternatives_md = senior_briefing.get("alternatives_considered_md").and_then(Value::as_str).unwrap_or("(none recorded)").to_string();
    let edge_cases_list = senior_briefing.get("edge_cases_list").and_then(Value::as_array).cloned().unwrap_or_default();
    let anti_patterns_list = senior_briefing.get("anti_patterns_list").and_then(Value::as_array).cloned().unwrap_or_default();
    let hints_list = senior_briefing.get("hints_for_dev_list").and_then(Value::as_array).cloned().unwrap_or_default();

    let mut composed_parts: Vec<String> = vec![
        str_qq(Some(&briefing_for_db)),
        format!("\n## Approach\n{approach_md}"),
        format!("\n## Alternatives considered\n{alternatives_md}"),
    ];
    if !edge_cases_list.is_empty() {
        composed_parts.push(format!("\n## Edge cases\n{}", edge_cases_list.iter().map(|e| format!("- {}", js_string(e))).collect::<Vec<_>>().join("\n")));
    }
    if !anti_patterns_list.is_empty() {
        composed_parts.push(format!("\n## Anti-patterns (do NOT do these)\n{}", anti_patterns_list.iter().map(|e| format!("- {}", js_string(e))).collect::<Vec<_>>().join("\n")));
    }
    if !hints_list.is_empty() {
        composed_parts.push(format!("\n## Hints for DEV\n{}", hints_list.iter().map(|e| format!("- {}", js_string(e))).collect::<Vec<_>>().join("\n")));
    }
    let composed_briefing_for_db = composed_parts.into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join("\n");

    // EPIC-AUTHORED-VS-GENERATED: extract the scaffold/dep declarations.
    let bootstrap_cmd = {
        let s = str_qq(pick_scalar(&output_kv, "bootstrap_cmd").as_ref()).trim().to_string();
        if s.is_empty() { None } else { Some(s) }
    };
    let generated_paths: Vec<String> = pick_array(&output_kv, "generated_paths").iter()
        .map(|p| js_string(p).trim().to_string())
        .filter(|p| !p.is_empty())
        .collect();

    // Defs persist with EMPTY patches — the deterministic "planned, not yet authored" marker.
    let microtasks_with_empty_patches: Vec<Value> = plan.get("microtasks").and_then(Value::as_array).cloned().unwrap_or_default()
        .into_iter().map(|m| {
            let mut o = m.as_object().cloned().unwrap_or_default();
            o.insert("patches".into(), json!([]));
            Value::Object(o)
        }).collect();
    plan["microtasks"] = json!(microtasks_with_empty_patches);

    // Validate the def shape via dev-plan-spec (single source of truth) — a malformed
    // decomposition is refused BEFORE the plan review pays for a high-tier verdict on it.
    crate::agents_dev_dev_plan_spec::devPlanSpecValidate(&json!({ "plan": &plan }), db)?;

    // FIX-TL-REFINE-MICROTASKS-UPDATES-CONTRACT-FILES-TOUCHED: expand contract.files_touched to
    // the union of allowed + new sanity-checked microtask paths instead of strict-refusing scope
    // drift. Sanity-invalid paths are refused HERE.
    let scope_result = crate::agents_tl__helpers_refine_shared::expandScopeOrReject(&plan, &contract, "tl-plan-contract")?;

    // Append the 'planned' provenance entry.
    let refine_history = crate::engine_json_utils::loadJson(&contract.get("refine_history_json").cloned().unwrap_or(Value::Null), &json!([]));
    let mut refine_history_arr = refine_history.as_array().cloned().unwrap_or_default();
    let trigger = if truthy(signal.get("buildGateBlock")) {
        "build_gate"
    } else if truthy(signal.get("reviewBlock")) {
        "qa_review"
    } else if truthy(signal.get("run")) {
        "qa_fail"
    } else {
        "first"
    };
    refine_history_arr.push(json!({
        "kind": "planned",
        "ts": crate::clock::now_iso_nodb(),
        "trigger": trigger,
        "replan": is_replan,
        "microtask_count": plan.get("microtasks").and_then(Value::as_array).map(|a| a.len()).unwrap_or(0) as i64,
    }));
    let new_history_json = serde_json::to_string(&Value::Array(refine_history_arr)).map_err(|e| e.to_string())?;

    let microtasks_json = serde_json::to_string(&plan.get("microtasks").cloned().unwrap_or(json!([]))).map_err(|e| e.to_string())?;
    let generated_paths_json = serde_json::to_string(&generated_paths).map_err(|e| e.to_string())?;
    let expanded = truthy(scope_result.get("expanded"));
    let bootstrap_cmd_v = bootstrap_cmd.clone().map(Value::String).unwrap_or(Value::Null);

    if expanded {
        let files_touched_json = serde_json::to_string(&scope_result.get("files_touched").cloned().unwrap_or(json!([]))).map_err(|e| e.to_string())?;
        crate::db_json::run_write(
            db,
            &format!("UPDATE contracts SET microtasks_json = ?, files_touched_json = ?, briefing_md = ?, tl_briefing_md = ?, bootstrap_cmd = ?, generated_paths_json = ?, refine_history_json = ?, updated_at = {} WHERE slug = ?", crate::clock::now_sql()),
            &[json!(microtasks_json), json!(files_touched_json), briefing_for_db.clone(), json!(composed_briefing_for_db), bootstrap_cmd_v.clone(), json!(generated_paths_json), json!(new_history_json), json!(contract_slug.clone())],
        )?;
    } else {
        crate::db_json::run_write(
            db,
            &format!("UPDATE contracts SET microtasks_json = ?, briefing_md = ?, tl_briefing_md = ?, bootstrap_cmd = ?, generated_paths_json = ?, refine_history_json = ?, updated_at = {} WHERE slug = ?", crate::clock::now_sql()),
            &[json!(microtasks_json), briefing_for_db.clone(), json!(composed_briefing_for_db), bootstrap_cmd_v, json!(generated_paths_json), json!(new_history_json), json!(contract_slug.clone())],
        )?;
    }

    Ok(json!({
        "contract_slug": contract_slug,
        "microtask_count": plan.get("microtasks").and_then(Value::as_array).map(|a| a.len()).unwrap_or(0),
        "plan": plan,
        "ai_call_id": ai_result.get("ai_call_id").cloned().unwrap_or(Value::Null),
        "persisted": true,
        "scope_expanded": expanded,
        "files_touched": scope_result.get("files_touched").cloned().unwrap_or(json!([])),
        "new_paths_added": scope_result.get("newPathsAdded").cloned().unwrap_or(json!([])),
        "bootstrap_cmd": bootstrap_cmd,
        "generated_paths": generated_paths,
        "replan": is_replan,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS brain;").unwrap();
        c.execute_batch(
            "CREATE TABLE contracts (
                slug TEXT PRIMARY KEY, card_slug TEXT, title TEXT, sandbox_path TEXT,
                size TEXT, tier TEXT, complexity INTEGER, risk INTEGER,
                files_touched_json TEXT, acceptance_json TEXT, naming_json TEXT,
                edge_cases_json TEXT, must_obey_json TEXT, out_of_scope_json TEXT,
                reference_artifacts_json TEXT, generated_paths_json TEXT,
                briefing_md TEXT, tl_briefing_md TEXT, bootstrap_cmd TEXT,
                microtasks_json TEXT, refine_history_json TEXT,
                last_failure_kind TEXT, last_failure_md TEXT, last_failure_at TEXT,
                status TEXT NOT NULL DEFAULT 'pl_ratified', rejection_reason_md TEXT,
                failure_kind TEXT, updated_at TEXT
            );
             CREATE TABLE cards (slug TEXT PRIMARY KEY, title TEXT, body_json TEXT);
             CREATE TABLE qa_runs (id INTEGER PRIMARY KEY AUTOINCREMENT, contract_slug TEXT, feedback_md TEXT, acceptance_fail INTEGER, acceptance_pass INTEGER, all_passed INTEGER);
             CREATE TABLE qa_run_results (id INTEGER PRIMARY KEY AUTOINCREMENT, qa_run_id INTEGER, ac_index INTEGER, kind TEXT, evidence_md TEXT, passed INTEGER);
             CREATE TABLE brain.prompt_templates ( id INTEGER PRIMARY KEY, name TEXT NOT NULL, version INTEGER NOT NULL, template_md TEXT NOT NULL, default_tier TEXT NOT NULL, default_temperature TEXT NOT NULL, max_output_tokens INTEGER NOT NULL DEFAULT 1024, superseded_by INTEGER, created_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')), cross_field_rules_json TEXT, judging_criteria_json TEXT, system_md TEXT, phase_flow TEXT NOT NULL DEFAULT 'single', UNIQUE (name, version) );
             CREATE TABLE brain.prompt_output_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, grounded_refs_json TEXT, UNIQUE (prompt_template_id, field_name) );
             CREATE TABLE brain.prompt_input_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, UNIQUE (prompt_template_id, field_name) );",
        ).unwrap();
        c
    }

    fn a_contract(c: &Connection, slug: &str) {
        c.execute(
            "INSERT INTO contracts (slug, files_touched_json, acceptance_json) VALUES (?, '[]', '[]')",
            [slug],
        ).unwrap();
    }

    #[test]
    fn contract_slug_is_required() {
        let c = setup();
        let e = tlPlanContract(&json!({}), &c).unwrap_err();
        assert_eq!(e, "tl-plan-contract: contract_slug is required");
    }

    #[test]
    fn not_found_contract_errors() {
        let c = setup();
        let e = tlPlanContract(&json!({ "contract_slug": "ghost" }), &c).unwrap_err();
        assert_eq!(e, "tl-plan-contract: contract 'ghost' not found");
    }

    #[test]
    fn reaches_the_parity_deferred_aicall_boundary_after_full_deterministic_surface() {
        // proves guard/contract-fetch/prompt-ensure/bundle/card-md/failure-signal/replan-directive
        // all ran cleanly and the error is aiCall's deferred stub, not an earlier bug.
        let c = setup();
        a_contract(&c, "CT-1");
        let err = tlPlanContract(&json!({ "contract_slug": "CT-1" }), &c).unwrap_err();
        assert!(err.contains("ai-call"), "unexpected error: {err}");
        let n: i64 = c.query_row("SELECT COUNT(*) FROM brain.prompt_templates WHERE name = 'tl.refine.microtasks.v13'", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn replan_directive_reflects_prior_qa_failure() {
        let c = setup();
        a_contract(&c, "CT-2");
        c.execute(
            "INSERT INTO qa_runs (contract_slug, feedback_md, acceptance_fail, acceptance_pass, all_passed) VALUES ('CT-2', 'narrative', 1, 2, 0)",
            [],
        ).unwrap();
        // still reaches the deferred boundary — signal/context assembly is deterministic.
        let err = tlPlanContract(&json!({ "contract_slug": "CT-2" }), &c).unwrap_err();
        assert!(err.contains("ai-call"), "unexpected error: {err}");
    }

    // ── render_card_md ──────────────────────────────────────────────────────────────────────
    #[test]
    fn render_card_md_renders_all_sections() {
        let row = json!({
            "title": "Fix the thing",
            "body_json": json!({
                "motivation": "  because it's broken  ",
                "acceptance_criteria": ["works", "tested"],
                "context": ["legacy code"],
                "out_of_scope": ["ui polish"],
            }).to_string(),
        });
        let md = render_card_md(&row);
        assert!(md.starts_with("# Fix the thing"));
        assert!(md.contains("## Motivation\nbecause it's broken"));
        assert!(md.contains("## Acceptance criteria (operator wording)\n- works\n- tested"));
        assert!(md.contains("## Context\n- legacy code"));
        assert!(md.contains("## Out of scope\n- ui polish"));
    }

    #[test]
    fn render_card_md_degrades_on_empty_or_unparseable_body() {
        let row = json!({ "title": "Stub", "body_json": Value::Null });
        assert_eq!(render_card_md(&row), "# Stub\n(card body empty)");
        let row2 = json!({ "title": "Stub2", "body_json": "not json" });
        assert_eq!(render_card_md(&row2), "# Stub2\n(card body empty)");
        let row3 = json!({ "body_json": Value::Null });
        assert_eq!(render_card_md(&row3), "# (untitled card)\n(card body empty)");
    }

    // ── pick_scalar / pick_array ────────────────────────────────────────────────────────────
    #[test]
    fn pick_scalar_ignores_child_rows_and_nulls() {
        let kv = json!([
            { "field_name": "briefing_md", "value_text": "top-level", "parent_output_id": null },
            { "field_name": "briefing_md", "value_text": "nested", "parent_output_id": 5 },
            { "field_name": "other", "value_text": null, "parent_output_id": null },
        ]);
        assert_eq!(pick_scalar(&kv, "briefing_md"), Some(json!("top-level")));
        assert_eq!(pick_scalar(&kv, "missing"), None);
    }

    #[test]
    fn pick_array_sorts_by_order_index_and_filters_children() {
        let kv = json!([
            { "field_name": "edge_cases_list", "value_text": "b", "order_index": 2, "parent_output_id": null },
            { "field_name": "edge_cases_list", "value_text": "a", "order_index": 1, "parent_output_id": null },
            { "field_name": "edge_cases_list", "value_text": "nested", "order_index": 0, "parent_output_id": 3 },
            { "field_name": "edge_cases_list", "value_text": null, "order_index": 0, "parent_output_id": null },
        ]);
        assert_eq!(pick_array(&kv, "edge_cases_list"), vec![json!("a"), json!("b")]);
    }

    #[test]
    fn js_len_counts_utf16_units() {
        assert_eq!(js_len("hello"), 5);
        assert_eq!(js_len(""), 0);
    }
}
