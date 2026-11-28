//! heimdall-tag-assist.rs — twin of skills/heimdall/heimdall-tag-assist.mjs (db, cross-db
//! brain.*/memory.*). NAMES ARE CONTRACT: default `heimdallTagAssist`. Human-tagging reading
//! aids — this skill NEVER writes a tag, never mints evidence, never touches the ledger.
//!
//! op:'pretag'  → deterministic case-file (no AI, no cost) — the real work of this file.
//! op:'context' → deterministic sidebar memory (no AI, no cost).
//! op:'summarize_input' / 'summarize_output' / 'analyze' → PARITY-DEFERRED: each dispatches
//! through `crate::ai_ai_call::aiCall` (the deferred network-AI stub, see ai-call.rs's module
//! doc) and PROPAGATES its Err (JS: no try/catch around `await aiCall(...)` in the default
//! export). Everything upstream of that call — the guards, `targetContext` reuse (already-
//! converted `heimdall_heimdall_ai_tag::targetContext`), `ensure_prompts` (reusing the already-
//! converted `ai_prompt_find`/`ai_prompt_add` twins), and (for `analyze`) the `pretag`-derived
//! demand list assembly — is real and IS the deterministic surface these tests exercise. The
//! text-extraction + CLAIM-line parsing after `aiCall` is ported byte-for-byte from the JS but
//! is unreachable by this file's own fixture — like book-run.rs / heimdall-ai-tag.rs, correctness
//! there is by inspection.

use rusqlite::Connection;
use serde_json::{json, Value};

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
fn str_qq(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(),
        Some(o) => o.to_string(),
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
fn tmpl(v: Option<&Value>) -> String {
    match v {
        None => "undefined".into(),
        Some(Value::Null) => "null".into(),
        Some(x) => js_string(x),
    }
}
fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) }
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

const SUM_PROMPT: &str = "heimdall.assist.summarize.v1";
const SUM_TEMPLATE: &str = "Summarize the CONTENT below for a busy human reviewer who must judge an AI call in seconds.\n\nReply with:\n- ONE line saying what this is.\n- 3-6 bullets with only the things that matter for judging quality (the ask, the constraints, the shape of the answer).\nPlain language, under 120 words, no preamble, no fluff.\n\nCONTENT ({{what}}):\n{{content}}";

const ANALYZE_PROMPT: &str = "heimdall.assist.analyze.v1";
const ANALYZE_SYSTEM: &str = "You assist a HUMAN who must rate an AI output. You NEVER give a verdict or a score — the human decides. You hand them decidable CLAIMS to confirm or deny.";
const ANALYZE_TEMPLATE: &str = "A human is about to rate the OUTPUT below. The DEMANDS are numbered — for EACH demand, emit exactly one claim about whether the output satisfies it, quoting the evidence.\n\nThink as much as you need. Then END with ONLY claim lines, one per line, EXACTLY:\nCLAIM|D<n>|met|<quote/point to the evidence in the output>\nCLAIM|D<n>|missed|<what is absent or wrong, concretely>\nCLAIM|D<n>|unsure|<what a human must eyeball to decide>\n(if you see a REAL defect no demand covers, add: CLAIM|extra|missed|<the defect>)\n\nRules: one claim per numbered demand, in order · evidence over opinion · NO verdicts or scores · nothing after the last claim line.\n\n═══ THE DEMANDS ═══\n{{demands}}\n\n═══ THE TASK (context) ═══\n{{task_input}}\n\n═══ THE OUTPUT ═══\n{{target_output}}";

const OPS: [&str; 5] = ["summarize_input", "summarize_output", "analyze", "pretag", "context"];

fn ensure_prompts(db: &Connection) -> Result<(), String> {
    let sum = crate::ai_prompt_find::promptFind(&json!({ "name": SUM_PROMPT }), db).ok();
    let need_sum = match &sum {
        None => true,
        Some(s) => s.get("template_md").and_then(Value::as_str) != Some(SUM_TEMPLATE),
    };
    if need_sum {
        crate::ai_prompt_add::promptAdd(
            &json!({
                "name": SUM_PROMPT, "template_md": SUM_TEMPLATE,
                "default_tier": "mid", "default_temperature": "cold", "max_output_tokens": 400,
                "output_fields": [{ "field_name": "summary_md", "field_type": "string" }],
                "judging_criteria": [
                    "Under 120 words, no preamble or fluff.",
                    "One line saying WHAT this is, then 3-6 bullets.",
                    "Every bullet is something a reviewer needs to judge quality (the ask, a constraint, the answer shape).",
                    "Nothing invented — every statement traceable to the content.",
                ],
            }),
            db,
        )?;
    }
    let ana = crate::ai_prompt_find::promptFind(&json!({ "name": ANALYZE_PROMPT }), db).ok();
    let need_ana = match &ana {
        None => true,
        Some(a) => a.get("template_md").and_then(Value::as_str) != Some(ANALYZE_TEMPLATE),
    };
    if need_ana {
        crate::ai_prompt_add::promptAdd(
            &json!({
                "name": ANALYZE_PROMPT, "template_md": ANALYZE_TEMPLATE, "system_md": ANALYZE_SYSTEM,
                "default_tier": "high", "default_temperature": "cold", "max_output_tokens": 700,
                "output_fields": [{ "field_name": "analysis_md", "field_type": "string" }],
                "judging_criteria": [
                    "Ends with 3-8 CLAIM|tone|text lines and nothing after them.",
                    "Every claim is ONE decidable sentence tied to something concrete in the output.",
                    "No verdicts, scores, or recommendations anywhere.",
                    "bad claims name a REAL defect (gap, invented import/API, wrong name, off-task).",
                ],
            }),
            db,
        )?;
    }
    Ok(())
}

// ── op:'context' — zero AI, the tagging sidebar's memory ───────────────────────────────────
fn tag_context(db: &Connection, ai_call_id: i64) -> Result<Value, String> {
    let call_rows = crate::db_json::query_rows(
        db,
        "SELECT a.id, a.ts, a.tier, a.model_id, a.contract_slug, a.prompt_template_id, \
         COALESCE(a.cost_usd_actual, 0) AS cost_usd, a.tokens_in, a.tokens_out, a.duration_ms, \
         p.name AS template_name \
         FROM brain.ai_calls a LEFT JOIN brain.prompt_templates p ON p.id = a.prompt_template_id \
         WHERE a.id = ?",
        &[json!(ai_call_id)],
    )?;
    let call = match first(&call_rows) {
        Some(c) => c,
        None => return Ok(json!({ "ok": true, "skipped": true, "reason": "call not found" })),
    };

    let casting_rows = crate::db_json::query_rows(db, "SELECT category, template_base FROM brain.heimdall_castings WHERE ai_call_id = ? ORDER BY id DESC LIMIT 1", &[json!(ai_call_id)])?;
    let casting = first(&casting_rows);
    let base = nz(casting.as_ref().and_then(|c| c.get("template_base")))
        .map(|v| tmpl(Some(v)))
        .unwrap_or_else(|| regex::Regex::new(r"\.v\d+$").unwrap().replace(&tmpl(call.get("template_name")), "").to_string());

    let mut contract_slug = call.get("contract_slug").cloned().filter(|v| !v.is_null());
    if contract_slug.is_none() {
        let inp_rows = crate::db_json::query_rows(db, "SELECT key, value_text FROM brain.ai_call_inputs WHERE ai_call_id = ? AND key IN ('contract_slug','card_slug')", &[json!(ai_call_id)])?;
        let inp = inp_rows.as_array().cloned().unwrap_or_default();
        let mut cs = inp.iter().find(|i| i.get("key").and_then(Value::as_str) == Some("contract_slug")).and_then(|i| i.get("value_text").cloned()).filter(|v| !v.is_null());
        let mut card_slug = inp.iter().find(|i| i.get("key").and_then(Value::as_str) == Some("card_slug")).and_then(|i| i.get("value_text").cloned()).filter(|v| !v.is_null());
        if cs.is_none() && card_slug.is_none() {
            let sid_rows = crate::db_json::query_rows(db, "SELECT session_id FROM brain.ai_calls WHERE id = ?", &[json!(ai_call_id)])?;
            let sid = first(&sid_rows).and_then(|r| r.get("session_id").cloned()).map(|v| tmpl(Some(&v))).unwrap_or_default();
            let re = regex::Regex::new(r"(?i)^card-(.+)-[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$").unwrap();
            if let Some(caps) = re.captures(&sid) { card_slug = Some(json!(caps.get(1).unwrap().as_str())); }
        }
        if cs.is_none() {
            if let Some(cslug) = &card_slug {
                let r = crate::db_json::query_rows(db, "SELECT slug FROM contracts WHERE card_slug = ? ORDER BY id DESC LIMIT 1", &[cslug.clone()])?;
                cs = first(&r).and_then(|row| row.get("slug").cloned()).filter(|v| !v.is_null());
            }
        }
        contract_slug = cs;
    }

    let mut contract = Value::Null;
    let mut card = Value::Null;
    if let Some(cslug) = contract_slug.clone() {
        let c_rows = crate::db_json::query_rows(db, "SELECT slug, card_slug, status, current_attempt, rejection_reason_md, worktree_path, goal_md, acceptance_json FROM contracts WHERE slug = ?", &[cslug])?;
        if let Some(c) = first(&c_rows) {
            let mut acceptance_count: i64 = 0;
            if let Some(s) = c.get("acceptance_json").and_then(Value::as_str) {
                if let Ok(Value::Array(arr)) = serde_json::from_str::<Value>(s) { acceptance_count = arr.len() as i64; }
            }
            let rejection = if truthy(c.get("rejection_reason_md")) { json!(take_chars(&tmpl(c.get("rejection_reason_md")), 220)) } else { Value::Null };
            contract = json!({
                "slug": c.get("slug").cloned().unwrap_or(Value::Null),
                "status": c.get("status").cloned().unwrap_or(Value::Null),
                "attempt": c.get("current_attempt").cloned().unwrap_or(Value::Null),
                "acceptance_count": acceptance_count,
                "worktree": truthy(c.get("worktree_path")),
                "rejection": rejection,
                "goal_head": take_chars(&tmpl(c.get("goal_md")), 220),
            });
            if let Some(card_slug_v) = nz(c.get("card_slug")) {
                let k_rows = crate::db_json::query_rows(db, "SELECT slug, title, status, body_json FROM cards WHERE slug = ?", &[card_slug_v.clone()])?;
                if let Some(k) = first(&k_rows) {
                    let mut m = serde_json::Map::new();
                    m.insert("slug".into(), k.get("slug").cloned().unwrap_or(Value::Null));
                    m.insert("title".into(), k.get("title").cloned().unwrap_or(Value::Null));
                    m.insert("status".into(), k.get("status").cloned().unwrap_or(Value::Null));
                    let body_raw = nz(k.get("body_json")).map(|v| tmpl(Some(v))).unwrap_or_else(|| "{}".to_string());
                    if let Ok(b) = serde_json::from_str::<Value>(&body_raw) {
                        m.insert("kind".into(), nz(b.get("kind")).cloned().unwrap_or(Value::Null));
                        m.insert("size".into(), nz(b.get("size")).cloned().unwrap_or(Value::Null));
                        m.insert("why".into(), json!(take_chars(&str_qq(b.get("why")), 200)));
                    }
                    card = Value::Object(m);
                }
            }
        }
    }

    let mut timeline: Vec<Value> = Vec::new();
    if !card.is_null() {
        let card_slug_v = card.get("slug").cloned().unwrap_or(Value::Null);
        let rows = crate::db_json::query_rows(
            db,
            "SELECT a.id, hc.template_base, a.outcome_class \
             FROM brain.ai_calls a JOIN brain.heimdall_castings hc ON hc.ai_call_id = a.id \
             WHERE a.cached = 0 AND ( \
               a.contract_slug IN (SELECT slug FROM contracts WHERE card_slug = ?) \
               OR a.session_id LIKE 'card-' || ? || '-%' \
             ) ORDER BY a.id LIMIT 60",
            &[card_slug_v.clone(), card_slug_v.clone()],
        )?;
        timeline = rows.as_array().cloned().unwrap_or_default().into_iter().map(|mut t| {
            let cur = t.get("id").and_then(Value::as_i64) == Some(ai_call_id);
            if let Value::Object(ref mut m) = t { m.insert("current".into(), json!(cur)); }
            t
        }).collect();
    }

    let mut final_v = Value::Null;
    if !card.is_null() {
        let card_slug_v = card.get("slug").cloned().unwrap_or(Value::Null);
        let burns_rows = crate::db_json::query_rows(
            db,
            "SELECT COUNT(*) AS n FROM brain.heimdall_evidence e JOIN brain.ai_calls a2 ON a2.id = e.ai_call_id \
             WHERE e.outcome = 'burn' AND ( \
               a2.contract_slug IN (SELECT slug FROM contracts WHERE card_slug = ?) \
               OR a2.session_id LIKE 'card-' || ? || '-%' \
             )",
            &[card_slug_v.clone(), card_slug_v.clone()],
        )?;
        let burns = first(&burns_rows).and_then(|r| r.get("n").cloned()).unwrap_or(json!(0));
        let mut last_tick = Value::Null;
        if let Ok(t_rows) = crate::db_json::query_rows(db, "SELECT outcome, decided_reason, started_at FROM driver_ticks WHERE card_slug = ? ORDER BY id DESC LIMIT 1", &[card_slug_v.clone()]) {
            if let Some(t) = first(&t_rows) {
                last_tick = json!({ "outcome": t.get("outcome").cloned().unwrap_or(Value::Null), "reason": take_chars(&str_qq(t.get("decided_reason")), 180), "ts": t.get("started_at").cloned().unwrap_or(Value::Null) });
            }
        }
        let mut pr = Value::Null;
        if let Ok(pr_rows) = crate::db_json::query_rows(
            db,
            "SELECT status, branch FROM cockpit_pull_requests \
             WHERE source = 'driver' AND ref_slug IN (SELECT slug FROM contracts WHERE card_slug = ?) ORDER BY id DESC LIMIT 1",
            &[card_slug_v.clone()],
        ) {
            if let Some(p) = first(&pr_rows) { pr = p; }
        }
        final_v = json!({ "card_status": card.get("status").cloned().unwrap_or(Value::Null), "burns": burns, "last_tick": last_tick, "pr": pr });
    }

    let mut prompt = Value::Null;
    if let Some(tn) = nz(call.get("template_name")) {
        let tn_v = tn.clone();
        let ptag_rows = crate::db_json::query_rows(db, "SELECT verdict, source, reason_md FROM brain.heimdall_prompt_tags WHERE template_name = ? ORDER BY id DESC LIMIT 1", &[tn_v.clone()])?;
        let ptag = first(&ptag_rows);
        let mut has_criteria = false;
        if let Ok(hc_rows) = crate::db_json::query_rows(db, "SELECT judging_criteria_json FROM brain.prompt_templates WHERE id = ?", &[call.get("prompt_template_id").cloned().unwrap_or(Value::Null)]) {
            has_criteria = truthy(first(&hc_rows).and_then(|r| r.get("judging_criteria_json").cloned()).as_ref());
        }
        let versions_rows = crate::db_json::query_rows(db, "SELECT name FROM brain.prompt_templates WHERE (name LIKE ? || '.v%' OR name = ?) AND superseded_by IS NULL ORDER BY id DESC LIMIT 5", &[json!(base.clone()), json!(base.clone())])?;
        let versions: Vec<Value> = versions_rows.as_array().cloned().unwrap_or_default().into_iter().map(|v| v.get("name").cloned().unwrap_or(Value::Null)).collect();
        let latest_tag = ptag.as_ref().map(|p| json!({
            "verdict": p.get("verdict").cloned().unwrap_or(Value::Null),
            "source": p.get("source").cloned().unwrap_or(Value::Null),
            "reason": take_chars(&str_qq(p.get("reason_md")), 160),
        })).unwrap_or(Value::Null);
        prompt = json!({ "name": tn_v, "tier": call.get("tier").cloned().unwrap_or(Value::Null), "has_criteria": has_criteria, "latest_tag": latest_tag, "versions": versions });
    }

    let history = crate::db_json::query_rows(
        db,
        "SELECT a.id, a.ts, a.outcome_class, a.model_id, ct.card_slug, \
           (SELECT h.verdict FROM brain.heimdall_human_tags h WHERE h.ai_call_id = a.id) AS human_verdict, \
           (SELECT t.verdict FROM brain.heimdall_ai_tags t WHERE t.target_ai_call_id = a.id AND t.superseded_by_human = 0) AS ai_verdict \
         FROM brain.ai_calls a JOIN brain.heimdall_castings hc ON hc.ai_call_id = a.id \
         LEFT JOIN contracts ct ON ct.slug = a.contract_slug \
         WHERE hc.template_base = ? AND a.id != ? AND a.cached = 0 \
         ORDER BY a.id DESC LIMIT 6",
        &[json!(base.clone()), json!(ai_call_id)],
    )?;

    let gate_avg_rows = crate::db_json::query_rows(
        db,
        "SELECT ROUND(AVG(COALESCE(a.cost_usd_actual,0)), 5) AS cost_usd, ROUND(AVG(a.tokens_out)) AS tokens_out, ROUND(AVG(a.duration_ms)) AS duration_ms \
         FROM brain.ai_calls a JOIN brain.heimdall_castings hc ON hc.ai_call_id = a.id \
         WHERE hc.template_base = ?",
        &[json!(base.clone())],
    )?;
    let gate_avg = first(&gate_avg_rows).unwrap_or(json!({}));

    Ok(json!({
        "ok": true,
        "context": {
            "call": {
                "id": call.get("id").cloned().unwrap_or(Value::Null), "ts": call.get("ts").cloned().unwrap_or(Value::Null),
                "tier": call.get("tier").cloned().unwrap_or(Value::Null), "model_id": call.get("model_id").cloned().unwrap_or(Value::Null),
                "cost_usd": call.get("cost_usd").cloned().unwrap_or(Value::Null), "tokens_in": call.get("tokens_in").cloned().unwrap_or(Value::Null),
                "tokens_out": call.get("tokens_out").cloned().unwrap_or(Value::Null), "duration_ms": call.get("duration_ms").cloned().unwrap_or(Value::Null),
            },
            "gate": { "base": base, "category": casting.as_ref().and_then(|c| c.get("category")).cloned().unwrap_or(Value::Null) },
            "card": card, "contract": contract, "timeline": timeline, "final": final_v, "prompt": prompt, "history": history, "gate_avg": gate_avg,
        }
    }))
}

// ── shared: parseArr / distill (reconciliation ledger + demand distillation) ───────────────
fn parse_arr(t: Option<&Value>) -> Vec<Value> {
    let s = match t.and_then(Value::as_str) { Some(s) => s, None => return vec![] };
    match serde_json::from_str::<Value>(s) {
        Ok(Value::Array(a)) => a,
        Ok(Value::Object(o)) => o.into_values().collect(),
        _ => vec![],
    }
}
fn distill(it: &Value) -> String {
    match it {
        Value::String(s) => s.clone(),
        Value::Object(_) | Value::Array(_) => {
            for k in ["text", "intent", "statement", "claim", "title", "then", "desc", "description", "id"] {
                if let Some(v) = it.get(k) {
                    if !v.is_null() { return js_string(v); }
                }
            }
            serde_json::to_string(it).unwrap_or_default()
        }
        _ => js_string(it),
    }
}
fn walk_tokens(v: &Value, re: &regex::Regex, tokens: &mut Vec<String>) {
    match v {
        Value::String(s) => {
            let t = s.trim();
            if re.is_match(t) && !tokens.iter().any(|x| x == t) { tokens.push(t.to_string()); }
        }
        Value::Array(a) => for x in a { walk_tokens(x, re, tokens); },
        Value::Object(o) => for x in o.values() { walk_tokens(x, re, tokens); },
        _ => {}
    }
}

// ── op:'pretag' — the deterministic CASE FILE (zero AI, zero cost) ─────────────────────────
fn pretag(db: &Connection, ai_call_id: i64) -> Result<Value, String> {
    let call_rows = crate::db_json::query_rows(
        db,
        "SELECT a.id, a.ts, a.tier, a.model_id, a.outcome, a.outcome_class, a.duration_ms, \
         a.tokens_in, a.tokens_out, a.contract_slug, a.prompt_template_id, pt.name AS template_name \
         FROM brain.ai_calls a LEFT JOIN brain.prompt_templates pt ON pt.id = a.prompt_template_id \
         WHERE a.id = ?",
        &[json!(ai_call_id)],
    )?;
    let call = match first(&call_rows) {
        Some(c) => c,
        None => return Ok(json!({ "ok": true, "skipped": true, "reason": "call not found" })),
    };
    let casting_rows = crate::db_json::query_rows(db, "SELECT category, template_base FROM brain.heimdall_castings WHERE ai_call_id = ? ORDER BY id DESC LIMIT 1", &[json!(ai_call_id)])?;
    let casting = first(&casting_rows);
    let cat_title = if let Some(c) = &casting {
        let rows = crate::db_json::query_rows(db, "SELECT title FROM brain.heimdall_categories WHERE slug = ?", &[c.get("category").cloned().unwrap_or(Value::Null)])?;
        first(&rows).and_then(|r| r.get("title").cloned()).unwrap_or(Value::Null)
    } else { Value::Null };

    // EXPECTED — the gate's output contract
    let ptid = call.get("prompt_template_id").cloned().filter(|v| !v.is_null()).unwrap_or(json!(-1));
    let exp_rows = crate::db_json::query_rows(db, "SELECT field_name, field_type, required, min_length, max_length, required_sections_json FROM brain.prompt_output_fields WHERE prompt_template_id = ? ORDER BY order_index", &[ptid])?;
    let expected_fields: Vec<Value> = exp_rows.as_array().cloned().unwrap_or_default().into_iter().map(|f| {
        let sections: Option<Value> = f.get("required_sections_json").and_then(Value::as_str).and_then(|s| serde_json::from_str(s).ok());
        let mut rules: Vec<Value> = Vec::new();
        if truthy(f.get("min_length")) { rules.push(json!(format!("≥{} chars", tmpl(f.get("min_length"))))); }
        if truthy(f.get("max_length")) { rules.push(json!(format!("≤{} chars", tmpl(f.get("max_length"))))); }
        if let Some(Value::Array(secs)) = &sections {
            if !secs.is_empty() {
                let names: Vec<String> = secs.iter().map(|s| tmpl(Some(s))).collect();
                rules.push(json!(format!("sections: {}", names.join(", "))));
            }
        }
        json!({
            "field": f.get("field_name").cloned().unwrap_or(Value::Null),
            "type": f.get("field_type").cloned().unwrap_or(Value::Null),
            "required": truthy(f.get("required")),
            "rules": rules,
        })
    }).collect();

    // RECEIVED — what actually came back, attempt by attempt
    let recv_rows = crate::db_json::query_rows(db, "SELECT field_name, LENGTH(COALESCE(value_text,'')) AS chars FROM brain.ai_call_outputs WHERE ai_call_id = ? ORDER BY order_index, id", &[json!(ai_call_id)])?;
    let received_fields: Vec<Value> = recv_rows.as_array().cloned().unwrap_or_default().into_iter()
        .filter(|o| !tmpl(o.get("field_name")).starts_with("__"))
        .map(|o| json!({ "field": o.get("field_name").cloned().unwrap_or(Value::Null), "chars": o.get("chars").cloned().unwrap_or(Value::Null) }))
        .collect();
    let att_rows = crate::db_json::query_rows(db, "SELECT attempt_no, outcome, correction_md, validation_errors_json FROM brain.ai_call_attempts WHERE ai_call_id = ? ORDER BY attempt_no", &[json!(ai_call_id)])?;
    let attempts: Vec<Value> = att_rows.as_array().cloned().unwrap_or_default().into_iter().map(|a| {
        let raw_s = a.get("validation_errors_json").and_then(Value::as_str).filter(|s| !s.is_empty());
        let errors_val = raw_s.and_then(|s| serde_json::from_str::<Value>(s).ok());
        let errs: Vec<Value> = match errors_val { Some(Value::Array(arr)) => arr, Some(other) => vec![other], None => vec![] };
        let errs_str: Vec<Value> = errs.iter().map(|e| json!(take_chars(&js_string(e), 240))).collect();
        json!({ "attempt": a.get("attempt_no").cloned().unwrap_or(Value::Null), "outcome": a.get("outcome").cloned().unwrap_or(Value::Null), "errors": errs_str })
    }).collect();

    // DEMANDS — distilled from the call's own structured inputs
    let inputs_rows = crate::db_json::query_rows(db, "SELECT key, value_text FROM brain.ai_call_inputs WHERE ai_call_id = ? ORDER BY order_index, id", &[json!(ai_call_id)])?;
    let inputs: Vec<Value> = inputs_rows.as_array().cloned().unwrap_or_default();

    let mut demands_acceptance: Vec<String> = Vec::new();
    let mut demands_must_obey: Vec<String> = Vec::new();
    let re_acc = regex::Regex::new(r"(^|_)acceptance$").unwrap();
    for i in &inputs {
        let key = tmpl(i.get("key"));
        if re_acc.is_match(&key) || key == "claims" || key == "questions" {
            for it in parse_arr(i.get("value_text")) { demands_acceptance.push(distill(&it)); }
        } else if key == "must_obey" {
            for it in parse_arr(i.get("value_text")) { demands_must_obey.push(distill(&it)); }
        }
    }
    let demands_acceptance: Vec<String> = demands_acceptance.into_iter().filter(|s| !s.is_empty()).map(|s| take_chars(&s, 180)).take(6).collect();
    let demands_must_obey: Vec<String> = demands_must_obey.into_iter().filter(|s| !s.is_empty()).map(|s| take_chars(&s, 160)).take(3).collect();

    let mut demand_goal: Value = Value::Null;
    if demands_acceptance.is_empty() {
        let g = inputs.iter().find(|i| {
            let key = tmpl(i.get("key"));
            (key == "goal_md" || key == "goal" || key == "intent") && !tmpl(i.get("value_text")).trim().is_empty()
        });
        if let Some(g) = g {
            demand_goal = json!(take_chars(&tmpl(g.get("value_text")), 600));
        } else {
            let cb = inputs.iter().find(|i| tmpl(i.get("key")) == "card_body");
            let ct = inputs.iter().find(|i| tmpl(i.get("key")) == "card_title");
            let mut why = String::new();
            let body_raw = cb.and_then(|c| nz(c.get("value_text"))).map(|v| tmpl(Some(v))).unwrap_or_else(|| "{}".to_string());
            if let Ok(b) = serde_json::from_str::<Value>(&body_raw) {
                let w = nz(b.get("why")).or_else(|| nz(b.get("motivation")));
                why = str_qq(w).trim().to_string();
            }
            let ct_str = ct.map(|c| tmpl(c.get("value_text"))).unwrap_or_default();
            let parts: Vec<String> = vec![ct_str.trim().to_string(), why].into_iter().filter(|s| !s.is_empty()).collect();
            if !parts.is_empty() { demand_goal = json!(take_chars(&parts.join(" — "), 600)); }
        }
    }

    let mut demands_gate_criteria: Vec<String> = Vec::new();
    if demands_acceptance.is_empty() && truthy(call.get("prompt_template_id")) {
        if let Ok(rows) = crate::db_json::query_rows(db, "SELECT judging_criteria_json FROM brain.prompt_templates WHERE id = ?", &[call.get("prompt_template_id").cloned().unwrap_or(Value::Null)]) {
            if let Some(jc) = first(&rows).and_then(|r| r.get("judging_criteria_json").cloned()) {
                if truthy(Some(&jc)) {
                    if let Some(s) = jc.as_str() {
                        if let Ok(Value::Array(arr)) = serde_json::from_str::<Value>(s) {
                            demands_gate_criteria = arr.iter().map(|c| take_chars(&tmpl(Some(c)), 200)).take(6).collect();
                        }
                    }
                }
            }
        }
    }

    // CHECKS — deterministic token presence
    let out_rows = crate::db_json::query_rows(db, "SELECT value_text FROM brain.ai_call_outputs WHERE ai_call_id = ? AND value_text IS NOT NULL ORDER BY order_index, id", &[json!(ai_call_id)])?;
    let out_text: String = out_rows.as_array().cloned().unwrap_or_default().iter().map(|o| tmpl(o.get("value_text"))).collect::<Vec<_>>().join("\n");

    let mut tokens: Vec<String> = Vec::new();
    let re_naming = regex::Regex::new(r"decided_names|naming").unwrap();
    let re_tok = regex::Regex::new(r"^[A-Za-z_$][A-Za-z0-9_$.-]{3,60}$").unwrap();
    let re_ext = regex::Regex::new(r"(?i)\.[a-z]+$").unwrap();
    for i in &inputs {
        let key = tmpl(i.get("key"));
        if re_naming.is_match(&key) {
            if let Some(vt) = i.get("value_text").and_then(Value::as_str) {
                if let Ok(parsed) = serde_json::from_str::<Value>(vt) { walk_tokens(&parsed, &re_tok, &mut tokens); }
            }
        }
        if key == "path" {
            if let Some(vt) = i.get("value_text").and_then(Value::as_str) {
                if !vt.is_empty() {
                    let base_name = vt.split(|c| c == '\\' || c == '/').last().unwrap_or("");
                    let stripped = re_ext.replace(base_name, "").to_string();
                    if stripped.chars().count() > 3 && !tokens.iter().any(|x| x == &stripped) { tokens.push(stripped); }
                }
            }
        }
    }
    let checks: Vec<Value> = tokens.iter().take(10).map(|t| json!({ "token": t, "found": out_text.contains(t.as_str()) })).collect();

    // EXCERPT — head+tail of the main output
    let main_rows = crate::db_json::query_rows(db, "SELECT field_name, value_text FROM brain.ai_call_outputs WHERE ai_call_id = ? AND value_text IS NOT NULL ORDER BY LENGTH(value_text) DESC LIMIT 1", &[json!(ai_call_id)])?;
    let main_out = first(&main_rows);
    let mut excerpt = Value::Null;
    if let Some(mo) = &main_out {
        if let Some(vt) = mo.get("value_text").and_then(Value::as_str) {
            if !vt.is_empty() {
                let lines: Vec<&str> = vt.split('\n').collect();
                let truncated = lines.len() > 16;
                let text_lines: Vec<String> = if truncated {
                    let mut v: Vec<String> = lines[..12].iter().map(|s| s.to_string()).collect();
                    v.push(format!("⋯ ({} lines hidden — flip ▸ for everything)", lines.len() - 16));
                    v.extend(lines[lines.len() - 4..].iter().map(|s| s.to_string()));
                    v
                } else {
                    lines.iter().map(|s| s.to_string()).collect()
                };
                let text = take_chars(&text_lines.join("\n"), 1600);
                excerpt = json!({ "field": mo.get("field_name").cloned().unwrap_or(Value::Null), "text": text, "truncated": truncated });
            }
        }
    }

    // RECONCILIATION LEDGER — pair produced items to the demand roster they answer
    let mut reconciliation = Value::Null;
    let mut produced_count: Option<i64> = None;
    {
        let roster_keys = ["block_acceptance", "contract_acceptance", "acceptance", "card_acceptance", "claims", "questions"];
        let mut roster_key: Option<String> = None;
        let mut expected_raw: Vec<Value> = Vec::new();
        for k in roster_keys {
            if let Some(inp) = inputs.iter().find(|i| tmpl(i.get("key")) == k) {
                let arr = parse_arr(inp.get("value_text"));
                if arr.len() >= 2 { roster_key = Some(k.to_string()); expected_raw = arr; break; }
            }
        }
        let mut produced_field: Option<String> = None;
        let mut produced_raw: Vec<Value> = Vec::new();
        let out2_rows = crate::db_json::query_rows(db, "SELECT field_name, value_text FROM brain.ai_call_outputs WHERE ai_call_id = ? AND value_text IS NOT NULL", &[json!(ai_call_id)])?;
        for o in out2_rows.as_array().cloned().unwrap_or_default() {
            let vt = match o.get("value_text").and_then(Value::as_str) { Some(s) => s.trim(), None => continue };
            let parsed = match serde_json::from_str::<Value>(vt) { Ok(v) => v, Err(_) => continue };
            match &parsed {
                Value::Array(arr) => { if arr.len() > produced_raw.len() { produced_field = Some(tmpl(o.get("field_name"))); produced_raw = arr.clone(); } }
                Value::Object(map) => {
                    for (prop, vv) in map {
                        if let Value::Array(arr2) = vv {
                            if arr2.len() > produced_raw.len() { produced_field = Some(format!("{}.{}", tmpl(o.get("field_name")), prop)); produced_raw = arr2.clone(); }
                        }
                    }
                }
                _ => {}
            }
        }
        if produced_field.is_some() { produced_count = Some(produced_raw.len() as i64); }

        if let (Some(rk), Some(pf)) = (roster_key.clone(), produced_field.clone()) {
            if expected_raw.len() >= 2 && produced_raw.len() >= 1 {
                let id_of = |it: &Value, text: &str, prefer_refs: bool| -> Option<String> {
                    let cand = if prefer_refs {
                        nz(it.get("acceptance_id")).or_else(|| nz(it.get("card_acceptance_id"))).or_else(|| nz(it.get("ref"))).or_else(|| nz(it.get("for"))).or_else(|| nz(it.get("id"))).or_else(|| nz(it.get("key")))
                    } else {
                        nz(it.get("id")).or_else(|| nz(it.get("key"))).or_else(|| nz(it.get("ref"))).or_else(|| nz(it.get("acceptance_id")))
                    };
                    if let Some(c) = cand { return Some(js_string(c)); }
                    let re = regex::Regex::new(r"(?i)^([a-z]{1,4}\d{1,3})\b").unwrap();
                    re.captures(text).map(|m| m.get(1).unwrap().as_str().to_string())
                };
                let stop: std::collections::HashSet<&str> = ["the", "that", "with", "this", "from", "should", "must", "when", "then", "into", "each", "every", "have", "will", "been", "shall", "does", "exactly"].into_iter().collect();
                let re_word = regex::Regex::new(r"[A-Za-z_][A-Za-z0-9_'-]{3,}").unwrap();
                let toks = |t: &str| -> std::collections::HashSet<String> {
                    re_word.find_iter(t).map(|m| m.as_str().to_lowercase()).filter(|w| !stop.contains(w.as_str())).collect()
                };
                struct Item { idx: usize, id: Option<String>, text: String, full: Option<Value> }
                let expected: Vec<Item> = expected_raw.iter().take(30).enumerate().map(|(i, it)| {
                    let text = take_chars(&distill(it), 220);
                    Item { idx: i, id: id_of(it, &text, false), text, full: None }
                }).collect();
                let produced: Vec<Item> = produced_raw.iter().take(30).enumerate().map(|(i, it)| {
                    let text = take_chars(&distill(it), 220);
                    Item { idx: i, id: id_of(it, &text, true), text, full: Some(it.clone()) }
                }).collect();
                let mut pairs: Vec<Value> = Vec::new();
                let mut used_e: std::collections::HashSet<usize> = std::collections::HashSet::new();
                let mut used_p: std::collections::HashSet<usize> = std::collections::HashSet::new();
                // pass 1: exact id echo
                for p in &produced {
                    if used_p.contains(&p.idx) { continue; }
                    let Some(pid) = &p.id else { continue };
                    if let Some(e) = expected.iter().find(|x| !used_e.contains(&x.idx) && x.id.as_ref().map(|xi| xi.to_lowercase()) == Some(pid.to_lowercase())) {
                        pairs.push(json!({ "expected_idx": e.idx, "produced_idx": p.idx, "method": "id" }));
                        used_e.insert(e.idx); used_p.insert(p.idx);
                    }
                }
                // pass 2: an expected id referenced inside the produced text
                for p in &produced {
                    if used_p.contains(&p.idx) { continue; }
                    let found = expected.iter().find(|x| {
                        if used_e.contains(&x.idx) { return false; }
                        let Some(xid) = &x.id else { return false };
                        let re = match regex::Regex::new(&format!(r"(?i)\b{}\b", regex::escape(xid))) { Ok(r) => r, Err(_) => return false };
                        let full_str = p.full.as_ref().map(|f| serde_json::to_string(f).unwrap_or_default()).unwrap_or_default();
                        re.is_match(&format!("{} {}", p.text, full_str))
                    });
                    if let Some(e) = found {
                        pairs.push(json!({ "expected_idx": e.idx, "produced_idx": p.idx, "method": "id-ref" }));
                        used_e.insert(e.idx); used_p.insert(p.idx);
                    }
                }
                // pass 3: conservative token overlap (>= 2 distinctive shared tokens)
                for p in &produced {
                    if used_p.contains(&p.idx) { continue; }
                    let pt2 = toks(&p.text);
                    let mut best: Option<usize> = None;
                    let mut best_n = 0usize;
                    for e in &expected {
                        if used_e.contains(&e.idx) { continue; }
                        let n = toks(&e.text).iter().filter(|w| pt2.contains(*w)).count();
                        if n > best_n { best_n = n; best = Some(e.idx); }
                    }
                    if let Some(bidx) = best { if best_n >= 2 {
                        pairs.push(json!({ "expected_idx": bidx, "produced_idx": p.idx, "method": "token" }));
                        used_e.insert(bidx); used_p.insert(p.idx);
                    }}
                }
                let gaps: Vec<Value> = expected.iter().filter(|e| !used_e.contains(&e.idx)).map(|e| json!(e.idx)).collect();
                let extras: Vec<Value> = produced.iter().filter(|p| !used_p.contains(&p.idx)).map(|p| json!(p.idx)).collect();
                let expansion = extras.len() > pairs.len().max(1);
                reconciliation = json!({
                    "roster_key": rk, "produced_field": pf,
                    "expected": expected.iter().map(|e| json!({ "idx": e.idx, "id": e.id, "text": e.text })).collect::<Vec<_>>(),
                    "produced": produced.iter().map(|p| json!({ "idx": p.idx, "id": p.id, "text": p.text })).collect::<Vec<_>>(),
                    "pairs": pairs, "gaps": gaps, "extras": extras,
                    "coverage": { "addressed": used_e.len(), "total": expected.len() },
                    "expansion": expansion,
                });
            }
        }
    }

    // SIGNALS — every recorded judgment/error, deterministic order
    let mut signals: Vec<Value> = Vec::new();
    let got: std::collections::HashSet<String> = received_fields.iter().filter(|f| f.get("chars").and_then(Value::as_i64).unwrap_or(0) > 0).map(|f| tmpl(f.get("field"))).collect();
    for f in &expected_fields {
        let required = f.get("required").and_then(Value::as_bool).unwrap_or(false);
        let fname = tmpl(f.get("field"));
        if required && !got.contains(&fname) {
            signals.push(json!({ "kind": "missing_field", "text": format!("required field '{}' is missing/empty in the output", fname) }));
        }
    }
    let n_want_inp = inputs.iter().find(|i| { let k = tmpl(i.get("key")); ["n_claims", "n_items", "n_questions", "count"].contains(&k.as_str()) });
    if let Some(nwi) = n_want_inp {
        let n_want: f64 = tmpl(nwi.get("value_text")).trim().parse().unwrap_or(f64::NAN);
        if n_want.is_finite() && n_want > 0.0 {
            if let Some(pc) = produced_count {
                if pc as f64 != n_want {
                    signals.push(json!({ "kind": "coverage", "text": format!("produced {} item(s) but {} were requested ({})", pc, n_want as i64, tmpl(nwi.get("key"))) }));
                }
            }
        }
    }
    if let Value::Object(_) = &reconciliation {
        let gaps_len = reconciliation.get("gaps").and_then(Value::as_array).map(|a| a.len()).unwrap_or(0);
        if gaps_len > 0 {
            let addressed = reconciliation.get("coverage").and_then(|c| c.get("addressed")).and_then(Value::as_u64).unwrap_or(0);
            let total = reconciliation.get("coverage").and_then(|c| c.get("total")).and_then(Value::as_u64).unwrap_or(0);
            signals.push(json!({ "kind": "coverage", "text": format!("covers {}/{} demands — {} unaddressed (see the ledger)", addressed, total, gaps_len) }));
        }
    }
    if truthy(call.get("outcome_class")) {
        let oc = tmpl(call.get("outcome_class"));
        if oc != "clean" && oc != "cached" {
            signals.push(json!({ "kind": "detector", "text": format!("outcome detector flagged: {}", oc) }));
        }
    }
    for a in &attempts {
        if let Some(errs) = a.get("errors").and_then(Value::as_array) {
            for e in errs {
                signals.push(json!({ "kind": "validation", "text": format!("attempt {}: {}", tmpl(a.get("attempt")), tmpl(Some(e))) }));
            }
        }
    }
    let ev_rows = crate::db_json::query_rows(db, "SELECT provenance, outcome FROM brain.heimdall_evidence WHERE ai_call_id = ? ORDER BY id", &[json!(ai_call_id)])?;
    for ev in ev_rows.as_array().cloned().unwrap_or_default() {
        if ev.get("outcome").and_then(Value::as_str) == Some("burn") {
            signals.push(json!({ "kind": "burn", "text": format!("downstream court: the artifact BURNED ({})", tmpl(ev.get("provenance"))) }));
        }
    }
    let aitag_rows = crate::db_json::query_rows(db, "SELECT verdict, confidence, failure_class, reason_md FROM brain.heimdall_ai_tags WHERE target_ai_call_id = ? AND superseded_by_human = 0", &[json!(ai_call_id)])?;
    if let Some(at) = first(&aitag_rows) {
        let verdict = at.get("verdict").and_then(Value::as_i64).unwrap_or(0);
        let mut text = format!("AI judge: {}{}", if verdict > 0 { "+" } else { "" }, verdict);
        if let Some(c) = at.get("confidence").and_then(Value::as_f64) { text.push_str(&format!(" @{:.2}", c)); }
        if truthy(at.get("failure_class")) { text.push_str(&format!(" · {}", tmpl(at.get("failure_class")))); }
        if truthy(at.get("reason_md")) { text.push_str(&format!(" — {}", take_chars(&tmpl(at.get("reason_md")), 200))); }
        signals.push(json!({ "kind": "ai_hint", "text": text }));
    }
    if truthy(call.get("contract_slug")) {
        if let Ok(er_rows) = crate::db_json::query_rows(db, "SELECT error_code, error_message, pattern_key FROM memory.error_events WHERE related_contract_slug = ? ORDER BY id DESC LIMIT 5", &[call.get("contract_slug").cloned().unwrap_or(Value::Null)]) {
            for er in er_rows.as_array().cloned().unwrap_or_default() {
                let label = nz(er.get("pattern_key")).map(|v| tmpl(Some(v))).unwrap_or_else(|| tmpl(er.get("error_code")));
                signals.push(json!({ "kind": "error_event", "text": format!("session error [{}]: {}", label, take_chars(&str_qq(er.get("error_message")), 200)) }));
            }
        }
    }

    let gate_val = nz(casting.as_ref().and_then(|c| c.get("template_base"))).or_else(|| nz(call.get("template_name"))).cloned().unwrap_or(json!("?"));

    Ok(json!({
        "ok": true,
        "pretag": {
            "gate": gate_val,
            "category": casting.as_ref().and_then(|c| c.get("category")).cloned().unwrap_or(Value::Null),
            "category_title": cat_title,
            "expected": { "fields": expected_fields },
            "demands": { "acceptance": demands_acceptance, "must_obey": demands_must_obey, "goal": demand_goal, "gate_criteria": demands_gate_criteria },
            "checks": checks, "excerpt": excerpt, "reconciliation": reconciliation,
            "received": {
                "outcome_class": call.get("outcome_class").cloned().unwrap_or(Value::Null),
                "duration_ms": call.get("duration_ms").cloned().unwrap_or(Value::Null),
                "tokens_in": call.get("tokens_in").cloned().unwrap_or(Value::Null),
                "tokens_out": call.get("tokens_out").cloned().unwrap_or(Value::Null),
                "fields": received_fields, "attempts": attempts,
            },
            "signals": signals,
        }
    }))
}

fn collect_demand_list(pre: &Value) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(d) = pre.get("pretag").and_then(|p| p.get("demands")) {
        if let Some(acc) = d.get("acceptance").and_then(Value::as_array) { out.extend(acc.iter().map(|v| tmpl(Some(v)))); }
        if let Some(mo) = d.get("must_obey").and_then(Value::as_array) { out.extend(mo.iter().map(|v| format!("obey: {}", tmpl(Some(v))))); }
        if let Some(gc) = d.get("gate_criteria").and_then(Value::as_array) { out.extend(gc.iter().map(|v| tmpl(Some(v)))); }
    }
    out.truncate(10);
    out
}

pub fn heimdallTagAssist(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS: `const { db, provider_fn = null } = context ?? {}; if (!db) throw err.contextMissing(...)`
    // — unreachable here (Rust's `db: &Connection` is never absent).
    let ai_call_id_num = js_number(input.get("ai_call_id"));
    if !ai_call_id_num.is_finite() || ai_call_id_num <= 0.0 {
        return Err("heimdall-tag-assist: ai_call_id is required".to_string());
    }
    let ai_call_id = ai_call_id_num as i64;
    let op = tmpl(input.get("op"));
    if !OPS.contains(&op.as_str()) {
        return Err(format!("heimdall-tag-assist: op must be one of [{}], got '{}'", OPS.join(", "), op));
    }

    if op == "pretag" { return pretag(db, ai_call_id); } // deterministic — no AI, no cost
    if op == "context" { return tag_context(db, ai_call_id); } // deterministic — the sidebar's memory

    let ctx = crate::heimdall_heimdall_ai_tag::targetContext(db, ai_call_id)?;
    if ctx.is_null() {
        return Ok(json!({ "ok": true, "skipped": true, "reason": "call not found" }));
    }
    ensure_prompts(db)?;

    let (res, field, demand_list_for_claims) = if op == "analyze" {
        let pre = pretag(db, ai_call_id)?;
        let demand_list = collect_demand_list(&pre);
        let demands_text = if demand_list.is_empty() {
            "(none declared — emit 3-6 claims about the most decidable properties, numbered D1..)".to_string()
        } else {
            demand_list.iter().enumerate().map(|(i, d)| format!("D{}. {}", i + 1, d)).collect::<Vec<_>>().join("\n")
        };
        let outputs_str = nz(ctx.get("outputs")).map(|v| tmpl(Some(v))).unwrap_or_else(|| "(empty output)".to_string());
        let ai_input = json!({
            "prompt_name": ANALYZE_PROMPT,
            "input_kv": [
                { "key": "demands", "value": demands_text },
                { "key": "task_input", "value": take_chars(&tmpl(ctx.get("rendered")), 8000) },
                { "key": "target_output", "value": take_chars(&outputs_str, 8000) },
            ],
            "tier": "high", "temperature": "cold", "phase_flow": "single_only", "two_phase": false, "structured_output": false, "provider_fn": Value::Null,
        });
        // PARITY-DEFERRED: aiCall always returns Err on this stub — `?` short-circuits here.
        (crate::ai_ai_call::aiCall(&ai_input, db)?, "analysis_md", demand_list)
    } else {
        let is_input = op == "summarize_input";
        let content_str = nz(if is_input { ctx.get("rendered") } else { ctx.get("outputs") }).map(|v| tmpl(Some(v))).unwrap_or_else(|| "(empty)".to_string());
        let ai_input = json!({
            "prompt_name": SUM_PROMPT,
            "input_kv": [
                { "key": "what", "value": if is_input { "the task the model received" } else { "the model's output" } },
                { "key": "content", "value": take_chars(&content_str, 9000) },
            ],
            "tier": "mid", "temperature": "cold", "provider_fn": Value::Null,
        });
        // PARITY-DEFERRED: aiCall always returns Err on this stub — `?` short-circuits here.
        (crate::ai_ai_call::aiCall(&ai_input, db)?, "summary_md", Vec::new())
    };

    // Unreachable past this point on the deferred stub — ported byte-for-byte from the JS,
    // correctness by inspection (mirrors book-run.rs / heimdall-ai-tag.rs).
    let text = res.get("output_kv").and_then(Value::as_array)
        .and_then(|a| a.iter().find(|o| o.get("field_name").and_then(Value::as_str) == Some(field)))
        .and_then(|o| o.get("value_text").cloned())
        .or_else(|| res.get("output_kv").and_then(Value::as_array).and_then(|a| a.iter().find(|o| !o.get("value_text").map_or(true, |v| v.is_null()))).and_then(|o| o.get("value_text").cloned()))
        .unwrap_or(json!("(empty reply)"));

    let mut claims: Vec<Value> = Vec::new();
    if op == "analyze" {
        let pre2 = pretag(db, ai_call_id)?;
        let demand_list2 = collect_demand_list(&pre2);
        let _ = &demand_list_for_claims; // the JS re-derives demandList a second time for the claims transport
        let tone_of = |t: &str| -> &'static str { match t.to_lowercase().as_str() { "met" | "ok" => "ok", "missed" | "bad" => "bad", _ => "warn" } };
        let re = regex::Regex::new(r"(?im)^CLAIM\|(?:(D\d+|extra)\|)?(met|missed|unsure|ok|bad|warn)\|(.+)$").unwrap();
        let text_str = tmpl(Some(&text));
        claims = re.captures_iter(&text_str).take(12).map(|m| {
            let tag = m.get(1).map(|x| x.as_str());
            let tone_raw = m.get(2).unwrap().as_str();
            let content = m.get(3).unwrap().as_str().trim();
            let demand: Value = match tag {
                Some(t) if !t.eq_ignore_ascii_case("extra") => {
                    let digits = &t[1..];
                    let d_idx: i64 = digits.parse::<i64>().unwrap_or(0) - 1;
                    if d_idx >= 0 && (d_idx as usize) < demand_list2.len() && !demand_list2[d_idx as usize].is_empty() {
                        json!(demand_list2[d_idx as usize].clone())
                    } else { Value::Null }
                }
                Some(_) => json!("(beyond the demands)"),
                None => Value::Null,
            };
            json!({ "demand": demand, "tone": tone_of(tone_raw), "text": take_chars(content, 300) })
        }).collect();
    }

    Ok(json!({ "ok": true, "text": text, "claims": claims, "assist_ai_call_id": res.get("ai_call_id").cloned().unwrap_or(Value::Null) }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS brain;").unwrap();
        c.execute_batch(
            "CREATE TABLE brain.ai_calls (id INTEGER PRIMARY KEY AUTOINCREMENT, ts TEXT, tier TEXT, model_id TEXT, contract_slug TEXT, prompt_template_id INTEGER, cost_usd_actual REAL, tokens_in INTEGER, tokens_out INTEGER, duration_ms INTEGER, outcome TEXT, outcome_class TEXT, session_id TEXT, cached INTEGER DEFAULT 0, cache_source_call_id INTEGER);
             CREATE TABLE brain.ai_call_inputs (id INTEGER PRIMARY KEY AUTOINCREMENT, ai_call_id INTEGER, key TEXT, value_text TEXT, order_index INTEGER);
             CREATE TABLE brain.ai_call_outputs (id INTEGER PRIMARY KEY AUTOINCREMENT, ai_call_id INTEGER, field_name TEXT, value_text TEXT, order_index INTEGER);
             CREATE TABLE brain.ai_call_attempts (id INTEGER PRIMARY KEY AUTOINCREMENT, ai_call_id INTEGER, attempt_no INTEGER, outcome TEXT, correction_md TEXT, validation_errors_json TEXT);
             CREATE TABLE brain.prompt_templates ( id INTEGER PRIMARY KEY, name TEXT NOT NULL, version INTEGER NOT NULL, template_md TEXT NOT NULL, default_tier TEXT NOT NULL CHECK (default_tier IN ('low','mid','high')), default_temperature TEXT NOT NULL CHECK (default_temperature IN ('freezed','cold','warm','hot','hell')), max_output_tokens INTEGER NOT NULL DEFAULT 1024, superseded_by INTEGER, created_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')), cross_field_rules_json TEXT, judging_criteria_json TEXT, system_md TEXT, phase_flow TEXT NOT NULL DEFAULT 'single', UNIQUE (name, version) );
             CREATE TABLE brain.prompt_output_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL CHECK (field_type IN ('string','number','boolean','enum','array_of_string','array_of_object')), enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, grounded_refs_json TEXT, UNIQUE (prompt_template_id, field_name) );
             CREATE TABLE brain.prompt_input_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL CHECK (field_type IN ('string','number','boolean','enum','array_of_string','array_of_object')), enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, UNIQUE (prompt_template_id, field_name) );
             CREATE TABLE brain.heimdall_categories (slug TEXT PRIMARY KEY, title TEXT);
             CREATE TABLE brain.heimdall_castings (id INTEGER PRIMARY KEY AUTOINCREMENT, ai_call_id INTEGER, category TEXT, template_base TEXT);
             CREATE TABLE brain.heimdall_human_tags (id INTEGER PRIMARY KEY AUTOINCREMENT, ai_call_id INTEGER, verdict TEXT);
             CREATE TABLE brain.heimdall_ai_tags (id INTEGER PRIMARY KEY AUTOINCREMENT, target_ai_call_id INTEGER UNIQUE, verdict INTEGER, confidence REAL, reason_md TEXT, failure_class TEXT, low_confidence INTEGER, panel_json TEXT, panel_agree INTEGER, judge_ai_call_id INTEGER, judge_model TEXT, superseded_by_human INTEGER NOT NULL DEFAULT 0, ts TEXT);
             CREATE TABLE brain.heimdall_evidence (id INTEGER PRIMARY KEY AUTOINCREMENT, model_slug TEXT, category TEXT, template_base TEXT, provenance TEXT, outcome TEXT, ai_call_id INTEGER, ai_tag_id INTEGER, human_tag_id INTEGER);
             CREATE TABLE brain.heimdall_prompt_tags (id INTEGER PRIMARY KEY AUTOINCREMENT, template_name TEXT, verdict TEXT, source TEXT, reason_md TEXT);
             CREATE TABLE contracts (id INTEGER PRIMARY KEY AUTOINCREMENT, slug TEXT, card_slug TEXT, status TEXT, current_attempt INTEGER, rejection_reason_md TEXT, worktree_path TEXT, goal_md TEXT, acceptance_json TEXT);
             CREATE TABLE cards (slug TEXT PRIMARY KEY, title TEXT, status TEXT, body_json TEXT);",
        ).unwrap();
        c
    }

    fn a_call(c: &Connection) -> i64 {
        c.execute("INSERT INTO brain.ai_calls (model_id) VALUES ('m')", []).unwrap();
        c.last_insert_rowid()
    }

    // ── guards ──────────────────────────────────────────────────────────
    #[test]
    fn ai_call_id_required() {
        let c = setup();
        assert!(heimdallTagAssist(&json!({}), &c).unwrap_err().contains("ai_call_id is required"));
        assert!(heimdallTagAssist(&json!({ "ai_call_id": -1, "op": "pretag" }), &c).unwrap_err().contains("ai_call_id is required"));
    }

    #[test]
    fn invalid_op_is_rejected() {
        let c = setup();
        let cid = a_call(&c);
        assert!(heimdallTagAssist(&json!({ "ai_call_id": cid, "op": "bogus" }), &c).unwrap_err().contains("op must be one of"));
    }

    // ── op:'pretag' ─────────────────────────────────────────────────────
    #[test]
    fn pretag_call_not_found_is_skipped() {
        let c = setup();
        let r = heimdallTagAssist(&json!({ "ai_call_id": 999, "op": "pretag" }), &c).unwrap();
        assert_eq!(r, json!({ "ok": true, "skipped": true, "reason": "call not found" }));
    }

    #[test]
    fn pretag_flags_missing_required_field_and_gate_falls_back_to_template_name() {
        let c = setup();
        c.execute("INSERT INTO brain.prompt_templates (id, name, version, template_md, default_tier, default_temperature) VALUES (1, 'gate.v3', 1, 't', 'mid', 'cold')", []).unwrap();
        c.execute("INSERT INTO brain.ai_calls (model_id, prompt_template_id) VALUES ('m', 1)", []).unwrap();
        let cid = c.last_insert_rowid();
        c.execute("INSERT INTO brain.prompt_output_fields (prompt_template_id, field_name, field_type, required, order_index) VALUES (1, 'answer', 'string', 1, 0)", []).unwrap();
        let r = heimdallTagAssist(&json!({ "ai_call_id": cid, "op": "pretag" }), &c).unwrap();
        assert_eq!(r["pretag"]["gate"], json!("gate.v3")); // no casting row -> raw template_name (pretag does not strip .vN)
        let signals = r["pretag"]["signals"].as_array().unwrap();
        assert!(signals.iter().any(|s| s["kind"] == "missing_field" && s["text"].as_str().unwrap().contains("'answer'")));
    }

    #[test]
    fn pretag_demands_and_checks_from_inputs() {
        let c = setup();
        let cid = a_call(&c);
        c.execute("INSERT INTO brain.ai_call_inputs (ai_call_id, key, value_text, order_index) VALUES (?1, 'acceptance', '[\"Do the thing\",\"Do it well\"]', 0)", [cid]).unwrap();
        c.execute("INSERT INTO brain.ai_call_inputs (ai_call_id, key, value_text, order_index) VALUES (?1, 'path', 'src/FooBar.mjs', 1)", [cid]).unwrap();
        c.execute("INSERT INTO brain.ai_call_outputs (ai_call_id, field_name, value_text, order_index) VALUES (?1, 'out', 'wrote FooBar cleanly', 0)", [cid]).unwrap();
        let r = heimdallTagAssist(&json!({ "ai_call_id": cid, "op": "pretag" }), &c).unwrap();
        assert_eq!(r["pretag"]["demands"]["acceptance"], json!(["Do the thing", "Do it well"]));
        let checks = r["pretag"]["checks"].as_array().unwrap();
        assert!(checks.iter().any(|ck| ck["token"] == "FooBar" && ck["found"] == json!(true)));
    }

    // ── op:'context' ────────────────────────────────────────────────────
    #[test]
    fn context_call_not_found_is_skipped() {
        let c = setup();
        let r = heimdallTagAssist(&json!({ "ai_call_id": 999, "op": "context" }), &c).unwrap();
        assert_eq!(r, json!({ "ok": true, "skipped": true, "reason": "call not found" }));
    }

    #[test]
    fn context_assembles_call_and_gate_without_a_card() {
        let c = setup();
        let cid = a_call(&c);
        let r = heimdallTagAssist(&json!({ "ai_call_id": cid, "op": "context" }), &c).unwrap();
        assert_eq!(r["context"]["call"]["id"], json!(cid));
        assert_eq!(r["context"]["card"], Value::Null);
        assert_eq!(r["context"]["contract"], Value::Null);
    }

    // ── op:'summarize_input' / 'analyze' — reach the PARITY-DEFERRED aiCall boundary ────
    #[test]
    fn summarize_input_reaches_the_deferred_aicall_boundary() {
        let c = setup();
        c.execute("INSERT INTO brain.prompt_templates (id, name, version, template_md, default_tier, default_temperature) VALUES (1, 'gate.v1', 1, 'Say hi to {{who}}.', 'mid', 'cold')", []).unwrap();
        let cid = a_call(&c);
        c.execute("UPDATE brain.ai_calls SET prompt_template_id = 1 WHERE id = ?1", [cid]).unwrap();
        c.execute("INSERT INTO brain.ai_call_inputs (ai_call_id, key, value_text, order_index) VALUES (?1, 'who', 'Dane', 0)", [cid]).unwrap();
        let err = heimdallTagAssist(&json!({ "ai_call_id": cid, "op": "summarize_input" }), &c).unwrap_err();
        assert!(err.contains("ai-call"), "unexpected error: {err}");
        // ensure_prompts ran and registered the summarize gate before the deferred call
        let n: i64 = c.query_row("SELECT COUNT(*) FROM brain.prompt_templates WHERE name = 'heimdall.assist.summarize.v1'", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn analyze_reaches_the_deferred_aicall_boundary_after_pretag_runs() {
        let c = setup();
        let cid = a_call(&c);
        let err = heimdallTagAssist(&json!({ "ai_call_id": cid, "op": "analyze" }), &c).unwrap_err();
        assert!(err.contains("ai-call"), "unexpected error: {err}");
        let n: i64 = c.query_row("SELECT COUNT(*) FROM brain.prompt_templates WHERE name = 'heimdall.assist.analyze.v1'", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1);
    }

    // ── parse_arr / distill (pure) ──────────────────────────────────────
    #[test]
    fn parse_arr_handles_array_object_and_garbage() {
        assert_eq!(parse_arr(Some(&json!("[1,2]"))), vec![json!(1), json!(2)]);
        assert_eq!(parse_arr(Some(&json!("{\"a\":1,\"b\":2}"))), vec![json!(1), json!(2)]);
        assert_eq!(parse_arr(Some(&json!("not json"))), Vec::<Value>::new());
        assert_eq!(parse_arr(None), Vec::<Value>::new());
    }

    #[test]
    fn distill_prefers_text_field_then_falls_back_to_json() {
        assert_eq!(distill(&json!("plain")), "plain");
        assert_eq!(distill(&json!({ "text": "the ask" })), "the ask");
        assert_eq!(distill(&json!({ "foo": "bar" })), "{\"foo\":\"bar\"}");
    }
}
