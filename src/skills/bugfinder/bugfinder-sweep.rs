//! bugfinder-sweep.rs — twin of skills/bugfinder/bugfinder-sweep.mjs (db, no cross-db writes).
//! NAMES ARE CONTRACT: default `bugfinderSweep`. WAVE 3: ONE mid-tier call over the whole scope
//! for anything the deterministic wave and the shard swarm structurally cannot see — the unknown
//! unknowns outside the covered categories. Told exactly what was already hunted/found so a
//! mid-tier token is never spent re-discovering a covered defect; a lead in a covered category is
//! void by rule ('off-scope'), same as the court's OOB discipline. Same verbatim-evidence floor:
//! one call, zero trust.
//!
//! PARITY-DEFERRED: the AI call is dispatched through `crate::ai_ai_call::aiCall`, the deferred
//! network-AI stub (see ai-call.rs's module doc) — it always returns `Err`, so `bugfinderSweep`
//! catches that Err into its own `errors` accumulator (the JS wraps the whole aiCall+parse block
//! in try/catch and returns `{calls:0, cost_usd:0, kept:[], dropped:[], errors:[{error}]}`, never
//! propagating) once the (real, ported) deterministic surface above it has run: the files guard,
//! the covered-set + bundle + known-leads assembly (`ensure_sweep_prompt`, reusing the already-
//! converted `ai_prompt_find`/`ai_prompt_add` twins, mirroring book-run.rs's `ensure_faculty` /
//! pl-research.rs's `ensure_prompt`). The response-parsing helper (`extract_findings_text`) and
//! the per-lead classify/keep/drop loop (`classify_lead`) are ported as their own PURE functions
//! and unit-tested directly with canned `res`/lead values — the classify loop reuses the already-
//! converted `bugfinder__helpers_grounding::{kebab, parseFindingsJson, validateLead}` twins,
//! mirroring the JS file's own import of `_helpers/grounding.mjs`.
// QUALITY: mixed unit — deterministic assembly/bundle/classify surface (fully ported,
// fixture-reachable up to the deferred aiCall boundary, whose Err is caught into `errors` rather
// than propagated) + 1 pure response-text extractor (unreachable from the default export since
// aiCall never resolves, unit-tested directly) + 1 bespoke/deferred aiCall dispatch (no
// fixture/frontier for the call itself).
use rusqlite::Connection;
use serde_json::{json, Map, Value};

// ── standard helpers (playbook) ─────────────────────────────────────────────────────────────
fn str_qq(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(),
        Some(o) => o.to_string(),
    }
} // = JS String(v ?? '')

const PROMPT: &str = "bugfinder-sweep";
const SYSTEM: &str = "You are the FINAL SWEEP of a layered bug finder. Deterministic tools already checked syntax, types and import existence; specialist finders already hunted the COVERED categories. Your job is ONLY what none of them cover: any OTHER real defect — e.g. resource leaks, config/env mistakes, data loss on error paths, broken invariants, encoding/locale issues, cache invalidation, migration hazards, incorrect state machines. Rules: (1) NEVER report a defect in a covered category — it is handled elsewhere and will be discarded. (2) {\"findings\":[]} is a complete, correct answer; NEVER invent a finding to seem useful. (3) \"evidence\" MUST be code copied VERBATIM from the named file — non-matching evidence is discarded automatically. (4) Max 8 findings across all files, highest confidence first. Output ONLY valid JSON: {\"findings\":[{\"file\":\"path\",\"line\":N,\"category\":\"short-kebab-label\",\"severity\":\"low|medium|high\",\"detail\":\"one sentence: what is wrong and when it breaks\",\"evidence\":\"VERBATIM code\"}]}. No prose outside the JSON.";
const TEMPLATE: &str = "COVERED CATEGORIES (never report these): {{covered}}\n\nALREADY FOUND (never re-report these):\n{{known}}\n\nFILES:\n{{bundle}}\n\nOutput the findings JSON.";

const MAX_BUNDLE_CHARS: usize = 28_000;
const MAX_LEADS: usize = 8;

// mirrors JS ensureSweepPrompt: try promptFind; if found AND template/system match verbatim,
// return; otherwise promptAdd (no re-find after — unlike book-run's ensure_faculty/pl-research's
// ensure_prompt, this file never re-reads after the add).
fn ensure_sweep_prompt(db: &Connection) -> Result<(), String> {
    if let Ok(existing) = crate::ai_prompt_find::promptFind(&json!({ "name": PROMPT }), db) {
        let matches = existing.get("template_md").and_then(Value::as_str) == Some(TEMPLATE)
            && existing.get("system_md").and_then(Value::as_str) == Some(SYSTEM);
        if matches {
            return Ok(());
        }
    }
    crate::ai_prompt_add::promptAdd(
        &json!({
            "name": PROMPT,
            "template_md": TEMPLATE,
            "system_md": SYSTEM,
            "default_tier": "mid",
            "default_temperature": "cold",
            "phase_flow": "single_only",
            "max_output_tokens": 2000,
            "output_fields": [{ "field_name": "findings_json", "field_type": "string" }],
        }),
        db,
    )?;
    Ok(())
}

// `f.path.replace(/\\/g,'/')` — used both for byPath keys and lead-file normalization.
fn norm_path(v: Option<&Value>) -> String {
    str_qq(v).replace('\\', "/")
}

// `res?.output_kv?.find(o => o.field_name==='findings_json' && o.parent_output_id==null)?.value_text ?? ''`
// Pure — unreachable from bugfinderSweep itself (aiCall's Err is caught before `res` exists), unit-
// tested directly per the crossing playbook's coverage rule (mirrors pl-research's build_research_report).
fn extract_findings_text(res: &Value) -> String {
    let rows = res.get("output_kv").and_then(Value::as_array);
    let Some(rows) = rows else { return String::new() };
    for row in rows {
        let field_ok = row.get("field_name").and_then(Value::as_str) == Some("findings_json");
        let parent_null = row.get("parent_output_id").map_or(true, |v| v.is_null());
        if field_ok && parent_null {
            return match row.get("value_text") {
                Some(Value::Null) | None => String::new(),
                Some(v) => str_qq(Some(v)),
            };
        }
    }
    String::new()
}

struct ClassifyOutcome {
    kept: Option<Value>,
    dropped: Option<Value>,
}

// The per-lead loop body from the JS `for (const raw of ...) {...}` — pure given the covered set +
// the byPath lookup outcome, so it is independently unit-testable. Mirrors JS byPath lookup exactly:
// exact match first, else the first entry whose key ENDS WITH the raw path.
fn classify_lead(raw: &Value, covered: &std::collections::HashSet<String>, by_path: &[(String, Value)], model: &Value) -> ClassifyOutcome {
    let cat_label_v = crate::bugfinder__helpers_grounding::kebab(raw.get("category").unwrap_or(&Value::Null));
    let cat_label = cat_label_v.as_str().unwrap_or("").to_string();
    let cat_label = if cat_label.is_empty() { "uncategorized".to_string() } else { cat_label };
    let file_path = norm_path(raw.get("file"));

    let target = by_path
        .iter()
        .find(|(p, _)| *p == file_path)
        .or_else(|| by_path.iter().find(|(p, _)| p.ends_with(&file_path)))
        .map(|(_, v)| v.clone());

    let mut base = Map::new();
    base.insert("category".into(), json!(format!("other:{cat_label}")));
    base.insert("severity".into(), json!("medium"));
    base.insert(
        "file".into(),
        target
            .as_ref()
            .and_then(|t| t.get("path").cloned())
            .unwrap_or_else(|| if file_path.is_empty() { Value::Null } else { json!(file_path) }),
    );
    base.insert("line".into(), Value::Null);
    let detail_raw = str_qq(raw.get("detail"));
    let detail: String = detail_raw.chars().take(500).collect();
    let evidence_raw = str_qq(raw.get("evidence"));
    let evidence: String = evidence_raw.chars().take(400).collect();
    base.insert("detail".into(), json!(detail));
    base.insert("evidence".into(), json!(evidence));
    base.insert("model".into(), model.clone());

    if covered.contains(&cat_label) {
        let mut dropped = base.clone();
        dropped.insert("drop_reason".into(), json!("off-scope"));
        return ClassifyOutcome { kept: None, dropped: Some(Value::Object(dropped)) };
    }
    let Some(target) = target else {
        let mut dropped = base.clone();
        dropped.insert("drop_reason".into(), json!("unknown-file"));
        return ClassifyOutcome { kept: None, dropped: Some(Value::Object(dropped)) };
    };
    let source = target.get("source").cloned().unwrap_or(json!(""));
    let v = crate::bugfinder__helpers_grounding::validateLead(raw, &source);
    let ok = v.get("ok").and_then(Value::as_bool).unwrap_or(false);
    let lead = v.get("lead").cloned().unwrap_or(json!({}));
    let severity = lead.get("severity").cloned().unwrap_or(json!("medium"));
    let line = lead.get("line").cloned().unwrap_or(Value::Null);
    base.insert("severity".into(), severity);
    base.insert("line".into(), line);
    let lead_detail = lead.get("detail").and_then(Value::as_str).unwrap_or("");
    if ok && !lead_detail.is_empty() {
        ClassifyOutcome { kept: Some(Value::Object(base)), dropped: None }
    } else {
        let mut dropped = base.clone();
        let reason = if ok {
            "malformed".to_string()
        } else {
            v.get("reason").and_then(Value::as_str).unwrap_or("malformed").to_string()
        };
        dropped.insert("drop_reason".into(), json!(reason));
        ClassifyOutcome { kept: None, dropped: Some(Value::Object(dropped)) }
    }
}

pub fn bugfinderSweep(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS `const files = Array.isArray(input?.files) ? input.files : null; if (!files) throw
    // err.required(...)`. `const db = context?.db; if (!db) throw err.required(...)` is
    // unreachable here (Rust's `db: &Connection` is never absent).
    let files = input.get("files").and_then(Value::as_array);
    let Some(files) = files else {
        return Err("bugfinder-sweep: files is required".to_string());
    };

    let usable: Vec<Value> = files
        .iter()
        .filter(|f| f.get("source").and_then(Value::as_str).map_or(false, |s| !s.trim().is_empty()))
        .cloned()
        .collect();
    if usable.is_empty() {
        return Ok(json!({ "calls": 0, "cost_usd": 0, "kept": [], "dropped": [], "errors": [] }));
    }

    // JS `new Set((input?.covered ?? []).map(kebab).filter(Boolean))` — a Set's iteration order is
    // insertion order (first-seen), which the covered-label prompt text below depends on.
    let mut covered_order: Vec<String> = Vec::new();
    let mut covered: std::collections::HashSet<String> = std::collections::HashSet::new();
    for c in input.get("covered").and_then(Value::as_array).cloned().unwrap_or_default() {
        let k = crate::bugfinder__helpers_grounding::kebab(&c);
        let s = k.as_str().unwrap_or("").to_string();
        if s.is_empty() { continue; }
        if covered.insert(s.clone()) {
            covered_order.push(s);
        }
    }

    let per_file = std::cmp::max(1200, MAX_BUNDLE_CHARS / usable.len());
    let bundle = usable
        .iter()
        .map(|f| {
            let path = str_qq(f.get("path"));
            let source = str_qq(f.get("source"));
            let clipped: String = source.chars().take(per_file).collect();
            format!("=== FILE: {path} ===\n{clipped}")
        })
        .collect::<Vec<_>>()
        .join("\n\n");

    let known_list: Vec<String> = input
        .get("known")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .take(60)
        .map(|v| str_qq(Some(&v)))
        .collect();
    let known_joined = known_list.join("\n");
    let known = if known_joined.is_empty() { "(nothing yet)".to_string() } else { known_joined };

    let by_path: Vec<(String, Value)> = usable.iter().map(|f| (norm_path(f.get("path")), f.clone())).collect();

    ensure_sweep_prompt(db)?;

    let mut kept: Vec<Value> = Vec::new();
    let mut dropped: Vec<Value> = Vec::new();
    let mut errors: Vec<Value> = Vec::new();
    let mut cost = 0.0f64;
    let mut calls = 0i64;

    let covered_label = if covered_order.is_empty() { "(none)".to_string() } else { covered_order.join(" · ") };

    let ai_input = json!({
        "prompt_name": PROMPT,
        "input_kv": [
            { "key": "covered", "value": covered_label },
            { "key": "known", "value": known },
            { "key": "bundle", "value": bundle },
        ],
        // JS threads `context?.provider_fn` (a JS function reference for provider selection/DI) —
        // no Rust equivalent to carry, so `null`, same convention as book-run.rs's/pl-research.rs's
        // `provider_fn`.
        "provider_fn": Value::Null,
    });

    // ── the AI call: PARITY-DEFERRED from here on (see module doc). JS wraps this whole block
    // (call + parse + classify loop) in try/catch, pushing `{error}` into `errors` on any throw —
    // aiCall's stub always returns Err, so that is exactly what happens here.
    match crate::ai_ai_call::aiCall(&ai_input, db) {
        Ok(res) => {
            calls = 1;
            cost = res.get("cost_usd").and_then(Value::as_f64).unwrap_or(0.0);
            let text = extract_findings_text(&res);
            let model = res.get("model_id").cloned().unwrap_or(Value::Null);
            let leads = crate::bugfinder__helpers_grounding::parseFindingsJson(&json!(text));
            let leads_arr = leads.as_array().cloned().unwrap_or_default();
            for raw in leads_arr.into_iter().take(MAX_LEADS) {
                let outcome = classify_lead(&raw, &covered, &by_path, &model);
                if let Some(k) = outcome.kept { kept.push(k); }
                if let Some(d) = outcome.dropped { dropped.push(d); }
            }
        }
        Err(e) => {
            let msg: String = e.chars().take(300).collect();
            errors.push(json!({ "error": msg }));
        }
    }

    Ok(json!({ "calls": calls, "cost_usd": cost, "kept": kept, "dropped": dropped, "errors": errors }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS brain;").unwrap();
        c.execute_batch(
            "CREATE TABLE brain.prompt_templates ( id INTEGER PRIMARY KEY, name TEXT NOT NULL, version INTEGER NOT NULL, template_md TEXT NOT NULL, default_tier TEXT NOT NULL, default_temperature TEXT NOT NULL, max_output_tokens INTEGER NOT NULL DEFAULT 1024, superseded_by INTEGER, created_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')), cross_field_rules_json TEXT, judging_criteria_json TEXT, system_md TEXT, phase_flow TEXT NOT NULL DEFAULT 'single', UNIQUE (name, version) );
             CREATE TABLE brain.prompt_output_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, grounded_refs_json TEXT, UNIQUE (prompt_template_id, field_name) );
             CREATE TABLE brain.prompt_input_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, UNIQUE (prompt_template_id, field_name) );",
        ).unwrap();
        c
    }

    #[test]
    fn files_is_required() {
        let c = setup();
        assert!(bugfinderSweep(&json!({}), &c).unwrap_err().contains("files is required"));
        assert!(bugfinderSweep(&json!({ "files": "nope" }), &c).unwrap_err().contains("files is required"));
    }

    #[test]
    fn no_usable_files_short_circuits_before_any_prompt_or_call() {
        let c = setup();
        let r = bugfinderSweep(&json!({ "files": [{ "path": "a.mjs", "source": "   " }] }), &c).unwrap();
        assert_eq!(r, json!({ "calls": 0, "cost_usd": 0, "kept": [], "dropped": [], "errors": [] }));
        let n: i64 = c.query_row("SELECT COUNT(*) FROM brain.prompt_templates", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn reaches_the_parity_deferred_aicall_boundary_and_catches_its_err() {
        // proves the guard/usable-filter/bundle/prompt-ensure all ran cleanly and the Err is
        // aiCall's deferred stub, caught into `errors` rather than propagated (unlike book-run's
        // and pl-research's default exports, which propagate — this file's JS wraps in try/catch).
        let c = setup();
        let r = bugfinderSweep(&json!({ "files": [{ "path": "a.mjs", "source": "const x = 1;" }] }), &c).unwrap();
        assert_eq!(r["calls"], json!(0));
        assert_eq!(r["cost_usd"], json!(0.0));
        assert_eq!(r["kept"], json!([]));
        assert_eq!(r["dropped"], json!([]));
        let errs = r["errors"].as_array().unwrap();
        assert_eq!(errs.len(), 1);
        assert!(errs[0]["error"].as_str().unwrap().contains("ai-call"));
        let n: i64 = c.query_row("SELECT COUNT(*) FROM brain.prompt_templates WHERE name = 'bugfinder-sweep'", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn ensure_sweep_prompt_skips_add_when_existing_matches_and_readds_when_it_diverges() {
        let c = setup();
        ensure_sweep_prompt(&c).unwrap();
        let n1: i64 = c.query_row("SELECT COUNT(*) FROM brain.prompt_templates WHERE name = 'bugfinder-sweep'", [], |r| r.get(0)).unwrap();
        assert_eq!(n1, 1);
        // matches → no re-add (version stays 1, no new row)
        ensure_sweep_prompt(&c).unwrap();
        let n2: i64 = c.query_row("SELECT COUNT(*) FROM brain.prompt_templates WHERE name = 'bugfinder-sweep'", [], |r| r.get(0)).unwrap();
        assert_eq!(n2, 1);
        // diverge the stored template, then ensure again → promptAdd fires a new version
        c.execute("UPDATE brain.prompt_templates SET template_md = 'stale' WHERE name = 'bugfinder-sweep'", []).unwrap();
        ensure_sweep_prompt(&c).unwrap();
        let n3: i64 = c.query_row("SELECT COUNT(*) FROM brain.prompt_templates WHERE name = 'bugfinder-sweep'", [], |r| r.get(0)).unwrap();
        assert_eq!(n3, 2);
    }

    #[test]
    fn extract_findings_text_finds_the_top_level_field_and_ignores_nested() {
        let res = json!({ "output_kv": [
            { "field_name": "findings_json", "parent_output_id": 5, "value_text": "nested-should-be-skipped" },
            { "field_name": "findings_json", "parent_output_id": null, "value_text": "{\"findings\":[]}" },
        ] });
        assert_eq!(extract_findings_text(&res), "{\"findings\":[]}");
        assert_eq!(extract_findings_text(&json!({})), "");
        assert_eq!(extract_findings_text(&json!({ "output_kv": [] })), "");
    }

    #[test]
    fn classify_lead_drops_covered_category_off_scope() {
        let covered: std::collections::HashSet<String> = ["sql-injection".to_string()].into_iter().collect();
        let by_path = vec![("a.mjs".to_string(), json!({ "path": "a.mjs", "source": "const x = 1;" }))];
        let raw = json!({ "file": "a.mjs", "category": "SQL Injection", "detail": "d", "evidence": "const x = 1;", "line": 1 });
        let out = classify_lead(&raw, &covered, &by_path, &Value::Null);
        assert!(out.kept.is_none());
        let d = out.dropped.unwrap();
        assert_eq!(d["drop_reason"], json!("off-scope"));
        assert_eq!(d["category"], json!("other:sql-injection"));
    }

    #[test]
    fn classify_lead_drops_unknown_file_and_keeps_when_grounded() {
        let covered: std::collections::HashSet<String> = std::collections::HashSet::new();
        let by_path = vec![("dir/a.mjs".to_string(), json!({ "path": "dir/a.mjs", "source": "const leak = open();" }))];
        let raw_unknown = json!({ "file": "nope.mjs", "category": "leak", "detail": "d", "evidence": "x", "line": 1 });
        let out_unknown = classify_lead(&raw_unknown, &covered, &by_path, &Value::Null);
        assert_eq!(out_unknown.dropped.unwrap()["drop_reason"], json!("unknown-file"));

        // ends-with fallback: raw file is a suffix of the byPath key
        let raw_kept = json!({ "file": "a.mjs", "category": "leak", "detail": "resource leak", "evidence": "const leak = open();", "line": 1 });
        let out_kept = classify_lead(&raw_kept, &covered, &by_path, &json!("model-x"));
        let k = out_kept.kept.unwrap();
        assert_eq!(k["category"], json!("other:leak"));
        assert_eq!(k["file"], json!("dir/a.mjs"));
        assert_eq!(k["line"], json!(1));
        assert_eq!(k["model"], json!("model-x"));
    }

    #[test]
    fn classify_lead_drops_ungrounded_evidence() {
        let covered: std::collections::HashSet<String> = std::collections::HashSet::new();
        let by_path = vec![("a.mjs".to_string(), json!({ "path": "a.mjs", "source": "const x = 1;" }))];
        let raw = json!({ "file": "a.mjs", "category": "other", "detail": "d", "evidence": "not present anywhere", "line": 1 });
        let out = classify_lead(&raw, &covered, &by_path, &Value::Null);
        assert_eq!(out.dropped.unwrap()["drop_reason"], json!("ungrounded-evidence"));
    }
}
