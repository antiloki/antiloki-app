//! constitution-business-gen.rs — twin of skills/constitution/constitution-business-gen.mjs
//! (db + deferred-AI lane). NAMES ARE CONTRACT: `constitutionBusinessGen` (default). Gathers
//! deterministic codebase context ($0: atlas entry points → surface, atlas metrics'
//! `top_central` → vocab, recent `memory.memories` decisions), then asks the AI to PROPOSE 3-6
//! business laws, persisting them as `laws.review_laws` CANDIDATES (never auto-ratified).
//!
//! PARITY-DEFERRED: the AI call is dispatched through `crate::ai_ai_call::aiCall`, the deferred
//! network-AI stub (see ai-call.rs's module doc) — it always returns `Err`, so this twin always
//! takes the JS `catch` branch and returns the `{proposed:0, candidates:[], ai_call_id:null,
//! cost_usd:0, error}` shape, same as pl-research.rs. The `Ok(res)` branch below (output-kv
//! parsing + the `laws.review_laws` upsert loop) is real, ported code — unreachable from THIS
//! stub but correct by construction, same convention as book-run.rs/pl-research.rs.
//!
//! `context.db` missing → JS throws `err.required(...)`. Not modeled: `db: &Connection` is a
//! required, non-nullable reference here — same "guard-throws are not exercised" convention used
//! by every sibling twin. `context.provider_fn` has no Rust equivalent to carry — threaded as
//! `null`, same as pl-research.rs/book-run.rs.
//!
//! `surface` is a faithful-as-written port of a JS quirk: `atlasEntryPoints`'s real return shape
//! is `{ origin, total, counts, by_path, snapshot }` (see atlas-entry-points.rs) — it has no
//! `routes`/`data`/`ui` keys. The JS `[...(ep.routes ?? []), ...(ep.data ?? []), ...(ep.ui ??
//! [])]` therefore ALWAYS spreads three `undefined ?? []` empty arrays, so `rows` is always `[]`
//! and `rows.join(' · ') || fallback` always takes the `counts ${JSON.stringify(ep.counts)}`
//! fallback — kept verbatim (never "fixed") per NAMES-ARE-CONTRACT, same precedent as
//! constitution-agents.rs's broken `symbols` census query.
// QUALITY: n/a — deterministic context-gathering (fixture-reachable) + always-deferred AI
// dispatch (no fixture/frontier for the aiCall boundary) + a real-but-unreachable-from-the-stub
// persistence loop (ported faithfully, mirrors pl-research.rs's convention).

use rusqlite::Connection;
use serde_json::{json, Map, Value};

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

// = JS `Number(v) || default` (0/NaN both fall back).
fn js_number_or(v: Option<&Value>, default: f64) -> f64 {
    let n = js_number(v);
    if n == 0.0 || n.is_nan() { default } else { n }
}

fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) }

const PROMPT: &str = "constitution-business-gen";
const SYSTEM: &str = "You are a staff engineer proposing BUSINESS LAWS for a specific codebase — domain invariants, security rules and workflow constraints that a reviewer should enforce, drawn from what THIS system is FOR. You are given its external surface (routes/APIs/data), the domain vocabulary of its most central modules, and recorded decisions. Rules: (1) propose 3-6 laws SPECIFIC to this domain — never generic style rules (those are covered elsewhere); (2) each law is a genuine business/security/workflow constraint (\"money is integer cents, never float\"; \"every API route checks authorization\"; \"PII is never logged\"); (3) where the rule is deterministically checkable by a simple text pattern, supply a `regex` (a JS regex string that flags a VIOLATION) so the system can scan for it; if it needs human/semantic judgment, set detector \"none\"; (4) ground each in the given context — do not invent domains the code shows no sign of. Output ONLY valid JSON: {\"laws\":[{\"key\":\"kebab-key\",\"title\":\"...\",\"rule_md\":\"one sentence — the invariant and why\",\"detector\":\"regex|none\",\"pattern\":\"(only if regex)\",\"weight\":1-10,\"severity\":\"info|warn|error|critical\"}]}. No prose outside the JSON.";
const TEMPLATE: &str = "EXTERNAL SURFACE (routes · APIs · data entry points):\n{{surface}}\n\nDOMAIN VOCABULARY (names of the most central modules/symbols):\n{{vocab}}\n\nRECORDED DECISIONS:\n{{decisions}}\n\nPropose the business-law JSON.";

// mirrors JS `ensurePrompt`: promptFind miss/mismatch → promptAdd (no re-fetch after, matching
// the JS source exactly — unlike pl-research.rs's ensure_prompt, which DOES re-fetch).
fn ensure_prompt(db: &Connection) -> Result<(), String> {
    let existing = crate::ai_prompt_find::promptFind(&json!({ "name": PROMPT }), db).ok();
    if let Some(e) = &existing {
        let tpl_ok = e.get("template_md").and_then(Value::as_str) == Some(TEMPLATE);
        let sys_ok = e.get("system_md").and_then(Value::as_str) == Some(SYSTEM);
        if tpl_ok && sys_ok {
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
            "max_output_tokens": 1400,
            "output_fields": [{ "field_name": "laws_json", "field_type": "string" }],
        }),
        db,
    )?;
    Ok(())
}

// `const a = s.indexOf('{'), b = s.lastIndexOf('}'); if (a < 0 || b <= a) return null; try {
// JSON.parse(s.slice(a, b + 1)) } catch { return null }` — byte/char index parity: JS string
// indices are UTF-16 code units, but `{`/`}` are ASCII so a byte-offset slice on the original
// str is equivalent here.
fn extract_json(t: &str) -> Option<Value> {
    let a = t.find('{')?;
    let b = t.rfind('}')?;
    if b <= a { return None; }
    serde_json::from_str(&t[a..=b]).ok()
}

// `String(s ?? '').toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/^-|-$/g, '')`
fn kebab(v: Option<&Value>) -> String {
    let s = match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(),
        Some(o) => o.to_string(),
    };
    let lower = s.to_lowercase();
    let mut out = String::new();
    let mut in_run = false;
    for c in lower.chars() {
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            out.push(c);
            in_run = false;
        } else if !in_run {
            out.push('-');
            in_run = true;
        }
    }
    out.trim_matches('-').to_string()
}

pub fn constitutionBusinessGen(input: &Value, db: &Connection) -> Result<Value, String> {
    let _ = crate::constitution__helpers_schema::ensureConstitutionSchema(db);
    let root = crate::engine_workspace_root::getWorkspaceRoot(db);
    ensure_prompt(db)?;

    // ── gather the context (deterministic, $0) ────────────────────────────
    let surface: String = match crate::codebase_atlas_entry_points::atlasEntryPoints(&json!({ "origin": "atlas", "root": root }), db) {
        Ok(ep) => {
            // `rows` (routes+data+ui) is ALWAYS empty — see module doc.
            let counts = ep.get("counts").cloned().unwrap_or(json!({}));
            format!("counts {}", serde_json::to_string(&counts).unwrap_or_else(|_| "{}".into()))
        }
        Err(_) => "(no entry points extracted)".to_string(),
    };

    let vocab: String = match crate::codebase_atlas_metrics::atlasMetrics(&json!({ "origin": "atlas", "top": 20 }), db) {
        Ok(m) => {
            let names: Vec<String> = m
                .get("top_central")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
                .iter()
                .filter_map(|c| c.get("path").and_then(Value::as_str))
                .map(|p| {
                    // `.split('/').pop().replace(/\.\w+$/, '')` — strips only a trailing
                    // `.` + word-chars run (the LAST dot, since \w never matches '.' so an
                    // earlier dot's \w+ attempt would hit a later '.' and fail before $).
                    let base = p.rsplit('/').next().unwrap_or(p);
                    match base.rfind('.') {
                        Some(i) if i + 1 < base.len() && base[i + 1..].chars().all(|c| c.is_ascii_alphanumeric() || c == '_') => base[..i].to_string(),
                        _ => base.to_string(),
                    }
                })
                .collect();
            let joined = names.join(" · ");
            if joined.is_empty() { "(no atlas)".to_string() } else { joined }
        }
        Err(_) => "(no atlas)".to_string(),
    };

    let decisions: String = match crate::db_json::query_rows(
        db,
        "SELECT title FROM memory.memories WHERE type IN ('decision','reference') AND superseded_by IS NULL ORDER BY id DESC LIMIT 12",
        &[],
    ) {
        Ok(rows) => {
            let titles: Vec<String> = rows
                .as_array()
                .cloned()
                .unwrap_or_default()
                .iter()
                .map(|r| r.get("title").and_then(Value::as_str).unwrap_or("").to_string())
                .collect();
            let joined = titles.join("\n");
            if joined.is_empty() { "(none)".to_string() } else { joined }
        }
        Err(_) => "(none)".to_string(),
    };

    // ── the AI proposal (one call, envelope persisted for the AI-call window) ──
    let ai_input = json!({
        "prompt_name": PROMPT,
        "input_kv": [
            { "key": "surface", "value": surface },
            { "key": "vocab", "value": vocab },
            { "key": "decisions", "value": decisions },
        ],
        "provider_fn": Value::Null,
    });
    let res = match crate::ai_ai_call::aiCall(&ai_input, db) {
        Ok(r) => r,
        Err(e) => {
            return Ok(json!({
                "proposed": 0,
                "candidates": [],
                "ai_call_id": Value::Null,
                "cost_usd": 0.0,
                "error": e.chars().take(200).collect::<String>(),
            }));
        }
    };

    let ai_call_id = res.get("ai_call_id").cloned().unwrap_or(Value::Null);
    let cost = js_number(res.get("cost_usd")).max(0.0); // `Number(res?.cost_usd ?? 0)` — NaN would propagate in JS too, but every real ai_calls row carries a numeric cost
    let cost = if cost.is_nan() { 0.0 } else { cost };
    let value_text = res
        .get("output_kv")
        .and_then(Value::as_array)
        .and_then(|arr| arr.iter().find(|o| {
            o.get("field_name").and_then(Value::as_str) == Some("laws_json")
                && nz(o.get("parent_output_id")).is_none()
        }))
        .and_then(|o| o.get("value_text"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let body = extract_json(value_text);
    let laws: Vec<Value> = body
        .as_ref()
        .and_then(|b| b.get("laws"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    // ── persist as CANDIDATES (operator ratifies) ─────────────────────────────
    let max_n = js_number_or(input.get("max"), 6.0).max(1.0).min(10.0) as usize;
    let mut candidates: Vec<Value> = Vec::new();
    for l in laws.iter().take(max_n) {
        let key_src = if nz(l.get("key")).is_some() { l.get("key") } else { l.get("title") };
        let key = kebab(key_src);
        if key.is_empty() { continue; }
        let is_regex = l.get("detector").and_then(Value::as_str) == Some("regex") && nz(l.get("pattern")).is_some();
        let detector = if is_regex { "regex" } else { "none" };
        let pattern_str = if is_regex {
            match l.get("pattern") {
                Some(Value::String(s)) => s.clone(),
                Some(v) => v.to_string(),
                None => String::new(),
            }
        } else {
            String::new()
        };
        let config = if is_regex { json!({ "pattern": pattern_str }) } else { json!({}) };
        if is_regex && regex::Regex::new(&pattern_str).is_err() { continue; } // a broken regex is dropped, not stored
        let enforcement = if detector == "none" { "advise" } else { "score" };
        let title = match l.get("title") { Some(Value::String(s)) => s.clone(), Some(v) if !v.is_null() => v.to_string(), _ => key.clone() };
        let rule_md = match l.get("rule_md") { Some(Value::String(s)) => s.clone(), Some(v) if !v.is_null() => v.to_string(), _ => String::new() };
        let weight = js_number_or(l.get("weight"), 6.0).max(1.0).min(10.0) as i64;
        let severity = match l.get("severity") { Some(Value::String(s)) => s.clone(), Some(v) if !v.is_null() => v.to_string(), _ => "error".to_string() };
        let applies_json = serde_json::to_string(&json!({ "model": "ai", "ai_call_id": ai_call_id })).unwrap_or_else(|_| "{}".into());

        crate::db_json::run_write(
            db,
            &format!("INSERT INTO laws.review_laws (scope, key, title, rule_md, detector, config_json, weight, severity, confidence, enforcement, applies_json, status, evidence_json, created_by) \
             VALUES ('business', ?, ?, ?, ?, ?, ?, ?, ?, ?, '{{}}', 'candidate', ?, 'ai') \
             ON CONFLICT(key) DO UPDATE SET rule_md=excluded.rule_md, updated_at={} WHERE laws.review_laws.status='candidate'", crate::clock::now_sql()),
            &[
                json!(key),
                json!(title),
                json!(rule_md),
                json!(detector),
                json!(serde_json::to_string(&config).unwrap_or_else(|_| "{}".into())),
                json!(weight),
                json!(severity),
                json!(0.7),
                json!(enforcement),
                json!(applies_json),
            ],
        )?;
        candidates.push(json!({
            "key": key,
            "title": l.get("title").cloned().unwrap_or(Value::Null),
            "detector": detector,
            "enforcement": enforcement,
            "weight": l.get("weight").cloned().unwrap_or(Value::Null),
            "severity": l.get("severity").cloned().unwrap_or(Value::Null),
        }));
    }

    let cost_rounded = (cost * 10000.0).round() / 10000.0;
    let mut out = Map::new();
    out.insert("proposed".into(), json!(candidates.len() as i64));
    out.insert("candidates".into(), json!(candidates));
    out.insert("ai_call_id".into(), ai_call_id);
    out.insert("cost_usd".into(), json!(cost_rounded));
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_json_finds_first_brace_pair() {
        assert_eq!(extract_json("prefix {\"a\":1} suffix"), Some(json!({"a":1})));
        assert_eq!(extract_json("no braces here"), None);
        assert_eq!(extract_json("{unclosed"), None);
        assert_eq!(extract_json("} {"), None); // b <= a
    }

    #[test]
    fn kebab_lowercases_and_collapses_non_alnum_runs() {
        assert_eq!(kebab(Some(&json!("Money Is Integer Cents"))), "money-is-integer-cents");
        assert_eq!(kebab(Some(&json!("  --Weird__Key!!--  "))), "weird-key");
        assert_eq!(kebab(Some(&Value::Null)), "");
        assert_eq!(kebab(None), "");
    }

    #[test]
    fn js_number_or_falls_back_on_zero_and_nan() {
        assert_eq!(js_number_or(Some(&json!(0)), 6.0), 6.0);
        assert_eq!(js_number_or(Some(&json!("not a number")), 6.0), 6.0);
        assert_eq!(js_number_or(None, 6.0), 6.0);
        assert_eq!(js_number_or(Some(&json!(3)), 6.0), 3.0);
    }

    #[test]
    fn constitution_business_gen_returns_error_shape_when_ai_call_is_deferred() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "ATTACH ':memory:' AS brain; ATTACH ':memory:' AS memory; ATTACH ':memory:' AS laws; ATTACH ':memory:' AS code;",
        )
        .unwrap();
        let _ = crate::constitution__helpers_schema::ensureConstitutionSchema(&c);
        c.execute_batch(
            "CREATE TABLE IF NOT EXISTS brain.prompt_templates ( id INTEGER PRIMARY KEY, name TEXT NOT NULL, version INTEGER NOT NULL, template_md TEXT NOT NULL, default_tier TEXT NOT NULL, default_temperature TEXT NOT NULL, max_output_tokens INTEGER NOT NULL DEFAULT 1024, superseded_by INTEGER, created_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')), cross_field_rules_json TEXT, judging_criteria_json TEXT, system_md TEXT, phase_flow TEXT NOT NULL DEFAULT 'single', UNIQUE (name, version) ); \
             CREATE TABLE IF NOT EXISTS brain.prompt_output_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, grounded_refs_json TEXT, UNIQUE (prompt_template_id, field_name) ); \
             CREATE TABLE IF NOT EXISTS brain.prompt_input_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, UNIQUE (prompt_template_id, field_name) ); \
             CREATE TABLE IF NOT EXISTS memory.memories ( id INTEGER PRIMARY KEY, type TEXT, title TEXT, superseded_by INTEGER ); \
             CREATE TABLE IF NOT EXISTS code_nodes ( id TEXT PRIMARY KEY, origin TEXT, type TEXT, path TEXT, span_json TEXT );",
        )
        .unwrap();

        let r = constitutionBusinessGen(&json!({}), &c).unwrap();
        assert_eq!(r["proposed"], json!(0));
        assert_eq!(r["candidates"], json!([]));
        assert_eq!(r["ai_call_id"], Value::Null);
        assert_eq!(r["cost_usd"], json!(0.0));
        assert!(r["error"].as_str().unwrap().contains("ai-call"));

        // ensurePrompt persisted the template even though the AI call itself was deferred.
        let found = crate::ai_prompt_find::promptFind(&json!({ "name": PROMPT }), &c).unwrap();
        assert_eq!(found["template_md"], json!(TEMPLATE));
        assert_eq!(found["system_md"], json!(SYSTEM));
    }
}
