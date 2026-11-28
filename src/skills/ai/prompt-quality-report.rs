//! prompt-quality-report.rs — twin of skills/ai/prompt-quality-report.mjs (db, cross-db
//! brain.*/memory.*). NAMES ARE CONTRACT: default `promptQualityReport`. Per-template calibration
//! scorecards (EPIC-AI-BEHAVIOR-STUDY): folds ai_calls + ai_call_attempts + ai_call_reports
//! behavioral stats with promptLint's static score into
//! calibration_score = 0.3*static + 0.5*success + 0.2*(1-retry_share), persisted to
//! brain.bench_runs. Optional advisory sinks: memory lessons (memoryAdd, superseding the prior
//! lesson per template) and an inbox digest (inboxMessageAdd) — both best-effort, mirroring the
//! JS try/catch swallows.
//!
//! PARITY-DEFERRED: the narrative leg ends in `crate::ai_ai_call::aiCall` (the deferred
//! network-AI stub — see ai-call.rs's module doc). It only fires when BOTH `narrative:true` AND
//! env `ANTILOKI_POSTMORTEM_NARRATIVE=1` — under the harness's frozen/controlled env this is
//! effectively never both true, and even when reached the aiCall Err is swallowed exactly like
//! the JS try/catch ("one bad narrative never stops the batch"), so `narratives_written` stays 0.
//! Ported faithfully anyway: `ensure_narrative_prompt` (reusing the already-converted
//! `ai_prompt_find`/`ai_prompt_add` twins) and the per-report aiCall-call + narrative_md UPDATE
//! shape, so a future live-aiCall build needs no further porting here.
//! Everything else — window/behavioral SQL folds, retry/histogram joins, promptLint's static
//! score, the calibration formula, trend-vs-prev-version, bench_runs persistence, the
//! memory-lesson sink, and the inbox digest — is ported byte-for-byte and IS this file's
//! deterministic (and fixtured) surface. Frozen clock 2026-01-01 for `datetime('now', ...)` and
//! the default run_id timestamp (mirrors `new Date().toISOString().replace(/[:.]/g,'-')`).
use rusqlite::Connection;
use serde_json::{json, Map, Value};

// ── standard helpers (playbook, copied verbatim — only the ones this module uses) ──────────
fn truthy(v: Option<&Value>) -> bool { match v { None|Some(Value::Null)=>false, Some(Value::Bool(b))=>*b, Some(Value::Number(n))=>n.as_f64().map_or(false,|f|f!=0.0&&!f.is_nan()), Some(Value::String(s))=>!s.is_empty(), Some(_)=>true } }
fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) }
fn js_number(v: Option<&Value>) -> f64 { match v { None=>f64::NAN, Some(Value::Null)=>0.0, Some(Value::Bool(b))=>if *b {1.0} else {0.0}, Some(Value::Number(n))=>n.as_f64().unwrap_or(f64::NAN), Some(Value::String(s))=>{let t=s.trim(); if t.is_empty(){0.0}else{t.parse().unwrap_or(f64::NAN)}}, Some(_)=>f64::NAN } }
// `${v}` template-literal stringify of a JSON Value (v is never undefined here — always a
// present destructured default or an explicit input value).
fn tmpl_v(v: &Value) -> String { match v { Value::Null=>"null".into(), Value::String(s)=>s.clone(), Value::Bool(b)=>b.to_string(), Value::Number(n)=>n.to_string(), other=>other.to_string() } }

// `+(x.toFixed(3))` — round to 3 decimals, returned as a number.
fn round3(f: f64) -> f64 { (f * 1000.0).round() / 1000.0 }
// `(x*100).toFixed(0)` used inline in a template string.
fn pct0(f: f64) -> String { format!("{}", (f * 100.0).round() as i64) }

fn sql_now() -> String { crate::clock::now_ts_nodb() }
// JS: new Date().toISOString().replace(/[:.]/g, '-')
fn run_id_ts() -> String { crate::clock::now_iso_nodb().replace([':', '.'], "-") }

// ── narrative prompt (registered with the ensurePrompt drift pattern) ──────────────────────
const NARRATIVE_PROMPT_NAME: &str = "keeper.postmortem.narrative.v1";
const NARRATIVE_TEMPLATE_MD: &str = "You are the postmortem analyst of an AI-orchestration substrate. A deterministic classifier already diagnosed ONE failed AI interaction; your job is the narrative WHY — the behavioral story behind the failure class, written for the operator who will calibrate the prompt.\n\n\u{2550}\u{2550}\u{2550} FAILURE CLASS (deterministic) \u{2550}\u{2550}\u{2550}\n{{failure_class}}\n\n\u{2550}\u{2550}\u{2550} DETERMINISTIC DIAGNOSIS \u{2550}\u{2550}\u{2550}\n{{diagnosis_md}}\n\n\u{2550}\u{2550}\u{2550} EVIDENCE (clipped) \u{2550}\u{2550}\u{2550}\n{{evidence_json}}\n\nWrite narrative_md: 3-8 sentences explaining WHY the model behaved this way (language-register causes, prompt-shape causes, tier limitations), what the evidence shows it was 'trying' to do, and what would most likely make the correct response the path of least resistance. No restating the diagnosis; add behavioral insight only.\nAlso produce calibration_hints_list: 1-3 concrete, operator-actionable hints (template wording, tier routing, output contract changes).\n\nReturn JSON only: {\"narrative_md\": \"...\", \"calibration_hints_list\": [\"...\"]}";

fn ensure_narrative_prompt(db: &Connection) {
    let existing = crate::ai_prompt_find::promptFind(&json!({ "name": NARRATIVE_PROMPT_NAME }), db).ok();
    if let Some(e) = &existing {
        if e.get("template_md").and_then(Value::as_str) == Some(NARRATIVE_TEMPLATE_MD) { return; }
    }
    let _ = crate::ai_prompt_add::promptAdd(&json!({
        "name": NARRATIVE_PROMPT_NAME,
        "template_md": NARRATIVE_TEMPLATE_MD,
        "default_tier": "mid", "default_temperature": "cold",
        "phase_flow": "single_only",
        "max_output_tokens": 1500,
        "output_fields": [
            { "field_name": "narrative_md", "field_type": "string", "required": true, "min_length": 100 },
            { "field_name": "calibration_hints_list", "field_type": "array_of_string", "required": false },
        ],
    }), db);
}

// ── scorecard accumulator (ported from the JS `byTpl` Map) ─────────────────────────────────
struct Accum {
    name: Value,
    version: Value,
    calls: i64,
    success: i64,
    validation_failed: i64,
    error: i64,
    timeout: i64,
    cached: i64,
    near_cap: i64,
    tier_order: Vec<String>, // Map insertion order
    tier_breakdown: std::collections::HashMap<String, (i64, f64)>, // tier -> (calls, fail_rate)
}

fn worst_tier(order: &[String], map: &std::collections::HashMap<String, (i64, f64)>) -> Option<String> {
    let mut worst: Option<(String, f64)> = None;
    for t in order {
        let (_c, fail_rate) = map[t];
        match &worst {
            None => worst = Some((t.clone(), fail_rate)),
            Some((_, wf)) if fail_rate > *wf => worst = Some((t.clone(), fail_rate)),
            _ => {}
        }
    }
    worst.map(|(t, _)| t)
}

pub fn promptQualityReport(input: &Value, db: &Connection) -> Result<Value, String> {
    let name = nz(input.get("name"));
    // destructuring defaults (`since_days = 14` etc.) fire only when the key is ABSENT; keep the
    // RAW value (as JS keeps the raw destructured binding) for display, separate from the
    // Number()-coerced value used in arithmetic/comparisons.
    let since_days_raw: Value = match input.get("since_days") { Some(v) => v.clone(), None => json!(14) };
    let min_calls_raw: Value = match input.get("min_calls") { Some(v) => v.clone(), None => json!(5) };
    let threshold_raw: Value = match input.get("threshold") { Some(v) => v.clone(), None => json!(60) };
    let n = js_number(Some(&since_days_raw));
    let since_days_num = if n.is_nan() || n == 0.0 { 14.0 } else { n };
    let since_days_i = since_days_num.max(1.0);
    let min_calls = js_number(Some(&min_calls_raw)); // `s.calls < min_calls` — ToNumber via relational compare
    let threshold = js_number(Some(&threshold_raw));
    let write_inbox = truthy(input.get("write_inbox"));
    let write_memory = truthy(input.get("write_memory"));
    let narrative = truthy(input.get("narrative"));

    let since_days_disp = tmpl_v(&since_days_raw);
    let threshold_disp = tmpl_v(&threshold_raw);

    let since_modifier = format!("-{} days", if since_days_i.fract() == 0.0 { format!("{}", since_days_i as i64) } else { format!("{}", since_days_i) });
    let window_row = crate::db_json::query_rows(db, &format!("SELECT datetime('{}', ?) AS t", sql_now()), &[json!(since_modifier)])?;
    let window_from = window_row.as_array().and_then(|a| a.first()).and_then(|r| r.get("t")).cloned().unwrap_or(Value::Null);
    let window_from_str = window_from.as_str().unwrap_or("").to_string();

    let run_id = match nz(input.get("run_id")) {
        Some(v) => v.clone(),
        None => json!(format!("pq-{}", run_id_ts())),
    };
    let run_id_str = run_id.as_str().unwrap_or("").to_string();

    // ── behavioral stats per (name, version, tier) ─────────────────────────────────────────
    let tier_rows_v = if let Some(name_v) = name {
        crate::db_json::query_rows(db, "
            SELECT p.name, p.version, a.tier,
                   COUNT(*) AS calls,
                   SUM(a.outcome = 'success') AS success,
                   SUM(a.outcome = 'validation_failed') AS validation_failed,
                   SUM(a.outcome = 'error') AS error,
                   SUM(a.outcome = 'timeout') AS timeout,
                   SUM(a.cached) AS cached,
                   MAX(p.max_output_tokens) AS max_out,
                   SUM(CASE WHEN p.max_output_tokens > 0 AND a.tokens_out >= 0.95 * p.max_output_tokens THEN 1 ELSE 0 END) AS near_cap
            FROM brain.ai_calls a
            JOIN brain.prompt_templates p ON p.id = a.prompt_template_id
            WHERE a.ts >= ? AND p.name = ?
            GROUP BY p.name, p.version, a.tier
        ", &[window_from.clone(), name_v.clone()])?
    } else {
        crate::db_json::query_rows(db, "
            SELECT p.name, p.version, a.tier,
                   COUNT(*) AS calls,
                   SUM(a.outcome = 'success') AS success,
                   SUM(a.outcome = 'validation_failed') AS validation_failed,
                   SUM(a.outcome = 'error') AS error,
                   SUM(a.outcome = 'timeout') AS timeout,
                   SUM(a.cached) AS cached,
                   MAX(p.max_output_tokens) AS max_out,
                   SUM(CASE WHEN p.max_output_tokens > 0 AND a.tokens_out >= 0.95 * p.max_output_tokens THEN 1 ELSE 0 END) AS near_cap
            FROM brain.ai_calls a
            JOIN brain.prompt_templates p ON p.id = a.prompt_template_id
            WHERE a.ts >= ?
            GROUP BY p.name, p.version, a.tier
        ", &[window_from.clone()])?
    };
    let tier_rows = tier_rows_v.as_array().cloned().unwrap_or_default();

    // retry stats from the attempts trail (guarded: pre-migration-134 DBs).
    let mut retry_by_tpl: std::collections::HashMap<String, (i64, i64)> = std::collections::HashMap::new(); // key -> (with_attempts, recovered)
    {
        let q = if let Some(name_v) = name {
            crate::db_json::query_rows(db, "
                SELECT p.name, p.version,
                       COUNT(DISTINCT t.ai_call_id) AS with_attempts,
                       COUNT(DISTINCT CASE WHEN t.outcome = 'success' THEN t.ai_call_id END) AS recovered
                FROM brain.ai_call_attempts t
                JOIN brain.ai_calls a ON a.id = t.ai_call_id
                JOIN brain.prompt_templates p ON p.id = a.prompt_template_id
                WHERE a.ts >= ? AND p.name = ?
                GROUP BY p.name, p.version
            ", &[window_from.clone(), name_v.clone()]).ok()
        } else {
            crate::db_json::query_rows(db, "
                SELECT p.name, p.version,
                       COUNT(DISTINCT t.ai_call_id) AS with_attempts,
                       COUNT(DISTINCT CASE WHEN t.outcome = 'success' THEN t.ai_call_id END) AS recovered
                FROM brain.ai_call_attempts t
                JOIN brain.ai_calls a ON a.id = t.ai_call_id
                JOIN brain.prompt_templates p ON p.id = a.prompt_template_id
                WHERE a.ts >= ?
                GROUP BY p.name, p.version
            ", &[window_from.clone()]).ok()
        };
        if let Some(rows) = q.and_then(|v| v.as_array().cloned()) {
            for r in rows {
                let key = format!("{}@{}", r["name"].as_str().unwrap_or(""), r["version"]);
                retry_by_tpl.insert(key, (r["with_attempts"].as_i64().unwrap_or(0), r["recovered"].as_i64().unwrap_or(0)));
            }
        }
    }

    // failure-class histogram from the postmortem reports (guarded).
    let mut hist_by_tpl: std::collections::HashMap<String, (Vec<String>, std::collections::HashMap<String, i64>)> = std::collections::HashMap::new();
    {
        let q = if let Some(name_v) = name {
            crate::db_json::query_rows(db, "
                SELECT prompt_name, prompt_version, failure_class, COUNT(*) AS n
                FROM brain.ai_call_reports
                WHERE ts >= ? AND prompt_name = ?
                GROUP BY prompt_name, prompt_version, failure_class
            ", &[window_from.clone(), name_v.clone()]).ok()
        } else {
            crate::db_json::query_rows(db, "
                SELECT prompt_name, prompt_version, failure_class, COUNT(*) AS n
                FROM brain.ai_call_reports
                WHERE ts >= ?
                GROUP BY prompt_name, prompt_version, failure_class
            ", &[window_from.clone()]).ok()
        };
        if let Some(rows) = q.and_then(|v| v.as_array().cloned()) {
            for r in rows {
                let key = format!("{}@{}", r["prompt_name"].as_str().unwrap_or(""), r["prompt_version"]);
                let entry = hist_by_tpl.entry(key).or_insert_with(|| (Vec::new(), std::collections::HashMap::new()));
                let fc = r["failure_class"].as_str().unwrap_or("").to_string();
                if !entry.1.contains_key(&fc) { entry.0.push(fc.clone()); }
                entry.1.insert(fc, r["n"].as_i64().unwrap_or(0));
            }
        }
    }

    // static prompt-lint scores (active versions).
    let mut static_by_name: std::collections::HashMap<String, i64> = std::collections::HashMap::new();
    {
        let lint_input = if let Some(name_v) = name { json!({ "name": name_v.clone() }) } else { json!({}) };
        if let Ok(r) = crate::ai_prompt_lint::promptLint(&lint_input, db) {
            if let Some(prompts) = r.get("prompts").and_then(Value::as_array) {
                for p in prompts {
                    let pname = p["name"].as_str().unwrap_or("").to_string();
                    let pversion = p["version"].clone();
                    let score = p["score_0_100"].as_i64().unwrap_or(0);
                    static_by_name.insert(format!("{pname}@{pversion}"), score);
                    static_by_name.entry(pname).or_insert(score);
                }
            }
        }
    }

    // ── fold tiers into per-(name, version) scorecards ─────────────────────────────────────
    let mut key_order: Vec<String> = Vec::new();
    let mut by_tpl: std::collections::HashMap<String, Accum> = std::collections::HashMap::new();
    for r in &tier_rows {
        let rname = r["name"].as_str().unwrap_or("").to_string();
        let rversion = r["version"].clone();
        let key = format!("{rname}@{rversion}");
        if !by_tpl.contains_key(&key) {
            key_order.push(key.clone());
            by_tpl.insert(key.clone(), Accum {
                name: r["name"].clone(), version: rversion.clone(),
                calls: 0, success: 0, validation_failed: 0, error: 0, timeout: 0, cached: 0, near_cap: 0,
                tier_order: Vec::new(), tier_breakdown: std::collections::HashMap::new(),
            });
        }
        let s = by_tpl.get_mut(&key).unwrap();
        let calls = r["calls"].as_i64().unwrap_or(0);
        let success = r["success"].as_i64().unwrap_or(0);
        s.calls += calls;
        s.success += success;
        s.validation_failed += r["validation_failed"].as_i64().unwrap_or(0);
        s.error += r["error"].as_i64().unwrap_or(0);
        s.timeout += r["timeout"].as_i64().unwrap_or(0);
        s.cached += r["cached"].as_i64().unwrap_or(0);
        s.near_cap += r["near_cap"].as_i64().unwrap_or(0);
        let tier = r["tier"].as_str().unwrap_or("").to_string();
        let fail_rate = if calls > 0 { round3((calls - success) as f64 / calls as f64) } else { 0.0 };
        if !s.tier_breakdown.contains_key(&tier) { s.tier_order.push(tier.clone()); }
        s.tier_breakdown.insert(tier, (calls, fail_rate));
    }

    let mut scorecards: Vec<Value> = Vec::new();
    let mut skipped_low_volume: i64 = 0;
    for key in &key_order {
        let s = &by_tpl[key];
        if (s.calls as f64) < min_calls {
            skipped_low_volume += 1;
            continue;
        }
        let (with_attempts, recovered) = retry_by_tpl.get(key).cloned().unwrap_or((0, 0));
        let (hist_order, hist_map) = hist_by_tpl.get(key).cloned().unwrap_or_else(|| (Vec::new(), std::collections::HashMap::new()));
        let name_str = s.name.as_str().unwrap_or("").to_string();
        let static_score = static_by_name.get(key).copied().or_else(|| static_by_name.get(&name_str).copied()).unwrap_or(0);
        let success_rate = if s.calls > 0 { s.success as f64 / s.calls as f64 } else { f64::NAN };
        let retry_share = if s.calls > 0 { with_attempts as f64 / s.calls as f64 } else { 0.0 };
        let calibration = (0.3 * static_score as f64 + 0.5 * success_rate * 100.0 + 0.2 * (1.0 - retry_share) * 100.0)
            .max(0.0).min(100.0).round() as i64;

        // dominant failure class (Object.entries insertion order, stable sort by count desc).
        let mut hist_entries: Vec<(String, i64)> = hist_order.iter().map(|k| (k.clone(), hist_map[k])).collect();
        hist_entries.sort_by(|a, b| b.1.cmp(&a.1));
        let dominant = hist_entries.first().map(|(k, _)| k.clone());

        let mut recommendation: Value = match &dominant {
            Some(d) => {
                let tier = worst_tier(&s.tier_order, &s.tier_breakdown);
                let evidence = json!({ "prompt_name": s.name.clone(), "tier": tier });
                crate::ai_failure_classes::renderRecommendationMd(&json!(d), &evidence)
            }
            None => Value::Null,
        };
        if let Some((low_calls, low_fail_rate)) = s.tier_breakdown.get("low").copied() {
            if (low_calls as f64) >= min_calls && low_fail_rate > 0.3 {
                let hint = format!(
                    "Low tier ('AI Helper') fails {}% of {} calls — consider routing the format leg to mid ('AI Builder') via routing_defaults (operator applies).",
                    pct0(low_fail_rate), name_str
                );
                recommendation = match recommendation.as_str() {
                    Some(r) => json!(format!("{r}\n\n{hint}")),
                    None => json!(hint),
                };
            }
        }

        let mut tier_breakdown_obj = Map::new();
        for t in &s.tier_order {
            let (calls, fail_rate) = s.tier_breakdown[t];
            tier_breakdown_obj.insert(t.clone(), json!({ "calls": calls, "fail_rate": fail_rate }));
        }
        let mut hist_obj = Map::new();
        for k in &hist_order {
            hist_obj.insert(k.clone(), json!(hist_map[k]));
        }

        let mut sc = Map::new();
        sc.insert("name".into(), s.name.clone());
        sc.insert("version".into(), s.version.clone());
        sc.insert("static_score_0_100".into(), json!(static_score));
        sc.insert("calls".into(), json!(s.calls));
        sc.insert("success".into(), json!(s.success));
        sc.insert("validation_failed".into(), json!(s.validation_failed));
        sc.insert("error".into(), json!(s.error));
        sc.insert("timeout".into(), json!(s.timeout));
        sc.insert("success_rate".into(), json!(round3(success_rate)));
        sc.insert("cached_share".into(), json!(round3(s.cached as f64 / s.calls as f64)));
        sc.insert("retry_share".into(), json!(round3(retry_share)));
        sc.insert("retry_recovered_rate".into(), if with_attempts > 0 { json!(round3(recovered as f64 / with_attempts as f64)) } else { Value::Null });
        sc.insert("near_cap_share".into(), json!(round3(s.near_cap as f64 / s.calls as f64)));
        sc.insert("tier_breakdown".into(), Value::Object(tier_breakdown_obj));
        sc.insert("failure_class_histogram".into(), Value::Object(hist_obj));
        sc.insert("dominant_failure_class".into(), dominant.map(|d| json!(d)).unwrap_or(Value::Null));
        sc.insert("calibration_score_0_100".into(), json!(calibration));
        sc.insert("recommendation_md".into(), recommendation);
        sc.insert("trend_vs_prev_version".into(), Value::Null);
        scorecards.push(Value::Object(sc));
    }

    // trend: success-rate delta vs the previous version IN the same window.
    let mut by_name_idx: Vec<(String, Vec<usize>)> = Vec::new();
    for (i, sc) in scorecards.iter().enumerate() {
        let nm = sc["name"].as_str().unwrap_or("").to_string();
        if let Some(pos) = by_name_idx.iter().position(|(n, _)| n == &nm) {
            by_name_idx[pos].1.push(i);
        } else {
            by_name_idx.push((nm, vec![i]));
        }
    }
    for (_nm, idxs) in &by_name_idx {
        let mut sorted_idxs = idxs.clone();
        sorted_idxs.sort_by(|&a, &b| {
            let va = scorecards[a]["version"].as_i64().unwrap_or(0);
            let vb = scorecards[b]["version"].as_i64().unwrap_or(0);
            va.cmp(&vb)
        });
        for i in 1..sorted_idxs.len() {
            let prev_idx = sorted_idxs[i - 1];
            let cur_idx = sorted_idxs[i];
            let prev_version = scorecards[prev_idx]["version"].clone();
            let prev_success = scorecards[prev_idx]["success_rate"].as_f64().unwrap_or(0.0);
            let cur_success = scorecards[cur_idx]["success_rate"].as_f64().unwrap_or(0.0);
            let delta = round3(cur_success - prev_success);
            scorecards[cur_idx]["trend_vs_prev_version"] = json!({ "prev_version": prev_version, "success_delta": delta });
        }
    }

    scorecards.sort_by(|a, b| {
        let ca = a["calibration_score_0_100"].as_i64().unwrap_or(0);
        let cb = b["calibration_score_0_100"].as_i64().unwrap_or(0);
        ca.cmp(&cb)
    });
    let worst_offenders: Vec<Value> = scorecards.iter()
        .filter(|sc| (sc["calibration_score_0_100"].as_i64().unwrap_or(0) as f64) < threshold)
        .map(|sc| json!(format!("{}@v{}", sc["name"].as_str().unwrap_or(""), sc["version"])))
        .collect();

    // ── persist scorecards to bench_runs (best-effort — absent on very old DBs) ────────────
    {
        let mut ok = true;
        for sc in &scorecards {
            if !ok { break; }
            let bench = format!("prompt-quality:{}", sc["name"].as_str().unwrap_or(""));
            let calls = [
                ("calibration_score", sc["calibration_score_0_100"].clone(), "score_0_100", json!(serde_json::to_string(sc).unwrap_or_default())),
                ("success_rate", sc["success_rate"].clone(), "ratio", Value::Null),
                ("retry_share", sc["retry_share"].clone(), "ratio", Value::Null),
                ("cached_share", sc["cached_share"].clone(), "ratio", Value::Null),
                ("calls", sc["calls"].clone(), "count", Value::Null),
            ];
            for (metric, value, unit, ctx) in calls {
                if crate::db_json::run_write(db,
                    "INSERT INTO brain.bench_runs (run_id, bench_name, metric, value, unit, context_json) VALUES (?, ?, ?, ?, ?, ?)",
                    &[run_id.clone(), json!(bench), json!(metric), value, json!(unit), ctx],
                ).is_err() { ok = false; break; }
            }
        }
    }

    // ── narrative leg (LLM, mid tier, env-gated, aggregator-context only) ──────────────────
    let mut narratives_written: i64 = 0;
    if narrative && std::env::var("ANTILOKI_POSTMORTEM_NARRATIVE").ok().as_deref() == Some("1") {
        ensure_narrative_prompt(db);
        let pending_q = if let Some(name_v) = name {
            crate::db_json::query_rows(db, "
                SELECT id, failure_class, diagnosis_md, evidence_json
                FROM brain.ai_call_reports
                WHERE ts >= ? AND narrative_md IS NULL AND prompt_name = ?
                ORDER BY id DESC LIMIT 10
            ", &[window_from.clone(), name_v.clone()])
        } else {
            crate::db_json::query_rows(db, "
                SELECT id, failure_class, diagnosis_md, evidence_json
                FROM brain.ai_call_reports
                WHERE ts >= ? AND narrative_md IS NULL
                ORDER BY id DESC LIMIT 10
            ", &[window_from.clone()])
        };
        if let Ok(pending_v) = pending_q {
            for rep in pending_v.as_array().cloned().unwrap_or_default() {
                let diag: String = rep.get("diagnosis_md").and_then(Value::as_str).unwrap_or("").chars().take(4000).collect();
                let evid: String = rep.get("evidence_json").and_then(Value::as_str).unwrap_or("{}").chars().take(6000).collect();
                let ai_input = json!({
                    "prompt_name": NARRATIVE_PROMPT_NAME,
                    "input_kv": [
                        { "key": "failure_class", "value": rep.get("failure_class").cloned().unwrap_or(Value::Null) },
                        { "key": "diagnosis_md", "value": diag },
                        { "key": "evidence_json", "value": evid },
                    ],
                });
                if let Ok(r) = crate::ai_ai_call::aiCall(&ai_input, db) {
                    let out_kv = r.get("output_kv").and_then(Value::as_array).cloned().unwrap_or_default();
                    let md = out_kv.iter().find(|o| o.get("field_name").and_then(Value::as_str) == Some("narrative_md"))
                        .and_then(|o| o.get("value_text")).cloned();
                    let hints: Vec<String> = out_kv.iter()
                        .filter(|o| o.get("field_name").and_then(Value::as_str) == Some("calibration_hints_list") && truthy(o.get("value_text")))
                        .filter_map(|o| o.get("value_text").and_then(Value::as_str).map(|s| format!("- {s}")))
                        .collect();
                    if let Some(md_v) = md {
                        if let Some(md_s) = md_v.as_str() {
                            let final_md = if !hints.is_empty() { format!("{}\n\n**Calibration hints:**\n{}", md_s, hints.join("\n")) } else { md_s.to_string() };
                            if crate::db_json::run_write(db, "UPDATE brain.ai_call_reports SET narrative_md = ? WHERE id = ?", &[json!(final_md), rep["id"].clone()]).is_ok() {
                                narratives_written += 1;
                            }
                        }
                    }
                }
                // one bad narrative never stops the batch — Err from aiCall (always, under the
                // deferred stub) is swallowed exactly like the JS try/catch.
            }
        }
    }

    // ── memory lessons (advisory, validated=0, supersede prior) ────────────────────────────
    let mut lessons_written: i64 = 0;
    if write_memory {
        for sc in &scorecards {
            if (sc["calibration_score_0_100"].as_i64().unwrap_or(0) as f64) >= threshold { continue; }
            let sc_name = sc["name"].as_str().unwrap_or("").to_string();
            let title = format!("prompt-quality: {sc_name}");
            let success_rate = sc["success_rate"].as_f64().unwrap_or(0.0);
            let retry_share = sc["retry_share"].as_f64().unwrap_or(0.0);
            let dominant = sc["dominant_failure_class"].as_str().map(|s| s.to_string()).unwrap_or_else(|| "(none reported)".to_string());
            let situation = format!(
                "Template {} v{} scored {}/100 over {}d: {} calls, {}% success, retry share {}%, dominant failure: {}.",
                sc_name, sc["version"], sc["calibration_score_0_100"],
                since_days_disp,
                sc["calls"], pct0(success_rate), pct0(retry_share), dominant
            );
            let guidance = sc.get("recommendation_md").and_then(Value::as_str).map(|s| s.to_string())
                .unwrap_or_else(|| "Inspect the failure-class histogram and the latest postmortem reports for this template.".to_string());
            let add_input = json!({
                "type": "lesson",
                "title": title,
                "situation": situation,
                "guidance": guidance,
                "tags": ["prompt-quality", sc_name],
                "confidence": 0.5,
                "created_by": "prompt-quality-report",
            });
            if let Ok(m) = crate::entities_memory_add::memoryAdd(&add_input, db) {
                let m_id = m.get("id").cloned().unwrap_or(Value::Null);
                if crate::db_json::run_write(db, "
                    UPDATE memory.memories SET superseded_by = ?
                    WHERE type = 'lesson' AND title = ? AND created_by = 'prompt-quality-report'
                      AND id != ? AND superseded_by IS NULL
                ", &[m_id.clone(), json!(title), m_id]).is_ok() {
                    lessons_written += 1;
                }
            }
        }
    }

    // ── inbox digest ─────────────────────────────────────────────────────────────────────
    let mut inbox_message_id: Value = Value::Null;
    if write_inbox && !scorecards.is_empty() {
        let offenders: Vec<&Value> = scorecards.iter().filter(|sc| (sc["calibration_score_0_100"].as_i64().unwrap_or(0) as f64) < threshold).collect();
        let subject = if !offenders.is_empty() {
            format!("AI quality: {} template(s) below {} — worst: {} v{} ({}/100)",
                offenders.len(), threshold_disp,
                offenders[0]["name"].as_str().unwrap_or(""), offenders[0]["version"], offenders[0]["calibration_score_0_100"])
        } else {
            format!("AI quality: all {} active templates \u{2265} {}/100", scorecards.len(), threshold_disp)
        };
        let mut lines: Vec<String> = vec![
            format!("## Prompt-quality window: last {} days (run `{run_id_str}`)", since_days_disp),
            String::new(),
            "| template | calib | calls | success | retry | dominant failure |".to_string(),
            "|---|---|---|---|---|---|".to_string(),
        ];
        for sc in scorecards.iter().take(15) {
            lines.push(format!("| {} v{} | {} | {} | {}% | {}% | {} |",
                sc["name"].as_str().unwrap_or(""), sc["version"], sc["calibration_score_0_100"], sc["calls"],
                pct0(sc["success_rate"].as_f64().unwrap_or(0.0)), pct0(sc["retry_share"].as_f64().unwrap_or(0.0)),
                sc["dominant_failure_class"].as_str().unwrap_or("\u{2014}")));
        }
        lines.push(String::new());
        for sc in offenders.iter().take(5) {
            if let Some(rec) = sc.get("recommendation_md").and_then(Value::as_str) {
                lines.push(format!("### {} v{}", sc["name"].as_str().unwrap_or(""), sc["version"]));
                lines.push(rec.to_string());
                lines.push(String::new());
            }
        }
        lines.push("_Recommendations are advisory — template/routing changes are operator-applied._".to_string());

        let payload = json!({ "run_id": run_id_str, "worst_offenders": worst_offenders, "scorecard_count": scorecards.len() as i64 });
        let inbox_input = json!({
            "kind": "prompt-quality-report",
            "subject": subject,
            "body_md": lines.join("\n"),
            "severity": if !offenders.is_empty() { "warn" } else { "info" },
            "payload_json": payload,
        });
        if let Ok(r) = crate::entities_inbox_message_add::inboxMessageAdd(&inbox_input, db) {
            inbox_message_id = r.get("id").cloned().unwrap_or(Value::Null);
        }
    }

    Ok(json!({
        "run_id": run_id_str,
        "window": { "from": window_from_str, "days": since_days_raw },
        "scorecards": scorecards,
        "worst_offenders": worst_offenders,
        "skipped_low_volume": skipped_low_volume,
        "narratives_written": narratives_written,
        "lessons_written": lessons_written,
        "inbox_message_id": inbox_message_id,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("
            ATTACH ':memory:' AS brain;
            ATTACH ':memory:' AS memory;
            CREATE TABLE brain.prompt_templates (id INTEGER PRIMARY KEY, name TEXT, version INTEGER, template_md TEXT, system_md TEXT, max_output_tokens INTEGER, cross_field_rules_json TEXT, superseded_by INTEGER);
            CREATE TABLE brain.prompt_output_fields (id INTEGER PRIMARY KEY, prompt_template_id INTEGER, field_name TEXT, field_type TEXT, enum_values TEXT, required INTEGER, description TEXT, order_index INTEGER, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, grounded_refs_json TEXT);
            CREATE TABLE brain.prompt_input_fields (id INTEGER PRIMARY KEY, prompt_template_id INTEGER, field_name TEXT, field_type TEXT, enum_values TEXT, required INTEGER, description TEXT, order_index INTEGER, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT);
            CREATE TABLE brain.ai_calls (id INTEGER PRIMARY KEY, prompt_template_id INTEGER, tier TEXT, outcome TEXT, cached INTEGER, tokens_out INTEGER, ts TEXT);
            CREATE TABLE brain.ai_call_attempts (id INTEGER PRIMARY KEY, ai_call_id INTEGER, outcome TEXT);
            CREATE TABLE brain.ai_call_reports (id INTEGER PRIMARY KEY, prompt_name TEXT, prompt_version INTEGER, failure_class TEXT, diagnosis_md TEXT, evidence_json TEXT, narrative_md TEXT, ts TEXT);
            CREATE TABLE brain.bench_runs (id INTEGER PRIMARY KEY, run_id TEXT, bench_name TEXT, metric TEXT, value REAL, unit TEXT, context_json TEXT);
            CREATE TABLE memory.memories (id INTEGER PRIMARY KEY, type TEXT, title TEXT, situation TEXT, guidance TEXT, naive_response TEXT, rationale TEXT, tags_json TEXT, source_card TEXT, source_event TEXT, source_session TEXT, confidence REAL, created_by TEXT, superseded_by INTEGER);
            CREATE TABLE inbox_messages (id INTEGER PRIMARY KEY, ts TEXT DEFAULT CURRENT_TIMESTAMP, kind TEXT, subject TEXT, body_md TEXT, severity TEXT, related_card_slug TEXT, related_contract_slug TEXT, suggested_reconciler TEXT, suggested_card_action TEXT, payload_json TEXT);
            INSERT INTO brain.prompt_templates (id, name, version, template_md, system_md, max_output_tokens, cross_field_rules_json, superseded_by)
              VALUES (1, 'plan.write', 1, 'You are a planner. Do the thing.', 'sys', 1000, NULL, NULL);
            INSERT INTO brain.ai_calls (id, prompt_template_id, tier, outcome, cached, tokens_out, ts) VALUES
              (1, 1, 'low', 'success', 0, 100, '2026-01-01 00:00:00'),
              (2, 1, 'low', 'success', 0, 100, '2026-01-01 00:00:00'),
              (3, 1, 'low', 'error', 0, 100, '2026-01-01 00:00:00'),
              (4, 1, 'low', 'validation_failed', 0, 100, '2026-01-01 00:00:00'),
              (5, 1, 'low', 'success', 1, 100, '2026-01-01 00:00:00'),
              (6, 1, 'low', 'success', 0, 950, '2026-01-01 00:00:00');
            INSERT INTO brain.ai_call_attempts (ai_call_id, outcome) VALUES (4, 'success');
            INSERT INTO brain.ai_call_reports (prompt_name, prompt_version, failure_class, diagnosis_md, evidence_json, ts)
              VALUES ('plan.write', 1, 'provider_error', 'diag', '{}', '2026-01-01 00:00:00');
        ").unwrap();
        c
    }

    #[test]
    fn folds_calls_into_a_scorecard_with_calibration_and_histogram() {
        let c = db();
        let r = promptQualityReport(&json!({ "min_calls": 1 }), &c).unwrap();
        let sc = &r["scorecards"][0];
        assert_eq!(sc["name"], json!("plan.write"));
        assert_eq!(sc["calls"], json!(6));
        assert_eq!(sc["success"], json!(4));
        assert_eq!(sc["dominant_failure_class"], json!("provider_error"));
        assert!(sc["calibration_score_0_100"].as_i64().unwrap() > 0);
        assert_eq!(r["skipped_low_volume"], json!(0));
    }

    #[test]
    fn skips_low_volume_templates() {
        let c = db();
        let r = promptQualityReport(&json!({ "min_calls": 50 }), &c).unwrap();
        assert_eq!(r["scorecards"].as_array().unwrap().len(), 0);
        assert_eq!(r["skipped_low_volume"], json!(1));
    }

    #[test]
    fn writes_inbox_digest_when_requested() {
        let c = db();
        let r = promptQualityReport(&json!({ "min_calls": 1, "write_inbox": true, "threshold": 90 }), &c).unwrap();
        assert!(!r["inbox_message_id"].is_null());
        let subject: String = c.query_row("SELECT subject FROM inbox_messages WHERE id = 1", [], |row| row.get(0)).unwrap();
        assert!(subject.contains("plan.write"));
    }

    #[test]
    fn writes_memory_lesson_for_offenders() {
        let c = db();
        let r = promptQualityReport(&json!({ "min_calls": 1, "write_memory": true, "threshold": 100 }), &c).unwrap();
        assert_eq!(r["lessons_written"], json!(1));
        let title: String = c.query_row("SELECT title FROM memory.memories WHERE id = 1", [], |row| row.get(0)).unwrap();
        assert_eq!(title, "prompt-quality: plan.write");
    }

    #[test]
    fn narrative_leg_is_a_noop_without_env_flag() {
        let c = db();
        let r = promptQualityReport(&json!({ "min_calls": 1, "narrative": true }), &c).unwrap();
        assert_eq!(r["narratives_written"], json!(0));
    }

    #[test]
    fn round3_and_pct0_match_js_tofixed() {
        assert_eq!(round3(0.66666666), 0.667);
        assert_eq!(pct0(0.5), "50");
        assert_eq!(pct0(0.333), "33");
    }
}
