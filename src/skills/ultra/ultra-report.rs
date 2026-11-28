//! ultra-report.rs — twin of skills/ultra/ultra-report.mjs (db READ lane). NAMES ARE CONTRACT:
//! `ultraReport` (default). ULTRA REVIEW's workspace payload: one survey run's scored territory
//! — summary + meta scores, folder roll-up (fan-in weighted), worst/units rankings, the concept
//! atlas (ideas + co-occurrence edges + scattered/untested/mixed smells), and run-over-run deltas.
//! Pure reads over judgments.ultra_runs/ultra_scores/ultra_insights/ultra_concepts — the formulas
//! already ran at survey time; this only aggregates and ranks.
// QUALITY: 80/100 — faithful port of a large read-only aggregator; Map-insertion-order semantics
// (dossiers/byConcept/byPathConcepts/pairCount) reproduced via serde_json::Map + first-seen Vecs.
use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::collections::{HashMap, HashSet};

const UNIT_CAP: usize = 600;
const WORST_CAP: usize = 50;
const CONCEPT_CAP: usize = 60;

fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}
fn tmpl(v: Option<&Value>) -> String {
    match v {
        None => "undefined".into(),
        Some(Value::Null) => "null".into(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(),
        Some(o) => o.to_string(),
    }
}
fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) } // JS `??` right operand: nz(a).or(nz(b))
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

// JS Number stringification: integral f64 -> integer JSON number, else float (gotcha #2).
fn num_val(x: f64) -> Value {
    if x.is_finite() && x.fract() == 0.0 && x.abs() < 9.007_199_254_740_992e15 { json!(x as i64) } else { json!(x) }
}
fn to_fixed4(x: f64) -> f64 { format!("{:.4}", x).parse::<f64>().unwrap_or(x) }

// mean(arr) = arr.length ? Math.round(sum/len) : null
fn mean(arr: &[f64]) -> Value {
    if arr.is_empty() { Value::Null } else { json!(crate::js_math::js_round(arr.iter().sum::<f64>() / arr.len() as f64) as i64) }
}

// [...new Set(vals)] — dedupe preserving first-occurrence order (vals are always TEXT-column strings here).
fn dedupe_str(vals: &[Value]) -> Vec<Value> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut out = Vec::new();
    for v in vals {
        let s = v.as_str().unwrap_or("").to_string();
        if seen.insert(s) { out.push(v.clone()); }
    }
    out
}

fn get_one(db: &Connection, sql: &str, params: &[Value]) -> Result<Option<Value>, String> {
    let rows = crate::db_json::query_rows(db, sql, params)?;
    Ok(rows.as_array().and_then(|a| a.first()).cloned())
}

// r => ({ ...r, smells: JSON.parse(r.smells_json ?? '[]'), axes: JSON.parse(r.axes_json ?? '{}') })
fn with_smells_axes(rows: &[Value]) -> Vec<Value> {
    rows.iter()
        .map(|r| {
            let mut o = r.as_object().cloned().unwrap_or_default();
            let smells_json = nz(o.get("smells_json")).and_then(|v| v.as_str()).unwrap_or("[]").to_string();
            let axes_json = nz(o.get("axes_json")).and_then(|v| v.as_str()).unwrap_or("{}").to_string();
            o.insert("smells".into(), serde_json::from_str(&smells_json).unwrap_or(json!([])));
            o.insert("axes".into(), serde_json::from_str(&axes_json).unwrap_or(json!({})));
            Value::Object(o)
        })
        .collect()
}

fn unit_shape(r: &Value) -> Value {
    let g = |k: &str| r.get(k).cloned().unwrap_or(Value::Null);
    let mut o = Map::new();
    o.insert("path".into(), g("path"));
    o.insert("folder".into(), g("folder"));
    o.insert("health".into(), g("health"));
    o.insert("ai_readability".into(), g("ai_readability"));
    o.insert("content_hash".into(), g("content_hash"));
    o.insert("atlas_known".into(), json!(truthy(r.get("atlas_known"))));
    o.insert("is_test".into(), json!(truthy(r.get("is_test"))));
    o.insert("loc".into(), g("loc"));
    o.insert("fan_in".into(), g("fan_in"));
    o.insert("fan_out".into(), g("fan_out"));
    o.insert("complexity".into(), g("complexity"));
    o.insert("transitive".into(), g("transitive"));
    o.insert("covered".into(), g("covered"));
    o.insert("churn".into(), g("churn"));
    o.insert("authors".into(), g("authors"));
    o.insert("findings".into(), g("findings"));
    o.insert("findings_high".into(), g("findings_high"));
    o.insert("smells".into(), g("smells"));
    o.insert("axes".into(), g("axes"));
    o.insert("pct_repo".into(), g("pct_repo"));
    o.insert("pct_folder".into(), g("pct_folder"));
    Value::Object(o)
}

pub fn ultraReport(input: &Value, db: &Connection) -> Result<Value, String> {
    crate::ultra__helpers_schema::ensureUltraSchema(db);

    let run_id_arg = input.get("run_id").filter(|v| !v.is_null());
    let run = match run_id_arg {
        Some(v) => get_one(db, "SELECT * FROM judgments.ultra_runs WHERE id = ?", &[json!(js_number(Some(v)))])?,
        None => get_one(db, "SELECT * FROM judgments.ultra_runs WHERE status='done' ORDER BY id DESC LIMIT 1", &[])?,
    };

    let runs_rows = crate::db_json::query_rows(db, "SELECT id, created_at, source, files, status, scope_json FROM judgments.ultra_runs ORDER BY id DESC LIMIT 15", &[])?;
    let runs: Vec<Value> = runs_rows.as_array().cloned().unwrap_or_default().iter().map(|r| {
        let mut o = r.as_object().cloned().unwrap_or_default();
        let scope_json = nz(o.get("scope_json")).and_then(|v| v.as_str()).unwrap_or("{}").to_string();
        o.insert("scope".into(), serde_json::from_str(&scope_json).unwrap_or(json!({})));
        Value::Object(o)
    }).collect();

    let run = match run {
        Some(r) => r,
        None => {
            return Ok(json!({
                "run": Value::Null, "runs": runs, "summary": Value::Null,
                "folders": Value::Array(vec![]), "worst": Value::Array(vec![]), "units": Value::Array(vec![])
            }));
        }
    };
    let run_id = run.get("id").cloned().unwrap_or(Value::Null);

    let rows_raw = crate::db_json::query_rows(db, "SELECT * FROM judgments.ultra_scores WHERE run_id = ? ORDER BY path", &[run_id.clone()])?;
    let rows: Vec<Value> = with_smells_axes(rows_raw.as_array().cloned().unwrap_or_default().as_slice());
    let cohort: Vec<Value> = rows.iter().filter(|r| !truthy(r.get("is_test"))).cloned().collect();

    // ── summary + meta scores ──────────────────────────────────────────────
    let healths: Vec<f64> = cohort.iter().filter_map(|r| r.get("health").filter(|v| !v.is_null()).and_then(|v| v.as_f64())).collect();
    let axis_names = ["structure", "coupling", "tests", "defects", "change", "knowledge"];
    let mut axes_avg = Map::new();
    for ax in axis_names {
        let vals: Vec<f64> = cohort.iter().filter_map(|r| {
            r.get("axes").and_then(|a| a.get(ax)).and_then(|o| o.get("score")).filter(|v| !v.is_null()).and_then(|v| v.as_f64())
        }).collect();
        axes_avg.insert(ax.into(), mean(&vals));
    }
    let known = cohort.iter().filter(|r| truthy(r.get("atlas_known"))).count();

    // deep-tier artifacts: consultant dossiers + the brief.
    let insight_rows_raw = crate::db_json::query_rows(db, "SELECT kind, path, body_json, model, cost_usd FROM judgments.ultra_insights WHERE run_id = ?", &[run_id.clone()])?;
    let insight_rows: Vec<Value> = insight_rows_raw.as_array().cloned().unwrap_or_default();
    let mut dossiers: Map<String, Value> = Map::new();
    let mut brief: Value = Value::Null;
    let mut insight_cost: f64 = 0.0;
    for r in &insight_rows {
        let cost_v = nz(r.get("cost_usd")).cloned().unwrap_or(json!(0));
        insight_cost += js_number(Some(&cost_v));
        if let Some(body_str) = r.get("body_json").and_then(|v| v.as_str()) {
            if let Ok(body) = serde_json::from_str::<Value>(body_str) {
                let kind = r.get("kind").and_then(|v| v.as_str());
                if kind == Some("brief") {
                    let mut b = body.as_object().cloned().unwrap_or_default();
                    b.insert("model".into(), r.get("model").cloned().unwrap_or(Value::Null));
                    brief = Value::Object(b);
                } else if truthy(r.get("path")) {
                    let path_key = r.get("path").and_then(|v| v.as_str()).unwrap_or("").to_string();
                    let mut b = body.as_object().cloned().unwrap_or_default();
                    b.insert("model".into(), r.get("model").cloned().unwrap_or(Value::Null));
                    dossiers.insert(path_key, Value::Object(b));
                }
            }
        }
    }
    let stored_totals: Value = {
        let s = run.get("totals_json").filter(|v| !v.is_null()).and_then(|v| v.as_str());
        match s { Some(txt) => serde_json::from_str(txt).unwrap_or(Value::Null), None => Value::Null }
    };

    let ai_r_vals: Vec<f64> = cohort.iter().filter_map(|r| r.get("ai_readability").filter(|v| !v.is_null()).and_then(|v| v.as_f64())).collect();
    let findings_total: i64 = cohort.iter().map(|r| r.get("findings").and_then(|v| v.as_i64()).unwrap_or(0)).sum();
    let co_change = nz(stored_totals.get("co_change")).cloned().unwrap_or(json!([]));
    let bench_cost = js_number(Some(&nz(stored_totals.get("bench_cost_usd")).cloned().unwrap_or(json!(0))));
    let determinism: Value = if !cohort.is_empty() {
        json!(crate::js_math::js_round((known as f64 / cohort.len() as f64) * 100.0) as i64)
    } else { Value::Null };
    let provenance = format!(
        "atlas metrics \u{b7} atlas smells \u{b7} import closure \u{b7} git log (2000 commits) \u{b7} bugfinder ledger \u{2014} formula {}",
        tmpl(run.get("formula_version"))
    );

    let mut summary = Map::new();
    summary.insert("run_id".into(), run.get("id").cloned().unwrap_or(Value::Null));
    summary.insert("created_at".into(), run.get("created_at").cloned().unwrap_or(Value::Null));
    summary.insert("formula_version".into(), run.get("formula_version").cloned().unwrap_or(Value::Null));
    summary.insert("files".into(), json!(rows.len() as i64));
    summary.insert("source_files".into(), json!(cohort.len() as i64));
    summary.insert("test_files".into(), json!((rows.len() - cohort.len()) as i64));
    summary.insert("health".into(), mean(&healths));
    summary.insert("determinism".into(), determinism);
    summary.insert("determinism_parts".into(), json!({ "atlas_known": known as i64, "unknown": (cohort.len() - known) as i64 }));
    summary.insert("ai_readability".into(), mean(&ai_r_vals));
    summary.insert("axes_avg".into(), Value::Object(axes_avg));
    summary.insert("findings_total".into(), json!(findings_total));
    summary.insert("co_change".into(), co_change);
    summary.insert("cost_usd".into(), num_val(to_fixed4(insight_cost + bench_cost)));
    summary.insert("provenance".into(), json!(provenance));
    let summary_val = Value::Object(summary);

    // ── folder roll-up — blast-radius weighted, worst child surfaced ───────
    let mut folder_order: Vec<String> = Vec::new();
    let mut folder_rows: HashMap<String, Vec<Value>> = HashMap::new();
    for r in &cohort {
        let folder = r.get("folder").and_then(|v| v.as_str()).unwrap_or("").to_string();
        if !folder_rows.contains_key(&folder) { folder_order.push(folder.clone()); folder_rows.insert(folder.clone(), Vec::new()); }
        folder_rows.get_mut(&folder).unwrap().push(r.clone());
    }
    let mut folders: Vec<Value> = folder_order.iter().map(|folder| {
        let fs = &folder_rows[folder];
        let mut wsum = 0.0_f64;
        let mut hsum = 0.0_f64;
        let mut worst_health: Option<f64> = None;
        let mut worst_val: Value = Value::Null;
        for f in fs {
            let health_f = match f.get("health").filter(|v| !v.is_null()).and_then(|v| v.as_f64()) { Some(h) => h, None => continue };
            let fan_in = nz(f.get("fan_in")).and_then(|v| v.as_f64()).unwrap_or(0.0);
            let w = 1.0 + fan_in;
            wsum += w;
            hsum += health_f * w;
            if worst_health.map_or(true, |wh| health_f < wh) {
                worst_health = Some(health_f);
                worst_val = json!({ "path": f.get("path").cloned().unwrap_or(Value::Null), "health": f.get("health").cloned().unwrap_or(Value::Null) });
            }
        }
        let health_out: Value = if wsum > 0.0 { json!(crate::js_math::js_round(hsum / wsum) as i64) } else { Value::Null };
        json!({ "folder": folder, "files": fs.len() as i64, "weight": num_val(wsum), "health": health_out, "worst": worst_val })
    }).collect();
    folders.sort_by(|a, b| {
        let ah = a.get("health").and_then(|v| v.as_f64()).unwrap_or(101.0);
        let bh = b.get("health").and_then(|v| v.as_f64()).unwrap_or(101.0);
        ah.partial_cmp(&bh).unwrap()
    });

    // ── rankings + the unit payload (treemap/dossier) ───────────────────────
    let mut worst_sorted: Vec<Value> = cohort.iter().filter(|r| r.get("health").map_or(false, |v| !v.is_null())).cloned().collect();
    worst_sorted.sort_by(|a, b| {
        let ah = a.get("health").and_then(|v| v.as_f64()).unwrap();
        let bh = b.get("health").and_then(|v| v.as_f64()).unwrap();
        ah.partial_cmp(&bh).unwrap()
    });
    let worst_out: Vec<Value> = worst_sorted.into_iter().take(WORST_CAP).map(|r| unit_shape(&r)).collect();
    let units_out: Vec<Value> = rows.iter().take(UNIT_CAP).map(unit_shape).collect();

    // ── THE CONCEPT ATLAS — the ideas + what only an idea-graph can see ────
    let concept_rows_raw = crate::db_json::query_rows(db, "SELECT path, name, kind, evidence, model FROM judgments.ultra_concepts WHERE run_id = ?", &[run_id.clone()])?;
    let concept_rows: Vec<Value> = concept_rows_raw.as_array().cloned().unwrap_or_default();

    let mut concept_order: Vec<String> = Vec::new();
    let mut concept_names: HashMap<String, Value> = HashMap::new();
    let mut concept_kinds: HashMap<String, Value> = HashMap::new();
    let mut concept_evidence: HashMap<String, Value> = HashMap::new();
    let mut concept_model: HashMap<String, Value> = HashMap::new();
    let mut concept_files: HashMap<String, Vec<Value>> = HashMap::new();
    let mut path_order: Vec<String> = Vec::new();
    let mut by_path_concepts: HashMap<String, Vec<Value>> = HashMap::new();

    for c in &concept_rows {
        let name_key = c.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string();
        if !concept_files.contains_key(&name_key) {
            concept_order.push(name_key.clone());
            concept_names.insert(name_key.clone(), c.get("name").cloned().unwrap_or(Value::Null));
            concept_kinds.insert(name_key.clone(), c.get("kind").cloned().unwrap_or(Value::Null));
            concept_evidence.insert(name_key.clone(), c.get("evidence").cloned().unwrap_or(Value::Null));
            concept_model.insert(name_key.clone(), c.get("model").cloned().unwrap_or(Value::Null));
            concept_files.insert(name_key.clone(), Vec::new());
        }
        concept_files.get_mut(&name_key).unwrap().push(c.get("path").cloned().unwrap_or(Value::Null));

        let path_key = c.get("path").and_then(|v| v.as_str()).unwrap_or("").to_string();
        if !by_path_concepts.contains_key(&path_key) { path_order.push(path_key.clone()); by_path_concepts.insert(path_key.clone(), Vec::new()); }
        by_path_concepts.get_mut(&path_key).unwrap().push(c.get("name").cloned().unwrap_or(Value::Null));
    }

    let mut health_of: HashMap<String, Value> = HashMap::new();
    for r in &rows {
        let p = r.get("path").and_then(|v| v.as_str()).unwrap_or("").to_string();
        health_of.insert(p, r.clone());
    }

    let mut concept_list: Vec<Value> = concept_order.iter().map(|name_key| {
        let files = dedupe_str(&concept_files[name_key]);
        let units_: Vec<&Value> = files.iter().filter_map(|p| health_of.get(p.as_str().unwrap_or(""))).collect();
        let covered = units_.iter().filter(|u| u.get("covered").and_then(|v| v.as_i64()) == Some(1)).count();
        let health: Value = if !units_.is_empty() {
            let sum: f64 = units_.iter().map(|u| u.get("health").filter(|v| !v.is_null()).and_then(|v| v.as_f64()).unwrap_or(0.0)).sum();
            json!(crate::js_math::js_round(sum / units_.len() as f64) as i64)
        } else { Value::Null };
        json!({
            "name": concept_names[name_key].clone(), "kind": concept_kinds[name_key].clone(), "evidence": concept_evidence[name_key].clone(), "model": concept_model[name_key].clone(),
            "files": files, "file_count": files.len() as i64,
            "health": health,
            "untested": !units_.is_empty() && covered == 0,
            "scattered": files.len() >= 3,
        })
    }).collect();
    concept_list.sort_by(|a, b| {
        let afc = a.get("file_count").and_then(|v| v.as_i64()).unwrap_or(0);
        let bfc = b.get("file_count").and_then(|v| v.as_i64()).unwrap_or(0);
        bfc.cmp(&afc).then_with(|| {
            let an = a.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let bn = b.get("name").and_then(|v| v.as_str()).unwrap_or("");
            an.cmp(bn)
        })
    });
    concept_list.truncate(CONCEPT_CAP);

    let mut pair_order: Vec<String> = Vec::new();
    let mut pair_count: HashMap<String, i64> = HashMap::new();
    for path in &path_order {
        let mut uniq: Vec<String> = dedupe_str(&by_path_concepts[path]).iter().map(|v| v.as_str().unwrap_or("").to_string()).collect();
        uniq.sort();
        for i in 0..uniq.len() {
            for j in (i + 1)..uniq.len() {
                let k = format!("{}|{}", uniq[i], uniq[j]);
                if !pair_count.contains_key(&k) { pair_order.push(k.clone()); }
                *pair_count.entry(k).or_insert(0) += 1;
            }
        }
    }
    let mut concept_edges: Vec<Value> = pair_order.iter().map(|k| {
        let parts: Vec<&str> = k.splitn(2, '|').collect();
        let a = parts.first().copied().unwrap_or("");
        let b = parts.get(1).copied().unwrap_or("");
        json!({ "a": a, "b": b, "count": *pair_count.get(k).unwrap_or(&0) })
    }).collect();
    concept_edges.sort_by(|x, y| {
        let xc = x.get("count").and_then(|v| v.as_i64()).unwrap_or(0);
        let yc = y.get("count").and_then(|v| v.as_i64()).unwrap_or(0);
        yc.cmp(&xc)
    });
    concept_edges.truncate(80);

    // a file carrying ideas that never co-occur anywhere else = poor cohesion
    let mut mixed: Vec<Value> = Vec::new();
    for path in &path_order {
        let uniq_vals = dedupe_str(&by_path_concepts[path]);
        if uniq_vals.len() < 2 { continue; }
        let uniq_strs: Vec<String> = uniq_vals.iter().map(|v| v.as_str().unwrap_or("").to_string()).collect();
        let mut shared = 0;
        for i in 0..uniq_strs.len() {
            for j in (i + 1)..uniq_strs.len() {
                let mut pair = [uniq_strs[i].clone(), uniq_strs[j].clone()];
                pair.sort();
                let k = format!("{}|{}", pair[0], pair[1]);
                if *pair_count.get(&k).unwrap_or(&0) > 1 { shared += 1; }
            }
        }
        if shared == 0 { mixed.push(json!({ "path": path, "concepts": uniq_vals })); }
    }
    mixed.truncate(12);

    let concepts_payload: Value = if !concept_rows.is_empty() {
        let scattered: Vec<Value> = concept_list.iter().filter(|c| truthy(c.get("scattered"))).take(10).cloned().collect();
        let untested: Vec<Value> = concept_list.iter().filter(|c| truthy(c.get("untested"))).take(10).cloned().collect();
        json!({
            "total": concept_order.len() as i64,
            "list": concept_list,
            "edges": concept_edges,
            "scattered": scattered,
            "untested": untested,
            "mixed": mixed,
        })
    } else { Value::Null };

    // ── DELTAS — the second run is worth more than the first ───────────────
    let compare_to_arg = input.get("compare_to").filter(|v| !v.is_null());
    let prev_run: Option<Value> = match compare_to_arg {
        Some(v) => get_one(db, "SELECT id, created_at, totals_json FROM judgments.ultra_runs WHERE id = ?", &[json!(js_number(Some(v)))])?,
        None => get_one(
            db,
            "SELECT id, created_at, totals_json FROM judgments.ultra_runs WHERE id < ? AND status='done' AND scope_json = ? ORDER BY id DESC LIMIT 1",
            &[run_id.clone(), run.get("scope_json").cloned().unwrap_or(Value::Null)],
        )?,
    };

    let deltas: Value = if let Some(prev_run) = &prev_run {
        let prev_run_id = prev_run.get("id").cloned().unwrap_or(Value::Null);
        let prev_rows_raw = crate::db_json::query_rows(db, "SELECT path, health, content_hash FROM judgments.ultra_scores WHERE run_id = ?", &[prev_run_id.clone()])?;
        let prev_rows: Vec<Value> = prev_rows_raw.as_array().cloned().unwrap_or_default();
        let mut prev_by: HashMap<String, Value> = HashMap::new();
        for r in &prev_rows {
            let p = r.get("path").and_then(|v| v.as_str()).unwrap_or("").to_string();
            prev_by.insert(p, r.clone());
        }
        let prev_totals: Value = {
            let s = prev_run.get("totals_json").filter(|v| !v.is_null()).and_then(|v| v.as_str());
            match s { Some(txt) => serde_json::from_str(txt).unwrap_or(Value::Null), None => Value::Null }
        };

        let mut changed: Vec<Value> = Vec::new();
        for r in &cohort {
            let path = r.get("path").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let p = match prev_by.get(&path) { Some(p) => p, None => continue };
            let p_health = p.get("health").filter(|v| !v.is_null());
            let r_health = r.get("health").filter(|v| !v.is_null());
            if p_health.is_none() || r_health.is_none() { continue; }
            let ph = p_health.and_then(|v| v.as_f64()).unwrap();
            let rh = r_health.and_then(|v| v.as_f64()).unwrap();
            let d = rh - ph;
            if d != 0.0 {
                let content_changed = p.get("content_hash").cloned().unwrap_or(Value::Null) != r.get("content_hash").cloned().unwrap_or(Value::Null);
                changed.push(json!({
                    "path": path, "from": p.get("health").cloned().unwrap_or(Value::Null), "to": r.get("health").cloned().unwrap_or(Value::Null),
                    "delta": num_val(d), "content_changed": content_changed
                }));
            }
        }
        changed.sort_by(|a, b| {
            let ad = a.get("delta").and_then(|v| v.as_f64()).unwrap_or(0.0);
            let bd = b.get("delta").and_then(|v| v.as_f64()).unwrap_or(0.0);
            ad.partial_cmp(&bd).unwrap()
        });

        let prev_paths: HashSet<String> = prev_rows.iter().map(|r| r.get("path").and_then(|v| v.as_str()).unwrap_or("").to_string()).collect();

        let health_delta: Value = {
            let pt_health = prev_totals.get("health").filter(|v| !v.is_null());
            let sh = summary_val.get("health").filter(|v| !v.is_null());
            match (pt_health, sh) {
                (Some(pth), Some(shv)) => num_val(shv.as_f64().unwrap_or(0.0) - pth.as_f64().unwrap_or(0.0)),
                _ => Value::Null,
            }
        };

        let worse: Vec<Value> = changed.iter().filter(|c| c.get("delta").and_then(|v| v.as_f64()).unwrap_or(0.0) < 0.0).take(10).cloned().collect();
        let better_all: Vec<Value> = changed.iter().filter(|c| c.get("delta").and_then(|v| v.as_f64()).unwrap_or(0.0) > 0.0).cloned().collect();
        let start = better_all.len().saturating_sub(10);
        let mut better: Vec<Value> = better_all[start..].to_vec();
        better.reverse();

        let new_units: Vec<Value> = cohort.iter()
            .filter(|r| !prev_paths.contains(r.get("path").and_then(|v| v.as_str()).unwrap_or("")))
            .map(|r| r.get("path").cloned().unwrap_or(Value::Null))
            .take(10).collect();
        let gone_units: Vec<Value> = prev_rows.iter()
            .filter(|r| !health_of.contains_key(r.get("path").and_then(|v| v.as_str()).unwrap_or("")))
            .map(|r| r.get("path").cloned().unwrap_or(Value::Null))
            .take(10).collect();
        let unchanged_content: i64 = cohort.iter().filter(|r| {
            let path = r.get("path").and_then(|v| v.as_str()).unwrap_or("");
            match prev_by.get(path) {
                Some(p) => p.get("content_hash").cloned().unwrap_or(Value::Null) == r.get("content_hash").cloned().unwrap_or(Value::Null),
                None => false,
            }
        }).count() as i64;

        json!({
            "from_run": prev_run_id, "from_at": prev_run.get("created_at").cloned().unwrap_or(Value::Null),
            "health": health_delta,
            "worse": worse, "better": better,
            "new_units": new_units, "gone_units": gone_units,
            "unchanged_content": unchanged_content,
        })
    } else {
        Value::Null
    };

    let run_scope: Value = {
        let s = nz(run.get("scope_json")).and_then(|v| v.as_str()).unwrap_or("{}");
        serde_json::from_str(s).unwrap_or(json!({}))
    };
    let run_out = json!({
        "id": run.get("id").cloned().unwrap_or(Value::Null),
        "created_at": run.get("created_at").cloned().unwrap_or(Value::Null),
        "source": run.get("source").cloned().unwrap_or(Value::Null),
        "status": run.get("status").cloned().unwrap_or(Value::Null),
        "root": run.get("root").cloned().unwrap_or(Value::Null),
        "scope": run_scope,
        "formula_version": run.get("formula_version").cloned().unwrap_or(Value::Null),
    });

    let truncated: i64 = if rows.len() as i64 > UNIT_CAP as i64 { rows.len() as i64 - UNIT_CAP as i64 } else { 0 };

    let mut out = Map::new();
    out.insert("run".into(), run_out);
    out.insert("runs".into(), Value::Array(runs));
    out.insert("summary".into(), summary_val);
    out.insert("folders".into(), Value::Array(folders));
    out.insert("worst".into(), Value::Array(worst_out));
    out.insert("units".into(), Value::Array(units_out));
    out.insert("dossiers".into(), Value::Object(dossiers));
    out.insert("brief".into(), brief);
    out.insert("concepts".into(), concepts_payload);
    out.insert("deltas".into(), deltas);
    out.insert("truncated".into(), json!(truncated));
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS judgments").unwrap();
        c.execute_batch(
            "
CREATE TABLE judgments.ultra_runs (
  id INTEGER PRIMARY KEY AUTOINCREMENT, created_at TEXT NOT NULL, source TEXT NOT NULL DEFAULT 'manual',
  root TEXT, scope_json TEXT NOT NULL DEFAULT '{}', status TEXT NOT NULL DEFAULT 'running',
  files INTEGER NOT NULL DEFAULT 0, formula_version TEXT NOT NULL DEFAULT 'v1', totals_json TEXT, error TEXT
);
CREATE TABLE judgments.ultra_scores (
  id INTEGER PRIMARY KEY AUTOINCREMENT, run_id INTEGER NOT NULL, path TEXT NOT NULL, folder TEXT NOT NULL,
  content_hash TEXT, atlas_known INTEGER NOT NULL DEFAULT 0, is_test INTEGER NOT NULL DEFAULT 0,
  loc INTEGER, fan_in INTEGER, fan_out INTEGER, complexity INTEGER, transitive INTEGER,
  covered INTEGER, churn INTEGER, authors INTEGER, findings INTEGER NOT NULL DEFAULT 0,
  findings_high INTEGER NOT NULL DEFAULT 0, smells_json TEXT, axes_json TEXT NOT NULL,
  health INTEGER, ai_readability INTEGER, pct_repo REAL, pct_folder REAL, created_at TEXT NOT NULL
);
CREATE TABLE judgments.ultra_insights (
  id INTEGER PRIMARY KEY AUTOINCREMENT, run_id INTEGER NOT NULL, kind TEXT NOT NULL, path TEXT,
  body_json TEXT NOT NULL, model TEXT, cost_usd REAL NOT NULL DEFAULT 0, created_at TEXT NOT NULL
);
CREATE TABLE judgments.ultra_concepts (
  id INTEGER PRIMARY KEY AUTOINCREMENT, run_id INTEGER NOT NULL, path TEXT NOT NULL, content_hash TEXT NOT NULL,
  name TEXT NOT NULL, kind TEXT NOT NULL DEFAULT 'mechanism', evidence TEXT, model TEXT, created_at TEXT NOT NULL
);
",
        )
        .unwrap();
        c
    }

    fn insert_run(c: &Connection, id: i64, created_at: &str, scope_json: &str, status: &str, formula_version: &str, totals_json: Option<&str>) {
        c.execute(
            "INSERT INTO judgments.ultra_runs (id, created_at, source, root, scope_json, status, files, formula_version, totals_json, error) VALUES (?,?,'manual',NULL,?,?,0,?,?,NULL)",
            rusqlite::params![id, created_at, scope_json, status, formula_version, totals_json],
        ).unwrap();
    }

    #[test]
    fn full_scenario_summary_folders_concepts_deltas() {
        let c = setup();
        insert_run(&c, 1, "2026-01-05 00:00:00", "{\"folder\":\"src\"}", "done", "v2", Some("{\"co_change\":[[\"a.mjs\",\"b.mjs\"]],\"bench_cost_usd\":1.5,\"health\":70}"));
        insert_run(&c, 2, "2025-12-20 00:00:00", "{\"folder\":\"src\"}", "running", "v1", Some("{\"health\":60}"));

        c.execute("INSERT INTO judgments.ultra_scores (run_id,path,folder,content_hash,atlas_known,is_test,loc,fan_in,fan_out,complexity,transitive,covered,churn,authors,findings,findings_high,smells_json,axes_json,health,ai_readability,pct_repo,pct_folder,created_at) VALUES (1,'src/a.mjs','src','hashA2',1,0,100,2,1,5,10,1,3,2,2,1,'[\"god-file\"]','{\"structure\":{\"score\":80},\"tests\":{\"score\":90}}',75,60,0.9,0.9,'2026-01-05 00:00:00')", []).unwrap();
        c.execute("INSERT INTO judgments.ultra_scores (run_id,path,folder,content_hash,atlas_known,is_test,loc,fan_in,fan_out,complexity,transitive,covered,churn,authors,findings,findings_high,smells_json,axes_json,health,ai_readability,pct_repo,pct_folder,created_at) VALUES (1,'src/b.mjs','src','hashB2',0,0,50,0,2,3,4,0,1,1,0,0,'[]','{\"structure\":{\"score\":60}}',75,20,0.3,0.3,'2026-01-05 00:00:00')", []).unwrap();
        c.execute("INSERT INTO judgments.ultra_scores (run_id,path,folder,content_hash,atlas_known,is_test,loc,fan_in,fan_out,complexity,transitive,covered,churn,authors,findings,findings_high,smells_json,axes_json,health,ai_readability,pct_repo,pct_folder,created_at) VALUES (1,'lib/c.mjs','lib','hashC1',1,0,200,5,0,8,20,1,5,3,1,0,'[\"long-file\"]','{\"coupling\":{\"score\":50}}',40,30,0.5,1.0,'2026-01-05 00:00:00')", []).unwrap();
        c.execute("INSERT INTO judgments.ultra_scores (run_id,path,folder,content_hash,atlas_known,is_test,loc,fan_in,fan_out,complexity,transitive,covered,churn,authors,findings,findings_high,smells_json,axes_json,health,ai_readability,pct_repo,pct_folder,created_at) VALUES (1,'src/d.test.mjs','src','hashD1',1,1,30,0,0,1,1,NULL,0,1,0,0,'[]','{}',95,NULL,NULL,NULL,'2026-01-05 00:00:00')", []).unwrap();

        c.execute("INSERT INTO judgments.ultra_scores (run_id,path,folder,content_hash,axes_json,health,created_at) VALUES (2,'lib/c.mjs','lib','hashC1','{}',50,'2025-12-20 00:00:00')", []).unwrap();
        c.execute("INSERT INTO judgments.ultra_scores (run_id,path,folder,content_hash,axes_json,health,created_at) VALUES (2,'src/a.mjs','src','hashA1','{}',70,'2025-12-20 00:00:00')", []).unwrap();
        c.execute("INSERT INTO judgments.ultra_scores (run_id,path,folder,content_hash,axes_json,health,created_at) VALUES (2,'src/x_gone.mjs','src','hashX','{}',55,'2025-12-20 00:00:00')", []).unwrap();

        c.execute("INSERT INTO judgments.ultra_insights (run_id,kind,path,body_json,model,cost_usd,created_at) VALUES (1,'dossier','src/a.mjs','{\"why\":\"complex\",\"alternatives\":[\"split\"],\"fix_sketch\":\"extract fn\",\"estimated_gain\":10}','claude',0.02,'2026-01-05 00:00:00')", []).unwrap();
        c.execute("INSERT INTO judgments.ultra_insights (run_id,kind,path,body_json,model,cost_usd,created_at) VALUES (1,'brief',NULL,'{\"headline\":\"Focus on src\",\"state_md\":\"# state\",\"moves\":[\"refactor a.mjs\"]}','claude',0.03,'2026-01-05 00:00:00')", []).unwrap();
        c.execute("INSERT INTO judgments.ultra_insights (run_id,kind,path,body_json,model,cost_usd,created_at) VALUES (1,'dossier','src/b.mjs','not-json{{','gpt',0.05,'2026-01-05 00:00:00')", []).unwrap();
        c.execute("INSERT INTO judgments.ultra_insights (run_id,kind,path,body_json,model,cost_usd,created_at) VALUES (1,'dossier','','{\"why\":\"n/a\"}','x',0.01,'2026-01-05 00:00:00')", []).unwrap();

        for (path, name) in [
            ("src/a.mjs", "audit-chain"), ("src/b.mjs", "audit-chain"), ("lib/c.mjs", "audit-chain"),
            ("src/a.mjs", "cache"), ("src/b.mjs", "cache"),
            ("lib/c.mjs", "orphan-concept"),
            ("src/b.mjs", "ghost-concept"),
        ] {
            c.execute("INSERT INTO judgments.ultra_concepts (run_id,path,content_hash,name,kind,evidence,model,created_at) VALUES (1,?,'h',?,'mechanism',NULL,NULL,'2026-01-05 00:00:00')", rusqlite::params![path, name]).unwrap();
        }

        let out = ultraReport(&json!({ "run_id": 1, "compare_to": 2 }), &c).unwrap();
        assert_eq!(out["run"]["id"], json!(1));
        assert_eq!(out["run"]["scope"], json!({ "folder": "src" }));
        assert_eq!(out["summary"]["files"], json!(4));
        assert_eq!(out["summary"]["source_files"], json!(3));
        assert_eq!(out["summary"]["test_files"], json!(1));
        assert_eq!(out["summary"]["health"], json!(63));
        assert_eq!(out["summary"]["determinism"], json!(67));
        assert_eq!(out["summary"]["determinism_parts"], json!({ "atlas_known": 2, "unknown": 1 }));
        assert_eq!(out["summary"]["ai_readability"], json!(37));
        assert_eq!(out["summary"]["axes_avg"], json!({ "structure": 70, "coupling": 50, "tests": 90, "defects": null, "change": null, "knowledge": null }));
        assert_eq!(out["summary"]["findings_total"], json!(3));
        assert_eq!(out["summary"]["co_change"], json!([["a.mjs", "b.mjs"]]));
        assert_eq!(out["summary"]["cost_usd"], json!(1.61));

        assert_eq!(out["folders"].as_array().unwrap().len(), 2);
        assert_eq!(out["folders"][0]["folder"], json!("lib"));
        assert_eq!(out["folders"][0]["health"], json!(40));
        assert_eq!(out["folders"][0]["worst"], json!({ "path": "lib/c.mjs", "health": 40 }));
        assert_eq!(out["folders"][1]["folder"], json!("src"));
        assert_eq!(out["folders"][1]["health"], json!(75));
        assert_eq!(out["folders"][1]["worst"], json!({ "path": "src/a.mjs", "health": 75 }));

        let worst_paths: Vec<&str> = out["worst"].as_array().unwrap().iter().map(|u| u["path"].as_str().unwrap()).collect();
        assert_eq!(worst_paths, vec!["lib/c.mjs", "src/a.mjs", "src/b.mjs"]);
        let unit_paths: Vec<&str> = out["units"].as_array().unwrap().iter().map(|u| u["path"].as_str().unwrap()).collect();
        assert_eq!(unit_paths, vec!["lib/c.mjs", "src/a.mjs", "src/b.mjs", "src/d.test.mjs"]);

        assert_eq!(out["dossiers"].as_object().unwrap().keys().collect::<Vec<_>>(), vec!["src/a.mjs"]);
        assert_eq!(out["dossiers"]["src/a.mjs"]["why"], json!("complex"));
        assert_eq!(out["brief"]["headline"], json!("Focus on src"));

        assert_eq!(out["concepts"]["total"], json!(4));
        let names: Vec<&str> = out["concepts"]["list"].as_array().unwrap().iter().map(|x| x["name"].as_str().unwrap()).collect();
        assert_eq!(names, vec!["audit-chain", "cache", "ghost-concept", "orphan-concept"]);
        assert_eq!(out["concepts"]["list"][0]["scattered"], json!(true));
        assert_eq!(out["concepts"]["list"][2]["untested"], json!(true));
        assert_eq!(out["concepts"]["edges"][0], json!({ "a": "audit-chain", "b": "cache", "count": 2 }));
        assert_eq!(out["concepts"]["edges"].as_array().unwrap().len(), 4);
        assert_eq!(out["concepts"]["mixed"], json!([{ "path": "lib/c.mjs", "concepts": ["audit-chain", "orphan-concept"] }]));

        assert_eq!(out["deltas"]["from_run"], json!(2));
        assert_eq!(out["deltas"]["health"], json!(3));
        let worse_paths: Vec<&str> = out["deltas"]["worse"].as_array().unwrap().iter().map(|c| c["path"].as_str().unwrap()).collect();
        assert_eq!(worse_paths, vec!["lib/c.mjs"]);
        assert_eq!(out["deltas"]["worse"][0]["delta"], json!(-10));
        let better_paths: Vec<&str> = out["deltas"]["better"].as_array().unwrap().iter().map(|c| c["path"].as_str().unwrap()).collect();
        assert_eq!(better_paths, vec!["src/a.mjs"]);
        assert_eq!(out["deltas"]["better"][0]["content_changed"], json!(true));
        assert_eq!(out["deltas"]["new_units"], json!(["src/b.mjs"]));
        assert_eq!(out["deltas"]["gone_units"], json!(["src/x_gone.mjs"]));
        assert_eq!(out["deltas"]["unchanged_content"], json!(1));
        assert_eq!(out["truncated"], json!(0));
    }

    #[test]
    fn run_not_found_returns_early_exit_shape_only() {
        let c = setup();
        insert_run(&c, 1, "2026-01-01 00:00:00", "{}", "done", "v1", None);
        let out = ultraReport(&json!({ "run_id": 999 }), &c).unwrap();
        assert_eq!(out["run"], Value::Null);
        assert_eq!(out["runs"].as_array().unwrap().len(), 1);
        assert_eq!(out["summary"], Value::Null);
        assert_eq!(out["folders"], json!([]));
        assert_eq!(out["worst"], json!([]));
        assert_eq!(out["units"], json!([]));
        let mut keys: Vec<&String> = out.as_object().unwrap().keys().collect();
        keys.sort();
        assert_eq!(keys, vec!["folders", "run", "runs", "summary", "units", "worst"]);
    }

    #[test]
    fn all_test_cohort_null_aggregates_units_from_all_rows() {
        let c = setup();
        insert_run(&c, 1, "2026-02-01 00:00:00", "{}", "done", "v3", None);
        c.execute("INSERT INTO judgments.ultra_scores (run_id,path,folder,content_hash,atlas_known,is_test,loc,fan_in,fan_out,complexity,transitive,covered,churn,authors,findings,findings_high,smells_json,axes_json,health,ai_readability,pct_repo,pct_folder,created_at) VALUES (1,'only.test.mjs','root','hZ',0,1,5,0,0,1,1,NULL,0,1,5,1,'[\"x\"]','{\"structure\":{\"score\":99}}',10,10,NULL,NULL,'2026-02-01 00:00:00')", []).unwrap();

        let out = ultraReport(&json!({ "run_id": 1 }), &c).unwrap();
        assert_eq!(out["summary"]["files"], json!(1));
        assert_eq!(out["summary"]["source_files"], json!(0));
        assert_eq!(out["summary"]["test_files"], json!(1));
        assert_eq!(out["summary"]["health"], Value::Null);
        assert_eq!(out["summary"]["determinism"], Value::Null);
        assert_eq!(out["summary"]["determinism_parts"], json!({ "atlas_known": 0, "unknown": 0 }));
        assert_eq!(out["summary"]["findings_total"], json!(0));
        assert_eq!(out["summary"]["co_change"], json!([]));
        assert_eq!(out["summary"]["cost_usd"], json!(0));
        assert_eq!(out["folders"], json!([]));
        assert_eq!(out["worst"], json!([]));
        let unit_paths: Vec<&str> = out["units"].as_array().unwrap().iter().map(|u| u["path"].as_str().unwrap()).collect();
        assert_eq!(unit_paths, vec!["only.test.mjs"]);
        assert_eq!(out["dossiers"], json!({}));
        assert_eq!(out["brief"], Value::Null);
        assert_eq!(out["concepts"], Value::Null);
        assert_eq!(out["deltas"], Value::Null);
        assert_eq!(out["truncated"], json!(0));
    }
}
