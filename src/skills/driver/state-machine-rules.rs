//! state-machine-rules.rs — twin of skills/driver/state-machine-rules.mjs (db WRITE lane —
//! standard (input,context) signature though the logic is pure/no-db). NAMES ARE CONTRACT:
//! `stateMachineRules` (default), `evaluateRules` (named).
//!
//! SCOPE: `stateMachineRules()` returns `RULES` VERBATIM in JS, including each rule's
//! function-VALUED `match`/`out` fields — `normalizeResult` stringifies a function via
//! `String(fn)` (the `packs.rs` precedent), so a byte-identical twin would need to reproduce the
//! EXACT V8 source text (whitespace/formatting/quote-style) of up to 230 individual closures
//! (115 rules × match + out). That is pure transcription risk with no logic value — the thing
//! that actually matters is `evaluateRules`, the ONLY consumer of RULES/RULES_V2 in production
//! (driver-state-machine.mjs calls evaluateRules, never stateMachineRules() for its return value
//! shape — stateMachineRules exists only as a lint-compliant "skill-conforming introspector").
//! `stateMachineRules` is therefore `class:"transitive"` (same precedent as a Map/Set-returning
//! fn): a real `pub fn` exists for names_ok, but no fixture/arm — proven via evaluateRules
//! exercising the SAME rule logic it introspects.
//!
//! `evaluateRules(facts)` is ported for real: first-match-wins over 69 RULES (v1) or 46 RULES_V2
//! rules (picked by `facts.pipelineV2Enabled`), each rule's `match`/`out` LOGIC (not source text)
//! reproduced as ordinary Rust control flow — chunked across multiple `try_*` functions (each
//! returning `Option<Value>`, Some on first internal match) chained in ORIGINAL rule order via
//! `.or_else(...)`, so the overall first-match-wins semantics exactly matches the JS for-loop.
use serde_json::{json, Map, Value};

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
#[allow(dead_code)]
fn tmpl(v: Option<&Value>) -> String {
    match v {
        None => "undefined".into(),
        Some(Value::Null) => "null".into(),
        Some(x) => str_qq(Some(x)),
    }
}
fn nz(v: Option<&Value>) -> Option<&Value> {
    v.filter(|x| !x.is_null())
}

// ── shared fact/card helpers (used across many rule predicates) ─────────────────────────────

// sizeEstimateForCard(card): body-length/keyword heuristic. `card?.body_json ?? ''`: a non-empty
// STRING is JSON.parse'd (parse failure -> body stays the raw string, matching JS's catch which
// re-stringifies the SAME already-string raw value); a parsed non-string value is
// JSON.stringify'd; anything else (absent/null/non-string body_json) leaves body = "".
fn size_estimate_for_card(card: &Value) -> &'static str {
    let raw = card.get("body_json");
    let body: String = match raw.and_then(|v| v.as_str()) {
        Some(s) if !s.is_empty() => match serde_json::from_str::<Value>(s) {
            Ok(Value::String(parsed_s)) => parsed_s,
            Ok(other) => serde_json::to_string(&other).unwrap_or_default(),
            Err(_) => s.to_string(),
        },
        _ => String::new(),
    };
    let len = body.chars().count();
    let lower = body.to_lowercase();
    let big_kw = big_kw_re();
    let file_mentions = file_ext_re().find_iter(&body).count();
    if len > 4000 || file_mentions >= 4 || big_kw.is_match(&lower) {
        "L"
    } else if len > 2000 || file_mentions >= 2 {
        "M"
    } else if len > 600 {
        "S"
    } else {
        "XS"
    }
}
fn big_kw_re() -> &'static regex::Regex {
    static R: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    R.get_or_init(|| regex::Regex::new(r"\b(epic|multiple|decompose|several|many files|across)\b").unwrap())
}
fn file_ext_re() -> &'static regex::Regex {
    static R: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    R.get_or_init(|| regex::Regex::new(r"\.(mjs|js|ts|tsx|json|md|sql)\b").unwrap())
}

// parsedDefs(contract): JSON.parse(contract?.microtasks_json ?? '[]'); non-array parse or parse
// failure -> [].
fn parsed_defs(contract: Option<&Value>) -> Vec<Value> {
    let s = contract
        .and_then(|c| c.get("microtasks_json"))
        .and_then(|v| v.as_str())
        .unwrap_or("[]");
    match serde_json::from_str::<Value>(s) {
        Ok(Value::Array(a)) => a,
        _ => Vec::new(),
    }
}

// hasPlanDefsFact(facts): explicit facts.hasPlanDefs wins (even if falsy) when the KEY is
// present; otherwise derive from contract.microtasks_json.
fn has_plan_defs_fact(facts: &Value) -> bool {
    match facts.get("hasPlanDefs") {
        Some(v) => truthy(Some(v)),
        None => !parsed_defs(facts.get("contract")).is_empty(),
    }
}

// allFilesAuthoredFact(facts): explicit facts.allFilesAuthored wins when present; otherwise every
// microtask def must have a non-empty `patches` array.
fn all_files_authored_fact(facts: &Value) -> bool {
    match facts.get("allFilesAuthored") {
        Some(v) => truthy(Some(v)),
        None => {
            let defs = parsed_defs(facts.get("contract"));
            !defs.is_empty()
                && defs.iter().all(|m| {
                    m.get("patches").and_then(|p| p.as_array()).map(|a| !a.is_empty()).unwrap_or(false)
                })
        }
    }
}

// needsMicrotasks(contract): contract?.size === 'M' || contract?.size === 'L'.
fn needs_microtasks(contract: Option<&Value>) -> bool {
    matches!(contract.and_then(|c| c.get("size")).and_then(|v| v.as_str()), Some("M") | Some("L"))
}

const MAX_DEV_RETRIES: i64 = 3;
const TL_MAX_AMENDMENTS: i64 = 2;
const MAX_REJECT_REDRAFTS: i64 = 2;
const MAX_CONSECUTIVE_IDENTICAL_FAILURES: i64 = 2;
const MAX_SWARM_MANIFEST_VERSIONS: i64 = 2;
const MAX_SWARM_AUTHOR_RETRIES: i64 = 3;
const MAX_V2_DESIGN_COURT_REJECTIONS: i64 = 2;
const MAX_V2_DESIGN_VERSIONS_HARD: i64 = 5;
const MAX_V2_AUTHOR_RETRIES: i64 = 3;

// ── RULES (v1) chunks, in original array order ───────────────────────────────────────────────

fn try_rules_v1_chunk_a(facts: &Value) -> Option<Value> {
    let null_v = Value::Null;
    let card_v = facts.get("card").unwrap_or(&null_v);
    let card_status = card_v.get("status").and_then(|s| s.as_str());
    let no_contract = !truthy(facts.get("contract"));

    // repeat-failure-escalate
    let recent_consecutive_failures = nz(facts.get("recentConsecutiveFailures"))
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    if recent_consecutive_failures >= MAX_CONSECUTIVE_IDENTICAL_FAILURES {
        let sig: String = nz(facts.get("recentFailureSignature"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .chars()
            .take(60)
            .collect();
        return Some(json!({
            "next_agent": null,
            "next_action": null,
            "reason": format!(
                "escalate-to-human: {} consecutive ticks failed with identical error signature '{}…' — daemon stopped dispatching to break the loop",
                tmpl(facts.get("recentConsecutiveFailures")),
                sig
            ),
            "terminal": true,
            "matched_rule": "repeat-failure-escalate"
        }));
    }

    // card-done
    if card_status == Some("done") {
        return Some(json!({
            "next_agent": null,
            "next_action": null,
            "reason": "card closed",
            "terminal": true,
            "matched_rule": "card-done"
        }));
    }

    // card-blocked
    if card_status == Some("blocked") {
        return Some(json!({
            "next_agent": null,
            "next_action": null,
            "reason": "card blocked — human review needed",
            "terminal": true,
            "matched_rule": "card-blocked"
        }));
    }

    // snippet-human-gate
    let contract_status = facts.get("contract").and_then(|c| c.get("status")).and_then(|s| s.as_str());
    if truthy(facts.get("snippetGateEnabled"))
        && contract_status == Some("pl_ratified")
        && (all_files_authored_fact(facts) || truthy(facts.get("swarmAuthored")))
        && !truthy(facts.get("snippetApproved"))
    {
        return Some(json!({
            "next_agent": null,
            "next_action": null,
            "reason": "snippet human-gate: snippets authored → awaiting human approve / reject / edit",
            "terminal": false,
            "matched_rule": "snippet-human-gate"
        }));
    }

    // card-has-children-awaiting
    let card_children = nz(facts.get("cardChildren")).and_then(|v| v.as_i64()).unwrap_or(0);
    if no_contract && card_status == Some("backlog") && card_children > 0 {
        return Some(json!({
            "next_agent": null,
            "next_action": null,
            "reason": "card already decomposed — work happens on children",
            "terminal": false,
            "matched_rule": "card-has-children-awaiting"
        }));
    }

    // skip-pl-route-blocked-too-big
    let pl_session_present = truthy(facts.get("plSession"));
    let skip_pl_flag_is_one = card_v.get("skip_pl_flag").and_then(|v| v.as_i64()) == Some(1);
    if no_contract
        && card_status == Some("backlog")
        && !pl_session_present
        && skip_pl_flag_is_one
        && (size_estimate_for_card(card_v) == "M" || size_estimate_for_card(card_v) == "L")
    {
        return Some(json!({
            "next_agent": "pl",
            "next_action": "pl.session.start",
            "reason": "skip_pl=1 set but card size > S — re-routing through PL for decomposition (size-cascade enforcement)",
            "terminal": false,
            "matched_rule": "skip-pl-route-blocked-too-big"
        }));
    }

    // skip-pl-route
    if no_contract && card_status == Some("backlog") && !pl_session_present && skip_pl_flag_is_one {
        return Some(json!({
            "next_agent": "tl",
            "next_action": "contract.draft.auto",
            "reason": "skip_pl_flag=1 (god-set) → bypass PL, route straight to contract draft",
            "terminal": false,
            "matched_rule": "skip-pl-route"
        }));
    }

    // pl-start
    let priority = nz(card_v.get("priority")).and_then(|v| v.as_i64()).unwrap_or(0);
    let driver_intent_is_play = card_v.get("driver_intent").and_then(|v| v.as_str()) == Some("play");
    if no_contract
        && card_status == Some("backlog")
        && !pl_session_present
        && (priority >= 5 || driver_intent_is_play)
    {
        return Some(json!({
            "next_agent": "pl",
            "next_action": "pl.session.start",
            "reason": "no PL session yet for this user-prioritized backlog card",
            "terminal": false,
            "matched_rule": "pl-start"
        }));
    }

    // pl-awaiting-user-start
    if no_contract
        && card_status == Some("backlog")
        && !pl_session_present
        && priority < 5
        && !driver_intent_is_play
    {
        return Some(json!({
            "next_agent": null,
            "next_action": null,
            "reason": "card not user-prioritized — type `do <slug>` in chat to start",
            "terminal": false,
            "matched_rule": "pl-awaiting-user-start"
        }));
    }

    // pl-clarifying-awaiting-answers
    let pl_session_status = facts.get("plSession").and_then(|p| p.get("status")).and_then(|s| s.as_str());
    let pending_questions = nz(facts.get("plSession").and_then(|p| p.get("pending_questions")))
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    if no_contract && pl_session_status == Some("clarifying") && pending_questions > 0 {
        return Some(json!({
            "next_agent": "pl",
            "next_action": null,
            "reason": "PL clarifying with unanswered questions — awaiting human input",
            "terminal": false,
            "matched_rule": "pl-clarifying-awaiting-answers"
        }));
    }

    // pl-clarifying
    if no_contract && pl_session_status == Some("clarifying") {
        return Some(json!({
            "next_agent": "pl",
            "next_action": "pl.session.advance.clarify",
            "reason": "PL clarifying → run clarify step (clarifying → researching)",
            "terminal": false,
            "matched_rule": "pl-clarifying"
        }));
    }

    // pl-researching
    if no_contract && pl_session_status == Some("researching") {
        return Some(json!({
            "next_agent": "pl",
            "next_action": "pl.session.advance.research",
            "reason": "PL researching → continue research step",
            "terminal": false,
            "matched_rule": "pl-researching"
        }));
    }

    // pl-deciding-split
    if no_contract && pl_session_status == Some("deciding_split") {
        return Some(json!({
            "next_agent": "pl",
            "next_action": "pl.session.advance.decompose",
            "reason": "PL deciding split → decompose",
            "terminal": false,
            "matched_rule": "pl-deciding-split"
        }));
    }

    // pl-tuning
    if no_contract && pl_session_status == Some("tuning") {
        return Some(json!({
            "next_agent": "pl",
            "next_action": "pl.session.finalize",
            "reason": "PL tuning → finalize",
            "terminal": false,
            "matched_rule": "pl-tuning"
        }));
    }

    None
}

fn try_rules_v1_chunk_b(facts: &Value) -> Option<Value> {
    let contract = facts.get("contract");
    let pl_session = facts.get("plSession");

    // pl-awaiting-approval
    if !truthy(contract)
        && pl_session.and_then(|v| v.get("status")).and_then(|v| v.as_str()) == Some("awaiting_approval")
        && !matches!(facts.get("autoApproveEnabled"), Some(Value::Bool(false)))
    {
        return Some(json!({
            "next_agent": "pl",
            "next_action": "pl.session.approve",
            "reason": "PL awaiting approval → auto-approve (driver)",
            "terminal": false,
            "matched_rule": "pl-awaiting-approval"
        }));
    }

    // pl-awaiting-approval-hold
    if !truthy(contract)
        && pl_session.and_then(|v| v.get("status")).and_then(|v| v.as_str()) == Some("awaiting_approval")
        && matches!(facts.get("autoApproveEnabled"), Some(Value::Bool(false)))
    {
        return Some(json!({
            "next_agent": null,
            "next_action": null,
            "reason": "awaiting human approval (ANTILOKI_AUTO_APPROVE=off) — a human must dispatch pl.session.approve",
            "terminal": false,
            "matched_rule": "pl-awaiting-approval-hold"
        }));
    }

    // pl-done-no-contract
    if !truthy(contract)
        && pl_session.and_then(|v| v.get("status")).and_then(|v| v.as_str()) == Some("done")
    {
        return Some(json!({
            "next_agent": "tl",
            "next_action": "contract.draft.auto",
            "reason": "PL session done → TL drafts contract",
            "terminal": false,
            "matched_rule": "pl-done-no-contract"
        }));
    }

    // contract-rejected-escalate-after-too-many
    if contract.and_then(|c| c.get("status")).and_then(|v| v.as_str()) == Some("rejected")
        && !truthy(contract.and_then(|c| c.get("superseded_by")))
        && contract.and_then(|c| c.get("failure_kind")).and_then(|v| v.as_str()) != Some("operator_restart")
        && (nz(facts.get("cardRejectionCount")).and_then(|v| v.as_i64()).unwrap_or(0) >= MAX_REJECT_REDRAFTS
            || nz(facts.get("autoRedraftsSinceManual")).and_then(|v| v.as_i64()).unwrap_or(0) >= MAX_REJECT_REDRAFTS
            || nz(facts.get("manBlockedRedrafts")).and_then(|v| v.as_i64()).unwrap_or(0) >= MAX_REJECT_REDRAFTS)
    {
        return Some(json!({
            "next_agent": null,
            "next_action": null,
            "reason": format!("escalate-to-human: attempt cap {} reached (cardRejectionCount OR autoRedraftsSinceManual OR manBlockedRedrafts) — auto-redraft budget exhausted, human inspector required", MAX_REJECT_REDRAFTS),
            "terminal": true,
            "matched_rule": "contract-rejected-escalate-after-too-many"
        }));
    }

    // contract-rejected-redo-operator-restart
    if contract.and_then(|c| c.get("status")).and_then(|v| v.as_str()) == Some("rejected")
        && !truthy(contract.and_then(|c| c.get("superseded_by")))
        && contract.and_then(|c| c.get("failure_kind")).and_then(|v| v.as_str()) == Some("operator_restart")
        && truthy(contract.and_then(|c| c.get("rejection_reason_md")))
    {
        return Some(json!({
            "next_agent": "tl",
            "next_action": "tl.contract.redraft",
            "reason": "operator restart → TL re-draft (cap bypassed; explicit human signal)",
            "terminal": false,
            "matched_rule": "contract-rejected-redo-operator-restart"
        }));
    }

    // contract-rejected-redo
    if contract.and_then(|c| c.get("status")).and_then(|v| v.as_str()) == Some("rejected")
        && !truthy(contract.and_then(|c| c.get("superseded_by")))
        && truthy(contract.and_then(|c| c.get("rejection_reason_md")))
        && nz(facts.get("cardRejectionCount")).and_then(|v| v.as_i64()).unwrap_or(0) < MAX_REJECT_REDRAFTS
        && nz(facts.get("autoRedraftsSinceManual")).and_then(|v| v.as_i64()).unwrap_or(0) < MAX_REJECT_REDRAFTS
        && nz(facts.get("manBlockedRedrafts")).and_then(|v| v.as_i64()).unwrap_or(0) < MAX_REJECT_REDRAFTS
    {
        return Some(json!({
            "next_agent": "tl",
            "next_action": "tl.contract.redraft",
            "reason": "rejected contract → TL re-draft (MAN will audit revision via standard Phase 1 gate)",
            "terminal": false,
            "matched_rule": "contract-rejected-redo"
        }));
    }

    // contract-proposed
    if contract.and_then(|c| c.get("status")).and_then(|v| v.as_str()) == Some("proposed") {
        return Some(json!({
            "next_agent": "pl",
            "next_action": "card.gate.pl.ratify",
            "reason": "contract proposed → PL ratifies",
            "terminal": false,
            "matched_rule": "contract-proposed"
        }));
    }

    // swarm-needs-manifest-draft
    if truthy(facts.get("swarmEnabled"))
        && contract.and_then(|c| c.get("status")).and_then(|v| v.as_str()) == Some("pl_ratified")
        && needs_microtasks(contract)
        && !truthy(facts.get("swarmManifestDraft"))
    {
        return Some(json!({
            "next_agent": "tl",
            "next_action": "swarm.manifest.design",
            "reason": "swarm on + no manifest yet → TL designs the blueprint nucleus (the typed dataflow manifest)",
            "terminal": false,
            "matched_rule": "swarm-needs-manifest-draft"
        }));
    }

    // swarm-needs-manifest-lint
    if truthy(facts.get("swarmEnabled"))
        && contract.and_then(|c| c.get("status")).and_then(|v| v.as_str()) == Some("pl_ratified")
        && needs_microtasks(contract)
        && truthy(facts.get("swarmManifestDraft"))
        && !truthy(facts.get("manifestLintFresh"))
    {
        return Some(json!({
            "next_agent": "qa",
            "next_action": "swarm.manifest.lint",
            "reason": "manifest drafted → deterministic lint gate (structure + bindings + type agreement)",
            "terminal": false,
            "matched_rule": "swarm-needs-manifest-lint"
        }));
    }

    // swarm-manifest-lint-failed-redesign
    if truthy(facts.get("swarmEnabled"))
        && contract.and_then(|c| c.get("status")).and_then(|v| v.as_str()) == Some("pl_ratified")
        && needs_microtasks(contract)
        && truthy(facts.get("manifestLintFresh"))
        && facts.get("lastManifestLint").and_then(|v| v.get("passed")).and_then(|v| v.as_i64()) == Some(0)
        && nz(facts.get("swarmManifestVersions")).and_then(|v| v.as_i64()).unwrap_or(0) < MAX_SWARM_MANIFEST_VERSIONS
    {
        return Some(json!({
            "next_agent": "tl",
            "next_action": "swarm.manifest.design",
            "reason": "manifest lint RED → TL re-designs the manifest (bounded)",
            "terminal": false,
            "matched_rule": "swarm-manifest-lint-failed-redesign"
        }));
    }

    // swarm-needs-author
    if truthy(facts.get("swarmEnabled"))
        && contract.and_then(|c| c.get("status")).and_then(|v| v.as_str()) == Some("pl_ratified")
        && needs_microtasks(contract)
        && truthy(facts.get("manifestLintFresh"))
        && facts.get("lastManifestLint").and_then(|v| v.get("passed")).and_then(|v| v.as_i64()) == Some(1)
        && !truthy(facts.get("compositionFresh"))
    {
        return Some(json!({
            "next_agent": "tl",
            "next_action": "swarm.author.blocks",
            "reason": "manifest GREEN → LOW author swarm writes each block + the composition gate runs",
            "terminal": false,
            "matched_rule": "swarm-needs-author"
        }));
    }

    // swarm-composition-failed-reauthor
    if truthy(facts.get("swarmEnabled"))
        && contract.and_then(|c| c.get("status")).and_then(|v| v.as_str()) == Some("pl_ratified")
        && needs_microtasks(contract)
        && truthy(facts.get("compositionFresh"))
        && facts.get("lastComposition").and_then(|v| v.get("passed")).and_then(|v| v.as_i64()) == Some(0)
        && nz(facts.get("swarmAuthorVersions")).and_then(|v| v.as_i64()).unwrap_or(0) < MAX_SWARM_AUTHOR_RETRIES
    {
        return Some(json!({
            "next_agent": "tl",
            "next_action": "swarm.author.blocks",
            "reason": "composition RED → re-author the blocks that do not compose (bounded)",
            "terminal": false,
            "matched_rule": "swarm-composition-failed-reauthor"
        }));
    }

    // swarm-needs-review
    if truthy(facts.get("swarmEnabled"))
        && contract.and_then(|c| c.get("status")).and_then(|v| v.as_str()) == Some("pl_ratified")
        && needs_microtasks(contract)
        && truthy(facts.get("swarmAuthored"))
        && !truthy(facts.get("swarmReviewed"))
    {
        return Some(json!({
            "next_agent": "qa",
            "next_action": "swarm.review.contract",
            "reason": "blocks composed → adversarial review swarm judges each file (advisory)",
            "terminal": false,
            "matched_rule": "swarm-needs-review"
        }));
    }

    // swarm-needs-coverage
    if truthy(facts.get("swarmEnabled"))
        && contract.and_then(|c| c.get("status")).and_then(|v| v.as_str()) == Some("pl_ratified")
        && needs_microtasks(contract)
        && truthy(facts.get("swarmAuthored"))
        && truthy(facts.get("swarmReviewed"))
        && !truthy(facts.get("swarmCoverageFresh"))
    {
        return Some(json!({
            "next_agent": "qa",
            "next_action": "qa.swarm.coverage",
            "reason": "blocks authored + reviewed → deterministic grep-acceptance coverage gate (against authored content)",
            "terminal": false,
            "matched_rule": "swarm-needs-coverage"
        }));
    }

    None
}

// gap fix: dropped at the B/C chunk boundary during the 9-way parallel port (rule #29).
fn try_rule_swarm_coverage_failed_reauthor(facts: &Value) -> Option<Value> {
    let contract = facts.get("contract");
    let swarm_author_versions = nz(facts.get("swarmAuthorVersions")).and_then(|v| v.as_i64()).unwrap_or(0);
    if truthy(facts.get("swarmEnabled"))
        && contract.and_then(|c| c.get("status")).and_then(|v| v.as_str()) == Some("pl_ratified")
        && needs_microtasks(contract)
        && truthy(facts.get("swarmCoverageFresh"))
        && facts.get("lastSwarmCoverage").and_then(|v| v.get("passed")).and_then(|v| v.as_i64()) == Some(0)
        && swarm_author_versions < MAX_SWARM_AUTHOR_RETRIES
    {
        return Some(json!({
            "next_agent": "tl",
            "next_action": "swarm.author.blocks",
            "reason": "swarm-coverage RED → re-author the files missing the acceptance literal (bounded)",
            "terminal": false,
            "matched_rule": "swarm-coverage-failed-reauthor"
        }));
    }
    None
}

fn try_rules_v1_chunk_c(facts: &Value) -> Option<Value> {
    // swarm-stuck-escalate
    if truthy(facts.get("swarmEnabled"))
        && facts.get("contract").and_then(|c| c.get("status")).and_then(|v| v.as_str()) == Some("pl_ratified")
        && needs_microtasks(facts.get("contract"))
        && ((truthy(facts.get("manifestLintFresh"))
                && facts.get("lastManifestLint").and_then(|v| v.get("passed")).and_then(|v| v.as_i64()) == Some(0)
                && facts.get("swarmManifestVersions").and_then(|v| v.as_i64()).unwrap_or(0) >= MAX_SWARM_MANIFEST_VERSIONS)
            || (truthy(facts.get("compositionFresh"))
                && facts.get("lastComposition").and_then(|v| v.get("passed")).and_then(|v| v.as_i64()) == Some(0)
                && facts.get("swarmAuthorVersions").and_then(|v| v.as_i64()).unwrap_or(0) >= MAX_SWARM_AUTHOR_RETRIES)
            || (truthy(facts.get("swarmCoverageFresh"))
                && facts.get("lastSwarmCoverage").and_then(|v| v.get("passed")).and_then(|v| v.as_i64()) == Some(0)
                && facts.get("swarmAuthorVersions").and_then(|v| v.as_i64()).unwrap_or(0) >= MAX_SWARM_AUTHOR_RETRIES))
    {
        return Some(json!({
            "next_agent": null,
            "next_action": null,
            "reason": "swarm exhausted its design/author budget without a clean blueprint — escalate to a human",
            "terminal": true,
            "matched_rule": "swarm-stuck-escalate"
        }));
    }

    // contract-pl-ratified-needs-plan
    if facts.get("contract").and_then(|c| c.get("status")).and_then(|v| v.as_str()) == Some("pl_ratified")
        && needs_microtasks(facts.get("contract"))
        && !has_plan_defs_fact(facts)
        && !truthy(facts.get("swarmEnabled"))
    {
        return Some(json!({
            "next_agent": "tl",
            "next_action": "tl.plan.contract",
            "reason": "size M/L needs a microtask plan (defs) before authoring/signing",
            "terminal": false,
            "matched_rule": "contract-pl-ratified-needs-plan"
        }));
    }

    // contract-planned-needs-plan-coverage
    if facts.get("contract").and_then(|c| c.get("status")).and_then(|v| v.as_str()) == Some("pl_ratified")
        && needs_microtasks(facts.get("contract"))
        && has_plan_defs_fact(facts)
        && !all_files_authored_fact(facts)
        && !truthy(facts.get("planCoverageFresh"))
    {
        return Some(json!({
            "next_agent": "qa",
            "next_action": "qa.contract.plan.coverage",
            "reason": "plan defs ready → deterministic coverage gate (file-exists + grep) runs first, before the AI review",
            "terminal": false,
            "matched_rule": "contract-planned-needs-plan-coverage"
        }));
    }

    // contract-plan-coverage-failed-retry
    if facts.get("contract").and_then(|c| c.get("status")).and_then(|v| v.as_str()) == Some("pl_ratified")
        && needs_microtasks(facts.get("contract"))
        && has_plan_defs_fact(facts)
        && !all_files_authored_fact(facts)
        && truthy(facts.get("planCoverageFresh"))
        && facts.get("lastPlanCoverage").and_then(|v| v.get("passed")).and_then(|v| v.as_i64()) == Some(0)
    {
        return Some(json!({
            "next_agent": "tl",
            "next_action": "tl.plan.contract",
            "reason": "plan-coverage RED → TL re-plans against the exact uncovered acceptances (deterministic — converges in one round)",
            "terminal": false,
            "matched_rule": "contract-plan-coverage-failed-retry"
        }));
    }

    // contract-planned-needs-plan-review
    if facts.get("contract").and_then(|c| c.get("status")).and_then(|v| v.as_str()) == Some("pl_ratified")
        && needs_microtasks(facts.get("contract"))
        && has_plan_defs_fact(facts)
        && !all_files_authored_fact(facts)
        && truthy(facts.get("planCoverageFresh"))
        && facts.get("lastPlanCoverage").and_then(|v| v.get("passed")).and_then(|v| v.as_i64()) == Some(1)
        && !truthy(facts.get("planReviewFresh"))
    {
        return Some(json!({
            "next_agent": "qa",
            "next_action": "qa.contract.review",
            "reason": "plan-coverage GREEN → QA reviews the decomposition (mode=plan, advisory) for open-ended quality",
            "terminal": false,
            "matched_rule": "contract-planned-needs-plan-review"
        }));
    }

    // contract-planned-needs-snippets
    if facts.get("contract").and_then(|c| c.get("status")).and_then(|v| v.as_str()) == Some("pl_ratified")
        && needs_microtasks(facts.get("contract"))
        && has_plan_defs_fact(facts)
        && !all_files_authored_fact(facts)
        && truthy(facts.get("planReviewFresh"))
        && (facts.get("lastPlanReview").and_then(|v| v.get("passed")).and_then(|v| v.as_i64()) == Some(1)
            || facts.get("lastPlanReview").and_then(|v| v.get("failure_target")).and_then(|v| v.as_str()) != Some("contract"))
    {
        return Some(json!({
            "next_agent": "tl",
            "next_action": "tl.author.snippets",
            "reason": "plan-coverage GREEN + AI review done (microtask findings advisory) → TL authors the per-file snippets",
            "terminal": false,
            "matched_rule": "contract-planned-needs-snippets"
        }));
    }

    // contract-pl-ratified
    if facts.get("contract").and_then(|c| c.get("status")).and_then(|v| v.as_str()) == Some("pl_ratified")
        && (!needs_microtasks(facts.get("contract"))
            || all_files_authored_fact(facts)
            || (truthy(facts.get("swarmAuthored")) && truthy(facts.get("swarmReviewed")) && truthy(facts.get("swarmCoverageGreen"))))
    {
        return Some(json!({
            "next_agent": "tl",
            "next_action": "contract.gate.tl.sign",
            "reason": "PL ratified → TL signs",
            "terminal": false,
            "matched_rule": "contract-pl-ratified"
        }));
    }

    // contract-tl-approved-high-risk
    if facts.get("contract").and_then(|c| c.get("status")).and_then(|v| v.as_str()) == Some("tl_approved")
        && facts.get("contract").and_then(|c| c.get("tier")).and_then(|v| v.as_str()) == Some("high-risk")
    {
        return Some(json!({
            "next_agent": null,
            "next_action": null,
            "reason": "high-risk contract awaiting human approval",
            "terminal": false,
            "matched_rule": "contract-tl-approved-high-risk"
        }));
    }

    // contract-tl-approved-needs-pl-review
    if facts.get("contract").and_then(|c| c.get("status")).and_then(|v| v.as_str()) == Some("tl_approved")
        && !truthy(facts.get("contract").and_then(|c| c.get("man_phase1_passed")))
        && facts.get("contract").and_then(|c| c.get("pl_review_count")).and_then(|v| v.as_i64()).unwrap_or(0) == 0
        && !(truthy(facts.get("swarmAuthored")) && truthy(facts.get("swarmReviewed")))
    {
        return Some(json!({
            "next_agent": "pl",
            "next_action": "pl.review.contract",
            "reason": "TL approved → PL reviews the contract (handshake before MAN Phase 1)",
            "terminal": false,
            "matched_rule": "contract-tl-approved-needs-pl-review"
        }));
    }

    // contract-tl-approved-needs-qa-review
    if facts.get("contract").and_then(|c| c.get("status")).and_then(|v| v.as_str()) == Some("tl_approved")
        && !truthy(facts.get("contract").and_then(|c| c.get("man_phase1_passed")))
        && facts.get("contract").and_then(|c| c.get("pl_review_count")).and_then(|v| v.as_i64()).unwrap_or(0) >= 1
        && !truthy(facts.get("contractQaReviewFresh"))
        && !(truthy(facts.get("swarmAuthored")) && truthy(facts.get("swarmReviewed")))
    {
        return Some(json!({
            "next_agent": "qa",
            "next_action": "qa.contract.review",
            "reason": "PL signed → QA #1 reviews the contract + microtasks + literal patches before DEV",
            "terminal": false,
            "matched_rule": "contract-tl-approved-needs-qa-review"
        }));
    }

    // contract-tl-approved-qa-review-contract-level-escalate
    if (facts.get("contract").and_then(|c| c.get("status")).and_then(|v| v.as_str()) == Some("tl_approved")
        && !truthy(facts.get("contract").and_then(|c| c.get("man_phase1_passed")))
        && truthy(facts.get("contractQaReviewFresh"))
        && facts.get("lastContractQaReview").and_then(|v| v.get("passed")).and_then(|v| v.as_i64()) == Some(0)
        && facts.get("lastContractQaReview").and_then(|v| v.get("failure_target")).and_then(|v| v.as_str()) == Some("contract"))
        || (facts.get("contract").and_then(|c| c.get("status")).and_then(|v| v.as_str()) == Some("pl_ratified")
            && truthy(facts.get("planReviewFresh"))
            && facts.get("lastPlanReview").and_then(|v| v.get("passed")).and_then(|v| v.as_i64()) == Some(0)
            && facts.get("lastPlanReview").and_then(|v| v.get("failure_target")).and_then(|v| v.as_str()) == Some("contract"))
    {
        return Some(json!({
            "next_agent": null,
            "next_action": null,
            "reason": "escalate-to-human: QA #1 found a CONTRACT-level defect (acceptance/scope/goal) that microtask re-authoring cannot fix — QA's exact fix hints are on the escalation notification",
            "terminal": true,
            "matched_rule": "contract-tl-approved-qa-review-contract-level-escalate"
        }));
    }

    // contract-tl-approved-qa-review-failed-retry
    if facts.get("contract").and_then(|c| c.get("status")).and_then(|v| v.as_str()) == Some("tl_approved")
        && !truthy(facts.get("contract").and_then(|c| c.get("man_phase1_passed")))
        && truthy(facts.get("contractQaReviewFresh"))
        && facts.get("lastContractQaReview").and_then(|v| v.get("passed")).and_then(|v| v.as_i64()) == Some(0)
        && facts.get("contractQaReauthors").and_then(|v| v.as_i64()).unwrap_or(0) < MAX_DEV_RETRIES
    {
        return Some(json!({
            "next_agent": "tl",
            "next_action": "tl.author.snippets",
            "reason": "QA #1 RED → TL re-authors the snippets against QA findings before DEV (no re-plan)",
            "terminal": false,
            "matched_rule": "contract-tl-approved-qa-review-failed-retry"
        }));
    }

    None
}

// gap fix: dropped at the C/D chunk boundary during the 9-way parallel port (rule #42).
fn try_rule_contract_tl_approved_qa_review_failed_escalate(facts: &Value) -> Option<Value> {
    let contract = facts.get("contract");
    let contract_qa_reauthors = nz(facts.get("contractQaReauthors")).and_then(|v| v.as_i64()).unwrap_or(0);
    if contract.and_then(|c| c.get("status")).and_then(|v| v.as_str()) == Some("tl_approved")
        && !truthy(contract.and_then(|c| c.get("man_phase1_passed")))
        && truthy(facts.get("contractQaReviewFresh"))
        && facts.get("lastContractQaReview").and_then(|v| v.get("passed")).and_then(|v| v.as_i64()) == Some(0)
        && contract_qa_reauthors >= MAX_DEV_RETRIES
    {
        return Some(json!({
            "next_agent": Value::Null,
            "next_action": Value::Null,
            "reason": format!("escalate-to-human: QA #1 contract review still RED after {} re-authors — findings are in the latest contract_qa_reviews.findings_md", MAX_DEV_RETRIES),
            "terminal": true,
            "matched_rule": "contract-tl-approved-qa-review-failed-escalate"
        }));
    }
    None
}

fn try_rules_v1_chunk_d(facts: &Value) -> Option<Value> {
    fn num_eq(v: Option<&Value>, n: i64) -> bool {
        match v {
            Some(Value::Number(num)) => num.as_i64() == Some(n) || num.as_f64() == Some(n as f64),
            _ => false,
        }
    }

    let contract = facts.get("contract").filter(|v| !v.is_null());
    let contract_status = contract.and_then(|c| c.get("status")).and_then(|v| v.as_str());

    // contract-tl-approved-needs-man-phase1
    {
        let contract_qa_review_fresh = truthy(facts.get("contractQaReviewFresh"));
        let last_contract_qa_review = facts.get("lastContractQaReview").filter(|v| !v.is_null());
        let pl_review_count = contract
            .and_then(|c| c.get("pl_review_count"))
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        let swarm_authored = truthy(facts.get("swarmAuthored"));
        let swarm_reviewed = truthy(facts.get("swarmReviewed"));
        if contract_status == Some("tl_approved")
            && !truthy(contract.and_then(|c| c.get("man_phase1_passed")))
            && ((pl_review_count >= 1
                && contract_qa_review_fresh
                && num_eq(last_contract_qa_review.and_then(|v| v.get("passed")), 1))
                || (swarm_authored && swarm_reviewed))
        {
            return Some(json!({
                "next_agent": "man",
                "next_action": "man.audit.contract.signed",
                "reason": "PL signed + QA #1 GREEN (or swarm authored + reviewed) → MAN Phase 1 audit before sandbox/DEV",
                "terminal": false,
                "matched_rule": "contract-tl-approved-needs-man-phase1"
            }));
        }
    }

    // contract-tl-approved-needs-worktree
    {
        let worktree_enabled = truthy(facts.get("worktreeEnabled"));
        if worktree_enabled
            && contract_status == Some("tl_approved")
            && truthy(contract.and_then(|c| c.get("man_phase1_passed")))
            && !truthy(contract.and_then(|c| c.get("worktree_path")))
            && !truthy(contract.and_then(|c| c.get("sandbox_path")))
        {
            return Some(json!({
                "next_agent": "git-manager",
                "next_action": "git-manager.worktree.create",
                "reason": "ANTILOKI_WORKTREE on → create a git worktree build env (full checkout) before DEV",
                "terminal": false,
                "matched_rule": "contract-tl-approved-needs-worktree"
            }));
        }
    }

    // contract-tl-approved-no-sandbox
    if contract_status == Some("tl_approved")
        && truthy(contract.and_then(|c| c.get("man_phase1_passed")))
        && !truthy(contract.and_then(|c| c.get("sandbox_path")))
        && !truthy(contract.and_then(|c| c.get("worktree_path")))
    {
        return Some(json!({
            "next_agent": "tl",
            "next_action": "tl.sandbox.create",
            "reason": "TL approved + MAN Phase 1 passed → create sandbox before DEV",
            "terminal": false,
            "matched_rule": "contract-tl-approved-no-sandbox"
        }));
    }

    // contract-tl-approved
    if contract_status == Some("tl_approved")
        && truthy(contract.and_then(|c| c.get("man_phase1_passed")))
        && (truthy(contract.and_then(|c| c.get("sandbox_path")))
            || truthy(contract.and_then(|c| c.get("worktree_path"))))
    {
        return Some(json!({
            "next_agent": "git-manager",
            "next_action": "git-manager.implement.code",
            "reason": "TL approved + MAN Phase 1 passed + build env ready (sandbox or worktree) → DEV implements",
            "terminal": false,
            "matched_rule": "contract-tl-approved"
        }));
    }

    // contract-human-approved
    if contract_status == Some("human_approved") {
        return Some(json!({
            "next_agent": "git-manager",
            "next_action": "git-manager.implement.code",
            "reason": "human approved → DEV implements",
            "terminal": false,
            "matched_rule": "contract-human-approved"
        }));
    }

    // contract-impl-needs-build-gate
    let build_gate_fresh = truthy(facts.get("buildGateFresh"));
    if contract_status == Some("impl") && !build_gate_fresh {
        return Some(json!({
            "next_agent": "git-manager",
            "next_action": "git-manager.build.gate",
            "reason": "DEV applied (status=impl) → run the deterministic build gate before QA",
            "terminal": false,
            "matched_rule": "contract-impl-needs-build-gate"
        }));
    }

    // contract-impl-build-gate-failed-retry
    let last_build_gate = facts.get("lastBuildGate").filter(|v| !v.is_null());
    {
        let build_gate_reauthors = facts
            .get("buildGateReauthors")
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        if contract_status == Some("impl")
            && build_gate_fresh
            && num_eq(last_build_gate.and_then(|v| v.get("passed")), 0)
            && build_gate_reauthors < MAX_DEV_RETRIES
        {
            return Some(json!({
                "next_agent": "tl",
                "next_action": "tl.author.snippets",
                "reason": "build gate RED → TL re-authors snippets against the real compiler errors before QA runs (no re-plan)",
                "terminal": false,
                "matched_rule": "contract-impl-build-gate-failed-retry"
            }));
        }
    }

    // contract-impl-build-gate-failed-escalate
    {
        let build_gate_reauthors = facts
            .get("buildGateReauthors")
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        if contract_status == Some("impl")
            && build_gate_fresh
            && num_eq(last_build_gate.and_then(|v| v.get("passed")), 0)
            && build_gate_reauthors >= MAX_DEV_RETRIES
        {
            return Some(json!({
                "next_agent": null,
                "next_action": null,
                "reason": format!("escalate-to-human: build gate still RED after {} re-authors — the real compiler error is in the latest build_gate_runs.evidence_md", MAX_DEV_RETRIES),
                "terminal": true,
                "matched_rule": "contract-impl-build-gate-failed-escalate"
            }));
        }
    }

    // contract-impl-no-qa
    let last_qa_run = facts.get("lastQaRun").filter(|v| !v.is_null());
    if contract_status == Some("impl")
        && !truthy(last_qa_run)
        && build_gate_fresh
        && num_eq(last_build_gate.and_then(|v| v.get("passed")), 1)
    {
        return Some(json!({
            "next_agent": "qa",
            "next_action": "qa.gate.contract.verify",
            "reason": "DEV applied + build gate GREEN → QA verifies",
            "terminal": false,
            "matched_rule": "contract-impl-no-qa"
        }));
    }

    // contract-impl-qa-escalation-reason-set
    if contract_status == Some("impl")
        && num_eq(last_qa_run.and_then(|v| v.get("all_passed")), 0)
        && truthy(last_qa_run.and_then(|v| v.get("escalation_reason")))
    {
        return Some(json!({
            "next_agent": null,
            "next_action": null,
            "reason": "escalate-to-human: QA set escalation_reason (e.g. tooling failure outside contract scope)",
            "terminal": true,
            "matched_rule": "contract-impl-qa-escalation-reason-set"
        }));
    }

    // contract-impl-qa-failed-no-more-work-escalate
    {
        let microtasks_total = contract
            .and_then(|c| c.get("microtasks_total"))
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        let microtasks_remaining = contract
            .and_then(|c| c.get("microtasks_remaining"))
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        if contract_status == Some("impl")
            && num_eq(last_qa_run.and_then(|v| v.get("all_passed")), 0)
            && microtasks_total > 0
            && microtasks_remaining == 0
            && !truthy(contract.and_then(|c| c.get("has_unconsumed_qa_suggestions")))
            && !truthy(contract.and_then(|c| c.get("has_unconsumed_amendment")))
        {
            return Some(json!({
                "next_agent": null,
                "next_action": null,
                "reason": "escalate-to-human: QA failed AND DEV finished every microtask AND no fresh QA suggestions to consume — no remediation work left",
                "terminal": true,
                "matched_rule": "contract-impl-qa-failed-no-more-work-escalate"
            }));
        }
    }

    // contract-impl-qa-failed-amendment-to-tl
    {
        let amendments_applied = contract
            .and_then(|c| c.get("amendments_applied"))
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        if contract_status == Some("impl")
            && num_eq(last_qa_run.and_then(|v| v.get("all_passed")), 0)
            && truthy(contract.and_then(|c| c.get("has_unconsumed_amendment")))
            && amendments_applied < TL_MAX_AMENDMENTS
        {
            return Some(json!({
                "next_agent": "tl",
                "next_action": "tl.contract.amend",
                "reason": "QA emitted contract-amendment microtask → TL applies scope change before DEV retries",
                "terminal": false,
                "matched_rule": "contract-impl-qa-failed-amendment-to-tl"
            }));
        }
    }

    // contract-impl-amendment-cap-reached-escalate
    {
        let amendments_applied = contract
            .and_then(|c| c.get("amendments_applied"))
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        if contract_status == Some("impl")
            && num_eq(last_qa_run.and_then(|v| v.get("all_passed")), 0)
            && truthy(contract.and_then(|c| c.get("has_unconsumed_amendment")))
            && amendments_applied >= TL_MAX_AMENDMENTS
        {
            return Some(json!({
                "next_agent": null,
                "next_action": null,
                "reason": format!("escalate-to-human: TL_MAX_AMENDMENTS={} reached — contract has needed scope changes too many times", TL_MAX_AMENDMENTS),
                "terminal": true,
                "matched_rule": "contract-impl-amendment-cap-reached-escalate"
            }));
        }
    }

    None
}

// gap fix: dropped at the D/E chunk boundary during the 9-way parallel port (rule #56).
fn try_rule_contract_impl_qa_failed_retry(facts: &Value) -> Option<Value> {
    let contract = facts.get("contract");
    let last_qa_run_all_passed_0 =
        facts.get("lastQaRun").and_then(|l| l.get("all_passed")).and_then(|v| v.as_i64()) == Some(0);
    let dev_attempts = facts.get("devAttempts").and_then(|v| v.as_i64());
    let reauthor_attempts = nz(facts.get("reauthorAttempts")).and_then(|v| v.as_i64()).unwrap_or(0);
    if contract.and_then(|c| c.get("status")).and_then(|v| v.as_str()) == Some("impl")
        && last_qa_run_all_passed_0
        && dev_attempts.map_or(false, |d| d < MAX_DEV_RETRIES)
        && reauthor_attempts < MAX_DEV_RETRIES
    {
        return Some(json!({
            "next_agent": "tl",
            "next_action": "tl.author.snippets",
            "reason": "QA failed → TL re-authors snippets (DEV is deterministic; a DEV replay would repeat the failure; no re-plan)",
            "terminal": false,
            "matched_rule": "contract-impl-qa-failed-retry"
        }));
    }
    None
}

fn try_rules_v1_chunk_e(facts: &Value) -> Option<Value> {
    let contract = facts.get("contract");
    let contract_status = contract.and_then(|c| c.get("status")).and_then(|s| s.as_str());
    let card = facts.get("card");
    let card_status = card.and_then(|c| c.get("status")).and_then(|s| s.as_str());
    let post_qa = facts.get("postQa");
    let post_qa_man = post_qa.and_then(|p| p.get("man"));
    let post_qa_keeper = post_qa.and_then(|p| p.get("keeper"));
    let post_qa_journey = post_qa.and_then(|p| p.get("journey"));
    let post_qa_blueprint = post_qa.and_then(|p| p.get("blueprint"));
    let post_qa_atlas = post_qa.and_then(|p| p.get("atlas"));

    // contract-impl-qa-failed-escalate
    {
        let last_qa_run_all_passed_0 = facts
            .get("lastQaRun")
            .and_then(|l| l.get("all_passed"))
            .and_then(|v| v.as_i64())
            == Some(0);
        let dev_attempts = facts.get("devAttempts").and_then(|v| v.as_i64());
        let reauthor_attempts = facts.get("reauthorAttempts").and_then(|v| v.as_i64()).unwrap_or(0);
        if contract_status == Some("impl")
            && last_qa_run_all_passed_0
            && (dev_attempts.map_or(false, |d| d >= MAX_DEV_RETRIES) || reauthor_attempts >= MAX_DEV_RETRIES)
        {
            return Some(json!({
                "next_agent": null,
                "next_action": null,
                "reason": format!("escalate-to-human: {} retries exhausted (dev re-applies OR TL re-authors)", MAX_DEV_RETRIES),
                "terminal": true,
                "matched_rule": "contract-impl-qa-failed-escalate"
            }));
        }
    }

    // contract-qa
    if contract_status == Some("qa") {
        return Some(json!({
            "next_agent": "qa",
            "next_action": "qa.gate.contract.verify",
            "reason": "contract at QA gate",
            "terminal": false,
            "matched_rule": "contract-qa"
        }));
    }

    // contract-done-needs-pr
    {
        let worktree_path_truthy = truthy(contract.and_then(|c| c.get("worktree_path")));
        let card_auto_merge = truthy(facts.get("cardAutoMerge"));
        let driver_pr_open = truthy(facts.get("driverPrOpen"));
        let last_pending_contract_not_false = facts.get("lastPendingContract") != Some(&Value::Bool(false));
        if contract_status == Some("done")
            && worktree_path_truthy
            && !card_auto_merge
            && !driver_pr_open
            && last_pending_contract_not_false
        {
            return Some(json!({
                "next_agent": "git-manager",
                "next_action": "git-manager.pr.open",
                "reason": "final QA green → open the card's pull request for operator review (auto-merge is off)",
                "terminal": false,
                "matched_rule": "contract-done-needs-pr"
            }));
        }
    }

    // contract-done-pr-pending
    {
        let worktree_path_truthy = truthy(contract.and_then(|c| c.get("worktree_path")));
        let card_auto_merge = truthy(facts.get("cardAutoMerge"));
        let driver_pr_open = truthy(facts.get("driverPrOpen"));
        let last_pending_contract_not_false = facts.get("lastPendingContract") != Some(&Value::Bool(false));
        if contract_status == Some("done")
            && worktree_path_truthy
            && !card_auto_merge
            && driver_pr_open
            && last_pending_contract_not_false
        {
            return Some(json!({
                "next_agent": null,
                "next_action": null,
                "reason": "pull request open — merge it from the Pulls screen (or enable auto-merge on the card)",
                "terminal": false,
                "matched_rule": "contract-done-pr-pending"
            }));
        }
    }

    // contract-done-needs-worktree-merge
    {
        let worktree_path_truthy = truthy(contract.and_then(|c| c.get("worktree_path")));
        let card_auto_merge = truthy(facts.get("cardAutoMerge"));
        if contract_status == Some("done") && worktree_path_truthy && card_auto_merge {
            return Some(json!({
                "next_agent": "git-manager",
                "next_action": "git-manager.worktree.merge",
                "reason": "QA green + worktree build env + auto-merge on → merge worktree back to base before closing",
                "terminal": false,
                "matched_rule": "contract-done-needs-worktree-merge"
            }));
        }
    }

    // contract-done-needs-man-review
    if contract_status == Some("done") && truthy(post_qa) && !truthy(post_qa_man) {
        return Some(json!({
            "next_agent": "man",
            "next_action": "man.card.review",
            "reason": "contract done → MAN audits the journey",
            "terminal": false,
            "matched_rule": "contract-done-needs-man-review"
        }));
    }

    // contract-done-needs-keeper-consolidate
    if contract_status == Some("done") && truthy(post_qa_man) && !truthy(post_qa_keeper) {
        return Some(json!({
            "next_agent": "keeper",
            "next_action": "keeper.card.consolidate",
            "reason": "MAN reviewed → Keeper distills lessons",
            "terminal": false,
            "matched_rule": "contract-done-needs-keeper-consolidate"
        }));
    }

    // contract-done-needs-journey-append
    if contract_status == Some("done")
        && truthy(post_qa_man)
        && truthy(post_qa_keeper)
        && !truthy(post_qa_journey)
    {
        return Some(json!({
            "next_agent": "keeper",
            "next_action": "keeper.journey.append",
            "reason": "lesson consolidated → append to project_journey",
            "terminal": false,
            "matched_rule": "contract-done-needs-journey-append"
        }));
    }

    // contract-done-needs-review-blueprint
    if contract_status == Some("done")
        && truthy(post_qa_man)
        && truthy(post_qa_keeper)
        && truthy(post_qa_journey)
        && !truthy(post_qa_blueprint)
    {
        return Some(json!({
            "next_agent": "board-keeper",
            "next_action": "board-keeper.card.review.blueprint.generate",
            "reason": "journey appended → freeze the review blueprint",
            "terminal": false,
            "matched_rule": "contract-done-needs-review-blueprint"
        }));
    }

    // contract-done-needs-atlas-integrate
    if contract_status == Some("done")
        && card_status != Some("done")
        && truthy(facts.get("atlasPopulated"))
        && truthy(post_qa_man)
        && truthy(post_qa_keeper)
        && truthy(post_qa_journey)
        && truthy(post_qa_blueprint)
        && !truthy(post_qa_atlas)
    {
        return Some(json!({
            "next_agent": "keeper",
            "next_action": "system.codebase.integrate",
            "reason": "post-QA reflections done → fold the built code into the atlas",
            "terminal": false,
            "matched_rule": "contract-done-needs-atlas-integrate"
        }));
    }

    // contract-done-card-needs-close
    if contract_status == Some("done")
        && card_status != Some("done")
        && truthy(post_qa_man)
        && truthy(post_qa_keeper)
        && truthy(post_qa_journey)
        && truthy(post_qa_blueprint)
    {
        return Some(json!({
            "next_agent": "board-keeper",
            "next_action": "board-keeper.card.status.change",
            "reason": "post-QA chain complete (MAN + keeper + journey + blueprint) → close the card",
            "terminal": false,
            "matched_rule": "contract-done-card-needs-close"
        }));
    }

    // contract-done
    if contract_status == Some("done")
        && (!truthy(post_qa)
            || (truthy(post_qa_man) && truthy(post_qa_keeper) && truthy(post_qa_journey) && truthy(post_qa_blueprint)))
    {
        return Some(json!({
            "next_agent": null,
            "next_action": null,
            "reason": "contract done — card closes",
            "terminal": true,
            "matched_rule": "contract-done"
        }));
    }

    // contract-rejected-or-superseded
    if contract_status == Some("rejected") || contract_status == Some("superseded") {
        return Some(json!({
            "next_agent": null,
            "next_action": null,
            "reason": "contract rejected or superseded",
            "terminal": true,
            "matched_rule": "contract-rejected-or-superseded"
        }));
    }

    None
}

// ── RULES_V2 chunks, in original array order ─────────────────────────────────────────────────

fn try_rules_v2_chunk_f(facts: &Value) -> Option<Value> {
    let card = facts.get("card");
    let card_status = card.and_then(|c| c.get("status")).and_then(|v| v.as_str());
    let contract = facts.get("contract").filter(|v| !v.is_null());
    let contract_status = contract.and_then(|c| c.get("status")).and_then(|v| v.as_str());
    let contract_truthy = truthy(facts.get("contract"));

    // repeat-failure-escalate
    let recent_consecutive_failures = facts
        .get("recentConsecutiveFailures")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    if recent_consecutive_failures >= MAX_CONSECUTIVE_IDENTICAL_FAILURES {
        let sig = facts
            .get("recentFailureSignature")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let truncated: String = sig.chars().take(60).collect();
        return Some(json!({
            "next_agent": null,
            "next_action": null,
            "reason": format!(
                "escalate-to-human: {} consecutive ticks failed with identical error signature '{}…' — daemon stopped dispatching to break the loop",
                tmpl(facts.get("recentConsecutiveFailures")),
                truncated
            ),
            "terminal": true,
            "matched_rule": "repeat-failure-escalate"
        }));
    }

    // card-done
    if card_status == Some("done") {
        return Some(json!({
            "next_agent": null,
            "next_action": null,
            "reason": "card closed",
            "terminal": true,
            "matched_rule": "card-done"
        }));
    }

    // card-blocked
    if card_status == Some("blocked") {
        return Some(json!({
            "next_agent": null,
            "next_action": null,
            "reason": "card blocked — human review needed",
            "terminal": true,
            "matched_rule": "card-blocked"
        }));
    }

    // v2-card-has-children-awaiting
    let card_children = facts
        .get("cardChildren")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    if !contract_truthy && card_status == Some("backlog") && card_children > 0 {
        return Some(json!({
            "next_agent": null,
            "next_action": null,
            "reason": "card already decomposed — work happens on children",
            "terminal": false,
            "matched_rule": "v2-card-has-children-awaiting"
        }));
    }

    // v2-needs-screening
    let card_priority = card
        .and_then(|c| c.get("priority"))
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let card_driver_intent = card.and_then(|c| c.get("driver_intent")).and_then(|v| v.as_str());
    let v2_screening_fresh = truthy(facts.get("v2ScreeningFresh"));
    if !contract_truthy
        && card_status == Some("backlog")
        && (card_priority >= 5 || card_driver_intent == Some("play"))
        && !v2_screening_fresh
    {
        return Some(json!({
            "next_agent": "pl",
            "next_action": "pl.screening",
            "reason": "v2: prioritized card → PL screens it into enriched context + acceptance S-tasks",
            "terminal": false,
            "matched_rule": "v2-needs-screening"
        }));
    }

    // v2-awaiting-user-start
    if !contract_truthy
        && card_status == Some("backlog")
        && card_priority < 5
        && card_driver_intent != Some("play")
        && !v2_screening_fresh
    {
        return Some(json!({
            "next_agent": null,
            "next_action": null,
            "reason": "v2: card not user-prioritized — type `do <slug>` in chat to start",
            "terminal": false,
            "matched_rule": "v2-awaiting-user-start"
        }));
    }

    // v2-screened-needs-contract
    if !contract_truthy && card_status == Some("backlog") && v2_screening_fresh {
        return Some(json!({
            "next_agent": "tl",
            "next_action": "tl.contract.assemble",
            "reason": "v2: screening done → TL assembles the contract (deterministic, card+screening)",
            "terminal": false,
            "matched_rule": "v2-screened-needs-contract"
        }));
    }

    // v2-contract-proposed
    if contract_status == Some("proposed") {
        return Some(json!({
            "next_agent": "pl",
            "next_action": "pl.ratify.contract",
            "reason": "v2: contract proposed → PL ratifies (compare contract acceptance vs card acceptance)",
            "terminal": false,
            "matched_rule": "v2-contract-proposed"
        }));
    }

    // v2-ratified-needs-design
    let v2_blueprint_active = truthy(facts.get("v2BlueprintActive"));
    if contract_status == Some("pl_ratified") && !v2_blueprint_active {
        return Some(json!({
            "next_agent": "tl",
            "next_action": "blueprint.design",
            "reason": "v2: ratified → TL designs the blueprint (blocks/bindings), gated by lint+coverage",
            "terminal": false,
            "matched_rule": "v2-ratified-needs-design"
        }));
    }

    // v2-design-needs-judgment
    let v2_design_judge_fresh = truthy(facts.get("v2DesignJudgeFresh"));
    if contract_status == Some("pl_ratified") && v2_blueprint_active && !v2_design_judge_fresh {
        return Some(json!({
            "next_agent": "qa",
            "next_action": "blueprint.judge.design",
            "reason": "v2: blueprint designed → QA judges the design (counting court)",
            "terminal": false,
            "matched_rule": "v2-design-needs-judgment"
        }));
    }

    // v2-design-judge-failed-redesign
    let v2_design_judge_passed = facts.get("v2DesignJudgePassed");
    let v2_design_judge_passed_false = matches!(v2_design_judge_passed, Some(Value::Bool(false)));
    let v2_design_judge_passed_true = matches!(v2_design_judge_passed, Some(Value::Bool(true)));
    let v2_design_court_rejections = facts
        .get("v2DesignCourtRejections")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let v2_design_versions = facts
        .get("v2DesignVersions")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let status_ratified_or_approved =
        contract_status == Some("pl_ratified") || contract_status == Some("tl_approved");
    if status_ratified_or_approved
        && v2_design_judge_fresh
        && v2_design_judge_passed_false
        && v2_design_court_rejections < MAX_V2_DESIGN_COURT_REJECTIONS
        && v2_design_versions < MAX_V2_DESIGN_VERSIONS_HARD
    {
        return Some(json!({
            "next_agent": "tl",
            "next_action": "blueprint.design",
            "reason": "v2: design judged RED → TL re-designs the blueprint (bounded by cause)",
            "terminal": false,
            "matched_rule": "v2-design-judge-failed-redesign"
        }));
    }

    // v2-design-stuck-escalate
    if status_ratified_or_approved
        && v2_design_judge_fresh
        && v2_design_judge_passed_false
        && (v2_design_court_rejections >= MAX_V2_DESIGN_COURT_REJECTIONS
            || v2_design_versions >= MAX_V2_DESIGN_VERSIONS_HARD)
    {
        let reason = if v2_design_court_rejections >= MAX_V2_DESIGN_COURT_REJECTIONS {
            format!(
                "v2: the court rejected {} distinct designs on the merits — a third redesign won't fix what two didn't; escalate to a human",
                tmpl(facts.get("v2DesignCourtRejections"))
            )
        } else {
            format!(
                "v2: {} designs without a green judgment (mechanical gate REDs) — past the {}-design hard ceiling; escalate to a human",
                tmpl(facts.get("v2DesignVersions")),
                MAX_V2_DESIGN_VERSIONS_HARD
            )
        };
        return Some(json!({
            "next_agent": null,
            "next_action": null,
            "reason": reason,
            "terminal": true,
            "matched_rule": "v2-design-stuck-escalate"
        }));
    }

    // v2-design-judged-needs-plan-seal
    if contract_status == Some("pl_ratified") && v2_design_judge_fresh && v2_design_judge_passed_true {
        return Some(json!({
            "next_agent": "man",
            "next_action": "blueprint.plan.seal",
            "reason": "v2: design judged GREEN → MAN seals the PLAN session (pl_ratified → tl_approved)",
            "terminal": false,
            "matched_rule": "v2-design-judged-needs-plan-seal"
        }));
    }

    // v2-approved-needs-worktree
    let worktree_enabled = truthy(facts.get("worktreeEnabled"));
    let contract_worktree_path = contract.and_then(|c| c.get("worktree_path"));
    let contract_sandbox_path = contract.and_then(|c| c.get("sandbox_path"));
    if contract_status == Some("tl_approved")
        && worktree_enabled
        && !truthy(contract_worktree_path)
        && !truthy(contract_sandbox_path)
    {
        return Some(json!({
            "next_agent": "git-manager",
            "next_action": "git-manager.worktree.create",
            "reason": "v2: create the worktree build env (full checkout) before authoring",
            "terminal": false,
            "matched_rule": "v2-approved-needs-worktree"
        }));
    }

    None
}

fn try_rules_v2_chunk_g(facts: &Value) -> Option<Value> {
    let contract = facts.get("contract");
    let contract_status = contract.and_then(|c| c.get("status")).and_then(|v| v.as_str());
    let sandbox_path = contract.and_then(|c| c.get("sandbox_path"));
    let worktree_path = contract.and_then(|c| c.get("worktree_path"));

    // v2-approved-needs-sandbox
    if contract_status == Some("tl_approved") && !truthy(sandbox_path) && !truthy(worktree_path) {
        return Some(json!({
            "next_agent": "tl",
            "next_action": "tl.sandbox.create",
            "reason": "v2: create the sandbox build env before authoring",
            "terminal": false,
            "matched_rule": "v2-approved-needs-sandbox"
        }));
    }

    // v2-env-needs-baseline-build
    if contract_status == Some("tl_approved")
        && (truthy(sandbox_path) || truthy(worktree_path))
        && !truthy(facts.get("v2BaselineBuildDone"))
    {
        return Some(json!({
            "next_agent": "git-manager",
            "next_action": "git-manager.build.baseline",
            "reason": "v2: build env ready → BASELINE build on the pristine worktree (before any code)",
            "terminal": false,
            "matched_rule": "v2-env-needs-baseline-build"
        }));
    }

    // v2-env-needs-baseline-tests
    if contract_status == Some("tl_approved")
        && (truthy(sandbox_path) || truthy(worktree_path))
        && truthy(facts.get("v2BaselineBuildDone"))
        && !truthy(facts.get("v2BaselineTestDone"))
    {
        return Some(json!({
            "next_agent": "git-manager",
            "next_action": "git-manager.test.baseline",
            "reason": "v2: baseline build recorded → BASELINE test run on the pristine worktree",
            "terminal": false,
            "matched_rule": "v2-env-needs-baseline-tests"
        }));
    }

    // v2-approved-needs-author
    if contract_status == Some("tl_approved")
        && (truthy(sandbox_path) || truthy(worktree_path))
        && !truthy(facts.get("v2SnippetsAuthored"))
        && truthy(facts.get("v2DesignJudgeFresh"))
    {
        return Some(json!({
            "next_agent": "git-manager",
            "next_action": "snippet.author",
            "reason": "v2: build env ready → DEV authors the per-block snippets (LOW swarm)",
            "terminal": false,
            "matched_rule": "v2-approved-needs-author"
        }));
    }

    // v2-snippets-need-judgment
    if contract_status == Some("tl_approved")
        && truthy(facts.get("v2SnippetsAuthored"))
        && !truthy(facts.get("v2SnippetJudgeFresh"))
    {
        return Some(json!({
            "next_agent": "qa",
            "next_action": "blueprint.judge.snippets",
            "reason": "v2: snippets authored → QA judges the code (court) + deterministic count gate",
            "terminal": false,
            "matched_rule": "v2-snippets-need-judgment"
        }));
    }

    // v2-snippet-defect-is-design-defect-redesign
    {
        let v2_design_versions = nz(facts.get("v2DesignVersions")).and_then(|v| v.as_i64()).unwrap_or(0);
        if contract_status == Some("tl_approved")
            && truthy(facts.get("v2SnippetJudgeFresh"))
            && matches!(facts.get("v2SnippetJudgePassed"), Some(Value::Bool(false)))
            && matches!(facts.get("v2SnippetDefectInBlueprint"), Some(Value::Bool(true)))
            && v2_design_versions < MAX_V2_DESIGN_VERSIONS_HARD
        {
            return Some(json!({
                "next_agent": "tl",
                "next_action": "blueprint.design",
                "reason": "v2: the upheld snippet defect originates IN the blueprint (its literal sits in the design) — TL redesigns; re-authoring cannot fix a design-level constant",
                "terminal": false,
                "matched_rule": "v2-snippet-defect-is-design-defect-redesign"
            }));
        }
    }

    // v2-approved-fresh-design-needs-judgment
    if contract_status == Some("tl_approved")
        && truthy(facts.get("v2BlueprintActive"))
        && !truthy(facts.get("v2DesignJudgeFresh"))
        && !truthy(facts.get("v2SnippetsAuthored"))
    {
        return Some(json!({
            "next_agent": "qa",
            "next_action": "blueprint.judge.design",
            "reason": "v2: redesigned at build stage → QA judges the new design before authoring",
            "terminal": false,
            "matched_rule": "v2-approved-fresh-design-needs-judgment"
        }));
    }

    // v2-snippet-judge-failed-reauthor
    {
        let v2_author_versions = nz(facts.get("v2AuthorVersions")).and_then(|v| v.as_i64()).unwrap_or(0);
        if contract_status == Some("tl_approved")
            && truthy(facts.get("v2SnippetJudgeFresh"))
            && matches!(facts.get("v2SnippetJudgePassed"), Some(Value::Bool(false)))
            && v2_author_versions < MAX_V2_AUTHOR_RETRIES
        {
            return Some(json!({
                "next_agent": "git-manager",
                "next_action": "snippet.author",
                "reason": "v2: code judged RED → DEV re-authors the failing blocks (bounded)",
                "terminal": false,
                "matched_rule": "v2-snippet-judge-failed-reauthor"
            }));
        }
    }

    // v2-snippet-stuck-escalate
    {
        let v2_author_versions = nz(facts.get("v2AuthorVersions")).and_then(|v| v.as_i64()).unwrap_or(0);
        if contract_status == Some("tl_approved")
            && truthy(facts.get("v2SnippetJudgeFresh"))
            && matches!(facts.get("v2SnippetJudgePassed"), Some(Value::Bool(false)))
            && v2_author_versions >= MAX_V2_AUTHOR_RETRIES
        {
            return Some(json!({
                "next_agent": null,
                "next_action": null,
                "reason": format!("v2: code judged RED past the {}-reauthor budget — escalate to a human", MAX_V2_AUTHOR_RETRIES),
                "terminal": true,
                "matched_rule": "v2-snippet-stuck-escalate"
            }));
        }
    }

    // v2-snippets-judged-needs-apply
    if contract_status == Some("tl_approved")
        && truthy(facts.get("v2SnippetJudgeFresh"))
        && matches!(facts.get("v2SnippetJudgePassed"), Some(Value::Bool(true)))
        && (truthy(facts.get("v2BaselineTestDone")) || (!truthy(sandbox_path) && !truthy(worktree_path)))
    {
        return Some(json!({
            "next_agent": "git-manager",
            "next_action": "blueprint.apply",
            "reason": "v2: code judged GREEN → DEV applies the snippets to real files (tl_approved → impl)",
            "terminal": false,
            "matched_rule": "v2-snippets-judged-needs-apply"
        }));
    }

    // v2-impl-needs-build-gate
    if contract_status == Some("impl") && !truthy(facts.get("buildGateFresh")) {
        return Some(json!({
            "next_agent": "git-manager",
            "next_action": "git-manager.build.gate",
            "reason": "v2: applied (status=impl) → deterministic build gate before QA",
            "terminal": false,
            "matched_rule": "v2-impl-needs-build-gate"
        }));
    }

    None
}

fn try_rules_v2_chunk_h(facts: &Value) -> Option<Value> {
    let contract = facts.get("contract");
    let contract_status = contract.and_then(|c| c.get("status")).and_then(|v| v.as_str());
    let contract_worktree_truthy = truthy(contract.and_then(|c| c.get("worktree_path")));

    // v2-impl-build-gate-failed-reauthor
    {
        let build_gate_fresh = truthy(facts.get("buildGateFresh"));
        let last_build_gate_passed = facts.get("lastBuildGate").and_then(|v| v.get("passed")).and_then(|v| v.as_i64());
        let build_gate_reauthors = facts.get("buildGateReauthors").and_then(|v| v.as_i64()).unwrap_or(0);
        if contract_status == Some("impl") && build_gate_fresh && last_build_gate_passed == Some(0)
            && build_gate_reauthors < MAX_V2_AUTHOR_RETRIES {
            return Some(json!({
                "next_agent": "git-manager",
                "next_action": "snippet.author",
                "reason": "v2: build gate RED → DEV re-authors against the real compiler errors (bounded)",
                "terminal": false,
                "matched_rule": "v2-impl-build-gate-failed-reauthor"
            }));
        }
    }

    // v2-impl-build-gate-failed-escalate
    {
        let build_gate_fresh = truthy(facts.get("buildGateFresh"));
        let last_build_gate_passed = facts.get("lastBuildGate").and_then(|v| v.get("passed")).and_then(|v| v.as_i64());
        let build_gate_reauthors = facts.get("buildGateReauthors").and_then(|v| v.as_i64()).unwrap_or(0);
        if contract_status == Some("impl") && build_gate_fresh && last_build_gate_passed == Some(0)
            && build_gate_reauthors >= MAX_V2_AUTHOR_RETRIES {
            return Some(json!({
                "next_agent": Value::Null,
                "next_action": Value::Null,
                "reason": format!("escalate-to-human: build gate still RED after {} failed gate runs — compiler error in the latest build_gate_runs.evidence_md", MAX_V2_AUTHOR_RETRIES),
                "terminal": true,
                "matched_rule": "v2-impl-build-gate-failed-escalate"
            }));
        }
    }

    // v2-impl-build-green-needs-test-gate
    {
        let build_gate_fresh = truthy(facts.get("buildGateFresh"));
        let last_build_gate_passed = facts.get("lastBuildGate").and_then(|v| v.get("passed")).and_then(|v| v.as_i64());
        let test_gate_fresh = truthy(facts.get("testGateFresh"));
        if contract_status == Some("impl") && build_gate_fresh && last_build_gate_passed == Some(1) && !test_gate_fresh {
            return Some(json!({
                "next_agent": "git-manager",
                "next_action": "git-manager.test.gate",
                "reason": "v2: build GREEN → deterministic test gate (workspace test script) before QA",
                "terminal": false,
                "matched_rule": "v2-impl-build-green-needs-test-gate"
            }));
        }
    }

    // v2-impl-test-gate-failed-reauthor
    {
        let test_gate_fresh = truthy(facts.get("testGateFresh"));
        let last_test_gate_passed = facts.get("lastTestGate").and_then(|v| v.get("passed")).and_then(|v| v.as_i64());
        let test_gate_reds = facts.get("testGateReds").and_then(|v| v.as_i64()).unwrap_or(0);
        if contract_status == Some("impl") && test_gate_fresh && last_test_gate_passed == Some(0)
            && test_gate_reds < MAX_V2_AUTHOR_RETRIES {
            return Some(json!({
                "next_agent": "git-manager",
                "next_action": "snippet.author",
                "reason": "v2: test gate RED → DEV re-authors against the real test failures (bounded)",
                "terminal": false,
                "matched_rule": "v2-impl-test-gate-failed-reauthor"
            }));
        }
    }

    // v2-impl-test-gate-failed-escalate
    {
        let test_gate_fresh = truthy(facts.get("testGateFresh"));
        let last_test_gate_passed = facts.get("lastTestGate").and_then(|v| v.get("passed")).and_then(|v| v.as_i64());
        let test_gate_reds = facts.get("testGateReds").and_then(|v| v.as_i64()).unwrap_or(0);
        if contract_status == Some("impl") && test_gate_fresh && last_test_gate_passed == Some(0)
            && test_gate_reds >= MAX_V2_AUTHOR_RETRIES {
            return Some(json!({
                "next_agent": Value::Null,
                "next_action": Value::Null,
                "reason": format!("escalate-to-human: test gate still RED after {} failed runs — test output in the latest test_gate_runs.evidence_md", MAX_V2_AUTHOR_RETRIES),
                "terminal": true,
                "matched_rule": "v2-impl-test-gate-failed-escalate"
            }));
        }
    }

    // v2-impl-tests-green-needs-bugfinder
    {
        let last_qa_run_truthy = truthy(facts.get("lastQaRun"));
        let build_gate_fresh = truthy(facts.get("buildGateFresh"));
        let last_build_gate_passed = facts.get("lastBuildGate").and_then(|v| v.get("passed")).and_then(|v| v.as_i64());
        let test_gate_fresh = truthy(facts.get("testGateFresh"));
        let last_test_gate_passed = facts.get("lastTestGate").and_then(|v| v.get("passed")).and_then(|v| v.as_i64());
        let bugfinder_attempted_is_false = matches!(facts.get("bugfinderAttempted"), Some(Value::Bool(false)));
        if contract_status == Some("impl") && !last_qa_run_truthy && build_gate_fresh && last_build_gate_passed == Some(1)
            && test_gate_fresh && last_test_gate_passed == Some(1) && bugfinder_attempted_is_false {
            return Some(json!({
                "next_agent": "system",
                "next_action": "system.bugfinder.run",
                "reason": "v2: build + tests GREEN → bug finder (3 waves, advisory — records findings, never blocks; QA follows next tick)",
                "terminal": false,
                "matched_rule": "v2-impl-tests-green-needs-bugfinder"
            }));
        }
    }

    // v2-impl-needs-qa
    {
        let last_qa_run_truthy = truthy(facts.get("lastQaRun"));
        let build_gate_fresh = truthy(facts.get("buildGateFresh"));
        let last_build_gate_passed = facts.get("lastBuildGate").and_then(|v| v.get("passed")).and_then(|v| v.as_i64());
        let test_gate_fresh = truthy(facts.get("testGateFresh"));
        let last_test_gate_passed = facts.get("lastTestGate").and_then(|v| v.get("passed")).and_then(|v| v.as_i64());
        if contract_status == Some("impl") && !last_qa_run_truthy && build_gate_fresh && last_build_gate_passed == Some(1)
            && test_gate_fresh && last_test_gate_passed == Some(1) {
            return Some(json!({
                "next_agent": "qa",
                "next_action": "qa.gate.contract.verify",
                "reason": "v2: build + tests GREEN → QA verifies (build/run/acceptance)",
                "terminal": false,
                "matched_rule": "v2-impl-needs-qa"
            }));
        }
    }

    // v2-impl-qa-failed-reauthor
    {
        let last_qa_all_passed = facts.get("lastQaRun").and_then(|v| v.get("all_passed")).and_then(|v| v.as_i64());
        let v2_author_versions = facts.get("v2AuthorVersions").and_then(|v| v.as_i64()).unwrap_or(0);
        if contract_status == Some("impl") && last_qa_all_passed == Some(0) && v2_author_versions < MAX_V2_AUTHOR_RETRIES {
            return Some(json!({
                "next_agent": "git-manager",
                "next_action": "snippet.author",
                "reason": "v2: QA RED → DEV re-authors the failing blocks (bounded)",
                "terminal": false,
                "matched_rule": "v2-impl-qa-failed-reauthor"
            }));
        }
    }

    // v2-impl-qa-failed-escalate
    {
        let last_qa_all_passed = facts.get("lastQaRun").and_then(|v| v.get("all_passed")).and_then(|v| v.as_i64());
        let v2_author_versions = facts.get("v2AuthorVersions").and_then(|v| v.as_i64()).unwrap_or(0);
        if contract_status == Some("impl") && last_qa_all_passed == Some(0) && v2_author_versions >= MAX_V2_AUTHOR_RETRIES {
            return Some(json!({
                "next_agent": Value::Null,
                "next_action": Value::Null,
                "reason": format!("v2: QA still RED after {} re-authors — escalate to a human", MAX_V2_AUTHOR_RETRIES),
                "terminal": true,
                "matched_rule": "v2-impl-qa-failed-escalate"
            }));
        }
    }

    // v2-done-needs-pr
    {
        let card_auto_merge = truthy(facts.get("cardAutoMerge"));
        let driver_pr_open = truthy(facts.get("driverPrOpen"));
        let last_pending_is_false = matches!(facts.get("lastPendingContract"), Some(Value::Bool(false)));
        if contract_status == Some("done") && contract_worktree_truthy && !card_auto_merge && !driver_pr_open && !last_pending_is_false {
            return Some(json!({
                "next_agent": "git-manager",
                "next_action": "git-manager.pr.open",
                "reason": "v2: QA green → open the card pull request for operator code review (auto-merge is off)",
                "terminal": false,
                "matched_rule": "v2-done-needs-pr"
            }));
        }
    }

    // v2-done-pr-pending
    {
        let card_auto_merge = truthy(facts.get("cardAutoMerge"));
        let driver_pr_open = truthy(facts.get("driverPrOpen"));
        let last_pending_is_false = matches!(facts.get("lastPendingContract"), Some(Value::Bool(false)));
        if contract_status == Some("done") && contract_worktree_truthy && !card_auto_merge && driver_pr_open && !last_pending_is_false {
            return Some(json!({
                "next_agent": Value::Null,
                "next_action": Value::Null,
                "reason": "v2: pull request open — merge it from the Pulls screen (or enable auto-merge on the card)",
                "terminal": false,
                "matched_rule": "v2-done-pr-pending"
            }));
        }
    }

    // v2-done-needs-worktree-merge
    {
        let card_auto_merge = truthy(facts.get("cardAutoMerge"));
        if contract_status == Some("done") && contract_worktree_truthy && card_auto_merge {
            return Some(json!({
                "next_agent": "git-manager",
                "next_action": "git-manager.worktree.merge",
                "reason": "v2: QA green + auto-merge on → merge back to base before closing",
                "terminal": false,
                "matched_rule": "v2-done-needs-worktree-merge"
            }));
        }
    }

    None
}

fn try_rules_v2_chunk_i(facts: &Value) -> Option<Value> {
    let contract = facts.get("contract");
    let contract_status = contract.and_then(|c| c.get("status")).and_then(|s| s.as_str());
    let card = facts.get("card");
    let card_status = card.and_then(|c| c.get("status")).and_then(|s| s.as_str());
    let post_qa = facts.get("postQa");

    // v2-done-needs-build-seal
    if contract_status == Some("done") && !truthy(facts.get("v2BuildSealed")) {
        return Some(json!({
            "next_agent": "man",
            "next_action": "blueprint.build.seal",
            "reason": "v2: QA green → MAN seals the BUILD session (audit trail → timeline)",
            "terminal": false,
            "matched_rule": "v2-done-needs-build-seal"
        }));
    }

    // v2-done-needs-keeper-consolidate
    if contract_status == Some("done")
        && truthy(facts.get("v2BuildSealed"))
        && !truthy(post_qa.and_then(|p| p.get("keeper")))
    {
        return Some(json!({
            "next_agent": "keeper",
            "next_action": "keeper.card.consolidate",
            "reason": "v2: BUILD sealed → Keeper distills lessons",
            "terminal": false,
            "matched_rule": "v2-done-needs-keeper-consolidate"
        }));
    }

    // v2-done-needs-journey-append
    if contract_status == Some("done")
        && truthy(facts.get("v2BuildSealed"))
        && truthy(post_qa.and_then(|p| p.get("keeper")))
        && !truthy(post_qa.and_then(|p| p.get("journey")))
    {
        return Some(json!({
            "next_agent": "keeper",
            "next_action": "keeper.journey.append",
            "reason": "v2: lessons consolidated → append to project_journey",
            "terminal": false,
            "matched_rule": "v2-done-needs-journey-append"
        }));
    }

    // v2-done-needs-atlas-integrate
    if contract_status == Some("done")
        && card_status != Some("done")
        && truthy(facts.get("atlasPopulated"))
        && truthy(facts.get("v2BuildSealed"))
        && truthy(post_qa.and_then(|p| p.get("keeper")))
        && truthy(post_qa.and_then(|p| p.get("journey")))
        && !truthy(post_qa.and_then(|p| p.get("atlas")))
    {
        return Some(json!({
            "next_agent": "keeper",
            "next_action": "system.codebase.integrate",
            "reason": "v2: reflections done → fold the built code into the atlas",
            "terminal": false,
            "matched_rule": "v2-done-needs-atlas-integrate"
        }));
    }

    // v2-done-card-needs-close
    if contract_status == Some("done")
        && card_status != Some("done")
        && truthy(facts.get("v2BuildSealed"))
        && truthy(post_qa.and_then(|p| p.get("keeper")))
        && truthy(post_qa.and_then(|p| p.get("journey")))
    {
        return Some(json!({
            "next_agent": "board-keeper",
            "next_action": "board-keeper.card.status.change",
            "reason": "v2: close chain complete (build-seal + keeper + journey) → close the card",
            "terminal": false,
            "matched_rule": "v2-done-card-needs-close"
        }));
    }

    // v2-contract-done
    if contract_status == Some("done")
        && truthy(facts.get("v2BuildSealed"))
        && truthy(post_qa.and_then(|p| p.get("keeper")))
        && truthy(post_qa.and_then(|p| p.get("journey")))
    {
        return Some(json!({
            "next_agent": null,
            "next_action": null,
            "reason": "v2: contract done — card closes",
            "terminal": true,
            "matched_rule": "v2-contract-done"
        }));
    }

    // v2-contract-rejected-escalate
    if contract_status == Some("rejected")
        && !truthy(contract.and_then(|c| c.get("superseded_by")))
        && contract.and_then(|c| c.get("failure_kind")).and_then(|v| v.as_str()) == Some("pl_ratify_rejected")
        && nz(facts.get("v2RatifyRejects")).and_then(|v| v.as_i64()).unwrap_or(0) >= 3
    {
        return Some(json!({
            "next_agent": null,
            "next_action": null,
            "reason": "escalate-to-human: PL ratify rejected 3 assemblies — the card scope and the size cap disagree; a human must decide (resize the card or trim its acceptance)",
            "terminal": true,
            "matched_rule": "v2-contract-rejected-escalate"
        }));
    }

    // v2-contract-rejected-reassemble
    if contract_status == Some("rejected")
        && !truthy(contract.and_then(|c| c.get("superseded_by")))
        && contract.and_then(|c| c.get("failure_kind")).and_then(|v| v.as_str()) == Some("pl_ratify_rejected")
        && truthy(contract.and_then(|c| c.get("rejection_reason_md")))
    {
        return Some(json!({
            "next_agent": "tl",
            "next_action": "tl.contract.assemble",
            "reason": "v2: ratify rejected → TL reassembles (cap relaxed one tier, rejection folded into the goal)",
            "terminal": false,
            "matched_rule": "v2-contract-rejected-reassemble"
        }));
    }

    // v2-contract-rejected-or-superseded
    if contract_status == Some("rejected") || contract_status == Some("superseded") {
        return Some(json!({
            "next_agent": null,
            "next_action": null,
            "reason": "contract rejected or superseded",
            "terminal": true,
            "matched_rule": "v2-contract-rejected-or-superseded"
        }));
    }

    None
}

pub fn stateMachineRules(_input: &Value, _conn: &rusqlite::Connection) -> Value {
    Value::Null // transitive — see module doc comment; never fixtured/dispatched
}

pub fn evaluateRules(facts: &Value) -> Result<Value, String> {
    // JS-throw fidelity (the db lane compares both-threw error strings byte-for-byte):
    // evaluateRules(null) throws at rule #1's destructure; any other facts shape survives rule #1
    // (plain destructure + `?? 0`) but rule #2 in BOTH tables reads `card.status` with no optional
    // chaining, so an absent/null card throws a TypeError — unless rule #1 matched first and
    // returned before any card read. The guard below mirrors that exact order; its predicate must
    // stay in lockstep with the repeat-failure-escalate blocks in chunk A / chunk F.
    if facts.is_null() {
        return Err("Cannot destructure property 'recentConsecutiveFailures' of 'object null' as it is null.".into());
    }
    let empty = Value::Object(Map::new());
    let f = if facts.is_object() { facts } else { &empty };
    let repeat_failure_matches = nz(f.get("recentConsecutiveFailures")).and_then(|v| v.as_i64()).unwrap_or(0)
        >= MAX_CONSECUTIVE_IDENTICAL_FAILURES;
    if !repeat_failure_matches {
        match f.get("card") {
            None => return Err("Cannot read properties of undefined (reading 'status')".into()),
            Some(Value::Null) => return Err("Cannot read properties of null (reading 'status')".into()),
            _ => {}
        }
    }
    let v2 = truthy(f.get("pipelineV2Enabled"));
    let matched = if v2 {
        try_rules_v2_chunk_f(f)
            .or_else(|| try_rules_v2_chunk_g(f))
            .or_else(|| try_rules_v2_chunk_h(f))
            .or_else(|| try_rules_v2_chunk_i(f))
    } else {
        try_rules_v1_chunk_a(f)
            .or_else(|| try_rules_v1_chunk_b(f))
            .or_else(|| try_rule_swarm_coverage_failed_reauthor(f))
            .or_else(|| try_rules_v1_chunk_c(f))
            .or_else(|| try_rule_contract_tl_approved_qa_review_failed_escalate(f))
            .or_else(|| try_rules_v1_chunk_d(f))
            .or_else(|| try_rule_contract_impl_qa_failed_retry(f))
            .or_else(|| try_rules_v1_chunk_e(f))
    };
    Ok(match matched {
        Some(v) => v,
        None => json!({
            "next_agent": Value::Null,
            "next_action": Value::Null,
            "reason": "no rule matched (unrecognized state combination)",
            "terminal": false,
            "matched_rule": Value::Null,
        }),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_rule_matched_default() {
        let r = evaluateRules(&json!({ "card": { "status": "in_progress" } })).unwrap();
        assert_eq!(r["matched_rule"], Value::Null);
        assert_eq!(r["terminal"], json!(false));
        assert_eq!(r["reason"], json!("no rule matched (unrecognized state combination)"));
    }

    #[test]
    fn js_throw_fidelity_on_missing_or_null_card() {
        assert_eq!(
            evaluateRules(&json!({})).unwrap_err(),
            "Cannot read properties of undefined (reading 'status')"
        );
        assert_eq!(
            evaluateRules(&json!({ "card": null })).unwrap_err(),
            "Cannot read properties of null (reading 'status')"
        );
        assert_eq!(
            evaluateRules(&Value::Null).unwrap_err(),
            "Cannot destructure property 'recentConsecutiveFailures' of 'object null' as it is null."
        );
        // rule #1 matches BEFORE any card read in JS — no throw even without a card
        let r = evaluateRules(&json!({ "recentConsecutiveFailures": 2 })).unwrap();
        assert_eq!(r["matched_rule"], json!("repeat-failure-escalate"));
    }
}
