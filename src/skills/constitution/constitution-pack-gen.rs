//! constitution-pack-gen.rs — twin of skills/constitution/constitution-pack-gen.mjs (db lane,
//! cross-db `laws`/`brain`). NAMES ARE CONTRACT: default `constitutionPackGen`. AI authors an
//! ENGINEERING PACK (4-8 deterministic, scannable laws) for a named technology; the pack lands
//! as `candidate` rows in `laws.review_laws` (never auto-ratified).
//!
//! `context.db` missing → JS throws `err.required('constitution-pack-gen', 'context.db')`.
//! Unreachable here: `db: &Connection` is a required, non-nullable reference (same convention as
//! every sibling db-lane twin). `context.provider_fn` (a JS function reference for provider
//! selection/DI) has no Rust equivalent to carry — threaded as `null` into the constructed
//! `aiCall` input, same as book-run.rs's/pl-research.rs's `provider_fn`.
//!
//! PARITY-DEFERRED: the AI call is dispatched through `crate::ai_ai_call::aiCall`, the deferred
//! network-AI stub (see ai-call.rs's module doc) — it ALWAYS returns `Err`. Unlike book-run.rs/
//! pl-research.rs (which propagate that error), THIS module's JS source wraps the `aiCall` await
//! in its OWN `try { ... } catch (e) { return {...} }` — so `constitutionPackGen` does NOT
//! propagate the error; it CATCHES it and returns a normal `Ok` envelope
//! `{ tech, proposed: 0, candidates: [], ai_call_id: null, cost_usd: 0, error: <msg>.slice(0,200) }`
//! (both `ai_call_id`/`cost` stay at their pre-try-block initial values — `null`/`0` — because the
//! JS assignment `ai_call_id = res?.ai_call_id ?? null` never runs when `await aiCall(...)` itself
//! throws before returning `res`). This IS this unit's real, reachable, fixture-tested output —
//! not a deferred boundary error like the sibling LLM-partials.
//!
//! The candidate-persistence code AFTER the try/catch (`prefix`, the per-law filter/insert loop,
//! `candidates.push`) is consequently UNREACHABLE from `constitutionPackGen` itself in this stub
//! world (the function always returns from the `catch` branch first) — ported faithfully as the
//! pure-ish `persist_candidates` helper (real `laws.review_laws` INSERT via `crate::db_json::
//! run_write`) and unit-tested directly with a canned `laws` array, mirroring pl-research.rs's
//! `build_research_report`/`build_sources` convention (correct by construction, independently
//! testable, unreachable from the default export only because of the deferred `aiCall` stub).
// QUALITY: mixed unit — deterministic guard/schema/prompt-ensure surface (fully ported,
// fixture-reachable) + the try/catch AI-call shell (real, reachable, tested: aiCall always errors
// so the catch-shaped Ok envelope IS this unit's actual behavior) + 1 pure persist_candidates
// helper (unreachable from the default export, unit-tested directly, no frontier/fixture).
use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::collections::HashSet;

fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}
// JS `${v}` / String(v) coercion (used for the template-literal key build + prose fields).
fn js_string(v: Option<&Value>) -> String {
    match v {
        None => "undefined".into(),
        Some(Value::Null) => "null".into(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(),
        Some(Value::Array(a)) => a.iter().map(|x| js_string(Some(x))).collect::<Vec<_>>().join(","),
        Some(Value::Object(_)) => "[object Object]".into(),
    }
}
// JS `nullish ?? ''` then `String()`: only null/undefined substitute; every other falsy value
// (0, false, "") still gets String()-coerced as itself.
fn js_string_or_empty(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        other => js_string(other),
    }
}
// JS `a ?? b` (nullish coalescing — only null/undefined fall through to b).
fn nz_or<'a>(a: Option<&'a Value>, b: &'a Value) -> &'a Value {
    match a {
        Some(v) if !v.is_null() => v,
        _ => b,
    }
}

const PROMPT: &str = "constitution-pack-gen";
const SYSTEM: &str = "You author an ENGINEERING PACK — a set of deterministic code-quality laws for ONE specific technology (a language, framework, or library). Given the tech (and optionally its docs), emit 4-8 laws that catch REAL, common mistakes for THAT tech, each with a scannable detector:\n- regex: a JS regex over the source; a match is the violation. config {pattern, flags?}\n- absent: a JS regex that MUST be present; missing it is the violation. config {pattern}\n- import: like regex but only import/require lines (forbidden/required deps). config {pattern}\n- metric: a threshold over an atlas metric in {loc, complexity, fan_in, fan_out}; over max is the violation. config {metric, max}\n- ast: a tree-sitter S-expression query per grammar for structural rules. config {queries:{tsx?:\"...\", python?:\"...\"}}\nRules: (1) STRONGLY prefer regex/metric; use ast only for a real structural rule; (2) each regex must be a valid JS regex flagging the violation; (3) ground every law in that tech's DOCUMENTED best practices — no generic style rules; (4) scope by language. Output ONLY JSON: {\"laws\":[{\"key\":\"kebab-key\",\"title\":\"short\",\"rule_md\":\"the rule and why\",\"detector\":\"regex|absent|import|metric|ast\",\"config\":{...},\"weight\":1-10,\"severity\":\"info|warn|error|critical\",\"languages\":[\"ts\",\"js\",\"py\"]}]}. No prose outside the JSON.";
const TEMPLATE: &str = "Author an engineering pack of deterministic laws for this technology:\n\nTECH: {{tech}}\n\nDOCS (optional grounding; may be empty — use your own knowledge if so):\n{{docs}}\n\nOutput the pack JSON.";

fn ensure_prompt(db: &Connection) -> Result<(), String> {
    let existing = crate::ai_prompt_find::promptFind(&json!({ "name": PROMPT }), db).ok();
    if let Some(e) = &existing {
        if e.get("template_md").and_then(Value::as_str) == Some(TEMPLATE)
            && e.get("system_md").and_then(Value::as_str) == Some(SYSTEM)
        {
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
            "max_output_tokens": 1800,
            "output_fields": [{ "field_name": "laws_json", "field_type": "string" }],
        }),
        db,
    )?;
    Ok(())
}

/// `function extractJson(t) { ... }` — first `{` to last `}` slice, best-effort JSON.parse.
fn extract_json(t: &str) -> Option<Value> {
    let a = t.find('{')?;
    let b = t.rfind('}')?;
    if b <= a {
        return None;
    }
    serde_json::from_str(&t[a..=b]).ok()
}

/// `const kebab = (s) => String(s ?? '').toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/^-|-$/g, '');`
fn kebab(s: &str) -> String {
    let lower = s.to_lowercase();
    let mut collapsed = String::new();
    let mut in_run = false;
    for c in lower.chars() {
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            collapsed.push(c);
            in_run = false;
        } else if !in_run {
            collapsed.push('-');
            in_run = true;
        }
    }
    collapsed.trim_matches('-').to_string()
}

fn regexy() -> &'static HashSet<&'static str> {
    static S: std::sync::OnceLock<HashSet<&'static str>> = std::sync::OnceLock::new();
    S.get_or_init(|| ["regex", "absent", "import"].into_iter().collect())
}
fn detectors() -> &'static HashSet<&'static str> {
    static S: std::sync::OnceLock<HashSet<&'static str>> = std::sync::OnceLock::new();
    S.get_or_init(|| ["regex", "absent", "import", "metric", "ast"].into_iter().collect())
}

/// `Math.max(1, Math.min(12, Number(input?.max) || 8))` — `Number(v) || 8`: NaN or 0 (or any
/// falsy numeric result) falls back to 8.
fn resolve_max(input_max: Option<&Value>) -> i64 {
    let n: f64 = match input_max {
        Some(Value::Number(n)) => n.as_f64().unwrap_or(f64::NAN),
        Some(Value::String(s)) => s.trim().parse::<f64>().unwrap_or(f64::NAN),
        Some(Value::Bool(true)) => 1.0,
        _ => f64::NAN,
    };
    let n = if !n.is_finite() || n == 0.0 { 8.0 } else { n };
    (n.min(12.0) as i64).max(1)
}

/// Post-try/catch persistence: `prefix` + per-law filter/insert loop. UNREACHABLE from
/// `constitutionPackGen` in this stub world (see module doc) — real code, unit-tested directly.
fn persist_candidates(db: &Connection, tech: &str, laws: &[Value], max: i64, ai_call_id: &Value) -> Result<Vec<Value>, String> {
    let prefix: String = kebab(tech).chars().take(16).collect();
    let mut candidates: Vec<Value> = Vec::new();
    for l in laws.iter().take(max.max(0) as usize) {
        let detector = l.get("detector").and_then(Value::as_str).unwrap_or("");
        if !detectors().contains(detector) {
            continue;
        }
        if regexy().contains(detector) {
            let pattern = l.get("config").and_then(|c| c.get("pattern"));
            if !truthy(pattern) {
                continue;
            }
            if regex::Regex::new(&js_string(pattern)).is_err() {
                continue;
            }
        }
        if detector == "metric" {
            let metric = l.get("config").and_then(|c| c.get("metric")).and_then(Value::as_str).unwrap_or("");
            if !["loc", "complexity", "fan_in", "fan_out"].contains(&metric) {
                continue;
            }
        }
        // `l.key || l.title` — plain OR (falsy, not just nullish): "", 0, false, null, undefined
        // all fall through to l.title.
        let key_src = if truthy(l.get("key")) { js_string(l.get("key")) } else { js_string(l.get("title")) };
        let key = kebab(&format!("{prefix}-{key_src}"));
        if key.is_empty() {
            continue;
        }
        let langs: Vec<Value> = l
            .get("languages")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter(|x| matches!(x.as_str(), Some("ts") | Some("js") | Some("py")))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        let title = l.get("title").cloned().unwrap_or(Value::Null);
        // `String(l.title ?? key)` — nullish-only fallback to the already-computed `key`.
        let key_val = json!(key.clone());
        let title_str = js_string(Some(nz_or(l.get("title"), &key_val)));
        // `String(l.rule_md ?? '')`
        let rule_md = js_string_or_empty(l.get("rule_md"));
        // `JSON.stringify(l.config ?? {})`
        let empty_obj = json!({});
        let config_json = serde_json::to_string(nz_or(l.get("config"), &empty_obj)).unwrap_or_default();
        let weight_n = l.get("weight").and_then(Value::as_f64).filter(|f| f.is_finite() && *f != 0.0).unwrap_or(5.0);
        let weight = (weight_n.min(10.0) as i64).max(1);
        let severity = match l.get("severity").and_then(Value::as_str) {
            Some(s) if ["info", "warn", "error", "critical"].contains(&s) => s,
            _ => "warn",
        };
        let mut applies = Map::new();
        if !langs.is_empty() {
            applies.insert("languages".into(), json!(langs));
        }
        let applies_json = serde_json::to_string(&Value::Object(applies)).unwrap_or_default();
        let evidence_json = serde_json::to_string(&json!({ "pack": tech, "model": "ai", "ai_call_id": ai_call_id })).unwrap_or_default();

        crate::db_json::run_write(
            db,
            &format!("INSERT INTO laws.review_laws (scope, key, title, rule_md, detector, config_json, weight, severity, confidence, enforcement, applies_json, status, evidence_json, created_by) \
             VALUES ('framework', ?, ?, ?, ?, ?, ?, ?, 0.7, 'score', ?, 'candidate', ?, 'ai') \
             ON CONFLICT(key) DO UPDATE SET rule_md=excluded.rule_md, updated_at={} WHERE laws.review_laws.status='candidate'", crate::clock::now_sql()),
            &[json!(key), json!(title_str), json!(rule_md), json!(detector), json!(config_json), json!(weight), json!(severity), json!(applies_json), json!(evidence_json)],
        )?;
        candidates.push(json!({ "key": key, "title": title, "detector": detector }));
    }
    Ok(candidates)
}

pub fn constitutionPackGen(input: &Value, db: &Connection) -> Result<Value, String> {
    let _ = crate::constitution__helpers_schema::ensureConstitutionSchema(db);
    // `String(input?.tech ?? '').trim()` — JS `??` only substitutes on null/undefined.
    let tech_raw = match input.get("tech") {
        Some(v) if !v.is_null() => js_string(Some(v)),
        _ => String::new(),
    };
    let tech = tech_raw.trim().to_string();
    if tech.is_empty() {
        return Err("constitution-pack-gen: tech is required".to_string());
    }
    ensure_prompt(db)?;

    let docs_raw = match input.get("docs") {
        Some(v) if !v.is_null() => js_string(Some(v)),
        _ => String::new(),
    };
    let docs_sliced: String = docs_raw.chars().take(12000).collect();
    let docs_value = if docs_sliced.is_empty() { "(none — use your knowledge)".to_string() } else { docs_sliced };

    let ai_input = json!({
        "prompt_name": PROMPT,
        "input_kv": [
            { "key": "tech", "value": tech },
            { "key": "docs", "value": docs_value },
        ],
        "provider_fn": Value::Null,
    });

    // ── PARITY-DEFERRED: aiCall always returns Err here — this module's OWN try/catch
    // turns that into a normal Ok envelope (see module doc), it does not propagate.
    match crate::ai_ai_call::aiCall(&ai_input, db) {
        Ok(res) => {
            let ai_call_id = res.get("ai_call_id").cloned().unwrap_or(Value::Null);
            let cost = res.get("cost_usd").and_then(Value::as_f64).unwrap_or(0.0);
            let laws_text = res
                .get("output_kv")
                .and_then(Value::as_array)
                .and_then(|a| {
                    a.iter().find(|o| o.get("field_name").and_then(Value::as_str) == Some("laws_json") && o.get("parent_output_id").map_or(true, Value::is_null))
                })
                .and_then(|o| o.get("value_text"))
                .and_then(Value::as_str)
                .unwrap_or("");
            let laws: Vec<Value> = extract_json(laws_text)
                .and_then(|b| b.get("laws").and_then(Value::as_array).cloned())
                .unwrap_or_default();
            let max = resolve_max(input.get("max"));
            let candidates = persist_candidates(db, &tech, &laws, max, &ai_call_id)?;
            Ok(json!({
                "tech": tech,
                "proposed": candidates.len(),
                "candidates": candidates,
                "ai_call_id": ai_call_id,
                "cost_usd": (cost * 10000.0).round() / 10000.0,
            }))
        }
        Err(e) => Ok(json!({
            "tech": tech,
            "proposed": 0,
            "candidates": [],
            "ai_call_id": Value::Null,
            "cost_usd": 0,
            "error": e.chars().take(200).collect::<String>(),
        })),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS laws; ATTACH ':memory:' AS brain;").unwrap();
        c.execute_batch(
            "CREATE TABLE brain.prompt_templates ( id INTEGER PRIMARY KEY, name TEXT NOT NULL, version INTEGER NOT NULL, template_md TEXT NOT NULL, default_tier TEXT NOT NULL, default_temperature TEXT NOT NULL, max_output_tokens INTEGER NOT NULL DEFAULT 1024, superseded_by INTEGER, created_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')), cross_field_rules_json TEXT, judging_criteria_json TEXT, system_md TEXT, phase_flow TEXT NOT NULL DEFAULT 'single', UNIQUE (name, version) );
             CREATE TABLE brain.prompt_output_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, grounded_refs_json TEXT, UNIQUE (prompt_template_id, field_name) );
             CREATE TABLE brain.prompt_input_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, UNIQUE (prompt_template_id, field_name) );",
        )
        .unwrap();
        c
    }

    #[test]
    fn tech_is_required() {
        let c = setup();
        let e = constitutionPackGen(&json!({}), &c).unwrap_err();
        assert_eq!(e, "constitution-pack-gen: tech is required");
        let e2 = constitutionPackGen(&json!({ "tech": "  " }), &c).unwrap_err();
        assert_eq!(e2, "constitution-pack-gen: tech is required");
    }

    #[test]
    fn deferred_aicall_error_is_caught_into_a_normal_envelope() {
        let c = setup();
        let r = constitutionPackGen(&json!({ "tech": "Redis" }), &c).unwrap();
        assert_eq!(r["tech"], json!("Redis"));
        assert_eq!(r["proposed"], json!(0));
        assert_eq!(r["candidates"], json!([]));
        assert_eq!(r["ai_call_id"], Value::Null);
        assert_eq!(r["cost_usd"], json!(0));
        assert!(r["error"].as_str().unwrap().contains("ai-call"));
        // ensure_prompt still ran (real, ported, reachable surface before the try/catch)
        let n: i64 = c.query_row("SELECT COUNT(*) FROM brain.prompt_templates WHERE name = 'constitution-pack-gen'", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn ensure_prompt_is_idempotent_when_unchanged() {
        let c = setup();
        constitutionPackGen(&json!({ "tech": "Postgres" }), &c).unwrap();
        constitutionPackGen(&json!({ "tech": "Postgres" }), &c).unwrap();
        let n: i64 = c.query_row("SELECT COUNT(*) FROM brain.prompt_templates WHERE name = 'constitution-pack-gen'", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1);
    }

    // ── extract_json ─────────────────────────────────────────────────────
    #[test]
    fn extract_json_slices_first_brace_to_last() {
        assert_eq!(extract_json("prose {\"laws\":[]} trailing"), Some(json!({ "laws": [] })));
        assert_eq!(extract_json("no braces here"), None);
        assert_eq!(extract_json("{not json"), None);
        assert_eq!(extract_json(""), None);
    }

    // ── kebab ─────────────────────────────────────────────────────────────
    #[test]
    fn kebab_lowercases_and_collapses_non_alnum_runs() {
        assert_eq!(kebab("React Native"), "react-native");
        assert_eq!(kebab("  --Foo__Bar99--  "), "foo-bar99");
        assert_eq!(kebab(""), "");
        assert_eq!(kebab("!!!"), "");
    }

    // ── resolve_max ──────────────────────────────────────────────────────
    #[test]
    fn resolve_max_defaults_to_8_clamps_1_to_12() {
        assert_eq!(resolve_max(None), 8);
        assert_eq!(resolve_max(Some(&json!(0))), 8); // 0 is falsy in `Number(v) || 8`
        assert_eq!(resolve_max(Some(&json!(3))), 3);
        assert_eq!(resolve_max(Some(&json!(20))), 12);
        assert_eq!(resolve_max(Some(&json!(-5))), 1);
        assert_eq!(resolve_max(Some(&json!("bogus"))), 8);
    }

    // ── persist_candidates (unreachable from the default export, real+tested per module doc) ──
    #[test]
    fn persist_candidates_filters_bad_detectors_and_inserts_candidate_rows() {
        let c = setup();
        crate::constitution__helpers_schema::ensureConstitutionSchema(&c);
        let laws = vec![
            json!({ "key": "no-console-log", "title": "No console.log", "rule_md": "use a logger", "detector": "regex", "config": { "pattern": "console\\.log" }, "weight": 4, "severity": "warn", "languages": ["ts", "js", "go"] }),
            json!({ "title": "Bad detector kind", "detector": "unknown-kind", "config": {} }),
            json!({ "key": "no-pattern", "title": "Missing pattern", "detector": "regex", "config": {} }),
            json!({ "key": "bad-metric", "title": "Bad metric name", "detector": "metric", "config": { "metric": "not-a-metric", "max": 5 } }),
            json!({ "key": "ok-metric", "title": "Complexity too high", "rule_md": "keep it low", "detector": "metric", "config": { "metric": "complexity", "max": 10 }, "weight": 6, "severity": "error" }),
        ];
        let out = persist_candidates(&c, "React", &laws, 8, &json!("call-123")).unwrap();
        assert_eq!(out.len(), 2);
        assert_eq!(out[0]["key"], json!("react-no-console-log"));
        assert_eq!(out[0]["detector"], json!("regex"));
        assert_eq!(out[1]["key"], json!("react-ok-metric"));

        let n: i64 = c.query_row("SELECT COUNT(*) FROM laws.review_laws WHERE status = 'candidate'", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 2);
        let (severity, weight, applies_json): (String, i64, String) = c
            .query_row(
                "SELECT severity, weight, applies_json FROM laws.review_laws WHERE key = 'react-no-console-log'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(severity, "warn");
        assert_eq!(weight, 4);
        assert_eq!(applies_json, r#"{"languages":["ts","js"]}"#); // "go" filtered out
    }

    #[test]
    fn persist_candidates_respects_max_and_skips_empty_key() {
        let c = setup();
        crate::constitution__helpers_schema::ensureConstitutionSchema(&c);
        let laws = vec![
            json!({ "key": "!!!", "title": "!!!", "detector": "absent", "config": { "pattern": "x" } }),
            json!({ "key": "second", "title": "Second", "rule_md": "r", "detector": "import", "config": { "pattern": "left-pad" } }),
        ];
        let out = persist_candidates(&c, "!!!", &laws, 1, &Value::Null).unwrap();
        // max=1 takes only the first law, whose kebabed key is empty (prefix "" + key "" → "") — skipped.
        assert_eq!(out.len(), 0);
    }
}
