//! engineering.rs — twin of skills/constitution/_helpers/engineering.mjs (helper module). NAMES
//! ARE CONTRACT: `pctOf` (no-db helper) · `gitHistory` (bespoke-native, see below) · `transitiveOf`
//! + `coChangePairs` (Map-arg → transitive class, see below) · `computeEngineering` (no-db
//! helper, the SSOT scoring pass). THE DETERMINISTIC ENGINEERING MODEL: given a codebase's atlas
//! signals + git history + law-compliance, produce per-unit engineering axes/health/knowledge map.
//!
//! `ENGINEERING_VERSION` (JS `export const ENGINEERING_VERSION = 'v2'`) is plain data, not a
//! function — not a census unit; its value ('v2') is inlined below wherever `computeEngineering`
//! embeds it into `summary.engineering_version`.
//!
//! GIT SUBPROCESS — BESPOKE-NATIVE (playbook's "not conditionally avoidable" rule,
//! code-git-info.rs / code-authors.rs precedent): unlike code-authors.mjs's `__log` test seam,
//! `gitHistory(root)` has NO input flag that selects a git-free path — EVERY success path
//! unconditionally shells `execFileSync('git', ...)`. Ported below as a real `git` subprocess
//! call (`std::process::Command`, mirroring the exact arg list), proven correct via `#[cfg(test)]`
//! native tests against a real temp-dir git repo (code-git-info.rs's precedent) — NO conform-db
//! fixture, NO frontier entry (per the task's disclosed-untested-vs-bespoke-native split: this
//! function's ENTIRE success path is the subprocess, so it fails the "conditionally reachable"
//! test that would let it ride the standard/no-db lane with a disclosed-untested branch).
//! Doubly so: the JS return shape `{ perFile: Map, commits }` is itself not JSON-serializable
//! (a `Map`) — matching the playbook's "transitive" class on the RETURN side too. Modeled here as
//! a plain Rust struct (`GitHistoryResult`), not `serde_json::Value`, since no dispatch arm can
//! ever call it generically.
//!
//! MAP-ARG → TRANSITIVE CLASS (dep-resolve.rs's `resolveSpecifier` Set-arg precedent, same
//! reasoning applied to a real `Map`, for which conform-db's `reviveArg` has no marker at all —
//! not even the `__set` marker Sets get): `transitiveOf(adj, start)` and
//! `coChangePairs(commits, scopeSet, adj, minCount, top)` both take `adj`, a real JS `Map<string,
//! string[]>` built by callers (constitution-judge.mjs: `new Map()` + `.set()`/`.get()`). JSON has
//! no Map representation and the harness cannot revive one, so — like `resolveSpecifier` — these
//! get real, correct `pub fn`s (for `names_ok`) with natural Rust signatures (`&HashMap<String,
//! Vec<String>>` for `adj`), proven via `#[cfg(test)]` unit tests mirroring the two real call
//! sites in constitution-judge.mjs/constitution-neighborhood.mjs (positional defaults 3/20, and
//! explicit 3/12), but NO conform-db fixture/arm — only a `frontier` entry with `class:
//! "transitive"`.
//!
//! `computeEngineering(units)` MUTATES each input unit object in place (adds `archetype`, `axes`,
//! `health`, `ai_readability` in the first pass; `pct_repo`/`pct_folder` in the second) and
//! returns `{ summary, cohort }` where `cohort` is the SAME (now-mutated) objects, filtered to
//! `!is_test`, in original order. The Rust twin below reconstructs this by working on a
//! `Vec<Map<String, Value>>` clone of the input and inserting the new keys in the EXACT JS
//! insertion order (gotcha 1): `archetype, axes, health, ai_readability` then, in the second pass,
//! `pct_repo, pct_folder`.
// QUALITY: no-db pure scoring pass + one bespoke-native git-log parser + two Map-arg transitive
// helpers, all in one file (mixed-lane precedent: only the risky/non-serializable UNITS are
// excluded from the fixture lane, not the whole file).

use serde_json::{json, Map, Value};
use std::collections::{HashMap, HashSet, VecDeque};

// ── standard helper functions (playbook) ────────────────────────────────────────────────────
fn truthy(v: Option<&Value>) -> bool { match v { None|Some(Value::Null)=>false, Some(Value::Bool(b))=>*b, Some(Value::Number(n))=>n.as_f64().map_or(false,|f|f!=0.0&&!f.is_nan()), Some(Value::String(s))=>!s.is_empty(), Some(_)=>true } }
fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) } // JS `??` right operand
fn js_number(v: Option<&Value>) -> f64 { match v { None=>f64::NAN, Some(Value::Null)=>0.0, Some(Value::Bool(b))=>if *b {1.0} else {0.0}, Some(Value::Number(n))=>n.as_f64().unwrap_or(f64::NAN), Some(Value::String(s))=>{let t=s.trim(); if t.is_empty(){0.0}else{t.parse().unwrap_or(f64::NAN)}}, Some(_)=>f64::NAN } } // = JS Number(v)

// JS `Math.round`: floor(x + 0.5) — rounds HALF UP (Rust arithmetic naturally propagates NaN).
fn js_round(x: f64) -> f64 { (x + 0.5).floor() }
// `Math.max`/`Math.min`: NaN-poisoning (any NaN operand → NaN), unlike Rust's f64::max/min.
fn js_math_max(a: f64, b: f64) -> f64 { if a.is_nan() || b.is_nan() { f64::NAN } else if a > b { a } else { b } }
fn js_math_min(a: f64, b: f64) -> f64 { if a.is_nan() || b.is_nan() { f64::NAN } else if a < b { a } else { b } }
// `Math.max(0, Math.min(100, Math.round(n)))` — a finite result is always integer-valued.
fn clamp100(n: f64) -> f64 { js_math_max(0.0, js_math_min(100.0, js_round(n))) }
// A computed JS number embedded directly as a JSON value: integer-render when whole (gotcha 2),
// and NaN/Infinity fall through to serde_json's own Number::from_f64 → Value::Null (matches
// JSON.stringify(NaN/Infinity) === null).
fn js_num(n: f64) -> Value { if n.is_finite() && n.fract() == 0.0 && n.abs() < 9e15 { json!(n as i64) } else { json!(n) } }
fn opt_num(v: Option<f64>) -> Value { match v { Some(f) => js_num(f), None => Value::Null } }

fn includes_str(v: Option<&Value>, needle: &str) -> bool {
    // `(u.smells ?? []).includes(needle)`
    nz(v).and_then(Value::as_array).map_or(false, |a| a.iter().any(|x| x.as_str() == Some(needle)))
}
fn is_exact_zero(v: Option<&Value>) -> bool {
    // JS `x === 0` — strict equality, only a real Number(0) qualifies.
    matches!(v, Some(Value::Number(n)) if n.as_f64() == Some(0.0))
}

// ── pctOf — percentile rank of v within a sorted numeric array (0..1); null-safe. ────────────
fn pct_of_f64(sorted: &[f64], v: Option<f64>) -> Option<f64> {
    let v = v?;
    if sorted.is_empty() { return None; }
    let mut lo: i64 = 0;
    let mut hi: i64 = sorted.len() as i64;
    while lo < hi {
        let mid = (lo + hi) >> 1;
        if sorted[mid as usize] <= v { lo = mid + 1; } else { hi = mid; }
    }
    let n = sorted.len() as i64;
    Some(if n == 1 { 0.0 } else { (lo - 1).max(0) as f64 / (n - 1) as f64 })
}

pub fn pctOf(sorted: &Value, v: &Value) -> Value {
    if matches!(v, Value::Null) { return Value::Null; } // `v == null` (undefined arg reads as Null too)
    let arr: Vec<f64> = sorted.as_array().map(|a| a.iter().map(|x| x.as_f64().unwrap_or(f64::NAN)).collect()).unwrap_or_default();
    opt_num(pct_of_f64(&arr, Some(js_number(Some(v)))))
}

// ── gitHistory — BESPOKE-NATIVE (see module doc). Real `git` subprocess, no fixture. ─────────
pub struct GitFileEntry { pub churn: i64, pub authors: HashSet<String> }
pub struct GitHistoryResult { pub per_file: HashMap<String, GitFileEntry>, pub commits: Vec<Vec<String>> }

/// `execFileSync('git', ['-C', root, 'log', '-n', '2000', '--name-only', '--format=@%an'], {...})`
/// then a one-pass parse: `@Author` lines start a new commit (pushing a new `commits` entry);
/// every other non-blank line is a changed file path (backslashes normalized to `/`), bumping
/// that file's churn + author set and appended to the CURRENT commit's file list.
pub fn gitHistory(root: &str) -> Option<GitHistoryResult> {
    let output = crate::proc::cmd("git")
        .arg("-C").arg(root)
        .arg("log").arg("-n").arg("2000").arg("--name-only").arg("--format=@%an")
        .output();
    let out = match output {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).into_owned(),
        _ => return None,
    };
    let mut map: HashMap<String, GitFileEntry> = HashMap::new();
    let mut commits: Vec<Vec<String>> = Vec::new();
    let mut author: Option<String> = None;
    let mut current: Option<usize> = None;
    for line in out.split('\n') {
        let t = line.trim();
        if t.is_empty() { continue; }
        if let Some(a) = t.strip_prefix('@') {
            author = Some(a.to_string());
            commits.push(Vec::new());
            current = Some(commits.len() - 1);
            continue;
        }
        let p = t.replace('\\', "/");
        let e = map.entry(p.clone()).or_insert_with(|| GitFileEntry { churn: 0, authors: HashSet::new() });
        e.churn += 1;
        if let Some(a) = &author { e.authors.insert(a.clone()); }
        if let Some(idx) = current { commits[idx].push(p); }
    }
    Some(GitHistoryResult { per_file: map, commits })
}

// ── transitiveOf — Map-arg → transitive class (see module doc). Real fn, no fixture. ─────────
/// `if (!adj.size) return null; BFS from start, capped at 500 seen nodes; return seen.size - 1.`
pub fn transitiveOf(adj: &HashMap<String, Vec<String>>, start: &str) -> Option<i64> {
    if adj.is_empty() { return None; }
    let mut seen: HashSet<String> = HashSet::new();
    seen.insert(start.to_string());
    let mut queue: VecDeque<String> = VecDeque::new();
    queue.push_back(start.to_string());
    while !queue.is_empty() && seen.len() < 500 {
        let cur = queue.pop_front().unwrap();
        if let Some(nexts) = adj.get(&cur) {
            for next in nexts {
                if !seen.contains(next) {
                    seen.insert(next.clone());
                    queue.push_back(next.clone());
                }
            }
        }
    }
    Some(seen.len() as i64 - 1)
}

// ── coChangePairs — Map-arg → transitive class (see module doc). Real fn, no fixture. ────────
/// Files that co-change (same commit) at least `min_count` times, WITHOUT an import edge between
/// them (`adj`), top `top` by count desc (stable — ties keep first-seen insertion order, mirroring
/// JS `Map` iteration order + `Array.prototype.sort`'s stability).
pub fn coChangePairs(
    commits: &[Vec<String>],
    scope_set: &HashSet<String>,
    adj: &HashMap<String, Vec<String>>,
    min_count: i64,
    top: i64,
) -> Vec<(String, String, i64)> {
    let mut order: Vec<String> = Vec::new();
    let mut counts: HashMap<String, i64> = HashMap::new();
    for files in commits {
        // `[...new Set(files.filter((p) => scopeSet.has(p)))]` — unique, first-seen order.
        let mut seen_in_commit: HashSet<&str> = HashSet::new();
        let mut in_scope: Vec<String> = Vec::new();
        for p in files {
            if scope_set.contains(p) && seen_in_commit.insert(p.as_str()) {
                in_scope.push(p.clone());
            }
        }
        if in_scope.len() < 2 || in_scope.len() > 25 { continue; }
        in_scope.sort();
        for i in 0..in_scope.len() {
            for j in (i + 1)..in_scope.len() {
                let k = format!("{}|{}", in_scope[i], in_scope[j]);
                match counts.get_mut(&k) {
                    Some(c) => { *c += 1; }
                    None => { counts.insert(k.clone(), 1); order.push(k); }
                }
            }
        }
    }
    let linked = |a: &str, b: &str| -> bool {
        adj.get(a).map_or(false, |v| v.iter().any(|x| x == b)) || adj.get(b).map_or(false, |v| v.iter().any(|x| x == a))
    };
    let mut entries: Vec<(String, String, i64)> = order
        .into_iter()
        .filter(|k| counts[k] >= min_count)
        .filter_map(|k| {
            let (a, b) = k.split_once('|')?;
            let (a, b) = (a.to_string(), b.to_string());
            if linked(&a, &b) { None } else { let n = counts[&format!("{a}|{b}")]; Some((a, b, n)) }
        })
        .collect();
    entries.sort_by(|x, y| y.2.cmp(&x.2)); // stable desc by count
    entries.truncate(top.max(0) as usize);
    entries
}

// ── computeEngineering — the SSOT scoring pass (standard no-db unit: fn(units) -> {summary, cohort}) ──
/// The revision of the deterministic INSTRUMENT: the axis formulas and weights here, plus what the
/// atlas extraction calls a definition (components/hooks recognised since v3) and an import edge
/// (type-only excluded, dynamic/re-export included since v3). Stamped on
/// `review_snapshots.engineering_version`; two snapshots are comparable only when this AND
/// `constitution_hash` (the laws) match — a delta across revisions is the instrument moving, not
/// the code. Bump it whenever a number would change for unchanged source.
pub const ENGINEERING_VERSION: &str = "v3";
const AXES_AVG_ORDER: [&str; 7] = ["compliance", "defects", "tests", "structure", "coupling", "cohesion", "change"];
const WEIGHTS: [(&str, f64); 7] = [("compliance", 0.25), ("defects", 0.18), ("tests", 0.16), ("structure", 0.15), ("coupling", 0.12), ("cohesion", 0.06), ("change", 0.08)];

fn get_f64(m: &Map<String, Value>, key: &str) -> Option<f64> { nz(m.get(key)).and_then(Value::as_f64) }

fn sorted_of(units: &[Map<String, Value>], cohort_idx: &[usize], key: &str) -> Vec<f64> {
    let mut v: Vec<f64> = cohort_idx.iter().filter_map(|&i| get_f64(&units[i], key)).collect();
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    v
}

fn axis_obj(score: Value, parts: Map<String, Value>) -> Value {
    let mut o = Map::new();
    o.insert("score".into(), score);
    o.insert("parts".into(), Value::Object(parts));
    Value::Object(o)
}

pub fn computeEngineering(input: &Value) -> Value {
    let mut units: Vec<Map<String, Value>> = input.as_array().map(|a| a.iter().map(|u| u.as_object().cloned().unwrap_or_default()).collect()).unwrap_or_default();
    let cohort_idx: Vec<usize> = (0..units.len()).filter(|&i| !truthy(units[i].get("is_test"))).collect();

    let s_complexity = sorted_of(&units, &cohort_idx, "complexity");
    let s_transitive = sorted_of(&units, &cohort_idx, "transitive");
    let s_fan_in = sorted_of(&units, &cohort_idx, "fan_in");
    let s_fan_out = sorted_of(&units, &cohort_idx, "fan_out");
    let s_churn = sorted_of(&units, &cohort_idx, "churn");
    let s_loc = sorted_of(&units, &cohort_idx, "loc");

    for i in 0..units.len() {
        let pc = pct_of_f64(&s_complexity, get_f64(&units[i], "complexity"));
        let pt = pct_of_f64(&s_transitive, get_f64(&units[i], "transitive"));
        let pfi = pct_of_f64(&s_fan_in, get_f64(&units[i], "fan_in"));
        let pfo = pct_of_f64(&s_fan_out, get_f64(&units[i], "fan_out"));
        let pch = pct_of_f64(&s_churn, get_f64(&units[i], "churn"));
        let ploc = pct_of_f64(&s_loc, get_f64(&units[i], "loc"));
        let smells = units[i].get("smells").cloned();
        let god: i64 = if includes_str(smells.as_ref(), "god-file") { 1 } else { 0 };
        let cycle: i64 = if includes_str(smells.as_ref(), "cycle") { 1 } else { 0 };

        // axes.compliance
        let compliance_v = units[i].get("compliance").cloned();
        let axes_compliance = match &compliance_v {
            None | Some(Value::Null) => Value::Null,
            Some(cv) => {
                let mut parts = Map::new();
                parts.insert("law_score".into(), cv.clone());
                axis_obj(cv.clone(), parts)
            }
        };

        // axes.structure
        let cg = nz(units[i].get("cog")).cloned();
        let axes_structure = if let Some(cg) = &cg {
            let worst = js_number(cg.get("worst"));
            let max_nesting = js_number(cg.get("max_nesting"));
            let pen_cog = js_math_max(0.0, js_math_min(55.0, (worst - 8.0) * 3.0));
            let pen_nest = if max_nesting > 4.0 { js_math_min(15.0, (max_nesting - 4.0) * 5.0) } else { 0.0 };
            let pen_tr = 20.0 * pt.unwrap_or(0.0);
            let pen_god = if god == 1 { 15.0 } else { 0.0 };
            let score = clamp100(100.0 - (pen_cog + pen_nest + pen_tr + pen_god));
            let mut parts = Map::new();
            parts.insert("basis".into(), json!("cognitive"));
            if let Some(v) = cg.get("worst_block") { parts.insert("worst_block".into(), v.clone()); }
            if let Some(v) = cg.get("worst") { parts.insert("worst_cognitive".into(), v.clone()); }
            if let Some(v) = cg.get("worst_line") { parts.insert("worst_line".into(), v.clone()); }
            if let Some(v) = cg.get("max_nesting") { parts.insert("max_nesting".into(), v.clone()); }
            if let Some(v) = cg.get("max_nesting_line") { parts.insert("max_nesting_line".into(), v.clone()); }
            if let Some(v) = cg.get("nested_loops") { parts.insert("nested_loops".into(), v.clone()); }
            parts.insert("pct_transitive".into(), opt_num(pt));
            parts.insert("god_file".into(), json!(god));
            axis_obj(js_num(score), parts)
        } else if pc.is_none() && pt.is_none() {
            Value::Null
        } else {
            let score = clamp100(100.0 - (55.0 * pc.unwrap_or(0.0) + 25.0 * pt.unwrap_or(0.0) + 20.0 * god as f64));
            let mut parts = Map::new();
            parts.insert("basis".into(), json!("atlas-cx"));
            parts.insert("pct_complexity".into(), opt_num(pc));
            parts.insert("pct_transitive".into(), opt_num(pt));
            parts.insert("god_file".into(), json!(god));
            axis_obj(js_num(score), parts)
        };

        // axes.coupling
        let axes_coupling = if pfi.is_none() && pfo.is_none() {
            Value::Null
        } else {
            let score = clamp100(100.0 - (45.0 * pfi.unwrap_or(0.0) + 35.0 * pfo.unwrap_or(0.0) + 20.0 * cycle as f64));
            let mut parts = Map::new();
            parts.insert("pct_fan_in".into(), opt_num(pfi));
            parts.insert("pct_fan_out".into(), opt_num(pfo));
            parts.insert("cycle".into(), json!(cycle));
            axis_obj(js_num(score), parts)
        };

        // axes.tests
        let is_test_b = truthy(units[i].get("is_test"));
        let covered_v = units[i].get("covered").cloned();
        let axes_tests = if is_test_b || matches!(&covered_v, None | Some(Value::Null)) {
            Value::Null
        } else {
            let score = if truthy(covered_v.as_ref()) { 100.0 } else { clamp100(100.0 - (45.0 + 45.0 * pfi.unwrap_or(0.0))) };
            let mut parts = Map::new();
            parts.insert("covered".into(), covered_v.clone().unwrap_or(Value::Null));
            parts.insert("pct_fan_in".into(), opt_num(pfi));
            // what measured it — `static-import` (a test file imports the source), `inline`
            // (`#[cfg(test)]`), `lcov`. The unmeasured case never reaches here: `covered` is null.
            if let Some(b) = units[i].get("tests_basis").and_then(Value::as_str) {
                parts.insert("basis".into(), json!(b));
            }
            axis_obj(js_num(score), parts)
        };

        // axes.defects
        let findings_v = nz(units[i].get("findings")).cloned().unwrap_or(json!(0));
        let findings_high_v = nz(units[i].get("findings_high")).cloned().unwrap_or(json!(0));
        let findings_f = js_number(Some(&findings_v));
        let findings_high_f = js_number(Some(&findings_high_v));
        let defects_score = clamp100(100.0 - js_math_min(90.0, findings_f * 15.0 + findings_high_f * 10.0));
        let axes_defects = {
            let mut parts = Map::new();
            parts.insert("findings".into(), findings_v);
            parts.insert("high".into(), findings_high_v);
            axis_obj(js_num(defects_score), parts)
        };

        // axes.change
        let churn_v = nz(units[i].get("churn")).cloned();
        let churn_f = churn_v.as_ref().map(|v| js_number(Some(v))).unwrap_or(0.0);
        let axes_change = if pch.is_none() {
            Value::Null
        } else if churn_f < 3.0 {
            let mut parts = Map::new();
            parts.insert("churn".into(), churn_v.clone().unwrap_or(json!(0)));
            parts.insert("note".into(), json!("churn too low to matter yet"));
            axis_obj(json!(100), parts)
        } else {
            let score = clamp100(100.0 - 90.0 * pch.unwrap_or(0.0) * js_math_max(pc.unwrap_or(0.3), 0.3));
            let mut parts = Map::new();
            parts.insert("churn".into(), units[i].get("churn").cloned().unwrap_or(Value::Null));
            parts.insert("pct_churn".into(), opt_num(pch));
            parts.insert("pct_complexity".into(), opt_num(pc));
            axis_obj(js_num(score), parts)
        };

        // axes.cohesion
        let cohesion_v = nz(units[i].get("cohesion")).cloned();
        let axes_cohesion = match &cohesion_v {
            None => Value::Null,
            Some(cv) => {
                let cf = js_number(Some(cv));
                let score = clamp100(cf * 100.0);
                let ratio = js_round(cf * 100.0) / 100.0;
                let mut parts = Map::new();
                parts.insert("connected_ratio".into(), js_num(ratio));
                axis_obj(js_num(score), parts)
            }
        };

        let mut axes: Map<String, Value> = Map::new();
        axes.insert("compliance".into(), axes_compliance);
        axes.insert("structure".into(), axes_structure);
        axes.insert("coupling".into(), axes_coupling);
        axes.insert("tests".into(), axes_tests);
        axes.insert("defects".into(), axes_defects);
        axes.insert("change".into(), axes_change);
        axes.insert("cohesion".into(), axes_cohesion);

        // isGlue / archetype — blanks 5 axes (NOT compliance/defects) when the file is atlas-known
        // structural glue (def_count === 0) and not itself a test.
        let atlas_known_b = truthy(units[i].get("atlas_known"));
        let def_count_v = units[i].get("def_count").cloned();
        let is_glue = !is_test_b && atlas_known_b && is_exact_zero(def_count_v.as_ref());
        let archetype = if is_test_b { "test" } else if matches!(&def_count_v, None | Some(Value::Null)) { "unknown" } else if is_glue { "glue" } else { "logic" };
        if is_glue {
            axes.insert("tests".into(), Value::Null);
            axes.insert("coupling".into(), Value::Null);
            axes.insert("structure".into(), Value::Null);
            axes.insert("change".into(), Value::Null);
            axes.insert("cohesion".into(), Value::Null);
        }

        let mut sum = 0.0;
        let mut wsum = 0.0;
        for (k, w) in WEIGHTS {
            if let Some(a) = axes.get(k) {
                if !a.is_null() {
                    if let Some(score) = a.get("score").and_then(Value::as_f64) { sum += score * w; wsum += w; }
                }
            }
        }
        let health: Option<f64> = if wsum > 0.0 { Some(js_round(sum / wsum)) } else { None };
        let ai_readability: Option<f64> = if ploc.is_none() && pt.is_none() && pfo.is_none() {
            None
        } else {
            Some(clamp100(100.0 - (45.0 * ploc.unwrap_or(0.0) + 35.0 * pt.unwrap_or(0.0) + 20.0 * pfo.unwrap_or(0.0))))
        };

        units[i].insert("archetype".into(), json!(archetype));
        units[i].insert("axes".into(), Value::Object(axes));
        units[i].insert("health".into(), opt_num(health));
        units[i].insert("ai_readability".into(), opt_num(ai_readability));
    }

    // neighborhood: health percentiles vs the repo cohort and vs folder siblings.
    let health_sorted: Vec<f64> = {
        let mut v: Vec<f64> = cohort_idx.iter().filter_map(|&i| units[i].get("health").and_then(Value::as_f64)).collect();
        v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        v
    };
    let mut by_folder: HashMap<String, Vec<f64>> = HashMap::new();
    for &i in &cohort_idx {
        let folder = units[i].get("folder").and_then(Value::as_str).unwrap_or("").to_string();
        let entry = by_folder.entry(folder).or_default();
        if let Some(h) = units[i].get("health").and_then(Value::as_f64) { entry.push(h); }
    }
    for arr in by_folder.values_mut() { arr.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal)); }

    for i in 0..units.len() {
        let health_v = units[i].get("health").and_then(Value::as_f64);
        let pct_repo = pct_of_f64(&health_sorted, health_v);
        let folder = units[i].get("folder").and_then(Value::as_str).unwrap_or("").to_string();
        let folder_arr = by_folder.get(&folder).cloned().unwrap_or_default();
        let pct_folder = pct_of_f64(&folder_arr, health_v);
        units[i].insert("pct_repo".into(), opt_num(pct_repo));
        units[i].insert("pct_folder".into(), opt_num(pct_folder));
    }

    // ── summary: meta scores + the knowledge map ──────────────────────────────────────────────
    let mean = |arr: &[f64]| -> Option<f64> { if arr.is_empty() { None } else { Some(js_round(arr.iter().sum::<f64>() / arr.len() as f64)) } };
    let known: i64 = cohort_idx.iter().filter(|&&i| truthy(units[i].get("atlas_known"))).count() as i64;
    let cohort_len = cohort_idx.len() as i64;

    let mut axes_avg = Map::new();
    for ax in AXES_AVG_ORDER {
        let vals: Vec<f64> = cohort_idx.iter().filter_map(|&i| units[i].get("axes").and_then(|a| a.get(ax)).and_then(|a| if a.is_null() { None } else { a.get("score") }).and_then(Value::as_f64)).collect();
        axes_avg.insert(ax.into(), opt_num(mean(&vals)));
    }

    let mut hotspots: Vec<(Value, Value, Value, f64)> = cohort_idx.iter().filter_map(|&i| {
        let churn = nz(units[i].get("churn")).cloned()?;
        let complexity = nz(units[i].get("complexity")).cloned()?;
        let churn_f = js_number(Some(&churn));
        let complexity_f = js_number(nz(Some(&complexity)));
        let hot = churn_f * complexity_f;
        Some((units[i].get("path").cloned().unwrap_or(Value::Null), churn, complexity, hot))
    }).collect();
    hotspots.sort_by(|a, b| b.3.partial_cmp(&a.3).unwrap_or(std::cmp::Ordering::Equal));
    hotspots.truncate(10);
    let hotspots_json: Vec<Value> = hotspots.into_iter().map(|(path, churn, complexity, hot)| {
        let mut o = Map::new();
        o.insert("path".into(), path);
        o.insert("churn".into(), churn);
        o.insert("complexity".into(), complexity);
        o.insert("hot".into(), js_num(hot));
        Value::Object(o)
    }).collect();

    let mut bus_factor: Vec<(Value, Value, f64)> = cohort_idx.iter().filter_map(|&i| {
        let authors_ok = matches!(units[i].get("authors"), Some(Value::Number(n)) if n.as_f64() == Some(1.0));
        if !authors_ok { return None; }
        // `(u.fan_in ?? 0) > 0` — a missing/null fan_in defaults to 0 (excluded), NOT NaN.
        let fan_in_f = nz(units[i].get("fan_in")).map(|v| js_number(Some(v))).unwrap_or(0.0);
        if !(fan_in_f > 0.0) { return None; }
        Some((units[i].get("path").cloned().unwrap_or(Value::Null), units[i].get("fan_in").cloned().unwrap_or(Value::Null), fan_in_f))
    }).collect();
    bus_factor.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal));
    bus_factor.truncate(10);
    let bus_factor_json: Vec<Value> = bus_factor.into_iter().map(|(path, fan_in, _)| {
        let mut o = Map::new();
        o.insert("path".into(), path);
        o.insert("fan_in".into(), fan_in);
        Value::Object(o)
    }).collect();

    let god_modules: Vec<Value> = cohort_idx.iter().filter(|&&i| includes_str(units[i].get("smells"), "god-file")).take(10).map(|&i| units[i].get("path").cloned().unwrap_or(Value::Null)).collect();
    let islands: Vec<Value> = cohort_idx.iter().filter(|&&i| {
        nz(units[i].get("smells")).and_then(Value::as_array).map_or(false, |a| a.iter().any(|s| s.as_str() == Some("orphan") || s.as_str() == Some("unreferenced-file")))
    }).take(10).map(|&i| units[i].get("path").cloned().unwrap_or(Value::Null)).collect();

    let mut hidden: Vec<(Value, Value, Value, f64)> = cohort_idx.iter().filter_map(|&i| {
        let transitive = nz(units[i].get("transitive")).cloned()?;
        let loc = nz(units[i].get("loc")).cloned()?;
        let loc_f = js_number(Some(&loc));
        let transitive_f = js_number(Some(&transitive));
        if !(loc_f < 120.0 && transitive_f >= 15.0) { return None; }
        Some((units[i].get("path").cloned().unwrap_or(Value::Null), loc, transitive, transitive_f))
    }).collect();
    hidden.sort_by(|a, b| b.3.partial_cmp(&a.3).unwrap_or(std::cmp::Ordering::Equal));
    hidden.truncate(10);
    let hidden_json: Vec<Value> = hidden.into_iter().map(|(path, loc, transitive, _)| {
        let mut o = Map::new();
        o.insert("path".into(), path);
        o.insert("loc".into(), loc);
        o.insert("transitive".into(), transitive);
        Value::Object(o)
    }).collect();

    let health_mean = mean(&cohort_idx.iter().filter_map(|&i| units[i].get("health").and_then(Value::as_f64)).collect::<Vec<_>>());
    let ai_readability_mean = mean(&cohort_idx.iter().filter_map(|&i| units[i].get("ai_readability").and_then(Value::as_f64)).collect::<Vec<_>>());
    let determinism: Value = if cohort_len > 0 { js_num(js_round((known as f64 / cohort_len as f64) * 100.0)) } else { Value::Null };

    let mut determinism_parts = Map::new();
    determinism_parts.insert("atlas_known".into(), json!(known));
    determinism_parts.insert("unknown".into(), json!(cohort_len - known));
    // the archetype split — how many files were scored on all axes vs on laws+defects only
    for a in ["logic", "glue", "test"] {
        determinism_parts.insert(a.into(), json!(units.iter().filter(|u| u.get("archetype").and_then(Value::as_str) == Some(a)).count()));
    }

    let mut knowledge = Map::new();
    knowledge.insert("hotspots".into(), json!(hotspots_json));
    knowledge.insert("bus_factor".into(), json!(bus_factor_json));
    knowledge.insert("god_modules".into(), json!(god_modules));
    knowledge.insert("islands".into(), json!(islands));
    knowledge.insert("hidden_complexity".into(), json!(hidden_json));

    let mut summary = Map::new();
    summary.insert("engineering_version".into(), json!(ENGINEERING_VERSION));
    summary.insert("health".into(), opt_num(health_mean));
    summary.insert("determinism".into(), determinism);
    summary.insert("determinism_parts".into(), Value::Object(determinism_parts));
    summary.insert("ai_readability".into(), opt_num(ai_readability_mean));
    summary.insert("axes_avg".into(), Value::Object(axes_avg));
    summary.insert("knowledge".into(), Value::Object(knowledge));

    let cohort_json: Vec<Value> = cohort_idx.iter().map(|&i| Value::Object(units[i].clone())).collect();

    let mut out = Map::new();
    out.insert("summary".into(), Value::Object(summary));
    out.insert("cohort".into(), json!(cohort_json));
    Value::Object(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pct_of_null_and_empty_and_midpoint() {
        assert_eq!(pctOf(&json!([1, 2, 3]), &Value::Null), Value::Null);
        assert_eq!(pctOf(&json!([]), &json!(5)), Value::Null);
        assert_eq!(pctOf(&json!([10]), &json!(10)), json!(0));
        assert_eq!(pctOf(&json!([1, 2, 3, 4]), &json!(3)), json!(2.0 / 3.0));
        assert_eq!(pctOf(&json!([1, 2, 3, 4]), &json!(4)), json!(1));
        assert_eq!(pctOf(&json!([1, 2, 3, 4]), &json!(0)), json!(0));
    }

    #[test]
    fn transitive_of_bfs_and_empty_adj() {
        let mut adj: HashMap<String, Vec<String>> = HashMap::new();
        adj.insert("a".into(), vec!["b".into(), "c".into()]);
        adj.insert("b".into(), vec!["d".into()]);
        assert_eq!(transitiveOf(&adj, "a"), Some(3)); // b, c, d reachable
        assert_eq!(transitiveOf(&adj, "z"), Some(0)); // no outgoing edges from z
        assert_eq!(transitiveOf(&HashMap::new(), "a"), None);
    }

    #[test]
    fn co_change_pairs_defaults_and_explicit_args_match_real_call_sites() {
        // mirrors constitution-judge.mjs: coChangePairs(git.commits, scopedSet, adj) — defaults 3/20
        let commits = vec![
            vec!["a.ts".to_string(), "b.ts".to_string()],
            vec!["a.ts".to_string(), "b.ts".to_string()],
            vec!["a.ts".to_string(), "b.ts".to_string()],
            vec!["c.ts".to_string()], // < 2 files in scope, skipped
        ];
        let scope: HashSet<String> = ["a.ts", "b.ts", "c.ts"].iter().map(|s| s.to_string()).collect();
        let adj: HashMap<String, Vec<String>> = HashMap::new(); // no import edges
        let r = coChangePairs(&commits, &scope, &adj, 3, 20);
        assert_eq!(r, vec![("a.ts".to_string(), "b.ts".to_string(), 3)]);

        // below min_count threshold → excluded
        let r2 = coChangePairs(&commits, &scope, &adj, 4, 20);
        assert!(r2.is_empty());

        // an import edge between a/b excludes the pair even though it co-changes enough
        let mut adj2: HashMap<String, Vec<String>> = HashMap::new();
        adj2.insert("a.ts".into(), vec!["b.ts".into()]);
        // mirrors constitution-neighborhood.mjs: coChangePairs(git.commits, shown, fwd, 3, 12)
        let r3 = coChangePairs(&commits, &scope, &adj2, 3, 12);
        assert!(r3.is_empty());
    }

    fn base_unit(path: &str, churn: i64, complexity: i64) -> Value {
        json!({
            "path": path, "folder": "src", "is_test": false, "atlas_known": true, "def_count": 5,
            "loc": 100, "fan_in": 2, "fan_out": 3, "complexity": complexity, "transitive": 4,
            "covered": true, "churn": churn, "authors": 2, "findings": 0, "findings_high": 0,
            "smells": [], "compliance": 100
        })
    }

    #[test]
    fn compute_engineering_change_axis_absolute_first() {
        let units = json!([base_unit("src/young.js", 2, 14), base_unit("src/hotspot.js", 12, 14)]);
        let r = computeEngineering(&units);
        let cohort = r["cohort"].as_array().unwrap();
        assert_eq!(cohort[0]["axes"]["change"]["score"], json!(100));
        assert!(cohort[1]["axes"]["change"]["score"].as_i64().unwrap() < 100);
        assert_eq!(cohort[1]["axes"]["change"]["parts"]["churn"], json!(12));
    }

    #[test]
    fn compute_engineering_tests_axis_is_absent_when_nothing_could_measure_it() {
        // covered = null means "no instrument" — not "measured 0". The axis drops out and health
        // is renormalised over what remains, the same way a glue file already works.
        let mut a = base_unit("src/a.ts", 1, 3);
        a["covered"] = Value::Null;
        let mut b = base_unit("src/b.ts", 1, 3);
        b["covered"] = json!(0);
        b["tests_basis"] = json!("static-import");
        let r = computeEngineering(&json!([a, b]));
        let ua = &r["cohort"][0];
        let ub = &r["cohort"][1];
        assert_eq!(ua["axes"]["tests"], Value::Null, "unmeasured → no axis");
        assert_eq!(ub["axes"]["tests"]["parts"]["covered"], json!(0), "measured 0 → a real score");
        assert_eq!(ub["axes"]["tests"]["parts"]["basis"], json!("static-import"));
        assert!(ua["health"].as_f64().unwrap() > ub["health"].as_f64().unwrap(), "a penalty for an unmeasured axis would be a lie");
        assert_eq!(r["summary"]["determinism_parts"]["logic"], json!(2));
        assert_eq!(r["summary"]["determinism_parts"]["glue"], json!(0));
    }

    #[test]
    fn compute_engineering_glue_blanks_five_axes_but_not_compliance_or_defects() {
        let mut glue = base_unit("src/index.ts", 1, 3);
        glue["def_count"] = json!(0);
        let units = json!([glue]);
        let r = computeEngineering(&units);
        let u = &r["cohort"][0];
        assert_eq!(u["archetype"], json!("glue"));
        assert_eq!(u["axes"]["tests"], Value::Null);
        assert_eq!(u["axes"]["coupling"], Value::Null);
        assert_eq!(u["axes"]["structure"], Value::Null);
        assert_eq!(u["axes"]["change"], Value::Null);
        assert_eq!(u["axes"]["cohesion"], Value::Null);
        assert_ne!(u["axes"]["compliance"], Value::Null);
        assert_ne!(u["axes"]["defects"], Value::Null);
    }

    #[test]
    fn compute_engineering_test_units_are_excluded_from_cohort_but_still_scored() {
        let mut t = base_unit("src/a.test.ts", 1, 3);
        t["is_test"] = json!(true);
        let units = json!([base_unit("src/a.ts", 1, 3), t]);
        let r = computeEngineering(&units);
        assert_eq!(r["cohort"].as_array().unwrap().len(), 1);
        assert_eq!(r["cohort"][0]["path"], json!("src/a.ts"));
        assert_eq!(r["summary"]["engineering_version"], json!(ENGINEERING_VERSION));
    }

    #[test]
    fn compute_engineering_summary_knowledge_map_hotspots_and_bus_factor() {
        let mut a = base_unit("src/a.ts", 12, 20);
        a["authors"] = json!(1);
        a["fan_in"] = json!(5);
        let b = base_unit("src/b.ts", 2, 3);
        let units = json!([a, b]);
        let r = computeEngineering(&units);
        let hotspots = r["summary"]["knowledge"]["hotspots"].as_array().unwrap();
        assert_eq!(hotspots[0]["path"], json!("src/a.ts"));
        assert_eq!(hotspots[0]["hot"], json!(240));
        let bus = r["summary"]["knowledge"]["bus_factor"].as_array().unwrap();
        assert_eq!(bus.len(), 1);
        assert_eq!(bus[0]["path"], json!("src/a.ts"));
    }

    #[test]
    fn compute_engineering_cognitive_structure_axis_uses_cog_when_present() {
        let mut a = base_unit("src/a.ts", 1, 3);
        a["cog"] = json!({ "worst": 20, "worst_block": "fn foo", "worst_line": 10, "max_nesting": 6, "max_nesting_line": 12, "nested_loops": 1 });
        let units = json!([a]);
        let r = computeEngineering(&units);
        let structure = &r["cohort"][0]["axes"]["structure"];
        assert_eq!(structure["parts"]["basis"], json!("cognitive"));
        assert_eq!(structure["parts"]["worst_cognitive"], json!(20));
        assert_eq!(structure["parts"]["max_nesting"], json!(6));
        assert!(structure["score"].as_i64().unwrap() < 100);
    }

    // real `git` subprocess (bespoke-native, see module doc) against a real temp-dir repo.
    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!("antiloki-engineering-rs-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }
    fn git_ok(args: &[&str], cwd: &std::path::Path) {
        let out = crate::proc::cmd("git").args(args).current_dir(cwd).output().unwrap();
        assert!(out.status.success(), "git {:?} failed: {}", args, String::from_utf8_lossy(&out.stderr));
    }

    #[test]
    fn git_history_parses_real_repo_churn_and_authors() {
        let dir = temp_dir("git-history");
        git_ok(&["init", "-q"], &dir);
        std::fs::write(dir.join("a.txt"), "1").unwrap();
        git_ok(&["add", "-A"], &dir);
        git_ok(&["-c", "user.name=Alice", "-c", "user.email=alice@x.com", "-c", "commit.gpgsign=false", "commit", "-q", "-m", "first"], &dir);
        std::fs::write(dir.join("a.txt"), "2").unwrap();
        std::fs::write(dir.join("b.txt"), "1").unwrap();
        git_ok(&["add", "-A"], &dir);
        git_ok(&["-c", "user.name=Bob", "-c", "user.email=bob@x.com", "-c", "commit.gpgsign=false", "commit", "-q", "-m", "second"], &dir);

        let root = dir.to_string_lossy().into_owned();
        let r = gitHistory(&root).expect("real repo should parse");
        assert_eq!(r.commits.len(), 2);
        let a = r.per_file.get("a.txt").expect("a.txt tracked");
        assert_eq!(a.churn, 2);
        assert!(a.authors.contains("Alice") && a.authors.contains("Bob"));
        let b = r.per_file.get("b.txt").expect("b.txt tracked");
        assert_eq!(b.churn, 1);
        assert!(b.authors.contains("Bob") && !b.authors.contains("Alice"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn git_history_returns_none_for_non_git_directory() {
        let dir = temp_dir("git-history-non-git");
        let root = dir.to_string_lossy().into_owned();
        assert!(gitHistory(&root).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
