//! prompt-section-improve.rs — twin of skills/heimdall/prompt-section-improve.mjs (db lane,
//! brain-attached). NAMES ARE CONTRACT: default `promptSectionImprove`; named exports
//! `splitSections`/`reassemble` (pure template-section (de)composition). Human prompt tagging
//! that REDESIGNS instead of ranking: `preview` regenerates ONE weak section (AI, nothing saved),
//! `apply` copy-bumps a new template version from the reviewed `new_template_md` (deterministic,
//! no AI).
//!
//! PARITY-DEFERRED: `preview`'s AI call is dispatched through `crate::ai_ai_call::aiCall`, the
//! deferred network-AI stub (see ai-call.rs's module doc) — it always returns `Err`. The JS wraps
//! this ONE `await aiCall(...)` in its own `try { } catch (e) { return {ok:false, reason:
//! \`ai_call_failed: ...\`} } ` — a failed/absent provider degrades GRACEFULLY, it never throws
//! (same CATCH contract as file-summarize.rs / codebase). So this twin mirrors that exactly: on
//! `aiCall` failure it returns `Ok({ok:false, reason: "ai_call_failed: <deferred message,
//! truncated to 200 chars>"})`, never propagating the deferred stub's `Err` up to the caller.
//! Everything else is the real, fully ported deterministic surface: op/guard validation,
//! `splitSections`/`reassemble` (own pure exported fns), section-index resolution, weakness-class
//! filtering, the `purpose` block assembly, `ensure_prompt` (reusing the already-converted
//! `ai_prompt_find`/`ai_prompt_add` twins, mirroring book-run.rs's `ensure_faculty`), and `apply`'s
//! `bump_version` copy-bump (fully deterministic, no AI at all — ported byte-for-byte including
//! its nested-transaction savepoint fallback, same pattern as prompt-add.rs's own `promptAdd`).
//!
//! The response-parsing/output-shaping helpers that run AFTER a successful `aiCall` —
//! `pick_improved` (picks the first non-null `value_text` from `output_kv`) and `finish_preview`
//! (strip code-fence/heading echo, the placeholder_drift SAFETY check, and the final
//! `new_template_md` assembly) — are ported as their own PURE functions and unit-tested directly
//! with canned values (same pattern as book-run.rs's `parse_arr`/`parse_obj` and pl-research.rs's
//! `build_research_report`/`build_sources`): unreachable from `promptSectionImprove` itself since
//! the deferred stub's `Err` is always caught first, but correct by construction.
// QUALITY: mixed unit — fully deterministic `apply` path (db write, no AI) + deterministic
// `preview` surface up to the deferred aiCall boundary (fixture-reachable) + 2 pure response-shape
// helpers unreachable from the default export, unit-tested directly (no fixture/frontier for those
// two) + 2 pure named-export helpers (splitSections/reassemble, fully fixture-reachable, no AI).

use regex::Regex;
use rusqlite::Connection;
use serde_json::{json, Value};
use std::collections::HashSet;
use std::sync::OnceLock;

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
fn js_string(v: &Value) -> String {
    match v {
        Value::Null => "null".into(),
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Array(a) => a.iter().map(js_string).collect::<Vec<_>>().join(","),
        Value::Object(_) => "[object Object]".into(),
    }
} // = JS String(v)
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

const IMPROVE_PROMPT: &str = "improve.prompt.section.v1";
// the design-weakness taxonomy the operator picks from (same chips as the AI prompt-auditor).
const WEAKNESS_CLASSES: [&str; 7] = ["ambiguous", "missing-context", "contradictory", "overlong", "wrong-constraints", "format-trap", "other"];

fn header_regex() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"^(?:═+.*|#{1,4}\s+\S.*|─{3,}.*)$").unwrap())
}
fn placeholder_regex() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"\{\{\s*(\w+)\s*\}\}").unwrap())
}
fn fence_open_regex() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"^```[\w-]*\n?").unwrap())
}
fn fence_close_regex() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"\n?```\s*$").unwrap())
}

// `[...new Set([...String(s ?? '').matchAll(/\{\{\s*(\w+)\s*\}\}/g)].map(m => m[1]))]` — dedup
// preserving first-appearance order (JS `Set` iteration order).
fn placeholders_of(s: &str) -> Vec<String> {
    let re = placeholder_regex();
    let mut seen: HashSet<String> = HashSet::new();
    let mut out = Vec::new();
    for cap in re.captures_iter(s) {
        let name = cap[1].to_string();
        if seen.insert(name.clone()) {
            out.push(name);
        }
    }
    out
}

// Split a template into ordered segments, each carrying its full raw text so that
// reassemble(splitSections(x)) === x. The preamble before the first header is index 0
// (heading null). NAMES ARE CONTRACT — exported like the JS.
pub fn splitSections(template_md: &Value) -> Value {
    let s = str_qq(Some(template_md));
    let re = header_regex();
    struct Seg {
        heading: Option<String>,
        lines: Vec<String>,
    }
    fn flush(segs: &mut Vec<Seg>, seg: Seg) {
        if !seg.lines.is_empty() || seg.heading.is_some() {
            segs.push(seg);
        }
    }
    let mut segs: Vec<Seg> = Vec::new();
    let mut cur = Seg { heading: None, lines: Vec::new() };
    for ln in s.split('\n') {
        if re.is_match(ln) {
            let old = std::mem::replace(&mut cur, Seg { heading: Some(ln.trim().to_string()), lines: vec![ln.to_string()] });
            flush(&mut segs, old);
        } else {
            cur.lines.push(ln.to_string());
        }
    }
    flush(&mut segs, cur);
    let arr: Vec<Value> = segs
        .into_iter()
        .enumerate()
        .map(|(i, seg)| {
            let has_heading = seg.heading.is_some();
            let body_lines: &[String] = if has_heading { &seg.lines[1..] } else { &seg.lines[..] };
            json!({
                "index": i as i64,
                "heading": seg.heading.clone().map(Value::String).unwrap_or(Value::Null),
                "body": body_lines.join("\n"),
                "text": seg.lines.join("\n"),
            })
        })
        .collect();
    Value::Array(arr)
}

pub fn reassemble(sections: &Value) -> Value {
    let text = sections
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|s| s.get("text").and_then(Value::as_str).unwrap_or("").to_string())
        .collect::<Vec<_>>()
        .join("\n");
    json!(text)
}

const SYSTEM: &str = "You are a PRECISE PROMPT ENGINEER improving ONE section of a template used inside an autonomous software substrate. You rewrite only the section you are given, keep it consistent with the rest of the prompt, and fix exactly the named weaknesses. You never add commentary and you never touch a {{placeholder}}.";

const TEMPLATE: &str = "Improve ONE section of a prompt template. Output ONLY the improved section body — no heading line, no fences, no explanation.

═══ WHAT THE WHOLE PROMPT IS FOR ═══
{{purpose}}

═══ THE FULL PROMPT (context — do NOT reproduce it) ═══
{{full_template}}

═══ THE SECTION TO IMPROVE (heading: {{section_heading}}) ═══
{{section_body}}

═══ WHY IT IS WEAK (fix EXACTLY these) ═══
{{weaknesses}}
{{operator_note}}

RULES:
- Rewrite ONLY this section's body. Keep it doing its job for the whole prompt.
- Preserve EVERY {{placeholder}} token exactly — dropping or renaming one breaks the template.
- Fix the named weaknesses; do not \"improve\" things that were fine. Keep it economical.
- Output the improved body text ONLY (no heading, no code fence, no preamble).";

// try promptFind; on miss OR drifted template, promptAdd (no re-verify — mirrors JS ensurePrompt).
fn ensure_prompt(db: &Connection) -> Result<(), String> {
    if let Ok(existing) = crate::ai_prompt_find::promptFind(&json!({ "name": IMPROVE_PROMPT }), db) {
        if existing.get("template_md").and_then(Value::as_str) == Some(TEMPLATE) {
            return Ok(());
        }
    }
    crate::ai_prompt_add::promptAdd(
        &json!({
            "name": IMPROVE_PROMPT,
            "template_md": TEMPLATE,
            "system_md": SYSTEM,
            "default_tier": "high",
            "default_temperature": "cold",
            "max_output_tokens": 2000,
            "output_fields": [{ "field_name": "improved_section", "field_type": "string", "description": "the rewritten section BODY only — no heading, no fence" }],
            "judging_criteria": [
                "Outputs only the section body — no heading line, no code fence, no commentary.",
                "Every {{placeholder}} from the original is preserved verbatim.",
                "The named weaknesses are addressed without rewriting parts that were fine.",
            ],
        }),
        db,
    )?;
    Ok(())
}

fn active_template(db: &Connection, name: &str) -> Result<Option<Value>, String> {
    let rows = crate::db_json::query_rows(
        db,
        "SELECT * FROM brain.prompt_templates WHERE name = ? AND superseded_by IS NULL ORDER BY id DESC LIMIT 1",
        &[json!(name)],
    )?;
    Ok(rows.as_array().and_then(|a| a.first()).cloned())
}

// copy-bump: a faithful new version that changes ONLY template_md. Copies every column + the
// output/input field rows verbatim (dynamic column enumeration off `SELECT *`'s row-object key
// order, same convention as query_rows elsewhere), then supersedes the old row. Ported
// byte-for-byte from the JS including its nested-transaction savepoint fallback (mirrors
// prompt-add.rs's `promptAdd`, whose comment explains why: BEGIN failing is unreachable on a
// fresh connection in the lane, but the fallback is ported anyway for parity).
fn bump_version(db: &Connection, name: &str, new_template_md: &str) -> Result<i64, String> {
    let cur = active_template(db, name)?
        .ok_or_else(|| format!("prompt-section-improve: active prompt '{name}' 'undefined' not found"))?;
    let cur_obj = cur.as_object().cloned().unwrap_or_default();
    let cols: Vec<String> = cur_obj
        .keys()
        .filter(|c| c.as_str() != "id" && c.as_str() != "version" && c.as_str() != "template_md" && c.as_str() != "superseded_by")
        .cloned()
        .collect();
    let cur_version = cur_obj.get("version").and_then(Value::as_i64).unwrap_or(0);
    let new_version = cur_version + 1;
    let cur_id = cur_obj.get("id").cloned().unwrap_or(Value::Null);

    let nested = db.execute_batch("BEGIN").is_err();
    if nested {
        db.execute_batch("SAVEPOINT psi_sp").map_err(|e| e.to_string())?;
    }
    let body = || -> Result<i64, String> {
        let mut insert_cols: Vec<String> = vec!["name".to_string(), "version".to_string(), "template_md".to_string()];
        insert_cols.extend(cols.iter().cloned());
        let mut values: Vec<Value> = vec![json!(name), json!(new_version), json!(new_template_md)];
        for c in &cols {
            values.push(cur_obj.get(c).cloned().unwrap_or(Value::Null));
        }
        let placeholders = insert_cols.iter().map(|_| "?").collect::<Vec<_>>().join(", ");
        let sql = format!("INSERT INTO brain.prompt_templates ({}) VALUES ({})", insert_cols.join(", "), placeholders);
        let (_n, new_id) = crate::db_json::run_write(db, &sql, &values)?;

        for tbl in ["prompt_output_fields", "prompt_input_fields"] {
            let sel_sql = format!("SELECT * FROM brain.{tbl} WHERE prompt_template_id = ?");
            let rows = crate::db_json::query_rows(db, &sel_sql, &[cur_id.clone()]).unwrap_or(json!([]));
            for r in rows.as_array().cloned().unwrap_or_default() {
                let robj = r.as_object().cloned().unwrap_or_default();
                let fcols: Vec<String> = robj.keys().filter(|c| c.as_str() != "id").cloned().collect();
                let fvals: Vec<Value> = fcols
                    .iter()
                    .map(|c| if c == "prompt_template_id" { json!(new_id) } else { robj.get(c).cloned().unwrap_or(Value::Null) })
                    .collect();
                let ph = fcols.iter().map(|_| "?").collect::<Vec<_>>().join(", ");
                let isql = format!("INSERT INTO brain.{tbl} ({}) VALUES ({})", fcols.join(", "), ph);
                crate::db_json::run_write(db, &isql, &fvals)?;
            }
        }
        crate::db_json::run_write(db, "UPDATE brain.prompt_templates SET superseded_by = ? WHERE id = ?", &[json!(new_id), cur_id.clone()])?;
        if nested {
            db.execute_batch("RELEASE SAVEPOINT psi_sp").map_err(|e| e.to_string())?;
        } else {
            db.execute_batch("COMMIT").map_err(|e| e.to_string())?;
        }
        Ok(new_version)
    };
    match body() {
        Ok(v) => Ok(v),
        Err(e) => {
            let _ = if nested {
                db.execute_batch("ROLLBACK TO SAVEPOINT psi_sp; RELEASE SAVEPOINT psi_sp")
            } else {
                db.execute_batch("ROLLBACK")
            };
            Err(e)
        }
    }
}

// JS `idx = Number(input?.section_index); sections[idx]` — real-array property lookup: only an
// exact non-negative integer index within bounds resolves (NaN/fractional/out-of-range → None,
// matching `undefined`).
fn int_index(n: f64, len: usize) -> Option<usize> {
    if !n.is_finite() || n.fract() != 0.0 || n < 0.0 {
        return None;
    }
    let i = n as usize;
    if i < len { Some(i) } else { None }
}

// `(res?.output_kv ?? []).find(o => o.value_text != null)?.value_text ?? ''` — pure, unreachable
// from `promptSectionImprove` itself (the deferred aiCall's Err is always caught first), unit-
// tested directly with canned `ai_result` values.
fn pick_improved(res: &Value) -> String {
    let arr = res.get("output_kv").and_then(Value::as_array).cloned().unwrap_or_default();
    arr.iter()
        .find(|o| o.get("value_text").map_or(false, |v| !v.is_null()))
        .and_then(|o| o.get("value_text").and_then(Value::as_str))
        .unwrap_or("")
        .to_string()
}

// `improved.replace(/^```[\w-]*\n?/, '').replace(/\n?```\s*$/, '').trim();` then, if the section
// has a heading and the text echoes it back, strip that heading line. Pure.
fn strip_fence_and_heading(raw: &str, heading: Option<&str>) -> String {
    let s = fence_open_regex().replace(raw, "");
    let s = fence_close_regex().replace(&s, "");
    let mut improved = s.trim().to_string();
    if let Some(h) = heading {
        if !h.is_empty() && improved.starts_with(h) {
            improved = improved.chars().skip(h.chars().count()).collect();
            if let Some(stripped) = improved.strip_prefix('\n') {
                improved = stripped.to_string();
            }
        }
    }
    improved
}

// Everything the JS does AFTER a successful aiCall response inside `op === 'preview'`: strip a
// fence/echoed heading, reject an empty regeneration, the placeholder_drift SAFETY check, and
// (on success) reassemble `new_template_md`. Pure — unreachable from `promptSectionImprove`
// itself (see module doc), unit-tested directly with canned inputs.
fn finish_preview(sections: &Value, idx: usize, target: &Value, raw_improved: &str, ai_call_id: &Value) -> Value {
    let heading = target.get("heading").and_then(Value::as_str);
    let improved = strip_fence_and_heading(raw_improved, heading);
    if improved.is_empty() {
        return json!({ "ok": false, "reason": "empty regeneration" });
    }
    let body = target.get("body").and_then(Value::as_str).unwrap_or("");
    let want = placeholders_of(body);
    let got: HashSet<String> = placeholders_of(&improved).into_iter().collect();
    let dropped: Vec<String> = want.into_iter().filter(|p| !got.contains(p)).collect();
    let heading_val = heading.map(|h| json!(h)).unwrap_or(Value::Null);
    if !dropped.is_empty() {
        return json!({
            "ok": false, "reason": "placeholder_drift", "dropped": dropped,
            "before": body, "after": improved,
            "section_heading": heading_val, "ai_call_id": ai_call_id.clone(),
        });
    }
    let new_text = match heading {
        Some(h) if !h.is_empty() => format!("{h}\n{improved}"),
        _ => improved.clone(),
    };
    let sections_arr = sections.as_array().cloned().unwrap_or_default();
    let new_sections: Vec<Value> = sections_arr
        .iter()
        .enumerate()
        .map(|(i, s)| {
            if i == idx {
                let mut o = s.as_object().cloned().unwrap_or_default();
                o.insert("text".into(), json!(new_text));
                Value::Object(o)
            } else {
                s.clone()
            }
        })
        .collect();
    let new_template_md = reassemble(&Value::Array(new_sections));
    json!({
        "ok": true, "section_index": idx as i64, "section_heading": heading_val,
        "before": body, "after": improved,
        "new_template_md": new_template_md, "ai_call_id": ai_call_id.clone(),
    })
}

pub fn promptSectionImprove(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS: `const { db, provider_fn = null } = context ?? {}; if (!db) throw err.contextMissing(...)`
    // — unreachable here (Rust's `db: &Connection` is never absent). provider_fn is a JS function
    // reference with no Rust equivalent to carry — threaded as `null` into aiCall's input, same
    // convention as book-run.rs / pl-research.rs.
    let provider_fn = Value::Null;
    let op = { let o = str_qq(input.get("op")); if o.is_empty() { "preview".to_string() } else { o } };
    let template_name = str_qq(input.get("template_name"));
    if template_name.is_empty() {
        return Err("prompt-section-improve: template_name is required".to_string());
    }

    if op == "apply" {
        let new_template_md = str_qq(input.get("new_template_md"));
        if new_template_md.chars().count() < 20 {
            return Err("prompt-section-improve: new_template_md is required".to_string());
        }
        let version = bump_version(db, &template_name, &new_template_md)?;
        return Ok(json!({ "ok": true, "applied": true, "template_name": template_name, "version": version }));
    }

    if op == "preview" {
        let tpl = match active_template(db, &template_name)? {
            Some(t) => t,
            None => return Ok(json!({ "ok": false, "reason": "template not found" })),
        };
        let template_md_val = tpl.get("template_md").cloned().unwrap_or(Value::Null);
        let sections = splitSections(&template_md_val);
        let sections_arr = sections.as_array().cloned().unwrap_or_default();
        let idx_num = js_number(input.get("section_index"));
        let idx = match int_index(idx_num, sections_arr.len()) {
            Some(i) => i,
            None => {
                let allowed: Vec<String> = (0..sections_arr.len() as i64).map(|i| i.to_string()).collect();
                return Err(format!(
                    "prompt-section-improve: section_index must be one of: {} (got '{}')",
                    allowed.join(", "),
                    tmpl(input.get("section_index"))
                ));
            }
        };
        let target = sections_arr[idx].clone();

        let classes: Vec<String> = input
            .get("weakness_classes")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .map(|c| js_string(&c))
            .filter(|c| WEAKNESS_CLASSES.contains(&c.as_str()))
            .collect();
        let note: String = str_qq(input.get("operator_note")).chars().take(800).collect::<String>().trim().to_string();
        let purpose_head = format!(
            "prompt: {} (tier {}/{})",
            template_name,
            str_qq(tpl.get("default_tier")),
            str_qq(tpl.get("default_temperature"))
        );
        let purpose_criteria = match tpl.get("judging_criteria_json").and_then(Value::as_str) {
            Some(s) if !s.is_empty() => match serde_json::from_str::<Value>(s) {
                Ok(Value::Array(a)) if !a.is_empty() => {
                    let joined = a.iter().map(js_string).collect::<Vec<_>>().join("; ");
                    format!("it must: {joined}")
                }
                _ => String::new(),
            },
            _ => String::new(),
        };
        let purpose = [purpose_head, purpose_criteria].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join("\n");

        ensure_prompt(db)?;

        let ai_input = json!({
            "prompt_name": IMPROVE_PROMPT,
            "input_kv": [
                { "key": "purpose", "value": if purpose.is_empty() { format!("prompt: {template_name}") } else { purpose } },
                { "key": "full_template", "value": str_qq(Some(&template_md_val)).chars().take(9000).collect::<String>() },
                { "key": "section_heading", "value": nz(target.get("heading")).cloned().unwrap_or(json!("(preamble)")) },
                { "key": "section_body", "value": target.get("body").cloned().unwrap_or(json!("")) },
                { "key": "weaknesses", "value": if classes.is_empty() { "- (unspecified — improve clarity + economy)".to_string() } else { classes.iter().map(|c| format!("- {c}")).collect::<Vec<_>>().join("\n") } },
                { "key": "operator_note", "value": if note.is_empty() { String::new() } else { format!("Operator note: {note}") } },
            ],
            "tier": "high", "temperature": "cold", "phase_flow": "single_only", "two_phase": false,
            "structured_output": false, "provider_fn": provider_fn,
        });
        let ai_result = match crate::ai_ai_call::aiCall(&ai_input, db) {
            Ok(r) => r,
            Err(e) => {
                let msg: String = e.chars().take(200).collect();
                return Ok(json!({ "ok": false, "reason": format!("ai_call_failed: {msg}") }));
            }
        };
        // unreachable in this twin (aiCall always Err) — ported for shape completeness; see
        // pick_improved/finish_preview's own unit tests for real coverage.
        let improved_raw = pick_improved(&ai_result);
        let ai_call_id = ai_result.get("ai_call_id").cloned().unwrap_or(Value::Null);
        return Ok(finish_preview(&sections, idx, &target, &improved_raw, &ai_call_id));
    }

    Err(format!("prompt-section-improve: op must be one of: preview, apply (got '{op}')"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCHEMA: &str = "
        CREATE TABLE brain.prompt_templates ( id INTEGER PRIMARY KEY, name TEXT NOT NULL, version INTEGER NOT NULL, template_md TEXT NOT NULL, default_tier TEXT NOT NULL CHECK (default_tier IN ('low','mid','high')), default_temperature TEXT NOT NULL CHECK (default_temperature IN ('freezed','cold','warm','hot','hell')), max_output_tokens INTEGER NOT NULL DEFAULT 1024, superseded_by INTEGER, created_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')), cross_field_rules_json TEXT, judging_criteria_json TEXT, system_md TEXT, phase_flow TEXT NOT NULL DEFAULT 'single', UNIQUE (name, version) );
        CREATE TABLE brain.prompt_output_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL CHECK (field_type IN ('string','number','boolean','enum','array_of_string','array_of_object')), enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, grounded_refs_json TEXT, UNIQUE (prompt_template_id, field_name) );
        CREATE TABLE brain.prompt_input_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL CHECK (field_type IN ('string','number','boolean','enum','array_of_string','array_of_object')), enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, UNIQUE (prompt_template_id, field_name) );
    ";

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS brain").unwrap();
        c.execute_batch(SCHEMA).unwrap();
        c
    }

    fn seed_template(c: &Connection, name: &str, template_md: &str) -> i64 {
        c.execute(
            "INSERT INTO brain.prompt_templates (name, version, template_md, default_tier, default_temperature, judging_criteria_json) VALUES (?, 1, ?, 'mid', 'cold', '[\"be concrete\"]')",
            rusqlite::params![name, template_md],
        )
        .unwrap();
        let id = c.last_insert_rowid();
        c.execute(
            "INSERT INTO brain.prompt_output_fields (prompt_template_id, field_name, field_type, order_index) VALUES (?, 'foo', 'string', 0)",
            [id],
        )
        .unwrap();
        id
    }

    // ── guards ───────────────────────────────────────────────────────────────────────────────
    #[test]
    fn template_name_is_required() {
        let c = db();
        let e = promptSectionImprove(&json!({}), &c).unwrap_err();
        assert_eq!(e, "prompt-section-improve: template_name is required");
    }

    #[test]
    fn unknown_op_errors() {
        let c = db();
        let e = promptSectionImprove(&json!({ "op": "bogus", "template_name": "x" }), &c).unwrap_err();
        assert_eq!(e, "prompt-section-improve: op must be one of: preview, apply (got 'bogus')");
    }

    // ── apply ────────────────────────────────────────────────────────────────────────────────
    #[test]
    fn apply_requires_new_template_md_at_least_20_chars() {
        let c = db();
        let e = promptSectionImprove(&json!({ "op": "apply", "template_name": "x", "new_template_md": "too short" }), &c).unwrap_err();
        assert_eq!(e, "prompt-section-improve: new_template_md is required");
    }

    #[test]
    fn apply_errors_when_no_active_prompt() {
        let c = db();
        let e = promptSectionImprove(&json!({ "op": "apply", "template_name": "ghost", "new_template_md": "a".repeat(25) }), &c).unwrap_err();
        assert_eq!(e, "prompt-section-improve: active prompt 'ghost' 'undefined' not found");
    }

    #[test]
    fn apply_copy_bumps_a_new_version_and_supersedes_the_old_row() {
        let c = db();
        let id = seed_template(&c, "greet", "## HDR\nold body");
        let new_md = "## HDR\nNEW BODY that is long enough to pass the guard.";
        let r = promptSectionImprove(&json!({ "op": "apply", "template_name": "greet", "new_template_md": new_md }), &c).unwrap();
        assert_eq!(r, json!({ "ok": true, "applied": true, "template_name": "greet", "version": 2 }));

        let (tmd, tier, temp, jcj, superseded): (String, String, String, String, Option<i64>) = c
            .query_row("SELECT template_md, default_tier, default_temperature, judging_criteria_json, superseded_by FROM brain.prompt_templates WHERE id = ?", [id], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?))
            })
            .unwrap();
        assert_eq!(tmd, "## HDR\nold body");
        assert!(superseded.is_some());

        let new_id: i64 = c.query_row("SELECT id FROM brain.prompt_templates WHERE superseded_by IS NULL AND name = 'greet'", [], |r| r.get(0)).unwrap();
        let (new_tmd, new_tier, new_temp, new_jcj): (String, String, String, String) = c
            .query_row("SELECT template_md, default_tier, default_temperature, judging_criteria_json FROM brain.prompt_templates WHERE id = ?", [new_id], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
            })
            .unwrap();
        assert_eq!(new_tmd, new_md);
        assert_eq!((new_tier.as_str(), new_temp.as_str()), (tier.as_str(), temp.as_str()));
        assert_eq!(new_jcj, jcj);

        let n_out: i64 = c.query_row("SELECT COUNT(*) FROM brain.prompt_output_fields WHERE prompt_template_id = ?", [new_id], |r| r.get(0)).unwrap();
        assert_eq!(n_out, 1);
        let field_name: String = c.query_row("SELECT field_name FROM brain.prompt_output_fields WHERE prompt_template_id = ?", [new_id], |r| r.get(0)).unwrap();
        assert_eq!(field_name, "foo");
    }

    // ── preview: guards + AI boundary ──────────────────────────────────────────────────────────
    #[test]
    fn preview_reports_template_not_found() {
        let c = db();
        let r = promptSectionImprove(&json!({ "op": "preview", "template_name": "ghost", "section_index": 0 }), &c).unwrap();
        assert_eq!(r, json!({ "ok": false, "reason": "template not found" }));
    }

    #[test]
    fn preview_rejects_out_of_range_section_index() {
        let c = db();
        seed_template(&c, "greet", "## A\nbody a\n## B\nbody b");
        let e = promptSectionImprove(&json!({ "op": "preview", "template_name": "greet", "section_index": 5 }), &c).unwrap_err();
        assert_eq!(e, "prompt-section-improve: section_index must be one of: 0, 1 (got '5')");
        // absent section_index → Number(undefined) = NaN → same guard
        let e2 = promptSectionImprove(&json!({ "op": "preview", "template_name": "greet" }), &c).unwrap_err();
        assert_eq!(e2, "prompt-section-improve: section_index must be one of: 0, 1 (got 'undefined')");
        // fractional index never resolves to a real array slot either
        let e3 = promptSectionImprove(&json!({ "op": "preview", "template_name": "greet", "section_index": 0.5 }), &c).unwrap_err();
        assert_eq!(e3, "prompt-section-improve: section_index must be one of: 0, 1 (got '0.5')");
    }

    #[test]
    fn preview_reaches_the_parity_deferred_aicall_boundary() {
        let c = db();
        seed_template(&c, "greet", "## HDR\nold body {{who}}");
        let r = promptSectionImprove(&json!({ "op": "preview", "template_name": "greet", "section_index": 0 }), &c).unwrap();
        assert_eq!(r["ok"], json!(false));
        let reason = r["reason"].as_str().unwrap();
        assert!(reason.starts_with("ai_call_failed:"), "unexpected reason: {reason}");
        // ensure_prompt DID run (deterministic surface reached the AI boundary).
        let n: i64 = c.query_row("SELECT COUNT(*) FROM brain.prompt_templates WHERE name = 'improve.prompt.section.v1'", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1);
    }

    // ── splitSections / reassemble ─────────────────────────────────────────────────────────────
    #[test]
    fn split_sections_then_reassemble_roundtrips() {
        let md = "preamble line\n## Section One\nbody one\nmore\n─── rule ───\nbody two\n═══ HDR ═══\nbody three";
        let sections = splitSections(&json!(md));
        let arr = sections.as_array().unwrap();
        assert_eq!(arr.len(), 4);
        assert_eq!(arr[0]["heading"], Value::Null);
        assert_eq!(arr[0]["body"], json!("preamble line"));
        assert_eq!(arr[1]["heading"], json!("## Section One"));
        assert_eq!(arr[1]["body"], json!("body one\nmore"));
        let roundtrip = reassemble(&sections);
        assert_eq!(roundtrip, json!(md));
    }

    #[test]
    fn split_sections_on_empty_template_yields_one_empty_preamble_segment() {
        // `String(null ?? '').split('\n')` = [''] in JS too — one empty preamble segment, not [].
        let sections = splitSections(&Value::Null);
        assert_eq!(sections, json!([{ "index": 0, "heading": null, "body": "", "text": "" }]));
    }

    // ── pure helpers: placeholders_of / pick_improved / finish_preview ────────────────────────
    #[test]
    fn placeholders_of_dedupes_preserving_first_appearance_order() {
        assert_eq!(placeholders_of("{{a}} text {{b}} more {{a}}"), vec!["a".to_string(), "b".to_string()]);
        assert_eq!(placeholders_of("no placeholders here"), Vec::<String>::new());
    }

    #[test]
    fn pick_improved_finds_first_non_null_value_text() {
        let res = json!({ "output_kv": [
            { "field_name": "other", "value_text": null },
            { "field_name": "improved_section", "value_text": "the new body" },
        ] });
        assert_eq!(pick_improved(&res), "the new body");
        assert_eq!(pick_improved(&json!({})), "");
    }

    #[test]
    fn finish_preview_rejects_empty_regeneration() {
        let sections = splitSections(&json!("## HDR\nold {{x}}"));
        let target = sections[0].clone();
        let r = finish_preview(&sections, 0, &target, "```\n\n```", &Value::Null);
        assert_eq!(r, json!({ "ok": false, "reason": "empty regeneration" }));
    }

    #[test]
    fn finish_preview_rejects_dropped_placeholder() {
        let sections = splitSections(&json!("## HDR\nold {{x}} {{y}}"));
        let target = sections[0].clone();
        let r = finish_preview(&sections, 0, &target, "new body with only {{x}}", &json!(42));
        assert_eq!(r["ok"], json!(false));
        assert_eq!(r["reason"], json!("placeholder_drift"));
        assert_eq!(r["dropped"], json!(["y"]));
        assert_eq!(r["ai_call_id"], json!(42));
    }

    #[test]
    fn finish_preview_succeeds_strips_fence_and_echoed_heading_rebuilds_template() {
        let sections = splitSections(&json!("preamble\n## HDR\nold {{x}}\n## OTHER\nunrelated"));
        let target = sections[1].clone();
        let raw = "```\n## HDR\nnew improved {{x}} body\n```";
        let r = finish_preview(&sections, 1, &target, raw, &json!(7));
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["section_index"], json!(1));
        assert_eq!(r["section_heading"], json!("## HDR"));
        assert_eq!(r["after"], json!("new improved {{x}} body"));
        assert_eq!(r["ai_call_id"], json!(7));
        assert_eq!(r["new_template_md"], json!("preamble\n## HDR\nnew improved {{x}} body\n## OTHER\nunrelated"));
    }
}
