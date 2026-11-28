//! constitution-report.rs — twin of skills/constitution/constitution-report.mjs (db lane,
//! cross-db `laws`+`judgments`, plus unqualified `code_nodes` on the main db). NAMES ARE
//! CONTRACT: `constitutionReport` (default). THE LENS payload — pure reads over one judged
//! snapshot: aggregates + ranks the already-computed verdicts for the screen (docs/
//! constitution-review-plan.html D7). $0, no AI calls of its own.
//!
//! Reuses already-converted twins: `ensureConstitutionSchema`/`normalizeScope` (schema.rs),
//! `composedConstitution` (constitution-law.rs — the same cross-module-reuse pattern
//! constitution-estimate.rs's module doc documents, except here the REAL pub twin already
//! exists so no local inlining is needed), `categoryOf` (categories.rs), `npmInstalled`
//! (codebase/lint-detect.rs). `LINTERS` (lint-detect.mjs's private module-level data) has no
//! `pub` accessor on its Rust twin, so — same precedent as constitution-estimate.rs's local
//! `SCOPES` copy — this file carries its OWN private copy of the two-entry table (id/label/
//! via/exts only; `pkg`/`bin` aren't read by this skill), while still calling through to the
//! real `npmInstalled` twin for the one fs-dependent bit.
//!
//! `npmInstalled` is a REAL `existsSync` probe (no subprocess here — unlike lint-detect's own
//! default export, this skill never calls the ruff/path branch, so no subprocess timing is in
//! play). Both lanes run the same probe against the same physical filesystem during conform-db,
//! so results agree by construction — same convention lint-detect.rs's own conversion already
//! established as convertible.
//!
//! Every computed float (risk rounded to 2dp, jaccard, cost_usd rounded to 4dp, pearson r,
//! by_category mass/score, delta) is re-rendered through `num_out` to match JS's automatic
//! int/float number normalization (gotcha #2) — Rust's `serde_json::Number` from an f64 keeps a
//! decimal point even for whole values, JS's doesn't.
// QUALITY: 74/100 — a wide but shallow aggregation module: many small transcribed slices, no
// deep algorithmic core beyond the Pearson correlation and Jaccard duplication pass.

use rusqlite::Connection;
use serde_json::{json, Map, Value};

fn truthy(v: Option<&Value>) -> bool { match v { None|Some(Value::Null)=>false, Some(Value::Bool(b))=>*b, Some(Value::Number(n))=>n.as_f64().map_or(false,|f|f!=0.0&&!f.is_nan()), Some(Value::String(s))=>!s.is_empty(), Some(_)=>true } }
fn js_string(v: &Value) -> String { match v { Value::Null=>"null".into(), Value::String(s)=>s.clone(), Value::Number(n)=>n.to_string(), Value::Bool(b)=>b.to_string(), Value::Array(a)=>a.iter().map(js_string).collect::<Vec<_>>().join(","), Value::Object(_)=>"[object Object]".into() } } // = JS String(v)
fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) } // JS `??` right operand
fn js_number(v: Option<&Value>) -> f64 { match v { None=>f64::NAN, Some(Value::Null)=>0.0, Some(Value::Bool(b))=>if *b {1.0} else {0.0}, Some(Value::Number(n))=>n.as_f64().unwrap_or(f64::NAN), Some(Value::String(s))=>{let t=s.trim(); if t.is_empty(){0.0}else{t.parse().unwrap_or(f64::NAN)}}, Some(_)=>f64::NAN } } // = JS Number(v)

// JS `Math.round`: floor(x + 0.5) — half-up, unlike Rust's half-away-from-zero `.round()`.
fn js_round(x: f64) -> f64 { (x + 0.5).floor() }
// `Number(x.toFixed(n))`: round-half-up at `n` decimals, then re-parse as a number.
fn to_fixed_num(x: f64, decimals: i32) -> f64 { let m = 10f64.powi(decimals); js_round(x * m) / m }
fn round2(x: f64) -> f64 { to_fixed_num(x, 2) }
// Render a computed f64 the way JS's automatic number normalization would: a whole-valued
// float becomes a bare integer in JSON.stringify (no `.0`), matching gotcha #2.
fn num_out(f: f64) -> Value {
    if f.is_finite() && f.fract() == 0.0 && f.abs() < 1e15 { json!(f as i64) } else { json!(f) }
}

// `try { JSON.parse(s ?? 'null') } catch { null }` — an absent/NULL column and malformed JSON
// text both collapse to `null`; only well-formed JSON text survives.
fn safe(v: Option<&Value>) -> Value {
    let text = match v { Some(Value::String(s)) => s.clone(), None | Some(Value::Null) => "null".to_string(), Some(other) => js_string(other) };
    serde_json::from_str(&text).unwrap_or(Value::Null)
}

const WORST_CAP: usize = 60;

// lint-detect.mjs's private `LINTERS` data (id/label/via/exts only — this skill never reads
// `pkg`/`bin`). See module doc for why this is a local copy, not an import.
struct LinterMeta { id: &'static str, label: &'static str, via: &'static str, exts: &'static [&'static str] }
const LINTERS: [LinterMeta; 2] = [
    LinterMeta { id: "eslint", label: "ESLint (JS/TS)", via: "npm", exts: &[".js", ".jsx", ".mjs", ".cjs", ".ts", ".tsx", ".mts", ".cts"] },
    LinterMeta { id: "ruff", label: "Ruff (Python)", via: "path", exts: &[".py", ".pyi"] },
];

// = JS `String(r.path).match(/\.[^./\\]+$/)?.[0].toLowerCase()`: rightmost dot in the final path
// segment, requiring >=1 char after it with no further separator.
fn ext_of(path: &str) -> Option<String> {
    let filename = match path.rfind(['/', '\\']) { Some(i) => &path[i + 1..], None => path };
    match filename.rfind('.') {
        Some(i) if i + 1 < filename.len() => Some(filename[i..].to_lowercase()),
        _ => None,
    }
}

fn pearson(pairs: &[(f64, f64)]) -> Option<f64> {
    let n = pairs.len();
    if n < 8 { return None; }
    let nf = n as f64;
    let mx: f64 = pairs.iter().map(|(x, _)| x).sum::<f64>() / nf;
    let my: f64 = pairs.iter().map(|(_, y)| y).sum::<f64>() / nf;
    let (mut sxy, mut sxx, mut syy) = (0.0, 0.0, 0.0);
    for (x, y) in pairs {
        sxy += (x - mx) * (y - my);
        sxx += (x - mx).powi(2);
        syy += (y - my).powi(2);
    }
    let d = (sxx * syy).sqrt();
    if d == 0.0 { None } else { Some(round2(sxy / d)) }
}

// `const shape = (s) => ({...})` — reshapes one review_scores row for the `worst` list.
fn shape(s: &Value, viol_by_path: &std::collections::HashMap<String, Vec<Value>>) -> Value {
    let mut m = Map::new();
    m.insert("kind".into(), s.get("unit_kind").cloned().unwrap_or(Value::Null));
    m.insert("path".into(), s.get("path").cloned().unwrap_or(Value::Null));
    m.insert("score".into(), s.get("score").cloned().unwrap_or(Value::Null));
    m.insert("health".into(), s.get("health").cloned().unwrap_or(Value::Null));
    m.insert("archetype".into(), s.get("archetype").cloned().unwrap_or(Value::Null));
    let risk0 = nz(s.get("risk")).cloned().unwrap_or(json!(0));
    m.insert("risk".into(), num_out(round2(js_number(Some(&risk0)))));
    m.insert("fan_in".into(), s.get("fan_in").cloned().unwrap_or(Value::Null));
    m.insert("fan_out".into(), s.get("fan_out").cloned().unwrap_or(Value::Null));
    m.insert("loc".into(), s.get("loc").cloned().unwrap_or(Value::Null));
    m.insert("complexity".into(), s.get("complexity").cloned().unwrap_or(Value::Null));
    m.insert("transitive".into(), s.get("transitive").cloned().unwrap_or(Value::Null));
    m.insert("covered".into(), s.get("covered").cloned().unwrap_or(Value::Null));
    m.insert("churn".into(), s.get("churn").cloned().unwrap_or(Value::Null));
    m.insert("authors".into(), s.get("authors").cloned().unwrap_or(Value::Null));
    m.insert("findings".into(), s.get("findings").cloned().unwrap_or(Value::Null));
    m.insert("ai_readability".into(), s.get("ai_readability").cloned().unwrap_or(Value::Null));
    m.insert("pct_repo".into(), s.get("pct_repo").cloned().unwrap_or(Value::Null));
    m.insert("pct_folder".into(), s.get("pct_folder").cloned().unwrap_or(Value::Null));
    m.insert("axes".into(), safe(s.get("axes_json")));
    m.insert("cached".into(), json!(truthy(s.get("cached"))));
    m.insert("breakdown".into(), safe(s.get("breakdown_json")));
    let path = s.get("path").and_then(Value::as_str).unwrap_or("").to_string();
    let violations: Vec<Value> = viol_by_path.get(&path).map(|v| v.iter().take(12).cloned().collect()).unwrap_or_default();
    m.insert("violations".into(), json!(violations));
    Value::Object(m)
}

pub fn constitutionReport(input: &Value, db: &Connection) -> Result<Value, String> {
    let _ = crate::constitution__helpers_schema::ensureConstitutionSchema(db);

    let snap: Option<Value> = if nz(input.get("snapshot_id")).is_some() {
        let id = js_number(input.get("snapshot_id"));
        crate::db_json::query_rows(db, "SELECT * FROM judgments.review_snapshots WHERE id=?", &[json!(id)])?
            .as_array().and_then(|a| a.first()).cloned()
    } else {
        crate::db_json::query_rows(db, "SELECT * FROM judgments.review_snapshots WHERE status='done' ORDER BY id DESC LIMIT 1", &[])?
            .as_array().and_then(|a| a.first()).cloned()
    };

    let snapshots: Vec<Value> = crate::db_json::query_rows(
        db,
        "SELECT id, created_at, source, files, judged, duration_ms, scope_json FROM judgments.review_snapshots ORDER BY id DESC LIMIT 15",
        &[],
    )?
    .as_array()
    .cloned()
    .unwrap_or_default()
    .into_iter()
    .map(|r| {
        let mut m = r.as_object().cloned().unwrap_or_default();
        let scope = safe(m.get("scope_json"));
        m.insert("scope".to_string(), scope);
        Value::Object(m)
    })
    .collect();

    let composed = crate::constitution_constitution_law::composedConstitution(db);
    let laws: Vec<Value> = composed.get("laws").and_then(Value::as_array).cloned().unwrap_or_default();
    let exceptions: Value = composed.get("exceptions").cloned().unwrap_or_else(|| json!([]));

    let constitution_laws: Vec<Value> = laws
        .iter()
        .map(|l| {
            let mut m = l.as_object().cloned().unwrap_or_default();
            m.insert("category".into(), crate::constitution__helpers_categories::categoryOf(l));
            m.insert("applies".into(), safe(l.get("applies_json")));
            m.insert("config".into(), safe(l.get("config_json")));
            m.insert("evidence".into(), safe(l.get("evidence_json")));
            Value::Object(m)
        })
        .collect();

    let mut counts_map = Map::new();
    for l in &laws {
        let status = l.get("status").and_then(Value::as_str).unwrap_or("").to_string();
        let n = counts_map.get(&status).and_then(Value::as_i64).unwrap_or(0) + 1;
        counts_map.insert(status, json!(n));
    }

    let mut constitution_map = Map::new();
    constitution_map.insert("laws".into(), json!(constitution_laws));
    constitution_map.insert("exceptions".into(), exceptions);
    constitution_map.insert("counts".into(), Value::Object(counts_map.clone()));
    let constitution_v = Value::Object(constitution_map);

    let snap = match snap {
        Some(s) => s,
        None => {
            let mut out = Map::new();
            out.insert("snapshot".into(), Value::Null);
            out.insert("snapshots".into(), json!(snapshots));
            out.insert("summary".into(), Value::Null);
            out.insert("by_law".into(), json!([]));
            out.insert("worst".into(), json!([]));
            out.insert("constitution".into(), constitution_v);
            out.insert("deltas".into(), Value::Null);
            out.insert("insights".into(), json!({}));
            return Ok(Value::Object(out));
        }
    };
    let snap_id = snap.get("id").cloned().unwrap_or(Value::Null);

    let totals_raw = safe(snap.get("totals_json"));
    let totals = if totals_raw.is_null() { json!({}) } else { totals_raw };

    let insight_rows: Vec<Value> = crate::db_json::query_rows(
        db,
        "SELECT path, body_json, model, attempts, cost_usd FROM judgments.review_insights WHERE snapshot_id=?",
        &[snap_id.clone()],
    )?
    .as_array()
    .cloned()
    .unwrap_or_default();
    let mut insights_map = Map::new();
    let mut ai_cost: f64 = 0.0;
    let mut brief: Value = Value::Null;
    for r in &insight_rows {
        let cost_v = nz(r.get("cost_usd")).cloned().unwrap_or(json!(0));
        ai_cost += js_number(Some(&cost_v));
        let b = safe(r.get("body_json"));
        if !truthy(Some(&b)) { continue; }
        let path = r.get("path").and_then(Value::as_str).unwrap_or("").to_string();
        let model = r.get("model").cloned().unwrap_or(Value::Null);
        if path == "__brief__" {
            let mut m = b.as_object().cloned().unwrap_or_default();
            m.insert("model".into(), model);
            brief = Value::Object(m);
        } else {
            let mut m = b.as_object().cloned().unwrap_or_default();
            m.insert("model".into(), model);
            m.insert("attempts".into(), r.get("attempts").cloned().unwrap_or(Value::Null));
            insights_map.insert(path, Value::Object(m));
        }
    }

    let scores: Vec<Value> = crate::db_json::query_rows(db, "SELECT * FROM judgments.review_scores WHERE snapshot_id=? ORDER BY risk DESC", &[snap_id.clone()])?
        .as_array()
        .cloned()
        .unwrap_or_default();
    let files: Vec<Value> = scores.iter().filter(|s| s.get("unit_kind").and_then(Value::as_str) == Some("file")).cloned().collect();
    let bindings: Vec<Value> = scores.iter().filter(|s| s.get("unit_kind").and_then(Value::as_str) == Some("binding")).cloned().collect();

    let viol_rows: Vec<Value> = crate::db_json::query_rows(
        db,
        "SELECT path, symbol, line, law_key, law_scope, weight, severity, confidence, evidence FROM judgments.review_violations WHERE snapshot_id=?",
        &[snap_id.clone()],
    )?
    .as_array()
    .cloned()
    .unwrap_or_default();
    let mut viol_by_path: std::collections::HashMap<String, Vec<Value>> = std::collections::HashMap::new();
    for v in viol_rows {
        let path = v.get("path").and_then(Value::as_str).unwrap_or("").to_string();
        viol_by_path.entry(path).or_default().push(v);
    }

    let compliances: Vec<f64> = files.iter().filter_map(|f| nz(f.get("score")).map(|v| js_number(Some(v)))).collect();
    let compliance_v: Value = if !compliances.is_empty() {
        let avg = compliances.iter().sum::<f64>() / compliances.len() as f64;
        json!(js_round(avg) as i64)
    } else {
        Value::Null
    };
    let carried = files.iter().filter(|f| truthy(f.get("cached"))).count() as i64;
    let laws_scoring = laws
        .iter()
        .filter(|l| l.get("status").and_then(Value::as_str) == Some("ratified") && l.get("enforcement").and_then(Value::as_str) == Some("score"))
        .count() as i64;
    let candidates_v = nz(counts_map.get("candidate")).cloned().unwrap_or(json!(0));

    let mut summary_map = Map::new();
    summary_map.insert("snapshot_id".into(), snap.get("id").cloned().unwrap_or(Value::Null));
    summary_map.insert("created_at".into(), snap.get("created_at").cloned().unwrap_or(Value::Null));
    summary_map.insert("constitution_hash".into(), snap.get("constitution_hash").cloned().unwrap_or(Value::Null));
    summary_map.insert("health".into(), nz(totals.get("health")).cloned().unwrap_or(Value::Null));
    summary_map.insert("compliance".into(), compliance_v);
    summary_map.insert("determinism".into(), nz(totals.get("determinism")).cloned().unwrap_or(Value::Null));
    summary_map.insert("determinism_parts".into(), nz(totals.get("determinism_parts")).cloned().unwrap_or(Value::Null));
    summary_map.insert("ai_readability".into(), nz(totals.get("ai_readability")).cloned().unwrap_or(Value::Null));
    summary_map.insert("axes_avg".into(), nz(totals.get("axes_avg")).cloned().unwrap_or(json!({})));
    summary_map.insert("knowledge".into(), nz(totals.get("knowledge")).cloned().unwrap_or(Value::Null));
    summary_map.insert("co_change".into(), nz(totals.get("co_change")).cloned().unwrap_or(json!([])));
    summary_map.insert("files".into(), json!(files.len() as i64));
    summary_map.insert("bindings".into(), json!(bindings.len() as i64));
    summary_map.insert("violations".into(), nz(totals.get("violations")).cloned().unwrap_or(json!(0)));
    summary_map.insert("judged".into(), snap.get("judged").cloned().unwrap_or(Value::Null));
    summary_map.insert("carried".into(), json!(carried));
    summary_map.insert("duration_ms".into(), snap.get("duration_ms").cloned().unwrap_or(Value::Null));
    summary_map.insert("cost_usd".into(), num_out(to_fixed_num(ai_cost, 4)));
    summary_map.insert("laws_scoring".into(), json!(laws_scoring));
    summary_map.insert("candidates".into(), candidates_v);
    summary_map.insert(
        "provenance".into(),
        json!("constitution laws + the engineering model (atlas metrics \u{b7} smells \u{b7} import closure \u{b7} git history \u{b7} bench ledger) \u{2014} deterministic, $0"),
    );

    // by_law
    let by_law_src: Vec<Value> = nz(totals.get("by_law")).and_then(Value::as_array).cloned().unwrap_or_default();
    let by_law: Vec<Value> = by_law_src
        .iter()
        .map(|r| {
            let law_key = r.get("law_key").cloned().unwrap_or(Value::Null);
            let law_scope = r.get("law_scope").cloned().unwrap_or(Value::Null);
            let law = laws.iter().find(|l| l.get("key").map(|k| k == &law_key).unwrap_or(false));
            let cat_input = match law {
                Some(l) => l.clone(),
                None => json!({ "key": law_key.clone(), "scope": law_scope.clone() }),
            };
            let category = crate::constitution__helpers_categories::categoryOf(&cat_input);
            let title = law.and_then(|l| nz(l.get("title")).cloned()).unwrap_or_else(|| law_key.clone());
            let weight = law.and_then(|l| nz(l.get("weight")).cloned()).unwrap_or(Value::Null);
            json!({
                "law_key": law_key, "scope": law_scope, "category": category,
                "n": r.get("n").cloned().unwrap_or(Value::Null), "mass": r.get("mass").cloned().unwrap_or(Value::Null),
                "title": title, "weight": weight,
            })
        })
        .collect();

    // by_category — a COMPLIANCE view decomposed per concern; only categories with an active
    // (ratified, score-enforced) law get an entry (Object.keys(activeByCat) drives the list).
    let mut active_by_cat = Map::new(); // preserves insertion order, matching JS object key order
    for l in &laws {
        if l.get("status").and_then(Value::as_str) == Some("ratified") && l.get("enforcement").and_then(Value::as_str) == Some("score") {
            let c = crate::constitution__helpers_categories::categoryOf(l);
            let c_s = c.as_str().unwrap_or("").to_string();
            let n = active_by_cat.get(&c_s).and_then(Value::as_i64).unwrap_or(0) + 1;
            active_by_cat.insert(c_s, json!(n));
        }
    }
    let mut cat_agg: std::collections::HashMap<String, (i64, f64)> = std::collections::HashMap::new();
    for r in &by_law {
        let cat = r.get("category").and_then(Value::as_str).unwrap_or("").to_string();
        let n = r.get("n").and_then(Value::as_i64).unwrap_or(0);
        let mass = r.get("mass").and_then(Value::as_f64).unwrap_or(0.0);
        let e = cat_agg.entry(cat).or_insert((0, 0.0));
        e.0 += n;
        e.1 += mass;
    }
    let file_count_for_cat = (files.len() as f64).max(1.0);
    let mut by_category: Vec<Value> = active_by_cat
        .iter()
        .map(|(c, active_laws)| {
            let (violations, mass) = cat_agg.get(c).cloned().unwrap_or((0, 0.0));
            let score = (100.0 - (js_round(mass / file_count_for_cat)).min(90.0)).max(10.0);
            json!({ "category": c, "active_laws": active_laws, "violations": violations, "mass": num_out(mass), "score": num_out(score) })
        })
        .collect();
    by_category.sort_by(|x, y| {
        let xs = x.get("score").and_then(Value::as_f64).unwrap_or(0.0);
        let ys = y.get("score").and_then(Value::as_f64).unwrap_or(0.0);
        xs.partial_cmp(&ys).unwrap_or(std::cmp::Ordering::Equal).then_with(|| {
            let xv = x.get("violations").and_then(Value::as_i64).unwrap_or(0);
            let yv = y.get("violations").and_then(Value::as_i64).unwrap_or(0);
            yv.cmp(&xv)
        })
    });

    // worst — by RISK: bad if it breaks laws OR is structurally weak (health/score < 100).
    let mut worst_src: Vec<Value> = Vec::new();
    for s in &scores {
        let base = nz(s.get("health")).or_else(|| nz(s.get("score"))).cloned().unwrap_or(json!(100));
        let health_or_score = js_number(Some(&base));
        let path = s.get("path").and_then(Value::as_str).unwrap_or("").to_string();
        let vio_len = viol_by_path.get(&path).map(|v| v.len()).unwrap_or(0);
        if health_or_score < 100.0 || vio_len > 0 { worst_src.push(s.clone()); }
    }
    let worst: Vec<Value> = worst_src.into_iter().take(WORST_CAP).map(|s| shape(&s, &viol_by_path)).collect();

    // deltas vs the previous done snapshot of the same scope
    let prev = crate::db_json::query_rows(
        db,
        "SELECT id, totals_json FROM judgments.review_snapshots WHERE id<? AND status='done' AND scope_json=? ORDER BY id DESC LIMIT 1",
        &[snap_id.clone(), snap.get("scope_json").cloned().unwrap_or(Value::Null)],
    )?
    .as_array()
    .and_then(|a| a.first())
    .cloned();
    let deltas: Value = match prev {
        Some(prev_row) => {
            let prev_id = prev_row.get("id").cloned().unwrap_or(Value::Null);
            let prev_scores_rows: Vec<Value> = crate::db_json::query_rows(
                db,
                "SELECT path, score FROM judgments.review_scores WHERE snapshot_id=? AND unit_kind='file'",
                &[prev_id.clone()],
            )?
            .as_array()
            .cloned()
            .unwrap_or_default();
            let mut prev_scores: std::collections::HashMap<String, Value> = std::collections::HashMap::new();
            for r in &prev_scores_rows {
                let path = r.get("path").and_then(Value::as_str).unwrap_or("").to_string();
                prev_scores.insert(path, r.get("score").cloned().unwrap_or(Value::Null));
            }
            let mut changed: Vec<Value> = Vec::new();
            for f in &files {
                let path = f.get("path").and_then(Value::as_str).unwrap_or("").to_string();
                let p = prev_scores.get(&path).cloned();
                let f_score = f.get("score").cloned().unwrap_or(Value::Null);
                let p_is_null = matches!(p, None | Some(Value::Null));
                let f_is_null = matches!(f_score, Value::Null);
                if p_is_null || f_is_null { continue; }
                let p_val = p.unwrap();
                if p_val == f_score { continue; }
                let delta = js_number(Some(&f_score)) - js_number(Some(&p_val));
                changed.push(json!({ "path": path, "from": p_val, "to": f_score, "delta": num_out(delta) }));
            }
            changed.sort_by(|a, b| {
                let ad = a.get("delta").and_then(Value::as_f64).unwrap_or(0.0);
                let bd = b.get("delta").and_then(Value::as_f64).unwrap_or(0.0);
                ad.partial_cmp(&bd).unwrap_or(std::cmp::Ordering::Equal)
            });
            let prev_totals_raw = safe(prev_row.get("totals_json"));
            let prev_totals = if prev_totals_raw.is_null() { json!({}) } else { prev_totals_raw };
            let health_delta = match (nz(prev_totals.get("health")), nz(summary_map.get("health"))) {
                (Some(pv), Some(sv)) => num_out(js_number(Some(sv)) - js_number(Some(pv))),
                _ => Value::Null,
            };
            let worse: Vec<Value> = changed.iter().filter(|c| c.get("delta").and_then(Value::as_f64).unwrap_or(0.0) < 0.0).take(10).cloned().collect();
            let better_all: Vec<Value> = changed.iter().filter(|c| c.get("delta").and_then(Value::as_f64).unwrap_or(0.0) > 0.0).cloned().collect();
            let start = better_all.len().saturating_sub(10);
            let mut better: Vec<Value> = better_all[start..].to_vec();
            better.reverse();
            json!({ "from_snapshot": prev_id, "health": health_delta, "worse": worse, "better": better })
        }
        None => Value::Null,
    };

    // cross-analysis — honest correlations, n always shown.
    let cohort_f: Vec<&Value> = files.iter().filter(|f| nz(f.get("health")).is_some()).collect();
    let vio_count = |path: &str| -> f64 { viol_by_path.get(path).map(|v| v.len()).unwrap_or(0) as f64 };
    let mut cross: Vec<Value> = Vec::new();
    let mut push_corr = |cross: &mut Vec<Value>, name: &str, pairs: Vec<(f64, f64)>, note: &str| {
        let n = pairs.len() as i64;
        if let Some(r) = pearson(&pairs) { cross.push(json!({ "name": name, "r": num_out(r), "n": n, "note": note })); }
    };
    let pairs1: Vec<(f64, f64)> = cohort_f
        .iter()
        .filter_map(|f| {
            let x = nz(f.get("complexity")).map(|v| js_number(Some(v)));
            let path = f.get("path").and_then(Value::as_str).unwrap_or("");
            x.map(|xv| (xv, vio_count(path)))
        })
        .collect();
    push_corr(&mut cross, "complexity \u{2194} law violations", pairs1, "do complex files break more rules?");
    let pairs2: Vec<(f64, f64)> = cohort_f
        .iter()
        .filter_map(|f| {
            let x = nz(f.get("fan_in")).map(|v| js_number(Some(v)));
            let y = nz(f.get("covered")).map(|v| js_number(Some(v)));
            match (x, y) { (Some(x), Some(y)) => Some((x, y)), _ => None }
        })
        .collect();
    push_corr(&mut cross, "fan-in \u{2194} coverage", pairs2, "are the most depended-on files tested?");
    let pairs3: Vec<(f64, f64)> = cohort_f
        .iter()
        .filter_map(|f| {
            let x = nz(f.get("churn")).map(|v| js_number(Some(v)));
            let y = nz(f.get("complexity")).map(|v| js_number(Some(v)));
            match (x, y) { (Some(x), Some(y)) => Some((x, y)), _ => None }
        })
        .collect();
    push_corr(&mut cross, "churn \u{2194} complexity", pairs3, "do the hotspots overlap the complex files?");

    // duplication — Jaccard over atlas symbol names, deterministic, $0.
    let duplication: Vec<Value> = (|| -> Option<Vec<Value>> {
        let rows: Vec<Value> = crate::db_json::query_rows(
            db,
            "SELECT path, name FROM code_nodes WHERE origin='atlas' AND type IN ('function','class','method','component','hook','const') AND name IS NOT NULL",
            &[],
        )
        .ok()?
        .as_array()?
        .clone();
        let mut order: Vec<String> = Vec::new();
        let mut sets: std::collections::HashMap<String, std::collections::HashSet<String>> = std::collections::HashMap::new();
        for r in &rows {
            let raw_path = js_string(r.get("path").unwrap_or(&Value::Null));
            let p = raw_path.replace('\\', "/");
            let name = js_string(r.get("name").unwrap_or(&Value::Null));
            if !sets.contains_key(&p) { order.push(p.clone()); sets.insert(p.clone(), std::collections::HashSet::new()); }
            sets.get_mut(&p).unwrap().insert(name);
        }
        let file_paths: std::collections::HashSet<&str> = files.iter().filter_map(|f| f.get("path").and_then(Value::as_str)).collect();
        let mut entries: Vec<(String, std::collections::HashSet<String>)> = order
            .into_iter()
            .filter_map(|p| sets.remove(&p).map(|s| (p, s)))
            .filter(|(p, s)| s.len() >= 4 && file_paths.contains(p.as_str()))
            .collect();
        if entries.len() > 400 { entries.truncate(400); }
        let mut dup: Vec<Value> = Vec::new();
        for i in 0..entries.len() {
            for j in (i + 1)..entries.len() {
                let (pa, sa) = &entries[i];
                let (pb, sb) = &entries[j];
                let inter = sa.intersection(sb).count() as i64;
                let union = sa.len() as i64 + sb.len() as i64 - inter;
                let jac = if union == 0 { 0.0 } else { inter as f64 / union as f64 };
                if jac >= 0.6 && inter >= 3 {
                    dup.push(json!({ "a": pa, "b": pb, "shared": inter, "jaccard": num_out(round2(jac)) }));
                }
            }
        }
        dup.sort_by(|x, y| {
            let yj = y.get("jaccard").and_then(Value::as_f64).unwrap_or(0.0);
            let xj = x.get("jaccard").and_then(Value::as_f64).unwrap_or(0.0);
            yj.partial_cmp(&xj).unwrap_or(std::cmp::Ordering::Equal)
        });
        dup.truncate(10);
        Some(dup)
    })()
    .unwrap_or_default();

    // linter runners — which of the project's own linters are relevant + wired into the judge.
    let linters: Vec<Value> = (|| -> Option<Vec<Value>> {
        let rows: Vec<Value> = crate::db_json::query_rows(db, "SELECT path FROM code_nodes WHERE origin='atlas' AND type='file'", &[]).ok()?.as_array()?.clone();
        let mut counts: std::collections::HashMap<String, i64> = std::collections::HashMap::new();
        for r in &rows {
            let path = js_string(r.get("path").unwrap_or(&Value::Null));
            if let Some(ext) = ext_of(&path) { *counts.entry(ext).or_insert(0) += 1; }
        }
        let lint_law = laws.iter().find(|l| l.get("key").and_then(Value::as_str) == Some("lint-clean"));
        let active = lint_law.map(|l| l.get("status").and_then(Value::as_str) == Some("ratified")).unwrap_or(false);
        let mut out = Vec::new();
        for l in LINTERS.iter() {
            let file_count: i64 = l.exts.iter().map(|e| *counts.get(*e).unwrap_or(&0)).sum();
            if file_count <= 0 { continue; }
            let installed = if l.via == "npm" { crate::codebase_lint_detect::npmInstalled(&json!(l.id)) } else { Value::Null };
            out.push(json!({ "id": l.id, "label": l.label, "via": l.via, "file_count": file_count, "installed": installed, "active": active }));
        }
        Some(out)
    })()
    .unwrap_or_default();

    let manifest_v = safe(snap.get("manifest_json"));
    let manifest_count = if manifest_v.is_null() { 0i64 } else { manifest_v.as_array().map(|a| a.len() as i64).unwrap_or(0) };
    let snapshot_v = json!({
        "id": snap.get("id").cloned().unwrap_or(Value::Null),
        "created_at": snap.get("created_at").cloned().unwrap_or(Value::Null),
        "source": snap.get("source").cloned().unwrap_or(Value::Null),
        "scope": safe(snap.get("scope_json")),
        "constitution_hash": snap.get("constitution_hash").cloned().unwrap_or(Value::Null),
        "manifest_count": manifest_count,
    });

    let mut out = Map::new();
    out.insert("snapshot".into(), snapshot_v);
    out.insert("snapshots".into(), json!(snapshots));
    out.insert("summary".into(), Value::Object(summary_map));
    out.insert("by_law".into(), json!(by_law));
    out.insert("by_category".into(), json!(by_category));
    out.insert("worst".into(), json!(worst));
    out.insert("constitution".into(), constitution_v);
    out.insert("deltas".into(), deltas);
    out.insert("insights".into(), Value::Object(insights_map));
    out.insert("brief".into(), brief);
    out.insert("cross".into(), json!(cross));
    out.insert("duplication".into(), json!(duplication));
    out.insert("linters".into(), json!(linters));
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conn() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS laws; ATTACH ':memory:' AS judgments").unwrap();
        c
    }

    #[test]
    fn no_snapshot_returns_the_shallow_shape() {
        let c = conn();
        let r = constitutionReport(&json!({}), &c).unwrap();
        assert_eq!(r["snapshot"], Value::Null);
        assert_eq!(r["summary"], Value::Null);
        assert_eq!(r["by_law"], json!([]));
        assert_eq!(r["worst"], json!([]));
        assert_eq!(r["deltas"], Value::Null);
        assert_eq!(r["insights"], json!({}));
        assert!(r.get("by_category").is_none()); // early-return branch omits these keys
        assert!(r.get("brief").is_none());
        assert!(r.get("cross").is_none());
        let keys: Vec<&String> = r.as_object().unwrap().keys().collect();
        assert_eq!(keys, ["snapshot", "snapshots", "summary", "by_law", "worst", "constitution", "deltas", "insights"]);
    }

    #[test]
    fn full_snapshot_aggregates_summary_worst_and_deltas() {
        let c = conn();
        let _ = crate::constitution__helpers_schema::ensureConstitutionSchema(&c);
        c.execute("INSERT INTO laws.review_laws (scope, key, title, rule_md, status, enforcement, weight) VALUES ('project','no-var','No var','r','ratified','score',5)", []).unwrap();
        c.execute(
            "INSERT INTO judgments.review_snapshots (id, created_at, source, scope_json, manifest_json, constitution_hash, status, judged, duration_ms, totals_json) VALUES \
             (1, '2025-12-20 00:00:00', 'wizard', '{\"all\":true}', '[\"a.js\",\"b.js\"]', 'h1', 'done', 2, 400, '{\"health\":92}')",
            [],
        )
        .unwrap();
        c.execute(
            "INSERT INTO judgments.review_snapshots (id, created_at, source, scope_json, manifest_json, constitution_hash, status, judged, duration_ms, totals_json) VALUES \
             (2, '2026-01-01 00:00:00', 'wizard', '{\"all\":true}', '[\"a.js\",\"b.js\"]', 'h2', 'done', 2, 500, \
              '{\"health\":80,\"determinism\":90,\"violations\":1,\"by_law\":[{\"law_key\":\"no-var\",\"law_scope\":\"project\",\"n\":1,\"mass\":5}]}')",
            [],
        )
        .unwrap();
        c.execute("INSERT INTO judgments.review_scores (snapshot_id, unit_kind, path, score) VALUES (1,'file','a.js',80)", []).unwrap();
        c.execute("INSERT INTO judgments.review_scores (snapshot_id, unit_kind, path, score, health, risk, cached) VALUES (2,'file','a.js',60,60,1.2,0)", []).unwrap();
        c.execute("INSERT INTO judgments.review_scores (snapshot_id, unit_kind, path, score, health, risk, cached) VALUES (2,'file','b.js',100,100,0,1)", []).unwrap();
        c.execute("INSERT INTO judgments.review_violations (snapshot_id, path, law_key, law_scope, weight) VALUES (2,'a.js','no-var','project',5)", []).unwrap();
        c.execute("INSERT INTO judgments.review_insights (snapshot_id, path, body_json, model, attempts, cost_usd) VALUES (2,'a.js','{\"insight\":\"x\"}','gpt',1,0.01)", []).unwrap();
        c.execute("INSERT INTO judgments.review_insights (snapshot_id, path, body_json, model, attempts, cost_usd) VALUES (2,'__brief__','{\"summary\":\"y\"}','gpt',1,0.02)", []).unwrap();

        let r = constitutionReport(&json!({}), &c).unwrap();
        assert_eq!(r["snapshot"]["id"], json!(2));
        assert_eq!(r["snapshot"]["manifest_count"], json!(2));
        assert_eq!(r["summary"]["health"], json!(80));
        assert_eq!(r["summary"]["compliance"], json!(80)); // round((60+100)/2)
        assert_eq!(r["summary"]["files"], json!(2));
        assert_eq!(r["summary"]["carried"], json!(1));
        assert_eq!(r["summary"]["cost_usd"], json!(0.03));
        assert_eq!(r["insights"]["a.js"]["model"], json!("gpt"));
        assert_eq!(r["brief"]["summary"], json!("y"));
        assert_eq!(r["by_law"][0]["title"], json!("No var"));
        assert_eq!(r["by_law"][0]["category"], json!("project")); // scope='project' wins before the correctness regex
        let worst = r["worst"].as_array().unwrap();
        assert_eq!(worst.len(), 1); // only a.js (health<100 or has a violation)
        assert_eq!(worst[0]["path"], json!("a.js"));
        assert_eq!(worst[0]["risk"], json!(1.2));
        assert_eq!(r["deltas"]["from_snapshot"], json!(1));
        assert_eq!(r["deltas"]["health"], json!(-12)); // 80 - 92
        assert_eq!(r["deltas"]["worse"][0]["delta"], json!(-20)); // a.js: 60 - 80
    }

    #[test]
    fn explicit_snapshot_id_selects_that_row() {
        let c = conn();
        let _ = crate::constitution__helpers_schema::ensureConstitutionSchema(&c);
        c.execute(
            "INSERT INTO judgments.review_snapshots (id, scope_json, manifest_json, constitution_hash, status) VALUES (5, '{}', '[]', 'h5', 'running')",
            [],
        )
        .unwrap();
        let r = constitutionReport(&json!({ "snapshot_id": 5 }), &c).unwrap();
        assert_eq!(r["snapshot"]["id"], json!(5));
    }
}
