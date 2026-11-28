//! heimdall-ai-tag.rs — twin of skills/heimdall/heimdall-ai-tag.mjs (db, cross-db brain.*). NAMES
//! ARE CONTRACT: default `heimdallAiTag`; named export `parseJudgeLine` (pure). Given a target
//! ai_call, a high-tier judge model rates its OUTPUT against its INPUT + the gate's declared
//! criteria on a −2..+2 scale — the AI half of Heimdall's tagging (HUMAN > AI: skips human-tagged
//! calls; a later human tag supersedes via heimdall-tag-add's own path).
//!
//! PARITY-DEFERRED: `run_seat` (the per-panel-seat judge call) ends in `crate::ai_ai_call::aiCall`
//! (the deferred network-AI stub — see ai-call.rs's module doc) and PROPAGATES its Err (JS: no
//! try/catch around `await aiCall(...)` inside `runSeat`, and `heimdallAiTag`'s own
//! `for (... ) seats.push(await runSeat(s))` loop has no try/catch either — an aiCall failure
//! throws straight out of the skill). Everything upstream of the first `run_seat` call is real and
//! IS the deterministic surface these tests exercise: the ai_call_id guard, HUMAN>AI skip,
//! already-tagged skip (force override), `target_context` (call/cache/inputs/outputs/casting
//! assembly, reusing `ai_ai_call::reconstructRenderedPrompt`), the category lookup, `ensure_prompt`
//! (reusing the already-converted `ai_prompt_find`/`ai_prompt_add` twins), the gate-criteria
//! best-effort read, and the deterministic input_kv assembly fed to the first seat. `parse_judge_line`
//! (pure, no db) and the post-seat consolidation/persistence logic (median verdict, panel agreement,
//! the upsert + evidence write) are ported byte-for-byte from the JS but — like book-run.rs's
//! response-parsing/staging logic — are unreachable by this file's own fixture because they run only
//! after a successful `run_seat`, which the deferred `aiCall` stub never produces; correctness there
//! is by inspection + `parse_judge_line`'s own direct unit tests.
//!
//! One deliberate narrowing: JS's `runSeat` throws `new Error(...)` with an attached
//! `.judge_ai_call_id` property when `parseJudgeLine` fails; `Result<_, String>` cannot carry that
//! extra field, so only the message text is preserved (unreachable code path regardless, since
//! `aiCall` itself already errors first on this deferred stub).

use rusqlite::Connection;
use serde_json::{json, Value};

// ── standard helpers (playbook) ─────────────────────────────────────────────────────────────
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
fn tmpl(v: Option<&Value>) -> String {
    match v {
        None => "undefined".into(),
        Some(Value::Null) => "null".into(),
        Some(x) => js_string(x),
    }
}
fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}
fn js_number(v: Option<&Value>) -> f64 {
    match v {
        None => f64::NAN,
        Some(Value::Null) => 0.0,
        Some(Value::Bool(b)) => if *b { 1.0 } else { 0.0 },
        Some(Value::Number(n)) => n.as_f64().unwrap_or(f64::NAN),
        Some(Value::String(s)) => { let t = s.trim(); if t.is_empty() { 0.0 } else { t.parse().unwrap_or(f64::NAN) } }
        Some(_) => f64::NAN,
    }
}
fn first(rows: &Value) -> Option<Value> { rows.as_array().and_then(|a| a.first()).cloned() }
fn take_chars(s: &str, n: usize) -> String { s.chars().take(n).collect() }

const JUDGE_PROMPT: &str = "heimdall.ai.judge.v1";

const CONFIDENCE_GATE: f64 = 0.7;
const FAILURE_CLASSES: [&str; 7] = ["wrong-name", "hallucinated-dep", "incomplete", "style", "off-scope", "degenerate", "other"];
const PANEL_SEATS: [(&str, &str); 3] = [("1", "cold"), ("2", "warm"), ("3", "hot")];

const SYSTEM: &str = "You are HEIMDALL'S QUALITY JUDGE. You rate the OUTPUT an AI model produced for one task inside an autonomous software substrate. You are calibrated, terse, and specific — you judge the work as written, never the model's identity.";

const TEMPLATE: &str = r#"Rate the OUTPUT below on a 4-point scale. Judge ONLY whether the output does its job for THIS task and category — not whether it is long, not whether it is polite.

═══ WHAT THIS CALL WAS FOR ═══
category: {{category}} — {{category_desc}}
gate (prompt): {{template_base}}
model that produced it: {{target_model}}

═══ THE SCALE (choose exactly one integer; there is NO zero) ═══
+2 EXCELLENT — fully does the job; correct, complete, idiomatic; nothing a competent reviewer would change.
+1 GOOD      — does the job; minor nits a reviewer might mention but would not block on.
-1 WEAK      — partially does the job; a real gap, omission, or sloppiness that needs a fix.
-2 WRONG     — fails the job; incorrect, off-task, truncated/degenerate, hallucinated, or unusable.

═══ CATEGORY RUBRIC (what "does the job" means here) ═══
- code-* : the code is syntactically valid, matches the requested signature/intent, no invented
  imports or APIs, no TODO where real code was asked; a STUB should declare signatures with TODO
  bodies (that is correct for a stub, not a defect).
- architecture / planning / api-contract : the decomposition covers the goal, files/blocks are
  sensible for the stack, every acceptance line maps to something; body-level detail is NOT expected.
- critique-adversarial / verdict-binary / rubric-scoring : the claims/verdicts are decidable and
  grounded, not vague or fabricated.
- prose-* / summarize / extract : accurate to the source, appropriately scoped, well-formed; no
  hallucinated facts.
- reasoning / logic / math : the answer is correct and the steps are sound.

═══ HARD ANTI-PATTERNS → -2 ═══
a 3-token/echo/degenerate reply · JSON where raw text was asked (or vice versa) · inventing a
library/store/API the task never mentioned · answering a DIFFERENT task · a stub that declares nothing.

═══ THIS GATE'S DECLARED CRITERIA (when present, they define "does the job") ═══
{{gate_criteria}}

═══ THE TASK THE MODEL RECEIVED (input) ═══
{{task_input}}

═══ THE OUTPUT THE MODEL PRODUCED ═══
{{target_output}}

Think through the rubric first — reason as much as you need, in any form.

Then END your reply with EXACTLY ONE line in EXACTLY this format (nothing after it):
VERDICT=<-2|-1|1|2> CONFIDENCE=<0.00-1.00> CLASS=<class> REASON=<one decidable sentence>

- CONFIDENCE is how sure YOU are of the verdict. ALWAYS include it — never omit.
- CLASS: for NEGATIVE verdicts exactly one of wrong-name (identifier/name drift) ·
  hallucinated-dep (invented import/library/API) · incomplete (a real gap or omission) ·
  style (works but sloppy/unidiomatic) · off-scope (answered a different task) ·
  degenerate (echo/truncated/unusable) · other. For POSITIVE verdicts write none.
- Example final line:  VERDICT=-2 CONFIDENCE=0.90 CLASS=hallucinated-dep REASON=The code imports a redux store the stack does not contain.
(seat {{panel_seat}})"#;

/// `export function parseJudgeLine(text)` — deterministic extraction of the final judgment line.
/// Takes the LAST `VERDICT=...` match (reasoning models may restate the format while thinking; the
/// last line is the answer). Returns `Value::Null` when no VERDICT is found (an unparseable
/// judgment is no judgment) or when the parsed verdict is 0/non-finite. Pure.
pub fn parseJudgeLine(text: &Value) -> Value {
    let t = tmpl(Some(text));
    let re = regex::Regex::new(r"(?i)VERDICT\s*[=:]\s*([+-]?\d+)[^\n]*").unwrap();
    let matches: Vec<regex::Captures> = re.captures_iter(&t).collect();
    let Some(last) = matches.last() else { return Value::Null };
    let line = last.get(0).unwrap().as_str();
    let verdict_raw: f64 = match last.get(1).unwrap().as_str().parse::<i64>() {
        Ok(n) => n as f64,
        Err(_) => f64::NAN,
    };
    if !verdict_raw.is_finite() || verdict_raw == 0.0 { return Value::Null; }
    let verdict_raw_i = verdict_raw as i64;
    let verdict = if [-2, -1, 1, 2].contains(&verdict_raw_i) { verdict_raw_i } else if verdict_raw_i < 0 { -1 } else { 1 };

    let conf_re = regex::Regex::new(r"(?i)CONFIDENCE\s*[=:]\s*([01](?:\.\d+)?)").unwrap();
    let confidence = conf_re.captures(line).and_then(|c| c.get(1)).and_then(|m| m.as_str().parse::<f64>().ok());

    let cls_re = regex::Regex::new(r"(?i)CLASS\s*[=:]\s*([a-zA-Z-]+)").unwrap();
    let failure_class = cls_re.captures(line).and_then(|c| c.get(1)).map(|m| m.as_str().to_lowercase());

    let reason_re = regex::Regex::new(r"(?i)REASON\s*[=:]\s*(.+)$").unwrap();
    let reason = reason_re.captures(line).and_then(|c| c.get(1)).map(|m| m.as_str().trim().to_string());

    json!({
        "verdict": verdict,
        "confidence": confidence,
        "failure_class": failure_class,
        "reason": reason,
    })
}

fn output_fields_json() -> Value {
    json!([
        { "field_name": "judge_raw", "field_type": "string", "description": "the full reply — reasoning followed by the final VERDICT line" },
    ])
}

// `ensurePrompt(db)` — try promptFind; a hit whose template_md already matches TEMPLATE is a no-op;
// any miss (or a stale template body) re-registers via promptAdd (bumping the version). Mirrors the
// JS try/catch around promptFind (a lookup failure is treated the same as "not found").
fn ensure_prompt(db: &Connection) -> Result<(), String> {
    let existing = crate::ai_prompt_find::promptFind(&json!({ "name": JUDGE_PROMPT }), db).ok();
    if let Some(e) = &existing {
        if e.get("template_md").and_then(Value::as_str) == Some(TEMPLATE) {
            return Ok(());
        }
    }
    crate::ai_prompt_add::promptAdd(
        &json!({
            "name": JUDGE_PROMPT,
            "template_md": TEMPLATE,
            "system_md": SYSTEM,
            "default_tier": "high",
            "default_temperature": "cold",
            "max_output_tokens": 1200,
            "output_fields": output_fields_json(),
            "judging_criteria": [
                "Ends with exactly one VERDICT=… CONFIDENCE=… CLASS=… REASON=… line.",
                "The reason is one decidable sentence about THIS output, not a generality.",
                "Confidence reflects real certainty (below 0.7 routes to a human).",
                "Judges the work against the category rubric, never the model identity.",
            ],
        }),
        db,
    )?;
    Ok(())
}

// `targetContext(db, ai_call_id)` — resolves the target call (+ its resolved model/template),
// reconstructs the rendered prompt (cache-aware: a cached call rides its cache_source_call_id's
// inputs/outputs), the joined outputs, and the most recent Heimdall casting for the call.
pub fn targetContext(db: &Connection, ai_call_id: i64) -> Result<Value, String> {
    let call_rows = crate::db_json::query_rows(
        db,
        "SELECT a.id, a.model_id, a.cached, pt.name AS template_name, pt.template_md \
         FROM brain.ai_calls a LEFT JOIN brain.prompt_templates pt ON pt.id = a.prompt_template_id \
         WHERE a.id = ?",
        &[json!(ai_call_id)],
    )?;
    let Some(call) = first(&call_rows) else { return Ok(Value::Null) };

    let src_id: Value = if truthy(call.get("cached")) {
        let r = crate::db_json::query_rows(db, "SELECT cache_source_call_id FROM brain.ai_calls WHERE id = ?", &[json!(ai_call_id)])?;
        first(&r)
            .and_then(|row| row.get("cache_source_call_id").cloned())
            .filter(|v| !v.is_null())
            .unwrap_or(json!(ai_call_id))
    } else {
        json!(ai_call_id)
    };

    let input_rows = crate::db_json::query_rows(
        db,
        "SELECT key, value_text FROM brain.ai_call_inputs WHERE ai_call_id = ? ORDER BY order_index, id",
        &[src_id.clone()],
    )?;
    let inputs: Vec<Value> = input_rows
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(|i| json!({ "key": i.get("key").cloned().unwrap_or(Value::Null), "value": i.get("value_text").cloned().unwrap_or(Value::Null) }))
        .collect();

    let template_md = call.get("template_md").cloned().unwrap_or(Value::Null);
    let rendered = if truthy(Some(&template_md)) {
        crate::ai_ai_call::reconstructRenderedPrompt(&template_md, &json!(inputs))
    } else {
        json!(inputs.iter().map(|i| format!("{}: {}", tmpl(i.get("key")), tmpl(i.get("value")))).collect::<Vec<_>>().join("\n"))
    };

    let output_rows = crate::db_json::query_rows(
        db,
        "SELECT field_name, value_text FROM brain.ai_call_outputs WHERE ai_call_id = ? AND value_text IS NOT NULL ORDER BY order_index, id",
        &[src_id],
    )?;
    let outputs = output_rows
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|o| format!("{}: {}", tmpl(o.get("field_name")), tmpl(o.get("value_text"))))
        .collect::<Vec<_>>()
        .join("\n\n");

    let casting_rows = crate::db_json::query_rows(
        db,
        "SELECT category, template_base FROM brain.heimdall_castings WHERE ai_call_id = ? ORDER BY id DESC LIMIT 1",
        &[json!(ai_call_id)],
    )?;
    let casting = first(&casting_rows).unwrap_or(Value::Null);

    Ok(json!({ "call": call, "rendered": rendered, "outputs": outputs, "casting": casting }))
}

// `runSeat` — the per-panel-seat judge call: assembles the deterministic input_kv, dispatches
// through `aiCall` (PARITY-DEFERRED, always Err on this stub), parses the reply, normalizes
// failure_class against the known set, and clamps confidence (a judge that omits it gets 0.5 —
// deliberately below the gate, routing silence to a human instead of minting evidence).
fn run_seat(db: &Connection, category: &str, category_desc: &str, template_base: &str, target_model: &str, rendered: &str, outputs: &str, gate_criteria: &str, seat: &str, temperature: &str) -> Result<Value, String> {
    let input = json!({
        "prompt_name": JUDGE_PROMPT,
        "input_kv": [
            { "key": "gate_criteria", "value": gate_criteria },
            { "key": "category", "value": category },
            { "key": "category_desc", "value": category_desc },
            { "key": "template_base", "value": template_base },
            { "key": "target_model", "value": target_model },
            { "key": "task_input", "value": take_chars(rendered, 8000) },
            { "key": "target_output", "value": take_chars(outputs, 8000) },
            { "key": "panel_seat", "value": seat },
        ],
        "tier": "high", "temperature": temperature, "phase_flow": "single_only",
        "two_phase": false, "structured_output": false, "provider_fn": Value::Null,
    });
    let res = crate::ai_ai_call::aiCall(&input, db)?;

    let raw = res.get("output_kv").and_then(Value::as_array).and_then(|a| a.iter().find(|o| o.get("field_name").and_then(Value::as_str) == Some("judge_raw"))).and_then(|o| o.get("value_text")).cloned().unwrap_or(json!(""));
    let parsed = parseJudgeLine(&raw);
    if parsed.is_null() {
        return Err(format!("judge reply had no VERDICT line (judge call #{})", tmpl(res.get("ai_call_id"))));
    }

    let mut failure_class = parsed.get("failure_class").cloned().unwrap_or(Value::Null);
    if failure_class.as_str() == Some("none") { failure_class = Value::Null; }
    if let Some(fc) = failure_class.as_str() {
        if !FAILURE_CLASSES.contains(&fc) { failure_class = json!("other"); }
    }
    let confidence = parsed.get("confidence").and_then(Value::as_f64).filter(|c| c.is_finite()).map(|c| c.max(0.0).min(1.0)).unwrap_or(0.5);
    let reason_raw = parsed.get("reason").and_then(Value::as_str).unwrap_or("");
    let reason: String = take_chars(reason_raw, 500);
    let reason = if reason.is_empty() { "(no reason line)".to_string() } else { reason };
    let verdict = parsed.get("verdict").and_then(Value::as_i64).unwrap_or(0);

    Ok(json!({
        "seat": seat,
        "verdict": verdict,
        "confidence": confidence,
        "reason": reason,
        "failure_class": if verdict < 0 { failure_class } else { Value::Null },
        "judge_ai_call_id": res.get("ai_call_id").cloned().unwrap_or(Value::Null),
        "judge_model": res.get("model_id").cloned().unwrap_or(Value::Null),
    }))
}

pub fn heimdallAiTag(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS: `const { db, provider_fn = null } = context ?? {}; if (!db) throw err.contextMissing(...)`
    // — unreachable here (Rust's `db: &Connection` is never absent).
    let ai_call_id_num = js_number(input.get("ai_call_id"));
    if !ai_call_id_num.is_finite() || ai_call_id_num <= 0.0 {
        return Err("heimdall-ai-tag: ai_call_id is required".to_string());
    }
    let ai_call_id = ai_call_id_num as i64;
    let force = input.get("force") == Some(&Value::Bool(true));

    // HUMAN > AI: never tag over a human verdict.
    let human_rows = crate::db_json::query_rows(db, "SELECT 1 FROM brain.heimdall_human_tags WHERE ai_call_id = ?", &[json!(ai_call_id)])?;
    if !human_rows.as_array().map_or(true, |a| a.is_empty()) {
        return Ok(json!({ "ok": true, "skipped": true, "reason": "human-tagged (human is authoritative)" }));
    }
    if !force {
        let tagged_rows = crate::db_json::query_rows(db, "SELECT 1 FROM brain.heimdall_ai_tags WHERE target_ai_call_id = ?", &[json!(ai_call_id)])?;
        if !tagged_rows.as_array().map_or(true, |a| a.is_empty()) {
            return Ok(json!({ "ok": true, "skipped": true, "reason": "already AI-tagged (pass force=true to re-run)" }));
        }
    }

    let ctx = targetContext(db, ai_call_id)?;
    if ctx.is_null() {
        return Ok(json!({ "ok": true, "skipped": true, "reason": "call not found" }));
    }
    let casting = ctx.get("casting").cloned().unwrap_or(Value::Null);
    if casting.is_null() {
        return Ok(json!({ "ok": true, "skipped": true, "reason": "no casting — not a Heimdall-classified call" }));
    }
    let category = casting.get("category").and_then(Value::as_str).unwrap_or("").to_string();
    let template_base = tmpl(casting.get("template_base"));
    let cat_rows = crate::db_json::query_rows(db, "SELECT title FROM brain.heimdall_categories WHERE slug = ?", &[json!(category.clone())])?;
    let cat_title = first(&cat_rows).and_then(|r| r.get("title").and_then(Value::as_str).map(String::from)).unwrap_or_default();

    ensure_prompt(db)?;
    let min_confidence_raw = js_number(input.get("min_confidence"));
    let min_confidence = if min_confidence_raw.is_finite() { min_confidence_raw } else { CONFIDENCE_GATE };
    let panel = input.get("panel") == Some(&Value::Bool(true));

    // the target gate's declared judging criteria sharpen the rubric (utility gates most).
    // JS wraps the query + JSON.parse in one try/catch ("legacy brain without the column").
    let mut gate_criteria = "(none declared — use the category rubric)".to_string();
    let jc_attempt: Result<Value, String> = (|| {
        let rows = crate::db_json::query_rows(
            db,
            "SELECT p.judging_criteria_json FROM brain.ai_calls a JOIN brain.prompt_templates p ON p.id = a.prompt_template_id WHERE a.id = ?",
            &[json!(ai_call_id)],
        )?;
        Ok(first(&rows).and_then(|r| r.get("judging_criteria_json").cloned()).unwrap_or(Value::Null))
    })();
    if let Ok(jc) = jc_attempt {
        if let Some(s) = jc.as_str() {
            if let Ok(Value::Array(arr)) = serde_json::from_str::<Value>(s) {
                if !arr.is_empty() {
                    gate_criteria = arr.iter().enumerate().map(|(i, c)| format!("{}. {}", i + 1, tmpl(Some(c)))).collect::<Vec<_>>().join("\n");
                }
            }
        }
    }

    let rendered = tmpl(ctx.get("rendered"));
    // JS: `String(ctx.outputs ?? '(empty output)')` — `ctx.outputs` is always a built string (never
    // null/undefined, see targetContext), so the `??` fallback never actually triggers even when
    // there were no output rows (outputs stays `''`); ported for the exact same (dead) fallback.
    let outputs_str = ctx.get("outputs").filter(|v| !v.is_null()).map(|v| tmpl(Some(v))).unwrap_or_else(|| "(empty output)".to_string());
    let target_model = ctx.get("call").and_then(|c| c.get("model_id")).and_then(Value::as_str).map(String::from).unwrap_or_else(|| "?".to_string());

    // consolidate: solo = the one seat; panel = MEDIAN verdict, MEAN confidence, agreement = every
    // seat on one sign. The loop below never completes past its first iteration on this deferred
    // stub — `run_seat` propagates aiCall's Err — so everything from here is ported by inspection.
    let seats_to_run: &[(&str, &str)] = if panel { &PANEL_SEATS } else { &PANEL_SEATS[..1] };
    let mut seats: Vec<Value> = Vec::new();
    for (seat, temperature) in seats_to_run {
        seats.push(run_seat(db, &category, &cat_title, &template_base, &target_model, &rendered, &outputs_str, &gate_criteria, seat, temperature)?);
    }

    let mut sorted = seats.clone();
    sorted.sort_by_key(|s| s.get("verdict").and_then(Value::as_i64).unwrap_or(0));
    let verdict = sorted[sorted.len() / 2].get("verdict").and_then(Value::as_i64).unwrap_or(0);
    let confidence = {
        let sum = seats.iter().fold(0.0_f64, |acc, s| acc + s.get("confidence").and_then(Value::as_f64).unwrap_or(0.0));
        let mean = sum / seats.len() as f64;
        (mean * 1000.0).round() / 1000.0
    };
    let panel_agree: Value = if panel {
        let all_pos = seats.iter().all(|s| s.get("verdict").and_then(Value::as_i64).unwrap_or(0) > 0);
        let all_neg = seats.iter().all(|s| s.get("verdict").and_then(Value::as_i64).unwrap_or(0) < 0);
        json!(if all_pos || all_neg { 1 } else { 0 })
    } else {
        Value::Null
    };
    let lead = seats.iter().find(|s| s.get("verdict").and_then(Value::as_i64) == Some(verdict)).cloned().unwrap_or_else(|| seats[0].clone());
    let reason = lead.get("reason").cloned().unwrap_or(Value::Null);
    let failure_class = if verdict < 0 {
        lead.get("failure_class").filter(|v| !v.is_null()).cloned()
            .or_else(|| seats.iter().find_map(|s| s.get("failure_class").filter(|v| !v.is_null()).cloned()))
            .unwrap_or(Value::Null)
    } else {
        Value::Null
    };
    let low_confidence = if confidence < min_confidence || panel_agree == json!(0) { 1 } else { 0 };

    let (_changes, insert_rowid) = crate::db_json::run_write(
        db,
        "INSERT INTO brain.heimdall_ai_tags (target_ai_call_id, verdict, confidence, reason_md, failure_class, low_confidence, panel_json, panel_agree, judge_ai_call_id, judge_model) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
         ON CONFLICT(target_ai_call_id) DO UPDATE SET verdict=excluded.verdict, confidence=excluded.confidence, \
           reason_md=excluded.reason_md, failure_class=excluded.failure_class, low_confidence=excluded.low_confidence, \
           panel_json=excluded.panel_json, panel_agree=excluded.panel_agree, \
           judge_ai_call_id=excluded.judge_ai_call_id, judge_model=excluded.judge_model, \
           superseded_by_human=0, ts=datetime('now')",
        &[
            json!(ai_call_id), json!(verdict), json!(confidence), reason.clone(), failure_class.clone(), json!(low_confidence),
            if panel {
                json!(serde_json::to_string(&json!(seats.iter().map(|s| json!({
                    "seat": s.get("seat"), "verdict": s.get("verdict"), "confidence": s.get("confidence"), "judge_ai_call_id": s.get("judge_ai_call_id"),
                })).collect::<Vec<_>>())).unwrap_or_default())
            } else { Value::Null },
            panel_agree.clone(),
            lead.get("judge_ai_call_id").cloned().unwrap_or(Value::Null),
            lead.get("judge_model").cloned().unwrap_or(Value::Null),
        ],
    )?;
    let tag_id = if insert_rowid != 0 {
        insert_rowid
    } else {
        first(&crate::db_json::query_rows(db, "SELECT id FROM brain.heimdall_ai_tags WHERE target_ai_call_id = ?", &[json!(ai_call_id)])?)
            .and_then(|r| r.get("id").and_then(Value::as_i64))
            .unwrap_or(0)
    };

    // evidence rides the TARGET call's casting — ai_panel (0.6) for an agreeing panel, ai_judge
    // (0.4) for a confident solo, NOTHING when low-confidence/split (human queue).
    crate::db_json::run_write(db, "DELETE FROM brain.heimdall_evidence WHERE ai_call_id = ? AND provenance IN ('ai_judge','ai_panel')", &[json!(ai_call_id)])?;
    let mut evidence_written = false;
    if low_confidence == 0 {
        let provenance = if panel && panel_agree == json!(1) { "ai_panel" } else { "ai_judge" };
        crate::db_json::run_write(
            db,
            "INSERT INTO brain.heimdall_evidence (model_slug, category, template_base, provenance, outcome, ai_call_id, ai_tag_id) VALUES (?, ?, ?, ?, ?, ?, ?)",
            &[
                ctx.get("call").and_then(|c| c.get("model_id")).cloned().unwrap_or(Value::Null),
                json!(category),
                json!(template_base),
                json!(provenance),
                json!(if verdict > 0 { "clean" } else { "fail" }),
                json!(ai_call_id),
                json!(tag_id),
            ],
        )?;
        evidence_written = true;
    }

    Ok(json!({
        "ok": true, "tagged": true, "verdict": verdict, "confidence": confidence, "reason": reason, "failure_class": failure_class,
        "low_confidence": low_confidence != 0, "evidence_written": evidence_written,
        "panel": if panel { json!({ "seats": seats.iter().map(|s| json!({ "seat": s.get("seat"), "verdict": s.get("verdict"), "confidence": s.get("confidence") })).collect::<Vec<_>>(), "agree": panel_agree == json!(1) }) } else { Value::Null },
        "judge_ai_call_id": lead.get("judge_ai_call_id").cloned().unwrap_or(Value::Null),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS brain;").unwrap();
        c.execute_batch(
            "CREATE TABLE brain.ai_calls (id INTEGER PRIMARY KEY AUTOINCREMENT, prompt_template_id INTEGER, model_id TEXT, cached INTEGER, cache_source_call_id INTEGER);
             CREATE TABLE brain.ai_call_inputs (id INTEGER PRIMARY KEY AUTOINCREMENT, ai_call_id INTEGER, key TEXT, value_text TEXT, order_index INTEGER);
             CREATE TABLE brain.ai_call_outputs (id INTEGER PRIMARY KEY AUTOINCREMENT, ai_call_id INTEGER, field_name TEXT, value_text TEXT, order_index INTEGER);
             CREATE TABLE brain.prompt_templates ( id INTEGER PRIMARY KEY, name TEXT NOT NULL, version INTEGER NOT NULL, template_md TEXT NOT NULL, default_tier TEXT NOT NULL, default_temperature TEXT NOT NULL, max_output_tokens INTEGER NOT NULL DEFAULT 1024, superseded_by INTEGER, created_at TEXT NOT NULL DEFAULT (datetime('now')), cross_field_rules_json TEXT, judging_criteria_json TEXT, system_md TEXT, phase_flow TEXT NOT NULL DEFAULT 'single', UNIQUE (name, version) );
             CREATE TABLE brain.prompt_output_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, grounded_refs_json TEXT, UNIQUE (prompt_template_id, field_name) );
             CREATE TABLE brain.prompt_input_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, UNIQUE (prompt_template_id, field_name) );
             CREATE TABLE brain.heimdall_categories (slug TEXT PRIMARY KEY, title TEXT);
             CREATE TABLE brain.heimdall_castings (id INTEGER PRIMARY KEY AUTOINCREMENT, ai_call_id INTEGER, category TEXT, template_base TEXT);
             CREATE TABLE brain.heimdall_human_tags (id INTEGER PRIMARY KEY AUTOINCREMENT, ai_call_id INTEGER);
             CREATE TABLE brain.heimdall_ai_tags (id INTEGER PRIMARY KEY AUTOINCREMENT, target_ai_call_id INTEGER UNIQUE, verdict INTEGER, confidence REAL, reason_md TEXT, failure_class TEXT, low_confidence INTEGER, panel_json TEXT, panel_agree INTEGER, judge_ai_call_id INTEGER, judge_model TEXT, superseded_by_human INTEGER NOT NULL DEFAULT 0, ts TEXT);
             CREATE TABLE brain.heimdall_evidence (id INTEGER PRIMARY KEY AUTOINCREMENT, model_slug TEXT, category TEXT, template_base TEXT, provenance TEXT, outcome TEXT, ai_call_id INTEGER, ai_tag_id INTEGER, human_tag_id INTEGER);",
        ).unwrap();
        c
    }

    fn a_call(c: &Connection, model: &str) -> i64 {
        c.execute("INSERT INTO brain.ai_calls (model_id, cached) VALUES (?, 0)", [model]).unwrap();
        c.last_insert_rowid()
    }

    // ── parseJudgeLine ───────────────────────────────────────────────────
    #[test]
    fn parse_judge_line_full_match() {
        let r = parseJudgeLine(&json!("some reasoning\nVERDICT=-2 CONFIDENCE=0.90 CLASS=hallucinated-dep REASON=Imports a store the stack lacks."));
        assert_eq!(r["verdict"], json!(-2));
        assert_eq!(r["confidence"], json!(0.90));
        assert_eq!(r["failure_class"], json!("hallucinated-dep"));
        assert_eq!(r["reason"], json!("Imports a store the stack lacks."));
    }

    #[test]
    fn parse_judge_line_takes_last_match() {
        let r = parseJudgeLine(&json!("VERDICT=1 CONFIDENCE=0.2 CLASS=none REASON=draft\nfinal thought\nVERDICT=2 CONFIDENCE=0.95 CLASS=none REASON=Correct and complete."));
        assert_eq!(r["verdict"], json!(2));
        assert_eq!(r["confidence"], json!(0.95));
        assert_eq!(r["reason"], json!("Correct and complete."));
    }

    #[test]
    fn parse_judge_line_no_verdict_or_zero_is_null() {
        assert_eq!(parseJudgeLine(&json!("no verdict here")), Value::Null);
        assert_eq!(parseJudgeLine(&json!("VERDICT=0 CONFIDENCE=0.5 REASON=weird")), Value::Null);
        assert_eq!(parseJudgeLine(&Value::Null), Value::Null);
    }

    #[test]
    fn parse_judge_line_clamps_out_of_scale_verdict_and_missing_optionals() {
        let r = parseJudgeLine(&json!("VERDICT=-9 nothing else on this line"));
        assert_eq!(r["verdict"], json!(-1));
        assert_eq!(r["confidence"], Value::Null);
        assert_eq!(r["failure_class"], Value::Null);
        assert_eq!(r["reason"], Value::Null);

        let r2 = parseJudgeLine(&json!("VERDICT=7 more text"));
        assert_eq!(r2["verdict"], json!(1));
    }

    // ── ensure_prompt ────────────────────────────────────────────────────
    #[test]
    fn ensure_prompt_registers_once_and_is_idempotent() {
        let c = setup();
        ensure_prompt(&c).unwrap();
        let n: i64 = c.query_row("SELECT COUNT(*) FROM brain.prompt_templates WHERE name = 'heimdall.ai.judge.v1'", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1);
        ensure_prompt(&c).unwrap();
        let n2: i64 = c.query_row("SELECT COUNT(*) FROM brain.prompt_templates WHERE name = 'heimdall.ai.judge.v1'", [], |r| r.get(0)).unwrap();
        assert_eq!(n2, 1, "matching template_md is a no-op, no version bump");
    }

    // ── targetContext ────────────────────────────────────────────────────
    #[test]
    fn target_context_not_found_returns_null() {
        let c = setup();
        assert_eq!(targetContext(&c, 999).unwrap(), Value::Null);
    }

    #[test]
    fn target_context_assembles_rendered_outputs_and_casting() {
        let c = setup();
        let cid = a_call(&c, "model-x");
        c.execute("INSERT INTO brain.prompt_templates (id, name, version, template_md, default_tier, default_temperature, created_at) VALUES (1, 'p', 1, 'Say hi to {{who}}.', 'mid', 'cold', '2026-01-01')", []).unwrap();
        c.execute("UPDATE brain.ai_calls SET prompt_template_id = 1 WHERE id = ?1", [cid]).unwrap();
        c.execute("INSERT INTO brain.ai_call_inputs (ai_call_id, key, value_text, order_index) VALUES (?1, 'who', 'Dane', 0)", [cid]).unwrap();
        c.execute("INSERT INTO brain.ai_call_outputs (ai_call_id, field_name, value_text, order_index) VALUES (?1, 'reply', 'Hello Dane', 0)", [cid]).unwrap();
        c.execute("INSERT INTO brain.heimdall_castings (ai_call_id, category, template_base) VALUES (?1, 'conversational', 'chat')", [cid]).unwrap();

        let ctx = targetContext(&c, cid).unwrap();
        assert_eq!(ctx["rendered"], json!("Say hi to Dane."));
        assert_eq!(ctx["outputs"], json!("reply: Hello Dane"));
        assert_eq!(ctx["casting"]["category"], json!("conversational"));
    }

    // ── heimdallAiTag: guards + skip paths ─────────────────────────────
    #[test]
    fn ai_call_id_required() {
        let c = setup();
        assert!(heimdallAiTag(&json!({}), &c).unwrap_err().contains("ai_call_id is required"));
        assert!(heimdallAiTag(&json!({ "ai_call_id": -1 }), &c).unwrap_err().contains("ai_call_id is required"));
    }

    #[test]
    fn human_tagged_call_is_skipped() {
        let c = setup();
        let cid = a_call(&c, "m");
        c.execute("INSERT INTO brain.heimdall_human_tags (ai_call_id) VALUES (?1)", [cid]).unwrap();
        let r = heimdallAiTag(&json!({ "ai_call_id": cid }), &c).unwrap();
        assert_eq!(r, json!({ "ok": true, "skipped": true, "reason": "human-tagged (human is authoritative)" }));
    }

    #[test]
    fn already_ai_tagged_is_skipped_unless_forced() {
        let c = setup();
        let cid = a_call(&c, "m");
        c.execute("INSERT INTO brain.heimdall_ai_tags (target_ai_call_id) VALUES (?1)", [cid]).unwrap();
        let r = heimdallAiTag(&json!({ "ai_call_id": cid }), &c).unwrap();
        assert_eq!(r["skipped"], json!(true));
        assert_eq!(r["reason"], json!("already AI-tagged (pass force=true to re-run)"));
        // force=true bypasses the skip and proceeds past it — with a casting row present it walks
        // all the way to the deferred aiCall boundary instead of skipping again.
        c.execute("INSERT INTO brain.heimdall_castings (ai_call_id, category, template_base) VALUES (?1, 'x', 'y')", [cid]).unwrap();
        let err2 = heimdallAiTag(&json!({ "ai_call_id": cid, "force": true }), &c).unwrap_err();
        assert!(err2.contains("ai-call"), "unexpected error: {err2}");
    }

    #[test]
    fn call_not_found_is_skipped() {
        let c = setup();
        let r = heimdallAiTag(&json!({ "ai_call_id": 12345 }), &c).unwrap();
        assert_eq!(r, json!({ "ok": true, "skipped": true, "reason": "call not found" }));
    }

    #[test]
    fn no_casting_is_skipped() {
        let c = setup();
        let cid = a_call(&c, "m");
        let r = heimdallAiTag(&json!({ "ai_call_id": cid }), &c).unwrap();
        assert_eq!(r, json!({ "ok": true, "skipped": true, "reason": "no casting — not a Heimdall-classified call" }));
    }

    #[test]
    fn reaches_the_parity_deferred_aicall_boundary() {
        // proves every deterministic gate (guards/skips/target_context/ensure_prompt/gate-criteria
        // read) ran cleanly and the error is aiCall's deferred stub, not an earlier bug.
        let c = setup();
        let cid = a_call(&c, "model-x");
        c.execute("INSERT INTO brain.heimdall_castings (ai_call_id, category, template_base) VALUES (?1, 'conversational', 'chat')", [cid]).unwrap();
        c.execute("INSERT INTO brain.heimdall_categories (slug, title) VALUES ('conversational', 'Conversational')", []).unwrap();
        let err = heimdallAiTag(&json!({ "ai_call_id": cid }), &c).unwrap_err();
        assert!(err.contains("ai-call"), "unexpected error: {err}");
        let n: i64 = c.query_row("SELECT COUNT(*) FROM brain.prompt_templates WHERE name = 'heimdall.ai.judge.v1'", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1, "ensure_prompt ran before the deferred call");
    }
}
