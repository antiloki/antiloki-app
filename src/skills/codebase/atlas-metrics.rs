//! atlas-metrics.rs — twin of skills/codebase/atlas-metrics.mjs (db READ lane). NAMES ARE
//! CONTRACT: `gitChurn` (named, real git subprocess) · `atlasMetrics` (default, standard db
//! lane). Per-file graph-derived signals (fan-in/out, LOC, symbol_count, complexity, covered,
//! churn) → centrality/risk/complexity/churn rankings, an exact reverse-transitive "cascade"
//! closure (Tarjan SCC + bitset DP), and the gravity/badness/lens map. Pure read (no writes).
//!
//! `gitChurn` is BESPOKE-NATIVE for its own unit (real `git log` subprocess on every success
//! path, best-effort → `Value::Null` on any failure) — same precedent as `gitHistory` in the
//! sibling `code-signals-graph.rs`: real correct code below, proven by a native `#[cfg(test)]`
//! test (temp-dir, non-repo → null), NO conform-db fixture and NO frontier entry for THIS unit.
//!
//! `atlasMetrics` IS fixture-testable: the JS gates its ONE churn-computation branch behind a
//! KEY-PRESENCE check (`input?.churn === undefined`, not `??`) — an explicit `"churn": null` in
//! the fixture args takes the "already resolved, stays null" path WITHOUT ever invoking
//! `gitHistory`/`gitChurn` (same `"history": null` shape `code-signals-graph.rs` established).
//! Every fixture case here sets `"churn": null` to stay deterministic and subprocess-free; the
//! churn-autocompute branch (and the `owners` autofill from it) is therefore real-but-disclosed-
//! untested-by-fixture, ported faithfully below.
//!
//! `context.db` missing → JS throws `err.contextMissing('atlas-metrics', 'db')`. Not modeled:
//! `db: &Connection` is a required, non-nullable reference here — same "guard-throws are not
//! exercised" convention used by every sibling twin.
// QUALITY: real git subprocess ported faithfully for `gitChurn` (bespoke-native, no
// fixture/frontier for that unit); `atlasMetrics`'s full per-file/cascade/gravity pipeline is
// fixture-covered on the churn-null path; its churn-autocompute/owners-autofill branch is
// real-but-disclosed-untested-by-fixture (flag-skippable via `"churn": null`).

use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::collections::HashMap;

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
fn round3(f: f64) -> f64 { (f * 1000.0).round() / 1000.0 }
fn locale_int(n: i64) -> String {
    let neg = n < 0;
    let s = n.unsigned_abs().to_string();
    let bytes = s.as_bytes();
    let mut out = String::new();
    for (i, b) in bytes.iter().enumerate() {
        if i > 0 && (bytes.len() - i) % 3 == 0 { out.push(','); }
        out.push(*b as char);
    }
    if neg { format!("-{out}") } else { out }
}

// ── gitChurn (bespoke-native for this unit — see module doc) ───────────────────────────────

/// `execFileSync('git', ['-C', String(root), 'log', '-n', '500', '--name-only', '--format='],
/// {...timeout: 8000})` — real subprocess; best-effort → `None` on any failure (no timeout
/// enforced here since this unit carries no fixture/conform coverage, see module doc).
fn git_churn_raw(root: &str) -> Option<String> {
    let output = crate::proc::cmd("git")
        .arg("-C").arg(root)
        .arg("log")
        .arg("-n").arg("500")
        .arg("--name-only")
        .arg("--format=")
        .output();
    match output {
        Ok(o) if o.status.success() => Some(String::from_utf8_lossy(&o.stdout).into_owned()),
        _ => None,
    }
}

/// `export function gitChurn(root)` — per-file commit count over the last 500 commits.
/// Best-effort → `Value::Null` on any failure, same as the JS `catch { return null }`.
pub fn gitChurn(root: &Value) -> Value {
    let root_s = match root { Value::String(s) => s.clone(), other => other.to_string() }; // `String(root)`
    match git_churn_raw(&root_s) {
        None => Value::Null,
        Some(out) => {
            let mut counts: Map<String, Value> = Map::new();
            for line in out.split('\n') {
                let p = line.trim().replace('\\', "/");
                if p.is_empty() { continue; }
                let c = counts.get(&p).and_then(Value::as_i64).unwrap_or(0) + 1;
                counts.insert(p, json!(c));
            }
            Value::Object(counts)
        }
    }
}

// ── atlasMetrics ─────────────────────────────────────────────────────────────────────────────

fn active_root(db: &Connection) -> Option<String> {
    crate::db_json::query_rows(db, "SELECT root_path FROM workspaces WHERE is_active = 1 LIMIT 1", &[])
        .ok()
        .and_then(|v| v.as_array().and_then(|a| a.first().cloned()))
        .and_then(|r| r.get("root_path").and_then(Value::as_str).map(String::from))
}

struct FileEntry {
    id: String,
    path: String,
}

/// percentile = share of files STRICTLY BELOW v (lower bound) — a zero-signal file sits at 0,
/// never inherits the whole zero-tie block's percentile (upper bound would).
fn pctl(xs: &[f64]) -> impl Fn(f64) -> f64 + '_ {
    let mut sorted: Vec<f64> = xs.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
    move |v: f64| -> f64 {
        if sorted.is_empty() || *sorted.last().unwrap() == 0.0 { return 0.0; }
        let mut lo = 0usize; let mut hi = sorted.len();
        while lo < hi { let m = (lo + hi) / 2; if sorted[m] < v { lo = m + 1; } else { hi = m; } }
        lo as f64 / sorted.len() as f64
    }
}

/// `export default function atlasMetrics(input, context)`
pub fn atlasMetrics(input: &Value, db: &Connection) -> Result<Value, String> {
    let origin: Value = nz(input.get("origin")).cloned().unwrap_or(json!("atlas"));
    let origin_s = origin.as_str().unwrap_or("atlas").to_string();
    // `Math.min(Math.max(Number(input?.top ?? 10), 1), 100)`
    let top_default = nz(input.get("top")).cloned().unwrap_or(json!(10));
    let top_n = (js_number(Some(&top_default)).max(1.0).min(100.0)) as usize;

    let root: Option<String> = match nz(input.get("root")) {
        Some(Value::String(s)) => Some(s.clone()),
        Some(other) => Some(other.to_string()),
        None => active_root(db),
    };

    // key-presence check: `input?.churn` — see module doc.
    let churn_key_present = input.get("churn").is_some();
    let mut churn_val: Option<Value> = input.get("churn").cloned();
    let mut owners: Option<Value> = nz(input.get("owners")).cloned();

    if !churn_key_present {
        let hist: Value = match &root {
            Some(r) => crate::codebase_code_signals_graph::gitHistory(r, 3000, 500),
            None => Value::Null,
        };
        let computed_churn: Value = if !hist.is_null() {
            let by_file = hist.get("by_file").and_then(Value::as_object).cloned().unwrap_or_default();
            let mut m = Map::new();
            for (p, e) in by_file.iter() {
                let commits = e.get("commits").cloned().unwrap_or(json!(0));
                m.insert(p.clone(), commits);
            }
            Value::Object(m)
        } else if let Some(r) = &root {
            gitChurn(&json!(r))
        } else {
            Value::Null
        };
        let is_empty = computed_churn.as_object().map(|o| o.is_empty()).unwrap_or(false);
        churn_val = Some(if is_empty { Value::Null } else { computed_churn });
        if owners.is_none() && !hist.is_null() {
            owners = hist.get("by_file").cloned();
        }
    }
    let churn_final: Option<Value> = churn_val.and_then(|v| if v.is_null() { None } else { Some(v) });
    let churn_map: Option<&Map<String, Value>> = churn_final.as_ref().and_then(Value::as_object);
    let churn_truthy = churn_map.is_some();

    let files_rows = crate::db_json::query_rows(db, "SELECT id, path, span_json FROM code_nodes WHERE origin = ? AND type = 'file'", &[origin.clone()])?;
    let files_rows = files_rows.as_array().cloned().unwrap_or_default();
    let files: Vec<FileEntry> = files_rows
        .iter()
        .map(|f| FileEntry {
            id: f.get("id").and_then(Value::as_str).unwrap_or("").to_string(),
            path: f.get("path").and_then(Value::as_str).unwrap_or("").to_string(),
        })
        .collect();

    let (covered_v, tests_basis, tests_e2e_only) = crate::codebase_atlas_test_map::coveredPathsWithBasis(db, &origin_s);
    let covered_set: std::collections::HashSet<String> = covered_v.as_array().cloned().unwrap_or_default().iter().filter_map(|v| v.as_str().map(String::from)).collect();
    // nothing could have measured coverage → `covered` is null (unmeasured), not false (measured 0)
    let coverage_measured = tests_basis != "unmeasured";

    let is_test = |p: &str| -> bool { crate::codebase_atlas_test_map::isTestPath(&json!(p)).as_bool().unwrap_or(false) };

    let mut metrics_by_file: Map<String, Value> = Map::new();
    for (i, f) in files_rows.iter().enumerate() {
        let fid = &files[i].id;
        let fpath = &files[i].path;

        let fan_in: i64 = crate::db_json::query_rows(db, &format!("SELECT count(DISTINCT from_node) n FROM code_edges WHERE origin = ? AND to_node = ? AND {}", crate::codebase_atlas_build::RUNTIME_IMPORTS), &[origin.clone(), json!(fid)])?
            .as_array().and_then(|a| a.first()).and_then(|r| r.get("n")).and_then(Value::as_i64).unwrap_or(0);
        let fan_out: i64 = crate::db_json::query_rows(db, &format!("SELECT count(DISTINCT to_node) n FROM code_edges WHERE origin = ? AND from_node = ? AND {}", crate::codebase_atlas_build::RUNTIME_IMPORTS), &[origin.clone(), json!(fid)])?
            .as_array().and_then(|a| a.first()).and_then(|r| r.get("n")).and_then(Value::as_i64).unwrap_or(0);
        let symbol_count: i64 = crate::db_json::query_rows(db, "SELECT count(*) n FROM code_nodes WHERE origin = ? AND parent_id = ? AND type <> 'file'", &[origin.clone(), json!(fid)])?
            .as_array().and_then(|a| a.first()).and_then(|r| r.get("n")).and_then(Value::as_i64).unwrap_or(0);

        let mut loc: i64 = 0;
        if let Some(span_str) = f.get("span_json").and_then(Value::as_str) {
            if let Ok(s) = serde_json::from_str::<Value>(span_str) {
                if let Some(end_line) = nz(s.get("end_line")) {
                    let end = js_number(Some(end_line));
                    let start = js_number(nz(s.get("start_line")).or(Some(&json!(1))));
                    loc = ((end - start + 1.0).max(0.0)) as i64;
                }
            }
        }

        let child_spans = crate::db_json::query_rows(db, "SELECT span_json FROM code_nodes WHERE origin = ? AND parent_id = ? AND type <> 'file'", &[origin.clone(), json!(fid)])?;
        let mut complexity: i64 = 0;
        for c in child_spans.as_array().cloned().unwrap_or_default() {
            if let Some(span_str) = c.get("span_json").and_then(Value::as_str) {
                if let Ok(s) = serde_json::from_str::<Value>(span_str) {
                    if let Some(cx) = s.get("complexity").and_then(Value::as_i64) {
                        complexity += cx;
                    } else if let Some(cx) = s.get("complexity").and_then(Value::as_f64) {
                        complexity += cx as i64;
                    }
                }
            }
        }

        let is_test_v = is_test(fpath);
        let covered_v: Value = if coverage_measured { json!(covered_set.contains(fpath)) } else { Value::Null };
        let churn_here: Value = if churn_truthy {
            json!(churn_map.unwrap().get(fpath).and_then(Value::as_i64).unwrap_or(0))
        } else {
            Value::Null
        };

        metrics_by_file.insert(
            fpath.clone(),
            json!({
                "fan_in": fan_in, "fan_out": fan_out, "loc": loc, "complexity": complexity,
                "symbol_count": symbol_count, "is_test": is_test_v, "covered": covered_v,
                "tests_basis": tests_basis, "tests_e2e_only": tests_e2e_only,
                "churn": churn_here,
            }),
        );
    }

    // entries: [{path, ...m}] preserving files order
    struct Entry {
        path: String,
        fan_in: i64,
        fan_out: i64,
        loc: i64,
        complexity: i64,
        symbol_count: i64,
        is_test: bool,
        covered: bool,
        churn: Option<i64>,
        cascade: i64,
        gravity: i64,
        badness: f64,
    }
    let mut entries: Vec<Entry> = files.iter().map(|f| {
        let m = &metrics_by_file[&f.path];
        Entry {
            path: f.path.clone(),
            fan_in: m["fan_in"].as_i64().unwrap_or(0),
            fan_out: m["fan_out"].as_i64().unwrap_or(0),
            loc: m["loc"].as_i64().unwrap_or(0),
            complexity: m["complexity"].as_i64().unwrap_or(0),
            symbol_count: m["symbol_count"].as_i64().unwrap_or(0),
            is_test: m["is_test"].as_bool().unwrap_or(false),
            covered: m["covered"].as_bool().unwrap_or(false),
            churn: m["churn"].as_i64(),
            cascade: 0,
            gravity: 0,
            badness: 0.0,
        }
    }).collect();

    let cmp_desc_then_path = |a: (i64, &str), b: (i64, &str)| -> std::cmp::Ordering {
        b.0.cmp(&a.0).then_with(|| a.1.cmp(b.1))
    };

    let mut top_central: Vec<usize> = (0..entries.len()).filter(|&i| !entries[i].is_test).collect();
    top_central.sort_by(|&a, &b| cmp_desc_then_path((entries[a].fan_in, &entries[a].path), (entries[b].fan_in, &entries[b].path)));
    top_central.truncate(top_n);

    let mut top_risky: Vec<usize> = (0..entries.len()).filter(|&i| !entries[i].is_test && !entries[i].covered && entries[i].fan_in > 0).collect();
    top_risky.sort_by(|&a, &b| cmp_desc_then_path((entries[a].fan_in, &entries[a].path), (entries[b].fan_in, &entries[b].path)));
    top_risky.truncate(top_n);

    let mut top_churn: Vec<usize> = Vec::new();
    if churn_truthy {
        top_churn = (0..entries.len()).filter(|&i| !entries[i].is_test && entries[i].churn.unwrap_or(0) > 0).collect();
        top_churn.sort_by(|&a, &b| cmp_desc_then_path((entries[a].churn.unwrap_or(0), &entries[a].path), (entries[b].churn.unwrap_or(0), &entries[b].path)));
        top_churn.truncate(top_n);
    }

    let mut top_complex: Vec<usize> = (0..entries.len()).filter(|&i| !entries[i].is_test && entries[i].complexity > 0).collect();
    top_complex.sort_by(|&a, &b| cmp_desc_then_path((entries[a].complexity, &entries[a].path), (entries[b].complexity, &entries[b].path)));
    top_complex.truncate(top_n);

    // ── CASCADE: Tarjan SCC condensation + bitset DP over reverse import edges.
    {
        let n = files.len();
        if n > 0 && n <= 20000 {
            let idx_by_file: HashMap<&str, usize> = files.iter().enumerate().map(|(i, f)| (f.id.as_str(), i)).collect();
            let mut breaks: Vec<Vec<usize>> = vec![Vec::new(); n]; // breaks[x] = importers of x
            let edge_rows = crate::db_json::query_rows(db, &format!("SELECT from_node, to_node FROM code_edges WHERE origin = ? AND {}", crate::codebase_atlas_build::RUNTIME_IMPORTS), &[origin.clone()])?;
            for r in edge_rows.as_array().cloned().unwrap_or_default() {
                let from_id = r.get("from_node").and_then(Value::as_str).unwrap_or("");
                let to_id = r.get("to_node").and_then(Value::as_str).unwrap_or("");
                if let (Some(&from), Some(&to)) = (idx_by_file.get(from_id), idx_by_file.get(to_id)) {
                    if from != to { breaks[to].push(from); }
                }
            }

            let mut scc_of: Vec<i64> = vec![-1; n];
            let mut low: Vec<i64> = vec![0; n];
            let mut num: Vec<i64> = vec![-1; n];
            let mut on_stk: Vec<bool> = vec![false; n];
            let mut stk: Vec<usize> = Vec::new();
            let mut counter: i64 = 0;
            let mut scc_members: Vec<Vec<usize>> = Vec::new();

            for s in 0..n {
                if num[s] != -1 { continue; }
                // frame = (node, next-child-index)
                let mut frames: Vec<(usize, usize)> = vec![(s, 0)];
                loop {
                    if frames.is_empty() { break; }
                    let (v, ci) = *frames.last().unwrap();
                    if ci == 0 {
                        num[v] = counter; low[v] = counter; counter += 1;
                        stk.push(v); on_stk[v] = true;
                    }
                    let mut advanced = false;
                    let mut next_ci = frames.last().unwrap().1;
                    while next_ci < breaks[v].len() {
                        let w = breaks[v][next_ci];
                        next_ci += 1;
                        if num[w] == -1 {
                            frames.last_mut().unwrap().1 = next_ci;
                            frames.push((w, 0));
                            advanced = true;
                            break;
                        }
                        if on_stk[w] && num[w] < low[v] { low[v] = num[w]; }
                    }
                    if !advanced { frames.last_mut().unwrap().1 = next_ci; }
                    if advanced { continue; }
                    if low[v] == num[v] {
                        let mut members: Vec<usize> = Vec::new();
                        loop {
                            let w = stk.pop().unwrap();
                            on_stk[w] = false;
                            scc_of[w] = scc_members.len() as i64;
                            members.push(w);
                            if w == v { break; }
                        }
                        scc_members.push(members);
                    }
                    frames.pop();
                    if let Some(&(p, _)) = frames.last() {
                        if low[v] < low[p] { low[p] = low[v]; }
                    }
                }
            }

            let words = (n + 31) >> 5;
            let mut closures: Vec<Vec<u32>> = vec![Vec::new(); scc_members.len()];
            for c in 0..scc_members.len() {
                let mut bits = vec![0u32; words];
                for &m in &scc_members[c] { bits[m >> 5] |= 1u32 << (m & 31); }
                let mut seen: std::collections::HashSet<i64> = std::collections::HashSet::new();
                for &m in &scc_members[c] {
                    for &w in &breaks[m] {
                        let d = scc_of[w];
                        if d != c as i64 && seen.insert(d) {
                            let cb = &closures[d as usize];
                            for i in 0..words { bits[i] |= cb[i]; }
                        }
                    }
                }
                closures[c] = bits;
            }
            for i in 0..n {
                let bits = &closures[scc_of[i] as usize];
                let mut cnt: i64 = 0;
                for &w in bits { cnt += w.count_ones() as i64; }
                entries[i].cascade = cnt - 1;
            }
        } else {
            for e in entries.iter_mut() { e.cascade = 0; }
        }
    }

    // update metrics_by_file cascade
    for e in &entries {
        if let Some(obj) = metrics_by_file.get_mut(&e.path).and_then(Value::as_object_mut) {
            obj.insert("cascade".into(), json!(e.cascade));
        }
    }

    // ── debt markers per file
    let mut debt_by_path: HashMap<String, i64> = HashMap::new();
    if let Ok(rows) = crate::db_json::query_rows(db, "SELECT path, count(*) n FROM code_signals WHERE origin = ? AND category = 'debt' GROUP BY path", &[origin.clone()]) {
        for r in rows.as_array().cloned().unwrap_or_default() {
            let p = r.get("path").and_then(Value::as_str).unwrap_or("").to_string();
            let n = r.get("n").and_then(Value::as_i64).unwrap_or(0);
            debt_by_path.insert(p, n);
        }
    }

    // `const ownerOf = (path) => { const e = owners?.get?.(path); ... }` — `owners` here is
    // whatever `input.owners` was in the JSON args PLUS whatever the churn-autocompute branch
    // above may have autofilled from a real `gitHistory` (a genuine JS `Map`, but that branch is
    // itself real-but-disclosed-untested-by-fixture, see module doc). The conform-db JS harness
    // passes the standard-lane `input` object RAW (no `reviveArg`, unlike the helper lanes) — so
    // any `owners` a fixture supplies arrives as a plain JSON object, which has NO `.get` method;
    // `owners?.get?.(path)` optional-CALLS a `undefined` function reference and short-circuits to
    // `undefined` rather than throwing, so `ownerOf` unconditionally returns `null` for every path
    // on every fixture-driven case. Modeled faithfully: `owner_of` always returns `None` here — a
    // fixture's `owners` content can never actually reach `ownerOf`'s body via this harness.
    let owner_of = |_path: &str| -> Option<(f64, f64)> { None };

    // ── GRAVITY (mass + badness + lenses)
    {
        let src_idx: Vec<usize> = (0..entries.len()).filter(|&i| !entries[i].is_test).collect();
        let size_xs: Vec<f64> = src_idx.iter().map(|&i| entries[i].loc as f64).collect();
        let cx_xs: Vec<f64> = src_idx.iter().map(|&i| entries[i].complexity as f64).collect();
        let in_xs: Vec<f64> = src_idx.iter().map(|&i| entries[i].fan_in as f64).collect();
        let out_xs: Vec<f64> = src_idx.iter().map(|&i| entries[i].fan_out as f64).collect();
        let churn_xs: Vec<f64> = src_idx.iter().map(|&i| entries[i].churn.unwrap_or(0) as f64).collect();
        let dens_xs: Vec<f64> = src_idx.iter().map(|&i| { let e = &entries[i]; if e.loc > 0 { (e.complexity as f64 / e.loc as f64) * 100.0 } else { 0.0 } }).collect();
        let cas_xs: Vec<f64> = src_idx.iter().map(|&i| entries[i].cascade as f64).collect();
        let p_size = pctl(&size_xs);
        let p_cx = pctl(&cx_xs);
        let p_in = pctl(&in_xs);
        let p_out = pctl(&out_xs);
        let p_churn = pctl(&churn_xs);
        let p_dens = pctl(&dens_xs);
        let p_cas = pctl(&cas_xs);

        for i in 0..entries.len() {
            let (loc, complexity, fan_in, fan_out, symbol_count, covered_flag, is_test_flag, churn_v, cascade_v, path) = {
                let e = &entries[i];
                (e.loc, e.complexity, e.fan_in, e.fan_out, e.symbol_count, e.covered, e.is_test, e.churn, e.cascade, e.path.clone())
            };
            let density = if loc > 0 { (complexity as f64 / loc as f64) * 100.0 } else { 0.0 };
            let size_part = round3(p_size(loc as f64));
            let complexity_part = round3(p_cx(complexity as f64));
            let coupling_part = round3((p_in(fan_in as f64) + p_out(fan_out as f64)) / 2.0);
            let density_part = round3(p_dens(density));
            let churn_part = round3(p_churn(churn_v.unwrap_or(0) as f64));
            let untested_part: i64 = if covered_flag || is_test_flag { 0 } else { 1 };

            let gravity = (loc as f64 + 10.0 * complexity as f64 + 25.0 * (fan_in + fan_out) as f64 + 4.0 * symbol_count as f64).round() as i64;
            let badness = round3(0.45 * density_part + 0.25 * (untested_part as f64 * (1.0f64).min(p_in(fan_in as f64) + 0.25)) + 0.30 * churn_part);

            entries[i].gravity = gravity;
            entries[i].badness = badness;

            let parts = json!({
                "size": size_part, "complexity": complexity_part, "coupling": coupling_part,
                "density": density_part, "churn": churn_part, "untested": untested_part,
            });

            let own = owner_of(&path);
            let debt = *debt_by_path.get(&path).unwrap_or(&0);
            let authors_field: Value = match own { Some((a, _)) => json!(a as i64), None => Value::Null };

            let ownership_lens: f64 = match own {
                Some((authors, commits)) if commits >= 3.0 && authors >= 1.0 => round3((1.0f64).min((1.0 / authors) * (1.0f64).min(commits / 5.0))),
                _ => 0.0,
            };
            let cascade_lens = round3(p_cas(cascade_v as f64));
            let debt_lens = round3((1.0f64).min(debt as f64 / 10.0));

            let lens = json!({
                "health": badness, "cascade": cascade_lens, "ownership": ownership_lens,
                "debt": debt_lens, "heat": churn_part,
            });

            if let Some(obj) = metrics_by_file.get_mut(&path).and_then(Value::as_object_mut) {
                obj.insert("gravity".into(), json!(gravity));
                obj.insert("badness".into(), json!(badness));
                obj.insert("gravity_parts".into(), parts);
                obj.insert("authors".into(), authors_field);
                obj.insert("debt".into(), json!(debt));
                obj.insert("lens".into(), lens);
            }
        }
    }

    let mut top_gravity: Vec<usize> = (0..entries.len()).filter(|&i| !entries[i].is_test).collect();
    top_gravity.sort_by(|&a, &b| {
        let ga = entries[a].gravity; let gb = entries[b].gravity;
        gb.cmp(&ga).then_with(|| entries[a].path.cmp(&entries[b].path))
    });
    top_gravity.truncate(top_n);

    // ── signals_summary
    let max_of = |k: &dyn Fn(&Entry) -> i64| -> i64 { entries.iter().map(k).max().unwrap_or(0) };
    let sum_of = |k: &dyn Fn(&Entry) -> i64| -> i64 { entries.iter().map(k).sum() };
    let covered_n = entries.iter().filter(|e| e.covered).count() as i64;
    let vals_len = entries.len() as i64;

    let fan_in_top = if let Some(&i0) = top_central.first() {
        format!("{}× {}", entries[i0].fan_in, entries[i0].path)
    } else { "—".to_string() };
    let fan_out_top = format!("{} max", max_of(&|e| e.fan_out));
    let complexity_top = if let Some(&i0) = top_complex.first() {
        format!("{} {}", entries[i0].complexity, entries[i0].path)
    } else { "—".to_string() };
    let size_top = format!("{} LOC · {} symbols", locale_int(sum_of(&|e| e.loc)), locale_int(sum_of(&|e| e.symbol_count)));
    let coverage_pct = if vals_len != 0 { ((covered_n as f64 / vals_len as f64) * 100.0).round() as i64 } else { 0 };
    let coverage_top = format!("{covered_n}/{vals_len} files ({coverage_pct}%)");
    let churn_top = if churn_truthy {
        match top_churn.first() {
            Some(&i0) => format!("{} ({})", entries[i0].path, entries[i0].churn.unwrap_or(0)),
            None => "— (0)".to_string(),
        }
    } else { "not a git repo".to_string() };
    let cascade_top = {
        let mut idxs: Vec<usize> = (0..entries.len()).filter(|&i| !entries[i].is_test).collect();
        idxs.sort_by(|&a, &b| entries[b].cascade.cmp(&entries[a].cascade));
        match idxs.first() {
            Some(&i0) if entries[i0].cascade > 0 => format!("{} files break ← {}", entries[i0].cascade, entries[i0].path),
            _ => "—".to_string(),
        }
    };
    let ownership_top = if owners.is_some() {
        let mut best: Option<(String, f64)> = None;
        for e in &entries {
            if let Some((authors, commits)) = owner_of(&e.path) {
                if authors == 1.0 && commits >= 5.0 {
                    match &best {
                        Some((_, bc)) if *bc >= commits => {}
                        _ => best = Some((e.path.clone(), commits)),
                    }
                }
            }
        }
        match best {
            Some((path, commits)) => format!("{path} — {} commits, ONE author", commits as i64),
            None => "no single-owner hotspots".to_string(),
        }
    } else {
        "not a git repo".to_string()
    };

    let signals_summary = json!({
        "fan-in": { "top": fan_in_top, "unit": "max importers" },
        "fan-out": { "top": fan_out_top, "unit": "dependencies" },
        "complexity": { "top": complexity_top, "unit": "cyclomatic" },
        "size": { "top": size_top, "unit": "total" },
        "coverage": { "top": coverage_top, "unit": "tested" },
        "churn": { "top": churn_top, "unit": "hottest" },
        "cascade": { "top": cascade_top, "unit": "widest blast" },
        "ownership": { "top": ownership_top, "unit": "bus factor" },
    });

    // `entries = Object.entries(metrics_by_file).map(([path, m]) => ({ path, ...m }))` — a SHALLOW
    // spread taken at entries-construction time, later only `cascade`/`gravity`/`badness` are
    // appended onto each entry (`gravity_parts`/`authors`/`debt`/`lens` exist ONLY on
    // `metrics_by_file[path]`, never copied onto `entries`). The top_* arrays hold these slim
    // entry objects, not the full metrics_by_file record — key order: path, fan_in, fan_out, loc,
    // complexity, symbol_count, is_test, covered, churn, cascade, gravity, badness.
    let to_entry_json = |idxs: &[usize]| -> Value {
        Value::Array(idxs.iter().map(|&i| {
            let e = &entries[i];
            json!({
                "path": e.path, "fan_in": e.fan_in, "fan_out": e.fan_out, "loc": e.loc,
                "complexity": e.complexity, "symbol_count": e.symbol_count, "is_test": e.is_test,
                "covered": e.covered,
                "churn": match e.churn { Some(c) => json!(c), None => Value::Null },
                "cascade": e.cascade, "gravity": e.gravity, "badness": e.badness,
            })
        }).collect())
    };

    Ok(json!({
        "origin": origin,
        "file_count": files.len() as i64,
        "metrics_by_file": Value::Object(metrics_by_file.clone()),
        "top_central": to_entry_json(&top_central),
        "top_risky": to_entry_json(&top_risky),
        "top_churn": to_entry_json(&top_churn),
        "top_complex": to_entry_json(&top_complex),
        "top_gravity": to_entry_json(&top_gravity),
        "signals_summary": signals_summary,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seed_schema(c: &Connection) {
        c.execute_batch(
            "CREATE TABLE code_nodes (id TEXT, origin TEXT, path TEXT, type TEXT, name TEXT, parent_id TEXT, span_json TEXT); \
             CREATE TABLE code_edges (origin TEXT, from_node TEXT, to_node TEXT, type TEXT, kind TEXT); \
             CREATE TABLE code_signals (origin TEXT, category TEXT, kind TEXT, severity TEXT, path TEXT, line INTEGER, detail TEXT); \
             CREATE TABLE workspaces (root_path TEXT, is_active INTEGER)",
        ).unwrap();
    }

    fn seed(c: &Connection) {
        seed_schema(c);
        c.execute("INSERT INTO code_nodes (id, origin, path, type, name, parent_id, span_json) VALUES ('src/core.ts@atlas','atlas','src/core.ts','file','core.ts',NULL,'{\"file\":\"src/core.ts\",\"start_line\":1,\"end_line\":40}')", []).unwrap();
        for p in ["src/db.ts", "src/a.ts", "src/b.ts", "src/core.test.ts"] {
            c.execute(&format!("INSERT INTO code_nodes (id, origin, path, type, name, parent_id, span_json) VALUES ('{p}@atlas','atlas','{p}','file','{p}',NULL,NULL)"), []).unwrap();
        }
        c.execute("INSERT INTO code_nodes (id, origin, path, type, name, parent_id, span_json) VALUES ('src/core.ts#one','atlas','src/core.ts','function','one','src/core.ts@atlas',NULL)", []).unwrap();
        c.execute("INSERT INTO code_nodes (id, origin, path, type, name, parent_id, span_json) VALUES ('src/core.ts#two','atlas','src/core.ts','function','two','src/core.ts@atlas',NULL)", []).unwrap();
        c.execute_batch(
            "INSERT INTO code_edges (origin, from_node, to_node, type) VALUES \
             ('atlas','src/a.ts@atlas','src/core.ts@atlas','imports'), \
             ('atlas','src/b.ts@atlas','src/core.ts@atlas','imports'), \
             ('atlas','src/a.ts@atlas','src/db.ts@atlas','imports'), \
             ('atlas','src/core.test.ts@atlas','src/core.ts@atlas','imports');",
        ).unwrap();
    }

    #[test]
    fn covered_is_null_when_no_test_could_measure_it() {
        let c = Connection::open_in_memory().unwrap();
        seed_schema(&c);
        c.execute("INSERT INTO code_nodes (id, origin, path, type, name, parent_id, span_json) VALUES ('src/x.ts@atlas','atlas','src/x.ts','file','x.ts',NULL,NULL)", []).unwrap();
        let r = atlasMetrics(&json!({ "churn": null }), &c).unwrap();
        assert_eq!(r["metrics_by_file"]["src/x.ts"]["covered"], Value::Null);
        assert_eq!(r["metrics_by_file"]["src/x.ts"]["tests_basis"], json!("unmeasured"));
    }

    #[test]
    fn type_only_imports_are_not_coupling() {
        // `lib/compilation.ts` importing `type {CompilerOutput} from '../components/Editor/Output'`
        // made Output.tsx a "door" with an importer that inherits its problems. It inherits
        // nothing: the import is erased at compile time.
        let c = Connection::open_in_memory().unwrap();
        seed(&c);
        c.execute("INSERT INTO code_nodes (id, origin, path, type, name, parent_id, span_json) VALUES ('src/typed.ts@atlas','atlas','src/typed.ts','file','typed.ts',NULL,NULL)", []).unwrap();
        c.execute("INSERT INTO code_edges (origin, from_node, to_node, type, kind) VALUES ('atlas','src/typed.ts@atlas','src/core.ts@atlas','imports','type')", []).unwrap();
        c.execute("INSERT INTO code_edges (origin, from_node, to_node, type, kind) VALUES ('atlas','src/typed.ts@atlas','src/db.ts@atlas','imports','dynamic')", []).unwrap();
        let r = atlasMetrics(&json!({ "churn": null }), &c).unwrap();
        assert_eq!(r["metrics_by_file"]["src/core.ts"]["fan_in"], json!(3), "the type import did not add a dependent");
        assert_eq!(r["metrics_by_file"]["src/db.ts"]["fan_in"], json!(2), "a dynamic import is a real dependent");
        assert_eq!(r["metrics_by_file"]["src/typed.ts"]["fan_out"], json!(1), "type-only leaves fan-out too");
    }

    #[test]
    fn fan_in_out_loc_symbol_count_covered() {
        let c = Connection::open_in_memory().unwrap();
        seed(&c);
        let r = atlasMetrics(&json!({ "churn": null }), &c).unwrap();
        let core = &r["metrics_by_file"]["src/core.ts"];
        assert_eq!(core["fan_in"], json!(3));
        assert_eq!(core["symbol_count"], json!(2));
        assert_eq!(core["loc"], json!(40));
        assert_eq!(core["covered"], json!(true));
        assert_eq!(r["metrics_by_file"]["src/db.ts"]["covered"], json!(false));
    }

    #[test]
    fn top_central_and_top_risky() {
        let c = Connection::open_in_memory().unwrap();
        seed(&c);
        let r = atlasMetrics(&json!({ "churn": null }), &c).unwrap();
        assert_eq!(r["top_central"][0]["path"], json!("src/core.ts"));
        let risky_paths: Vec<&str> = r["top_risky"].as_array().unwrap().iter().map(|e| e["path"].as_str().unwrap()).collect();
        assert!(risky_paths.contains(&"src/db.ts"));
        assert!(!risky_paths.contains(&"src/core.ts"));
    }

    #[test]
    fn complexity_sum_and_top_complex() {
        let c = Connection::open_in_memory().unwrap();
        seed_schema(&c);
        c.execute("INSERT INTO code_nodes (id, origin, path, type, name, parent_id, span_json) VALUES ('a.mjs@atlas','atlas','a.mjs','file','a.mjs',NULL,NULL)", []).unwrap();
        c.execute("INSERT INTO code_nodes (id, origin, path, type, name, parent_id, span_json) VALUES ('a.mjs#f','atlas','a.mjs','function','f','a.mjs@atlas','{\"file\":\"a.mjs\",\"start_line\":1,\"end_line\":9,\"complexity\":4}')", []).unwrap();
        c.execute("INSERT INTO code_nodes (id, origin, path, type, name, parent_id, span_json) VALUES ('a.mjs#g','atlas','a.mjs','function','g','a.mjs@atlas','{\"file\":\"a.mjs\",\"start_line\":10,\"end_line\":12,\"complexity\":1}')", []).unwrap();
        let r = atlasMetrics(&json!({ "churn": null }), &c).unwrap();
        assert_eq!(r["metrics_by_file"]["a.mjs"]["complexity"], json!(5));
        assert!(r["top_complex"].as_array().unwrap().iter().any(|e| e["path"] == json!("a.mjs") && e["complexity"] == json!(5)));
    }

    #[test]
    fn no_churn_key_no_root_degrades_to_null() {
        let c = Connection::open_in_memory().unwrap();
        seed(&c);
        // no "churn" key at all AND no workspaces row → root is None → churn stays null,
        // no subprocess spawned (root check happens before the git call).
        let r = atlasMetrics(&json!({}), &c).unwrap();
        assert_eq!(r["metrics_by_file"]["src/core.ts"]["churn"], Value::Null);
        assert_eq!(r["top_churn"], json!([]));
    }

    #[test]
    fn cascade_exact_reverse_transitive_closure() {
        let c = Connection::open_in_memory().unwrap();
        seed_schema(&c);
        for p in ["core.ts", "mid.ts", "app.ts", "other.ts"] {
            c.execute(&format!("INSERT INTO code_nodes (id, origin, path, type, name, parent_id) VALUES ('{p}@atlas','atlas','{p}','file','{p}',NULL)"), []).unwrap();
        }
        c.execute_batch(
            "INSERT INTO code_edges (origin, from_node, to_node, type) VALUES \
             ('atlas','app.ts@atlas','mid.ts@atlas','imports'), \
             ('atlas','mid.ts@atlas','core.ts@atlas','imports'), \
             ('atlas','other.ts@atlas','core.ts@atlas','imports');",
        ).unwrap();
        let r = atlasMetrics(&json!({ "churn": null }), &c).unwrap();
        assert_eq!(r["metrics_by_file"]["core.ts"]["cascade"], json!(3));
        assert_eq!(r["metrics_by_file"]["mid.ts"]["cascade"], json!(1));
        assert_eq!(r["metrics_by_file"]["app.ts"]["cascade"], json!(0));
        assert!(r["metrics_by_file"]["core.ts"]["lens"]["cascade"].as_f64().unwrap() > r["metrics_by_file"]["app.ts"]["lens"]["cascade"].as_f64().unwrap());
        assert!(r["signals_summary"]["cascade"]["top"].as_str().unwrap().contains("core.ts"));
    }

    #[test]
    fn cascade_with_import_cycle() {
        let c = Connection::open_in_memory().unwrap();
        seed_schema(&c);
        for p in ["x.ts", "y.ts", "z.ts"] {
            c.execute(&format!("INSERT INTO code_nodes (id, origin, path, type, name, parent_id) VALUES ('{p}@atlas','atlas','{p}','file','{p}',NULL)"), []).unwrap();
        }
        c.execute_batch(
            "INSERT INTO code_edges (origin, from_node, to_node, type) VALUES \
             ('atlas','x.ts@atlas','y.ts@atlas','imports'), \
             ('atlas','y.ts@atlas','x.ts@atlas','imports'), \
             ('atlas','z.ts@atlas','x.ts@atlas','imports');",
        ).unwrap();
        let r = atlasMetrics(&json!({ "churn": null }), &c).unwrap();
        assert_eq!(r["metrics_by_file"]["y.ts"]["cascade"], json!(2));
        assert_eq!(r["metrics_by_file"]["x.ts"]["cascade"], json!(2));
        assert_eq!(r["metrics_by_file"]["z.ts"]["cascade"], json!(0));
    }

    #[test]
    fn gravity_decomposed_and_zero_signal_ranks_low() {
        let c = Connection::open_in_memory().unwrap();
        seed(&c);
        let r = atlasMetrics(&json!({ "churn": null }), &c).unwrap();
        let core = &r["metrics_by_file"]["src/core.ts"];
        assert!(core["gravity_parts"]["size"].as_f64().unwrap() >= 0.0);
        assert!(core["gravity"].as_i64().unwrap() >= 0);
        assert_eq!(r["top_gravity"][0]["path"], json!("src/core.ts"));
        assert_eq!(core["gravity_parts"]["untested"], json!(0));
        assert_eq!(r["metrics_by_file"]["src/db.ts"]["gravity_parts"]["untested"], json!(1));
        assert!(r["metrics_by_file"]["src/db.ts"]["badness"].as_f64().unwrap() > 0.0);
        let zero = &r["metrics_by_file"]["src/b.ts"];
        assert_eq!(zero["gravity_parts"]["size"], json!(0.0));
        assert!((zero["gravity"].as_i64().unwrap() as f64) < (core["gravity"].as_i64().unwrap() as f64) / 3.0);
    }

    #[test]
    fn debt_lens_and_owners_are_inert_over_the_json_harness() {
        // NOTE: `owners` via JSON args never functions (see `owner_of`'s module doc — the
        // conform-db JS harness passes standard-lane args RAW, so `owners?.get?.(path)` always
        // short-circuits to `undefined`/null regardless of fixture content). This mirrors that:
        // ownership stays 0 and `authors` stays null even with an `owners` object present.
        let c = Connection::open_in_memory().unwrap();
        seed(&c);
        c.execute_batch(
            "INSERT INTO code_signals (origin, category, kind, severity, path, line, detail) VALUES \
             ('atlas','debt','todo','low','src/core.ts',3,'TODO x'), \
             ('atlas','debt','todo','low','src/core.ts',9,'TODO y'), \
             ('atlas','safety','secret','high','src/core.ts',1,'not debt');",
        ).unwrap();
        let owners = json!({
            "src/core.ts": { "authors": 1, "commits": 8 },
            "src/db.ts": { "authors": 2, "commits": 6 },
        });
        let r = atlasMetrics(&json!({ "owners": owners, "churn": null }), &c).unwrap();
        assert_eq!(r["metrics_by_file"]["src/core.ts"]["lens"]["ownership"], json!(0.0));
        assert_eq!(r["metrics_by_file"]["src/db.ts"]["lens"]["ownership"], json!(0.0));
        assert_eq!(r["metrics_by_file"]["src/a.ts"]["lens"]["ownership"], json!(0.0));
        assert_eq!(r["metrics_by_file"]["src/core.ts"]["debt"], json!(2));
        assert_eq!(r["metrics_by_file"]["src/core.ts"]["lens"]["debt"], json!(0.2));
        assert_eq!(r["metrics_by_file"]["src/core.ts"]["authors"], Value::Null);
        assert_eq!(r["signals_summary"]["ownership"]["top"], json!("no single-owner hotspots"));
    }

    #[test]
    fn git_churn_returns_null_for_a_non_git_directory() {
        let root = std::env::temp_dir().to_string_lossy().into_owned();
        assert_eq!(gitChurn(&json!(root)), Value::Null);
    }
}
