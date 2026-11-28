//! atlas-score.rs — twin of skills/codebase/atlas-score.mjs (db READ lane). NAMES ARE CONTRACT:
//! `atlasScore` (default export). The composite 0-100 health/risk score, normalizing a MINIMAL
//! subset of atlas-metrics.mjs's signals (fan_in, complexity, churn, covered/is_test) into
//! risk = w.fanin·norm(fan_in) + w.cx·norm(complexity) + w.churn·norm(churn) + w.uncov·uncovered.
//!
//! DEPENDENCY SCOPE (playbook lesson 6's "inline what you need, don't convert the whole
//! dependency" pattern, applied one level up from roots.mjs to a skill-level import): no
//! atlas-metrics.rs twin exists yet, so the handful of fields atlas-score.mjs actually reads off
//! `atlasMetrics(...).metrics_by_file` are recomputed here directly against `code_nodes`/`code_edges`
//! (the SAME SQL atlas-metrics.mjs uses for those fields) rather than calling an unconverted module.
//! `is_test`/`covered` reuse the already-converted `codebase_atlas_test_map` twin verbatim (lesson 4).
//!
//! CHURN SCOPE LIMIT — disclosed, deliberate (playbook lesson 5's skippable-branch escape hatch,
//! confirmed by reading atlas-metrics.mjs rather than assumed): atlas-metrics resolves churn via
//! `root = input.root ?? activeRoot(db)` (the active `workspaces` row), then prefers a richer
//! `gitHistory(root)` (code-signals-graph.mjs — not yet a Rust twin) and falls back to the simpler
//! `gitChurn(root)` (a bounded `git log -n 500 --name-only` subprocess) only when gitHistory itself
//! returns null. This twin reproduces the root-resolution AND the gitChurn fallback verbatim, but
//! does NOT reproduce gitHistory (a separate, unconverted module — out of scope for atlas-score).
//! Consequence: root == null (no active workspace row + no `input.root` — the ONLY case any fixture
//! here exercises, matching the real JS test file's own scope, which also never sets a root) is
//! byte-exact. A caller that supplies a root resolving non-null would hit the real `git` subprocess;
//! since a real git history is host-dependent, that path is untestable via a deterministic fixture
//! regardless of how it's implemented, so it is not fixture-exercised — a disclosed, not a silent, gap.
use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::collections::HashMap;

fn truthy(v: Option<&Value>) -> bool { match v { None|Some(Value::Null)=>false, Some(Value::Bool(b))=>*b, Some(Value::Number(n))=>n.as_f64().map_or(false,|f|f!=0.0&&!f.is_nan()), Some(Value::String(s))=>!s.is_empty(), Some(_)=>true } }
fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) } // JS `??` right operand
fn js_number(v: Option<&Value>) -> f64 { match v { None=>f64::NAN, Some(Value::Null)=>0.0, Some(Value::Bool(b))=>if *b {1.0} else {0.0}, Some(Value::Number(n))=>n.as_f64().unwrap_or(f64::NAN), Some(Value::String(s))=>{let t=s.trim(); if t.is_empty(){0.0}else{t.parse().unwrap_or(f64::NAN)}}, Some(_)=>f64::NAN } }

// = atlas-score.mjs's p95(values): 95th percentile of the positive values (0 if none).
fn p95(values: &[f64]) -> f64 {
    let mut v: Vec<f64> = values.iter().copied().filter(|x| *x > 0.0).collect();
    if v.is_empty() { return 0.0; }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let idx = (((v.len() - 1) as f64) * 0.95).floor() as usize;
    v[idx.min(v.len() - 1)]
}
// = atlas-score.mjs's norm(x, p): Math.min(1, x / p) when p > 0, else 0.
fn norm(x: f64, p: f64) -> f64 { if p > 0.0 { (x / p).min(1.0) } else { 0.0 } }

// gotcha 2: norm()/the uncovered ternary often land on exactly 0 or 1 — JS renders a whole-number
// double bare ("1", not "1.0"); serde_json renders an f64 with a decimal point regardless. Mirror
// JS's Number-to-JSON-string behavior by emitting a JSON integer whenever the value is whole.
fn num_to_value(x: f64) -> Value {
    if x == 0.0 { return json!(0i64); } // normalize -0.0 → 0
    if x.is_finite() && x.fract() == 0.0 && x.abs() < 9.007_199_254_740_992e15 { json!(x as i64) } else { json!(x) }
}

// = atlas-metrics.mjs's activeRoot(db): the active workspace's root, best-effort (missing
// `workspaces` table / no active row → None, matching the JS try/catch → null).
fn active_root(db: &Connection) -> Option<String> {
    crate::db_json::query_rows(db, "SELECT root_path FROM workspaces WHERE is_active = 1 LIMIT 1", &[])
        .ok()
        .and_then(|r| r.as_array().and_then(|a| a.first()).cloned())
        .and_then(|row| row.get("root_path").and_then(|v| v.as_str()).map(|s| s.to_string()))
}

// = atlas-metrics.mjs's gitChurn(root): per-file commit count over the last 500 commits,
// best-effort (see module doc — this is the disclosed, fixture-unreachable churn-fallback path;
// unlike the JS it does not first try gitHistory, which lives in an unconverted module).
fn git_churn(root: &str) -> Option<HashMap<String, i64>> {
    let out = crate::proc::cmd("git")
        .args(["-C", root, "log", "-n", "500", "--name-only", "--format="])
        .output()
        .ok()?;
    if !out.status.success() { return None; }
    let text = String::from_utf8_lossy(&out.stdout);
    let mut counts: HashMap<String, i64> = HashMap::new();
    for line in text.split('\n') {
        let p = line.trim().replace('\\', "/");
        if !p.is_empty() { *counts.entry(p).or_insert(0) += 1; }
    }
    // atlas-metrics.mjs: `if (churn && churn.size === 0) churn = null;` — an empty-but-successful
    // result (bounded commit window touched zero files) normalizes to the SAME "unavailable" state
    // as a failed git invocation, forcing the caller's weight-renormalization branch.
    if counts.is_empty() { None } else { Some(counts) }
}

struct FileMetric { fan_in: i64, complexity: f64, is_test: bool, covered: bool, churn: Option<i64> }

// Recomputes the minimal subset of atlas-metrics.mjs's metrics_by_file that atlas-score.mjs reads
// (fan_in, complexity, is_test, covered, churn) — see module doc for the churn scope limit.
fn gather_metrics(db: &Connection, origin: &str, root: Option<&str>) -> Result<Vec<(String, FileMetric)>, String> {
    let files = crate::db_json::query_rows(db, "SELECT id, path, span_json FROM code_nodes WHERE origin = ? AND type = 'file'", &[json!(origin)])?;
    let files = files.as_array().cloned().unwrap_or_default();

    let covered_arr = crate::codebase_atlas_test_map::coveredPaths(db, origin);
    let covered: std::collections::HashSet<String> = covered_arr.as_array()
        .map(|a| a.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect())
        .unwrap_or_default();

    let churn_map: Option<HashMap<String, i64>> = match root {
        Some(r) if !r.is_empty() => git_churn(r),
        _ => None,
    };

    let mut out: Vec<(String, FileMetric)> = Vec::new();
    for f in &files {
        let id = f.get("id").cloned().unwrap_or(Value::Null);
        let path = f.get("path").and_then(|v| v.as_str()).unwrap_or("").to_string();

        let fan_in_rows = crate::db_json::query_rows(db, "SELECT count(DISTINCT from_node) n FROM code_edges WHERE origin = ? AND to_node = ? AND type = 'imports'", &[json!(origin), id.clone()])?;
        let fan_in = fan_in_rows.as_array().and_then(|a| a.first()).and_then(|r| r.get("n")).and_then(|v| v.as_i64()).unwrap_or(0);

        let child_rows = crate::db_json::query_rows(db, "SELECT span_json FROM code_nodes WHERE origin = ? AND parent_id = ? AND type <> 'file'", &[json!(origin), id.clone()])?;
        let mut complexity: f64 = 0.0;
        if let Some(arr) = child_rows.as_array() {
            for c in arr {
                if let Some(sj) = c.get("span_json").and_then(|v| v.as_str()) {
                    if let Ok(s) = serde_json::from_str::<Value>(sj) {
                        if let Some(n) = s.get("complexity") { if n.is_number() { complexity += n.as_f64().unwrap_or(0.0); } }
                    }
                }
            }
        }

        let is_test = crate::codebase_atlas_test_map::isTestPath(&Value::String(path.clone())) == json!(true);
        let is_covered = covered.contains(&path);
        let churn = churn_map.as_ref().map(|m| m.get(&path).copied().unwrap_or(0));

        out.push((path, FileMetric { fan_in, complexity, is_test, covered: is_covered, churn }));
    }
    Ok(out)
}

pub fn atlasScore(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS: const { db } = context ?? {}; if (!db) throw err.contextMissing('atlas-score', 'db') —
    // the db lane always injects a live connection, so that guard is unreachable here (gotcha 9),
    // same pattern as atlas-get.rs.
    let origin = nz(input.get("origin")).and_then(|v| v.as_str()).unwrap_or("atlas").to_string();

    // Math.max(1, Number(input?.top ?? 10)) — success-path only (gotcha 9): Rust's f64::max ignores
    // NaN where JS's Math.max propagates it (atlas-get.rs precedent); not exercised by any valid
    // fixture (top is always a plain number or absent there).
    let top_raw = nz(input.get("top")).cloned().unwrap_or(json!(10));
    let top_n_f = js_number(Some(&top_raw)).max(1.0);
    let top_n: usize = if top_n_f.is_finite() { top_n_f as usize } else { 0 };

    // root = input?.root ?? activeRoot(db); truthy gate (empty string / 0 / null all skip churn).
    let root_val: Option<Value> = nz(input.get("root")).cloned().or_else(|| active_root(db).map(Value::String));
    let root_str: Option<String> = if truthy(root_val.as_ref()) {
        root_val.as_ref().and_then(|v| v.as_str()).map(|s| s.to_string())
    } else {
        None
    };

    let files_all = gather_metrics(db, &origin, root_str.as_deref())?;
    let files: Vec<(String, FileMetric)> = files_all.into_iter().filter(|(_, x)| !x.is_test).collect();

    if files.is_empty() {
        return Ok(json!({
            "project": { "health": 100, "risk": 0 },
            "by_file": {},
            "top_risk": [],
            "file_count": 0,
            "churn_available": false,
            "weights": { "fanin": 0.30, "cx": 0.25, "churn": 0.20, "uncov": 0.25 }
        }));
    }

    // churnAvailable = files.some(([, x]) => typeof x.churn === 'number') — post is_test-filter.
    let churn_available = files.iter().any(|(_, x)| x.churn.is_some());

    // w = { ...DEFAULT_WEIGHTS, ...(input?.weights ?? {}) } — gotcha 1: insert-in-place preserves
    // DEFAULT_WEIGHTS key order; an unrecognized extra key in input.weights would append here, but
    // (see module doc) churn_available is always false in this file's fixture scope, so the
    // !churn_available branch below always rebuilds a fresh 4-key object and any such extra key
    // never survives to the output — a fixture-unreachable distinction, not a silent gap.
    let mut w: Map<String, Value> = Map::new();
    w.insert("fanin".into(), json!(0.30));
    w.insert("cx".into(), json!(0.25));
    w.insert("churn".into(), json!(0.20));
    w.insert("uncov".into(), json!(0.25));
    if let Some(Value::Object(uw)) = nz(input.get("weights")) {
        for (k, v) in uw { w.insert(k.clone(), v.clone()); }
    }
    if !churn_available {
        // drop the churn term + renormalize the rest to sum 1: const keep = (sum) || 1
        let sum = js_number(w.get("fanin")) + js_number(w.get("cx")) + js_number(w.get("uncov"));
        let keep = if sum == 0.0 || sum.is_nan() { 1.0 } else { sum };
        let mut w2: Map<String, Value> = Map::new();
        w2.insert("fanin".into(), num_to_value(js_number(w.get("fanin")) / keep));
        w2.insert("cx".into(), num_to_value(js_number(w.get("cx")) / keep));
        w2.insert("churn".into(), json!(0));
        w2.insert("uncov".into(), num_to_value(js_number(w.get("uncov")) / keep));
        w = w2;
    }
    let w_fanin = js_number(w.get("fanin"));
    let w_cx = js_number(w.get("cx"));
    let w_churn = js_number(w.get("churn"));
    let w_uncov = js_number(w.get("uncov"));

    let p95_fanin = p95(&files.iter().map(|(_, x)| x.fan_in as f64).collect::<Vec<_>>());
    let p95_cx = p95(&files.iter().map(|(_, x)| x.complexity).collect::<Vec<_>>());
    let p95_churn = p95(&files.iter().map(|(_, x)| x.churn.map(|c| c as f64).unwrap_or(0.0)).collect::<Vec<_>>());

    let mut by_file: Map<String, Value> = Map::new();
    let mut weighted_risk_sum = 0.0f64;
    let mut weight_sum = 0.0f64;
    for (path, x) in &files {
        let f_fanin = norm(x.fan_in as f64, p95_fanin);
        let f_cx = norm(x.complexity, p95_cx);
        let f_churn = if churn_available { norm(x.churn.map(|c| c as f64).unwrap_or(0.0), p95_churn) } else { 0.0 };
        let f_uncov = if x.covered { 0.0 } else { 1.0 };
        let risk = w_fanin * f_fanin + w_cx * f_cx + w_churn * f_churn + w_uncov * f_uncov;
        let risk_i = crate::js_math::js_round(risk * 100.0) as i64;
        let health_i = crate::js_math::js_round((1.0 - risk) * 100.0) as i64;
        let factors = json!({ "fan_in": num_to_value(f_fanin), "complexity": num_to_value(f_cx), "churn": num_to_value(f_churn), "uncovered": num_to_value(f_uncov) });
        by_file.insert(path.clone(), json!({ "risk": risk_i, "health": health_i, "factors": factors }));
        let fw = x.fan_in as f64 + 1.0;
        weighted_risk_sum += risk * fw;
        weight_sum += fw;
    }
    let project_risk = if weight_sum > 0.0 { weighted_risk_sum / weight_sum } else { 0.0 };

    // top_risk = Object.entries(by_file).map(([path,s]) => ({path, ...s})) — by_file iterates in
    // insertion order (== the SQL `files` scan order, gotcha 1), then sort desc by risk, ties by
    // path.localeCompare (ascending — ASCII byte order matches Rust's default String Ord here).
    let mut top_risk: Vec<Value> = by_file.iter().map(|(path, s)| {
        let mut o = s.as_object().unwrap().clone();
        let mut ordered = Map::new();
        ordered.insert("path".into(), json!(path));
        ordered.append(&mut o);
        Value::Object(ordered)
    }).collect();
    top_risk.sort_by(|a, b| {
        let br = b["risk"].as_i64().unwrap_or(0);
        let ar = a["risk"].as_i64().unwrap_or(0);
        br.cmp(&ar).then_with(|| a["path"].as_str().unwrap_or("").cmp(b["path"].as_str().unwrap_or("")))
    });
    top_risk.truncate(top_n);

    // `weights: w` in the JS return — the object is never re-derived for output, only used by
    // value for the arithmetic above; return it verbatim (preserves original int/float typing +
    // any extra keys from input.weights when churn_available kept the spread-merged object).
    Ok(json!({
        "project": { "health": crate::js_math::js_round((1.0 - project_risk) * 100.0) as i64, "risk": crate::js_math::js_round(project_risk * 100.0) as i64 },
        "by_file": by_file,
        "top_risk": top_risk,
        "file_count": files.len() as i64,
        "churn_available": churn_available,
        "weights": Value::Object(w),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCHEMA: &str = "
        CREATE TABLE code_nodes (id TEXT PRIMARY KEY, origin TEXT, path TEXT, type TEXT, parent_id TEXT, span_json TEXT);
        CREATE TABLE code_edges (origin TEXT, from_node TEXT, to_node TEXT, type TEXT, kind TEXT);
    ";
    fn db() -> Connection { let c = Connection::open_in_memory().unwrap(); c.execute_batch(SCHEMA).unwrap(); c }

    #[test]
    fn central_complex_uncovered_outranks_a_simple_covered_file() {
        let c = db();
        c.execute_batch("
            INSERT INTO code_nodes (id,origin,path,type,parent_id,span_json) VALUES
              ('core.ts@atlas','atlas','core.ts','file',NULL,NULL),
              ('core.ts#big','atlas','core.ts','function','core.ts@atlas','{\"file\":\"core.ts\",\"start_line\":1,\"end_line\":50,\"complexity\":20}'),
              ('safe.ts@atlas','atlas','safe.ts','file',NULL,NULL),
              ('safe.ts#tiny','atlas','safe.ts','function','safe.ts@atlas','{\"file\":\"safe.ts\",\"start_line\":1,\"end_line\":3,\"complexity\":1}'),
              ('a.ts@atlas','atlas','a.ts','file',NULL,NULL),
              ('b.ts@atlas','atlas','b.ts','file',NULL,NULL),
              ('c.ts@atlas','atlas','c.ts','file',NULL,NULL),
              ('safe.test.ts@atlas','atlas','safe.test.ts','file',NULL,NULL);
            INSERT INTO code_edges (origin,from_node,to_node,type) VALUES
              ('atlas','a.ts@atlas','core.ts@atlas','imports'),
              ('atlas','b.ts@atlas','core.ts@atlas','imports'),
              ('atlas','c.ts@atlas','core.ts@atlas','imports'),
              ('atlas','safe.test.ts@atlas','safe.ts@atlas','imports');
        ").unwrap();
        let r = atlasScore(&json!({}), &c).unwrap();
        let core = &r["by_file"]["core.ts"];
        let safe = &r["by_file"]["safe.ts"];
        assert!(core["risk"].as_i64().unwrap() > safe["risk"].as_i64().unwrap());
        assert_eq!(r["top_risk"][0]["path"], json!("core.ts"));
        assert!(core["factors"]["fan_in"].as_f64().unwrap() > 0.0 && core["factors"]["complexity"].as_f64().unwrap() > 0.0);
        assert_eq!(core["factors"]["uncovered"], json!(1));
        assert_eq!(safe["factors"]["uncovered"], json!(0));
        let proj_risk = r["project"]["risk"].as_i64().unwrap();
        assert!(proj_risk >= 0 && proj_risk <= 100);
        assert_eq!(r["churn_available"], json!(false));
    }

    #[test]
    fn empty_atlas_is_perfect_health_no_files() {
        let c = db();
        let r = atlasScore(&json!({}), &c).unwrap();
        assert_eq!(r["file_count"], json!(0));
        assert_eq!(r["project"]["health"], json!(100));
    }

    #[test]
    fn custom_weights_isolate_fanin_and_top_limits_plus_ties_by_path() {
        let c = db();
        c.execute_batch("
            INSERT INTO code_nodes (id,origin,path,type,parent_id,span_json) VALUES
              ('x.ts@atlas','atlas','x.ts','file',NULL,NULL),
              ('y.ts@atlas','atlas','y.ts','file',NULL,NULL),
              ('z.ts@atlas','atlas','z.ts','file',NULL,NULL);
            INSERT INTO code_edges (origin,from_node,to_node,type) VALUES
              ('atlas','ix1','x.ts@atlas','imports'), ('atlas','ix2','x.ts@atlas','imports'),
              ('atlas','ix3','x.ts@atlas','imports'), ('atlas','ix4','x.ts@atlas','imports'),
              ('atlas','ix5','x.ts@atlas','imports'),
              ('atlas','iy1','y.ts@atlas','imports'), ('atlas','iy2','y.ts@atlas','imports'),
              ('atlas','iy3','y.ts@atlas','imports'), ('atlas','iy4','y.ts@atlas','imports'),
              ('atlas','iy5','y.ts@atlas','imports'),
              ('atlas','iz1','z.ts@atlas','imports');
        ").unwrap();
        let r = atlasScore(&json!({ "weights": { "fanin": 1, "cx": 0, "churn": 0, "uncov": 0 }, "top": 2 }), &c).unwrap();
        let top = r["top_risk"].as_array().unwrap();
        assert_eq!(top.len(), 2); // top=2 truncation
        // x.ts and y.ts tie on fan_in (5 each) — same risk, tie broken by path ascending.
        assert_eq!(top[0]["path"], json!("x.ts"));
        assert_eq!(top[1]["path"], json!("y.ts"));
        assert_eq!(top[0]["risk"], top[1]["risk"]);
        assert_eq!(r["weights"]["churn"], json!(0));
    }
}
