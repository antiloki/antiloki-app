//! heimdall-prompt-patch.rs — twin of skills/heimdall/heimdall-prompt-patch.mjs (db lane,
//! cross-db ATTACH: brain). NAMES ARE CONTRACT: `heimdallPromptPatch` (default). Closes the
//! tagging loop: op:'draft' reads a template's recorded evidence (negative human/AI tags,
//! prompt-design tags, lint/validation errors, detector flags, one real exemplar output) and one
//! high-tier call proposes a revised template (nothing self-applies — the human reviews);
//! op:'derive_criteria' decomposes a template's own text into decidable judging criteria (one
//! call, stored on the row, no version bump); op:'apply' is fully deterministic — registers a
//! human-approved `new_template_md` as the next version via `promptAdd`, carrying the current
//! version's frozen output contract (tier/temperature/caps/output_fields) forward.
//!
//! `context.db` missing → JS throws `err.contextMissing('heimdall-prompt-patch', 'db')`.
//! Unreachable here: `db: &Connection` is a required, non-nullable reference (same convention as
//! every sibling db-lane twin). `context.provider_fn` (a JS function reference for provider
//! selection/DI) has no Rust equivalent to carry — threaded as `null` into the constructed
//! aiCall input, same as book-run.rs's/pl-research.rs's `provider_fn`.
//!
//! PARITY-DEFERRED (draft, derive_criteria only): each dispatches through
//! `crate::ai_ai_call::aiCall`, the deferred network-AI stub (see ai-call.rs's module doc) — it
//! always returns `Err`, so both ops propagate that error (JS: plain `await aiCall(...)`, no
//! try/catch) once the (real, ported) deterministic surface above them has run: guards, the
//! active-template lookup, the evidence-gathering queries (draft) / template_md assembly (derive),
//! and `ensureDraftPrompt`/`ensureDerivePrompt` (reusing the already-converted
//! `ai_prompt_find`/`ai_prompt_add` twins, mirroring book-run.rs's `ensure_faculty`). The
//! response-shaping code that would run after a successful call (`proposed_template_md`
//! extraction, `CRITERION|` line parsing + the `UPDATE ... judging_criteria_json` write) is
//! ported byte-for-byte from the JS but is unreachable by this file's own fixture since it runs
//! only after a successful `aiCall`, which the deferred stub never produces — correctness there is
//! by inspection, same convention as book-run.rs's post-wave staging logic.
//!
//! op:'apply' is FULLY DETERMINISTIC (no aiCall) and IS fixture-reachable end-to-end: it composes
//! the already-converted `ai_prompt_add::promptAdd` twin, so its db write is real and tested.
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
fn str_qq(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(),
        Some(o) => o.to_string(),
    }
} // = JS String(v ?? '')
fn tmpl(v: Option<&Value>) -> String {
    match v {
        None => "undefined".into(),
        Some(Value::Null) => "null".into(),
        Some(x) => str_qq(Some(x)),
    }
} // = JS `${v}`
fn nz(v: Option<&Value>) -> Option<&Value> {
    v.filter(|x| !x.is_null())
} // JS `??` right operand
fn first(rows: &Value) -> Option<Value> {
    rows.as_array().and_then(|a| a.first()).cloned()
}

const DRAFT_PROMPT: &str = "heimdall.prompt.patch.v1";
const SYSTEM: &str = "You are a PROMPT SURGEON. You revise ONE prompt template using the recorded evidence of how it failed. You keep everything that works, change only what the failures implicate, and never invent new output fields — the output contract is frozen.";
const TEMPLATE: &str = "Revise the prompt template below. Recorded evidence of how it behaves is listed — verdicts humans and judges recorded, lint failures, detector flags.

═══ RULES ═══
- Keep the SAME placeholders ({{...}}) and the SAME output contract — you may clarify how to fill fields, never add/remove/rename them.
- Change only what the evidence (and the operator's direction) implicates: ambiguity → sharpen; missing context → demand or provide it; format traps → defuse; contradictions → resolve.
- Keep it as short as it can be while fixing the problems.

═══ OPERATOR'S DIRECTION (obey above all else) ═══
{{operator_direction}}

═══ THE ACTIVE TEMPLATE ({{template_name}}) ═══
{{template_md}}

═══ RECORDED EVIDENCE ═══
{{evidence}}

Reply with ONLY the full revised template text — no preamble, no fences, no commentary.";

const DERIVE_PROMPT: &str = "heimdall.criteria.derive.v1";
const DERIVE_TEMPLATE: &str = "Decompose the prompt template below into its JUDGING CRITERIA: every distinct thing it DEMANDS of the output, as decidable statements a reviewer can verify one by one.

Think as needed, then END with ONLY criterion lines, one per line, EXACTLY:
CRITERION|<one decidable demand the template makes of the output>

Rules: 3-8 lines · each independently checkable against an output · cover format demands AND content demands · no restating placeholders, only what the OUTPUT must satisfy.

═══ THE TEMPLATE ({{template_name}}) ═══
{{template_md}}";

// try promptFind; if found AND its template_md still matches, skip; else (re-)register.
fn ensure_derive_prompt(db: &Connection) -> Result<(), String> {
    let existing = crate::ai_prompt_find::promptFind(&json!({ "name": DERIVE_PROMPT }), db).ok();
    if let Some(e) = &existing {
        if e.get("template_md").and_then(Value::as_str) == Some(DERIVE_TEMPLATE) {
            return Ok(());
        }
    }
    crate::ai_prompt_add::promptAdd(
        &json!({
            "name": DERIVE_PROMPT, "template_md": DERIVE_TEMPLATE,
            "default_tier": "high", "default_temperature": "cold", "max_output_tokens": 900,
            "output_fields": [{ "field_name": "derive_raw", "field_type": "string" }],
            "judging_criteria": [
                "Ends with only CRITERION| lines.",
                "Each criterion is independently checkable against an output.",
                "Covers both format and content demands of the template.",
            ],
        }),
        db,
    )?;
    Ok(())
}

fn ensure_draft_prompt(db: &Connection) -> Result<(), String> {
    let existing = crate::ai_prompt_find::promptFind(&json!({ "name": DRAFT_PROMPT }), db).ok();
    if let Some(e) = &existing {
        if e.get("template_md").and_then(Value::as_str) == Some(TEMPLATE) {
            return Ok(());
        }
    }
    crate::ai_prompt_add::promptAdd(
        &json!({
            "name": DRAFT_PROMPT, "template_md": TEMPLATE, "system_md": SYSTEM,
            "default_tier": "high", "default_temperature": "cold", "max_output_tokens": 4000,
            "output_fields": [{ "field_name": "revised_template_md", "field_type": "string" }],
            "judging_criteria": [
                "Returns ONLY the full revised template — no preamble, fences, or commentary.",
                "Keeps every {{placeholder}} and the exact output contract.",
                "Every change traces to a listed failure or the operator direction.",
                "Not longer than the fixes require.",
            ],
        }),
        db,
    )?;
    Ok(())
}

const ALL_SOURCES: [&str; 5] = ["negative_tags", "prompt_tags", "validation_errors", "detector_flags", "exemplar"];

pub fn heimdallPromptPatch(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS: `const { db, provider_fn = null } = context ?? {}; if (!db) throw err.contextMissing(...)`
    // — unreachable here (Rust's `db: &Connection` is never absent). provider_fn threaded as null.
    let provider_fn = Value::Null;
    let op = str_qq(input.get("op"));

    if op == "draft" {
        let base = str_qq(input.get("template_base"));
        if base.is_empty() {
            return Err("heimdall-prompt-patch: template_base is required".to_string());
        }
        let tpl_rows = crate::db_json::query_rows(
            db,
            "SELECT id, name, template_md, system_md FROM brain.prompt_templates
             WHERE (name = ? OR name LIKE ? || '.v%') AND superseded_by IS NULL
             ORDER BY id DESC LIMIT 1",
            &[json!(base), json!(base)],
        )?;
        let Some(tpl) = first(&tpl_rows) else {
            return Ok(json!({ "ok": true, "skipped": true, "reason": format!("no active template for base '{base}'") }));
        };
        let tpl_id = tpl.get("id").cloned().unwrap_or(Value::Null);
        let tpl_name = js_string(tpl.get("name").unwrap_or(&Value::Null));

        // PERSONALIZED DRAFT: the operator picks which evidence SOURCES feed the surgeon and can
        // add free-text direction. Default: everything available. Set-dedup preserves first
        // occurrence order (JS `new Set([...])`).
        let chosen: Vec<String> = match input.get("sources").and_then(Value::as_array) {
            Some(a) if !a.is_empty() => {
                let mut seen: Vec<String> = Vec::new();
                for s in a {
                    if let Some(s) = s.as_str() {
                        if ALL_SOURCES.contains(&s) && !seen.iter().any(|x| x == s) {
                            seen.push(s.to_string());
                        }
                    }
                }
                seen
            }
            _ => ALL_SOURCES.iter().map(|s| s.to_string()).collect(),
        };
        let has = |name: &str| chosen.iter().any(|s| s == name);
        let instructions: String = str_qq(input.get("instructions")).trim().chars().take(2000).collect();

        let mut sections: Vec<String> = Vec::new();
        let mut evidence_rows: i64 = 0;

        if has("negative_tags") {
            let rows = crate::db_json::query_rows(
                db,
                "SELECT 'human' AS src, h.verdict, NULL AS failure_class, h.reason_md
                 FROM brain.heimdall_human_tags h JOIN brain.heimdall_castings c ON c.ai_call_id = h.ai_call_id
                 WHERE c.template_base = ? AND h.verdict < 0
                 UNION ALL
                 SELECT 'ai_judge' AS src, t.verdict, t.failure_class, t.reason_md
                 FROM brain.heimdall_ai_tags t JOIN brain.heimdall_castings c2 ON c2.ai_call_id = t.target_ai_call_id
                 WHERE c2.template_base = ? AND t.verdict < 0 AND t.superseded_by_human = 0
                 LIMIT 15",
                &[json!(base), json!(base)],
            )?;
            let rows_arr = rows.as_array().cloned().unwrap_or_default();
            if !rows_arr.is_empty() {
                evidence_rows += rows_arr.len() as i64;
                let body = rows_arr.iter().enumerate().map(|(i, f)| {
                    let cls = if truthy(f.get("failure_class")) { format!(" class={}", js_string(f.get("failure_class").unwrap())) } else { String::new() };
                    let reason: String = js_string(nz(f.get("reason_md")).unwrap_or(&json!("(no reason)"))).chars().take(300).collect();
                    format!("{}. [{} {}]{cls} {reason}", i + 1, tmpl(f.get("src")), tmpl(f.get("verdict")))
                }).collect::<Vec<_>>().join("\n");
                sections.push(format!("— NEGATIVE MODEL TAGS (verdicts on outputs this template produced) —\n{body}"));
            }
        }
        if has("prompt_tags") {
            let rows = crate::db_json::query_rows(
                db,
                "SELECT source, verdict, failure_class, reason_md FROM brain.heimdall_prompt_tags WHERE template_name = ? AND verdict < 0 ORDER BY id DESC LIMIT 8",
                &[json!(tpl_name.clone())],
            )?;
            let rows_arr = rows.as_array().cloned().unwrap_or_default();
            if !rows_arr.is_empty() {
                evidence_rows += rows_arr.len() as i64;
                let body = rows_arr.iter().enumerate().map(|(i, f)| {
                    let cls = if truthy(f.get("failure_class")) { format!(" class={}", js_string(f.get("failure_class").unwrap())) } else { String::new() };
                    let reason: String = str_qq(f.get("reason_md")).chars().take(300).collect();
                    format!("{}. [{} {}]{cls} {reason}", i + 1, tmpl(f.get("source")), tmpl(f.get("verdict")))
                }).collect::<Vec<_>>().join("\n");
                sections.push(format!("— NEGATIVE PROMPT-DESIGN TAGS (verdicts on THIS TEMPLATE's design) —\n{body}"));
            }
        }
        if has("validation_errors") {
            let rows = crate::db_json::query_rows(
                db,
                "SELECT at.validation_errors_json FROM brain.ai_call_attempts at JOIN brain.ai_calls a ON a.id = at.ai_call_id
                 WHERE a.prompt_template_id = ? AND at.validation_errors_json IS NOT NULL ORDER BY at.id DESC LIMIT 8",
                &[tpl_id.clone()],
            )?;
            let mut errs: Vec<String> = Vec::new();
            for r in rows.as_array().cloned().unwrap_or_default() {
                if let Some(s) = r.get("validation_errors_json").and_then(Value::as_str) {
                    if let Ok(Value::Array(arr)) = serde_json::from_str::<Value>(s) {
                        for e in arr {
                            errs.push(js_string(&e).chars().take(200).collect());
                        }
                    }
                }
            }
            if !errs.is_empty() {
                evidence_rows += errs.len() as i64;
                let body = errs.iter().take(10).enumerate().map(|(i, e)| format!("{}. {e}", i + 1)).collect::<Vec<_>>().join("\n");
                sections.push(format!("— LINT/VALIDATION ERRORS (what the format gate rejected, verbatim) —\n{body}"));
            }
        }
        if has("detector_flags") {
            let rows = crate::db_json::query_rows(
                db,
                "SELECT outcome_class, COUNT(*) AS n FROM brain.ai_calls
                 WHERE prompt_template_id = ? AND cached = 0 AND outcome_class IS NOT NULL AND outcome_class NOT IN ('clean','cached')
                 GROUP BY outcome_class ORDER BY n DESC",
                &[tpl_id.clone()],
            )?;
            let rows_arr = rows.as_array().cloned().unwrap_or_default();
            if !rows_arr.is_empty() {
                evidence_rows += rows_arr.len() as i64;
                let body = rows_arr.iter().map(|r| format!("{}: {} call(s)", tmpl(r.get("outcome_class")), tmpl(r.get("n")))).collect::<Vec<_>>().join("\n");
                sections.push(format!("— OUTCOME DETECTOR FLAGS —\n{body}"));
            }
        }
        if has("exemplar") {
            let ex_rows = crate::db_json::query_rows(db, "SELECT MAX(id) AS id FROM brain.ai_calls WHERE prompt_template_id = ? AND cached = 0", &[tpl_id.clone()])?;
            let ex = first(&ex_rows).and_then(|r| r.get("id").cloned()).filter(|v| !v.is_null());
            if let Some(ex_id) = ex {
                let out_rows = crate::db_json::query_rows(
                    db,
                    "SELECT field_name, value_text FROM brain.ai_call_outputs WHERE ai_call_id = ? AND value_text IS NOT NULL ORDER BY order_index, id",
                    &[ex_id],
                )?;
                let outs: String = out_rows.as_array().cloned().unwrap_or_default().iter()
                    .map(|o| format!("{}: {}", tmpl(o.get("field_name")), tmpl(o.get("value_text"))))
                    .collect::<Vec<_>>().join("\n\n").chars().take(1500).collect();
                if !outs.is_empty() {
                    sections.push(format!("— ONE REAL OUTPUT UNDER THIS TEMPLATE (context) —\n{outs}"));
                }
            }
        }

        if evidence_rows == 0 && instructions.is_empty() {
            return Ok(json!({ "ok": true, "skipped": true, "reason": "no evidence in the selected sources and no operator direction — nothing to fix from" }));
        }
        ensure_draft_prompt(db)?;
        let evidence_joined = sections.join("\n\n");
        let operator_direction_val: String = if instructions.is_empty() { "(none — follow the evidence)".to_string() } else { instructions };
        let template_md_val: String = js_string(tpl.get("template_md").unwrap_or(&Value::Null)).chars().take(12000).collect();
        let evidence_val: String = if evidence_joined.is_empty() { "(no recorded evidence — operator direction only)".to_string() } else { evidence_joined };
        let ai_input = json!({
            "prompt_name": DRAFT_PROMPT,
            "input_kv": [
                { "key": "operator_direction", "value": operator_direction_val },
                { "key": "template_name", "value": tpl_name.clone() },
                { "key": "template_md", "value": template_md_val },
                { "key": "evidence", "value": evidence_val },
            ],
            "tier": "high", "temperature": "cold", "phase_flow": "single_only",
            "two_phase": false, "structured_output": false, "provider_fn": provider_fn,
        });
        // ── PARITY-DEFERRED from here: crate::ai_ai_call::aiCall always returns Err (see module doc).
        let res = crate::ai_ai_call::aiCall(&ai_input, db)?;

        let proposed = res.get("output_kv").and_then(Value::as_array).and_then(|a| {
            a.iter().find(|o| o.get("field_name").and_then(Value::as_str) == Some("revised_template_md"))
                .and_then(|o| o.get("value_text")).filter(|v| !v.is_null())
                .or_else(|| a.iter().find_map(|o| o.get("value_text").filter(|v| !v.is_null())))
        }).cloned();
        let proposed_str = proposed.as_ref().map(js_string).unwrap_or_default();
        if proposed_str.trim().is_empty() {
            return Ok(json!({ "ok": true, "skipped": true, "reason": "the surgeon returned an empty draft" }));
        }
        return Ok(json!({
            "ok": true, "drafted": true, "template_name": tpl_name,
            "current_template_md": tpl.get("template_md").cloned().unwrap_or(Value::Null),
            "current_system_md": tpl.get("system_md").cloned().unwrap_or(Value::Null),
            "proposed_template_md": proposed_str.trim(),
            "failures_used": evidence_rows,
            "sources_used": chosen,
            "draft_ai_call_id": res.get("ai_call_id").cloned().unwrap_or(Value::Null),
        }));
    }

    // DERIVE CRITERIA: one call decomposes the TEMPLATE'S OWN TEXT into decidable criteria,
    // stored on the current row (metadata, no version bump).
    if op == "derive_criteria" {
        let template_name = str_qq(input.get("template_name"));
        if template_name.is_empty() {
            return Err("heimdall-prompt-patch: template_name is required".to_string());
        }
        let cur_rows = crate::db_json::query_rows(
            db,
            "SELECT id, name, template_md, system_md FROM brain.prompt_templates WHERE name = ? AND superseded_by IS NULL ORDER BY id DESC LIMIT 1",
            &[json!(template_name)],
        )?;
        let Some(cur) = first(&cur_rows) else {
            return Ok(json!({ "ok": true, "skipped": true, "reason": format!("no active template named '{template_name}'") }));
        };
        let cur_id = cur.get("id").cloned().unwrap_or(Value::Null);
        let cur_name = js_string(cur.get("name").unwrap_or(&Value::Null));
        ensure_derive_prompt(db)?;

        let system_md = str_qq(cur.get("system_md"));
        let template_md = str_qq(cur.get("template_md"));
        let combined: String = if !system_md.is_empty() { format!("{system_md}\n\n{template_md}") } else { template_md };
        let combined: String = combined.chars().take(12000).collect();
        let ai_input = json!({
            "prompt_name": DERIVE_PROMPT,
            "input_kv": [
                { "key": "template_name", "value": cur_name.clone() },
                { "key": "template_md", "value": combined },
            ],
            "tier": "high", "temperature": "cold", "phase_flow": "single_only",
            "two_phase": false, "structured_output": false, "provider_fn": provider_fn,
        });
        // ── PARITY-DEFERRED from here: crate::ai_ai_call::aiCall always returns Err (see module doc).
        let res = crate::ai_ai_call::aiCall(&ai_input, db)?;

        let raw = res.get("output_kv").and_then(Value::as_array)
            .and_then(|a| a.iter().find_map(|o| o.get("value_text").filter(|v| !v.is_null())))
            .cloned().unwrap_or(json!(""));
        let raw_str = js_string(&raw);
        let re = regex::Regex::new(r"(?mi)^CRITERION\|(.+)$").unwrap();
        let criteria: Vec<String> = re.captures_iter(&raw_str)
            .map(|c| c[1].trim().chars().take(240).collect::<String>())
            .take(8)
            .collect();
        if criteria.is_empty() {
            return Ok(json!({ "ok": true, "skipped": true, "reason": "the decomposer returned no CRITERION lines", "derive_ai_call_id": res.get("ai_call_id").cloned().unwrap_or(Value::Null) }));
        }
        crate::db_json::run_write(
            db,
            "UPDATE brain.prompt_templates SET judging_criteria_json = ? WHERE id = ?",
            &[json!(serde_json::to_string(&criteria).map_err(|e| e.to_string())?), cur_id],
        )?;
        return Ok(json!({
            "ok": true, "derived": true, "template_name": cur_name, "criteria": criteria,
            "derive_ai_call_id": res.get("ai_call_id").cloned().unwrap_or(Value::Null),
        }));
    }

    if op == "apply" {
        let template_name = str_qq(input.get("template_name"));
        let new_template_md = str_qq(input.get("new_template_md"));
        if template_name.is_empty() {
            return Err("heimdall-prompt-patch: template_name is required".to_string());
        }
        if new_template_md.is_empty() || new_template_md.chars().count() < 5 {
            return Err("heimdall-prompt-patch: new_template_md is required".to_string());
        }
        let cur_rows = crate::db_json::query_rows(
            db,
            "SELECT * FROM brain.prompt_templates WHERE name = ? AND superseded_by IS NULL ORDER BY id DESC LIMIT 1",
            &[json!(template_name.clone())],
        )?;
        let Some(cur) = first(&cur_rows) else {
            return Ok(json!({ "ok": true, "skipped": true, "reason": format!("no active template named '{template_name}'") }));
        };
        let cur_id = cur.get("id").cloned().unwrap_or(Value::Null);
        // carry the frozen output contract forward verbatim.
        let field_rows = crate::db_json::query_rows(
            db,
            "SELECT field_name, field_type, enum_values, required, description,
                    min_length, max_length, required_sections_json, pattern, grounded_refs_json
             FROM brain.prompt_output_fields WHERE prompt_template_id = ? ORDER BY order_index",
            &[cur_id.clone()],
        )?;
        let mut fields: Vec<Value> = Vec::new();
        for f in field_rows.as_array().cloned().unwrap_or_default() {
            let mut o: Map<String, Value> = Map::new();
            o.insert("field_name".into(), f.get("field_name").cloned().unwrap_or(Value::Null));
            o.insert("field_type".into(), f.get("field_type").cloned().unwrap_or(Value::Null));
            if let Some(ev) = f.get("enum_values").and_then(Value::as_str).filter(|s| !s.is_empty()) {
                o.insert("enum_values".into(), json!(ev.split('|').collect::<Vec<&str>>()));
            }
            o.insert("required".into(), json!(truthy(f.get("required"))));
            if let Some(d) = f.get("description").filter(|v| !v.is_null()) { o.insert("description".into(), d.clone()); }
            if let Some(m) = f.get("min_length").filter(|v| !v.is_null()) { o.insert("min_length".into(), m.clone()); }
            if let Some(m) = f.get("max_length").filter(|v| !v.is_null()) { o.insert("max_length".into(), m.clone()); }
            if let Some(rs) = f.get("required_sections_json").and_then(Value::as_str).filter(|s| !s.is_empty()) {
                if let Ok(v) = serde_json::from_str::<Value>(rs) { o.insert("required_sections".into(), v); }
            }
            if let Some(p) = f.get("pattern").filter(|v| !v.is_null()) { o.insert("pattern".into(), p.clone()); }
            if let Some(gr) = f.get("grounded_refs_json").and_then(Value::as_str).filter(|s| !s.is_empty()) {
                if let Ok(v) = serde_json::from_str::<Value>(gr) { o.insert("grounded_refs".into(), v); }
            }
            fields.push(Value::Object(o));
        }

        let system_md_param = match input.get("new_system_md") {
            Some(v) => if truthy(Some(v)) { v.clone() } else { Value::Null },
            None => nz(cur.get("system_md")).cloned().unwrap_or(Value::Null),
        };
        let phase_flow_param = nz(cur.get("phase_flow")).cloned().unwrap_or(json!("single"));
        let cross_field_rules_param = match cur.get("cross_field_rules_json").and_then(Value::as_str).filter(|s| !s.is_empty()) {
            Some(s) => serde_json::from_str::<Value>(s).unwrap_or(Value::Null),
            None => Value::Null,
        };
        let judging_criteria_param = match cur.get("judging_criteria_json").and_then(Value::as_str).filter(|s| !s.is_empty()) {
            Some(s) => serde_json::from_str::<Value>(s).unwrap_or(Value::Null),
            None => Value::Null,
        };

        let add_input = json!({
            "name": cur.get("name").cloned().unwrap_or(Value::Null),
            "template_md": new_template_md,
            "system_md": system_md_param,
            "default_tier": cur.get("default_tier").cloned().unwrap_or(Value::Null),
            "default_temperature": cur.get("default_temperature").cloned().unwrap_or(Value::Null),
            "max_output_tokens": cur.get("max_output_tokens").cloned().unwrap_or(Value::Null),
            "phase_flow": phase_flow_param,
            "cross_field_rules": cross_field_rules_param,
            "judging_criteria": judging_criteria_param,
            "output_fields": fields,
        });
        // fully deterministic — the REAL (already-converted) promptAdd twin, not deferred.
        let r = crate::ai_prompt_add::promptAdd(&add_input, db)?;
        return Ok(json!({
            "ok": true, "applied": true,
            "name": r.get("name").cloned().unwrap_or(Value::Null),
            "version": r.get("version").cloned().unwrap_or(Value::Null),
            "previous_version": cur.get("version").cloned().unwrap_or(Value::Null),
        }));
    }

    Err(format!("heimdall-prompt-patch: op must be one of: draft, apply (got '{op}')"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCHEMA: &[&str] = &[
        "CREATE TABLE brain.prompt_templates ( id INTEGER PRIMARY KEY, name TEXT NOT NULL, version INTEGER NOT NULL, template_md TEXT NOT NULL, default_tier TEXT NOT NULL, default_temperature TEXT NOT NULL, max_output_tokens INTEGER NOT NULL DEFAULT 1024, superseded_by INTEGER, created_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')), cross_field_rules_json TEXT, judging_criteria_json TEXT, system_md TEXT, phase_flow TEXT NOT NULL DEFAULT 'single', UNIQUE (name, version) )",
        "CREATE TABLE brain.prompt_output_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, grounded_refs_json TEXT, UNIQUE (prompt_template_id, field_name) )",
        "CREATE TABLE brain.prompt_input_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, UNIQUE (prompt_template_id, field_name) )",
        "CREATE TABLE brain.heimdall_human_tags (id INTEGER PRIMARY KEY AUTOINCREMENT, ai_call_id INTEGER, verdict INTEGER, reason_md TEXT)",
        "CREATE TABLE brain.heimdall_castings (id INTEGER PRIMARY KEY AUTOINCREMENT, ai_call_id INTEGER, template_base TEXT)",
        "CREATE TABLE brain.heimdall_ai_tags (id INTEGER PRIMARY KEY AUTOINCREMENT, target_ai_call_id INTEGER, superseded_by_human INTEGER NOT NULL DEFAULT 0, verdict INTEGER, failure_class TEXT, reason_md TEXT)",
        "CREATE TABLE brain.heimdall_prompt_tags (id INTEGER PRIMARY KEY AUTOINCREMENT, template_name TEXT, source TEXT, verdict INTEGER, failure_class TEXT, reason_md TEXT)",
        "CREATE TABLE brain.ai_call_attempts (id INTEGER PRIMARY KEY AUTOINCREMENT, ai_call_id INTEGER, validation_errors_json TEXT)",
        "CREATE TABLE brain.ai_calls (id INTEGER PRIMARY KEY AUTOINCREMENT, prompt_template_id INTEGER, cached INTEGER NOT NULL DEFAULT 0, outcome_class TEXT)",
        "CREATE TABLE brain.ai_call_outputs (id INTEGER PRIMARY KEY AUTOINCREMENT, ai_call_id INTEGER, field_name TEXT, value_text TEXT, order_index INTEGER)",
    ];

    fn setup() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS brain;").unwrap();
        for s in SCHEMA { c.execute_batch(s).unwrap(); }
        c
    }

    fn a_template(c: &Connection, name: &str, template_md: &str) -> i64 {
        c.execute(
            "INSERT INTO brain.prompt_templates (name, version, template_md, default_tier, default_temperature, max_output_tokens, phase_flow) VALUES (?, 1, ?, 'mid', 'cold', 1024, 'single')",
            rusqlite::params![name, template_md],
        ).unwrap();
        c.last_insert_rowid()
    }

    #[test]
    fn draft_requires_template_base() {
        let c = setup();
        let e = heimdallPromptPatch(&json!({ "op": "draft" }), &c).unwrap_err();
        assert_eq!(e, "heimdall-prompt-patch: template_base is required");
    }

    #[test]
    fn draft_skips_when_no_active_template() {
        let c = setup();
        let r = heimdallPromptPatch(&json!({ "op": "draft", "template_base": "chat" }), &c).unwrap();
        assert_eq!(r, json!({ "ok": true, "skipped": true, "reason": "no active template for base 'chat'" }));
    }

    #[test]
    fn draft_skips_when_no_evidence_and_no_instructions() {
        let c = setup();
        a_template(&c, "chat", "Say {{x}}.");
        let r = heimdallPromptPatch(&json!({ "op": "draft", "template_base": "chat" }), &c).unwrap();
        assert_eq!(r, json!({ "ok": true, "skipped": true, "reason": "no evidence in the selected sources and no operator direction — nothing to fix from" }));
    }

    #[test]
    fn draft_reaches_the_parity_deferred_aicall_boundary_with_instructions_only() {
        let c = setup();
        a_template(&c, "chat", "Say {{x}}.");
        let err = heimdallPromptPatch(&json!({ "op": "draft", "template_base": "chat", "instructions": "make it shorter" }), &c).unwrap_err();
        assert!(err.contains("ai-call"), "unexpected error: {err}");
        // the draft prompt WAS ensured (registered) before the deferred call
        let n: i64 = c.query_row("SELECT COUNT(*) FROM brain.prompt_templates WHERE name = 'heimdall.prompt.patch.v1'", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn draft_gathers_evidence_from_every_source_before_the_deferred_call() {
        let c = setup();
        let tpl_id = a_template(&c, "chat", "Say {{x}}.");
        c.execute("INSERT INTO brain.heimdall_castings (ai_call_id, template_base) VALUES (1, 'chat')", []).unwrap();
        c.execute("INSERT INTO brain.heimdall_human_tags (ai_call_id, verdict, reason_md) VALUES (1, -1, 'too verbose')", []).unwrap();
        c.execute("INSERT INTO brain.heimdall_castings (ai_call_id, template_base) VALUES (2, 'chat')", []).unwrap();
        c.execute("INSERT INTO brain.heimdall_ai_tags (target_ai_call_id, superseded_by_human, verdict, failure_class, reason_md) VALUES (2, 0, -1, 'hallucination', 'made facts up')", []).unwrap();
        c.execute("INSERT INTO brain.heimdall_prompt_tags (template_name, source, verdict, failure_class, reason_md) VALUES ('chat', 'operator', -1, NULL, 'ambiguous wording')", []).unwrap();
        c.execute("INSERT INTO brain.ai_calls (id, prompt_template_id, cached, outcome_class) VALUES (10, ?, 0, 'ok')", [tpl_id]).unwrap();
        c.execute("INSERT INTO brain.ai_call_attempts (ai_call_id, validation_errors_json) VALUES (10, '[\"missing field x\"]')", []).unwrap();
        c.execute("INSERT INTO brain.ai_calls (id, prompt_template_id, cached, outcome_class) VALUES (11, ?, 0, 'truncated')", [tpl_id]).unwrap();
        c.execute("INSERT INTO brain.ai_call_outputs (ai_call_id, field_name, value_text, order_index) VALUES (11, 'message', 'hi there', 0)", []).unwrap();
        let err = heimdallPromptPatch(&json!({ "op": "draft", "template_base": "chat" }), &c).unwrap_err();
        assert!(err.contains("ai-call"), "unexpected error: {err}");
    }

    #[test]
    fn derive_criteria_requires_template_name_and_skips_when_missing() {
        let c = setup();
        let e = heimdallPromptPatch(&json!({ "op": "derive_criteria" }), &c).unwrap_err();
        assert_eq!(e, "heimdall-prompt-patch: template_name is required");
        let r = heimdallPromptPatch(&json!({ "op": "derive_criteria", "template_name": "nope" }), &c).unwrap();
        assert_eq!(r, json!({ "ok": true, "skipped": true, "reason": "no active template named 'nope'" }));
    }

    #[test]
    fn derive_criteria_reaches_the_parity_deferred_aicall_boundary() {
        let c = setup();
        a_template(&c, "chat", "Say {{x}}.");
        let err = heimdallPromptPatch(&json!({ "op": "derive_criteria", "template_name": "chat" }), &c).unwrap_err();
        assert!(err.contains("ai-call"), "unexpected error: {err}");
        let n: i64 = c.query_row("SELECT COUNT(*) FROM brain.prompt_templates WHERE name = 'heimdall.criteria.derive.v1'", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1);
        // the target row's judging_criteria_json is untouched (aiCall never succeeded)
        let jc: Option<String> = c.query_row("SELECT judging_criteria_json FROM brain.prompt_templates WHERE name = 'chat'", [], |r| r.get(0)).unwrap();
        assert_eq!(jc, None);
    }

    #[test]
    fn criterion_line_regex_extracts_up_to_eight_trimmed_lines() {
        let raw = "some reasoning\ncriterion|first one\nCRITERION| second one \nCRITERION|third\nnot a line";
        let re = regex::Regex::new(r"(?mi)^CRITERION\|(.+)$").unwrap();
        let criteria: Vec<String> = re.captures_iter(raw).map(|c| c[1].trim().chars().take(240).collect::<String>()).take(8).collect();
        assert_eq!(criteria, vec!["first one".to_string(), "second one".to_string(), "third".to_string()]);
    }

    #[test]
    fn apply_requires_template_name_and_new_template_md() {
        let c = setup();
        assert_eq!(heimdallPromptPatch(&json!({ "op": "apply", "new_template_md": "hello world" }), &c).unwrap_err(), "heimdall-prompt-patch: template_name is required");
        assert_eq!(heimdallPromptPatch(&json!({ "op": "apply", "template_name": "chat" }), &c).unwrap_err(), "heimdall-prompt-patch: new_template_md is required");
        assert_eq!(heimdallPromptPatch(&json!({ "op": "apply", "template_name": "chat", "new_template_md": "hi" }), &c).unwrap_err(), "heimdall-prompt-patch: new_template_md is required");
    }

    #[test]
    fn apply_skips_when_no_active_template() {
        let c = setup();
        let r = heimdallPromptPatch(&json!({ "op": "apply", "template_name": "nope", "new_template_md": "hello world" }), &c).unwrap();
        assert_eq!(r, json!({ "ok": true, "skipped": true, "reason": "no active template named 'nope'" }));
    }

    #[test]
    fn apply_bumps_version_and_carries_output_fields_forward() {
        let c = setup();
        let tpl_id = a_template(&c, "chat", "Say {{x}}.");
        c.execute(
            "INSERT INTO brain.prompt_output_fields (prompt_template_id, field_name, field_type, enum_values, required, order_index, min_length, max_length, required_sections_json, pattern, grounded_refs_json) VALUES (?, 'verdict', 'enum', 'pass|fail', 1, 0, NULL, NULL, NULL, '^(pass|fail)$', '[\"card_slug\"]')",
            [tpl_id],
        ).unwrap();
        let r = heimdallPromptPatch(&json!({ "op": "apply", "template_name": "chat", "new_template_md": "Say {{x}} politely." }), &c).unwrap();
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["applied"], json!(true));
        assert_eq!(r["name"], json!("chat"));
        assert_eq!(r["version"], json!(2));
        assert_eq!(r["previous_version"], json!(1));
        let n: i64 = c.query_row("SELECT COUNT(*) FROM brain.prompt_templates WHERE name = 'chat'", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 2);
        let superseded_by: Option<i64> = c.query_row("SELECT superseded_by FROM brain.prompt_templates WHERE id = ?", [tpl_id], |r| r.get(0)).unwrap();
        assert!(superseded_by.is_some());
        let ev: String = c.query_row("SELECT enum_values FROM brain.prompt_output_fields WHERE field_name = 'verdict' AND prompt_template_id = (SELECT id FROM brain.prompt_templates WHERE name = 'chat' AND superseded_by IS NULL)", [], |r| r.get(0)).unwrap();
        assert_eq!(ev, "pass|fail");
    }

    #[test]
    fn apply_defaults_new_system_md_from_current_when_absent() {
        let c = setup();
        c.execute(
            "INSERT INTO brain.prompt_templates (name, version, template_md, default_tier, default_temperature, max_output_tokens, phase_flow, system_md) VALUES ('chat', 1, 'Say {{x}}.', 'mid', 'cold', 1024, 'single', 'You are nice.')",
            [],
        ).unwrap();
        c.execute(
            "INSERT INTO brain.prompt_output_fields (prompt_template_id, field_name, field_type, required, order_index) VALUES (1, 'message', 'string', 1, 0)",
            [],
        ).unwrap();
        heimdallPromptPatch(&json!({ "op": "apply", "template_name": "chat", "new_template_md": "Say {{x}} kindly." }), &c).unwrap();
        let sys: Option<String> = c.query_row("SELECT system_md FROM brain.prompt_templates WHERE name = 'chat' AND superseded_by IS NULL", [], |r| r.get(0)).unwrap();
        assert_eq!(sys, Some("You are nice.".to_string()));
    }

    #[test]
    fn unknown_op_errors_with_only_draft_and_apply_listed() {
        let c = setup();
        let e = heimdallPromptPatch(&json!({ "op": "bogus" }), &c).unwrap_err();
        assert_eq!(e, "heimdall-prompt-patch: op must be one of: draft, apply (got 'bogus')");
        let e2 = heimdallPromptPatch(&json!({}), &c).unwrap_err();
        assert_eq!(e2, "heimdall-prompt-patch: op must be one of: draft, apply (got '')");
    }
}
