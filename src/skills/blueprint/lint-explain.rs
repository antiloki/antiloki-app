//! lint-explain.rs — twin of skills/blueprint/lint-explain.mjs. NAMES ARE CONTRACT:
//! `lintExplain` (default). Deterministic when GREEN, EXPLAINED when RED: the structural lint
//! itself is pure; ONLY on a RED does this fire ONE low-tier AI call that reads the deterministic
//! findings + the composed source of each failing file and returns an imperative fix.
//!
//! PARITY-DEFERRED: the `aiCall` leg (`crate::ai_ai_call::aiCall`) is the deferred network
//! boundary — see ai-call.rs's own doc. In the real JS, a `try { await aiCall(...) } catch {
//! return { explanation_md: '', ai_call_id: null } }` wraps the call, so ANY aiCall failure
//! (including this twin's permanent stub error) degrades identically to the documented
//! "additive — a failure degrades to no explanation" contract. This twin therefore always takes
//! that fail-open branch once past the deterministic guards/composition, which is EXACT for the
//! stub (not an approximation) — the only thing not exercised here is the success-path response
//! shaping, ported below as a pure, independently-tested helper (`clean_explanation_md`) fed
//! canned reply text mirroring the real JS test fixtures (lint-explain.test.mjs).
//!
//! Deterministic surface ported faithfully: contract_slug guard, info-severity filter,
//! short-circuit on no findings, blueprint row resolution (by id or latest-active by
//! contract_slug), blueprint_json parse + normalizeBlueprint, blueprint_snippets → authored map,
//! distinct failing file paths, per-file blueprintCompose + 3000-char slice, 12000-char join
//! slice with fallback text, and ensurePrompt's promptFind/promptAdd bookkeeping (registers the
//! `blueprint.lint.explain.v1` template idempotently — reused verbatim from prompt-find.rs/
//! prompt-add.rs twins).
use rusqlite::Connection;
use serde_json::{json, Map, Value};

const PROMPT: &str = "blueprint.lint.explain.v1";
const SYSTEM: &str = "You explain deterministic lint failures on generated code to the agent that will re-write it. Be concrete and short: name the file, the line, the symbol, and the exact change. No preamble.";
const TEMPLATE: &str = "A DETERMINISTIC structural lint (no AI, cannot be wrong) marked this authored code RED. State the EXACT fix the re-author must make. This is an INSTRUCTION to the re-author, not an analysis for a reader.

LINT ERRORS:
{{errors}}

THE COMPOSED FILES THAT FAILED (this is what the lint parsed):
{{sources}}

Output AT MOST 4 imperative bullet lines — each a concrete action (\"Remove the second declaration of X\", \"Emit the COMPLETE file including the existing imports, not a fragment\", \"Close the import: change line N to ...\"). Rules:
- Do NOT reason step by step, do NOT count lines out loud, do NOT say \"wait\" or \"however\" or \"let me check\" — you are WRITING THE FIX, not deliberating about it.
- If a symbol is declared twice: \"keep one declaration of <name>, delete the other.\"
- If a file does not parse: name the single change that makes it parse.
Output ONLY the imperative bullets. No preamble, no restatement, no reasoning.";

fn truthy(v: Option<&Value>) -> bool { match v { None|Some(Value::Null)=>false, Some(Value::Bool(b))=>*b, Some(Value::Number(n))=>n.as_f64().map_or(false,|f|f!=0.0&&!f.is_nan()), Some(Value::String(s))=>!s.is_empty(), Some(_)=>true } }
fn str_qq(v: Option<&Value>) -> String { match v { None|Some(Value::Null)=>String::new(), Some(Value::String(s))=>s.clone(), Some(Value::Bool(b))=>b.to_string(), Some(n@Value::Number(_))=>n.to_string(), Some(o)=>o.to_string() } }

// `function ensurePrompt(db)` — idempotent template registration; matches JS: look up by name,
// skip re-add if the active row already has the identical template_md, else promptAdd (which
// itself supersedes any existing active row).
fn ensure_prompt(db: &Connection) -> Result<(), String> {
    let existing = crate::ai_prompt_find::promptFind(&json!({ "name": PROMPT }), db).ok();
    if let Some(e) = &existing {
        if e.get("template_md").and_then(Value::as_str) == Some(TEMPLATE) { return Ok(()); }
    }
    crate::ai_prompt_add::promptAdd(&json!({
        "name": PROMPT, "template_md": TEMPLATE, "system_md": SYSTEM,
        "default_tier": "low", "default_temperature": "cold", "phase_flow": "single_only",
        "max_output_tokens": 700,
        "output_fields": [{ "field_name": "explanation_md", "field_type": "string" }],
    }), db)?;
    Ok(())
}

// `const clean = raw.split('\n').map(trim).filter(...).filter(...).slice(0,6).join('\n').slice(0,1200)`
// — strips reasoning-model deliberation tells, keeps only imperative fix lines. Pure; fed the
// resolved AI reply text (the success path this deferred twin never reaches at runtime, but
// which lint-explain.test.mjs pins byte-exactly — tested here with the same canned fixtures).
pub fn clean_explanation_md(raw: &str) -> String {
    static STRIP_RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    static KEEP_RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let strip_re = STRIP_RE.get_or_init(|| regex::Regex::new(r"(?i)^(we are given|looking at|let'?s count|wait\b|however,? note|alternatively|this is confusing|but the error|line \d+:)").unwrap());
    let keep_re = KEEP_RE.get_or_init(|| regex::Regex::new(r"(?i)^[-*•]|\b(remove|delete|keep|emit|close|add|change|merge|replace|rename|declare|move|import|export)\b").unwrap());
    let lines: Vec<String> = raw
        .split('\n')
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty() && !strip_re.is_match(l))
        .filter(|l| keep_re.is_match(l))
        .take(6)
        .collect();
    let joined = lines.join("\n");
    joined.chars().take(1200).collect()
}

pub fn lintExplain(input: &Value, db: &Connection) -> Result<Value, String> {
    let contract_slug = input.get("contract_slug").filter(|v| truthy(Some(v))).cloned();
    let contract_slug = contract_slug.ok_or_else(|| "lint-explain: contract_slug is required".to_string())?;

    let all_findings = input.get("findings").and_then(Value::as_array).cloned().unwrap_or_default();
    let findings: Vec<Value> = all_findings
        .into_iter()
        .filter(|f| f.get("severity").and_then(Value::as_str) != Some("info"))
        .collect();
    if findings.is_empty() {
        return Ok(json!({ "explanation_md": "", "ai_call_id": Value::Null }));
    }

    let blueprint_id = input.get("blueprint_id").filter(|v| truthy(Some(v)));
    let rows = if let Some(bid) = blueprint_id {
        crate::db_json::query_rows(db, "SELECT id, blueprint_json FROM blueprints WHERE id = ?", &[bid.clone()])?
    } else {
        crate::db_json::query_rows(
            db,
            "SELECT id, blueprint_json FROM blueprints WHERE contract_slug = ? AND superseded_by IS NULL ORDER BY id DESC LIMIT 1",
            &[contract_slug.clone()],
        )?
    };
    let row = match rows.as_array().and_then(|a| a.first()) {
        Some(r) => r.clone(),
        None => return Ok(json!({ "explanation_md": "", "ai_call_id": Value::Null })),
    };
    let row_id = row.get("id").cloned().unwrap_or(Value::Null);

    let raw_bp: Value = row
        .get("blueprint_json")
        .and_then(Value::as_str)
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or_else(|| json!({}));
    let bp = crate::blueprint_block_shape::normalizeBlueprint(&raw_bp);

    let snips = crate::db_json::query_rows(
        db,
        "SELECT block_id, code, imports_json FROM blueprint_snippets WHERE blueprint_id = ?",
        &[row_id],
    )?;
    let mut authored = Map::new();
    if let Some(arr) = snips.as_array() {
        for s in arr {
            let block_id = str_qq(s.get("block_id"));
            let imports: Value = s
                .get("imports_json")
                .and_then(Value::as_str)
                .and_then(|t| serde_json::from_str(t).ok())
                .unwrap_or_else(|| json!([]));
            authored.insert(block_id, json!({ "code": s.get("code").cloned().unwrap_or(Value::Null), "imports": imports }));
        }
    }
    let authored = Value::Object(authored);

    let mut failed_paths: Vec<String> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for f in &findings {
        let p = f.get("file").and_then(Value::as_str).unwrap_or("");
        if !p.is_empty() && p != "(blueprint)" && seen.insert(p.to_string()) {
            failed_paths.push(p.to_string());
        }
    }

    let empty_blocks = vec![];
    let blocks = bp.get("blocks").and_then(Value::as_array).unwrap_or(&empty_blocks);
    let mut source_parts: Vec<String> = Vec::new();
    for p in &failed_paths {
        let fb = blocks.iter().find(|b| {
            b.get("type").and_then(Value::as_str) == Some("file")
                && (b.get("path").and_then(Value::as_str) == Some(p.as_str()) || b.get("id").and_then(Value::as_str) == Some(p.as_str()))
        });
        match fb {
            None => source_parts.push(format!("--- {p} --- (file block not found)")),
            Some(fb) => {
                let composed = crate::blueprint_blueprint_compose::blueprintCompose(&json!({ "block": fb, "authored": authored }), db)?;
                let src = composed.get("source").and_then(Value::as_str).unwrap_or("");
                let sliced: String = src.chars().take(3000).collect();
                source_parts.push(format!("--- {p} ---\n{sliced}"));
            }
        }
    }
    let joined = source_parts.join("\n\n");
    let sliced_sources: String = joined.chars().take(12000).collect();
    let sources = if sliced_sources.is_empty() { "(sources unavailable)".to_string() } else { sliced_sources };

    let errors_str: String = {
        let joined = findings
            .iter()
            .map(|f| format!("- {}: {}", f.get("file").and_then(Value::as_str).unwrap_or("?"), f.get("detail").and_then(Value::as_str).or_else(|| f.get("message").and_then(Value::as_str)).unwrap_or("")))
            .collect::<Vec<_>>()
            .join("\n");
        joined.chars().take(3000).collect()
    };

    ensure_prompt(db)?;

    // PARITY-DEFERRED: the JS wraps `await aiCall(...)` in try/catch, returning the fail-open
    // result on ANY failure (network error, provider exhaustion, validation failure). This
    // stub always errs, so the twin always takes that same fail-open branch here — exact for
    // the stub, matching the documented "additive" contract.
    let ai_input = json!({
        "prompt_name": PROMPT,
        "input_kv": [{ "key": "errors", "value": errors_str }, { "key": "sources", "value": sources }],
        "contract_slug": contract_slug, "tier": "low", "temperature": "cold",
        "phase_flow": "single_only", "structured_output": false,
    });
    match crate::ai_ai_call::aiCall(&ai_input, db) {
        Ok(res) => {
            let raw = res.get("explanation_md").and_then(Value::as_str).unwrap_or("");
            let clean = clean_explanation_md(raw);
            let ai_call_id = res.get("ai_call_id").cloned().unwrap_or(Value::Null);
            Ok(json!({ "explanation_md": clean, "ai_call_id": ai_call_id }))
        }
        Err(_) => Ok(json!({ "explanation_md": "", "ai_call_id": Value::Null })),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS brain").unwrap();
        c.execute_batch(
            "CREATE TABLE blueprints (id INTEGER PRIMARY KEY, contract_slug TEXT NOT NULL, version INTEGER NOT NULL, blueprint_json TEXT NOT NULL, lint_passed INTEGER, superseded_by INTEGER);
             CREATE TABLE blueprint_snippets (id INTEGER PRIMARY KEY, blueprint_id INTEGER NOT NULL, block_id TEXT NOT NULL, code TEXT, imports_json TEXT, authored_by TEXT);
             CREATE TABLE brain.prompt_templates ( id INTEGER PRIMARY KEY, name TEXT NOT NULL, version INTEGER NOT NULL, template_md TEXT NOT NULL, default_tier TEXT NOT NULL, default_temperature TEXT NOT NULL, max_output_tokens INTEGER NOT NULL DEFAULT 1024, superseded_by INTEGER, created_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')), cross_field_rules_json TEXT, judging_criteria_json TEXT, system_md TEXT, phase_flow TEXT NOT NULL DEFAULT 'single', UNIQUE (name, version) );
             CREATE TABLE brain.prompt_output_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, grounded_refs_json TEXT, UNIQUE (prompt_template_id, field_name) );
             CREATE TABLE brain.prompt_input_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, UNIQUE (prompt_template_id, field_name) );",
        ).unwrap();
        c
    }

    fn seed_blueprint(db: &Connection, slug: &str, code: &str) -> i64 {
        let bp = json!({
            "blocks": [{ "id": "src/q.js", "type": "file", "path": "src/q.js", "children": [{ "id": "src/q.js.f", "type": "function" }] }],
            "bindings": [], "planned_nodes": []
        });
        db.execute(
            "INSERT INTO blueprints (contract_slug, version, blueprint_json, lint_passed) VALUES (?,1,?,1)",
            rusqlite::params![format!("CARD-{slug}-C1"), serde_json::to_string(&bp).unwrap()],
        ).unwrap();
        let bid = db.last_insert_rowid();
        db.execute(
            "INSERT INTO blueprint_snippets (blueprint_id, block_id, code, authored_by) VALUES (?,?,?,'dev')",
            rusqlite::params![bid, "src/q.js.f", code],
        ).unwrap();
        bid
    }

    fn red_finding() -> Value {
        json!({ "file": "src/q.js", "detail": "duplicate declaration of f", "kind": "duplicate" })
    }

    #[test]
    fn missing_contract_slug_errors() {
        let c = db();
        let e = lintExplain(&json!({ "findings": [red_finding()] }), &c).unwrap_err();
        assert!(e.contains("contract_slug is required"));
    }

    #[test]
    fn no_findings_short_circuits() {
        let c = db();
        seed_blueprint(&c, "NOFIND", "export function f(x){ return x; }");
        let r = lintExplain(&json!({ "contract_slug": "CARD-NOFIND-C1", "findings": [] }), &c).unwrap();
        assert_eq!(r, json!({ "explanation_md": "", "ai_call_id": Value::Null }));
    }

    #[test]
    fn info_only_findings_are_filtered_to_empty() {
        let c = db();
        seed_blueprint(&c, "INFO", "export function f(x){ return x; }");
        let r = lintExplain(&json!({ "contract_slug": "CARD-INFO-C1", "findings": [{ "file": "src/q.js", "detail": "advisory only", "severity": "info" }] }), &c).unwrap();
        assert_eq!(r, json!({ "explanation_md": "", "ai_call_id": Value::Null }));
    }

    #[test]
    fn no_active_blueprint_short_circuits() {
        let c = db();
        c.execute("INSERT INTO blueprints (contract_slug, version, blueprint_json, lint_passed) VALUES ('CARD-NOBP-C1',1,'{}',1)", []).ok();
        // superseded row: still resolves since superseded_by is NULL by default (row exists),
        // so instead exercise the true "no row at all" branch on a slug with zero rows.
        let r = lintExplain(&json!({ "contract_slug": "CARD-TRULY-NONE-C1", "findings": [red_finding()] }), &c).unwrap();
        assert_eq!(r, json!({ "explanation_md": "", "ai_call_id": Value::Null }));
    }

    #[test]
    fn ai_failure_degrades_to_empty_and_ensure_prompt_still_registers() {
        let c = db();
        seed_blueprint(&c, "AIFAIL", "export function f(x){ return x; }\nexport function f(y){ return y; }");
        let r = lintExplain(&json!({ "contract_slug": "CARD-AIFAIL-C1", "findings": [red_finding()] }), &c).unwrap();
        assert_eq!(r, json!({ "explanation_md": "", "ai_call_id": Value::Null }));
        let registered: Option<i64> = c
            .query_row("SELECT id FROM brain.prompt_templates WHERE name='blueprint.lint.explain.v1'", [], |row| row.get(0))
            .ok();
        assert!(registered.is_some(), "ensurePrompt registered the explain template even on AI failure");
    }

    // ── clean_explanation_md (pure, canned-response fed) ────────────────
    #[test]
    fn clean_strips_reasoning_tells_and_keeps_imperative_fixes() {
        let rambly = [
            "We are given a file that does not parse.",
            "Wait, I need to count the lines first.",
            "The function is defined more than once in the composed output.",
            "Alternatively we could rename one of them.",
            "- Keep one declaration of f and delete the duplicate.",
            "Emit the COMPLETE file including the existing imports.",
        ].join("\n");
        let cleaned = clean_explanation_md(&rambly);
        assert_eq!(cleaned, "- Keep one declaration of f and delete the duplicate.\nEmit the COMPLETE file including the existing imports.");
    }

    #[test]
    fn clean_passes_through_already_tight_reply_unchanged() {
        let clean_reply = [
            "- Remove the second declaration of f, keeping only the first.",
            "- Close the import on line 1: change it to a complete statement.",
        ].join("\n");
        assert_eq!(clean_explanation_md(&clean_reply), clean_reply);
    }
}
