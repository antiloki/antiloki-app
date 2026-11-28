//! review.rs — twin of skills/swarm/review.mjs (db, cross-db judgments.*). NAMES ARE CONTRACT:
//! default `review`, named `ensureReviewPrompts`, `voidDesignCategoryErrors`.
//!
//! The adversarial REVIEW SWARM for one scope: a LOW prosecutor + LOW defender each emit an
//! equal-length list of linked claims, `argument-links` validates the links (grounding floor),
//! the MID judge reads the two lists in two separate calls and grades true/false + accuracy, and
//! `judgment-score` decides by counting. Everything is persisted to judgments.sqlite.
//!
//! PARITY-DEFERRED: `debate()`'s first `crate::ai_ai_call::aiCall` call (the prosecutor debate)
//! ends in the deferred network-AI stub (always `Err` — see ai-call.rs's module doc). The JS
//! `await aiCall(...)` inside `debate()` is UNGUARDED (no try/catch), so the rejection propagates
//! straight out of `review` — this twin mirrors that with `?`. Everything up to and including
//! that first call is ported faithfully and IS the deterministic surface these tests exercise:
//! the contract/card lookups, goal derivation (`swarm_goal_derive::deriveGoal`), the deterministic
//! evidence bundle (`swarm_court_evidence_gather::courtEvidenceGather`), the phase charter, the
//! size-honesty scale note, `ensureReviewPrompts` (reusing the already-converted
//! `ai_prompt_find`/`ai_prompt_add` twins), and persona resolution
//! (`entities_agent_persona_resolve::resolvePersona`). The rest of the court — the defender/judge
//! calls, grounding (`swarm_argument_links::validateArgument`), scoring
//! (`swarm_judgment_score::scoreJudgment`), the design-phase-charter void
//! (`voidDesignCategoryErrors` — itself fully deterministic and independently unit-tested below),
//! the session-case document (`swarm_session_case::renderSessionCase`), and the judgment
//! persistence (`persist_judgment`) — are ported byte-for-byte from the JS but are unreachable by
//! this file's own fixture since they run only after a successful `aiCall`, which the deferred
//! stub never produces; correctness there is by inspection, mirroring book-run.rs / author-blocks.rs.
use regex::Regex;
use rusqlite::Connection;
use serde_json::{json, Value};
use std::sync::OnceLock;

// ── standard JS coercion helpers (playbook-standard set) ───────────────────
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
fn nz(v: Option<&Value>) -> Option<&Value> {
    v.filter(|x| !x.is_null())
}
fn query_one(db: &Connection, sql: &str, params: &[Value]) -> Result<Option<Value>, String> {
    let rows = crate::db_json::query_rows(db, sql, params)?;
    Ok(rows.as_array().and_then(|a| a.first()).cloned())
}
fn safe_json(j: Option<&str>, fallback: Value) -> Value {
    match serde_json::from_str::<Value>(j.unwrap_or("")) {
        Ok(v) if !v.is_null() => v,
        _ => fallback,
    }
}

const N_ITEMS: i64 = 3;
const PROSECUTOR: &str = "swarm.prosecutor.v1";
const DEFENDER: &str = "swarm.defender.v1";
const JUDGE: &str = "swarm.judge.v1";
const PROSECUTOR_DESIGN: &str = "swarm.prosecutor.design.v1";
const DEFENDER_DESIGN: &str = "swarm.defender.design.v1";
const JUDGE_DESIGN: &str = "swarm.judge.design.v1";

const PHASE_CHARTER_DESIGN: &str = "═══ PHASE CHARTER — you are judging the PLAN phase (blueprint design) ═══
The driver pipeline is: PL screening → TL contract → PL ratify → TL BLUEPRINT DESIGN (← you are here)
→ QA design court → DEV authors code → QA code court → apply → build gate → QA acceptance.
At THIS phase the artifact is a DESIGN STUB per file: imports + every top-level declaration with its
full signature — and every function BODY is a TODO placeholder BY CONTRACT (the code phase fills them).
VALID claims here: a missing file; a missing export/declaration/constant the contract mandates; a
mandated identifier left unnamed; a duplicate declaration across files; a contradiction between blocks;
an acceptance line no block covers.
INVALID claims here (category errors — never file them): any demand for implementation inside a body
(hook calls, localStorage.getItem/setItem calls, state wiring, logic flow) — bodies are TODO by design;
any demand for JSX/markup attributes, props, or event handlers inside components (aria-*, className,
onClick) — body content the CODE phase fills; whether an acceptance line like accessibility is COVERED
is judged against the DESIGN COVERAGE map + block intents, never against stub bodies;
any demand for runtime/test/build evidence.";

const PHASE_CHARTER_CODE: &str = "═══ PHASE CHARTER — you are judging the BUILD phase (authored code) ═══
The driver pipeline is: … TL blueprint design → QA design court → DEV AUTHORS CODE (← you are judging
its output) → apply → build gate → QA acceptance. At THIS phase the artifact is REAL code — bodies are
implemented, TODO placeholders are defects now. VALID claims: unimplemented bodies, wrong logic vs the
acceptance, missing declarations, signature drift. Runtime/test EXECUTION evidence still does not exist
(the build gate + acceptance run later) — do not demand execution proof; judge the code as written.";

fn phase_charter(is_design: bool) -> &'static str {
    if is_design { PHASE_CHARTER_DESIGN } else { PHASE_CHARTER_CODE }
}

// The deterministic half of the design charter (the prose PHASE_CHARTER_DESIGN guides; this
// enforces). A sustained prosecutor claim whose stated evidence is the TODO-ness of stub bodies
// is VOID — mirrors the grounding floor: void by rule, judge only what remains.
fn design_oob_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        let parts = [
            r"TODO\s*stubs?",
            r"stub bod(y|ies)",
            r"bod(y|ies)\s+(is|are)\s+(a\s+)?(TODO|stub)",
            r"not\s+(yet\s+)?implemented",
            r"implementation\s+(is\s+)?(absent|missing)",
            r"no\s+\w+\s+(initialization|call|invocation)\s+specified",
            r"logic\s+absent",
            r"(plan|stub|blueprint|design)s?\s+(fails?\s+to\s+specify|omits?|lacks?|is\s+missing)\s+(an?\s+|the\s+)?(aria|accessib\w*|attribute|props?\b|event\s+handlers?|onclick|class\s*name)",
            r"aria[-\s]?\w*\s+(is|are)?\s*(missing|absent|omitted|not\s+specified)",
        ];
        Regex::new(&format!("(?i){}", parts.join("|"))).unwrap()
    })
}

/// JS: `export function voidDesignCategoryErrors(items)` — mutates sustained (`value===true`)
/// items whose statement+why matches DESIGN_OOB_RE: forces value=false, accuracy=0, and prefixes
/// `why`. Returns the count voided. `items` is a JSON array of judged-item objects (each having
/// `value`/`statement`/`why`/`accuracy` keys) — mutated in place, matching the JS array mutation.
pub fn voidDesignCategoryErrors(items: &mut Value) -> i64 {
    let mut voided = 0i64;
    let Some(arr) = items.as_array_mut() else { return 0 };
    let re = design_oob_re();
    for it in arr.iter_mut() {
        if !truthy(it.get("value")) {
            continue; // only sustained claims can burn a design — the rest are inert
        }
        let statement = str_qq(it.get("statement"));
        let why = str_qq(it.get("why"));
        let haystack = format!("{} {}", statement, why);
        if re.is_match(&haystack) {
            if let Some(o) = it.as_object_mut() {
                o.insert("value".into(), json!(false));
                o.insert("accuracy".into(), json!(0));
                let old_why = o.get("why").map(|v| str_qq(Some(v))).unwrap_or_default();
                let new_why = format!("[voided by phase charter: body-level content is TODO by design] {}", old_why).trim().to_string();
                o.insert("why".into(), json!(new_why));
            }
            voided += 1;
        }
    }
    voided
}

const LINK_RULES: &str = "Every claim MUST cite a proof in \"links\": [{kind, ref}], pointing at a real piece of the EVIDENCE below. kind is one of:
  - \"contract\" — ref is an acceptance line NUMBER from the numbered CONTRACT ACCEPTANCE list;
  - \"card\" — ref is a card field name (e.g. \"why\", \"kind\") or an acceptance feature id from the CARD;
  - \"block\" — ref is a design node id from the BLUEPRINT DESIGN (a file block or any nested block);
  - \"screening\" — ref is a key from the PL SCREENING;
  - \"microtask\" — ref is a microtask id from the PLAN;
  - \"snippet\" — ref is a file path (optionally \"path:line\"/\"path:a-b\") from the AUTHORED FILES.
A claim with no real, on-point link is discarded by the court — cite precisely, from the evidence given (you do NOT research; everything you may cite is below).
Example of ONE correctly-linked item (follow this shape exactly — links is a JSON array on the item, never text inside the statement):
{\"item_id\":\"p1\",\"statement\":\"the plan never names the localStorage key\",\"links\":[{\"kind\":\"contract\",\"ref\":\"6\"},{\"kind\":\"block\",\"ref\":\"src/components/ClickMeButton.jsx\"}]}";

const DEBATE_OUT: &str = "{{scale_note}}
Output a JSON object into items_json, EXACTLY {{n_claims}} item(s), no prose, no fences:
{\"items\":[{\"item_id\":\"<short>\",\"statement\":\"<one decidable sentence>\",\"links\":[{\"kind\":\"contract|card|block|screening|microtask|snippet\",\"ref\":\"<locator>\"}]}]}";

const DESIGN_OOB: &str = "⛔ OUT OF BOUNDS for a plan — these are category errors, NOT defects; any such claim is INVALID and discarded: \"no test/runtime/build evidence\", \"not implemented yet\", \"code is absent/missing\", \"unverified at runtime\", \"no proof it executes/passes\". A plan cannot carry runtime evidence — demanding it is never a valid defect.";

fn prosecutor_tmpl() -> String {
    let mut s = String::from("You are the PROSECUTOR. Argue the authored work FAILS to meet the goal. Make exactly {{n_claims}} specific, decidable defect claims — concrete, checkable against the evidence, never vague.\n\nGOAL (the single measurable bar): {{goal}}\n\n");
    s.push_str(LINK_RULES);
    s.push_str("\n\nEVIDENCE:\n{{evidence}}\n\n");
    s.push_str(DEBATE_OUT);
    s
}
fn defender_tmpl() -> String {
    let mut s = String::from("You are the DEFENDER. Argue the authored work SATISFIES the goal. Make exactly {{n_claims}} specific, decidable sufficiency claims — concrete, checkable against the evidence, never vague.\n\nGOAL (the single measurable bar): {{goal}}\n\n");
    s.push_str(LINK_RULES);
    s.push_str("\n\nEVIDENCE:\n{{evidence}}\n\n");
    s.push_str(DEBATE_OUT);
    s
}
fn judge_tmpl() -> String {
    "You are the JUDGE. For each statement below: (1) decide TRUE or FALSE strictly against the GOAL and the cited evidence — not on eloquence; if unsupported or unverifiable, mark it false. (2) GRADE its accuracy: a number 0.0–1.0 for how well the CITED EVIDENCE actually supports the statement — 1.0 = the evidence fully proves it, 0.5 = partial or ambiguous support, 0.0 = the evidence is absent, off-point, or contradicts it. Grade against the evidence, not the claim's confidence.\n\nGOAL (the single measurable bar): {{goal}}\n\nEVIDENCE:\n{{evidence}}\n\nSTATEMENTS:\n{{statements}}\n\nOutput ONLY a fenced json block, one verdict per statement:\n```json\n{\"verdicts\":[{\"item_id\":\"<id>\",\"value\":true,\"accuracy\":0.0,\"why\":\"<≤15 words>\"}]}\n```".to_string()
}
fn prosecutor_design_tmpl() -> String {
    let mut s = String::from("You are the PROSECUTOR reviewing a DESIGN / PLAN — no code exists yet, by definition. Argue the PLAN FAILS to COVER the goal. Make exactly {{n_claims}} specific, decidable defect claims about the PLAN ITSELF — concrete, checkable against the evidence, never vague.\n\n");
    s.push_str(DESIGN_OOB);
    s.push_str("\n\nA VALID design defect is a gap in the PLAN: an acceptance line with no covering block/file/intent; a contradiction or unsound step; a missing file, dependency, or interface the plan needs; an ambiguity that would block implementation.\n\nGOAL (what the plan must COVER): {{goal}}\n\n");
    s.push_str(LINK_RULES);
    s.push_str("\n\nEVIDENCE:\n{{evidence}}\n\n");
    s.push_str(DEBATE_OUT);
    s
}
fn defender_design_tmpl() -> String {
    let mut s = String::from("You are the DEFENDER reviewing a DESIGN / PLAN — no code exists yet. Argue the PLAN COVERS the goal. Make exactly {{n_claims}} specific, decidable sufficiency claims about the PLAN — concrete, checkable against the evidence, never vague: each acceptance line maps to a block/file/intent that addresses it, and the planned steps are sound and complete.\n\nGOAL (what the plan must COVER): {{goal}}\n\n");
    s.push_str(LINK_RULES);
    s.push_str("\n\nEVIDENCE:\n{{evidence}}\n\n");
    s.push_str(DEBATE_OUT);
    s
}
fn judge_design_tmpl() -> String {
    let mut s = String::from("You are the JUDGE reviewing a DESIGN / PLAN — no code exists yet, by definition. For each statement below: (1) decide TRUE or FALSE strictly against the GOAL (what the plan must COVER) and the cited evidence — not on eloquence.\n\n");
    s.push_str(DESIGN_OOB);
    s.push_str("\nSo a defect-style statement complaining about missing runtime/test/build evidence, code \"not implemented\", or anything \"unverifiable at runtime\" is INVALID for a plan — mark it FALSE. Judge ONLY whether the PLAN, as written, COVERS and soundly addresses the goal.\n\n(2) GRADE its accuracy: a number 0.0–1.0 for how well the CITED EVIDENCE actually supports the statement — 1.0 = the evidence fully proves it, 0.5 = partial or ambiguous, 0.0 = absent, off-point, or contradicts it.\n\nGOAL (the single measurable bar): {{goal}}\n\nEVIDENCE:\n{{evidence}}\n\nSTATEMENTS:\n{{statements}}\n\nOutput ONLY a fenced json block, one verdict per statement:\n```json\n{\"verdicts\":[{\"item_id\":\"<id>\",\"value\":true,\"accuracy\":0.0,\"why\":\"<≤15 words>\"}]}\n```");
    s
}

fn ensure_one(db: &Connection, name: &str, template_md: &str, tier: &str, field_name: &str) -> Result<(), String> {
    let existing = crate::ai_prompt_find::promptFind(&json!({ "name": name }), db).ok();
    if let Some(e) = &existing {
        if e.get("template_md").and_then(|v| v.as_str()) == Some(template_md) {
            return Ok(());
        }
    }
    crate::ai_prompt_add::promptAdd(&json!({
        "name": name, "template_md": template_md, "default_tier": tier, "default_temperature": "cold",
        "phase_flow": "single_only", "max_output_tokens": 1500,
        "output_fields": [{ "field_name": field_name, "field_type": "string" }],
    }), db)?;
    Ok(())
}

/// JS: `export function ensureReviewPrompts(db)`.
pub fn ensureReviewPrompts(db: &Connection) -> Result<(), String> {
    ensure_one(db, PROSECUTOR, &prosecutor_tmpl(), "low", "items_json")?;
    ensure_one(db, DEFENDER, &defender_tmpl(), "low", "items_json")?;
    ensure_one(db, JUDGE, &judge_tmpl(), "mid", "verdicts_json")?;
    ensure_one(db, PROSECUTOR_DESIGN, &prosecutor_design_tmpl(), "low", "items_json")?;
    ensure_one(db, DEFENDER_DESIGN, &defender_design_tmpl(), "low", "items_json")?;
    ensure_one(db, JUDGE_DESIGN, &judge_design_tmpl(), "mid", "verdicts_json")?;
    Ok(())
}

// `res?.output_kv?.find((o) => o.field_name === field && o.parent_output_id == null)?.value_text ?? null`
fn pick(res: &Value, field: &str) -> Value {
    let empty = vec![];
    let kv = res.get("output_kv").and_then(|v| v.as_array()).unwrap_or(&empty);
    for o in kv {
        let field_matches = o.get("field_name").and_then(|v| v.as_str()) == Some(field);
        let parent_is_null = o.get("parent_output_id").map_or(true, |v| v.is_null());
        if field_matches && parent_is_null {
            // `?.value_text ?? null` — stop at the FIRST match regardless of its value_text.
            return o.get("value_text").cloned().filter(|v| !v.is_null()).unwrap_or(Value::Null);
        }
    }
    Value::Null
}
fn parse_loose(s: &Value) -> Value {
    if s.is_null() {
        return Value::Null;
    }
    let str_v = str_qq(Some(s));
    static FENCE: OnceLock<Regex> = OnceLock::new();
    let fence = FENCE.get_or_init(|| Regex::new(r"(?is)```(?:json)?\s*([\s\S]*?)```").unwrap());
    static OBJ: OnceLock<Regex> = OnceLock::new();
    let obj_re = OBJ.get_or_init(|| Regex::new(r"(?s)\{[\s\S]*\}").unwrap());
    let fenced = fence.captures(&str_v).and_then(|c| c.get(1)).map(|m| m.as_str().to_string());
    let obj_match = obj_re.find(&str_v).map(|m| m.as_str().to_string());
    for candidate in [fenced, Some(str_v.clone()), obj_match] {
        if let Some(c) = candidate {
            if let Ok(v) = serde_json::from_str::<Value>(c.trim()) {
                return v;
            }
        }
    }
    Value::Null
}
fn safe_arr(s: Option<&str>) -> Value {
    match serde_json::from_str::<Value>(s.unwrap_or("[]")) {
        Ok(v @ Value::Array(_)) => v,
        _ => json!([]),
    }
}

// Force exactly n items so the two sides are equal-length (judgment-score requires it).
fn normalize_items(raw: &Value, side_letter: &str, n: i64) -> Value {
    let empty = vec![];
    let items_src: &Vec<Value> = if let Some(a) = raw.get("items").and_then(|v| v.as_array()) {
        a
    } else if let Some(a) = raw.as_array() {
        a
    } else {
        &empty
    };
    let n = n.max(0) as usize;
    let mut out: Vec<Value> = items_src.iter().take(n).enumerate().map(|(i, it)| {
        let item_id = match it.get("item_id") {
            Some(Value::Null) | None => format!("{}{}", side_letter, i + 1),
            Some(v) => str_qq(Some(v)),
        };
        let statement = match it.get("statement") {
            Some(Value::Null) | None => "(no claim)".to_string(),
            Some(v) => str_qq(Some(v)),
        };
        let links = it.get("links").filter(|v| v.is_array()).cloned().unwrap_or(json!([]));
        json!({ "item_id": item_id, "statement": statement, "links": links })
    }).collect();
    while out.len() < n {
        out.push(json!({ "item_id": format!("{}{}", side_letter, out.len() + 1), "statement": "(no claim)", "links": [] }));
    }
    Value::Array(out)
}

/// JS: `export default async function review(input, context)`. Note: `context` is folded into
/// `db` (required, non-nullable per Rust convention) — `provider_fn` (a JS function reference for
/// provider DI) has no Rust equivalent and is passed as `null` into the constructed aiCall input,
/// matching book-run.rs's convention.
pub fn review(input: &Value, db: &Connection) -> Result<Value, String> {
    let provider_fn = Value::Null;
    let slug_v = input.get("contract_slug").cloned().unwrap_or(Value::Null);
    if !truthy(Some(&slug_v)) {
        return Err("review: contract_slug is required".into());
    }
    let slug = str_qq(Some(&slug_v));
    let scope_kind = match nz(input.get("scope_kind")) { Some(v) => str_qq(Some(v)), None => "microtask".to_string() };
    let scope_ref = match nz(input.get("scope_ref")) { Some(v) => str_qq(Some(v)), None => String::new() };
    let is_design = input.get("scope_class").and_then(|v| v.as_str()) == Some("design");

    let contract = query_one(db, "SELECT slug, card_slug, acceptance_json, files_touched_json FROM contracts WHERE slug = ?", &[json!(slug)])?
        .ok_or_else(|| format!("review: contract '{}' not found", slug))?;

    let acceptance = safe_arr(contract.get("acceptance_json").and_then(|v| v.as_str()));
    let snippets = input.get("snippets").filter(|v| v.is_array()).cloned().unwrap_or(json!([]));
    let scope = json!({
        "kind": scope_kind, "file": scope_ref, "coverage": is_design,
    });
    let derived = crate::swarm_goal_derive::deriveGoal(&acceptance, &scope);
    let derived_goal = derived.get("goal_line").and_then(|v| v.as_str()).filter(|s| !s.is_empty());
    let goal = match nz(input.get("goal_line")).and_then(|v| v.as_str()).filter(|s| !s.is_empty()) {
        Some(g) => g.to_string(),
        None => derived_goal.unwrap_or("(no measurable goal derived)").to_string(),
    };
    let measurable = !goal.is_empty() && goal != "(no measurable goal derived)";

    let gather_input = json!({
        "contract_slug": slug, "scope_kind": scope_kind, "scope_ref": scope_ref, "snippets": snippets,
    });
    let gathered_res = crate::swarm_court_evidence_gather::courtEvidenceGather(&gather_input, db)?;
    let corpus = gathered_res.get("corpus").cloned().unwrap_or(json!({}));
    let gathered = str_qq(gathered_res.get("evidence_md"));

    let charter = phase_charter(is_design);
    let prior_defects = nz(input.get("prior_defects_md")).map(|v| str_qq(Some(v))).unwrap_or_default();
    let evidence = [charter.to_string(), gathered, prior_defects]
        .into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join("\n\n");

    // FEAT-COURT-SCALES-WITH-SIZE + SIZE HONESTY: keys on REALITY (files actually touched), not
    // the upstream card's (possibly inflated) size label.
    let mut card_size: Option<String> = None;
    if let Some(card_slug) = nz(contract.get("card_slug")).and_then(|v| v.as_str()) {
        if let Ok(Some(row)) = query_one(db, "SELECT body_json FROM cards WHERE slug = ?", &[json!(card_slug)]) {
            let body = safe_json(row.get("body_json").and_then(|v| v.as_str()), json!({}));
            let size = body.get("size").map(|v| str_qq(Some(v)).to_uppercase()).filter(|s| !s.is_empty());
            card_size = size;
        }
    }
    let files_touched: Option<i64> = match serde_json::from_str::<Value>(contract.get("files_touched_json").and_then(|v| v.as_str()).unwrap_or("null")) {
        Ok(Value::Array(a)) => Some(a.len() as i64),
        _ => None,
    };
    let small_card = card_size.as_deref() == Some("XS") || card_size.as_deref() == Some("S")
        || matches!(files_touched, Some(n) if n > 0 && n <= 2);
    let n_claims = if small_card { 1 } else { N_ITEMS };
    let scale_note = if small_card {
        "This is a SMALL change. If you find no REAL, decidable claim, output {\"items\":[]} — do NOT manufacture one.".to_string()
    } else {
        String::new()
    };

    ensureReviewPrompts(db)?;
    // EPIC-SUBAGENT: mounts each panel leaf's composed persona as system_md. Defensive: a
    // resolvePersona failure (missing leaf) degrades to no system override.
    let persona = |name: &str| -> Value {
        crate::entities_agent_persona_resolve::resolvePersona(db, name).get("system_md").cloned().unwrap_or(Value::Null)
    };
    let prosecutor_persona = persona("qa.prosecutor");
    let defender_persona = persona("qa.lawyer");
    let judge_persona = persona("qa.judge");

    let gather_summary = format!(
        "gathered evidence for {}:{} — card {}, blueprint {}, screening {}; {} blocks · {} microtasks · {} files",
        scope_kind, scope_ref,
        if truthy(corpus.get("card")) { "✓" } else { "✗" },
        if truthy(corpus.get("blueprint")) { "✓" } else { "✗" },
        if truthy(corpus.get("screening")) { "✓" } else { "✗" },
        corpus.get("blocks").and_then(|v| v.as_array()).map(|a| a.len()).unwrap_or(0),
        corpus.get("microtasks").and_then(|v| v.as_array()).map(|a| a.len()).unwrap_or(0),
        corpus.get("snippets").and_then(|v| v.as_array()).map(|a| a.len()).unwrap_or(0),
    );
    let mut calls: Vec<Value> = vec![json!({
        "role": "evidence.gather", "tier": "deterministic", "prompt_md": gather_summary, "response_md": evidence,
    })];

    let pros_items = debate_call(
        db, if is_design { PROSECUTOR_DESIGN } else { PROSECUTOR }, "low", "p",
        &goal, &evidence, n_claims, &scale_note,
        &prosecutor_persona, "qa.prosecutor",
        &slug, &provider_fn, &mut calls,
    )?;
    let def_items = debate_call(
        db, if is_design { DEFENDER_DESIGN } else { DEFENDER }, "low", "d",
        &goal, &evidence, n_claims, &scale_note,
        &defender_persona, "qa.lawyer",
        &slug, &provider_fn, &mut calls,
    )?;

    // Grounding floor: mark each item grounded iff >=1 of its links resolves. Unreachable given
    // the aiCall Err above — ported byte-for-byte for fidelity (mirrors book-run.rs's convention).
    let pros_grounded = ground_items(&pros_items, "prosecution", &corpus);
    let def_grounded = ground_items(&def_items, "defense", &corpus);

    let judge_def_raw = judge_call(db, is_design, "d", &goal, &evidence, &def_grounded, &judge_persona, &slug, &provider_fn, &mut calls)?;
    let judge_pros_raw = judge_call(db, is_design, "p", &goal, &evidence, &pros_grounded, &judge_persona, &slug, &provider_fn, &mut calls)?;

    let score_input = json!({
        "defender_list": def_grounded, "prosecutor_list": pros_grounded,
        "judge_defender": judge_def_raw, "judge_prosecutor": judge_pros_raw,
        "thresholds": input.get("thresholds").cloned().unwrap_or(Value::Null),
    });
    let mut result = crate::swarm_judgment_score::scoreJudgment(&score_input);

    // FEAT-COURT-PHASE-CHARTER (deterministic half): void sustained prosecutor claims whose
    // evidence is the TODO-ness of stub bodies. Recount after voiding.
    if is_design {
        let mut pros_items_val = result.get("prosecutor").and_then(|p| p.get("items")).cloned().unwrap_or(json!([]));
        let voided = voidDesignCategoryErrors(&mut pros_items_val);
        if voided > 0 {
            let items_arr = pros_items_val.as_array().cloned().unwrap_or_default();
            let trues = items_arr.iter().filter(|i| truthy(i.get("value"))).count() as i64;
            let total = items_arr.len() as i64;
            let rate = if total > 0 { trues as f64 / total as f64 } else { 0.0 };
            let thresholds = result.get("thresholds").cloned().unwrap_or(json!({}));
            let pros_max = thresholds.get("pros_max").and_then(|v| v.as_f64()).unwrap_or(0.2);
            let def_min = thresholds.get("def_min").and_then(|v| v.as_f64()).unwrap_or(0.8);
            let def_rate = result.get("defender").and_then(|d| d.get("rate")).and_then(|v| v.as_f64()).unwrap_or(0.0);
            let prosecutor_ok = rate <= pros_max;
            // mirror judgment-score's sufficiency-by-majority: zero grounded defects + a strict
            // defender majority passes; a landed defect keeps the strict bar.
            let defender_ok = def_rate >= def_min || (trues == 0 && def_rate > 0.5);
            let verdict = if prosecutor_ok && defender_ok { "pass" } else { "fail" };
            if let Some(o) = result.as_object_mut() {
                if let Some(p) = o.get_mut("prosecutor").and_then(|v| v.as_object_mut()) {
                    p.insert("items".into(), pros_items_val);
                    p.insert("trues".into(), json!(trues));
                    p.insert("rate".into(), json!(rate));
                }
                o.insert("verdict".into(), json!(verdict));
                if let Some(reasons) = o.get_mut("reasons").and_then(|v| v.as_array_mut()) {
                    reasons.push(json!("phase-charter void: sustained stub-body claims removed (design bodies are TODO by contract)"));
                }
            }
        }
    }

    let judgment_id = crate::swarm_judgment_id::makeJudgmentId(
        &nz(contract.get("card_slug")).cloned().unwrap_or(json!(slug)), &json!(scope_kind), &json!(scope_ref),
    )?;
    persist_judgment(db, &judgment_id, &contract, &scope_kind, &scope_ref, &goal, &result, &calls)?;

    let sc_input = json!({ "scope": judgment_id, "prosecution": pros_items, "defense": def_items });
    let sc = crate::swarm_session_case::renderSessionCase(&sc_input, &corpus);

    Ok(json!({
        "judgment_id": judgment_id,
        "verdict": result.get("verdict"),
        "n": result.get("n"),
        "defender": result.get("defender"),
        "prosecutor": result.get("prosecutor"),
        "reasons": result.get("reasons"),
        "document_md": sc.get("document_md"),
        "measurable": measurable,
    }))
}

// Plain (non-capturing) fns rather than closures — `review` calls each of these twice with
// shared borrowed state (goal/evidence/slug/provider_fn), and a closure that inserted those
// captures by value into `json!` would only implement FnOnce (callable once); explicit
// parameters sidestep that entirely, matching book-run.rs's / author-blocks.rs's fac_call style.
fn debate_call(
    db: &Connection, name: &str, tier: &str, side_letter: &str,
    goal: &str, evidence: &str, n_claims: i64, scale_note: &str,
    system_md: &Value, subagent: &str, slug: &str, provider_fn: &Value, calls: &mut Vec<Value>,
) -> Result<Value, String> {
    let ai_input = json!({
        "prompt_name": name,
        "input_kv": [
            { "key": "goal", "value": goal },
            { "key": "evidence", "value": evidence },
            { "key": "n_claims", "value": n_claims.to_string() },
            { "key": "scale_note", "value": scale_note },
        ],
        "system_md": system_md,
        "subagent": subagent,
        "contract_slug": slug,
        "tier": tier, "temperature": "cold", "phase_flow": "single_only", "provider_fn": provider_fn,
    });
    // PARITY-DEFERRED: dispatches through crate::ai_ai_call::aiCall, which always returns Err
    // (see that module's doc). The prosecutor debate is the FIRST such call — its Err propagates
    // straight out of `review`, exactly mirroring the JS's unguarded `await`.
    let res = crate::ai_ai_call::aiCall(&ai_input, db)?;
    let raw = pick(&res, "items_json");
    calls.push(json!({
        "role": if side_letter == "p" { "prosecutor" } else { "defender" }, "tier": tier,
        "prompt_md": goal, "response_md": str_qq(Some(&raw)),
    }));
    Ok(normalize_items(&parse_loose(&raw), side_letter, n_claims))
}

// Grounding floor: mark each item grounded iff >=1 of its links resolves.
fn ground_items(items: &Value, side: &str, corpus: &Value) -> Value {
    let empty = vec![];
    let arr = items.as_array().unwrap_or(&empty);
    Value::Array(arr.iter().map(|it| {
        let arg = json!({ "id": it.get("item_id"), "side": side, "links": it.get("links") });
        let validated = crate::swarm_argument_links::validateArgument(&arg, corpus);
        let mut o = it.as_object().cloned().unwrap_or_default();
        o.insert("grounded".into(), validated.get("grounded").cloned().unwrap_or(json!(false)));
        Value::Object(o)
    }).collect())
}

#[allow(clippy::too_many_arguments)]
fn judge_call(
    db: &Connection, is_design: bool, side_letter: &str, goal: &str, evidence: &str, items: &Value,
    judge_persona: &Value, slug: &str, provider_fn: &Value, calls: &mut Vec<Value>,
) -> Result<Value, String> {
    let empty = vec![];
    let statements = items.as_array().unwrap_or(&empty).iter()
        .map(|it| format!("{}: {}", tmpl(it.get("item_id")), tmpl(it.get("statement"))))
        .collect::<Vec<_>>().join("\n");
    let ai_input = json!({
        "prompt_name": if is_design { JUDGE_DESIGN } else { JUDGE },
        "input_kv": [
            { "key": "goal", "value": goal },
            { "key": "evidence", "value": evidence },
            { "key": "statements", "value": statements },
        ],
        "system_md": judge_persona,
        "subagent": "qa.judge",
        "contract_slug": slug,
        "tier": "mid", "temperature": "cold", "phase_flow": "single_only", "provider_fn": provider_fn,
    });
    let res = crate::ai_ai_call::aiCall(&ai_input, db)?;
    let raw = pick(&res, "verdicts_json");
    calls.push(json!({
        "role": if side_letter == "p" { "judge.prosecutor" } else { "judge.defender" }, "tier": "mid",
        "prompt_md": statements, "response_md": str_qq(Some(&raw)),
    }));
    Ok(raw)
}

// Split out of persistJudgment's inline try/catch: archives the prior round (if any) under
// '<judgment_id>::r<N>' before the upsert below overwrites it with the new ruling.
fn archive_prior_round(db: &Connection, judgment_id: &str) -> Result<(), String> {
    let prior = query_one(db, "SELECT * FROM judgments.judgment_cases WHERE judgment_id = ?", &[json!(judgment_id)])?;
    let Some(prior) = prior else { return Ok(()) };
    let n = query_one(db, "SELECT COUNT(*) AS n FROM judgments.judgment_cases WHERE judgment_id LIKE ? || '::r%'", &[json!(judgment_id)])?
        .and_then(|r| r.get("n").and_then(|v| v.as_i64())).unwrap_or(0) + 1;
    let archive_id = format!("{}::r{}", judgment_id, n);
    crate::db_json::run_write(db, "
        INSERT OR IGNORE INTO judgments.judgment_cases (judgment_id, card_slug, scope_kind, scope_ref, goal_line, n_items, defender_trues, prosecutor_trues, defender_rate, prosecutor_rate, defender_accuracy, prosecutor_accuracy, verdict, created_at, updated_at, snippets_hash)
        VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)
      ", &[
        json!(archive_id), prior.get("card_slug").cloned().unwrap_or(Value::Null),
        prior.get("scope_kind").cloned().unwrap_or(Value::Null), json!(format!("{}::r{}", str_qq(prior.get("scope_ref")), n)),
        prior.get("goal_line").cloned().unwrap_or(Value::Null), prior.get("n_items").cloned().unwrap_or(Value::Null),
        prior.get("defender_trues").cloned().unwrap_or(Value::Null), prior.get("prosecutor_trues").cloned().unwrap_or(Value::Null),
        prior.get("defender_rate").cloned().unwrap_or(Value::Null), prior.get("prosecutor_rate").cloned().unwrap_or(Value::Null),
        prior.get("defender_accuracy").cloned().unwrap_or(Value::Null), prior.get("prosecutor_accuracy").cloned().unwrap_or(Value::Null),
        prior.get("verdict").cloned().unwrap_or(Value::Null), prior.get("created_at").cloned().unwrap_or(Value::Null),
        prior.get("updated_at").cloned().unwrap_or(Value::Null), prior.get("snippets_hash").cloned().unwrap_or(Value::Null),
    ])?;
    let copy_items = crate::db_json::query_rows(db, "SELECT * FROM judgments.judgment_items WHERE judgment_id = ?", &[json!(judgment_id)])?;
    for it in copy_items.as_array().cloned().unwrap_or_default() {
        crate::db_json::run_write(db, "INSERT OR IGNORE INTO judgments.judgment_items (judgment_id, side, item_id, statement, links_json, grounded, judge_value, judge_accuracy, judge_why) VALUES (?,?,?,?,?,?,?,?,?)", &[
            json!(archive_id.clone()), it.get("side").cloned().unwrap_or(Value::Null), it.get("item_id").cloned().unwrap_or(Value::Null),
            it.get("statement").cloned().unwrap_or(Value::Null), it.get("links_json").cloned().unwrap_or(Value::Null),
            it.get("grounded").cloned().unwrap_or(Value::Null), it.get("judge_value").cloned().unwrap_or(Value::Null),
            it.get("judge_accuracy").cloned().unwrap_or(Value::Null), it.get("judge_why").cloned().unwrap_or(Value::Null),
        ])?;
    }
    Ok(())
}

// Ported byte-for-byte from the JS `persistJudgment` — unreachable by this file's own fixture
// (only ever called after successful aiCall calls, which the deferred stub never produces).
fn persist_judgment(
    db: &Connection, judgment_id: &str, contract: &Value, scope_kind: &str, scope_ref: &str,
    goal: &str, result: &Value, calls: &[Value],
) -> Result<(), String> {
    // ROUND ARCHIVE: the upsert keeps judgment_id = the LATEST ruling, but a re-judged case
    // archives its predecessor first: '<id>::r<N>' (items included) — history is evidence.
    // archive is additive — never block the ruling (JS: try/catch swallow).
    let _ = archive_prior_round(db, judgment_id);

    let card_slug = nz(contract.get("card_slug")).cloned().unwrap_or_else(|| contract.get("slug").cloned().unwrap_or(Value::Null));
    let defender = result.get("defender").cloned().unwrap_or(json!({}));
    let prosecutor = result.get("prosecutor").cloned().unwrap_or(json!({}));
    crate::db_json::run_write(db, "
    INSERT INTO judgments.judgment_cases (judgment_id, card_slug, scope_kind, scope_ref, goal_line, n_items, defender_trues, prosecutor_trues, defender_rate, prosecutor_rate, defender_accuracy, prosecutor_accuracy, verdict)
    VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?)
    ON CONFLICT(judgment_id) DO UPDATE SET goal_line=excluded.goal_line, n_items=excluded.n_items,
      defender_trues=excluded.defender_trues, prosecutor_trues=excluded.prosecutor_trues,
      defender_rate=excluded.defender_rate, prosecutor_rate=excluded.prosecutor_rate,
      defender_accuracy=excluded.defender_accuracy, prosecutor_accuracy=excluded.prosecutor_accuracy,
      verdict=excluded.verdict, updated_at=datetime('now')
  ", &[
        json!(judgment_id), card_slug, json!(scope_kind), json!(scope_ref), json!(goal),
        result.get("n").cloned().unwrap_or(Value::Null),
        defender.get("trues").cloned().unwrap_or(Value::Null), prosecutor.get("trues").cloned().unwrap_or(Value::Null),
        defender.get("rate").cloned().unwrap_or(Value::Null), prosecutor.get("rate").cloned().unwrap_or(Value::Null),
        defender.get("accuracy").cloned().unwrap_or(Value::Null), prosecutor.get("accuracy").cloned().unwrap_or(Value::Null),
        result.get("verdict").cloned().unwrap_or(Value::Null),
    ])?;

    let write_side = |side: &str, items: &Value| -> Result<(), String> {
        for it in items.as_array().cloned().unwrap_or_default() {
            crate::db_json::run_write(db, "
    INSERT INTO judgments.judgment_items (judgment_id, side, item_id, statement, links_json, grounded, judge_value, judge_accuracy, judge_why)
    VALUES (?,?,?,?,?,?,?,?,?)
    ON CONFLICT(judgment_id, side, item_id) DO UPDATE SET statement=excluded.statement,
      links_json=excluded.links_json, grounded=excluded.grounded, judge_value=excluded.judge_value,
      judge_accuracy=excluded.judge_accuracy, judge_why=excluded.judge_why
  ", &[
                json!(judgment_id), json!(side), it.get("item_id").cloned().unwrap_or(Value::Null),
                it.get("statement").cloned().unwrap_or(Value::Null),
                json!(serde_json::to_string(&it.get("links").cloned().unwrap_or(json!([]))).unwrap_or_default()),
                json!(truthy(it.get("grounded"))), json!(truthy(it.get("value"))),
                it.get("accuracy").cloned().unwrap_or(Value::Null), it.get("why").cloned().unwrap_or(json!("")),
            ])?;
        }
        Ok(())
    };
    write_side("defender", defender.get("items").unwrap_or(&Value::Null))?;
    write_side("prosecutor", prosecutor.get("items").unwrap_or(&Value::Null))?;

    for c in calls {
        crate::db_json::run_write(db, "INSERT INTO judgments.judgment_calls (judgment_id, role, tier, prompt_md, response_md) VALUES (?,?,?,?,?)", &[
            json!(judgment_id), c.get("role").cloned().unwrap_or(Value::Null), c.get("tier").cloned().unwrap_or(Value::Null),
            c.get("prompt_md").cloned().unwrap_or(Value::Null), c.get("response_md").cloned().unwrap_or(Value::Null),
        ])?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conn() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("
            CREATE TABLE contracts (id INTEGER PRIMARY KEY, slug TEXT, card_slug TEXT, acceptance_json TEXT, microtasks_json TEXT, files_touched_json TEXT);
            CREATE TABLE cards (id INTEGER PRIMARY KEY, slug TEXT, title TEXT, body_json TEXT, acceptance_json TEXT);
            CREATE TABLE agents (id INTEGER PRIMARY KEY, name TEXT, persona_md TEXT, system_md TEXT);
            CREATE TABLE blueprints (id INTEGER PRIMARY KEY, contract_slug TEXT, version INTEGER, lint_passed INTEGER, blueprint_json TEXT, superseded_by INTEGER);
            CREATE TABLE pl_screenings (id INTEGER PRIMARY KEY, card_slug TEXT, screening_json TEXT);
        ").unwrap();
        c.execute_batch("ATTACH ':memory:' AS brain; ATTACH ':memory:' AS judgments; ATTACH ':memory:' AS knowledge;").unwrap();
        c.execute_batch("
            CREATE TABLE brain.prompt_templates ( id INTEGER PRIMARY KEY, name TEXT NOT NULL, version INTEGER NOT NULL DEFAULT 1, template_md TEXT NOT NULL, default_tier TEXT NOT NULL, default_temperature TEXT NOT NULL, max_output_tokens INTEGER NOT NULL DEFAULT 1024, superseded_by INTEGER, created_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')), cross_field_rules_json TEXT, judging_criteria_json TEXT, system_md TEXT, phase_flow TEXT NOT NULL DEFAULT 'single', UNIQUE (name, version) );
            CREATE TABLE brain.prompt_output_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL DEFAULT 0, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, grounded_refs_json TEXT, UNIQUE (prompt_template_id, field_name) );
            CREATE TABLE brain.prompt_input_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL DEFAULT 0, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, UNIQUE (prompt_template_id, field_name) );
        ").unwrap();
        c.execute_batch("
            CREATE TABLE judgments.judgment_cases (judgment_id TEXT PRIMARY KEY, card_slug TEXT NOT NULL, scope_kind TEXT NOT NULL, scope_ref TEXT NOT NULL DEFAULT '', goal_line TEXT, n_items INTEGER, defender_trues INTEGER, prosecutor_trues INTEGER, defender_rate REAL, prosecutor_rate REAL, defender_accuracy REAL, prosecutor_accuracy REAL, verdict TEXT, created_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')), updated_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')), snippets_hash TEXT);
            CREATE TABLE judgments.judgment_items (id INTEGER PRIMARY KEY AUTOINCREMENT, judgment_id TEXT NOT NULL, side TEXT NOT NULL, item_id TEXT NOT NULL, statement TEXT NOT NULL DEFAULT '', links_json TEXT NOT NULL DEFAULT '[]', grounded INTEGER NOT NULL DEFAULT 1, judge_value INTEGER, judge_accuracy REAL, judge_why TEXT, created_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')), UNIQUE (judgment_id, side, item_id));
            CREATE TABLE judgments.judgment_calls (id INTEGER PRIMARY KEY AUTOINCREMENT, judgment_id TEXT NOT NULL, role TEXT NOT NULL, tier TEXT, prompt_md TEXT, response_md TEXT, ai_call_id INTEGER, created_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')));
            CREATE TABLE judgments.court_evidence (judgment_id TEXT PRIMARY KEY, contract_slug TEXT, scope_kind TEXT, scope_ref TEXT, corpus_json TEXT, evidence_md TEXT, n_blocks INTEGER, n_acceptance INTEGER, has_card INTEGER, has_blueprint INTEGER, has_screening INTEGER, created_at TEXT DEFAULT '2026-01-01 00:00:00');
        ").unwrap();
        c
    }

    #[test]
    fn missing_contract_slug_is_required() {
        let c = conn();
        let err = review(&json!({}), &c).unwrap_err();
        assert!(err.contains("contract_slug is required"));
    }

    #[test]
    fn contract_not_found() {
        let c = conn();
        let err = review(&json!({ "contract_slug": "nope" }), &c).unwrap_err();
        assert!(err.contains("contract 'nope' not found"));
    }

    #[test]
    fn deterministic_surface_propagates_deferred_ai_call_err() {
        let c = conn();
        c.execute("INSERT INTO contracts (slug, card_slug, acceptance_json, files_touched_json) VALUES ('cs1','card1','[{\"kind\":\"file_exists\",\"path\":\"src/a.rs\"}]','[\"src/a.rs\"]')", []).unwrap();
        c.execute("INSERT INTO cards (slug, body_json) VALUES ('card1', '{\"kind\":\"feature\",\"size\":\"S\",\"why\":\"x\"}')", []).unwrap();
        let err = review(&json!({ "contract_slug": "cs1", "scope_kind": "microtask", "scope_ref": "src/a.rs" }), &c).unwrap_err();
        assert!(err.contains("ai-call"), "unexpected error: {err}");
        // ensureReviewPrompts DID run before the deferred call (real deterministic surface) —
        // the six prompts were written even though the call itself failed.
        let n = crate::db_json::query_rows(&c, "SELECT count(*) n FROM brain.prompt_templates", &[]).unwrap();
        assert_eq!(n[0]["n"], json!(6));
    }

    #[test]
    fn void_design_category_errors_voids_sustained_stub_body_claims() {
        let mut items = json!([
            { "item_id": "p1", "value": true, "statement": "the login body is not yet implemented", "why": "stub body" },
            { "item_id": "p2", "value": true, "statement": "the plan never names the localStorage key", "why": "" },
            { "item_id": "p3", "value": false, "statement": "not yet implemented", "why": "" },
        ]);
        let voided = voidDesignCategoryErrors(&mut items);
        assert_eq!(voided, 1);
        let arr = items.as_array().unwrap();
        assert_eq!(arr[0]["value"], json!(false));
        assert_eq!(arr[0]["accuracy"], json!(0));
        assert!(arr[0]["why"].as_str().unwrap().starts_with("[voided by phase charter"));
        // real defect untouched
        assert_eq!(arr[1]["value"], json!(true));
        // inert (not sustained) claim untouched even though it matches the pattern
        assert_eq!(arr[2]["value"], json!(false));
        assert_eq!(arr[2]["why"], json!(""));
    }

    #[test]
    fn void_design_category_errors_no_match_returns_zero() {
        let mut items = json!([
            { "item_id": "p1", "value": true, "statement": "acceptance line 6 maps to no block", "why": "" },
        ]);
        assert_eq!(voidDesignCategoryErrors(&mut items), 0);
        assert_eq!(items[0]["value"], json!(true));
    }

    #[test]
    fn ensure_review_prompts_is_idempotent() {
        let c = conn();
        ensureReviewPrompts(&c).unwrap();
        ensureReviewPrompts(&c).unwrap();
        let n = crate::db_json::query_rows(&c, "SELECT count(*) n FROM brain.prompt_templates", &[]).unwrap();
        assert_eq!(n[0]["n"], json!(6));
    }
}
