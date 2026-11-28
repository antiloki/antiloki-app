//! tl-author-snippets.rs — twin of skills/agents/tl/tl-author-snippets.mjs (db lane, TL-REFINE-
//! STAGE-SPLIT). NAMES ARE CONTRACT: default `tlAuthorSnippets`. The AUTHOR half of the v11
//! plan-then-author-per-file split: loads the microtask DEFINITIONS already persisted by
//! tl-plan-contract (`contracts.microtasks_json`) and runs the v11 per-file authoring engine
//! (`crate::agents_tl__helpers_refine_shared::AuthorEngine`, reused not reimplemented) over them —
//! deciding first/full/sliced retry mode, per-file authoring, the defense-in-depth anchor dry-run
//! + byte-exact grep-AC echo check, `dev-plan-spec` validation, scope expansion, coverage-gap
//! refusal, refine_history provenance, and (on a QA-fail re-author) the `impl` → `tl_approved`
//! loop-closing status flip.
//!
//! `context.db` missing → JS throws `err.contextMissing('tl-author-snippets', 'db')` — unreachable
//! here: `db: &Connection` is a required, non-nullable reference (same convention as every sibling
//! db-lane twin). JS threads `context.provider_fn` into `createAuthorEngine({..., provider_fn,
//! ...})`; `AuthorEngine::new` (refine-shared.rs) already has no `provider_fn` parameter — elided
//! there once, nothing to re-thread here (unlike tl-plan-contract.rs, which builds its own aiCall
//! input object directly and so keeps a `provider_fn` local).
//!
//! PARITY-DEFERRED (leaf only): every per-file authoring attempt dispatches through
//! `crate::ai_ai_call::aiCall` (see ai-call.rs's module doc — it always `Err`s), so
//! `AuthorEngine::author_one_file` always exhausts its 3-attempt ladder to a `failedFiles` entry —
//! BUT unlike tl-plan-contract.rs (whose single top-level `aiCall` call short-circuits the whole
//! function via `?`), that failure here is caught INSIDE the engine and surfaces as an ordinary
//! `failedFiles` result. Every deterministic surface around it — the plan-shape guards, prompt-
//! ensure, sandbox/workspace root resolution, the failure-signal gathering, the first/full/sliced
//! retry-mode decision (`decideRetryMode`/`implicateMicrotasks`), def preparation, real per-file
//! `resolve_source` filesystem reads, and `persistPartialAndThrow`'s real
//! `refine_history_json` UPDATE (a genuine, fixture-worthy db write) — all runs for real and is
//! exercised by the native tests below. Only the SUCCESS branch inside `author_one_file` (a real
//! `gate.ok` per-file patch) is structurally unreachable through the deferred boundary, same
//! disclosed shape as book-run.rs's `fac_call`/`step` and tl-plan-contract.rs's post-aiCall logic.
//!
//! Cross-module (already-converted twins, reused not reimplemented):
//! `crate::contract_contract_get::contractGet`, `crate::agents_tl__helpers_refine_shared::{
//! ensurePlanPrompts, AuthorEngine, gatherFailureSignal, buildQaFailureContext,
//! implicateMicrotasks, decideRetryMode, mergeSlicedPlan, expandScopeOrReject, coverageGaps,
//! grepAcEchoGaps, dryRunPatches}`, `crate::agents_dev_dev_plan_spec::devPlanSpecValidate`,
//! `crate::engine_workspace_root::getWorkspaceRoot`, `crate::engine_json_utils::loadJson`,
//! `crate::ai_ai_call::aiCall` (transitively, via AuthorEngine).
//!
//! `AntilokiError`/`ErrorCodes` (engine/error.mjs) are PARITY-DEFERRED at the class level (same
//! convention as every other bespoke twin) — every throw site collapses to `Err("<skill>:
//! <cause>".into())`.
//!
//! `SUBSTRATE_ROOT` (workspace-root.mjs) has no `pub` accessor — `sandbox_path` resolution inlines
//! the same `ANTILOKI_SUBSTRATE_ROOT`-env-override-else-`CARGO_MANIFEST_DIR`-parent derivation used
//! by tl-plan-contract.rs's/refine-shared.rs's private `substrate_root_local` (duplicated here, not
//! imported — private to those modules). The per-file `resolveSource` closure (source-file
//! resolution: fileState → sandbox copy → workspace original) is ported as a free fn
//! `resolve_source_impl` so it can be handed to `AuthorEngine::authorDefs` fresh at each of the
//! (mutually reachable, not mutually exclusive — a sliced escalation falls through to the full
//! branch in the SAME call) sliced/full call sites without a move conflict.
//!
//! `Date.now()` → frozen `1767225600000i64` (2026-01-01 00:00:00 UTC) per the crossing playbook's
//! frozen-clock convention, for both `plan_id` and the `refine_history` `ts` field.
// QUALITY: mixed unit — deterministic guard/decision/persistence surface (fully ported, fixture-
// reachable through the real `persistPartialAndThrow` db write) + 1 leaf deferred aiCall dispatch
// (inside the reused AuthorEngine, no fixture/frontier for that leaf).

use rusqlite::Connection;
use serde_json::{json, Map, Value};

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

// `resolveSource(file, fileState)` from createAuthorEngine: fileState (already-authored-this-run
// content) → sandbox copy (if the contract has one) → workspace original → None ("new file").
fn resolve_source_impl(
    file: &str, file_state: &std::collections::HashMap<String, String>,
    sandbox_abs_root: Option<&str>, workspace_root: &str,
) -> Option<String> {
    if let Some(v) = file_state.get(file) {
        return Some(v.clone());
    }
    if let Some(sb) = sandbox_abs_root {
        let p = std::path::Path::new(sb).join(file);
        if p.exists() {
            if let Ok(s) = std::fs::read_to_string(&p) {
                return Some(s);
            }
        }
    }
    let orig = std::path::Path::new(workspace_root).join(file);
    if orig.exists() {
        if let Ok(s) = std::fs::read_to_string(&orig) {
            return Some(s);
        }
    }
    None
}

pub fn tlAuthorSnippets(input: &Value, db: &Connection) -> Result<Value, String> {
    let contract_slug = str_qq(input.get("contract_slug"));
    if contract_slug.is_empty() {
        return Err("tl-author-snippets: contract_slug is required".to_string());
    }
    let total_max_cost_usd = if input.get("total_max_cost_usd").is_some() { js_number(input.get("total_max_cost_usd")) } else { 0.5 };
    let bypass_cache = if input.get("bypass_cache").is_some() { truthy(input.get("bypass_cache")) } else { false };

    let contract = crate::contract_contract_get::contractGet(&json!({ "slug": contract_slug.clone() }), db)?;
    if contract.is_null() {
        return Err(format!("tl-author-snippets: contract '{contract_slug}' not found"));
    }

    let prior_microtasks = crate::engine_json_utils::loadJson(&contract.get("microtasks_json").cloned().unwrap_or(Value::Null), &json!([]));
    let prior_arr: Vec<Value> = prior_microtasks.as_array().cloned().unwrap_or_default();
    if prior_arr.is_empty() {
        return Err(format!(
            "tl-author-snippets: contract '{contract_slug}' has no microtask defs — run tl.plan.contract first (authoring without a plan has nothing to author)"
        ));
    }
    // Pre-v11 plans carry no per-file defs (file + file_spec_md) — the per-file author cannot
    // work from them. Re-plan first.
    if prior_arr.iter().any(|m| m.get("file").and_then(Value::as_str).map_or(true, |s| s.is_empty())) {
        return Err(format!(
            "tl-author-snippets: contract '{contract_slug}' microtask defs lack per-file targets (pre-v11 plan shape) — re-plan via tl.plan.contract before authoring"
        ));
    }

    crate::agents_tl__helpers_refine_shared::ensurePlanPrompts(db)?; // the per-file author prompt must exist

    let sandbox_abs_root: Option<String> = contract.get("sandbox_path").and_then(Value::as_str).filter(|s| !s.is_empty())
        .map(|sp| std::path::Path::new(&substrate_root_local()).join(sp).to_string_lossy().into_owned());
    let workspace_root_v = crate::engine_workspace_root::getWorkspaceRoot(db);
    let workspace_root = workspace_root_v.as_str().unwrap_or("").to_string();

    // EPIC-QA-RETRY-CONVERGENCE: re-query the failure context in-skill.
    let signal = crate::agents_tl__helpers_refine_shared::gatherFailureSignal(db, &contract);
    let qa_failure_context = crate::agents_tl__helpers_refine_shared::buildQaFailureContext(db, &contract, &signal);
    let refine_history = crate::engine_json_utils::loadJson(&contract.get("refine_history_json").cloned().unwrap_or(Value::Null), &json!([]));
    let refine_history_arr: Vec<Value> = refine_history.as_array().cloned().unwrap_or_default();
    // 'planned' + 'partial_authoring' entries are bookkeeping, not completed authoring attempts —
    // exclude both from the attempt counter.
    let real_history: Vec<Value> = refine_history_arr.iter()
        .filter(|e| {
            let k = e.get("kind").and_then(Value::as_str);
            k != Some("partial_authoring") && k != Some("planned")
        })
        .cloned().collect();
    // contract-get strips the _json suffix while parsing: the parsed array is contract.acceptance
    // (acceptance_json stays the raw TEXT).
    let acceptance_parsed: Value = if contract.get("acceptance").map_or(false, Value::is_array) {
        contract.get("acceptance").cloned().unwrap()
    } else {
        crate::engine_json_utils::loadJson(&contract.get("acceptance_json").cloned().unwrap_or(Value::Null), &json!([]))
    };
    let is_reauthor = truthy(Some(&qa_failure_context));

    // ═══ FEAT-SLICED-RETRIES: decide HOW to re-author. Slicing only makes sense over a
    // PREVIOUSLY AUTHORED plan (some def carries patches) — freshly planned defs (all patches
    // empty) are a first authoring pass.
    let sliced_eligible = prior_arr.iter().any(|m| m.get("patches").and_then(Value::as_array).map_or(false, |p| !p.is_empty()));
    let mut decision: Value = json!({
        "mode": if is_reauthor { "full" } else { "first" },
        "why": if is_reauthor { "default" } else { "first-attempt" },
    });
    let mut kept_microtasks: Vec<Value> = Vec::new();
    let mut implicated_microtasks: Vec<Value> = Vec::new();
    if truthy(signal.get("run")) && sliced_eligible {
        let implication = crate::agents_tl__helpers_refine_shared::implicateMicrotasks(
            &signal.get("failing").cloned().unwrap_or(json!([])), &acceptance_parsed, &prior_microtasks,
        );
        decision = crate::agents_tl__helpers_refine_shared::decideRetryMode(&json!({
            "buildGateRed": truthy(signal.get("buildGateBlock")),
            "qaRun": signal.get("run").cloned().unwrap_or(Value::Null),
            "failingAcs": signal.get("failing").cloned().unwrap_or(json!([])),
            "totalAcs": acceptance_parsed.as_array().map(|a| a.len() as i64).unwrap_or(0),
            "implication": implication,
            "planSize": prior_arr.len() as i64,
            "refineHistory": &refine_history,
        }));
        // AC-list integrity guard: positional ac_index mapping is only valid if the acceptance
        // list hasn't changed since the QA run (tl.contract.amend can change it) — count
        // mismatch voids a sliced decision. Applied AFTER decideRetryMode so build-gate-red etc.
        // keep their more specific 'why'.
        let pass = signal.get("run").and_then(|r| r.get("acceptance_pass")).and_then(Value::as_i64).unwrap_or(0);
        let fail = signal.get("run").and_then(|r| r.get("acceptance_fail")).and_then(Value::as_i64).unwrap_or(0);
        let ac_count_at_run = pass + fail;
        let acceptance_len = acceptance_parsed.as_array().map(|a| a.len() as i64).unwrap_or(0);
        if decision.get("mode").and_then(Value::as_str) == Some("sliced") && ac_count_at_run != acceptance_len {
            decision = json!({ "mode": "full", "why": "acceptance-list-changed" });
        }
        if decision.get("mode").and_then(Value::as_str) == Some("sliced") {
            let replaced_set: std::collections::HashSet<String> = decision.get("replaced_ids")
                .and_then(Value::as_array)
                .map(|a| a.iter().map(js_string).collect())
                .unwrap_or_default();
            kept_microtasks = prior_arr.iter()
                .filter(|m| !replaced_set.contains(&js_string(m.get("microtask_id").unwrap_or(&Value::Null))))
                .cloned().collect();
            implicated_microtasks = prior_arr.iter()
                .filter(|m| replaced_set.contains(&js_string(m.get("microtask_id").unwrap_or(&Value::Null))))
                .cloned().collect();
        }
    }

    let mut engine = crate::agents_tl__helpers_refine_shared::AuthorEngine::new(
        db, contract.clone(), acceptance_parsed.clone(), signal.clone(), is_reauthor, bypass_cache,
        refine_history.clone(), "tl-author-snippets",
    );

    let mut mode = decision.get("mode").and_then(Value::as_str).unwrap_or("full").to_string();
    let mut escalated_from_sliced: Value = Value::Null;
    let mut plan: Value = Value::Null;
    let briefing_excerpt = {
        let s = js_slice(&str_qq(contract.get("briefing_md")), 800);
        if s.is_empty() { "(no briefing)".to_string() } else { s }
    };

    if mode == "sliced" {
        // Sliced needs per-file specs on the implicated microtasks (a v11 plan persists file +
        // file_spec_md). A spec-less prior plan can't be sliced through the per-file path —
        // escalate to full.
        let unsliceable = implicated_microtasks.iter().any(|m| {
            m.get("file").and_then(Value::as_str).map_or(true, |s| s.is_empty())
                || m.get("file_spec_md").and_then(Value::as_str).map_or(true, |s| js_len(s) < 80)
        });
        if unsliceable {
            escalated_from_sliced = json!("prior-plan-missing-file-specs");
            mode = "full".to_string();
            decision = json!({ "mode": "full", "why": format!("sliced-escalated: {}", js_string(&escalated_from_sliced)) });
            kept_microtasks = Vec::new();
            implicated_microtasks = Vec::new();
        }
    }

    if mode == "sliced" {
        let kept_ids_v: Vec<Value> = kept_microtasks.iter().map(|m| m.get("microtask_id").cloned().unwrap_or(Value::Null)).collect();
        let replacement_defs: Vec<Value> = implicated_microtasks.iter().map(|m| {
            let mut def: Map<String, Value> = m.as_object().cloned().unwrap_or_default();
            def.remove("patches");
            def.remove("origin");
            let files_touched = m.get("files_touched").and_then(Value::as_array).filter(|a| !a.is_empty()).cloned()
                .unwrap_or_else(|| m.get("file").filter(|f| !f.is_null()).map(|f| vec![f.clone()]).unwrap_or_default());
            def.insert("files_touched".to_string(), Value::Array(files_touched));
            Value::Object(def)
        }).collect();
        let mut plan_files: Vec<String> = Vec::new();
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        for m in &kept_microtasks {
            let fts: Vec<Value> = m.get("files_touched").and_then(Value::as_array).cloned()
                .unwrap_or_else(|| m.get("file").filter(|f| !f.is_null()).map(|f| vec![f.clone()]).unwrap_or_default());
            for f in fts {
                if let Some(s) = f.as_str() {
                    if seen.insert(s.to_string()) { plan_files.push(s.to_string()); }
                }
            }
        }
        for d in &replacement_defs {
            if let Some(s) = d.get("file").and_then(Value::as_str) {
                if seen.insert(s.to_string()) { plan_files.push(s.to_string()); }
            }
        }
        let plan_files_block = if plan_files.is_empty() { "(none)".to_string() } else { plan_files.join("\n") };

        let sb_opt = sandbox_abs_root.clone();
        let ws = workspace_root.clone();
        let authoring = engine.authorDefs(
            &json!(replacement_defs), &plan_files_block, &briefing_excerpt,
            |file, fs| resolve_source_impl(file, fs, sb_opt.as_deref(), &ws),
        );
        if authoring.get("failedFiles").and_then(Value::as_array).map_or(false, |a| !a.is_empty()) {
            engine.persistPartialAndThrow("sliced", &authoring)?;
        }
        let merge = crate::agents_tl__helpers_refine_shared::mergeSlicedPlan(
            &prior_microtasks, &json!(kept_ids_v), &json!(replacement_defs),
            &json!({
                "mode": "sliced", "attempt": real_history.len() as i64 + 1,
                "qa_run_id": signal.get("run").and_then(|r| r.get("id")).cloned().unwrap_or(Value::Null),
            }),
        );
        if !truthy(merge.get("ok")) {
            // Defensive (a per-file re-author only touches its own file, so this should be
            // unreachable) — degrade to a full re-author.
            escalated_from_sliced = merge.get("reason").cloned().unwrap_or(Value::Null);
            mode = "full".to_string();
            decision = json!({ "mode": "full", "why": format!("sliced-escalated: {}", js_string(&escalated_from_sliced)) });
            kept_microtasks = Vec::new();
        } else {
            let replaced_ids_str = decision.get("replaced_ids").and_then(Value::as_array)
                .map(|a| a.iter().map(js_string).collect::<Vec<_>>().join(", ")).unwrap_or_default();
            plan = json!({
                "plan_id": format!("plan-{}-{}", contract.get("slug").and_then(Value::as_str).unwrap_or(""), crate::clock::now_ms()),
                "card_slug": nz(contract.get("card_slug")).cloned().unwrap_or_else(|| contract.get("slug").cloned().unwrap_or(Value::Null)),
                "rationale": format!("sliced re-author of {replaced_ids_str} via per-file authoring"),
                "total_max_cost_usd": total_max_cost_usd,
                "microtasks": merge.get("merged").cloned().unwrap_or(json!([])),
                "sliced_kept_count": merge.get("kept_count").cloned().unwrap_or(json!(0)),
            });
        }
    }

    if mode != "sliced" {
        // First pass / full re-author: author EVERY def from its existing spec. Prior patches
        // (if any) are discarded — replaying them is what produced the failure (DEV is
        // deterministic).
        let defs: Vec<Value> = prior_arr.iter().map(|m| {
            let mut def: Map<String, Value> = m.as_object().cloned().unwrap_or_default();
            def.remove("patches");
            def.remove("origin");
            let files_touched = m.get("files_touched").and_then(Value::as_array).filter(|a| !a.is_empty()).cloned()
                .unwrap_or_else(|| m.get("file").filter(|f| !f.is_null()).map(|f| vec![f.clone()]).unwrap_or_default());
            def.insert("files_touched".to_string(), Value::Array(files_touched));
            Value::Object(def)
        }).collect();
        let mut plan_files: Vec<String> = Vec::new();
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        for d in &defs {
            if let Some(s) = d.get("file").and_then(Value::as_str) {
                if seen.insert(s.to_string()) { plan_files.push(s.to_string()); }
            }
        }
        let plan_files_block = if plan_files.is_empty() { "(none)".to_string() } else { plan_files.join("\n") };

        let sb_opt = sandbox_abs_root.clone();
        let ws = workspace_root.clone();
        let authoring = engine.authorDefs(
            &json!(defs), &plan_files_block, &briefing_excerpt,
            |file, fs| resolve_source_impl(file, fs, sb_opt.as_deref(), &ws),
        );
        if authoring.get("failedFiles").and_then(Value::as_array).map_or(false, |a| !a.is_empty()) {
            engine.persistPartialAndThrow(&mode, &authoring)?;
        }
        plan = json!({
            "plan_id": format!("plan-{}-{}", contract.get("slug").and_then(Value::as_str).unwrap_or(""), crate::clock::now_ms()),
            "card_slug": nz(contract.get("card_slug")).cloned().unwrap_or_else(|| contract.get("slug").cloned().unwrap_or(Value::Null)),
            "rationale": if is_reauthor {
                format!("full per-file re-author of {} microtask(s) ({})", defs.len(), decision.get("why").and_then(Value::as_str).unwrap_or(""))
            } else {
                format!("per-file authoring of the planned {} microtask(s)", defs.len())
            },
            "total_max_cost_usd": total_max_cost_usd,
            "microtasks": defs,
        });
    }

    // Final deterministic passes (defense-in-depth — the per-file gates already enforced both
    // properties during authoring): anchor dry-run over the (re-)authored patches, byte-exact
    // grep-AC echo over plan-created files.
    let authored_slice: Value = if mode == "sliced" {
        let kept_count = plan.get("sliced_kept_count").and_then(Value::as_i64).unwrap_or(0) as usize;
        let arr = plan.get("microtasks").and_then(Value::as_array).cloned().unwrap_or_default();
        Value::Array(arr.into_iter().skip(kept_count).collect())
    } else {
        plan.get("microtasks").cloned().unwrap_or(json!([]))
    };
    let unresolved_anchors = crate::agents_tl__helpers_refine_shared::dryRunPatches(&contract, &authored_slice, &workspace_root);
    let echo_gaps = crate::agents_tl__helpers_refine_shared::grepAcEchoGaps(&acceptance_parsed, &plan.get("microtasks").cloned().unwrap_or(json!([])));
    if let Some(gaps) = echo_gaps.as_array() {
        if !gaps.is_empty() {
            let joined = gaps.iter().map(|g| format!("{} missing `{}`", js_string(&g["file"]), js_string(&g["pattern"]))).collect::<Vec<_>>().join("; ");
            return Err(format!("tl-author-snippets: byte-exact grep-AC check failed after per-file authoring: {joined}"));
        }
    }

    // Validate via dev-plan-spec (single source of truth for the plan shape).
    crate::agents_dev_dev_plan_spec::devPlanSpecValidate(&json!({ "plan": &plan }), db)?;

    // FIX-TL-REFINE-MICROTASKS-UPDATES-CONTRACT-FILES-TOUCHED (#380): normally a no-op here
    // (tl-plan-contract already expanded scope at plan time) — kept as defense-in-depth for
    // plans persisted by other paths.
    let scope_result = crate::agents_tl__helpers_refine_shared::expandScopeOrReject(&plan, &contract, "tl-author-snippets")?;

    // EPIC-AUTHORED-VS-GENERATED: every net-new in-scope file must be authored (a patch) or
    // generated (a bootstrap prefix). A gap means DEV would write an empty file — refuse and
    // route back to TL re-author rather than ship a stuck contract downstream.
    let generated_paths: Vec<Value> = contract.get("generated_paths").and_then(Value::as_array).cloned().unwrap_or_default();
    let ws_for_exists = workspace_root.clone();
    let gaps = crate::agents_tl__helpers_refine_shared::coverageGaps(
        &scope_result.get("files_touched").cloned().unwrap_or(json!([])),
        &plan.get("microtasks").cloned().unwrap_or(json!([])),
        &json!(generated_paths),
        |f: &str| std::path::Path::new(&ws_for_exists).join(f).exists(),
    );
    if let Some(g) = gaps.as_array() {
        if !g.is_empty() {
            let gap_list: Vec<String> = g.iter().map(js_string).collect();
            return Err(format!(
                "tl-author-snippets: coverage gap: {} net-new file(s) in files_touched have no authoring patch and are not under generated_paths: {}. Plan a 'create' microtask for each, or declare it generated (bootstrap_cmd + generated_paths).",
                gap_list.len(), gap_list.join(", ")
            ));
        }
    }

    // FEAT-SLICED-RETRIES: stamp origin on re-authored entries (sliced entries were stamped in
    // mergeSlicedPlan; full re-authors stamp everything).
    let attempt = real_history.len() as i64 + 1;
    if is_reauthor && mode != "sliced" {
        let arr = plan.get("microtasks").and_then(Value::as_array).cloned().unwrap_or_default();
        let stamped: Vec<Value> = arr.into_iter().map(|m| {
            let mut o = m.as_object().cloned().unwrap_or_default();
            o.insert("origin".to_string(), json!({
                "mode": "full", "attempt": attempt,
                "qa_run_id": signal.get("run").and_then(|r| r.get("id")).cloned().unwrap_or(Value::Null),
            }));
            Value::Object(o)
        }).collect();
        plan["microtasks"] = json!(stamped);
    }
    let kept_ids: Vec<Value> = if mode == "sliced" {
        kept_microtasks.iter().map(|m| m.get("microtask_id").cloned().unwrap_or(Value::Null)).collect()
    } else {
        Vec::new()
    };
    let replaced_ids: Vec<Value> = if mode == "sliced" {
        let kept_count = plan.get("sliced_kept_count").and_then(Value::as_i64).unwrap_or(0) as usize;
        plan.get("microtasks").and_then(Value::as_array).cloned().unwrap_or_default()
            .into_iter().skip(kept_count).map(|m| m.get("microtask_id").cloned().unwrap_or(Value::Null)).collect()
    } else if is_reauthor {
        plan.get("microtasks").and_then(Value::as_array).cloned().unwrap_or_default()
            .into_iter().map(|m| m.get("microtask_id").cloned().unwrap_or(Value::Null)).collect()
    } else {
        Vec::new()
    };
    let failing_ac_indices: Vec<Value> = signal.get("failing").and_then(Value::as_array).cloned().unwrap_or_default()
        .into_iter().map(|f| f.get("ac_index").cloned().unwrap_or(Value::Null)).collect();
    let history_mode = if is_reauthor { mode.clone() } else { "first".to_string() };
    let trigger = if truthy(signal.get("buildGateBlock")) { "build_gate" } else if truthy(signal.get("run")) { "qa_fail" } else { "first" };
    let history_entry = json!({
        "attempt": attempt,
        "ts": crate::clock::now_iso_nodb(), // frozen clock
        "mode": &history_mode,
        "trigger": trigger,
        "qa_run_id": signal.get("run").and_then(|r| r.get("id")).cloned().unwrap_or(Value::Null),
        "kept_ids": &kept_ids,
        "replaced_ids": &replaced_ids,
        "failing_ac_indices": failing_ac_indices,
        "why": decision.get("why").cloned().unwrap_or(Value::Null),
    });
    // Consumed partial entries are dropped — the run completed.
    let mut new_history_arr: Vec<Value> = refine_history_arr.iter()
        .filter(|e| e.get("kind").and_then(Value::as_str) != Some("partial_authoring")).cloned().collect();
    new_history_arr.push(history_entry.clone());
    let new_history_json = serde_json::to_string(&Value::Array(new_history_arr)).map_err(|e| e.to_string())?;

    // Persist the authored microtasks (+ expanded files_touched when the defense-in-depth pass
    // found drift) + retry provenance. Briefing / bootstrap declarations were persisted by
    // tl-plan-contract.
    let microtasks_json = serde_json::to_string(&plan.get("microtasks").cloned().unwrap_or(json!([]))).map_err(|e| e.to_string())?;
    let expanded = truthy(scope_result.get("expanded"));
    if expanded {
        let files_touched_json = serde_json::to_string(&scope_result.get("files_touched").cloned().unwrap_or(json!([]))).map_err(|e| e.to_string())?;
        crate::db_json::run_write(
            db,
            &format!("UPDATE contracts SET microtasks_json = ?, files_touched_json = ?, refine_history_json = ?, updated_at = {} WHERE slug = ?", crate::clock::now_sql()),
            &[json!(microtasks_json), json!(files_touched_json), json!(new_history_json), json!(contract_slug.clone())],
        )?;
    } else {
        crate::db_json::run_write(
            db,
            &format!("UPDATE contracts SET microtasks_json = ?, refine_history_json = ?, updated_at = {} WHERE slug = ?", crate::clock::now_sql()),
            &[json!(microtasks_json), json!(new_history_json), json!(contract_slug.clone())],
        )?;
    }

    // FIX-QA-RETRY-LOOP-NEVER-REDISPATCHES-DEV: after a QA-fail re-author the contract sat at
    // status='impl' with a stale plan cursor, so NO rule ever sent DEV back to apply the new
    // patches — the loop was refine→refine→refine→escalate. Close it: flip back to
    // 'tl_approved' (the new patches deserve the QA #1 thinking gate again), point the cursor
    // past the kept (already-applied) microtasks, and mark the failed run's suggestions consumed
    // so dev-consume-qa-suggestions doesn't append stale patchless tasks DEV would refuse.
    let mut loop_closed = false;
    if is_reauthor && contract.get("status").and_then(Value::as_str) == Some("impl") {
        let progress = if mode == "sliced" { plan.get("sliced_kept_count").and_then(Value::as_i64).unwrap_or(0) } else { 0 };
        let qa_run_id = signal.get("run").and_then(|r| r.get("id")).cloned().unwrap_or(Value::Null);
        crate::db_json::run_write(
            db,
            &format!("UPDATE contracts SET status = 'tl_approved', dev_microtask_progress = ?, last_qa_consumed_run_id = COALESCE(?, last_qa_consumed_run_id), updated_at = {} WHERE slug = ?", crate::clock::now_sql()),
            &[json!(progress), qa_run_id, json!(contract_slug.clone())],
        )?;
        loop_closed = true;
    }

    Ok(json!({
        "contract_slug": contract_slug,
        "microtask_count": plan.get("microtasks").and_then(Value::as_array).map(|a| a.len() as i64).unwrap_or(0),
        "plan": plan,
        "ai_call_id": engine.getLastAuthorCallId(),
        "persisted": true,
        "scope_expanded": expanded,
        "files_touched": scope_result.get("files_touched").cloned().unwrap_or(json!([])),
        "new_paths_added": scope_result.get("newPathsAdded").cloned().unwrap_or(json!([])),
        "bootstrap_cmd": nz(contract.get("bootstrap_cmd")).cloned().unwrap_or(Value::Null),
        "generated_paths": generated_paths,
        "unresolved_anchors": unresolved_anchors,
        "retry_mode": history_mode,
        "retry_why": decision.get("why").cloned().unwrap_or(Value::Null),
        "kept_ids": kept_ids,
        "replaced_ids": replaced_ids,
        "loop_closed": loop_closed,
        "escalated_from_sliced": escalated_from_sliced,
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
                failure_kind TEXT, dev_microtask_progress INTEGER, last_qa_consumed_run_id INTEGER,
                updated_at TEXT
            );
             CREATE TABLE qa_runs (id INTEGER PRIMARY KEY AUTOINCREMENT, contract_slug TEXT, feedback_md TEXT, acceptance_fail INTEGER, acceptance_pass INTEGER, all_passed INTEGER);
             CREATE TABLE qa_run_results (id INTEGER PRIMARY KEY AUTOINCREMENT, qa_run_id INTEGER, ac_index INTEGER, kind TEXT, evidence_md TEXT, passed INTEGER);
             CREATE TABLE brain.prompt_templates ( id INTEGER PRIMARY KEY, name TEXT NOT NULL, version INTEGER NOT NULL, template_md TEXT NOT NULL, default_tier TEXT NOT NULL, default_temperature TEXT NOT NULL, max_output_tokens INTEGER NOT NULL DEFAULT 1024, superseded_by INTEGER, created_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')), cross_field_rules_json TEXT, judging_criteria_json TEXT, system_md TEXT, phase_flow TEXT NOT NULL DEFAULT 'single', UNIQUE (name, version) );
             CREATE TABLE brain.prompt_output_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, grounded_refs_json TEXT, UNIQUE (prompt_template_id, field_name) );
             CREATE TABLE brain.prompt_input_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, UNIQUE (prompt_template_id, field_name) );",
        ).unwrap();
        c
    }

    fn a_contract(c: &Connection, slug: &str, microtasks_json: &str, extra_status: Option<&str>) {
        c.execute(
            "INSERT INTO contracts (slug, files_touched_json, acceptance_json, microtasks_json, refine_history_json, status) VALUES (?, '[]', '[]', ?, '[]', ?)",
            rusqlite::params![slug, microtasks_json, extra_status.unwrap_or("tl_approved")],
        ).unwrap();
    }

    #[test]
    fn contract_slug_is_required() {
        let c = setup();
        let e = tlAuthorSnippets(&json!({}), &c).unwrap_err();
        assert_eq!(e, "tl-author-snippets: contract_slug is required");
    }

    #[test]
    fn not_found_contract_errors() {
        let c = setup();
        let e = tlAuthorSnippets(&json!({ "contract_slug": "ghost" }), &c).unwrap_err();
        assert_eq!(e, "tl-author-snippets: contract 'ghost' not found");
    }

    #[test]
    fn no_microtask_defs_errors() {
        let c = setup();
        a_contract(&c, "CT-1", "[]", None);
        let e = tlAuthorSnippets(&json!({ "contract_slug": "CT-1" }), &c).unwrap_err();
        assert!(e.contains("has no microtask defs"), "unexpected: {e}");
    }

    #[test]
    fn pre_v11_plan_shape_errors() {
        let c = setup();
        a_contract(&c, "CT-2", r#"[{"microtask_id":"m1","description":"no file field"}]"#, None);
        let e = tlAuthorSnippets(&json!({ "contract_slug": "CT-2" }), &c).unwrap_err();
        assert!(e.contains("pre-v11 plan shape"), "unexpected: {e}");
    }

    #[test]
    fn first_pass_reaches_persist_partial_and_throw_after_deferred_aicall_exhausts_the_ladder() {
        let c = setup();
        a_contract(
            &c, "CT-3",
            r#"[{"microtask_id":"m1","file":"src/foo.rs","file_spec_md":"do the thing","op_kind":"create","patches":[]}]"#,
            None,
        );
        let err = tlAuthorSnippets(&json!({ "contract_slug": "CT-3" }), &c).unwrap_err();
        assert!(err.contains("per-file authoring failed for 1 file(s)"), "unexpected: {err}");
        assert!(err.contains("src/foo.rs"), "unexpected: {err}");

        // real db write proven: the partial_authoring entry persisted before the throw.
        let hist: String = c.query_row("SELECT refine_history_json FROM contracts WHERE slug = 'CT-3'", [], |r| r.get(0)).unwrap();
        let parsed: Value = serde_json::from_str(&hist).unwrap();
        assert_eq!(parsed[0]["kind"], json!("partial_authoring"));
        assert_eq!(parsed[0]["mode"], json!("first"));

        // the prompt-ensure surface also ran for real.
        let n: i64 = c.query_row("SELECT COUNT(*) FROM brain.prompt_templates", [], |r| r.get(0)).unwrap();
        assert!(n >= 1);
    }

    #[test]
    fn reauthor_reaches_persist_partial_and_throw_with_full_mode_when_no_qa_run_signal() {
        // No qa_runs row → gatherFailureSignal.run stays null → buildQaFailureContext falls back
        // to the build/review blocks (both null here too) → qaFailureContext null → isReauthor
        // false → 'first' mode (mirrors the JS: sliced retries require a real qa_runs failure).
        let c = setup();
        a_contract(
            &c, "CT-4",
            r#"[{"microtask_id":"m1","file":"src/bar.rs","file_spec_md":"do another thing","op_kind":"create","patches":["stale"]}]"#,
            Some("impl"),
        );
        let err = tlAuthorSnippets(&json!({ "contract_slug": "CT-4" }), &c).unwrap_err();
        assert!(err.contains("per-file authoring failed for 1 file(s)"), "unexpected: {err}");
        // status stays 'impl' — loop_closed only fires on an actual re-author.
        let status: String = c.query_row("SELECT status FROM contracts WHERE slug = 'CT-4'", [], |r| r.get(0)).unwrap();
        assert_eq!(status, "impl");
    }

    #[test]
    fn resolve_source_impl_prefers_file_state_then_sandbox_then_workspace() {
        let dir = std::env::temp_dir().join(format!("antiloki-rs-test-tl-author-snippets-{}", std::process::id()));
        let ws = dir.join("ws");
        let sb = dir.join("sb");
        std::fs::create_dir_all(&ws).unwrap();
        std::fs::create_dir_all(&sb).unwrap();
        std::fs::write(ws.join("f.txt"), "workspace content").unwrap();
        std::fs::write(sb.join("f.txt"), "sandbox content").unwrap();

        let mut state = std::collections::HashMap::new();
        state.insert("f.txt".to_string(), "state content".to_string());
        assert_eq!(resolve_source_impl("f.txt", &state, Some(sb.to_str().unwrap()), ws.to_str().unwrap()), Some("state content".to_string()));

        let empty = std::collections::HashMap::new();
        assert_eq!(resolve_source_impl("f.txt", &empty, Some(sb.to_str().unwrap()), ws.to_str().unwrap()), Some("sandbox content".to_string()));
        assert_eq!(resolve_source_impl("f.txt", &empty, None, ws.to_str().unwrap()), Some("workspace content".to_string()));
        assert_eq!(resolve_source_impl("missing.txt", &empty, None, ws.to_str().unwrap()), None);

        std::fs::remove_dir_all(&dir).ok();
    }
}
