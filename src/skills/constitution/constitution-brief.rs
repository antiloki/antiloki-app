//! constitution-brief.rs — twin of skills/constitution/constitution-brief.mjs (db lane,
//! cross-db brain/judgments/laws via constitution-report/schema/prompt-find/prompt-add). NAMES
//! ARE CONTRACT: default `constitutionBrief`. THE EXECUTIVE BRIEF — ONE high-tier AI call over
//! the whole DETERMINISTIC model (health + axes, worst spots + broken laws, knowledge map,
//! hidden couplings, cross-analysis, deltas), producing a headline / state-of-codebase /
//! 5-7 ranked moves.
//!
//! PARITY-DEFERRED: the AI call is dispatched through `crate::ai_ai_call::aiCall`, the deferred
//! network-AI stub (see ai-call.rs's module doc) — it always returns `Err`, so `constitutionBrief`
//! takes the SAME `catch` branch the JS does on every real dispatch, returning `{ ok:false,
//! cost_usd:0, ai_call_id:null, error: "..." }` (never panics/short-circuits with `?`, mirroring
//! JS's `try { await aiCall(...) } catch (e) { return {...} }`). Everything else is ported
//! faithfully and IS the deterministic surface these tests exercise: the guard, `ensure_prompt`
//! (reusing the already-converted `ai_prompt_find`/`ai_prompt_add` twins, including the JS
//! template/system-md MISMATCH re-add path — not just a bare existence check like book-run.rs's
//! `ensure_faculty`), the report call (reusing `constitution_constitution_report::
//! constitutionReport`), and the worst/knowledge/signals/deltas prompt-block assembly. `extract_
//! json` is ported as its own pure fn, unit-tested directly with canned strings (unreachable
//! from `constitutionBrief` itself once `aiCall` always errs, but correct by construction —
//! same pattern as pl-research.rs's `build_research_report`).
// QUALITY: mixed unit — deterministic assembly/db-read surface (fully ported, fixture-reachable
// up to the deferred aiCall boundary) + 1 pure response-parsing helper (unreachable from the
// default export, unit-tested directly) + 1 bespoke/deferred aiCall dispatch (no fixture/frontier).
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
fn nz(v: Option<&Value>) -> Option<&Value> {
    v.filter(|x| !x.is_null())
}

const PROMPT: &str = "constitution-brief";
const SYSTEM: &str = "You are a staff engineer writing THE EXECUTIVE BRIEF of a codebase appraisal for its owner, from a COMPLETELY DETERMINISTIC model — you are given counted engineering scores (health axes incl. law compliance, determinism, AI-readability), the worst-ranked units with the house LAWS they break, the knowledge map (hotspots, bus-factor, god modules, islands, hidden complexity), hidden couplings (co-change with no import edge), cross-analysis correlations, and what changed since last time. Rules: (1) every claim traces to the given data — cite file paths and law keys; (2) moves are ranked by payoff-per-effort, each independently actionable; (3) no platitudes — if it is healthy, say so and keep moves small; (4) effort S (hours) / M (a day or two) / L (a week+); (5) prefer moves the DATA makes visible that a reader could not get from skimming — a broken architecture law, a scattered/duplicated concern, a bus-factor-1 hotspot, a regression. Output ONLY valid JSON: {\"headline\":\"one blunt sentence — the verdict\",\"state_md\":\"<=180 words, plain prose\",\"moves\":[{\"title\":\"...\",\"why\":\"1-2 sentences tied to the data\",\"evidence\":[\"path or law-key\"],\"effort\":\"S|M|L\",\"gain\":\"which numbers move\"}]} with 5-7 moves. No prose outside the JSON.";
const TEMPLATE: &str = "ENGINEERING MODEL (counted):\n{{model}}\n\nWORST UNITS (health \u{b7} broken laws):\n{{worst}}\n\nKNOWLEDGE MAP:\n{{knowledge}}\n\nHIDDEN COUPLINGS \u{b7} DUPLICATION \u{b7} CROSS-ANALYSIS:\n{{signals}}\n\nSINCE LAST REVIEW:\n{{deltas}}\n\nOutput the brief JSON.";

// mirrors JS ensurePrompt: try promptFind; if found AND template_md+system_md already match the
// constants, no-op; otherwise (missing OR stale) promptAdd. Unlike book-run.rs's ensure_faculty
// (bare existence check), this ALSO re-adds on a content mismatch — ported verbatim from the JS.
fn ensure_prompt(db: &Connection) -> Result<(), String> {
    let e = crate::ai_prompt_find::promptFind(&json!({ "name": PROMPT }), db).ok();
    if let Some(ev) = &e {
        let tpl_ok = ev.get("template_md").and_then(Value::as_str) == Some(TEMPLATE);
        let sys_ok = ev.get("system_md").and_then(Value::as_str) == Some(SYSTEM);
        if tpl_ok && sys_ok {
            return Ok(());
        }
    }
    crate::ai_prompt_add::promptAdd(
        &json!({
            "name": PROMPT,
            "template_md": TEMPLATE,
            "system_md": SYSTEM,
            "default_tier": "high",
            "default_temperature": "cold",
            "phase_flow": "single_only",
            "max_output_tokens": 2200,
            "output_fields": [{ "field_name": "brief_json", "field_type": "string" }],
        }),
        db,
    )?;
    Ok(())
}

// `String(t ?? '')`, first `{` .. last `}` slice, `JSON.parse` with try/catch → null.
// Pure, unit-tested directly — unreachable from `constitutionBrief` once `aiCall` always errs.
fn extract_json(t: Option<&Value>) -> Option<Value> {
    let s = match t {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(),
        Some(o) => o.to_string(),
    };
    let a = s.find('{')?;
    let b = s.rfind('}')?;
    if b <= a {
        return None;
    }
    serde_json::from_str::<Value>(&s[a..=b]).ok()
}

pub fn constitutionBrief(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS: `const db = context?.db; if (!db) throw err.required(...)` — unreachable here
    // (`db: &Connection` is a required, non-nullable reference).
    let _ = crate::constitution__helpers_schema::ensureConstitutionSchema(db);

    let rep = crate::constitution_constitution_report::constitutionReport(
        &json!({ "snapshot_id": input.get("snapshot_id").cloned().unwrap_or(Value::Null) }),
        db,
    )?;
    let summary = nz(rep.get("summary")).cloned();
    let Some(s) = summary else {
        return Err("constitution-brief: a done snapshot is required".to_string());
    };
    ensure_prompt(db)?;

    let snapshot_id = s.get("snapshot_id").cloned().unwrap_or(Value::Null);

    // worst — top 15 file-kind entries, health/compliance/laws
    let worst_arr: Vec<Value> = rep.get("worst").and_then(Value::as_array).cloned().unwrap_or_default();
    let worst_lines: Vec<String> = worst_arr
        .iter()
        .filter(|w| w.get("kind").and_then(Value::as_str) == Some("file"))
        .take(15)
        .map(|w| {
            let laws: Vec<String> = w
                .get("breakdown")
                .and_then(Value::as_object)
                .map(|o| o.keys().cloned().collect())
                .unwrap_or_default();
            let laws_str = if laws.is_empty() { "none".to_string() } else { laws.join(", ") };
            let path = w.get("path").and_then(Value::as_str).unwrap_or("");
            let health = w.get("health").cloned().unwrap_or(Value::Null);
            let score = w.get("score").cloned().unwrap_or(Value::Null);
            format!("{path} health={} compliance={} laws=[{laws_str}]", disp(&health), disp(&score))
        })
        .collect();
    let worst = if worst_lines.is_empty() { "(none)".to_string() } else { worst_lines.join("\n") };

    // knowledge map
    let k = s.get("knowledge").cloned().unwrap_or(json!({}));
    let mut knowledge_lines: Vec<String> = Vec::new();
    if let Some(a) = k.get("hotspots").and_then(Value::as_array) {
        if !a.is_empty() {
            let paths: Vec<String> = a.iter().take(5).map(|h| h.get("path").and_then(Value::as_str).unwrap_or("").to_string()).collect();
            knowledge_lines.push(format!("HOTSPOTS (churn\u{d7}complexity): {}", paths.join(" \u{b7} ")));
        }
    }
    if let Some(a) = k.get("bus_factor").and_then(Value::as_array) {
        if !a.is_empty() {
            let paths: Vec<String> = a.iter().take(5).map(|b| b.get("path").and_then(Value::as_str).unwrap_or("").to_string()).collect();
            knowledge_lines.push(format!("BUS-FACTOR 1 (single author, depended-on): {}", paths.join(" \u{b7} ")));
        }
    }
    if let Some(a) = k.get("god_modules").and_then(Value::as_array) {
        if !a.is_empty() {
            let names: Vec<String> = a.iter().map(|g| g.as_str().unwrap_or("").to_string()).collect();
            knowledge_lines.push(format!("GOD MODULES: {}", names.join(" \u{b7} ")));
        }
    }
    if let Some(a) = k.get("islands").and_then(Value::as_array) {
        if !a.is_empty() {
            let names: Vec<String> = a.iter().map(|i| i.as_str().unwrap_or("").to_string()).collect();
            knowledge_lines.push(format!("ISLANDS (disconnected): {}", names.join(" \u{b7} ")));
        }
    }
    if let Some(a) = k.get("hidden_complexity").and_then(Value::as_array) {
        if !a.is_empty() {
            let items: Vec<String> = a
                .iter()
                .take(5)
                .map(|h| {
                    let path = h.get("path").and_then(Value::as_str).unwrap_or("");
                    let transitive = h.get("transitive").cloned().unwrap_or(Value::Null);
                    format!("{path}(\u{2192}{})", disp(&transitive))
                })
                .collect();
            knowledge_lines.push(format!("HIDDEN COMPLEXITY (simple-looking, deep deps): {}", items.join(" \u{b7} ")));
        }
    }
    let knowledge = if knowledge_lines.is_empty() { "(clean)".to_string() } else { knowledge_lines.join("\n") };

    // signals — co_change, duplication, cross
    let mut signal_lines: Vec<String> = Vec::new();
    if let Some(a) = s.get("co_change").and_then(Value::as_array) {
        if !a.is_empty() {
            let items: Vec<String> = a
                .iter()
                .take(5)
                .map(|c| {
                    let ca = c.get("a").and_then(Value::as_str).unwrap_or("");
                    let cb = c.get("b").and_then(Value::as_str).unwrap_or("");
                    let count = c.get("count").cloned().unwrap_or(Value::Null);
                    format!("{ca}\u{2194}{cb}\u{d7}{}", disp(&count))
                })
                .collect();
            signal_lines.push(format!("COUPLINGS: {}", items.join(" \u{b7} ")));
        }
    }
    if let Some(a) = rep.get("duplication").and_then(Value::as_array) {
        if !a.is_empty() {
            let items: Vec<String> = a
                .iter()
                .take(5)
                .map(|d| {
                    let da = d.get("a").and_then(Value::as_str).unwrap_or("");
                    let db_ = d.get("b").and_then(Value::as_str).unwrap_or("");
                    let jac = d.get("jaccard").cloned().unwrap_or(Value::Null);
                    format!("{da}\u{2248}{db_}({})", disp(&jac))
                })
                .collect();
            signal_lines.push(format!("DUPLICATION: {}", items.join(" \u{b7} ")));
        }
    }
    if let Some(a) = rep.get("cross").and_then(Value::as_array) {
        if !a.is_empty() {
            let items: Vec<String> = a
                .iter()
                .map(|c| {
                    let name = c.get("name").and_then(Value::as_str).unwrap_or("");
                    let r = c.get("r").cloned().unwrap_or(Value::Null);
                    let n = c.get("n").cloned().unwrap_or(Value::Null);
                    format!("{name} r={} (n={})", disp(&r), disp(&n))
                })
                .collect();
            signal_lines.push(format!("CORRELATIONS: {}", items.join(" \u{b7} ")));
        }
    }
    let signals = if signal_lines.is_empty() { "(none detected)".to_string() } else { signal_lines.join("\n") };

    // deltas
    let deltas_v = rep.get("deltas").cloned().unwrap_or(Value::Null);
    let deltas = if truthy(Some(&deltas_v)) {
        let from = deltas_v.get("from_snapshot").cloned().unwrap_or(Value::Null);
        let health = deltas_v.get("health").cloned().unwrap_or(Value::Null);
        let health_str = if truthy(Some(&health)) {
            let h = health.as_f64().unwrap_or(0.0);
            format!("{}{}", if h > 0.0 { "+" } else { "" }, disp(&health))
        } else {
            "?".to_string()
        };
        let worse_arr: Vec<Value> = deltas_v.get("worse").and_then(Value::as_array).cloned().unwrap_or_default();
        let worse_str = if worse_arr.is_empty() {
            "none".to_string()
        } else {
            worse_arr
                .iter()
                .map(|d| {
                    let path = d.get("path").and_then(Value::as_str).unwrap_or("");
                    let from_v = d.get("from").cloned().unwrap_or(Value::Null);
                    let to_v = d.get("to").cloned().unwrap_or(Value::Null);
                    format!("{path} {}\u{2192}{}", disp(&from_v), disp(&to_v))
                })
                .collect::<Vec<String>>()
                .join(", ")
        };
        format!("vs snapshot #{} \u{b7} health {} \u{b7} worse: {worse_str}", disp(&from), health_str)
    } else {
        "(first review \u{2014} no deltas)".to_string()
    };

    // model kv value
    let health = s.get("health").cloned().unwrap_or(Value::Null);
    let compliance = s.get("compliance").cloned().unwrap_or(Value::Null);
    let determinism = s.get("determinism").cloned().unwrap_or(Value::Null);
    let ai_readability = s.get("ai_readability").cloned().unwrap_or(Value::Null);
    let axes_avg = s.get("axes_avg").cloned().unwrap_or(json!({}));
    let violations = s.get("violations").cloned().unwrap_or(Value::Null);
    let files = s.get("files").cloned().unwrap_or(Value::Null);
    let model_kv = format!(
        "health={} compliance={} determinism={}% ai_readability={} axes={} violations={} files={}",
        disp(&health),
        disp(&compliance),
        disp(&determinism),
        disp(&ai_readability),
        serde_json::to_string(&axes_avg).unwrap_or_else(|_| "{}".to_string()),
        disp(&violations),
        disp(&files),
    );

    let input_kv = json!([
        { "key": "model", "value": model_kv },
        { "key": "worst", "value": worst },
        { "key": "knowledge", "value": knowledge },
        { "key": "signals", "value": signals },
        { "key": "deltas", "value": deltas },
    ]);
    let provider_fn = input.get("provider_fn").cloned().unwrap_or(Value::Null);
    let ai_input = json!({ "prompt_name": PROMPT, "input_kv": input_kv, "provider_fn": provider_fn });

    // PARITY-DEFERRED: mirrors JS `try { await aiCall(...) } catch (e) { return {...} }` — the
    // deferred stub always Errs, so this branch is the one always taken during conform-db.
    match crate::ai_ai_call::aiCall(&ai_input, db) {
        Ok(res) => {
            let cost = res.get("cost_usd").and_then(Value::as_f64).unwrap_or(0.0);
            let model = res.get("model_id").cloned().unwrap_or(Value::Null);
            let ai_call_id = res.get("ai_call_id").cloned().unwrap_or(Value::Null);
            let value_text = res
                .get("output_kv")
                .and_then(Value::as_array)
                .and_then(|arr| {
                    arr.iter().find(|o| {
                        o.get("field_name").and_then(Value::as_str) == Some("brief_json")
                            && matches!(o.get("parent_output_id"), None | Some(Value::Null))
                    })
                })
                .and_then(|o| o.get("value_text"));
            let brief_opt = extract_json(value_text);
            if let Some(brief) = &brief_opt {
                if truthy(brief.get("headline")) {
                    let mut body = brief.as_object().cloned().unwrap_or_default();
                    body.insert("kind".into(), json!("brief"));
                    let body_str = serde_json::to_string(&Value::Object(body)).unwrap_or_default();
                    let body_str: String = body_str.chars().take(12000).collect();
                    crate::db_json::run_write(
                        db,
                        "INSERT INTO judgments.review_insights (snapshot_id, path, body_json, model, attempts, cost_usd) VALUES (?, '__brief__', ?, ?, 1, ?)",
                        &[snapshot_id.clone(), json!(body_str), model.clone(), json!(cost)],
                    )?;
                    let mut out_brief = brief.as_object().cloned().unwrap_or_default();
                    out_brief.insert("model".into(), model);
                    return Ok(json!({
                        "snapshot_id": snapshot_id, "ok": true, "cost_usd": num_out4(cost),
                        "ai_call_id": ai_call_id, "brief": Value::Object(out_brief),
                    }));
                }
            }
            Ok(json!({
                "snapshot_id": snapshot_id, "ok": false, "cost_usd": num_out4(cost),
                "ai_call_id": ai_call_id, "brief": Value::Null,
            }))
        }
        Err(e) => {
            let msg: String = e.chars().take(200).collect();
            Ok(json!({ "snapshot_id": snapshot_id, "ok": false, "cost_usd": 0, "ai_call_id": Value::Null, "error": msg }))
        }
    }
}

// number → JS template-literal text (integers render bare, no ".0"); null/missing → "null"/"undefined" per tmpl.
fn disp(v: &Value) -> String {
    match v {
        Value::Null => "null".to_string(),
        Value::Number(n) => n.to_string(),
        Value::String(s) => s.clone(),
        Value::Bool(b) => b.to_string(),
        other => other.to_string(),
    }
}

fn num_out4(f: f64) -> Value {
    let m = 10000.0;
    let rounded = ((f * m) + 0.5).floor() / m;
    if rounded.fract() == 0.0 { json!(rounded as i64) } else { json!(rounded) }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BRAIN_SCHEMA: &str = "
        CREATE TABLE brain.prompt_templates ( id INTEGER PRIMARY KEY, name TEXT NOT NULL, version INTEGER NOT NULL, template_md TEXT NOT NULL, default_tier TEXT NOT NULL CHECK (default_tier IN ('low','mid','high')), default_temperature TEXT NOT NULL CHECK (default_temperature IN ('freezed','cold','warm','hot','hell')), max_output_tokens INTEGER NOT NULL DEFAULT 1024, superseded_by INTEGER, created_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')), cross_field_rules_json TEXT, judging_criteria_json TEXT, system_md TEXT, phase_flow TEXT NOT NULL DEFAULT 'single', UNIQUE (name, version) );
        CREATE TABLE brain.prompt_output_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL CHECK (field_type IN ('string','number','boolean','enum','array_of_string','array_of_object')), enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, grounded_refs_json TEXT, UNIQUE (prompt_template_id, field_name) );
        CREATE TABLE brain.prompt_input_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL CHECK (field_type IN ('string','number','boolean','enum','array_of_string','array_of_object')), enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, UNIQUE (prompt_template_id, field_name) );
    ";

    fn conn() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS laws; ATTACH ':memory:' AS judgments; ATTACH ':memory:' AS brain").unwrap();
        c.execute_batch(BRAIN_SCHEMA).unwrap();
        c
    }

    #[test]
    fn extract_json_finds_object_between_braces() {
        assert_eq!(extract_json(Some(&json!("prefix {\"a\":1} suffix"))), Some(json!({"a":1})));
    }

    #[test]
    fn extract_json_no_braces_is_none() {
        assert_eq!(extract_json(Some(&json!("no json here"))), None);
        assert_eq!(extract_json(None), None);
        assert_eq!(extract_json(Some(&Value::Null)), None);
    }

    #[test]
    fn extract_json_invalid_json_is_none() {
        assert_eq!(extract_json(Some(&json!("{not valid}"))), None);
    }

    #[test]
    fn no_done_snapshot_errors() {
        let c = conn();
        let err = constitutionBrief(&json!({}), &c).unwrap_err();
        assert!(err.contains("a done snapshot is required"), "{err}");
    }

    #[test]
    fn done_snapshot_takes_the_deferred_ai_call_catch_branch() {
        let c = conn();
        let _ = crate::constitution__helpers_schema::ensureConstitutionSchema(&c);
        c.execute(
            "INSERT INTO judgments.review_snapshots (id, created_at, source, scope_json, manifest_json, constitution_hash, status, judged, duration_ms, totals_json) VALUES \
             (1, '2026-01-01 00:00:00', 'wizard', '{\"all\":true}', '[\"a.js\"]', 'h1', 'done', 1, 100, '{\"health\":90}')",
            [],
        )
        .unwrap();
        let r = constitutionBrief(&json!({}), &c).unwrap();
        assert_eq!(r["ok"], json!(false));
        assert_eq!(r["snapshot_id"], json!(1));
        assert!(r.get("error").and_then(Value::as_str).unwrap_or("").contains("ai-call") || r.get("error").is_some());
    }

    #[test]
    fn ensure_prompt_writes_then_is_idempotent() {
        let c = conn();
        ensure_prompt(&c).unwrap();
        let found = crate::ai_prompt_find::promptFind(&json!({ "name": PROMPT }), &c).unwrap();
        assert_eq!(found.get("template_md").and_then(Value::as_str), Some(TEMPLATE));
        assert_eq!(found.get("system_md").and_then(Value::as_str), Some(SYSTEM));
        // second call is a no-op (already matches) — still Ok
        ensure_prompt(&c).unwrap();
    }
}
