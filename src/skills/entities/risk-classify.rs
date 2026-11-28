//! risk-classify.rs — twin of skills/entities/risk-classify.mjs (db read+write lane). NAMES ARE
//! CONTRACT: default `riskClassify`. AI BATCH classification of the recurring-failure corpus:
//! ONE high-tier call ingests every eligible pattern (from `failureFeed`) and classifies each with
//! a retryability axis + a design-time prevention sentence + a language signature, minting a
//! `status='proposed'` risk tag (via `riskTag`) per classified pattern.
//!
//! PARITY-DEFERRED: the AI call is dispatched through `crate::ai_ai_call::aiCall`, the deferred
//! network-AI stub (see ai-call.rs's module doc) — it always returns `Err`. Like file-summarize.rs
//! (and unlike pl-research.rs/book-run.rs), the JS here wraps the `await aiCall(...)` in its own
//! `try { } catch (e) { return {...} }` — a failed/absent provider degrades GRACEFULLY: it returns
//! `{ ok:false, considered, classified:0, proposed:[], skipped:considered, error:<msg, ≤200 chars> }`,
//! never throwing. This twin mirrors that exactly: on `aiCall` failure it returns
//! `Ok(json!({ok:false, ...}))`, never propagating the deferred stub's `Err` up to the caller.
//! Everything else — guard (`context.db` unreachable/non-nullable here, same convention as every
//! sibling twin), days/min_count/limit/force resolution, the `failureFeed`/`riskTag{op:'list'}`
//! calls (already-converted twins), the locked/anyTagged candidate filtering, the failures-md
//! prompt-block renderer, and `ensure_prompt` (reusing `ai_prompt_find`/`ai_prompt_add`) — is the
//! real, fully ported deterministic surface. `parseClassifications` (the per-line BATCH-PARSE
//! extractor) is ported as its own PURE function and unit-tested directly with canned text
//! (unreachable from `riskClassify` itself since the deferred aiCall always fails first, but
//! correct by construction and independently testable per the crossing playbook's coverage rule).
// QUALITY: mixed unit — deterministic assembly/db-read+write surface (fully ported, fixture-
// reachable up to the deferred aiCall boundary) + 1 pure response-line-parsing helper (unreachable
// from the default export, unit-tested directly) + 1 bespoke/deferred aiCall dispatch, caught and
// folded into a graceful non-throwing result (matches JS contract: this function never throws once
// the db guard passes).

use rusqlite::Connection;
use serde_json::{json, Value};
use regex::Regex;
use std::collections::HashSet;
use std::sync::OnceLock;

fn str_qq(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(),
        Some(o) => o.to_string(),
    }
} // = JS String(v ?? '')

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

const PROMPT: &str = "risk.classify.batch.v1";
fn retryability_set() -> &'static HashSet<&'static str> {
    static S: OnceLock<HashSet<&'static str>> = OnceLock::new();
    S.get_or_init(|| ["structural", "retryable", "transient", "human"].into_iter().collect())
}
fn human_locked_set() -> &'static HashSet<&'static str> {
    static S: OnceLock<HashSet<&'static str>> = OnceLock::new();
    S.get_or_init(|| ["active", "confirmed", "guarded"].into_iter().collect())
}

const SYSTEM: &str = "You are the failure-corpus CLASSIFIER for an autonomous software substrate. You read recurring failure patterns and classify each so the substrate can PREVENT the class before it recurs. You are terse, decisive, and specific to the evidence.";

const TEMPLATE: &str = "Below are {{count}} RECURRING failure patterns the substrate recorded (each a group of failures sharing a deterministic key). Classify EVERY one.

For each pattern decide:

RETRYABILITY — the load-bearing axis (choose exactly one):
- structural : a re-author/retry can NEVER fix it — it needs a CODE or DESIGN change (a wrong shape, a missing wiring, a bad decomposition). Distinguishing this early is the whole point.
- retryable  : a retry with better context/prompt would plausibly fix it (a truncation, a weak draft).
- transient  : infrastructure/flake — not the work's fault (a locked DB, a network blip, a provider error).
- human      : needs a human decision (an ambiguous requirement, a policy call).

PREVENTION — ONE imperative sentence telling a future DESIGN/AUTHOR step what to DO to avoid this class. It will be injected verbatim into the design of any future card matching this shape, so make it a concrete instruction, not a description of the failure.

LANGS — the file extensions this class touches (comma-separated, no dots), or 'none' if it is language-agnostic.

═══ THE PATTERNS ═══
{{failures}}

Reason as much as you need first. THEN end your reply with EXACTLY ONE line per pattern, nothing after, in EXACTLY this format (use the #N index shown above):

#N RETRY=<structural|retryable|transient|human> LANGS=<ext,ext|none> PREVENTION=<one imperative sentence>

Example:
#1 RETRY=structural LANGS=jsx,tsx PREVENTION=Author each cohesive file in a single call; never concatenate per-function fragments.";

fn prompt_output_fields() -> Value {
    json!([{ "field_name": "classifications", "field_type": "string", "description": "reasoning followed by one #N RETRY=… LANGS=… PREVENTION=… line per pattern" }])
}
fn judging_criteria() -> Value {
    json!([
        "Ends with exactly one #N RETRY=… LANGS=… PREVENTION=… line per pattern.",
        "RETRY=structural only when a re-author could never fix it (a code/design defect).",
        "PREVENTION is a concrete imperative instruction, not a restatement of the failure.",
    ])
}

fn slugify(s: &str) -> String {
    static SP: OnceLock<Regex> = OnceLock::new();
    let re = SP.get_or_init(|| Regex::new(r"(?i)[^a-z0-9]+").unwrap());
    let base = if s.is_empty() { "risk" } else { s };
    let squashed = re.replace_all(base, "-");
    let trimmed = squashed.trim_matches('-');
    let lowered = trimmed.to_lowercase();
    lowered.chars().take(60).collect()
}

// `.replace(/\s+/g, ' ')` — collapses whitespace runs to a single space, WITHOUT trimming edges
// (unlike `.split_whitespace().join(' ')`, which would also strip leading/trailing whitespace).
fn collapse_ws(s: &str) -> String {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"\s+").unwrap());
    re.replace_all(s, " ").to_string()
}

fn sniff_langs(s: &str) -> Vec<String> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"(?i)\.(jsx|tsx|ts|js|scss|css|py|mjs|json)\b").unwrap());
    let mut seen: Vec<String> = Vec::new();
    for m in re.find_iter(s) {
        let ext = m.as_str()[1..].to_string(); // drop leading '.'
        if !seen.contains(&ext) {
            seen.push(ext);
        }
    }
    seen
}

// BATCH-PARSE-BY-LINE — the batch analogue of parseJudgeLine. Tolerant of #N / N. / N) prefixes;
// a line without a usable PREVENTION is dropped (useless for injection).
pub fn parseClassifications(text: &str) -> Vec<Value> {
    static RETRY_RE: OnceLock<Regex> = OnceLock::new();
    static LANGS_RE: OnceLock<Regex> = OnceLock::new();
    static PREV_RE: OnceLock<Regex> = OnceLock::new();
    let retry_re = RETRY_RE.get_or_init(|| Regex::new(r"(?i)^\s*#?(\d+)[.):]?\s+RETRY\s*[=:]\s*(structural|retryable|transient|human)\b").unwrap());
    let langs_re = LANGS_RE.get_or_init(|| Regex::new(r"(?i)LANGS\s*[=:]\s*([^\n]*?)(?:\s+PREVENTION\b|$)").unwrap());
    let prev_re = PREV_RE.get_or_init(|| Regex::new(r"(?i)PREVENTION\s*[=:]\s*(.+)$").unwrap());

    let mut out: Vec<Value> = Vec::new();
    for line in text.split('\n') {
        let m = match retry_re.captures(line) { Some(c) => c, None => continue };
        let index: i64 = m[1].parse().unwrap_or(0);
        let retryability = m[2].to_lowercase();
        let langs_m = langs_re.captures(line);
        let prev_m = prev_re.captures(line);
        let prevention = prev_m.map(|c| c[1].trim().to_string()).unwrap_or_default();
        if prevention.is_empty() { continue; }
        let langs: Vec<Value> = langs_m
            .map(|c| c[1].split(',')
                .map(|s| s.trim().trim_start_matches('.').to_string())
                .filter(|x| !x.is_empty() && x.to_lowercase() != "none")
                .map(Value::String)
                .collect())
            .unwrap_or_default();
        let prevention_clamped: String = prevention.chars().take(400).collect();
        out.push(json!({ "index": index, "retryability": retryability, "langs": langs, "prevention": prevention_clamped }));
    }
    out
}

// JS: `try { existing = promptFind(...) } catch { existing = null }; if (existing &&
// existing.template_md === TEMPLATE) return;` — promptAdd only when missing or drifted.
fn ensure_prompt(db: &Connection) -> Result<(), String> {
    if let Ok(existing) = crate::ai_prompt_find::promptFind(&json!({ "name": PROMPT }), db) {
        if existing.get("template_md").and_then(Value::as_str) == Some(TEMPLATE) {
            return Ok(());
        }
    }
    crate::ai_prompt_add::promptAdd(
        &json!({
            "name": PROMPT, "template_md": TEMPLATE, "system_md": SYSTEM,
            "default_tier": "high", "default_temperature": "cold", "max_output_tokens": 2200,
            "output_fields": prompt_output_fields(),
            "judging_criteria": judging_criteria(),
        }),
        db,
    )?;
    Ok(())
}

pub fn riskClassify(input: &Value, db: &Connection) -> Result<Value, String> {
    let days_num = js_number(input.get("days"));
    let days: f64 = if days_num.is_finite() && days_num > 0.0 { days_num } else { 30.0 };
    let min_count_num = js_number(input.get("min_count"));
    let min_count: f64 = if min_count_num.is_finite() { min_count_num.max(1.0) } else { 2.0 };
    let limit_num = js_number(input.get("limit"));
    let limit: usize = if limit_num.is_finite() { limit_num.max(1.0).min(60.0) as usize } else { 40 };
    let force = matches!(input.get("force"), Some(Value::Bool(true)));
    let only_unclassified = matches!(input.get("only_unclassified"), Some(Value::Bool(true)));

    let feed = crate::driver_failure_feed::failureFeed(&json!({ "days": days, "limit": 400 }), db)?;
    let existing_res = crate::entities_risk_tag::riskTag(&json!({ "op": "list" }), db)?;
    let existing: Vec<Value> = existing_res.get("tags").and_then(Value::as_array).cloned().unwrap_or_default();
    let locked: HashSet<String> = existing.iter()
        .filter(|t| t.get("status").and_then(Value::as_str).map_or(false, |s| human_locked_set().contains(s)))
        .filter_map(|t| t.get("source_pattern_key").and_then(Value::as_str))
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect();
    let any_tagged: HashSet<String> = existing.iter()
        .filter_map(|t| t.get("source_pattern_key").and_then(Value::as_str))
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect();

    let groups: Vec<Value> = feed.get("groups").and_then(Value::as_array).cloned().unwrap_or_default();
    let candidates: Vec<Value> = groups.into_iter()
        .filter(|g| js_number(g.get("count")) >= min_count)
        .filter(|g| {
            let pk = str_qq(g.get("pattern_key"));
            force || !locked.contains(&pk)
        })
        .filter(|g| {
            let pk = str_qq(g.get("pattern_key"));
            !only_unclassified || !any_tagged.contains(&pk)
        })
        .take(limit)
        .collect();

    if candidates.is_empty() {
        return Ok(json!({
            "ok": true, "considered": 0, "classified": 0, "proposed": [], "skipped": 0,
            "reason": "no eligible recurring patterns",
        }));
    }

    let rows: Vec<Value> = feed.get("rows").and_then(Value::as_array).cloned().unwrap_or_default();
    let rep_row = |pk: &str| -> Value {
        rows.iter().find(|r| str_qq(r.get("pattern_key")) == pk).cloned().unwrap_or(json!({}))
    };

    let failures_md: String = candidates.iter().enumerate().map(|(i, g)| {
        let pk = str_qq(g.get("pattern_key"));
        let rr = rep_row(&pk);
        let count = str_qq(g.get("count"));
        let sample_title: String = collapse_ws(&str_qq(g.get("sample_title"))).chars().take(160).collect();
        let remedy_raw = str_qq(rr.get("remedy_md"));
        let detail_raw = if !remedy_raw.is_empty() { remedy_raw } else { str_qq(rr.get("detail")) };
        let detail: String = collapse_ws(&detail_raw).chars().take(300).collect();
        let detail_line = if !detail.is_empty() { format!("\n  detail: {detail}") } else { String::new() };
        format!("#{} [{}] ×{}\n  sample: {}{}", i + 1, pk, count, sample_title, detail_line)
    }).collect::<Vec<_>>().join("\n\n");

    ensure_prompt(db)?;

    let ai_input = json!({
        "prompt_name": PROMPT,
        "input_kv": [
            { "key": "count", "value": candidates.len().to_string() },
            { "key": "failures", "value": failures_md },
        ],
        "tier": "high", "temperature": "cold", "phase_flow": "single_only",
        "two_phase": false, "structured_output": false, "provider_fn": Value::Null,
    });
    let ai_result = match crate::ai_ai_call::aiCall(&ai_input, db) {
        Ok(r) => r,
        Err(e) => {
            let msg: String = e.chars().take(200).collect();
            return Ok(json!({
                "ok": false, "considered": candidates.len() as i64, "classified": 0,
                "proposed": [], "skipped": candidates.len() as i64, "error": msg,
            }));
        }
    };
    let raw = ai_result.get("output_kv").and_then(Value::as_array)
        .and_then(|a| a.iter().find(|o| o.get("value_text").map_or(false, |v| !v.is_null())))
        .and_then(|o| o.get("value_text")).and_then(Value::as_str).unwrap_or("").to_string();
    let ai_call_id = ai_result.get("ai_call_id").cloned().unwrap_or(Value::Null);

    let parsed: Vec<Value> = parseClassifications(&raw).into_iter()
        .filter(|p| p.get("retryability").and_then(Value::as_str).map_or(false, |r| retryability_set().contains(r)))
        .collect();

    let mut proposed: Vec<Value> = Vec::new();
    for p in &parsed {
        let index = p.get("index").and_then(Value::as_i64).unwrap_or(0);
        let g = match candidates.get((index - 1) as usize) {
            Some(g) if index >= 1 => g,
            _ => continue,
        };
        let pk = str_qq(g.get("pattern_key"));
        let rr = rep_row(&pk);
        let p_langs: Vec<String> = p.get("langs").and_then(Value::as_array).map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect()).unwrap_or_default();
        let langs: Vec<String> = if !p_langs.is_empty() {
            p_langs
        } else {
            let corpus = format!("{} {}", str_qq(rr.get("detail")), str_qq(rr.get("title")));
            let corpus2 = format!("{} {}", corpus, str_qq(g.get("sample_title")));
            sniff_langs(&corpus2)
        };
        let prevention = str_qq(p.get("prevention"));
        let retryability = str_qq(p.get("retryability"));
        let description: String = str_qq(g.get("sample_title")).chars().take(140).collect();

        crate::entities_risk_tag::riskTag(&json!({
            "op": "mint", "slug": slugify(&pk), "prevention_md": prevention, "retryability": retryability,
            "source_pattern_key": pk, "status": "proposed", "description": description,
            "applies_to": { "globs": [], "langs": langs.clone() },
        }), db)?;
        proposed.push(json!({ "pattern_key": pk, "retryability": retryability, "prevention": prevention, "langs": langs }));
    }

    let classified = proposed.len() as i64;
    Ok(json!({
        "ok": true, "considered": candidates.len() as i64, "classified": classified,
        "proposed": proposed, "skipped": candidates.len() as i64 - classified, "ai_call_id": ai_call_id,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS brain; ATTACH ':memory:' AS judgments; ATTACH ':memory:' AS memory;").unwrap();
        c.execute_batch(
            "CREATE TABLE tags (slug TEXT PRIMARY KEY, kind TEXT, description TEXT, applies_to_json TEXT, prevention_md TEXT, retryability TEXT, source_pattern_key TEXT, fix_commit TEXT, canary_test TEXT, weight INTEGER, status TEXT);
             CREATE TABLE contracts (id INTEGER PRIMARY KEY, card_slug TEXT, files_touched_json TEXT);
             CREATE TABLE brain.driver_ticks (id INTEGER PRIMARY KEY, started_at TEXT, card_slug TEXT, decided_agent TEXT, decided_action TEXT, decided_reason TEXT, outcome TEXT, error_message TEXT);
             CREATE TABLE build_gate_runs (id INTEGER PRIMARY KEY, contract_slug TEXT, evidence_md TEXT, ts TEXT, passed INTEGER, skipped INTEGER);
             CREATE TABLE lint_gate_runs (id INTEGER PRIMARY KEY, contract_slug TEXT, mode TEXT, version TEXT, findings_json TEXT, ts TEXT, passed INTEGER);
             CREATE TABLE judgments.judgment_cases (judgment_id TEXT, card_slug TEXT, scope_ref TEXT, goal_line TEXT, updated_at TEXT, verdict TEXT);
             CREATE TABLE judgments.judgment_items (judgment_id TEXT, side TEXT, judge_value INTEGER, statement TEXT);
             CREATE TABLE qa_runs (id INTEGER PRIMARY KEY, contract_slug TEXT, ts TEXT, acceptance_fail INTEGER, all_passed INTEGER);
             CREATE TABLE qa_run_results (qa_run_id INTEGER, ac_index INTEGER, kind TEXT, evidence_md TEXT, passed INTEGER);
             CREATE TABLE brain.ai_calls (id INTEGER PRIMARY KEY, ts TEXT, model_id TEXT, outcome TEXT, outcome_class TEXT, session_id TEXT, contract_slug TEXT);
             CREATE TABLE notifications (id INTEGER PRIMARY KEY, ts TEXT, kind TEXT, severity TEXT, message TEXT, related_card_slug TEXT, dismissed_at TEXT);
             CREATE TABLE memory.error_events (id INTEGER PRIMARY KEY, ts TEXT, pattern_key TEXT, skill TEXT, message TEXT, recovery_outcome TEXT);
             CREATE TABLE brain.prompt_templates ( id INTEGER PRIMARY KEY, name TEXT NOT NULL, version INTEGER NOT NULL, template_md TEXT NOT NULL, default_tier TEXT NOT NULL, default_temperature TEXT NOT NULL, max_output_tokens INTEGER NOT NULL DEFAULT 1024, superseded_by INTEGER, created_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')), cross_field_rules_json TEXT, judging_criteria_json TEXT, system_md TEXT, phase_flow TEXT NOT NULL DEFAULT 'single', UNIQUE (name, version) );
             CREATE TABLE brain.prompt_output_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, grounded_refs_json TEXT, UNIQUE (prompt_template_id, field_name) );
             CREATE TABLE brain.prompt_input_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, UNIQUE (prompt_template_id, field_name) );",
        ).unwrap();
        c
    }

    #[test]
    fn no_eligible_patterns_returns_early_without_ai_call() {
        let c = setup();
        let r = riskClassify(&json!({}), &c).unwrap();
        assert_eq!(r, json!({ "ok": true, "considered": 0, "classified": 0, "proposed": [], "skipped": 0, "reason": "no eligible recurring patterns" }));
        let n: i64 = c.query_row("SELECT COUNT(*) FROM brain.prompt_templates", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn reaches_the_parity_deferred_aicall_boundary_and_degrades_gracefully() {
        let c = setup();
        // 3 recurring ticks sharing the same pattern_key -> one group with count=3 (>= default min_count 2)
        c.execute_batch(
            "INSERT INTO brain.driver_ticks (id, started_at, decided_action, outcome, error_message) VALUES
               (1, '2025-12-30 10:00:00', 'dev.impl', 'failed', 'boom A'),
               (2, '2025-12-29 10:00:00', 'dev.impl', 'failed', 'boom B'),
               (3, '2025-12-28 10:00:00', 'dev.impl', 'failed', 'boom C');",
        ).unwrap();
        let r = riskClassify(&json!({}), &c).unwrap();
        assert_eq!(r["ok"], json!(false));
        assert_eq!(r["considered"], json!(1));
        assert_eq!(r["classified"], json!(0));
        assert_eq!(r["proposed"], json!([]));
        assert_eq!(r["skipped"], json!(1));
        assert!(r["error"].as_str().unwrap().len() > 0);
        // ensure_prompt DID run (deterministic surface reached the AI boundary).
        let n: i64 = c.query_row("SELECT COUNT(*) FROM brain.prompt_templates WHERE name = 'risk.classify.batch.v1'", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1);
        // no risk tag was minted — the deferred aiCall failed before any parse/mint happened.
        let tagn: i64 = c.query_row("SELECT COUNT(*) FROM tags", [], |r| r.get(0)).unwrap();
        assert_eq!(tagn, 0);
    }

    #[test]
    fn human_locked_tags_exclude_their_pattern_unless_forced() {
        let c = setup();
        c.execute_batch(
            "INSERT INTO brain.driver_ticks (id, started_at, decided_action, outcome, error_message) VALUES
               (1, '2025-12-30 10:00:00', 'dev.impl', 'failed', 'boom A'),
               (2, '2025-12-29 10:00:00', 'dev.impl', 'failed', 'boom B');",
        ).unwrap();
        // the tick pattern_key is tick:failed:dev.impl (constructed the same way failure-feed.rs builds it)
        c.execute("INSERT INTO tags (slug, kind, source_pattern_key, status) VALUES ('locked-1', 'risk', 'tick:failed:dev.impl', 'confirmed')", []).unwrap();
        let r = riskClassify(&json!({}), &c).unwrap();
        assert_eq!(r, json!({ "ok": true, "considered": 0, "classified": 0, "proposed": [], "skipped": 0, "reason": "no eligible recurring patterns" }));
        // force=true bypasses the lock and reaches the deferred boundary
        let r2 = riskClassify(&json!({ "force": true }), &c).unwrap();
        assert_eq!(r2["considered"], json!(1));
    }

    #[test]
    fn only_unclassified_skips_already_tagged_patterns() {
        let c = setup();
        c.execute_batch(
            "INSERT INTO brain.driver_ticks (id, started_at, decided_action, outcome, error_message) VALUES
               (1, '2025-12-30 10:00:00', 'dev.impl', 'failed', 'boom A'),
               (2, '2025-12-29 10:00:00', 'dev.impl', 'failed', 'boom B');",
        ).unwrap();
        c.execute("INSERT INTO tags (slug, kind, source_pattern_key, status) VALUES ('proposed-1', 'risk', 'tick:failed:dev.impl', 'proposed')", []).unwrap();
        let r = riskClassify(&json!({ "only_unclassified": true }), &c).unwrap();
        assert_eq!(r, json!({ "ok": true, "considered": 0, "classified": 0, "proposed": [], "skipped": 0, "reason": "no eligible recurring patterns" }));
    }

    #[test]
    fn parse_classifications_extracts_tolerant_lines_and_drops_unusable_ones() {
        let text = "Some reasoning here about the patterns.\n\
#1 RETRY=structural LANGS=jsx,tsx PREVENTION=Author each cohesive file in a single call.\n\
2) RETRY=retryable LANGS=none PREVENTION=Retry with more context next time.\n\
3. RETRY=transient LANGS=.py,.js PREVENTION=Nothing to do, infra flake.\n\
#4 RETRY=bogus LANGS=js PREVENTION=should be dropped by the caller filter.\n\
#5 RETRY=human PREVENTION=missing LANGS entirely is fine, langs empty.\n\
#6 RETRY=structural LANGS=js PREVENTION=\n"; // empty PREVENTION -> dropped
        let parsed = parseClassifications(text);
        // real JS truth (verified via node repro of parseClassifications): #4 is dropped
        // too — the RETRY capture group is itself the enum alternation
        // (structural|retryable|transient|human), so RETRY=bogus never matches the retry_re
        // at all (no #N line is even recognized), not merely "recognized but unfiltered".
        // Only #1, 2, 3, 5 survive; #6 is dropped separately for its empty PREVENTION.
        assert_eq!(parsed.len(), 4);
        assert_eq!(parsed[0], json!({ "index": 1, "retryability": "structural", "langs": ["jsx", "tsx"], "prevention": "Author each cohesive file in a single call." }));
        assert_eq!(parsed[1]["index"], json!(2));
        assert_eq!(parsed[1]["langs"], json!([])); // 'none' filtered out
        assert_eq!(parsed[2]["langs"], json!(["py", "js"])); // leading dots stripped
        assert_eq!(parsed[3]["index"], json!(5)); // #4 (bogus retry) never matched; #5 is next surviving line
        assert_eq!(parsed[3]["retryability"], json!("human"));
        assert_eq!(parsed[3]["langs"], json!([])); // LANGS absent entirely
    }

    #[test]
    fn slugify_and_sniff_langs() {
        assert_eq!(slugify("tick:failed:dev.impl"), "tick-failed-dev-impl");
        assert_eq!(slugify(""), "risk");
        assert_eq!(slugify(&"a".repeat(100)), "a".repeat(60));
        assert_eq!(sniff_langs("touches a.jsx and b.TSX and a.jsx again"), vec!["jsx".to_string(), "TSX".to_string()]);
        assert_eq!(sniff_langs("no extensions here"), Vec::<String>::new());
    }
}
