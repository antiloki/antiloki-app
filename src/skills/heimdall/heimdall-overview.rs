//! heimdall-overview.rs — twin of skills/heimdall/heimdall-overview.mjs (db READ+cascade lane,
//! cross-db: brain + judgments; async-over-sync default export, frozen-clock). NAMES ARE
//! CONTRACT: `heimdallOverview` (default). The CONTROL CENTER payload for the Settings → Heimdall
//! screen: rankings, taxonomy tree, ledger, evidence provenance, per-GATE decisions, recent
//! castings, weight profiles, and several judge-quality reports — all pure reads except one
//! SELF-FRESHENING cascade (attribute + ledger-rebuild) that fires inline when real outcomes have
//! landed since the ledger was last computed and a 60s throttle allows it.
//! Composes four already-converted twins: heimdallAttribute, heimdallLedgerRebuild (both WRITE,
//! self-freshening only), heimdallFlipflops, heimdallAiCalibration (both READ, report sections).
//! Every JS try/catch that swallows to a stored default is mirrored as a closure whose `Err` falls
//! back to that same default value — the surrounding fields must never break on one section's
//! failure (control-center reads never break on a write).
//! FROZEN-CLOCK LANE: `julianday('now')` (throttle) and `date('now')` (spend-today) pinned to the
//! frozen instant 2026-01-01 00:00:00. JS `if (!db) throw err.contextMissing(...)` is unreachable
//! in the db lane (a connection is always injected), so it is not transcribed.
// QUALITY: 78/100 — 16-section control-center aggregation, self-freshening cascade, 6 independently
// try/catch-guarded report sections composed from 4 sibling twins
use regex::Regex;
use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::sync::OnceLock;

// Standard helper functions (playbook §Standard helper functions) — copied verbatim (subset used).
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
        Some(x) => str_qq(Some(x)),
    }
}
fn nz(v: Option<&Value>) -> Option<&Value> {
    v.filter(|x| !x.is_null())
}
fn js_number(v: Option<&Value>) -> f64 {
    match v {
        None => f64::NAN,
        Some(Value::Null) => 0.0,
        Some(Value::Bool(b)) => if *b { 1.0 } else { 0.0 },
        Some(Value::Number(n)) => n.as_f64().unwrap_or(f64::NAN),
        Some(Value::String(s)) => { let t = s.trim(); if t.is_empty() { 0.0 } else { t.parse().unwrap_or(f64::NAN) } },
        Some(_) => f64::NAN,
    }
}

// safeParse(j): `try { return JSON.parse(j); } catch { return null; }` — JSON.parse(undefined)
// throws (String(undefined)="undefined"), JSON.parse(null) parses the string "null" -> null: both
// converge on Value::Null, so None/Null/parse-failure all collapse to the same result as JS.
fn safe_parse(v: Option<&Value>) -> Value {
    match v {
        Some(Value::String(s)) => serde_json::from_str(s).unwrap_or(Value::Null),
        _ => Value::Null,
    }
}

// JS `+x.toFixed(3)`: fixed-3 round then collapse to integer JSON when whole (gotcha 2 — a
// computed fraction, not a DB passthrough, so it needs the int-vs-float rendering fix).
fn to_fixed3(x: f64) -> Value {
    let p: f64 = format!("{:.3}", x).parse().unwrap();
    if p.fract() == 0.0 && p.abs() < 9.007_199_254_740_992e15 { json!(p as i64) } else { json!(p) }
}
// JSON.stringify(N) for a Rust f64 LITERAL (PROVENANCE_TRUST seed values) that may be whole
// (gotcha 2 — json!(1.0f64) renders "1.0" but JS `1.0 === 1` renders "1").
fn num_json(f: f64) -> Value {
    if f.is_finite() && f.fract() == 0.0 && f.abs() < 9.007_199_254_740_992e15 { json!(f as i64) } else { json!(f) }
}

fn version_suffix_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"\.v\d+$").unwrap())
}

// _helpers/casting.mjs PROVENANCE_TRUST — data const, not a unit (see casting.rs); transcribed by
// value in the SAME declaration order Object.entries() would iterate.
const PROVENANCE_TRUST: &[(&str, f64)] = &[
    ("human", 1.0),
    ("prod_gate", 1.0),
    ("prod_terminal", 1.0),
    ("prod_court", 0.7),
    ("ai_judge", 0.4),
    ("ai_panel", 0.6),
    ("bench_det", 0.3),
    ("bench_rubric", 0.2),
    ("playground", 0.2),
    ("lineage_prior", 0.15),
];

pub fn heimdallOverview(input: &Value, db: &Connection) -> Result<Value, String> {
    let limit_raw = js_number(Some(&nz(input.get("castings_limit")).cloned().unwrap_or(json!(40))));
    let limit = if limit_raw.is_nan() { f64::NAN } else { limit_raw.min(200.0) };

    // ── SELF-FRESHENING: materialize inline if real outcomes landed since the last rebuild (or
    // none ever ran), throttled to 60s. Any failure degrades to the stored ledger. ──
    let _ = (|| -> Result<(), String> {
        let last_computed = nz(crate::db_json::query_rows(db, "SELECT MAX(computed_at) AS t FROM brain.heimdall_ledger", &[])?
            .as_array().and_then(|a| a.first()).and_then(|r| r.get("t"))).cloned();
        let throttle_ok = match &last_computed {
            None => true,
            Some(lc) => {
                let s = crate::db_json::query_rows(db, &format!("SELECT ({} - julianday(?)) * 86400.0 AS s", crate::clock::julianday_now_sql()), &[lc.clone()])?
                    .as_array().and_then(|a| a.first()).and_then(|r| nz(r.get("s"))).and_then(|v| v.as_f64())
                    .unwrap_or(999.0);
                s > 60.0
            }
        };
        if throttle_ok {
            let newest = nz(crate::db_json::query_rows(db, "SELECT MAX(ts) AS t FROM brain.ai_calls WHERE cached = 0 AND outcome_class IS NOT NULL", &[])?
                .as_array().and_then(|a| a.first()).and_then(|r| r.get("t"))).cloned();
            if truthy(newest.as_ref()) {
                let should_refresh = match &last_computed {
                    None => true,
                    Some(lc) => tmpl(newest.as_ref()) > tmpl(Some(lc)),
                };
                if should_refresh {
                    crate::heimdall_heimdall_attribute::heimdallAttribute(&json!({}), db)?;
                    crate::heimdall_heimdall_ledger_rebuild::heimdallLedgerRebuild(&json!({}), db)?;
                }
            }
        }
        Ok(())
    })();

    let profiles_rows = crate::db_json::query_rows(db, "SELECT slug, title, weights_json FROM brain.heimdall_weight_profiles ORDER BY slug", &[])?;
    let profiles: Vec<Value> = profiles_rows.as_array().cloned().unwrap_or_default().iter().map(|p| {
        json!({
            "slug": p.get("slug").cloned().unwrap_or(Value::Null),
            "title": p.get("title").cloned().unwrap_or(Value::Null),
            "weights": safe_parse(p.get("weights_json")),
        })
    }).collect();

    // ── the taxonomy TREE with per-category usage ──
    let cat_rows_val = crate::db_json::query_rows(db, "
        SELECT c.slug, c.family, c.title, c.status, c.graded_by, c.weight_profile,
               (SELECT COUNT(*) FROM brain.heimdall_template_categories t WHERE t.category = c.slug) AS template_count,
               (SELECT COUNT(*) FROM brain.heimdall_castings hc WHERE hc.category = c.slug) AS casting_count
        FROM brain.heimdall_categories c ORDER BY c.family, c.slug
    ", &[])?;
    let cat_rows: Vec<Value> = cat_rows_val.as_array().cloned().unwrap_or_default();
    let mut fam_groups: Vec<(Value, Vec<Value>)> = Vec::new();
    for c in &cat_rows {
        let family_value = c.get("family").cloned().unwrap_or(Value::Null);
        match fam_groups.iter_mut().find(|(f, _)| *f == family_value) {
            Some(entry) => entry.1.push(c.clone()),
            None => fam_groups.push((family_value, vec![c.clone()])),
        }
    }
    let families: Vec<Value> = fam_groups.into_iter().map(|(family, categories)| json!({ "family": family, "categories": categories })).collect();

    // ── per-GATE decisions: every template_base Heimdall has seen, its outcome record, its daily
    // spend, and its editable spend CEILING ──
    let gates = crate::db_json::query_rows(db, &format!("
        SELECT hc.template_base, hc.category,
               COUNT(*) AS castings,
               MAX(hc.ts) AS last_ts,
               (SELECT effective_model FROM brain.heimdall_castings x WHERE x.template_base = hc.template_base ORDER BY x.id DESC LIMIT 1) AS last_model,
               (SELECT reason FROM brain.heimdall_castings x WHERE x.template_base = hc.template_base ORDER BY x.id DESC LIMIT 1) AS last_reason,
               SUM(CASE WHEN a.outcome_class = 'clean' THEN 1 ELSE 0 END) AS clean,
               SUM(CASE WHEN a.outcome_class = 'degenerate' THEN 1 ELSE 0 END) AS degenerate,
               SUM(CASE WHEN a.outcome_class = 'truncated' THEN 1 ELSE 0 END) AS truncated,
               SUM(CASE WHEN a.outcome_class = 'validation_failed' THEN 1 ELSE 0 END) AS validation_failed,
               (SELECT gc.ceiling_usd FROM brain.heimdall_gate_ceilings gc WHERE gc.template_base = hc.template_base) AS ceiling_usd,
               (SELECT COALESCE(SUM(a2.cost_usd_actual), 0)
                  FROM brain.ai_calls a2 JOIN brain.prompt_templates p2 ON p2.id = a2.prompt_template_id
                  WHERE (p2.name = hc.template_base OR p2.name LIKE hc.template_base || '.v%')
                    AND date(a2.ts) = date({})) AS spent_today_usd
        FROM brain.heimdall_castings hc
        LEFT JOIN brain.ai_calls a ON a.id = hc.ai_call_id
        GROUP BY hc.template_base, hc.category
        ORDER BY castings DESC, hc.template_base
    ", crate::clock::now_sql_inner()), &[])?;

    // ── the ledger: every rung, ranked within its scope ──
    let ledger = crate::db_json::query_rows(db, "
        SELECT model_slug, scope_kind, scope_ref, window_days, calls, clean, fails, burns, ships,
               weighted_score, sample_ok, computed_at
        FROM brain.heimdall_ledger
        ORDER BY scope_kind, scope_ref, weighted_score DESC
    ", &[])?;
    let ledger_arr: Vec<Value> = ledger.as_array().cloned().unwrap_or_default();

    // ── PROMPT DIMENSION (P-1): bases with >=2 template_version rungs, grouped by (base, model). ──
    let version_rows: Vec<&Value> = ledger_arr.iter().filter(|l| l.get("scope_kind").and_then(|v| v.as_str()) == Some("template_version")).collect();
    let mut pv_groups: Vec<(String, Value)> = Vec::new(); // key -> { template_base, model_slug, versions }
    for v in version_rows {
        let scope_ref = v.get("scope_ref").cloned().unwrap_or(Value::Null);
        let base = version_suffix_re().replace(&js_string(&scope_ref), "").to_string();
        let model_slug = v.get("model_slug").cloned().unwrap_or(Value::Null);
        let key = format!("{} {}", base, tmpl(Some(&model_slug)));
        let entry_version = json!({
            "version": scope_ref,
            "calls": v.get("calls").cloned().unwrap_or(Value::Null),
            "clean": v.get("clean").cloned().unwrap_or(Value::Null),
            "fails": v.get("fails").cloned().unwrap_or(Value::Null),
            "burns": v.get("burns").cloned().unwrap_or(Value::Null),
            "ships": v.get("ships").cloned().unwrap_or(Value::Null),
            "weighted_score": v.get("weighted_score").cloned().unwrap_or(Value::Null),
            "sample_ok": v.get("sample_ok").cloned().unwrap_or(Value::Null),
        });
        match pv_groups.iter_mut().find(|(k, _)| *k == key) {
            Some((_, group)) => group["versions"].as_array_mut().unwrap().push(entry_version),
            None => pv_groups.push((key, json!({ "template_base": base, "model_slug": model_slug, "versions": [entry_version] }))),
        }
    }
    let prompt_versions: Vec<Value> = pv_groups.into_iter()
        .map(|(_, g)| g)
        .filter(|g| g["versions"].as_array().unwrap().len() >= 2)
        .map(|mut g| {
            let mut versions = g["versions"].as_array().cloned().unwrap_or_default();
            versions.sort_by(|a, b| {
                let av = a["weighted_score"].as_f64().unwrap_or(f64::NAN);
                let bv = b["weighted_score"].as_f64().unwrap_or(f64::NAN);
                bv.partial_cmp(&av).unwrap_or(std::cmp::Ordering::Equal)
            });
            g["versions"] = json!(versions);
            g
        })
        .collect();

    // ── evidence provenance breakdown ──
    let evidence = crate::db_json::query_rows(db, "
        SELECT provenance, outcome, COUNT(*) AS n FROM brain.heimdall_evidence
        GROUP BY provenance, outcome ORDER BY provenance, outcome
    ", &[])?;

    // ── recent castings feed + abstention ──
    let castings_recent = crate::db_json::query_rows(db, "
        SELECT id, ai_call_id, template_base, category, requested_tier, effective_model, reason, ts
        FROM brain.heimdall_castings ORDER BY id DESC LIMIT ?
    ", &[json!(limit)])?;

    let trace_rows_val = crate::db_json::query_rows(db, "SELECT trace_json FROM brain.heimdall_castings ORDER BY id DESC LIMIT 200", &[])?;
    let trace_rows: Vec<Value> = trace_rows_val.as_array().cloned().unwrap_or_default();

    let mut cnt_template: i64 = 0;
    let mut cnt_category: i64 = 0;
    let mut cnt_family: i64 = 0;
    let mut cnt_global: i64 = 0;
    let mut cnt_none: i64 = 0;
    for r in &trace_rows {
        let parsed = safe_parse(r.get("trace_json"));
        let rung = parsed.get("decision").and_then(|d| d.get("rung")).cloned().unwrap_or(Value::Null);
        let key = if truthy(Some(&rung)) { rung.as_str() } else { None };
        match key {
            Some("template") => cnt_template += 1,
            Some("category") => cnt_category += 1,
            Some("family") => cnt_family += 1,
            Some("global") => cnt_global += 1,
            _ => cnt_none += 1,
        }
    }
    let traced: i64 = if trace_rows.is_empty() { 1 } else { trace_rows.len() as i64 };

    // ── TRUST MAP: seed hierarchy + every committed override ──
    let trust_map: Vec<Value> = (|| -> Result<Vec<Value>, String> {
        let overrides = crate::db_json::query_rows(db, "SELECT provenance, category, trust, reason_md FROM brain.heimdall_trust_overrides ORDER BY provenance, category", &[])?
            .as_array().cloned().unwrap_or_default();
        let mut tm: Vec<Value> = PROVENANCE_TRUST.iter().map(|(provenance, seed)| {
            let effective = overrides.iter()
                .find(|o| o.get("provenance").and_then(|v| v.as_str()) == Some(*provenance) && o.get("category").and_then(|v| v.as_str()) == Some(""))
                .and_then(|o| nz(o.get("trust")))
                .cloned()
                .unwrap_or_else(|| num_json(*seed));
            let ov_list: Vec<Value> = overrides.iter()
                .filter(|o| o.get("provenance").and_then(|v| v.as_str()) == Some(*provenance))
                .map(|o| {
                    let category_val = o.get("category").cloned().unwrap_or(Value::Null);
                    let category_out = if category_val.as_str() == Some("") { json!("(global)") } else { category_val };
                    json!({ "category": category_out, "trust": o.get("trust").cloned().unwrap_or(Value::Null), "reason_md": o.get("reason_md").cloned().unwrap_or(Value::Null) })
                }).collect();
            json!({ "provenance": provenance, "seed": num_json(*seed), "effective": effective, "overrides": ov_list })
        }).collect();
        tm.sort_by(|a, b| {
            let av = a["effective"].as_f64().unwrap_or(f64::NAN);
            let bv = b["effective"].as_f64().unwrap_or(f64::NAN);
            bv.partial_cmp(&av).unwrap_or(std::cmp::Ordering::Equal)
        });
        Ok(tm)
    })().unwrap_or_default();

    // ── DRY-RUN: what live casting WOULD have done, from each casting's own trace ──
    let mut dry_sampled: i64 = 0;
    let mut dry_resolved: i64 = 0;
    let mut dry_would_override: i64 = 0;
    let mut dry_agree: i64 = 0;
    let mut dry_sample: Vec<Value> = Vec::new();
    for r in &trace_rows {
        let t = safe_parse(r.get("trace_json"));
        if !truthy(Some(&t)) { continue; }
        dry_sampled += 1;
        let would = nz(t.get("scores").and_then(|s| s.as_array()).and_then(|a| a.first()).and_then(|s0| s0.get("model"))).cloned().unwrap_or(Value::Null);
        let actual = nz(t.get("decision").and_then(|d| d.get("effective_model"))).cloned().unwrap_or(Value::Null);
        if !truthy(Some(&would)) || !truthy(Some(&actual)) { continue; }
        dry_resolved += 1;
        if would == actual {
            dry_agree += 1;
        } else {
            dry_would_override += 1;
            if dry_sample.len() < 8 {
                let template_base = nz(t.get("classify").and_then(|c| c.get("template_base"))).cloned().unwrap_or(json!("?"));
                let category = nz(t.get("classify").and_then(|c| c.get("category"))).cloned().unwrap_or(json!("?"));
                let rung = nz(t.get("decision").and_then(|d| d.get("rung"))).cloned().unwrap_or(json!("?"));
                let would_score = nz(t.get("scores").and_then(|s| s.as_array()).and_then(|a| a.first()).and_then(|s0| s0.get("blended"))).cloned().unwrap_or(Value::Null);
                dry_sample.push(json!({
                    "template_base": template_base, "category": category, "rung": rung,
                    "would": would, "would_score": would_score, "actual": actual,
                }));
            }
        }
    }
    let dry_run = json!({
        "sampled": dry_sampled, "resolved": dry_resolved,
        "would_override": dry_would_override, "agree": dry_agree, "sample": dry_sample,
    });

    // ── AI TAGGING: coverage + agreement with humans where both exist ──
    let default_ai_tagging = || json!({
        "total": 0, "active": 0, "superseded": 0,
        "avg_verdict": null, "avg_confidence": null, "agreement": null, "recent": []
    });
    let ai_tagging: Value = (|| -> Result<Value, String> {
        let a = crate::db_json::query_rows(db, "SELECT COUNT(*) AS total, SUM(CASE WHEN superseded_by_human=0 THEN 1 ELSE 0 END) AS active,
          SUM(superseded_by_human) AS superseded, AVG(verdict) AS avg_verdict, AVG(confidence) AS avg_confidence FROM brain.heimdall_ai_tags", &[])?
            .as_array().and_then(|arr| arr.first()).cloned();
        let agg = crate::db_json::query_rows(db, "
          SELECT COUNT(*) AS both, SUM(CASE WHEN (t.verdict>0)=(h.verdict>0) THEN 1 ELSE 0 END) AS agree
          FROM brain.heimdall_ai_tags t JOIN brain.heimdall_human_tags h ON h.ai_call_id = t.target_ai_call_id
        ", &[])?.as_array().and_then(|arr| arr.first()).cloned();
        let recent_rows = crate::db_json::query_rows(db, "
          SELECT t.target_ai_call_id, t.verdict, t.confidence, t.reason_md, t.failure_class, t.low_confidence,
                 t.panel_agree, t.judge_ai_call_id, t.superseded_by_human,
                 hc.template_base, hc.category, hc.effective_model AS target_model, t.ts,
                 ct.card_slug, a.tokens_in, a.tokens_out,
                 (t.confidence IS NULL OR t.confidence <= 0.5 OR (t.verdict < 0 AND t.failure_class IS NULL)) AS needs_retag
          FROM brain.heimdall_ai_tags t
          LEFT JOIN brain.heimdall_castings hc ON hc.ai_call_id = t.target_ai_call_id
          LEFT JOIN brain.ai_calls a ON a.id = t.target_ai_call_id
          LEFT JOIN contracts ct ON ct.slug = a.contract_slug
          ORDER BY t.id DESC LIMIT 100
        ", &[])?;
        let recent: Vec<Value> = recent_rows.as_array().cloned().unwrap_or_default().into_iter().map(|r| {
            let mut obj = r.as_object().cloned().unwrap_or_default();
            let needs_retag_raw = obj.get("needs_retag").cloned().unwrap_or(Value::Null);
            obj.insert("needs_retag".into(), json!(truthy(Some(&needs_retag_raw))));
            let jet = crate::heimdall__helpers_casting::judgeEstTokens(&json!({
                "tokens_in": obj.get("tokens_in").cloned().unwrap_or(Value::Null),
                "tokens_out": obj.get("tokens_out").cloned().unwrap_or(Value::Null),
            }));
            obj.insert("judge_est_tokens".into(), jet);
            Value::Object(obj)
        }).collect();
        let needs_retag = crate::db_json::query_rows(db, "
          SELECT COUNT(*) AS n FROM brain.heimdall_ai_tags
          WHERE superseded_by_human = 0
            AND (confidence IS NULL OR confidence <= 0.5 OR (verdict < 0 AND failure_class IS NULL))
        ", &[])?.as_array().and_then(|a| a.first()).and_then(|r| nz(r.get("n"))).cloned().unwrap_or(json!(0));
        let low_conf = crate::db_json::query_rows(db, "SELECT COUNT(*) AS n FROM brain.heimdall_ai_tags WHERE low_confidence = 1 AND superseded_by_human = 0", &[])?
            .as_array().and_then(|a| a.first()).and_then(|r| nz(r.get("n"))).cloned().unwrap_or(json!(0));
        let failure_classes = crate::db_json::query_rows(db, "
          SELECT failure_class, COUNT(*) AS n FROM brain.heimdall_ai_tags
          WHERE failure_class IS NOT NULL GROUP BY failure_class ORDER BY n DESC
        ", &[])?;

        let agreement = match agg.as_ref().and_then(|x| x.get("both")) {
            Some(both_v) if truthy(Some(both_v)) => {
                let agree_v = agg.as_ref().and_then(|x| x.get("agree")).cloned().unwrap_or(Value::Null);
                let pct = to_fixed3(agree_v.as_f64().unwrap_or(0.0) / both_v.as_f64().unwrap_or(0.0));
                json!({ "both": both_v, "agree": agree_v, "pct": pct })
            }
            _ => Value::Null,
        };

        Ok(json!({
            "total": a.as_ref().and_then(|x| nz(x.get("total"))).cloned().unwrap_or(json!(0)),
            "active": a.as_ref().and_then(|x| nz(x.get("active"))).cloned().unwrap_or(json!(0)),
            "superseded": a.as_ref().and_then(|x| nz(x.get("superseded"))).cloned().unwrap_or(json!(0)),
            "low_confidence": low_conf, "needs_retag": needs_retag, "failure_classes": failure_classes,
            "avg_verdict": a.as_ref().and_then(|x| nz(x.get("avg_verdict"))).cloned().unwrap_or(Value::Null),
            "avg_confidence": a.as_ref().and_then(|x| nz(x.get("avg_confidence"))).cloned().unwrap_or(Value::Null),
            "agreement": agreement,
            "recent": recent,
        }))
    })().unwrap_or_else(|_| default_ai_tagging());

    // ── AI-3/5/8 report card (heimdallAiCalibration), summarized/capped here ──
    let ai_report: Value = (|| -> Result<Value, String> {
        let rep = crate::heimdall_heimdall_ai_calibration::heimdallAiCalibration(&json!({}), db)?;
        let calibration: Vec<Value> = rep.get("calibration").and_then(|v| v.as_array()).cloned().unwrap_or_default().into_iter().take(10).collect();
        let cd = rep.get("court_disagreements").cloned().unwrap_or(json!({ "count": 0, "pairs": [] }));
        let pairs: Vec<Value> = cd.get("pairs").and_then(|v| v.as_array()).cloned().unwrap_or_default().into_iter().take(5).collect();
        let weak_gates: Vec<Value> = rep.get("weak_gates").and_then(|v| v.as_array()).cloned().unwrap_or_default().into_iter().take(5).collect();
        Ok(json!({
            "calibration": calibration,
            "court_disagreements": { "count": cd.get("count").cloned().unwrap_or(json!(0)), "pairs": pairs },
            "weak_gates": weak_gates,
        }))
    })().unwrap_or_else(|_| json!({ "calibration": [], "court_disagreements": { "count": 0, "pairs": [] }, "weak_gates": [] }));

    // ── Judge-consistency (flipflops) + HUMAN vs COURT ──
    let flipflops_base: Value = (|| -> Result<Value, String> {
        let ff = crate::heimdall_heimdall_flipflops::heimdallFlipflops(&json!({}), db)?;
        let pairs: Vec<Value> = ff.get("pairs").and_then(|v| v.as_array()).cloned().unwrap_or_default().into_iter().take(5).collect();
        Ok(json!({ "count": ff.get("count").cloned().unwrap_or(json!(0)), "pairs": pairs, "human_vs_court": 0 }))
    })().unwrap_or_else(|_| json!({ "count": 0, "pairs": [], "human_vs_court": 0 }));
    let mut flipflops = flipflops_base.as_object().cloned().unwrap_or_default();
    if let Ok(rows) = crate::db_json::query_rows(db, "
        SELECT COUNT(*) AS n FROM brain.heimdall_human_tags t
        WHERE t.verdict > 0 AND EXISTS (SELECT 1 FROM brain.heimdall_evidence e WHERE e.ai_call_id = t.ai_call_id AND e.outcome = 'burn')
    ", &[]) {
        if let Some(n) = rows.as_array().and_then(|a| a.first()).and_then(|r| r.get("n")) {
            flipflops.insert("human_vs_court".into(), n.clone());
        }
    }

    let human_tags_row = crate::db_json::query_rows(db, "SELECT COUNT(*) AS n, AVG(verdict) AS avg FROM brain.heimdall_human_tags", &[])?
        .as_array().and_then(|a| a.first()).cloned();
    let human_tags = json!({
        "n": human_tags_row.as_ref().and_then(|r| nz(r.get("n"))).cloned().unwrap_or(json!(0)),
        "avg": human_tags_row.as_ref().and_then(|r| nz(r.get("avg"))).cloned().unwrap_or(Value::Null),
    });

    let categories_active = cat_rows.iter().filter(|c| c.get("status").and_then(|v| v.as_str()) == Some("active")).count();
    let template_mappings = crate::db_json::query_rows(db, "SELECT COUNT(*) AS n FROM brain.heimdall_template_categories", &[])?
        .as_array().and_then(|a| a.first()).and_then(|r| r.get("n")).cloned().unwrap_or(Value::Null);
    let castings_total = crate::db_json::query_rows(db, "SELECT COUNT(*) AS n FROM brain.heimdall_castings", &[])?
        .as_array().and_then(|a| a.first()).and_then(|r| r.get("n")).cloned().unwrap_or(Value::Null);
    let evidence_total = crate::db_json::query_rows(db, "SELECT COUNT(*) AS n FROM brain.heimdall_evidence", &[])?
        .as_array().and_then(|a| a.first()).and_then(|r| r.get("n")).cloned().unwrap_or(Value::Null);
    let ledger_computed_at = nz(crate::db_json::query_rows(db, "SELECT MAX(computed_at) AS t FROM brain.heimdall_ledger", &[])?
        .as_array().and_then(|a| a.first()).and_then(|r| r.get("t"))).cloned().unwrap_or(Value::Null);

    let counts = json!({
        "categories": cat_rows.len() as i64,
        "categories_active": categories_active as i64,
        "template_mappings": template_mappings,
        "castings_total": castings_total,
        "evidence_total": evidence_total,
        "ledger_rows": ledger_arr.len() as i64,
        "ledger_computed_at": ledger_computed_at,
    });

    let mut rung_counts = Map::new();
    rung_counts.insert("template".into(), json!(cnt_template));
    rung_counts.insert("category".into(), json!(cnt_category));
    rung_counts.insert("family".into(), json!(cnt_family));
    rung_counts.insert("global".into(), json!(cnt_global));
    rung_counts.insert("none".into(), json!(cnt_none));

    let mut out = Map::new();
    out.insert("ok".into(), json!(true));
    out.insert("profiles".into(), json!(profiles));
    out.insert("families".into(), json!(families));
    out.insert("gates".into(), gates);
    out.insert("ledger".into(), ledger);
    out.insert("prompt_versions".into(), json!(prompt_versions));
    out.insert("evidence".into(), evidence);
    out.insert("castings_recent".into(), castings_recent);
    out.insert("abstention".into(), json!({
        "sampled": trace_rows.len() as i64,
        "rung_counts": Value::Object(rung_counts),
        "abstain_pct": to_fixed3(cnt_none as f64 / traced as f64),
    }));
    out.insert("human_tags".into(), human_tags);
    out.insert("trust_map".into(), json!(trust_map));
    out.insert("dry_run".into(), dry_run);
    out.insert("ai_tagging".into(), ai_tagging);
    out.insert("ai_report".into(), ai_report);
    out.insert("flipflops".into(), Value::Object(flipflops));
    out.insert("counts".into(), counts);
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn schema(db: &Connection) {
        db.execute_batch("
            ATTACH ':memory:' AS brain;
            ATTACH ':memory:' AS judgments;
            CREATE TABLE cards (slug TEXT PRIMARY KEY, status TEXT NOT NULL);
            CREATE TABLE contracts (slug TEXT PRIMARY KEY, card_slug TEXT NOT NULL);
            CREATE TABLE brain.prompt_templates (id INTEGER PRIMARY KEY, name TEXT NOT NULL);
            CREATE TABLE brain.ai_calls (id INTEGER PRIMARY KEY, ts TEXT NOT NULL, prompt_template_id INTEGER NOT NULL, cached INTEGER NOT NULL DEFAULT 0, model_id TEXT, outcome TEXT NOT NULL, outcome_class TEXT, contract_slug TEXT, cost_usd_actual REAL, tokens_in INTEGER, tokens_out INTEGER);
            CREATE TABLE brain.heimdall_template_categories (template_base TEXT PRIMARY KEY, category TEXT NOT NULL);
            CREATE TABLE brain.heimdall_categories (slug TEXT PRIMARY KEY, family TEXT NOT NULL, title TEXT, status TEXT, graded_by TEXT, weight_profile TEXT);
            CREATE TABLE brain.heimdall_evidence (id INTEGER PRIMARY KEY AUTOINCREMENT, ts TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')), model_slug TEXT NOT NULL, category TEXT, template_base TEXT, provenance TEXT NOT NULL, outcome TEXT NOT NULL, ai_call_id INTEGER);
            CREATE UNIQUE INDEX brain.uq_heimdall_evidence_call ON heimdall_evidence(ai_call_id, provenance, outcome);
            CREATE TABLE brain.heimdall_ledger (model_slug TEXT NOT NULL, scope_kind TEXT NOT NULL, scope_ref TEXT NOT NULL, window_days INTEGER NOT NULL, calls INTEGER NOT NULL DEFAULT 0, clean INTEGER NOT NULL DEFAULT 0, fails INTEGER NOT NULL DEFAULT 0, burns INTEGER NOT NULL DEFAULT 0, ships INTEGER NOT NULL DEFAULT 0, weighted_score REAL, sample_ok INTEGER NOT NULL DEFAULT 0, computed_at TEXT);
            CREATE TABLE brain.heimdall_weight_profiles (slug TEXT PRIMARY KEY, title TEXT, weights_json TEXT);
            CREATE TABLE brain.heimdall_castings (id INTEGER PRIMARY KEY AUTOINCREMENT, ai_call_id INTEGER, template_base TEXT, category TEXT, requested_tier TEXT, effective_model TEXT, reason TEXT, ts TEXT, trace_json TEXT);
            CREATE TABLE brain.heimdall_gate_ceilings (template_base TEXT PRIMARY KEY, ceiling_usd REAL);
            CREATE TABLE brain.heimdall_trust_overrides (provenance TEXT, category TEXT, trust REAL, reason_md TEXT);
            CREATE TABLE brain.heimdall_human_tags (id INTEGER PRIMARY KEY AUTOINCREMENT, ai_call_id INTEGER, verdict INTEGER);
            CREATE TABLE brain.heimdall_ai_tags (id INTEGER PRIMARY KEY AUTOINCREMENT, target_ai_call_id INTEGER, verdict INTEGER, confidence REAL, reason_md TEXT, failure_class TEXT, low_confidence INTEGER, panel_agree INTEGER, judge_ai_call_id INTEGER, superseded_by_human INTEGER, ts TEXT);
            CREATE TABLE judgments.judgment_cases (judgment_id TEXT PRIMARY KEY, card_slug TEXT, scope_kind TEXT, scope_ref TEXT, verdict TEXT, goal_line TEXT, updated_at TEXT);
            CREATE TABLE judgments.judgment_items (judgment_id TEXT, side TEXT, item_id TEXT, statement TEXT, judge_value INTEGER);
        ").unwrap();
    }

    #[test]
    fn cold_start_everything_empty() {
        let db = Connection::open_in_memory().unwrap();
        schema(&db);
        let r = heimdallOverview(&json!({}), &db).unwrap();
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["profiles"], json!([]));
        assert_eq!(r["families"], json!([]));
        assert_eq!(r["gates"], json!([]));
        assert_eq!(r["ledger"], json!([]));
        assert_eq!(r["prompt_versions"], json!([]));
        assert_eq!(r["abstention"], json!({ "sampled": 0, "rung_counts": { "template": 0, "category": 0, "family": 0, "global": 0, "none": 0 }, "abstain_pct": 0 }));
        assert_eq!(r["human_tags"], json!({ "n": 0, "avg": null }));
        assert_eq!(r["dry_run"], json!({ "sampled": 0, "resolved": 0, "would_override": 0, "agree": 0, "sample": [] }));
        assert_eq!(r["ai_tagging"]["total"], json!(0));
        assert_eq!(r["ai_tagging"]["recent"], json!([]));
        assert_eq!(r["ai_report"], json!({ "calibration": [], "court_disagreements": { "count": 0, "pairs": [] }, "weak_gates": [] }));
        assert_eq!(r["flipflops"], json!({ "count": 0, "pairs": [], "human_vs_court": 0 }));
        assert_eq!(r["counts"], json!({ "categories": 0, "categories_active": 0, "template_mappings": 0, "castings_total": 0, "evidence_total": 0, "ledger_rows": 0, "ledger_computed_at": null }));
        let trust_map = r["trust_map"].as_array().unwrap();
        assert_eq!(trust_map.len(), 10);
        assert!(trust_map.iter().all(|t| t["overrides"] == json!([])));
        let human = trust_map.iter().find(|t| t["provenance"] == json!("human")).unwrap();
        assert_eq!(human["seed"], json!(1));
        assert_eq!(human["effective"], json!(1));
    }

    #[test]
    fn rich_data_read_only_self_freshening_skipped_via_fresh_ledger() {
        let db = Connection::open_in_memory().unwrap();
        schema(&db);
        db.execute_batch("
            INSERT INTO brain.heimdall_weight_profiles (slug, title, weights_json) VALUES
              ('general', 'General', '{\"a\":1}'), ('coder', 'Coder', '{\"b\":2}'), ('broken', 'Broken', 'not-json');
            INSERT INTO brain.heimdall_categories (slug, family, title, status, graded_by, weight_profile) VALUES
              ('conversational', 'LANGUAGE', 'Conversational', 'active', 'ai', 'general'),
              ('coding', 'CODE', 'Coding', 'active', 'ai', 'coder'),
              ('archived-cat', 'LANGUAGE', 'Archived', 'inactive', 'human', 'general');
            INSERT INTO brain.heimdall_template_categories (template_base, category) VALUES
              ('chat', 'conversational'), ('codegen', 'coding');
            INSERT INTO brain.prompt_templates (id, name) VALUES (1, 'chat.v1');
            INSERT INTO brain.ai_calls (id, ts, prompt_template_id, cached, model_id, outcome, outcome_class, contract_slug, cost_usd_actual, tokens_in, tokens_out) VALUES
              (101, '2026-01-01 00:00:00', 1, 0, 'claude-sonnet-4-6', 'success', 'clean', NULL, 0.02, 100, 50),
              (102, '2026-01-02 00:00:00', 1, 0, 'gpt-5', 'error', 'degenerate', NULL, 0.05, 200, 80),
              (103, '2026-01-04 00:00:00', 1, 0, 'claude-sonnet-4-6', 'success', 'clean', 'CT-1', 0.01, 50, 20);
            INSERT INTO contracts (slug, card_slug) VALUES ('CT-1', 'CARD-1');
            INSERT INTO brain.heimdall_castings (ai_call_id, template_base, category, requested_tier, effective_model, reason, ts, trace_json) VALUES
              (101, 'chat', 'conversational', 'mid', 'claude-sonnet-4-6', 'observe', '2026-01-01 00:00:00', '{\"classify\":{\"template_base\":\"chat\",\"category\":\"conversational\"},\"decision\":{\"rung\":\"template\",\"effective_model\":\"claude-sonnet-4-6\"},\"scores\":[{\"model\":\"claude-sonnet-4-6\",\"blended\":0.8}]}'),
              (102, 'chat', 'conversational', 'mid', 'gpt-5', 'override', '2026-01-02 00:00:00', '{\"classify\":{\"template_base\":\"chat\",\"category\":\"conversational\"},\"decision\":{\"rung\":\"category\",\"effective_model\":\"gpt-5\"},\"scores\":[{\"model\":\"claude-sonnet-4-6\",\"blended\":0.75}]}'),
              (NULL, 'codegen', 'coding', '', NULL, 'observe', '2026-01-03 00:00:00', '{\"decision\":{\"rung\":null}}'),
              (103, 'chat', 'conversational', 'low', 'claude-sonnet-4-6', 'ledger', '2026-01-04 00:00:00', '{\"decision\":{\"rung\":\"global\"}}');
            INSERT INTO brain.heimdall_gate_ceilings (template_base, ceiling_usd) VALUES ('chat', 5.0);
            INSERT INTO brain.heimdall_evidence (ts, model_slug, category, template_base, provenance, outcome, ai_call_id) VALUES
              ('2026-01-01 00:00:00', 'claude-sonnet-4-6', 'conversational', 'chat', 'prod_gate', 'clean', 101),
              ('2026-01-02 00:00:00', 'gpt-5', 'conversational', 'chat', 'prod_gate', 'fail', 102),
              ('2026-01-04 00:00:00', 'claude-sonnet-4-6', 'conversational', 'chat', 'human', 'clean', 103),
              ('2026-01-01 00:00:00', 'claude-sonnet-4-6', 'conversational', 'chat', 'prod_court', 'burn', 101);
            INSERT INTO brain.heimdall_ledger (model_slug, scope_kind, scope_ref, window_days, calls, clean, fails, burns, ships, weighted_score, sample_ok, computed_at) VALUES
              ('claude-sonnet-4-6', 'template_version', 'chat.v1', 30, 10, 8, 2, 0, 0, 0.75, 1, '2026-01-01 00:00:00'),
              ('claude-sonnet-4-6', 'template_version', 'chat.v2', 30, 20, 18, 2, 0, 0, 0.85, 1, '2026-01-01 00:00:00'),
              ('claude-sonnet-4-6', 'template', 'chat', 30, 30, 26, 4, 0, 0, 0.8, 1, '2026-01-01 00:00:00'),
              ('claude-sonnet-4-6', 'category', 'conversational', 30, 30, 26, 4, 0, 0, 0.8, 1, '2026-01-01 00:00:00'),
              ('claude-sonnet-4-6', 'global', 'global', 30, 50, 40, 10, 0, 0, 0.78, 1, '2026-01-01 00:00:00');
            INSERT INTO brain.heimdall_human_tags (ai_call_id, verdict) VALUES (101, 1), (102, -1);
            INSERT INTO brain.heimdall_trust_overrides (provenance, category, trust, reason_md) VALUES
              ('prod_gate', '', 0.95, 'override global prod_gate'),
              ('ai_judge', 'conversational', 0.5, 'category-specific ai_judge');
            INSERT INTO brain.heimdall_ai_tags (target_ai_call_id, verdict, confidence, reason_md, failure_class, low_confidence, panel_agree, judge_ai_call_id, superseded_by_human, ts) VALUES
              (101, 1, 0.9, 'good', NULL, 0, 1, 999, 0, '2026-01-01 00:00:00'),
              (102, -1, 0.3, 'bad', 'off-scope', 1, 0, 998, 0, '2026-01-02 00:00:00'),
              (103, -1, NULL, 'silence', NULL, 0, 0, 997, 1, '2026-01-03 00:00:00');
            INSERT INTO judgments.judgment_cases (judgment_id, card_slug) VALUES ('J-1', 'CARD-1');
            INSERT INTO judgments.judgment_items (judgment_id, side, item_id, statement, judge_value) VALUES
              ('J-1', 'defender', 'd1', 'widget lacks accessibility labels', 1),
              ('J-1', 'defender', 'd2', 'widget lacks aria label completely', 0);
        ").unwrap();

        let r = heimdallOverview(&json!({ "castings_limit": 2 }), &db).unwrap();
        assert_eq!(r["ok"], json!(true));

        // self-freshening was skipped (ledger already fresh at the frozen instant) — no extra evidence/ledger rows
        assert_eq!(r["counts"]["evidence_total"], json!(4));
        assert_eq!(r["counts"]["ledger_rows"], json!(5));
        assert_eq!(r["counts"]["ledger_computed_at"], json!("2026-01-01 00:00:00"));
        assert_eq!(r["counts"]["categories"], json!(3));
        assert_eq!(r["counts"]["categories_active"], json!(2));
        assert_eq!(r["counts"]["template_mappings"], json!(2));
        assert_eq!(r["counts"]["castings_total"], json!(4));

        // castings_limit=2 clamps the recent feed to the 2 highest ids
        assert_eq!(r["castings_recent"].as_array().unwrap().len(), 2);

        // safeParse: valid json, valid json, malformed -> null
        let profiles = r["profiles"].as_array().unwrap();
        assert_eq!(profiles.len(), 3);
        assert_eq!(profiles.iter().find(|p| p["slug"] == json!("general")).unwrap()["weights"], json!({"a":1}));
        assert_eq!(profiles.iter().find(|p| p["slug"] == json!("broken")).unwrap()["weights"], Value::Null);

        // families grouped, 2 families (LANGUAGE has 2 categories, CODE has 1)
        let families = r["families"].as_array().unwrap();
        assert_eq!(families.len(), 2);
        let lang = families.iter().find(|f| f["family"] == json!("LANGUAGE")).unwrap();
        assert_eq!(lang["categories"].as_array().unwrap().len(), 2);

        // gates: 2 groups (chat/conversational, codegen/coding); chat has a ceiling + today's spend
        let gates = r["gates"].as_array().unwrap();
        let chat_gate = gates.iter().find(|g| g["template_base"] == json!("chat")).unwrap();
        assert_eq!(chat_gate["castings"], json!(3));
        assert_eq!(chat_gate["clean"], json!(2));
        assert_eq!(chat_gate["degenerate"], json!(1));
        assert_eq!(chat_gate["ceiling_usd"], json!(5.0));
        assert_eq!(chat_gate["spent_today_usd"], json!(0.02));
        assert_eq!(chat_gate["last_model"], json!("claude-sonnet-4-6"));
        assert_eq!(chat_gate["last_reason"], json!("ledger"));
        let codegen_gate = gates.iter().find(|g| g["template_base"] == json!("codegen")).unwrap();
        assert_eq!(codegen_gate["ceiling_usd"], Value::Null);

        // prompt_versions: chat has 2 versions for claude-sonnet-4-6, sorted by weighted_score desc
        let pv = r["prompt_versions"].as_array().unwrap();
        assert_eq!(pv.len(), 1);
        assert_eq!(pv[0]["template_base"], json!("chat"));
        let versions = pv[0]["versions"].as_array().unwrap();
        assert_eq!(versions[0]["version"], json!("chat.v2"));
        assert_eq!(versions[1]["version"], json!("chat.v1"));

        // abstention: rungs template/category/global each 1, none 1 (4 castings total)
        assert_eq!(r["abstention"]["rung_counts"], json!({ "template": 1, "category": 1, "family": 0, "global": 1, "none": 1 }));
        assert_eq!(r["abstention"]["abstain_pct"], json!(0.25));

        // dry_run: agrees once (id101), overrides once (id102) with a sample entry
        assert_eq!(r["dry_run"]["sampled"], json!(4));
        assert_eq!(r["dry_run"]["resolved"], json!(2));
        assert_eq!(r["dry_run"]["agree"], json!(1));
        assert_eq!(r["dry_run"]["would_override"], json!(1));
        let sample = r["dry_run"]["sample"].as_array().unwrap();
        assert_eq!(sample.len(), 1);
        assert_eq!(sample[0]["would"], json!("claude-sonnet-4-6"));
        assert_eq!(sample[0]["actual"], json!("gpt-5"));

        // trust_map: prod_gate has a global override, ai_judge keeps its seed but lists its category override
        let trust_map = r["trust_map"].as_array().unwrap();
        let prod_gate = trust_map.iter().find(|t| t["provenance"] == json!("prod_gate")).unwrap();
        assert_eq!(prod_gate["effective"], json!(0.95));
        let ai_judge = trust_map.iter().find(|t| t["provenance"] == json!("ai_judge")).unwrap();
        assert_eq!(ai_judge["effective"], json!(0.4));
        assert_eq!(ai_judge["overrides"], json!([{ "category": "conversational", "trust": 0.5, "reason_md": "category-specific ai_judge" }]));

        // ai_tagging: needs_retag + judge_est_tokens computed per row, agreement 100% (both signs matched)
        assert_eq!(r["ai_tagging"]["total"], json!(3));
        assert_eq!(r["ai_tagging"]["active"], json!(2));
        assert_eq!(r["ai_tagging"]["superseded"], json!(1));
        assert_eq!(r["ai_tagging"]["low_confidence"], json!(1));
        assert_eq!(r["ai_tagging"]["needs_retag"], json!(1));
        assert_eq!(r["ai_tagging"]["agreement"], json!({ "both": 2, "agree": 2, "pct": 1 }));
        let recent = r["ai_tagging"]["recent"].as_array().unwrap();
        assert_eq!(recent.len(), 3);
        let r103 = recent.iter().find(|x| x["target_ai_call_id"] == json!(103)).unwrap();
        assert_eq!(r103["needs_retag"], json!(true));
        assert_eq!(r103["card_slug"], json!("CARD-1"));
        let r101 = recent.iter().find(|x| x["target_ai_call_id"] == json!(101)).unwrap();
        assert_eq!(r101["needs_retag"], json!(false));
        assert_eq!(r101["judge_est_tokens"], json!(1400));

        // ai_report AI-5: verdict>0 AND burned -> id101 shows up
        assert_eq!(r["ai_report"]["court_disagreements"]["count"], json!(1));
        assert_eq!(r["ai_report"]["court_disagreements"]["pairs"][0]["ai_call_id"], json!(101));

        // flipflops: one opposite-ruling pair sharing the subject, human_vs_court from the burned id101 positive tag
        assert_eq!(r["flipflops"]["count"], json!(1));
        assert_eq!(r["flipflops"]["human_vs_court"], json!(1));

        // human_tags: n=2, avg=(1-1)/2=0 (SQLite AVG() always yields a REAL, so 0.0 not 0)
        assert_eq!(r["human_tags"], json!({ "n": 2, "avg": 0.0 }));
    }

    #[test]
    fn self_freshening_triggers_and_rolls_into_gates() {
        let db = Connection::open_in_memory().unwrap();
        schema(&db);
        db.execute_batch("
            INSERT INTO brain.heimdall_categories (slug, family, title, status, graded_by, weight_profile) VALUES ('code-stub', 'BUILD', 'Code Stub', 'active', 'ai', 'general');
            INSERT INTO brain.heimdall_template_categories (template_base, category) VALUES ('blueprint.stub', 'code-stub');
            INSERT INTO brain.prompt_templates (id, name) VALUES (1, 'blueprint.stub.v1');
            INSERT INTO brain.ai_calls (id, ts, prompt_template_id, cached, model_id, outcome, outcome_class) VALUES (10, '2026-01-01 00:00:00', 1, 0, 'm-x', 'success', 'clean');
            INSERT INTO brain.heimdall_castings (ai_call_id, template_base, category, requested_tier, effective_model, reason, ts, trace_json) VALUES
              (10, 'blueprint.stub', 'code-stub', 'low', 'm-x', 'observe', '2026-01-01 00:00:00', '{\"decision\":{\"rung\":null}}');
            INSERT INTO brain.heimdall_human_tags (ai_call_id, verdict) VALUES (10, 2);
        ").unwrap();

        let r = heimdallOverview(&json!({}), &db).unwrap();
        assert_eq!(r["ok"], json!(true));
        // attribute inserted 1 evidence row (CALL level), ledger-rebuild fanned it into 5 rungs
        // (template_version 'blueprint.stub.v1' != base 'blueprint.stub', + template/category/family/global)
        assert_eq!(r["counts"]["evidence_total"], json!(1));
        assert_eq!(r["counts"]["ledger_rows"], json!(5));
        assert_eq!(r["counts"]["ledger_computed_at"], json!("2026-01-01 00:00:00"));
        assert_eq!(r["ledger"].as_array().unwrap().len(), 5);
        // a single template_version -> not >=2 -> no prompt_versions group
        assert_eq!(r["prompt_versions"], json!([]));

        let families = r["families"].as_array().unwrap();
        assert_eq!(families.len(), 1);
        assert_eq!(families[0]["family"], json!("BUILD"));
        assert_eq!(families[0]["categories"][0]["casting_count"], json!(1));

        let gates = r["gates"].as_array().unwrap();
        assert_eq!(gates.len(), 1);
        assert_eq!(gates[0]["clean"], json!(1));

        // SQLite AVG() always yields a REAL, so 2.0 not 2
        assert_eq!(r["human_tags"], json!({ "n": 1, "avg": 2.0 }));
    }
}
