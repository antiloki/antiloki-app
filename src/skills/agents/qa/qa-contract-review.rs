//! qa-contract-review.rs — twin of skills/agents/qa/qa-contract-review.mjs (db lane, PARITY-
//! DEFERRED aiCall boundary). NAMES ARE CONTRACT: default `qaContractReview`. EPIC-SHIFT-LEFT-QA's
//! thinking gate: reads a contract's goal/briefing/acceptance + either the DECOMPOSITION (mode
//! 'plan', defs only) or the FULL microtasks+patches (mode 'full', default) and asks a low-tier AI
//! reviewer whether it's good enough to reach DEV/the per-file author. Writes one
//! contract_qa_reviews row — a derived artifact (like build_gate_runs/qa_runs), direct INSERT, not
//! audited.
//!
//! PARITY-DEFERRED: the review verdict itself comes from `crate::ai_ai_call::aiCall`, the always-
//! `Err` deferred stub (see that module's doc) — so this twin's `qaContractReview` always returns
//! `Err` once every guard/assembly step ahead of it has run cleanly (mirrors book-run.rs /
//! man-interpret-diagnosis.rs's shape). Everything else is ported faithfully and IS the
//! deterministic surface: the contract_slug/mode guards, `contractGet` dispatch + NOT_FOUND
//! shaping, `loadJsonArray`, the `clip` truncation-announcer, `renderMicrotasks`/`renderPlanDefs`
//! rendering, `ensurePrompt`'s find-or-register-with-drift-check (reusing the already-converted
//! `ai_prompt_find`/`ai_prompt_add` twins), and the post-call response-parsing + grounding-
//! reconciliation logic (`extract_scalar`/`extract_issues`/`reconcile_grounding`, JS
//! FIX-QA-REVIEW-GROUNDING) — ported as pure fns from the JS's inline `outputs.find`/`.filter`
//! chains, unit-tested directly with canned `output_kv` rows (the shape `aiCall` would have
//! returned on success) since the deferred stub's `Err` short-circuits `qaContractReview` before
//! that code path is reachable through the public fn itself.
// QUALITY: thin AI-gate wrapper — guards/assembly/response-parsing fully ported around the
// deferred aiCall leg; no fixture/frontier (bespoke-native, dispatcher-over-nondet-dep class).

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
fn nz(v: Option<&Value>) -> Option<&Value> {
    v.filter(|x| !x.is_null())
}

pub const PROMPT_NAME: &str = "qa.contract.review.v2";
pub const PLAN_PROMPT_NAME: &str = "qa.contract.plan.review.v2";

const PROMPT_TEMPLATE_MD: &str = r#"You are QA reviewing a CONTRACT before DEV implements it. DEV is a pure deterministic EXECUTOR: it applies the literal code patches below with NO judgment of its own. So a broken snippet, a weak plan, or an out-of-scope edit reaches production unless YOU stop it now.

Your job: decide whether this contract + its microtasks + the literal code snippets are GOOD ENOUGH to hand to DEV. Bounce it back to TL re-authoring if they are weak, wrong, out of scope, or do not cover the acceptance.

Review checklist:
  1. COVERAGE — do the microtasks together satisfy every acceptance criterion? A missing acceptance is a fail.
  2. SNIPPET SANITY — read each patch's `replacement`. Is it real, complete code (not a stub/placeholder/"// TODO")? Do anchors look like they target the right place? Obviously broken/empty/nonsense snippets are a fail.
  3. SCOPE — does every patch.file stay inside files_touched, and does the work stay inside the contract goal (nothing in out_of_scope)? An out-of-scope edit is a fail.
  4. SUFFICIENCY — are the microtasks concrete enough for a no-judgment executor? Vague intent with no real patch is a fail.

═══ CONTRACT GOAL ═══
{{goal_md}}

═══ BRIEFING (full context TL authored) ═══
{{briefing_md}}

═══ ACCEPTANCE CRITERIA (must all be covered) ═══
{{acceptance}}

═══ FILES TOUCHED (scope boundary) ═══
{{files_touched}}

═══ OUT OF SCOPE (must NOT be touched) ═══
{{out_of_scope}}

═══ MICROTASKS + LITERAL PATCHES (the code DEV will apply) ═══
{{microtasks}}

Return JSON of this exact shape (all values PLAIN STRINGS unless noted):
{
  "verdict": "pass" | "fail",
  "failure_target": "microtasks" | "contract",
  "findings_md": "<concise narrative naming what you checked and, if failing, WHY — cite the specific microtask_id / file / snippet>",
  "issues": [
    {
      "severity": "blocker" | "major" | "minor",
      "microtask_id": "<the offending microtask id, or '' if contract-level>",
      "file": "<the offending file, or '' if not file-specific>",
      "problem": "<what is wrong>",
      "fix_hint": "<concrete pointer for TL to fix it>"
    }
  ]
}

failure_target — WHERE does the fix belong? This routes the failure:
  - "microtasks": every blocker can be fixed by TL re-authoring the microtasks/patches (missing coverage, bad snippet, out-of-scope edit). The system loops back to TL automatically.
  - "contract": at least one blocker requires changing the CONTRACT ITSELF — an acceptance criterion that is impossible or self-contradictory (e.g. expecting exit code 0 from a killed long-running process), untestable, scope that is fundamentally wrong, or an incoherent goal. TL re-authoring can NEVER fix these; saying "microtasks" for them creates an infinite loop. Choosing "contract" sends your findings + fix_hints straight to the human operator.
  - On verdict='pass', use "microtasks" (the value is ignored).

Hard rules:
  - GROUND EVERY FINDING. Each issue MUST carry the microtask_id COPIED VERBATIM from the plan above, or the exact file path from the plan. The substrate MECHANICALLY DISCARDS issues citing neither — and a fail whose issues are all discarded becomes a pass. Never invent acceptance criteria: judge ONLY against the acceptance list above.
  - verdict='fail' REQUIRES at least one grounded issue. An empty issues array with verdict='fail' is forbidden.
  - If ANY issue's real fix is a change to acceptance criteria, scope, or the goal — failure_target MUST be "contract", and that issue's fix_hint must state the exact contract change needed.
  - verdict='pass' means you would be comfortable letting this reach DEV as-is. If in doubt because a snippet is unreadable or acceptance is uncovered, FAIL — a bounce to TL is cheap, a bad contract reaching DEV is not."#;

const PLAN_PROMPT_TEMPLATE_MD: &str = r#"You are QA reviewing a MICROTASK PLAN (a decomposition) before any code is authored. TL planned the contract into per-file microtask definitions; a separate obedient per-file author will write the literal code from EACH definition's file_spec_md and NOTHING ELSE. So an insufficient spec, a missing file, or a wrong decomposition becomes wrong code unless YOU stop it now — and stopping it now costs ONE review instead of N authoring calls.

Your job: decide whether this decomposition is GOOD ENOUGH to hand to the per-file author. Bounce it back to TL re-planning if it is not.

═══ ACCEPTANCE SEMANTICS — what each AC kind demands from THE PLAN ═══
  file-exists    → covered by an op_kind=create microtask for that path OR a generated_paths prefix (with the bootstrap_cmd that produces it).
  grep-contains  → the file carrying the pattern must be planned (created/modified) and its file_spec_md must demand the EXACT bytes.
  command / test → executed by the QA RUNNER in the sandbox, NOT by microtasks. The plan covers a command AC by producing the files + bootstrap_cmd the command needs to PASS (e.g. 'npm run build' needs the source files + package.json the plan creates). Do NOT demand 'execution microtasks' — they don't exist in this system, and a plan containing them is itself a defect. Only fail a command AC's coverage when the command obviously needs a file or bootstrap step the plan does not produce.
  impossible ACs → a command AC that CANNOT pass regardless of any plan (e.g. expecting exit code 0 from a timeout-killed dev server — kill = exit 124, always) is a CONTRACT defect: verdict fail, failure_target='contract', fix_hint states the exact AC change.

Review checklist:
  1. COVERAGE — per the ACCEPTANCE SEMANTICS above. An uncovered acceptance criterion or net-new file is a fail.
  2. FILE-SPEC SUFFICIENCY — read each file_spec_md as if YOU had to write the file from it alone (plus the contract constraints). Is it actionable: names, exports, signatures, constant values, behaviors? A vague spec ("implement the logic", "as appropriate") is a fail — the author cannot invent what the spec omits.
  3. OP_KIND SANITY — op_kind=create for files that do not exist yet (or are fully rewritten); op_kind=modify for edits to existing files shown in files_touched. A modify on a net-new file (nothing to modify) or a create that would clobber a file other microtasks depend on is a fail.
  4. MISSING FILES — does the plan FORGET a file the work obviously needs (an import target named in a spec but planned nowhere, a config the acceptance greps, a module the goal demands)? A missing file is a fail.
  5. JUNK EXECUTION MICROTASKS — a microtask whose intent is to RUN a command (verify/run/execute install/build/lint/dev) is a plan defect (the runner does that): fail with failure_target='microtasks' and tell TL to drop it.

═══ CONTRACT GOAL ═══
{{goal_md}}

═══ BRIEFING (full context TL authored) ═══
{{briefing_md}}

═══ ACCEPTANCE CRITERIA (must all be covered by the plan) ═══
{{acceptance}}

═══ FILES TOUCHED (scope boundary) ═══
{{files_touched}}

═══ OUT OF SCOPE (must NOT be touched) ═══
{{out_of_scope}}

═══ GENERATED (tool-produced, excluded from authoring) ═══
  bootstrap_cmd:   {{bootstrap_cmd}}
  generated_paths: {{generated_paths}}

═══ MICROTASK PLAN (definitions only — code is authored later, one file per call) ═══
{{microtasks}}

Return JSON of this exact shape (all values PLAIN STRINGS unless noted):
{
  "verdict": "pass" | "fail",
  "failure_target": "microtasks" | "contract",
  "findings_md": "<concise narrative naming what you checked and, if failing, WHY — cite the specific microtask_id / file / spec sentence>",
  "issues": [
    {
      "severity": "blocker" | "major" | "minor",
      "microtask_id": "<the offending microtask id, or '' if plan-level>",
      "file": "<the offending file, or '' if not file-specific>",
      "problem": "<what is wrong>",
      "fix_hint": "<concrete pointer for TL to fix the plan>"
    }
  ]
}

failure_target — WHERE does the fix belong? This routes the failure:
  - "microtasks": every blocker can be fixed by TL re-planning the decomposition (missing file, thin spec, wrong op_kind, uncovered acceptance). The system loops back to TL automatically.
  - "contract": at least one blocker requires changing the CONTRACT ITSELF — an acceptance criterion that is impossible or self-contradictory, untestable, scope that is fundamentally wrong, or an incoherent goal. Re-planning can NEVER fix these; choosing "contract" sends your findings + fix_hints straight to the human operator.
  - On verdict='pass', use "microtasks" (the value is ignored).

Hard rules:
  - GROUND EVERY FINDING. Each issue MUST carry the microtask_id COPIED VERBATIM from the plan above, or the exact file path from the plan. The substrate MECHANICALLY DISCARDS issues citing neither — and a fail whose issues are all discarded becomes a pass. Never invent acceptance criteria: judge ONLY against the acceptance list above.
  - verdict='fail' REQUIRES at least one grounded issue. An empty issues array with verdict='fail' is forbidden.
  - If ANY issue's real fix is a change to acceptance criteria, scope, or the goal — failure_target MUST be "contract", and that issue's fix_hint must state the exact contract change needed.
  - verdict='pass' means you would be comfortable paying for one authoring call per file from these specs as-is. If in doubt because a spec is thin or coverage is unclear, FAIL — a bounce to re-plan costs one call, N bad authoring calls cost N."#;

fn prompt_output_fields() -> Value {
    json!([
        { "field_name": "verdict", "field_type": "enum", "required": true, "enum": ["pass", "fail"] },
        { "field_name": "failure_target", "field_type": "enum", "required": true, "enum": ["microtasks", "contract"] },
        { "field_name": "findings_md", "field_type": "string", "required": true, "min_length": 20 },
        { "field_name": "issues", "field_type": "array_of_object", "required": false,
          "description": "each: {severity, microtask_id, file, problem, fix_hint}. Non-empty required when verdict=fail." },
    ])
}
fn prompt_cross_field_rules() -> Value {
    json!([
        {
            "id": "fail-requires-issue",
            "expression": "output.verdict !== 'fail' || (Array.isArray(output.issues) && output.issues.length >= 1)",
            "message": "verdict=fail requires at least one issue naming the offending microtask/file/snippet",
        },
    ])
}

// `JSON.parse(s ?? '[]')`, Array.isArray guard, `[]` on falsy/parse-failure.
fn load_json_array(s: Option<&Value>) -> Vec<Value> {
    let s = match s {
        None | Some(Value::Null) => return vec![],
        Some(Value::String(t)) if t.is_empty() => return vec![],
        Some(v) => js_string(v),
    };
    match serde_json::from_str::<Value>(&s) {
        Ok(Value::Array(a)) => a,
        _ => vec![],
    }
}

// find-or-register-with-drift-check: re-registers when the on-disk template_md differs from what
// is stored (ensurePromptCurrent pattern) — code-side template edits never reached existing DBs
// under the old find-or-only.
fn ensure_prompt(db: &Connection, name: &str, template_md: &str) -> Result<(), String> {
    if let Ok(existing) = crate::ai_prompt_find::promptFind(&json!({ "name": name }), db) {
        if existing.get("template_md").and_then(|v| v.as_str()) == Some(template_md) {
            return Ok(());
        }
    }
    crate::ai_prompt_add::promptAdd(
        &json!({
            "name": name,
            "template_md": template_md,
            "default_tier": "low",
            "default_temperature": "cold",
            "max_output_tokens": 1500,
            "output_fields": prompt_output_fields(),
            "cross_field_rules": prompt_cross_field_rules(),
        }),
        db,
    )?;
    let _ = crate::ai_prompt_find::promptFind(&json!({ "name": name }), db)?;
    Ok(())
}

// FIX-FEED-TRUNCATION: a clip now ANNOUNCES itself instead of silently amputating.
fn clip(s: &str, n: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= n {
        s.to_string()
    } else {
        let head: String = chars[..n].iter().collect();
        format!("{head}\n…[substrate: clipped at {n} chars — judge ONLY what is shown]")
    }
}

// Render the microtasks + their literal patches into a compact, readable block the reviewer can
// reason over. The patches are the real work being judged.
fn render_microtasks(microtasks: &[Value]) -> String {
    if microtasks.is_empty() {
        return "(no microtasks authored — this is itself a fail)".to_string();
    }
    microtasks
        .iter()
        .enumerate()
        .map(|(i, m)| {
            let patches: Vec<Value> = m.get("patches").and_then(|v| v.as_array()).cloned().unwrap_or_default();
            let patch_str = if patches.is_empty() {
                "    (no patches — DEV has nothing to apply)".to_string()
            } else {
                patches
                    .iter()
                    .enumerate()
                    .map(|(j, p)| {
                        let file = p.get("file").filter(|v| !v.is_null()).map(js_string).unwrap_or_else(|| "?".to_string());
                        let op = p.get("op").filter(|v| !v.is_null()).map(js_string).unwrap_or_else(|| "?".to_string());
                        let anchor_before: String = str_qq(p.get("anchor_before")).chars().take(200).collect();
                        let anchor_after: String = str_qq(p.get("anchor_after")).chars().take(200).collect();
                        let replacement: String = str_qq(p.get("replacement")).chars().take(1200).collect();
                        let replacement_indented = replacement.split('\n').map(|l| format!("        {l}")).collect::<Vec<_>>().join("\n");
                        format!(
                            "    Patch {}: file={file} op={op}\n      anchor_before: {}\n      anchor_after:  {}\n      replacement:\n{replacement_indented}",
                            j + 1,
                            serde_json::to_string(&anchor_before).unwrap_or_default(),
                            serde_json::to_string(&anchor_after).unwrap_or_default(),
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            };
            let accept = match m.get("acceptance") {
                Some(Value::Array(a)) => a.iter().map(js_string).collect::<Vec<_>>().join("; "),
                Some(v) => js_string(v),
                None => String::new(),
            };
            let microtask_id = m.get("microtask_id").filter(|v| !v.is_null()).map(js_string).unwrap_or_else(|| "?".to_string());
            let intent = m.get("intent").filter(|v| !v.is_null()).map(js_string).unwrap_or_else(|| "(no intent)".to_string());
            let files_touched = m.get("files_touched").cloned().unwrap_or_else(|| json!([]));
            format!(
                "{}. [{microtask_id}] {intent}\n   files_touched: {}\n   acceptance: {accept}\n{patch_str}",
                i + 1,
                serde_json::to_string(&files_touched).unwrap_or_default(),
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

// Plan-mode rendering: the DEFINITIONS only — id / intent / file / op_kind / file_spec_md /
// acceptance. Patches are deliberately absent (none exist yet; the review judges the
// decomposition, not code).
fn render_plan_defs(microtasks: &[Value]) -> String {
    if microtasks.is_empty() {
        return "(no microtask definitions — this is itself a fail)".to_string();
    }
    microtasks
        .iter()
        .enumerate()
        .map(|(i, m)| {
            let accept = match m.get("acceptance") {
                Some(Value::Array(a)) => a.iter().map(js_string).collect::<Vec<_>>().join("; "),
                Some(v) => js_string(v),
                None => String::new(),
            };
            let spec_src = match nz(m.get("file_spec_md")) {
                Some(v) => str_qq(Some(v)),
                None => "(no file_spec_md)".to_string(),
            };
            let spec = spec_src.split('\n').map(|l| format!("     {l}")).collect::<Vec<_>>().join("\n");
            let microtask_id = m.get("microtask_id").filter(|v| !v.is_null()).map(js_string).unwrap_or_else(|| "?".to_string());
            let intent = m.get("intent").filter(|v| !v.is_null()).map(js_string).unwrap_or_else(|| "(no intent)".to_string());
            let file = m.get("file").filter(|v| !v.is_null()).map(js_string).unwrap_or_else(|| "?".to_string());
            let op_kind = m.get("op_kind").filter(|v| !v.is_null()).map(js_string).unwrap_or_else(|| "?".to_string());
            let depends_on = m.get("depends_on").cloned().unwrap_or_else(|| json!([]));
            format!(
                "{}. [{microtask_id}] {intent}\n   file: {file}   op_kind: {op_kind}   depends_on: {}\n   acceptance: {accept}\n   file_spec_md:\n{spec}",
                i + 1,
                serde_json::to_string(&depends_on).unwrap_or_default(),
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

// ── response-parsing (fed the RESOLVED aiCall output shape — unreachable through the public
// fn while aiCall stays PARITY-DEFERRED, ported byte-for-byte from the JS `outputs.find`/`.filter`
// chains and unit-tested with canned output_kv rows below). ──────────────────────────────────
fn extract_scalar<'a>(outputs: &'a [Value], field_name: &str) -> Option<&'a str> {
    outputs
        .iter()
        .find(|o| o.get("field_name").and_then(|v| v.as_str()) == Some(field_name))
        .and_then(|o| o.get("value_text"))
        .and_then(|v| v.as_str())
}

// issues: array_of_object — parents carry field_name='issues' with NULL value cols + order_index;
// children carry the per-issue fields (mirror of qa-feedback-generate's suggested_microtasks
// decomposition).
fn extract_issues(outputs: &[Value]) -> Vec<Value> {
    let mut parents: Vec<&Value> = outputs
        .iter()
        .filter(|o| {
            o.get("field_name").and_then(|v| v.as_str()) == Some("issues")
                && o.get("value_text").map_or(true, |v| v.is_null())
                && o.get("value_number").map_or(true, |v| v.is_null())
                && o.get("value_boolean").map_or(true, |v| v.is_null())
        })
        .collect();
    parents.sort_by_key(|o| o.get("order_index").and_then(|v| v.as_i64()).unwrap_or(0));

    parents
        .iter()
        .filter_map(|parent| {
            let parent_id = parent.get("id").cloned();
            let children: Vec<&Value> = outputs
                .iter()
                .filter(|o| o.get("parent_output_id").cloned() == parent_id && parent_id.is_some())
                .collect();
            let cell = |k: &str| -> Value {
                let Some(c) = children.iter().find(|x| x.get("field_name").and_then(|v| v.as_str()) == Some(k)) else {
                    return Value::Null;
                };
                for key in ["value_text", "value_number", "value_boolean"] {
                    if let Some(v) = c.get(key) {
                        if !v.is_null() {
                            return v.clone();
                        }
                    }
                }
                Value::Null
            };
            let severity = cell("severity");
            let severity = if severity.is_null() { json!("major") } else { severity };
            let microtask_id = cell("microtask_id");
            let microtask_id = if microtask_id.is_null() { json!("") } else { microtask_id };
            let file = cell("file");
            let file = if file.is_null() { json!("") } else { file };
            let problem = cell("problem");
            let problem = if problem.is_null() { json!("") } else { problem };
            let fix_hint = cell("fix_hint");
            let fix_hint = if fix_hint.is_null() { json!("") } else { fix_hint };
            if !truthy(Some(&problem)) {
                return None;
            }
            Some(json!({
                "severity": severity, "microtask_id": microtask_id, "file": file,
                "problem": problem, "fix_hint": fix_hint,
            }))
        })
        .collect()
}

struct Reconciled {
    issues: Vec<Value>,
    verdict: String,
    findings_md: String,
}

// FIX-QA-REVIEW-GROUNDING: the gate referees the referee mechanically — a microtasks-target issue
// must cite a real plan microtask_id (verbatim or embedded) or a real plan file path; everything
// else is discarded with an audit note. A fail whose issues are ALL discarded reconciles to pass.
// Contract-target reviews are exempt (their issues route to a human).
fn reconcile_grounding(microtasks: &[Value], failure_target: &str, verdict: &str, findings_md: &str, issues: Vec<Value>) -> Reconciled {
    if failure_target != "microtasks" {
        return Reconciled { issues, verdict: verdict.to_string(), findings_md: findings_md.to_string() };
    }
    let plan_ids: Vec<String> = microtasks.iter().filter_map(|m| m.get("microtask_id")).filter(|v| truthy(Some(v))).map(js_string).collect();
    let plan_files: std::collections::HashSet<String> = microtasks.iter().filter_map(|m| m.get("file")).filter(|v| truthy(Some(v))).map(js_string).collect();
    let grounded = |iss: &Value| -> bool {
        let mid = js_string(iss.get("microtask_id").unwrap_or(&Value::Null));
        let file = js_string(iss.get("file").unwrap_or(&Value::Null));
        plan_ids.iter().any(|id| &mid == id || mid.contains(id.as_str())) || plan_files.contains(&file)
    };
    let (effective, discarded): (Vec<Value>, Vec<Value>) = issues.into_iter().partition(grounded);
    let mut findings = findings_md.to_string();
    if !discarded.is_empty() {
        let joined = discarded
            .iter()
            .map(|d| format!("\"{}\"", js_string(d.get("problem").unwrap_or(&Value::Null)).chars().take(140).collect::<String>()))
            .collect::<Vec<_>>()
            .join(" · ");
        findings += &format!("\n\n[substrate] discarded {} ungrounded issue(s) (cited no plan microtask_id/file): {joined}", discarded.len());
    }
    let mut effective_verdict = verdict.to_string();
    if effective_verdict == "fail" && effective.is_empty() {
        effective_verdict = "pass".to_string();
        findings += "\n[substrate] verdict reconciled to PASS — the fail carried only ungrounded issues.";
    }
    Reconciled { issues: effective, verdict: effective_verdict, findings_md: findings }
}

pub fn qaContractReview(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS: `const { db, provider_fn = null } = context ?? {}; if (!db) throw err.contextMissing(...)`
    // — unreachable here (Rust's `db: &Connection` is never absent).
    let provider_fn = input.get("provider_fn").cloned().unwrap_or(Value::Null);
    let contract_slug = input.get("contract_slug").cloned().unwrap_or(Value::Null);
    // JS destructuring default (`reviewer = 'driver:auto'`) triggers only when the key is
    // absent/undefined — an explicit null or '' is passed through verbatim.
    let reviewer = match input.get("reviewer") {
        None => json!("driver:auto"),
        Some(v) => v.clone(),
    };
    let mode = if input.get("mode").and_then(|v| v.as_str()) == Some("plan") { "plan" } else { "full" };
    if !truthy(Some(&contract_slug)) {
        return Err("qa-contract-review: contract_slug is required".to_string());
    }
    let contract_slug_s = js_string(&contract_slug);

    let contract = crate::contract_contract_get::contractGet(&json!({ "slug": contract_slug_s }), db)?;
    if contract.is_null() {
        return Err(format!("qa-contract-review: contract '{contract_slug_s}' not found"));
    }

    let microtasks = load_json_array(contract.get("microtasks_json"));
    let acceptance = load_json_array(contract.get("acceptance_json"));
    let files_touched = load_json_array(contract.get("files_touched_json"));
    let out_of_scope = load_json_array(contract.get("out_of_scope_json"));

    let prompt_name = if mode == "plan" { PLAN_PROMPT_NAME } else { PROMPT_NAME };
    ensure_prompt(db, prompt_name, if mode == "plan" { PLAN_PROMPT_TEMPLATE_MD } else { PROMPT_TEMPLATE_MD })?;

    let goal_md = { let g = str_qq(nz(contract.get("goal_md"))); if g.is_empty() { "(no goal)".to_string() } else { g } };
    let briefing_md = {
        let b = nz(contract.get("tl_briefing_md")).or_else(|| nz(contract.get("briefing_md")));
        let s = str_qq(b);
        if s.is_empty() { "(no briefing)".to_string() } else { s }
    };

    let mut input_kv: Vec<Value> = vec![
        json!({ "key": "goal_md", "value": clip(&goal_md, 2500) }),
        json!({ "key": "briefing_md", "value": clip(&briefing_md, 5000) }),
        json!({ "key": "acceptance", "value": clip(&serde_json::to_string_pretty(&acceptance).unwrap_or_default(), 4000) }),
        json!({ "key": "files_touched", "value": clip(&serde_json::to_string(&files_touched).unwrap_or_default(), 1500) }),
        json!({ "key": "out_of_scope", "value": clip(&serde_json::to_string(&out_of_scope).unwrap_or_default(), 1500) }),
    ];
    if mode == "plan" {
        let bootstrap_cmd = { let s = str_qq(nz(contract.get("bootstrap_cmd"))); if s.is_empty() { "(none)".to_string() } else { s } };
        let generated_paths = contract.get("generated_paths").cloned().unwrap_or_else(|| json!([]));
        input_kv.push(json!({ "key": "bootstrap_cmd", "value": bootstrap_cmd }));
        input_kv.push(json!({ "key": "generated_paths", "value": clip(&serde_json::to_string(&generated_paths).unwrap_or_default(), 1500) }));
        input_kv.push(json!({ "key": "microtasks", "value": clip(&render_plan_defs(&microtasks), 12000) }));
    } else {
        input_kv.push(json!({ "key": "microtasks", "value": clip(&render_microtasks(&microtasks), 12000) }));
    }

    // ── PARITY-DEFERRED from here: the review verdict comes from a real network AI call. ──────
    let ai_result = crate::ai_ai_call::aiCall(
        &json!({
            "prompt_name": prompt_name,
            "input_kv": input_kv,
            "tier": "low",
            "temperature": "cold",
            "structured_output": true,
            "provider_fn": provider_fn,
        }),
        db,
    )?;

    // Unreachable while aiCall is deferred (see doc) — ported byte-for-byte from the JS
    // response-parsing + grounding-reconciliation, exercised directly by this file's unit tests
    // against canned output_kv shapes.
    let outputs: Vec<Value> = ai_result.get("output_kv").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    let verdict = extract_scalar(&outputs, "verdict").map(|s| s.to_string());
    let findings_md = extract_scalar(&outputs, "findings_md").unwrap_or("").to_string();
    let raw_target = extract_scalar(&outputs, "failure_target").unwrap_or("");
    let failure_target = if raw_target == "contract" { "contract" } else { "microtasks" };

    let verdict = match verdict.as_deref() {
        Some("pass") | Some("fail") => verdict.unwrap(),
        _ => {
            return Err(format!(
                "qa-contract-review: QA contract review returned no usable verdict (got '{}') — expected 'pass' or 'fail'.",
                verdict.as_deref().unwrap_or("undefined"),
            ))
        }
    };

    let issues = extract_issues(&outputs);
    let reconciled = reconcile_grounding(&microtasks, failure_target, &verdict, &findings_md, issues.clone());
    let discarded_issues = (issues.len() - reconciled.issues.len()) as i64;

    let passed_i = if reconciled.verdict == "pass" { 1 } else { 0 };
    let (_, review_id) = crate::db_json::run_write(
        db,
        "INSERT INTO contract_qa_reviews (contract_slug, passed, verdict, findings_md, issues_json, reviewer, failure_target, review_mode) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        &[
            json!(contract_slug_s),
            json!(passed_i),
            json!(reconciled.verdict),
            json!(reconciled.findings_md),
            json!(serde_json::to_string(&reconciled.issues).unwrap_or_default()),
            reviewer.clone(),
            json!(failure_target),
            json!(mode),
        ],
    )?;

    Ok(json!({
        "contract_slug": contract_slug_s,
        "verdict": reconciled.verdict,
        "passed": passed_i == 1,
        "failure_target": failure_target,
        "review_mode": mode,
        "review_id": review_id,
        "issues": reconciled.issues,
        "findings_md": reconciled.findings_md,
        "discarded_issues": discarded_issues,
        "ai_call_id": ai_result.get("ai_call_id").cloned().unwrap_or(Value::Null),
        "cost_usd": ai_result.get("cost_usd").and_then(|v| v.as_f64()).unwrap_or(0.0),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS brain;").unwrap();
        c.execute_batch(
            "CREATE TABLE contracts (slug TEXT PRIMARY KEY, goal_md TEXT, tl_briefing_md TEXT, briefing_md TEXT, \
             bootstrap_cmd TEXT, generated_paths_json TEXT, microtasks_json TEXT, acceptance_json TEXT, \
             files_touched_json TEXT, naming_json TEXT, edge_cases_json TEXT, must_obey_json TEXT, \
             out_of_scope_json TEXT, reference_artifacts_json TEXT);
             CREATE TABLE contract_qa_reviews (id INTEGER PRIMARY KEY AUTOINCREMENT, contract_slug TEXT, ts TEXT, \
             passed INTEGER, verdict TEXT, findings_md TEXT, issues_json TEXT, reviewer TEXT, failure_target TEXT, review_mode TEXT);
             CREATE TABLE brain.prompt_templates ( id INTEGER PRIMARY KEY, name TEXT NOT NULL, version INTEGER NOT NULL, \
             template_md TEXT NOT NULL, default_tier TEXT NOT NULL, default_temperature TEXT NOT NULL, \
             max_output_tokens INTEGER NOT NULL DEFAULT 1024, superseded_by INTEGER, \
             created_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')), cross_field_rules_json TEXT, \
             judging_criteria_json TEXT, system_md TEXT, phase_flow TEXT NOT NULL DEFAULT 'single', UNIQUE (name, version) );
             CREATE TABLE brain.prompt_output_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, \
             field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, \
             description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, \
             required_sections_json TEXT, pattern TEXT, grounded_refs_json TEXT, UNIQUE (prompt_template_id, field_name) );
             CREATE TABLE brain.prompt_input_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, \
             field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, \
             description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, \
             required_sections_json TEXT, pattern TEXT, UNIQUE (prompt_template_id, field_name) );",
        )
        .unwrap();
        c
    }

    fn a_contract(c: &Connection, slug: &str, microtasks_json: &str) {
        c.execute(
            "INSERT INTO contracts (slug, goal_md, microtasks_json, acceptance_json, files_touched_json, out_of_scope_json) VALUES (?, 'do the thing', ?, '[]', '[]', '[]')",
            rusqlite::params![slug, microtasks_json],
        )
        .unwrap();
    }

    #[test]
    fn missing_contract_slug_errors() {
        let c = db();
        let e = qaContractReview(&json!({}), &c).unwrap_err();
        assert_eq!(e, "qa-contract-review: contract_slug is required");
    }

    #[test]
    fn unknown_contract_errors_not_found() {
        let c = db();
        let e = qaContractReview(&json!({ "contract_slug": "GHOST" }), &c).unwrap_err();
        assert_eq!(e, "qa-contract-review: contract 'GHOST' not found");
    }

    #[test]
    fn reaches_the_parity_deferred_aicall_boundary_and_registers_prompt() {
        // proves every deterministic gate (contract lookup / json-col parsing / clip / render /
        // prompt-ensure) ran cleanly and the error is aiCall's deferred stub, not an earlier bug.
        let c = db();
        a_contract(&c, "C-1", "[]");
        let e = qaContractReview(&json!({ "contract_slug": "C-1" }), &c).unwrap_err();
        assert!(e.contains("ai-call"), "unexpected error: {e}");
        let n: i64 = c.query_row("SELECT COUNT(*) FROM brain.prompt_templates WHERE name = 'qa.contract.review.v2'", [], |row| row.get(0)).unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn plan_mode_registers_the_plan_prompt() {
        let c = db();
        a_contract(&c, "C-2", "[]");
        let e = qaContractReview(&json!({ "contract_slug": "C-2", "mode": "plan" }), &c).unwrap_err();
        assert!(e.contains("ai-call"));
        let n: i64 = c.query_row("SELECT COUNT(*) FROM brain.prompt_templates WHERE name = 'qa.contract.plan.review.v2'", [], |row| row.get(0)).unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn ensure_prompt_reregisters_on_template_drift() {
        let c = db();
        // promptAdd requires output_fields (≥1) — matches JS err.required('prompt-add', 'output_fields (≥1)').
        crate::ai_prompt_add::promptAdd(&json!({ "name": "qa.contract.review.v2", "template_md": "stale", "output_fields": [{ "field_name": "x", "field_type": "string" }] }), &c).unwrap();
        ensure_prompt(&c, "qa.contract.review.v2", PROMPT_TEMPLATE_MD).unwrap();
        let row = crate::ai_prompt_find::promptFind(&json!({ "name": "qa.contract.review.v2" }), &c).unwrap();
        assert_eq!(row["template_md"], json!(PROMPT_TEMPLATE_MD));
        assert_eq!(row["version"], json!(2));
    }

    #[test]
    fn ensure_prompt_is_noop_when_template_matches() {
        let c = db();
        ensure_prompt(&c, "qa.contract.review.v2", PROMPT_TEMPLATE_MD).unwrap();
        ensure_prompt(&c, "qa.contract.review.v2", PROMPT_TEMPLATE_MD).unwrap();
        let n: i64 = c.query_row("SELECT COUNT(*) FROM brain.prompt_templates WHERE name = 'qa.contract.review.v2'", [], |row| row.get(0)).unwrap();
        assert_eq!(n, 1);
    }

    // ── load_json_array ─────────────────────────────────────────────────────────────────────
    #[test]
    fn load_json_array_defaults_and_guards() {
        assert_eq!(load_json_array(None), Vec::<Value>::new());
        assert_eq!(load_json_array(Some(&Value::Null)), Vec::<Value>::new());
        assert_eq!(load_json_array(Some(&json!(""))), Vec::<Value>::new());
        assert_eq!(load_json_array(Some(&json!("not json"))), Vec::<Value>::new());
        assert_eq!(load_json_array(Some(&json!("{}"))), Vec::<Value>::new()); // parses but not array
        assert_eq!(load_json_array(Some(&json!("[1,2]"))), vec![json!(1), json!(2)]);
    }

    // ── clip ─────────────────────────────────────────────────────────────────────────────────
    #[test]
    fn clip_passes_short_strings_through() {
        assert_eq!(clip("short", 20), "short");
    }
    #[test]
    fn clip_announces_truncation() {
        let out = clip("abcdefgh", 4);
        assert_eq!(out, "abcd\n…[substrate: clipped at 4 chars — judge ONLY what is shown]");
    }

    // ── render_microtasks / render_plan_defs ────────────────────────────────────────────────
    #[test]
    fn render_microtasks_empty_is_itself_a_fail_note() {
        assert_eq!(render_microtasks(&[]), "(no microtasks authored — this is itself a fail)");
    }
    #[test]
    fn render_microtasks_renders_patches() {
        let ms = vec![json!({
            "microtask_id": "m1", "intent": "add fn", "files_touched": ["a.rs"], "acceptance": ["works"],
            "patches": [{ "file": "a.rs", "op": "insert", "anchor_before": "fn a", "anchor_after": "}", "replacement": "fn b(){}" }],
        })];
        let out = render_microtasks(&ms);
        assert!(out.contains("1. [m1] add fn"));
        assert!(out.contains("Patch 1: file=a.rs op=insert"));
        assert!(out.contains("fn b(){}"));
    }
    #[test]
    fn render_plan_defs_empty_is_itself_a_fail_note() {
        assert_eq!(render_plan_defs(&[]), "(no microtask definitions — this is itself a fail)");
    }
    #[test]
    fn render_plan_defs_renders_spec() {
        let ms = vec![json!({ "microtask_id": "m1", "intent": "add fn", "file": "a.rs", "op_kind": "create", "depends_on": [], "acceptance": "works", "file_spec_md": "line1\nline2" })];
        let out = render_plan_defs(&ms);
        assert!(out.contains("file: a.rs   op_kind: create"));
        assert!(out.contains("     line1\n     line2"));
    }

    // ── extract_scalar / extract_issues (canned output_kv, the shape aiCall would return) ─────
    fn canned_outputs() -> Vec<Value> {
        vec![
            json!({ "id": 1, "field_name": "verdict", "value_text": "fail", "parent_output_id": null, "order_index": 0 }),
            json!({ "id": 2, "field_name": "failure_target", "value_text": "microtasks", "parent_output_id": null, "order_index": 1 }),
            json!({ "id": 3, "field_name": "findings_md", "value_text": "bad snippet", "parent_output_id": null, "order_index": 2 }),
            json!({ "id": 4, "field_name": "issues", "value_text": null, "value_number": null, "value_boolean": null, "parent_output_id": null, "order_index": 3 }),
            json!({ "id": 5, "field_name": "severity", "value_text": "blocker", "parent_output_id": 4 }),
            json!({ "id": 6, "field_name": "microtask_id", "value_text": "m1", "parent_output_id": 4 }),
            json!({ "id": 7, "field_name": "file", "value_text": "a.rs", "parent_output_id": 4 }),
            json!({ "id": 8, "field_name": "problem", "value_text": "empty replacement", "parent_output_id": 4 }),
            json!({ "id": 9, "field_name": "fix_hint", "value_text": "write real code", "parent_output_id": 4 }),
        ]
    }

    #[test]
    fn extract_scalar_finds_by_field_name() {
        let o = canned_outputs();
        assert_eq!(extract_scalar(&o, "verdict"), Some("fail"));
        assert_eq!(extract_scalar(&o, "nope"), None);
    }

    #[test]
    fn extract_issues_decomposes_parent_child_rows() {
        let o = canned_outputs();
        let issues = extract_issues(&o);
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0]["severity"], json!("blocker"));
        assert_eq!(issues[0]["microtask_id"], json!("m1"));
        assert_eq!(issues[0]["file"], json!("a.rs"));
        assert_eq!(issues[0]["problem"], json!("empty replacement"));
    }

    #[test]
    fn extract_issues_drops_issue_with_empty_problem() {
        let o = vec![
            json!({ "id": 4, "field_name": "issues", "value_text": null, "value_number": null, "value_boolean": null, "parent_output_id": null, "order_index": 0 }),
            json!({ "id": 5, "field_name": "problem", "value_text": "", "parent_output_id": 4 }),
        ];
        assert_eq!(extract_issues(&o).len(), 0);
    }

    // ── reconcile_grounding ──────────────────────────────────────────────────────────────────
    #[test]
    fn grounded_issue_survives_and_verdict_stays_fail() {
        let microtasks = vec![json!({ "microtask_id": "m1", "file": "a.rs" })];
        let issues = vec![json!({ "microtask_id": "m1", "file": "", "problem": "bad" })];
        let r = reconcile_grounding(&microtasks, "microtasks", "fail", "findings", issues);
        assert_eq!(r.issues.len(), 1);
        assert_eq!(r.verdict, "fail");
    }

    #[test]
    fn all_ungrounded_issues_reconcile_verdict_to_pass() {
        let microtasks = vec![json!({ "microtask_id": "m1", "file": "a.rs" })];
        let issues = vec![json!({ "microtask_id": "ghost", "file": "ghost.rs", "problem": "invented acceptance" })];
        let r = reconcile_grounding(&microtasks, "microtasks", "fail", "findings", issues);
        assert_eq!(r.issues.len(), 0);
        assert_eq!(r.verdict, "pass");
        assert!(r.findings_md.contains("discarded 1 ungrounded issue"));
        assert!(r.findings_md.contains("reconciled to PASS"));
    }

    #[test]
    fn contract_target_issues_are_exempt_from_grounding() {
        let microtasks = vec![json!({ "microtask_id": "m1", "file": "a.rs" })];
        let issues = vec![json!({ "microtask_id": "ghost", "file": "ghost.rs", "problem": "impossible AC" })];
        let r = reconcile_grounding(&microtasks, "contract", "fail", "findings", issues.clone());
        assert_eq!(r.issues, issues);
        assert_eq!(r.verdict, "fail");
    }

    #[test]
    fn file_grounded_issue_survives_via_plan_file_set() {
        let microtasks = vec![json!({ "microtask_id": "m1", "file": "a.rs" })];
        let issues = vec![json!({ "microtask_id": "", "file": "a.rs", "problem": "scope creep" })];
        let r = reconcile_grounding(&microtasks, "microtasks", "fail", "findings", issues);
        assert_eq!(r.issues.len(), 1);
    }
}
