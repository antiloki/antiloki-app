//! heimdall-prompt-tags.rs — twin of skills/heimdall/heimdall-prompt-tags.mjs (db READ+WRITE
//! lane, brain-attached, op-dispatched: list | tag | aitag). NAMES ARE CONTRACT:
//! `heimdallPromptTags` (default). The PROMPT tagging dimension — distinct from model tagging —
//! rates the PROMPT VERSION's design, never the model's performance under it.
//! `op:'list'` is pure reads + deterministic aggregation (trust-weighted combined score, 30d
//! usage analytics, failure list, insight sentences). `op:'tag'` is a human-verdict write
//! (latest-wins per template_name via delete-then-insert). `op:'aitag'` composes ensureJudgePrompt
//! (promptFind/promptAdd, both real twins) + `aiCall` (PARITY-DEFERRED — see ai_ai_call::aiCall
//! doc: the network round-trip is unfixturable) — the JS awaits aiCall with no surrounding
//! try/catch, so its rejection PROPAGATES unchanged (`?`) rather than being caught into a status
//! object. `parseJudgeLine` is reused from the already-converted heimdall-ai-tag twin (pure).
//! FROZEN-CLOCK LANE: `datetime('now','-30 days')` pinned to the frozen instant 2026-01-01.
use rusqlite::Connection;
use serde_json::{json, Map, Value};

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
fn as_f64(v: Option<&Value>) -> f64 {
    v.and_then(Value::as_f64).unwrap_or(0.0)
}
fn as_i64(v: Option<&Value>) -> i64 {
    v.and_then(|x| x.as_i64().or_else(|| x.as_f64().map(|f| f as i64))).unwrap_or(0)
}
fn take_n(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

const TRUST_HUMAN: f64 = 1.0;
const TRUST_AI: f64 = 0.4;
const PROMPT_CLASSES: [&str; 7] = ["ambiguous", "missing-context", "contradictory", "overlong", "wrong-constraints", "format-trap", "other"];
const JUDGE_PROMPT: &str = "heimdall.prompt.judge.v1";

const SYSTEM: &str = "You are HEIMDALL'S PROMPT AUDITOR. You rate the DESIGN OF A PROMPT TEMPLATE — never the model's performance under it. A bad output from a well-designed prompt is the model's fault; a good output despite a confusing prompt is luck. You judge the words the prompt author wrote.";

fn template_md() -> String {
    "Rate the PROMPT TEMPLATE below on a 4-point scale. You are judging the PROMPT'S DESIGN, using one real exchange only as context for how the prompt behaves in practice.\n\n═══ THE SCALE (one integer; NO zero) ═══\n+2 EXCELLENT — task crystal clear, context sufficient, constraints right, output contract unambiguous.\n+1 GOOD      — does its job; minor wording/structure nits.\n-1 WEAK      — a real design flaw: an ambiguity, a missing piece of context the task needs, a confusing constraint.\n-2 WRONG     — misleading, self-contradictory, demands the impossible, or sets a format trap.\n\n═══ WHAT TO CHECK ═══\ntask clarity (is the ask decidable?) · context sufficiency (does it PROVIDE what it demands?) ·\ninternal contradictions (rules that fight each other) · constraint fit (rules that cause the very\nfailures they prevent) · output contract (is the expected reply shape unmistakable?) · economy\n(does length earn its tokens?).\n\n═══ THE PROMPT TEMPLATE UNDER REVIEW ({{template_name}}) ═══\n{{template_md}}\n\n(review pass {{judge_pass}})\n\n═══ ONE REAL EXCHANGE UNDER THIS PROMPT (context only — do NOT rate the model) ═══\n--- rendered input ---\n{{exemplar_input}}\n--- model output ---\n{{exemplar_output}}\n\nThink it through, then END with EXACTLY ONE line (nothing after it):\nVERDICT=<-2|-1|1|2> CONFIDENCE=<0.00-1.00> CLASS=<class> REASON=<one decidable sentence about the PROMPT>\n- ALWAYS include CONFIDENCE.\n- CLASS for NEGATIVE verdicts: ambiguous · missing-context · contradictory · overlong ·\n  wrong-constraints · format-trap · other. For POSITIVE verdicts write none.".to_string()
}

fn ensure_judge_prompt(db: &Connection) -> Result<(), String> {
    let existing = crate::ai_prompt_find::promptFind(&json!({ "name": JUDGE_PROMPT }), db).ok();
    if let Some(e) = &existing {
        if e.get("template_md").and_then(Value::as_str) == Some(template_md().as_str()) {
            return Ok(());
        }
    }
    crate::ai_prompt_add::promptAdd(
        &json!({
            "name": JUDGE_PROMPT, "template_md": template_md(), "system_md": SYSTEM,
            "default_tier": "high", "default_temperature": "cold", "max_output_tokens": 900,
            "output_fields": [{ "field_name": "judge_raw", "field_type": "string", "description": "reasoning + the final VERDICT line" }],
            "judging_criteria": [
                "Ends with exactly one VERDICT=… CONFIDENCE=… CLASS=… REASON=… line.",
                "Rates the PROMPT design only — never the model output quality.",
                "The reason names a concrete design property (ambiguity, missing context, format trap).",
            ],
        }),
        db,
    )?;
    Ok(())
}

fn list(db: &Connection) -> Result<Value, String> {
    let rows_v = crate::db_json::query_rows(
        db,
        &format!("SELECT p.id AS template_id, p.name AS template_name, p.template_md, p.system_md, p.default_tier, p.default_temperature, p.judging_criteria_json,
                (SELECT COUNT(*) FROM brain.ai_calls a WHERE a.prompt_template_id = p.id AND a.cached = 0 AND a.ts >= datetime({},'-30 days')) AS calls_30d,
                (SELECT MAX(a2.id) FROM brain.ai_calls a2 WHERE a2.prompt_template_id = p.id AND a2.cached = 0) AS exemplar_ai_call_id
         FROM brain.prompt_templates p
         WHERE p.superseded_by IS NULL
         ORDER BY calls_30d DESC, p.name", crate::clock::now_sql_inner()),
        &[],
    )?;
    let rows = rows_v.as_array().cloned().unwrap_or_default();

    let mut out: Vec<Value> = Vec::with_capacity(rows.len());
    for r in &rows {
        let template_name = str_qq(r.get("template_name"));
        let template_id = r.get("template_id").cloned().unwrap_or(Value::Null);
        let exemplar_ai_call_id = r.get("exemplar_ai_call_id").cloned().unwrap_or(Value::Null);

        let ex_tok = if truthy(Some(&exemplar_ai_call_id)) {
            crate::db_json::query_rows(db, "SELECT tokens_in, tokens_out FROM brain.ai_calls WHERE id = ?", &[exemplar_ai_call_id.clone()])?
                .as_array().and_then(|a| a.first()).cloned()
        } else {
            None
        };
        let judge_est_tokens = (500.0
            + (str_qq(r.get("template_md")).chars().count().min(8000) as f64) / 4.0
            + as_f64(ex_tok.as_ref().and_then(|t| t.get("tokens_in"))).min(1500.0)
            + as_f64(ex_tok.as_ref().and_then(|t| t.get("tokens_out"))).min(1000.0)
            + 600.0)
            .round() as i64;

        let human = crate::db_json::query_rows(
            db,
            "SELECT verdict, reason_md, ts FROM brain.heimdall_prompt_tags WHERE template_name = ? AND source = 'human' ORDER BY id DESC LIMIT 1",
            &[json!(template_name)],
        )?.as_array().and_then(|a| a.first()).cloned();
        let ai_agg = crate::db_json::query_rows(
            db,
            "SELECT COUNT(*) AS n, AVG(verdict) AS avg_verdict, AVG(confidence) AS avg_confidence FROM brain.heimdall_prompt_tags WHERE template_name = ? AND source = 'ai'",
            &[json!(template_name)],
        )?.as_array().and_then(|a| a.first()).cloned().unwrap_or(json!({}));
        let latest_ai = crate::db_json::query_rows(
            db,
            "SELECT verdict, confidence, failure_class, reason_md, judge_ai_call_id, ts FROM brain.heimdall_prompt_tags WHERE template_name = ? AND source = 'ai' ORDER BY id DESC LIMIT 1",
            &[json!(template_name)],
        )?.as_array().and_then(|a| a.first()).cloned();

        let u = crate::db_json::query_rows(
            db,
            &format!("SELECT COUNT(*) AS total, COALESCE(SUM(cached),0) AS cached,
                    SUM(CASE WHEN cached = 0 THEN 1 ELSE 0 END) AS live,
                    ROUND(SUM(COALESCE(cost_usd_actual,0)),4) AS cost_usd,
                    COALESCE(SUM(tokens_in),0) AS tokens_in, COALESCE(SUM(tokens_out),0) AS tokens_out,
                    SUM(CASE WHEN cached = 0 AND outcome_class IS NOT NULL AND outcome_class NOT IN ('clean','cached') THEN 1 ELSE 0 END) AS failed
             FROM brain.ai_calls WHERE prompt_template_id = ? AND ts >= datetime({},'-30 days')", crate::clock::now_sql_inner()),
            &[template_id.clone()],
        )?.as_array().and_then(|a| a.first()).cloned().unwrap_or(json!({}));
        let retries = as_i64(
            crate::db_json::query_rows(
                db,
                "SELECT COUNT(*) AS n FROM brain.ai_call_attempts at JOIN brain.ai_calls a ON a.id = at.ai_call_id WHERE a.prompt_template_id = ? AND at.attempt_no > 1",
                &[template_id.clone()],
            )?.as_array().and_then(|a| a.first()).and_then(|row| row.get("n")).cloned().as_ref(),
        );

        let mut failures: Vec<Value> = Vec::new();
        let fail_calls = crate::db_json::query_rows(
            db,
            "SELECT a.id, a.outcome_class, a.ts,
                    (SELECT validation_errors_json FROM brain.ai_call_attempts at WHERE at.ai_call_id = a.id ORDER BY at.attempt_no DESC LIMIT 1) AS verrs
             FROM brain.ai_calls a
             WHERE a.prompt_template_id = ? AND a.cached = 0 AND a.outcome_class IS NOT NULL AND a.outcome_class NOT IN ('clean','cached')
             ORDER BY a.id DESC LIMIT 5",
            &[template_id.clone()],
        )?;
        for f in fail_calls.as_array().cloned().unwrap_or_default() {
            let mut extra = String::new();
            if let Some(verrs) = f.get("verrs").and_then(Value::as_str) {
                if let Ok(errs) = serde_json::from_str::<Value>(verrs) {
                    if let Some(arr) = errs.as_array() {
                        if let Some(first) = arr.first() {
                            extra = format!(": {}", take_n(&js_stringify(first), 140));
                        }
                    }
                }
            }
            let outcome_class = str_qq(f.get("outcome_class"));
            failures.push(json!({
                "kind": outcome_class,
                "text": format!("{}{}", outcome_class, extra),
                "ai_call_id": f.get("id").cloned().unwrap_or(Value::Null),
                "ts": f.get("ts").cloned().unwrap_or(Value::Null),
            }));
        }
        let neg_tags = crate::db_json::query_rows(
            db,
            "SELECT t.target_ai_call_id AS id, t.verdict, t.failure_class, t.reason_md, t.ts, 'ai' AS src
             FROM brain.heimdall_ai_tags t JOIN brain.ai_calls a ON a.id = t.target_ai_call_id
             WHERE a.prompt_template_id = ? AND t.verdict < 0 AND t.superseded_by_human = 0
             UNION ALL
             SELECT h.ai_call_id, h.verdict, NULL, h.reason_md, h.ts, 'human'
             FROM brain.heimdall_human_tags h JOIN brain.ai_calls a2 ON a2.id = h.ai_call_id
             WHERE a2.prompt_template_id = ? AND h.verdict < 0
             ORDER BY 5 DESC LIMIT 5",
            &[template_id.clone(), template_id.clone()],
        )?;
        for t in neg_tags.as_array().cloned().unwrap_or_default() {
            let src = str_qq(t.get("src"));
            let verdict = t.get("verdict").cloned().unwrap_or(Value::Null);
            let failure_class = t.get("failure_class").filter(|v| !v.is_null());
            let reason_md = t.get("reason_md").filter(|v| !v.is_null());
            let mut text = format!("{} {}", src, js_stringify(&verdict));
            if let Some(fc) = failure_class {
                text.push_str(&format!(" · {}", str_qq(Some(fc))));
            }
            if let Some(rm) = reason_md {
                text.push_str(&format!(" — {}", take_n(&str_qq(Some(rm)), 140)));
            }
            failures.push(json!({
                "kind": format!("{}_tag", src),
                "text": text,
                "ai_call_id": t.get("id").cloned().unwrap_or(Value::Null),
                "ts": t.get("ts").cloned().unwrap_or(Value::Null),
            }));
        }
        failures.sort_by(|a, b| {
            let ta = str_qq(a.get("ts"));
            let tb = str_qq(b.get("ts"));
            tb.cmp(&ta)
        });
        failures.truncate(8);

        let neg_prompt = as_i64(
            crate::db_json::query_rows(db, "SELECT COUNT(*) AS n FROM brain.heimdall_prompt_tags WHERE template_name = ? AND verdict < 0", &[json!(template_name)])?
                .as_array().and_then(|a| a.first()).and_then(|row| row.get("n")).cloned().as_ref(),
        );
        let live = as_i64(u.get("live"));
        let failed = as_i64(u.get("failed"));
        let success_rate = if live > 0 { Some(((live - failed) as f64 / live as f64 * 1000.0).round() / 1000.0) } else { None };

        let mut w_sum = 0.0;
        let mut w_all = 0.0;
        let all_tags = crate::db_json::query_rows(db, "SELECT source, verdict FROM brain.heimdall_prompt_tags WHERE template_name = ?", &[json!(template_name)])?;
        for t in all_tags.as_array().cloned().unwrap_or_default() {
            let source = str_qq(t.get("source"));
            let w = if source == "human" { TRUST_HUMAN } else if source == "ai" { TRUST_AI } else { 0.0 };
            let verdict = as_f64(t.get("verdict"));
            w_sum += w * ((verdict + 2.0) / 4.0);
            w_all += w;
        }
        let prompt_score = if w_all > 0.0 { Some((w_sum / w_all * 1000.0).round() / 1000.0) } else { None };

        let mut insights: Vec<String> = Vec::new();
        if live >= 5 && (failed as f64 / live as f64) > 0.2 {
            insights.push(format!("{}% of live calls fail — this gate (or its models) needs attention.", ((failed as f64 / live as f64) * 100.0).round() as i64));
        }
        if retries > 0 {
            insights.push(format!("{} paid retry attempt(s) — every one is a format/lint defect the template could defuse.", retries));
        }
        let total = as_i64(u.get("total"));
        let cached = as_i64(u.get("cached"));
        if total >= 4 && (cached as f64 / (if total != 0 { total as f64 } else { 1.0 })) >= 0.5 {
            insights.push(format!("{}% cache-served — a cheap gate.", ((cached as f64 / (if total != 0 { total as f64 } else { 1.0 })) * 100.0).round() as i64));
        }
        let ai_n = as_i64(ai_agg.get("n"));
        let has_tag_failures = failures.iter().any(|f| str_qq(f.get("kind")).ends_with("_tag"));
        if live >= 10 && human.is_none() && ai_n == 0 && neg_prompt == 0 && failures.is_empty() {
            insights.push("high traffic with zero quality verdicts — tag it on /tagging.".to_string());
        } else if has_tag_failures && success_rate == Some(1.0) {
            let n_tag_failures = failures.iter().filter(|f| str_qq(f.get("kind")).ends_with("_tag")).count();
            insights.push(format!("detectors say 100% ok, but the judge/human recorded {} negative verdict(s) — the failures here are semantic, not structural.", n_tag_failures));
        }
        if let Some(ps) = prompt_score {
            if ps < 0.5 {
                insights.push(format!("design score {} — a candidate for 🩹 draft fix.", ps));
            }
        }

        let judging_criteria = r.get("judging_criteria_json").and_then(Value::as_str).and_then(|s| serde_json::from_str::<Value>(s).ok());

        let mut obj = Map::new();
        obj.insert("template_name".into(), json!(template_name));
        obj.insert("template_base".into(), json!(regex::Regex::new(r"\.v\d+$").unwrap().replace(&template_name, "").to_string()));
        obj.insert("template_md".into(), r.get("template_md").cloned().unwrap_or(Value::Null));
        obj.insert("system_md".into(), r.get("system_md").filter(|v| !v.is_null()).cloned().unwrap_or(Value::Null));
        obj.insert("tier".into(), r.get("default_tier").cloned().unwrap_or(Value::Null));
        obj.insert("temperature".into(), r.get("default_temperature").cloned().unwrap_or(Value::Null));
        obj.insert("judging_criteria".into(), judging_criteria.unwrap_or(Value::Null));
        obj.insert("calls_30d".into(), r.get("calls_30d").cloned().unwrap_or(json!(0)));
        obj.insert("exemplar_ai_call_id".into(), exemplar_ai_call_id);
        obj.insert("human_tag".into(), human.unwrap_or(Value::Null));
        obj.insert("ai_tags".into(), json!({
            "n": ai_n,
            "avg_verdict": ai_agg.get("avg_verdict").filter(|v| !v.is_null()).cloned().unwrap_or(Value::Null),
            "avg_confidence": ai_agg.get("avg_confidence").filter(|v| !v.is_null()).cloned().unwrap_or(Value::Null),
        }));
        obj.insert("latest_ai".into(), latest_ai.unwrap_or(Value::Null));
        obj.insert("judge_est_tokens".into(), json!(judge_est_tokens));
        obj.insert("prompt_score".into(), prompt_score.map(Value::from).unwrap_or(Value::Null));
        obj.insert("usage_30d".into(), json!({
            "total": total, "live": live, "cached": cached,
            "cost_usd": u.get("cost_usd").cloned().unwrap_or(json!(0)),
            "tokens_in": u.get("tokens_in").cloned().unwrap_or(json!(0)),
            "tokens_out": u.get("tokens_out").cloned().unwrap_or(json!(0)),
            "failed": failed, "success_rate": success_rate.map(Value::from).unwrap_or(Value::Null), "retries": retries,
        }));
        obj.insert("failures".into(), json!(failures));
        obj.insert("insights".into(), json!(insights));
        obj.insert("failure_sources".into(), json!({
            "negative_tags": failures.iter().filter(|f| str_qq(f.get("kind")).ends_with("_tag")).count(),
            "prompt_tags": neg_prompt,
            "validation_errors": retries,
            "detector_flags": failed,
        }));
        out.push(Value::Object(obj));
    }
    Ok(json!(out))
}

// = JS String(v) for the negative-tags text-building join above (verdict is always a number here).
fn js_stringify(v: &Value) -> String {
    match v {
        Value::Null => "null".into(),
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        other => other.to_string(),
    }
}

pub fn heimdallPromptTags(input: &Value, db: &Connection) -> Result<Value, String> {
    let op = if truthy(input.get("op")) { str_qq(input.get("op")) } else { "list".to_string() };

    if op == "list" {
        return Ok(json!({ "ok": true, "prompts": list(db)? }));
    }

    let template_name = str_qq(input.get("template_name"));
    if template_name.is_empty() {
        return Err("heimdall-prompt-tags: template_name is required".into());
    }

    if op == "tag" {
        let verdict = input.get("verdict").and_then(Value::as_f64).unwrap_or(f64::NAN);
        let verdict_i = verdict as i64;
        if !verdict.is_finite() || ![-2, -1, 1, 2].contains(&verdict_i) {
            return Err(format!(
                "heimdall-prompt-tags: verdict must be one of -2, -1, 1, 2 (got '{}')",
                input.get("verdict").map(js_stringify).unwrap_or("undefined".into())
            ));
        }
        crate::db_json::run_write(db, "DELETE FROM brain.heimdall_prompt_tags WHERE template_name = ? AND source = 'human'", &[json!(template_name)])?;
        let reason_md = input.get("reason_md").filter(|v| truthy(Some(v))).map(|v| take_n(&str_qq(Some(v)), 1000));
        let (_, id) = crate::db_json::run_write(
            db,
            "INSERT INTO brain.heimdall_prompt_tags (template_name, verdict, reason_md, source) VALUES (?, ?, ?, 'human')",
            &[json!(template_name), json!(verdict_i), reason_md.map(Value::from).unwrap_or(Value::Null)],
        )?;
        return Ok(json!({ "ok": true, "tagged": true, "tag_id": id, "verdict": verdict_i, "source": "human" }));
    }

    if op == "aitag" {
        let tpl = {
            let mut r = crate::db_json::query_rows(
                db,
                "SELECT id, name, template_md FROM brain.prompt_templates WHERE name = ? AND superseded_by IS NULL ORDER BY id DESC LIMIT 1",
                &[json!(template_name)],
            )?.as_array().and_then(|a| a.first()).cloned();
            if r.is_none() {
                r = crate::db_json::query_rows(
                    db,
                    "SELECT id, name, template_md FROM brain.prompt_templates WHERE name = ? ORDER BY id DESC LIMIT 1",
                    &[json!(template_name)],
                )?.as_array().and_then(|a| a.first()).cloned();
            }
            r
        };
        let Some(tpl) = tpl else {
            return Ok(json!({ "ok": true, "skipped": true, "reason": "template not found" }));
        };
        let tpl_id = tpl.get("id").cloned().unwrap_or(Value::Null);
        let tpl_md = str_qq(tpl.get("template_md"));

        let exemplar_id_input = input.get("exemplar_ai_call_id").and_then(Value::as_i64);
        let exemplar_id = match exemplar_id_input {
            Some(n) if n != 0 => Some(n),
            _ => crate::db_json::query_rows(db, "SELECT MAX(id) AS id FROM brain.ai_calls WHERE prompt_template_id = ? AND cached = 0", &[tpl_id.clone()])?
                .as_array().and_then(|a| a.first()).and_then(|r| r.get("id")).and_then(Value::as_i64),
        };

        let mut exemplar_input = "(no real call under this prompt yet)".to_string();
        let mut exemplar_output = "(none)".to_string();
        if let Some(eid) = exemplar_id {
            let inputs_rows = crate::db_json::query_rows(db, "SELECT key, value_text FROM brain.ai_call_inputs WHERE ai_call_id = ? ORDER BY order_index, id", &[json!(eid)])?;
            let inputs: Vec<Value> = inputs_rows.as_array().cloned().unwrap_or_default().iter()
                .map(|i| json!({ "key": i.get("key").cloned().unwrap_or(Value::Null), "value": i.get("value_text").cloned().unwrap_or(Value::Null) }))
                .collect();
            exemplar_input = if !tpl_md.is_empty() {
                let rendered = crate::ai_ai_call::reconstructRenderedPrompt(&json!(tpl_md), &json!(inputs));
                str_qq(Some(&rendered))
            } else {
                inputs.iter().map(|i| format!("{}: {}", str_qq(i.get("key")), str_qq(i.get("value")))).collect::<Vec<_>>().join("\n")
            };
            exemplar_input = take_n(&exemplar_input, 6000);
            let outputs_rows = crate::db_json::query_rows(
                db,
                "SELECT field_name, value_text FROM brain.ai_call_outputs WHERE ai_call_id = ? AND value_text IS NOT NULL ORDER BY order_index, id",
                &[json!(eid)],
            )?;
            let joined = outputs_rows.as_array().cloned().unwrap_or_default().iter()
                .map(|o| format!("{}: {}", str_qq(o.get("field_name")), str_qq(o.get("value_text"))))
                .collect::<Vec<_>>().join("\n\n");
            exemplar_output = if joined.is_empty() { "(empty output)".to_string() } else { take_n(&joined, 4000) };
        }

        ensure_judge_prompt(db)?;

        let judge_pass = as_i64(
            crate::db_json::query_rows(
                db,
                "SELECT COUNT(*) AS n FROM brain.heimdall_prompt_tags WHERE template_name = ? AND source = 'ai' AND exemplar_ai_call_id IS ?",
                &[json!(template_name), exemplar_id.map(Value::from).unwrap_or(Value::Null)],
            )?.as_array().and_then(|a| a.first()).and_then(|r| r.get("n")).cloned().as_ref(),
        );

        // aiCall (PARITY-DEFERRED): mirrors the JS `await aiCall(...)` with no surrounding
        // try/catch — the rejection propagates unchanged to the caller.
        let res = crate::ai_ai_call::aiCall(
            &json!({
                "prompt_name": JUDGE_PROMPT,
                "input_kv": [
                    { "key": "template_name", "value": template_name },
                    { "key": "template_md", "value": take_n(&tpl_md, 8000) },
                    { "key": "judge_pass", "value": judge_pass.to_string() },
                    { "key": "exemplar_input", "value": exemplar_input },
                    { "key": "exemplar_output", "value": exemplar_output },
                ],
                "tier": "high", "temperature": "cold", "phase_flow": "single_only",
                "two_phase": false, "structured_output": false, "provider_fn": Value::Null,
            }),
            db,
        )?;

        let raw = res.get("output_kv").and_then(Value::as_array).and_then(|arr| {
            arr.iter().find(|o| o.get("field_name").and_then(Value::as_str) == Some("judge_raw"))
        }).and_then(|o| o.get("value_text")).cloned().unwrap_or(json!(""));
        let parsed = crate::heimdall_heimdall_ai_tag::parseJudgeLine(&raw);
        if parsed.is_null() {
            return Err(format!(
                "prompt judge reply had no VERDICT line (judge call #{})",
                res.get("ai_call_id").map(js_stringify).unwrap_or("?".into())
            ));
        }
        let verdict = parsed.get("verdict").and_then(Value::as_i64).unwrap_or(0);
        let confidence = parsed.get("confidence").and_then(Value::as_f64);
        let mut failure_class = parsed.get("failure_class").and_then(Value::as_str).map(|s| s.to_string());
        if failure_class.as_deref() == Some("none") {
            failure_class = None;
        }
        if let Some(fc) = &failure_class {
            if !PROMPT_CLASSES.contains(&fc.as_str()) {
                failure_class = Some("other".to_string());
            }
        }
        crate::db_json::run_write(
            db,
            "DELETE FROM brain.heimdall_prompt_tags WHERE template_name = ? AND source = 'ai' AND exemplar_ai_call_id IS ?",
            &[json!(template_name), exemplar_id.map(Value::from).unwrap_or(Value::Null)],
        )?;
        let confidence_clamped = match confidence {
            Some(c) if c.is_finite() => c.max(0.0).min(1.0),
            _ => 0.5,
        };
        let reason = parsed.get("reason").and_then(Value::as_str).unwrap_or("");
        let reason_out = if reason.is_empty() { "(no reason line)".to_string() } else { take_n(reason, 500) };
        let failure_class_stored = if verdict < 0 { failure_class.clone() } else { None };
        let (_, id) = crate::db_json::run_write(
            db,
            "INSERT INTO brain.heimdall_prompt_tags (template_name, verdict, confidence, reason_md, failure_class, source, exemplar_ai_call_id, judge_ai_call_id) VALUES (?, ?, ?, ?, ?, 'ai', ?, ?)",
            &[
                json!(template_name), json!(verdict), json!(confidence_clamped), json!(reason_out),
                failure_class_stored.clone().map(Value::from).unwrap_or(Value::Null),
                exemplar_id.map(Value::from).unwrap_or(Value::Null),
                res.get("ai_call_id").cloned().unwrap_or(Value::Null),
            ],
        )?;
        return Ok(json!({
            "ok": true, "tagged": true, "tag_id": id, "verdict": verdict, "confidence": confidence,
            "failure_class": failure_class_stored, "judge_ai_call_id": res.get("ai_call_id").cloned().unwrap_or(Value::Null), "source": "ai",
        }));
    }

    Err(format!("heimdall-prompt-tags: op must be one of list, tag, aitag (got '{}')", op))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCHEMA: &str = "
        CREATE TABLE brain.prompt_templates ( id INTEGER PRIMARY KEY, name TEXT NOT NULL, version INTEGER NOT NULL DEFAULT 1, template_md TEXT NOT NULL, default_tier TEXT NOT NULL DEFAULT 'mid', default_temperature TEXT NOT NULL DEFAULT 'cold', max_output_tokens INTEGER NOT NULL DEFAULT 1024, superseded_by INTEGER, created_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')), cross_field_rules_json TEXT, judging_criteria_json TEXT, system_md TEXT, phase_flow TEXT NOT NULL DEFAULT 'single', UNIQUE (name, version) );
        CREATE TABLE brain.prompt_output_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL DEFAULT 'string', enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, grounded_refs_json TEXT, UNIQUE (prompt_template_id, field_name) );
        CREATE TABLE brain.prompt_input_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL DEFAULT 'string', enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, UNIQUE (prompt_template_id, field_name) );
        CREATE TABLE brain.ai_calls ( id INTEGER PRIMARY KEY, ts TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')), prompt_template_id INTEGER NOT NULL, cached INTEGER NOT NULL DEFAULT 0, outcome_class TEXT, cost_usd_actual REAL, tokens_in INTEGER, tokens_out INTEGER );
        CREATE TABLE brain.ai_call_attempts ( id INTEGER PRIMARY KEY, ai_call_id INTEGER NOT NULL, attempt_no INTEGER NOT NULL, validation_errors_json TEXT );
        CREATE TABLE brain.ai_call_inputs ( id INTEGER PRIMARY KEY, ai_call_id INTEGER NOT NULL, key TEXT, value_text TEXT, order_index INTEGER NOT NULL DEFAULT 0 );
        CREATE TABLE brain.ai_call_outputs ( id INTEGER PRIMARY KEY, ai_call_id INTEGER NOT NULL, field_name TEXT, value_text TEXT, order_index INTEGER NOT NULL DEFAULT 0 );
        CREATE TABLE brain.heimdall_prompt_tags ( id INTEGER PRIMARY KEY AUTOINCREMENT, template_name TEXT NOT NULL, verdict INTEGER NOT NULL, confidence REAL, reason_md TEXT, failure_class TEXT, source TEXT NOT NULL, exemplar_ai_call_id INTEGER, judge_ai_call_id INTEGER, ts TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')) );
        CREATE TABLE brain.heimdall_ai_tags ( id INTEGER PRIMARY KEY AUTOINCREMENT, target_ai_call_id INTEGER, verdict INTEGER, confidence REAL, reason_md TEXT, failure_class TEXT, superseded_by_human INTEGER NOT NULL DEFAULT 0, ts TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')) );
        CREATE TABLE brain.heimdall_human_tags ( id INTEGER PRIMARY KEY AUTOINCREMENT, ai_call_id INTEGER, verdict INTEGER, reason_md TEXT, ts TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')) );
    ";

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS brain").unwrap();
        c.execute_batch(SCHEMA).unwrap();
        c
    }

    fn seed_template(c: &Connection, name: &str) -> i64 {
        c.execute("INSERT INTO brain.prompt_templates (name, template_md) VALUES (?, 'hi {{who}}')", [name]).unwrap();
        c.last_insert_rowid()
    }

    #[test]
    fn list_returns_prompt_score_and_usage() {
        let c = db();
        let tid = seed_template(&c, "chat.v1");
        c.execute(
            "INSERT INTO brain.ai_calls (prompt_template_id, cached, outcome_class, ts) VALUES (?, 0, 'clean', datetime('2026-01-01 00:00:00'))",
            [tid],
        ).unwrap();
        c.execute(
            "INSERT INTO brain.heimdall_prompt_tags (template_name, verdict, source) VALUES ('chat.v1', 2, 'human')",
            [],
        ).unwrap();
        let out = heimdallPromptTags(&json!({ "op": "list" }), &c).unwrap();
        let prompts = out.get("prompts").unwrap().as_array().unwrap();
        assert_eq!(prompts.len(), 1);
        let p = &prompts[0];
        assert_eq!(p.get("template_name").unwrap(), "chat.v1");
        assert_eq!(p.get("template_base").unwrap(), "chat");
        assert_eq!(p.get("prompt_score").unwrap(), &json!(1.0));
        assert_eq!(p.get("usage_30d").unwrap().get("live").unwrap(), &json!(1));
    }

    #[test]
    fn tag_op_requires_valid_verdict() {
        let c = db();
        seed_template(&c, "chat.v1");
        let err = heimdallPromptTags(&json!({ "op": "tag", "template_name": "chat.v1", "verdict": 0 }), &c).unwrap_err();
        assert!(err.contains("verdict"));
    }

    #[test]
    fn tag_op_latest_wins_per_template() {
        let c = db();
        seed_template(&c, "chat.v1");
        heimdallPromptTags(&json!({ "op": "tag", "template_name": "chat.v1", "verdict": -1, "reason_md": "first" }), &c).unwrap();
        let r2 = heimdallPromptTags(&json!({ "op": "tag", "template_name": "chat.v1", "verdict": 2, "reason_md": "second" }), &c).unwrap();
        assert_eq!(r2.get("tag_id").unwrap(), &json!(2));
        let n: i64 = c.query_row("SELECT COUNT(*) FROM brain.heimdall_prompt_tags WHERE template_name = 'chat.v1' AND source = 'human'", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn aitag_skips_when_template_not_found() {
        let c = db();
        let out = heimdallPromptTags(&json!({ "op": "aitag", "template_name": "nope.v1" }), &c).unwrap();
        assert_eq!(out.get("skipped").unwrap(), &json!(true));
    }

    #[test]
    fn aitag_propagates_the_deferred_ai_call_error() {
        let c = db();
        seed_template(&c, "chat.v1");
        let err = heimdallPromptTags(&json!({ "op": "aitag", "template_name": "chat.v1" }), &c).unwrap_err();
        assert!(err.contains("ai-call"), "unexpected error: {err}");
    }

    #[test]
    fn op_defaults_to_list_and_unknown_op_errors() {
        let c = db();
        let out = heimdallPromptTags(&json!({}), &c).unwrap();
        assert_eq!(out.get("ok").unwrap(), &json!(true));
        let err = heimdallPromptTags(&json!({ "op": "bogus", "template_name": "chat.v1" }), &c).unwrap_err();
        assert!(err.contains("op must be one of"));
    }
}
