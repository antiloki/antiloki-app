//! tl-contract-redraft.rs — twin of skills/agents/tl/tl-contract-redraft.mjs (db, LLM-partial).
//! NAMES ARE CONTRACT: `tlContractRedraft` (default), `buildPriorFailuresMd` (named export).
//!
//! AI-driven re-draft of a REJECTED/SUPERSEDED contract: reads the old contract + its
//! rejection_reason_md, asks TL (mid/cold) to produce a corrected contract, then persists v(n+1)
//! via `contract-supersede`. PARITY-DEFERRED: the `aiCall` leg — see `ai-call.rs`'s module doc.
//! Everything else is ported faithfully and IS the deterministic surface these tests exercise:
//! all guards (old-contract lookup, status/superseded_by/rejection_reason_md checks, card lookup),
//! `cardBodyMd` parse-or-fallback, `ensurePrompt`'s re-register-on-drift logic (reusing the
//! already-converted `ai_prompt_find`/`ai_prompt_add` twins), `readHeadSha` (real `git rev-parse
//! HEAD` subprocess — same bespoke-native convention as `man-audit-contract-phase1.rs`'s
//! `read_head_sha`, same 4-level-up REPO_ROOT target from `skills/agents/tl/`), the candidate-paths
//! grounding call (reuses the already-converted `ai_ai_prompt_path_grounding` twin — a REAL
//! effect, not deferred), `buildPriorFailuresMd`'s db read + formatting, the aiCall input
//! assembly, and — past the deferred boundary — the response-parsing helpers (`get_scalar`,
//! `get_string_arr`, `get_object_arr`, `unwrap_possible_json_value`) ported byte-for-byte from the
//! JS and directly unit-tested against canned `ai_call_outputs`-shaped rows (never reachable via
//! this file's own fixture, since `aiCall`'s Err short-circuits before any real output_kv exists —
//! same convention as `book-run.rs`'s `parse_arr`/`parse_obj`), plus the post-parse validation
//! (empty files_touched / missing hard laws / empty out_of_scope) and new-contract shaping, all
//! ported by inspection for the same reason.
use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};

// ── standard helpers (playbook) ─────────────────────────────────────────────────────────────
fn truthy(v: Option<&Value>) -> bool { match v { None|Some(Value::Null)=>false, Some(Value::Bool(b))=>*b, Some(Value::Number(n))=>n.as_f64().map_or(false,|f|f!=0.0&&!f.is_nan()), Some(Value::String(s))=>!s.is_empty(), Some(_)=>true } }
fn str_qq(v: Option<&Value>) -> String { match v { None|Some(Value::Null)=>String::new(), Some(Value::String(s))=>s.clone(), Some(Value::Bool(b))=>b.to_string(), Some(n@Value::Number(_))=>n.to_string(), Some(o)=>o.to_string() } } // = JS String(v ?? '')
fn js_string(v:&Value)->String{ match v{Value::Null=>"null".into(),Value::String(s)=>s.clone(),Value::Number(n)=>n.to_string(),Value::Bool(b)=>b.to_string(),Value::Array(a)=>a.iter().map(js_string).collect::<Vec<_>>().join(","),Value::Object(_)=>"[object Object]".into()} } // = JS String(v)
fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) }

// JS: `const HERE = dirname(fileURLToPath(import.meta.url)); const REPO_ROOT = resolve(HERE,
// '..','..','..','..');` from skills/agents/tl/ — same 4-level-up target as
// man-audit-contract-phase1.rs's repo_root() (copied verbatim, same precedent).
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("antiloki-core-rust has a parent dir")
        .to_path_buf()
}

/// `function readHeadSha() { try { return execSync('git rev-parse HEAD', {...}).trim(); } catch
/// { return null; } }` — real subprocess call, mirrors man-audit-contract-phase1.rs's read_head_sha.
fn read_head_sha() -> Value {
    let out = crate::proc::cmd("git").args(["rev-parse", "HEAD"]).current_dir(repo_root()).output();
    match out {
        Ok(o) if o.status.success() => {
            let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
            if s.is_empty() { Value::Null } else { json!(s) }
        }
        _ => Value::Null,
    }
}

const PROMPT_NAME: &str = "tl.contract.redraft.v1";

const PROMPT_TEMPLATE_MD: &str = r#"You are TL (Tech Lead). A previous contract on this card was REJECTED. Read the rejection reason carefully, then produce a CORRECTED contract draft that addresses the failure.

═══ CARD BODY (source of truth) ═══
{{card_body_md}}

{{candidate_paths_md}}

{{prior_failures_md}}

═══ PREVIOUS CONTRACT (verbatim — for context, NOT to be copied wholesale) ═══
{{old_contract_json}}

═══ FAILURE_KIND ═══
{{failure_kind}}

═══ REJECTION REASON ═══
{{rejection_reason_md}}

Rules:
  - Address the rejection. If files_touched was empty → list concrete files. If acceptance was thin → flesh it out. If out_of_scope was missing → add safety rails.
  - PRESERVE what worked. Don't gratuitously change fields the rejection didn't flag.
  - PATHS: prefer paths from the CANDIDATE PATHS section above. Only invent a NEW path if creating a new file the substrate does not yet have (e.g., a new test file). Never invent a path that conflicts with an existing-different path.
  - Keep tier/risk realistic: high-risk for migrations/schema/engine files; standard otherwise.
  - must_obey MUST include the two core hard laws: NEVER_BYPASS_WITH_AUDIT, HISTORY_IS_SACRED_NO_REVERT.
  - out_of_scope MUST be non-empty — name the dangerous neighboring things DEV should NOT touch.
  - STRUCTURAL FIELDS: if the CARD BODY or the rejection asks for them, you MUST populate must_obey, naming, and template_kind with real content (not empty {} or []). A PL pushback frequently fires because naming/must_obey/template_kind are empty while goal_md requires them — fill them in.
    - naming: a list of {"name","meaning"} pairs documenting each new named symbol/identifier and its convention (e.g. [{"name":"handlerFn","meaning":"the exported default"}]). [] only if the work introduces no new named symbols.
    - template_kind: a short string classifying the change shape (e.g. "skill", "migration", "test", "frontend-component", "config"). Required when goal_md references it.

Output JSON of this exact shape:
{
  "reasoning_md":    "<≤ 200 word explanation of what changed and why>",
  "files_touched":   ["<path1>", "<path2>"],
  "acceptance":      [{"kind":"sql|test|file-exists|diff_in_paths", "...":"..."}],
  "must_obey":       ["NEVER_BYPASS_WITH_AUDIT", "HISTORY_IS_SACRED_NO_REVERT", "..."],
  "out_of_scope":    ["<rule1>", "<rule2>"],
  "naming":          [{"name":"<symbol>", "meaning":"<convention>"}],
  "template_kind":   "<skill|migration|test|frontend-component|config|...>",
  "tier":            "trivial|standard|high-risk",
  "risk":            <1..5>,
  "complexity":      <1..5>,
  "size":            "XS|S|M|L"
}"#;

fn prompt_output_fields() -> Value {
    json!([
        { "field_name": "reasoning_md",  "field_type": "string" },
        { "field_name": "files_touched", "field_type": "array_of_string" },
        { "field_name": "acceptance",    "field_type": "array_of_object" },
        { "field_name": "must_obey",     "field_type": "array_of_string" },
        { "field_name": "out_of_scope",  "field_type": "array_of_string" },
        { "field_name": "naming",        "field_type": "array_of_object", "required": false },
        { "field_name": "template_kind", "field_type": "string", "required": false },
        { "field_name": "tier",          "field_type": "string" },
        { "field_name": "risk",          "field_type": "number" },
        { "field_name": "complexity",    "field_type": "number" },
        { "field_name": "size",          "field_type": "string" },
    ])
}

/// `function ensurePrompt(db)` — re-register on template drift: promptAdd auto-bumps version +
/// supersedes the prior row when the stored template_md differs from PROMPT_TEMPLATE_MD.
fn ensure_prompt(db: &Connection) -> Result<(), String> {
    let existing = crate::ai_prompt_find::promptFind(&json!({ "name": PROMPT_NAME }), db).ok();
    if let Some(e) = &existing {
        if e.get("template_md").and_then(Value::as_str) == Some(PROMPT_TEMPLATE_MD) {
            return Ok(());
        }
    }
    crate::ai_prompt_add::promptAdd(
        &json!({
            "name": PROMPT_NAME,
            "template_md": PROMPT_TEMPLATE_MD,
            "default_tier": "mid",
            "default_temperature": "cold",
            "max_output_tokens": 2000,
            "output_fields": prompt_output_fields(),
        }),
        db,
    )?;
    Ok(())
}

/// `export function buildPriorFailuresMd(db, card_slug, current_old_id)` — every PRIOR failed
/// contract on this card (id < current `old`'s id, same card_slug, failure_kind IS NOT NULL).
pub fn buildPriorFailuresMd(db: &Connection, card_slug: &Value, current_old_id: &Value) -> Result<Value, String> {
    const EMPTY: &str = "═══ PRIOR FAILED ATTEMPTS ON THIS CARD — DO NOT REPEAT ═══\n(no prior failed attempts on this card)\n";
    if !truthy(Some(card_slug)) { return Ok(json!(EMPTY)); }
    let rows = crate::db_json::query_rows(
        db,
        "SELECT slug, current_attempt, failure_kind, rejection_reason_md, status \
         FROM contracts WHERE card_slug = ? AND id < ? AND failure_kind IS NOT NULL \
         ORDER BY current_attempt ASC, id ASC",
        &[card_slug.clone(), current_old_id.clone()],
    )?;
    let priors = rows.as_array().cloned().unwrap_or_default();
    if priors.is_empty() { return Ok(json!(EMPTY)); }

    let mut lines: Vec<String> = vec!["═══ PRIOR FAILED ATTEMPTS ON THIS CARD — DO NOT REPEAT ═══".to_string()];
    for p in &priors {
        let full_reason = nz(p.get("rejection_reason_md")).map(js_string).unwrap_or_else(|| "(no reason recorded)".to_string());
        // JS: `(p.rejection_reason_md ?? '(no reason recorded)').slice(0, 250).replace(/\n+/g, ' ')`
        // — slice BEFORE the newline collapse. Mirror the ORDER exactly.
        let sliced: String = full_reason.chars().take(250).collect();
        let summary = collapse_newlines(&sliced);
        let attempt = js_string(nz(p.get("current_attempt")).unwrap_or(&json!("?")));
        let slug = js_string(p.get("slug").unwrap_or(&Value::Null));
        let failure_kind = nz(p.get("failure_kind")).map(js_string).unwrap_or_else(|| "(unknown)".to_string());
        let ellipsis = if full_reason.chars().count() > 250 { "…" } else { "" };
        lines.push(format!("attempt #{attempt} (slug: {slug})"));
        lines.push(format!("  failure_kind: {failure_kind}"));
        lines.push(format!("  rejection:    {summary}{ellipsis}"));
        lines.push(String::new());
    }
    Ok(json!(lines.join("\n")))
}

// JS `/\n+/g` → single space.
fn collapse_newlines(s: &str) -> String {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let re = RE.get_or_init(|| regex::Regex::new(r"\n+").unwrap());
    re.replace_all(s, " ").into_owned()
}

// ── ai_call_outputs row helpers — pure, fed canned rows shaped like the real table
// (field_name, value_text, value_number, value_boolean, parent_output_id, order_index). ──────

/// `function getScalar(outputs, field_name)` — scoped to top-level (parent_output_id IS NULL).
fn get_scalar(outputs: &[Value], field_name: &str) -> Value {
    for o in outputs {
        if o.get("field_name").and_then(Value::as_str) == Some(field_name) && o.get("parent_output_id").map_or(true, Value::is_null) {
            if let Some(v) = nz(o.get("value_text")) { return v.clone(); }
            if let Some(v) = nz(o.get("value_number")) { return v.clone(); }
            if let Some(v) = nz(o.get("value_boolean")) { return v.clone(); }
            return Value::Null;
        }
    }
    Value::Null
}

/// `function getStringArr(outputs, field_name)` — top-level rows only.
fn get_string_arr(outputs: &[Value], field_name: &str) -> Vec<String> {
    outputs
        .iter()
        .filter(|o| o.get("field_name").and_then(Value::as_str) == Some(field_name) && o.get("parent_output_id").map_or(true, Value::is_null))
        .filter_map(|o| o.get("value_text").and_then(Value::as_str))
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

/// `function unwrapPossibleJsonValue(s)` — auto-unwrap a stringified JSON array/object.
fn unwrap_possible_json_value(v: &Value) -> Value {
    let s = match v.as_str() { Some(s) => s, None => return v.clone() };
    let trimmed = s.trim();
    if trimmed.chars().count() < 2 { return v.clone(); }
    let first = trimmed.chars().next().unwrap();
    let last = trimmed.chars().last().unwrap();
    let bracket_ok = (first == '[' && last == ']') || (first == '{' && last == '}');
    if !bracket_ok { return v.clone(); }
    match serde_json::from_str::<Value>(trimmed) {
        Ok(p @ Value::Array(_)) | Ok(p @ Value::Object(_)) => p,
        _ => v.clone(),
    }
}

/// `function getObjectArr(outputs, field_name)` — reconstruct each object from child rows
/// (rows whose parent_output_id == parent's id), ordered by order_index. Fixture rows carry an
/// `id` key for parent linkage (the real table's PK).
fn get_object_arr(outputs: &[Value], field_name: &str) -> Vec<Value> {
    let mut parents: Vec<&Value> = outputs
        .iter()
        .filter(|o| {
            o.get("field_name").and_then(Value::as_str) == Some(field_name)
                && o.get("value_text").map_or(true, Value::is_null)
                && o.get("value_number").map_or(true, Value::is_null)
                && o.get("value_boolean").map_or(true, Value::is_null)
        })
        .collect();
    parents.sort_by_key(|p| p.get("order_index").and_then(Value::as_i64).unwrap_or(0));
    parents
        .into_iter()
        .map(|p| {
            let parent_id = p.get("id").cloned().unwrap_or(Value::Null);
            let mut obj = Map::new();
            for c in outputs.iter().filter(|c| c.get("parent_output_id").cloned().unwrap_or(Value::Null) == parent_id && !parent_id.is_null()) {
                if let Some(fname) = c.get("field_name").and_then(Value::as_str) {
                    let raw = nz(c.get("value_text")).or_else(|| nz(c.get("value_number"))).or_else(|| nz(c.get("value_boolean"))).cloned().unwrap_or(Value::Null);
                    obj.insert(fname.to_string(), unwrap_possible_json_value(&raw));
                }
            }
            Value::Object(obj)
        })
        .collect()
}

fn load_json(s: Option<&Value>, default: Value) -> Value {
    match s.and_then(Value::as_str) {
        Some(t) => serde_json::from_str(t).unwrap_or(default),
        None => default,
    }
}

pub fn tlContractRedraft(input: &Value, db: &Connection) -> Result<Value, String> {
    // `actor` (JS default `'agent:tl'`) is only consumed by the post-aiCall shaping
    // (`shape_and_supersede`'s `signed_by`) — unreachable here past the deferred boundary, so it
    // is not extracted in this fn (see `shape_and_supersede`, which takes it as its own param).
    let old_slug = nz(input.get("old_slug")).cloned();
    let old_slug = match old_slug { Some(s) if truthy(Some(&s)) => s, _ => return Err("tl-contract-redraft: old_slug is required".into()) };

    let old_rows = crate::db_json::query_rows(db, "SELECT * FROM contracts WHERE slug = ?", &[old_slug.clone()])?;
    let old = match old_rows.as_array().and_then(|a| a.first()) {
        Some(o) => o.clone(),
        None => return Err(format!("tl-contract-redraft: old contract '{}' not found", js_string(&old_slug))),
    };
    let status = old.get("status").and_then(Value::as_str).unwrap_or("");
    if status != "rejected" && status != "superseded" {
        return Err(format!(
            "tl-contract-redraft: re-draft only valid for rejected/superseded contracts; got status='{status}'"
        ));
    }
    let old_superseded_by = old.get("superseded_by").cloned().unwrap_or(Value::Null);
    if truthy(Some(&old_superseded_by)) {
        return Err(format!(
            "tl-contract-redraft: contract '{}' is already superseded by '{}'",
            js_string(&old_slug), js_string(&old_superseded_by)
        ));
    }
    let rejection_reason_md = nz(old.get("rejection_reason_md")).cloned();
    let rejection_reason_md = match rejection_reason_md {
        Some(v) => v,
        None => return Err(format!("tl-contract-redraft: contract '{}' has no rejection_reason_md — nothing to remediate", js_string(&old_slug))),
    };

    let card_slug = old.get("card_slug").cloned().unwrap_or(Value::Null);
    let card_rows = crate::db_json::query_rows(db, "SELECT slug, title, body_json FROM cards WHERE slug = ?", &[card_slug.clone()])?;
    let card = match card_rows.as_array().and_then(|a| a.first()) {
        Some(c) => c.clone(),
        None => return Err(format!("tl-contract-redraft: card '{}' not found", js_string(&card_slug))),
    };
    let card_body_md = match card.get("body_json").and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()) {
        Some(s) => match serde_json::from_str::<Value>(s) {
            Ok(Value::String(inner)) => inner,
            Ok(parsed) => serde_json::to_string_pretty(&parsed).unwrap_or_default(),
            Err(_) => str_qq(card.get("body_json").or(card.get("title"))),
        },
        None => str_qq(card.get("body_json").or(card.get("title"))),
    };

    ensure_prompt(db)?;

    // FEAT-AI-PROMPT-PATH-GROUNDING-UTILITY: best-effort grounding — on error, empty block.
    let candidate_paths_md = crate::ai_ai_prompt_path_grounding::aiPromptPathGrounding(
        &json!({ "card_slug": card.get("slug").cloned().unwrap_or(Value::Null), "max_paths": 50 }),
        db,
    )
    .ok()
    .and_then(|g| g.get("candidate_paths_md").cloned())
    .unwrap_or(json!(""));

    let prior_failures_md = buildPriorFailuresMd(db, &card_slug, &old.get("id").cloned().unwrap_or(Value::Null))?;

    let old_contract_json = json!({
        "slug": old.get("slug").cloned().unwrap_or(Value::Null),
        "title": old.get("title").cloned().unwrap_or(Value::Null),
        "tier": old.get("tier").cloned().unwrap_or(Value::Null),
        "size": old.get("size").cloned().unwrap_or(Value::Null),
        "template_kind": nz(old.get("template_kind")).cloned().unwrap_or(Value::Null),
        "files_touched": load_json(old.get("files_touched_json"), json!([])),
        "acceptance": load_json(old.get("acceptance_json"), json!([])),
        "naming": load_json(old.get("naming_json"), json!({})),
        "must_obey": load_json(old.get("must_obey_json"), json!([])),
        "out_of_scope": load_json(old.get("out_of_scope_json"), json!([])),
        "microtasks": load_json(old.get("microtasks_json"), json!([])),
    });
    let old_contract_json_s: String = serde_json::to_string(&old_contract_json).unwrap_or_default().chars().take(6000).collect();
    let card_body_md_s: String = card_body_md.chars().take(6000).collect();

    let attempt = nz(old.get("current_attempt")).and_then(Value::as_i64).unwrap_or(1) + 1;

    let ai_input = json!({
        "prompt_name": PROMPT_NAME,
        "input_kv": [
            { "key": "card_body_md", "value": card_body_md_s },
            { "key": "candidate_paths_md", "value": candidate_paths_md },
            { "key": "prior_failures_md", "value": prior_failures_md },
            { "key": "old_contract_json", "value": old_contract_json_s },
            { "key": "failure_kind", "value": nz(old.get("failure_kind")).cloned().unwrap_or(json!("(unknown)")) },
            { "key": "rejection_reason_md", "value": rejection_reason_md },
        ],
        "provider_fn": Value::Null,
    });

    // ── PARITY-DEFERRED past here: crate::ai_ai_call::aiCall always returns Err (see its module
    // doc) — so the response-parsing/validation/new-contract-shaping/supersede-persist code below
    // this point is UNREACHABLE by this file's own execution, ported by inspection like
    // book-run.rs's staging loops, and unit-tested directly against canned output_kv rows.
    let contract_ctx = json!({ "current_attempt": attempt });
    let _ = contract_ctx; // JS threads this through context, not aiCall's input; kept for parity commentary
    crate::ai_ai_call::aiCall(&ai_input, db)?;
    unreachable!("ai-call twin's aiCall always returns Err — see module doc")
}

/// Post-aiCall shaping — extracted so the deterministic-but-unreachable-in-this-twin logic is
/// directly unit-testable against canned `ai_call_outputs`-shaped rows (mirrors the JS body from
/// `const outputs = aiResult.output_kv` through the `contractSupersede` call).
#[allow(dead_code)]
fn shape_and_supersede(
    db: &Connection,
    old: &Value,
    old_slug: &Value,
    actor: &Value,
    outputs: &[Value],
    ai_call_id: &Value,
    cost_usd: f64,
) -> Result<Value, String> {
    let reasoning_md = get_scalar(outputs, "reasoning_md");
    let files_touched = get_string_arr(outputs, "files_touched");
    let must_obey = get_string_arr(outputs, "must_obey");
    let out_of_scope = get_string_arr(outputs, "out_of_scope");
    let acceptance = get_object_arr(outputs, "acceptance");

    let naming_arr = get_object_arr(outputs, "naming");
    let mut naming_map = Map::new();
    for n in &naming_arr {
        if let Some(k) = n.get("name").and_then(Value::as_str) {
            let k = k.trim();
            if !k.is_empty() {
                let v = n.get("meaning").map(|m| if m.is_string() { js_string(m) } else { js_string(m) }).unwrap_or_default();
                naming_map.insert(k.to_string(), json!(v));
            }
        }
    }
    let naming = if !naming_map.is_empty() { Value::Object(naming_map) } else { load_json(old.get("naming_json"), json!({})) };

    let template_kind = {
        let g = get_scalar(outputs, "template_kind");
        if truthy(Some(&g)) { g } else { nz(old.get("template_kind")).cloned().unwrap_or(Value::Null) }
    };
    let tier = { let g = get_scalar(outputs, "tier"); if truthy(Some(&g)) { g } else { nz(old.get("tier")).cloned().unwrap_or(json!("standard")) } };
    let risk = { let g = get_scalar(outputs, "risk"); if truthy(Some(&g)) { g } else { nz(old.get("risk")).cloned().unwrap_or(json!(3)) } };
    let complexity = { let g = get_scalar(outputs, "complexity"); if truthy(Some(&g)) { g } else { nz(old.get("complexity")).cloned().unwrap_or(json!(3)) } };
    let size = { let g = get_scalar(outputs, "size"); if truthy(Some(&g)) { g } else { nz(old.get("size")).cloned().unwrap_or(json!("S")) } };

    if files_touched.is_empty() {
        return Err("tl-contract-redraft: AI returned empty files_touched — re-draft refused (would just hit the same MAN Phase 1 block)".into());
    }
    const HARD_LAWS: [&str; 2] = ["NEVER_BYPASS_WITH_AUDIT", "HISTORY_IS_SACRED_NO_REVERT"];
    let missing: Vec<&str> = HARD_LAWS.iter().filter(|l| !must_obey.iter().any(|m| m == *l)).copied().collect();
    if !missing.is_empty() {
        return Err(format!("tl-contract-redraft: AI redraft dropped hard law(s) from must_obey: {} — refused (every contract must carry the core laws; the prompt demands it)", missing.join(", ")));
    }
    if out_of_scope.is_empty() {
        return Err("tl-contract-redraft: AI redraft returned empty out_of_scope — refused (the prompt requires naming the dangerous neighbors DEV must NOT touch)".into());
    }

    let old_attempt = nz(old.get("current_attempt")).and_then(Value::as_i64).unwrap_or(1);
    let attempt = old_attempt + 1;
    let old_slug_s = old_slug.as_str().unwrap_or("");
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let re = RE.get_or_init(|| regex::Regex::new(r"-CONTRACT-\d+$").unwrap());
    let candidate = re.replace(old_slug_s, format!("-CONTRACT-{attempt}").as_str()).into_owned();
    let base_slug = if candidate == old_slug_s { format!("{old_slug_s}-V{attempt}") } else { candidate };

    let base_sha = read_head_sha();
    let tl_briefing_prev = js_string(old.get("tl_briefing_md").unwrap_or(&json!("")));
    let reasoning_str = if reasoning_md.is_null() { "(no rationale)".to_string() } else { js_string(&reasoning_md) };
    let tl_briefing_md = format!("{tl_briefing_prev}\nv{attempt} (auto-redraft): {reasoning_str}");

    let new_contract = json!({
        "slug": base_slug,
        "title": old.get("title").cloned().unwrap_or(Value::Null),
        "card_slug": old.get("card_slug").cloned().unwrap_or(Value::Null),
        "status": "proposed",
        "tier": tier, "size": size, "complexity": complexity, "risk": risk,
        "template_kind": template_kind,
        "files_touched": files_touched,
        "acceptance_json": acceptance,
        "naming": naming,
        "must_obey": must_obey,
        "out_of_scope": out_of_scope,
        "microtasks": Value::Array(vec![]),
        "briefing_md": old.get("briefing_md").cloned().unwrap_or(Value::Null),
        "base_sha": base_sha,
        "signed_by": actor,
        "signed_at": crate::clock::now_iso_nodb(),
        "tl_briefing_md": tl_briefing_md,
        "goal_md": old.get("goal_md").cloned().unwrap_or(Value::Null),
    });

    let supersede_result = crate::contract_contract_supersede::contractSupersede(
        &json!({ "old_slug": old_slug, "new_contract": new_contract }),
        db,
    )?;

    Ok(json!({
        "old_slug": old_slug,
        "new_slug": supersede_result.get("new_slug").cloned().unwrap_or(Value::Null),
        "current_attempt": supersede_result.get("current_attempt").cloned().unwrap_or(Value::Null),
        "reasoning_md": reasoning_md,
        "ai_call_id": ai_call_id,
        "cost_usd": cost_usd,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS brain;").unwrap();
        c.execute_batch(
            "CREATE TABLE cards (slug TEXT PRIMARY KEY, title TEXT, status TEXT, body_json TEXT, updated_at TEXT);
             CREATE TABLE contracts (
               id INTEGER PRIMARY KEY AUTOINCREMENT, slug TEXT NOT NULL UNIQUE, title TEXT NOT NULL,
               status TEXT NOT NULL DEFAULT 'proposed', template_kind TEXT, size TEXT, complexity INTEGER,
               risk INTEGER, tier TEXT, card_slug TEXT, files_touched_json TEXT NOT NULL DEFAULT '[]',
               acceptance_json TEXT NOT NULL DEFAULT '[]', naming_json TEXT NOT NULL DEFAULT '{}',
               edge_cases_json TEXT NOT NULL DEFAULT '[]', must_obey_json TEXT NOT NULL DEFAULT '[]',
               out_of_scope_json TEXT NOT NULL DEFAULT '[]', reference_artifacts_json TEXT NOT NULL DEFAULT '[]',
               briefing_md TEXT, base_sha TEXT, current_attempt INTEGER NOT NULL DEFAULT 1, superseded_by TEXT,
               signed_by TEXT, signed_at TEXT, tl_briefing_md TEXT, tl_evaluation_md TEXT,
               microtasks_json TEXT NOT NULL DEFAULT '[]', goal_md TEXT, failure_kind TEXT,
               rejection_reason_md TEXT, created_at TEXT, updated_at TEXT);
             CREATE TABLE brain.prompt_templates ( id INTEGER PRIMARY KEY, name TEXT NOT NULL, version INTEGER NOT NULL, template_md TEXT NOT NULL, default_tier TEXT NOT NULL, default_temperature TEXT NOT NULL, max_output_tokens INTEGER NOT NULL DEFAULT 1024, superseded_by INTEGER, created_at TEXT NOT NULL DEFAULT (datetime('now')), cross_field_rules_json TEXT, judging_criteria_json TEXT, system_md TEXT, phase_flow TEXT NOT NULL DEFAULT 'single', UNIQUE (name, version) );
             CREATE TABLE brain.prompt_output_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, grounded_refs_json TEXT, UNIQUE (prompt_template_id, field_name) );
             CREATE TABLE brain.prompt_input_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, UNIQUE (prompt_template_id, field_name) );",
        ).unwrap();
        c
    }

    fn a_card(c: &Connection, slug: &str, body: &str) {
        c.execute("INSERT INTO cards (slug, title, status, body_json) VALUES (?1, ?2, 'backlog', ?3)", rusqlite::params![slug, "the card", body]).unwrap();
    }

    #[test]
    fn old_slug_required() {
        let c = setup();
        assert!(tlContractRedraft(&json!({}), &c).unwrap_err().contains("old_slug is required"));
    }

    #[test]
    fn old_contract_not_found() {
        let c = setup();
        assert!(tlContractRedraft(&json!({ "old_slug": "NOPE" }), &c).unwrap_err().contains("not found"));
    }

    #[test]
    fn status_must_be_rejected_or_superseded() {
        let c = setup();
        c.execute("INSERT INTO contracts (slug, title, status, card_slug) VALUES ('C-1', 't', 'proposed', 'CARD-1')", []).unwrap();
        let e = tlContractRedraft(&json!({ "old_slug": "C-1" }), &c).unwrap_err();
        assert!(e.contains("re-draft only valid for rejected/superseded"));
        assert!(e.contains("status='proposed'"));
    }

    #[test]
    fn already_superseded_refused() {
        let c = setup();
        c.execute("INSERT INTO contracts (slug, title, status, card_slug, superseded_by) VALUES ('C-1', 't', 'rejected', 'CARD-1', 'C-2')", []).unwrap();
        let e = tlContractRedraft(&json!({ "old_slug": "C-1" }), &c).unwrap_err();
        assert!(e.contains("already superseded by 'C-2'"));
    }

    #[test]
    fn missing_rejection_reason_refused() {
        let c = setup();
        c.execute("INSERT INTO contracts (slug, title, status, card_slug) VALUES ('C-1', 't', 'rejected', 'CARD-1')", []).unwrap();
        let e = tlContractRedraft(&json!({ "old_slug": "C-1" }), &c).unwrap_err();
        assert!(e.contains("has no rejection_reason_md"));
    }

    #[test]
    fn card_not_found_after_rejection_reason_present() {
        let c = setup();
        c.execute("INSERT INTO contracts (slug, title, status, card_slug, rejection_reason_md) VALUES ('C-1', 't', 'rejected', 'GHOST-CARD', 'bad')", []).unwrap();
        let e = tlContractRedraft(&json!({ "old_slug": "C-1" }), &c).unwrap_err();
        assert!(e.contains("card 'GHOST-CARD' not found"));
    }

    #[test]
    fn reaches_the_parity_deferred_aicall_boundary() {
        // proves every deterministic gate (old/status/superseded/rejection/card lookups,
        // ensure_prompt, grounding, prior-failures) ran cleanly before hitting aiCall's stub.
        let c = setup();
        a_card(&c, "CARD-1", "\"do the thing\"");
        c.execute("INSERT INTO contracts (slug, title, status, card_slug, rejection_reason_md, current_attempt) VALUES ('CARD-1-CONTRACT-1', 't', 'rejected', 'CARD-1', 'missing files', 1)", []).unwrap();
        let e = tlContractRedraft(&json!({ "old_slug": "CARD-1-CONTRACT-1" }), &c).unwrap_err();
        assert!(e.contains("ai-call"), "unexpected error: {e}");
        let n: i64 = c.query_row("SELECT COUNT(*) FROM brain.prompt_templates WHERE name = 'tl.contract.redraft.v1'", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn ensure_prompt_reregisters_on_drift() {
        let c = setup();
        crate::ai_prompt_add::promptAdd(&json!({
            "name": PROMPT_NAME, "template_md": "an old, different template body",
            "output_fields": [{ "field_name": "x", "field_type": "string" }],
        }), &c).unwrap();
        ensure_prompt(&c).unwrap();
        let n: i64 = c.query_row("SELECT COUNT(*) FROM brain.prompt_templates WHERE name = ?", [PROMPT_NAME], |r| r.get(0)).unwrap();
        assert_eq!(n, 2, "drifted template body must trigger a re-register");
        let active = crate::ai_prompt_find::promptFind(&json!({ "name": PROMPT_NAME }), &c).unwrap();
        assert_eq!(active["template_md"], json!(PROMPT_TEMPLATE_MD));
    }

    #[test]
    fn ensure_prompt_is_a_noop_when_unchanged() {
        let c = setup();
        ensure_prompt(&c).unwrap();
        ensure_prompt(&c).unwrap();
        let n: i64 = c.query_row("SELECT COUNT(*) FROM brain.prompt_templates WHERE name = ?", [PROMPT_NAME], |r| r.get(0)).unwrap();
        assert_eq!(n, 1);
    }

    // ── buildPriorFailuresMd ─────────────────────────────────────────────────────────────
    #[test]
    fn build_prior_failures_md_empty_cases() {
        let c = setup();
        assert!(buildPriorFailuresMd(&c, &Value::Null, &json!(5)).unwrap().as_str().unwrap().contains("no prior failed attempts"));
        assert!(buildPriorFailuresMd(&c, &json!("CARD-1"), &json!(1)).unwrap().as_str().unwrap().contains("no prior failed attempts"));
    }

    #[test]
    fn build_prior_failures_md_lists_priors_excludes_current_and_others_and_truncates() {
        let c = setup();
        let long_reason = "x".repeat(260);
        c.execute("INSERT INTO contracts (id, slug, title, status, card_slug, current_attempt, failure_kind, rejection_reason_md) VALUES (1, 'C-1-CONTRACT-1', 't', 'rejected', 'CARD-1', 1, 'tl_empty_refusal', ?)", [long_reason.clone()]).unwrap();
        c.execute("INSERT INTO contracts (id, slug, title, status, card_slug, current_attempt) VALUES (2, 'C-1-CONTRACT-2', 't', 'rejected', 'CARD-1', 2)", []).unwrap(); // no failure_kind, excluded
        c.execute("INSERT INTO contracts (id, slug, title, status, card_slug, current_attempt) VALUES (3, 'C-1-CONTRACT-3', 't', 'rejected', 'OTHER-CARD', 1)", []).unwrap(); // other card
        let md = buildPriorFailuresMd(&c, &json!("CARD-1"), &json!(2)).unwrap();
        let s = md.as_str().unwrap();
        assert!(s.contains("attempt #1 (slug: C-1-CONTRACT-1)"));
        assert!(s.contains("failure_kind: tl_empty_refusal"));
        assert!(s.contains("…")); // truncated past 250 chars
        assert!(!s.contains("C-1-CONTRACT-3"));
    }

    // ── response-parsing helpers (canned ai_call_outputs-shaped rows) ──────────────────────
    #[test]
    fn get_scalar_reads_top_level_only() {
        let rows = vec![
            json!({ "field_name": "tier", "value_text": "high-risk", "parent_output_id": null }),
            json!({ "field_name": "tier", "value_text": "nested-shadow", "parent_output_id": 99 }),
        ];
        assert_eq!(get_scalar(&rows, "tier"), json!("high-risk"));
        assert_eq!(get_scalar(&rows, "missing"), Value::Null);
    }

    #[test]
    fn get_string_arr_filters_nested_and_empty() {
        let rows = vec![
            json!({ "field_name": "files_touched", "value_text": "a.mjs", "parent_output_id": null }),
            json!({ "field_name": "files_touched", "value_text": "b.mjs", "parent_output_id": null }),
            json!({ "field_name": "files_touched", "value_text": "[\"nested.mjs\"]", "parent_output_id": 7 }),
            json!({ "field_name": "files_touched", "value_text": "", "parent_output_id": null }),
        ];
        assert_eq!(get_string_arr(&rows, "files_touched"), vec!["a.mjs".to_string(), "b.mjs".to_string()]);
    }

    #[test]
    fn get_object_arr_reconstructs_children_and_unwraps_stringified_json() {
        let rows = vec![
            json!({ "id": 10, "field_name": "acceptance", "order_index": 0 }),
            json!({ "id": 20, "field_name": "acceptance", "order_index": 1 }),
            json!({ "parent_output_id": 10, "field_name": "kind", "value_text": "sql" }),
            json!({ "parent_output_id": 10, "field_name": "files_touched", "value_text": "[\"docs/a.md\"]" }),
            json!({ "parent_output_id": 20, "field_name": "kind", "value_text": "test" }),
        ];
        let arr = get_object_arr(&rows, "acceptance");
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0]["kind"], json!("sql"));
        assert_eq!(arr[0]["files_touched"], json!(["docs/a.md"]), "stringified nested array unwrapped");
        assert_eq!(arr[1]["kind"], json!("test"));
    }

    #[test]
    fn unwrap_possible_json_value_only_unwraps_bracketed_json() {
        assert_eq!(unwrap_possible_json_value(&json!("plain text")), json!("plain text"));
        assert_eq!(unwrap_possible_json_value(&json!("[1,2,3]")), json!([1, 2, 3]));
        assert_eq!(unwrap_possible_json_value(&json!("{\"k\":1}")), json!({"k": 1}));
        assert_eq!(unwrap_possible_json_value(&json!("[not json")), json!("[not json"));
    }

    // ── shape_and_supersede (post-aiCall shaping, unreachable via aiCall's stub, unit-tested directly) ──
    #[test]
    fn shape_and_supersede_empty_files_touched_refused() {
        let c = setup();
        let old = json!({ "slug": "C-1-CONTRACT-1", "current_attempt": 1, "title": "t", "card_slug": "CARD-1" });
        let e = shape_and_supersede(&c, &old, &json!("C-1-CONTRACT-1"), &json!("agent:tl"), &[], &Value::Null, 0.0).unwrap_err();
        assert!(e.contains("empty files_touched"));
    }

    #[test]
    fn shape_and_supersede_missing_hard_law_refused() {
        let c = setup();
        let old = json!({ "slug": "C-1-CONTRACT-1", "current_attempt": 1, "title": "t", "card_slug": "CARD-1" });
        let outputs = vec![
            json!({ "field_name": "files_touched", "value_text": "a.mjs", "parent_output_id": null }),
            json!({ "field_name": "must_obey", "value_text": "NEVER_BYPASS_WITH_AUDIT", "parent_output_id": null }),
            json!({ "field_name": "out_of_scope", "value_text": "don't touch b.mjs", "parent_output_id": null }),
        ];
        let e = shape_and_supersede(&c, &old, &json!("C-1-CONTRACT-1"), &json!("agent:tl"), &outputs, &Value::Null, 0.0).unwrap_err();
        assert!(e.contains("dropped hard law"));
        assert!(e.contains("HISTORY_IS_SACRED_NO_REVERT"));
    }

    #[test]
    fn shape_and_supersede_empty_out_of_scope_refused() {
        let c = setup();
        let old = json!({ "slug": "C-1-CONTRACT-1", "current_attempt": 1, "title": "t", "card_slug": "CARD-1" });
        let outputs = vec![
            json!({ "field_name": "files_touched", "value_text": "a.mjs", "parent_output_id": null }),
            json!({ "field_name": "must_obey", "value_text": "NEVER_BYPASS_WITH_AUDIT", "parent_output_id": null }),
            json!({ "field_name": "must_obey", "value_text": "HISTORY_IS_SACRED_NO_REVERT", "parent_output_id": null }),
        ];
        let e = shape_and_supersede(&c, &old, &json!("C-1-CONTRACT-1"), &json!("agent:tl"), &outputs, &Value::Null, 0.0).unwrap_err();
        assert!(e.contains("empty out_of_scope"));
    }

    #[test]
    fn shape_and_supersede_happy_path_persists_via_contract_supersede() {
        let c = setup();
        c.execute("INSERT INTO cards (slug, status, updated_at) VALUES ('CARD-1', 'backlog', '2025-12-01 00:00:00')", []).unwrap();
        c.execute("INSERT INTO contracts (slug, title, status, card_slug, current_attempt) VALUES ('CARD-1-CONTRACT-1', 't', 'rejected', 'CARD-1', 1)", []).unwrap();
        let old = json!({ "slug": "CARD-1-CONTRACT-1", "current_attempt": 1, "title": "t", "card_slug": "CARD-1", "tl_briefing_md": "" });
        let outputs = vec![
            json!({ "field_name": "reasoning_md", "value_text": "fixed the files_touched gap", "parent_output_id": null }),
            json!({ "field_name": "files_touched", "value_text": "a.mjs", "parent_output_id": null }),
            json!({ "field_name": "files_touched", "value_text": "b.mjs", "parent_output_id": null }),
            json!({ "field_name": "must_obey", "value_text": "NEVER_BYPASS_WITH_AUDIT", "parent_output_id": null }),
            json!({ "field_name": "must_obey", "value_text": "HISTORY_IS_SACRED_NO_REVERT", "parent_output_id": null }),
            json!({ "field_name": "out_of_scope", "value_text": "don't touch schema", "parent_output_id": null }),
            json!({ "field_name": "tier", "value_text": "standard", "parent_output_id": null }),
        ];
        let r = shape_and_supersede(&c, &old, &json!("CARD-1-CONTRACT-1"), &json!("agent:tl"), &outputs, &json!(42), 0.02).unwrap();
        assert_eq!(r["old_slug"], json!("CARD-1-CONTRACT-1"));
        assert_eq!(r["new_slug"], json!("CARD-1-CONTRACT-2"));
        assert_eq!(r["current_attempt"], json!(2));
        assert_eq!(r["ai_call_id"], json!(42));
        assert_eq!(r["cost_usd"], json!(0.02));
        let (st, sb): (String, String) = c.query_row("SELECT status, superseded_by FROM contracts WHERE slug='CARD-1-CONTRACT-1'", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert_eq!(st, "superseded");
        assert_eq!(sb, "CARD-1-CONTRACT-2");
        let new_status: String = c.query_row("SELECT status FROM contracts WHERE slug='CARD-1-CONTRACT-2'", [], |r| r.get(0)).unwrap();
        assert_eq!(new_status, "proposed");
    }
}
