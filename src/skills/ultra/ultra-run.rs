//! ultra-run.rs — twin of skills/ultra/ultra-run.mjs. NAMES ARE CONTRACT: `ultraRun` (default,
//! standard db lane). ULTRA REVIEW's deterministic score substrate: surveys a TERRITORY (a real
//! filesystem walk under `root`, or an explicit file list), scores it from counted signals only
//! (atlas metrics/smells via the already-converted `codebase_atlas_metrics`/`codebase_atlas_smells`
//! twins, a real `git log` history pass, the bugfinder ledger), persists one `ultra_runs` row +
//! one `ultra_scores` row per unit, then re-aggregates via the already-converted
//! `ultra_ultra_report::ultraReport` twin.
//!
//! BESPOKE-NATIVE (real, non-mocked side effects on every success path, no conform-db fixture
//! possible — same precedent as `codebase/atlas-metrics.rs`'s `gitChurn` and
//! `codebase/code-signals-graph.rs`'s `gitHistory`):
//! - `walk_folder` does a REAL recursive `std::fs::read_dir` walk of the filesystem when the
//!   caller doesn't pass an explicit `files` list (the JS `walkFolder` over `node:fs`).
//! - `git_history` (local, private — a DIFFERENT function from the already-converted
//!   `codebase_code_signals_graph::gitHistory` twin; this module's own copy mirrors the JS
//!   module's own private `gitHistory`, which uses different git args: `-n 2000 --format=@%an`,
//!   no sha/no churn-limit) shells out to a real `git log` subprocess, best-effort → `None` on
//!   any failure (non-repo, missing binary), proven by a native test against a real non-git temp
//!   dir.
//! - the per-unit `content_hash`/LOC-fallback reads REAL file bytes off disk
//!   (`std::fs::read_to_string`) for whatever the walk/caller scoped in.
//!
//! Everything else (the axes/health formula, percentile ranks, the transitive-import BFS, the
//! db persistence, the co-change pairing) is pure/deterministic and IS exercised by native tests
//! against a real temp dir + an in-memory `judgments`-attached db, mirroring the DEPENDENCY SCOPE
//! precedent (`atlas-smells.rs`'s module doc): the atlas/smells/report calls below go through the
//! REAL already-converted twins (`codebase_atlas_metrics::atlasMetrics`,
//! `codebase_atlas_smells::atlasSmells`, `ultra_ultra_report::ultraReport`,
//! `ultra__helpers_schema::ensureUltraSchema`, `engine_workspace_root::getWorkspaceRoot`) — never
//! re-implemented inline.
//!
//! `resolve(input.root)` (`node:path` win32 `resolve`) is ported as a best-effort join +
//! `.`/`..` component collapse (`resolve_root_arg`), NOT byte-identical to Node's full algorithm
//! (documented gap, same "disclosed win32-only/simplified path" convention as
//! `engine/workspace-root.rs`) — every call site here only ever anchors an already-mostly-formed
//! root, so the divergence is confined to pathological relative-root inputs this substrate never
//! actually passes.
//!
//! `context.db` missing → JS throws `err.contextMissing('ultra-run', 'context.db')`. Not modeled
//! — same "guard-throws are not exercised" convention used by every sibling twin (`db: &Connection`
//! is a required, non-nullable reference here).
// QUALITY: real fs walk + real git subprocess ported faithfully (bespoke-native, no
// fixture/frontier for those units); the axes/health/percentile/persistence pipeline is a
// faithful deterministic port, native-tested end to end against a real temp dir + in-memory db.

use regex::Regex;
use rusqlite::Connection;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

pub const FORMULA_VERSION: &str = "v1";
const MAX_FILES_DEFAULT: i64 = 400;

// Delegates to the ONE capability list (ast-parse's adapter table filtered to grammars that
// actually have a walk profile). This used to be a private copy of
// `(mjs|cjs|js|jsx|ts|tsx|py)`; four skills each held one, none ever gained `rs`, and the
// review pass walked straight past every Rust file while reporting success.
fn code_re() -> &'static Regex { crate::codebase_ast_parse::analyzable_ext_regex() }
fn test_dir_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"(?:^|/)(?:tests?|__tests__)/").unwrap())
}
fn test_file_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"\.(?:test|spec)\.\w+$").unwrap())
}
fn walk_skip(name: &str) -> bool {
    matches!(name, "node_modules" | ".git" | "dist" | "build" | "coverage" | ".antiloki" | ".next" | "out")
}

fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) }
fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}

fn is_win_sep(c: u8) -> bool { c == b'/' || c == b'\\' }
fn is_absolute_like(p: &str) -> bool {
    let b = p.as_bytes();
    if b.is_empty() { return false; }
    if is_win_sep(b[0]) { return true; }
    b.len() > 2 && b[0].is_ascii_alphabetic() && b[1] == b':' && is_win_sep(b[2])
}

/// `resolve(input.root)` — see module doc for the disclosed simplification.
fn resolve_root_arg(root: &str) -> String {
    let p = Path::new(root);
    let abs = if is_absolute_like(root) { p.to_path_buf() } else { std::env::current_dir().unwrap_or_default().join(p) };
    let mut out: Vec<std::ffi::OsString> = Vec::new();
    for comp in abs.components() {
        use std::path::Component::*;
        match comp {
            CurDir => {}
            ParentDir => { out.pop(); }
            other => out.push(other.as_os_str().to_os_string()),
        }
    }
    let mut result = PathBuf::new();
    for c in out { result.push(c); }
    result.to_string_lossy().into_owned()
}

fn join_root(root: &str, p: &str) -> PathBuf {
    if is_absolute_like(p) { PathBuf::from(p) } else { Path::new(root).join(p) }
}

/// `walkFolder(root, folderRel, cap)` — a real recursive directory walk (bespoke-native, see
/// module doc). DFS via a pop-stack, same shape as the JS (order not guaranteed identical to
/// Node's `readdirSync` across platforms — the final result is `.sort()`ed either way).
fn walk_folder(root: &str, folder_rel: &str, cap: i64) -> Vec<String> {
    let start = if folder_rel == "." { PathBuf::from(root) } else { Path::new(root).join(folder_rel) };
    let mut out: Vec<String> = Vec::new();
    let mut stack: Vec<PathBuf> = vec![start];
    let cap3 = (cap.max(0) * 3) as usize;
    while let Some(dir) = stack.pop() {
        if out.len() >= cap3 { break; }
        let entries = match std::fs::read_dir(&dir) { Ok(e) => e, Err(_) => continue };
        for e in entries.flatten() {
            let ft = match e.file_type() { Ok(f) => f, Err(_) => continue };
            let name = e.file_name().to_string_lossy().into_owned();
            if ft.is_dir() {
                if !walk_skip(&name) && !name.starts_with('.') { stack.push(e.path()); }
            } else if code_re().is_match(&name) {
                let rel = e.path().strip_prefix(root).map(|p| p.to_path_buf()).unwrap_or_else(|_| e.path());
                out.push(rel.to_string_lossy().replace('\\', "/"));
            }
        }
    }
    out.sort();
    out
}

struct FileHist { churn: i64, authors: HashSet<String> }
struct GitHist { per_file: HashMap<String, FileHist>, commits: Vec<Vec<String>> }

/// `gitHistory(root)` — the JS module's OWN local git pass (distinct from
/// `codebase_code_signals_graph::gitHistory`): `git -C <root> log -n 2000 --name-only
/// --format=@%an`. Real subprocess, best-effort → `None` on any failure. Bespoke-native, see
/// module doc.
fn git_history(root: &str) -> Option<GitHist> {
    let output = crate::proc::cmd("git")
        .arg("-C").arg(root)
        .arg("log")
        .arg("-n").arg("2000")
        .arg("--name-only")
        .arg("--format=@%an")
        .output();
    match output {
        Ok(o) if o.status.success() => {
            let text = String::from_utf8_lossy(&o.stdout).into_owned();
            let mut map: HashMap<String, FileHist> = HashMap::new();
            let mut commits: Vec<Vec<String>> = Vec::new();
            let mut author: Option<String> = None;
            let mut cur_idx: Option<usize> = None;
            for line in text.split('\n') {
                let t = line.trim();
                if t.is_empty() { continue; }
                if let Some(rest) = t.strip_prefix('@') {
                    author = Some(rest.to_string());
                    commits.push(Vec::new());
                    cur_idx = Some(commits.len() - 1);
                    continue;
                }
                let p = t.replace('\\', "/");
                let e = map.entry(p.clone()).or_insert_with(|| FileHist { churn: 0, authors: HashSet::new() });
                e.churn += 1;
                if let Some(a) = &author { e.authors.insert(a.clone()); }
                if let Some(ci) = cur_idx { commits[ci].push(p); }
            }
            Some(GitHist { per_file: map, commits })
        }
        _ => None,
    }
}

/// `coChangePairs` — files that co-change without an import edge, over the SAME git pass.
fn co_change_pairs(commits: &[Vec<String>], scope_set: &HashSet<String>, adj: &HashMap<String, Vec<String>>, min_count: i64, top: usize) -> Vec<Value> {
    let mut order: Vec<String> = Vec::new();
    let mut counts: HashMap<String, i64> = HashMap::new();
    for files in commits {
        let mut in_scope: Vec<String> = Vec::new();
        let mut seen: HashSet<&str> = HashSet::new();
        for p in files {
            if scope_set.contains(p) && seen.insert(p.as_str()) { in_scope.push(p.clone()); }
        }
        if in_scope.len() < 2 || in_scope.len() > 25 { continue; }
        in_scope.sort();
        for i in 0..in_scope.len() {
            for j in (i + 1)..in_scope.len() {
                let k = format!("{}|{}", in_scope[i], in_scope[j]);
                if !counts.contains_key(&k) { order.push(k.clone()); }
                *counts.entry(k).or_insert(0) += 1;
            }
        }
    }
    let linked = |a: &str, b: &str| -> bool {
        adj.get(a).map(|v| v.iter().any(|x| x == b)).unwrap_or(false)
            || adj.get(b).map(|v| v.iter().any(|x| x == a)).unwrap_or(false)
    };
    let mut filtered: Vec<(String, i64)> = order
        .into_iter()
        .map(|k| { let n = counts[&k]; (k, n) })
        .filter(|(_, n)| *n >= min_count)
        .filter(|(k, _)| {
            let parts: Vec<&str> = k.splitn(2, '|').collect();
            !linked(parts[0], parts.get(1).copied().unwrap_or(""))
        })
        .collect();
    filtered.sort_by(|a, b| b.1.cmp(&a.1)); // stable — ties keep insertion order, like JS .sort()
    filtered.truncate(top);
    filtered
        .into_iter()
        .map(|(k, n)| {
            let parts: Vec<&str> = k.splitn(2, '|').collect();
            json!({ "a": parts[0], "b": parts.get(1).copied().unwrap_or(""), "count": n })
        })
        .collect()
}

/// percentile rank of `v` within a SORTED ascending numeric array (0..1); null-safe.
fn pct_of(sorted: &[f64], v: Option<f64>) -> Option<f64> {
    let v = v?;
    if sorted.is_empty() { return None; }
    let mut lo = 0usize;
    let mut hi = sorted.len();
    while lo < hi {
        let mid = (lo + hi) / 2;
        if sorted[mid] <= v { lo = mid + 1; } else { hi = mid; }
    }
    if sorted.len() == 1 { Some(0.0) } else { Some((lo as i64 - 1).max(0) as f64 / (sorted.len() - 1) as f64) }
}
fn clamp100(n: f64) -> i64 { crate::js_math::js_round(n).max(0.0).min(100.0) as i64 }

struct Unit {
    path: String,
    folder: String,
    content_hash: Option<String>,
    atlas_known: bool,
    is_test: bool,
    loc: Option<i64>,
    fan_in: Option<i64>,
    fan_out: Option<i64>,
    complexity: Option<i64>,
    transitive: Option<i64>,
    covered: Option<i64>,
    churn: Option<i64>,
    authors: Option<i64>,
    findings: i64,
    findings_high: i64,
    smells: Vec<String>,
    axes: Value,
    health: Option<i64>,
    ai_readability: Option<i64>,
    pct_repo: Option<f64>,
    pct_folder: Option<f64>,
}

fn opt_i64(v: Option<&Value>) -> Option<i64> { v.and_then(|x| if x.is_null() { None } else { x.as_i64() }) }

/// `export default async function ultraRun(input, context)` — async-over-sync, see playbook
/// (this skill awaits only sync db reads/writes plus the already-converted-twin calls below).
#[allow(non_snake_case)]
pub fn ultraRun(input: &Value, db: &Connection) -> Result<Value, String> {
    crate::ultra__helpers_schema::ensureUltraSchema(db);

    let root: String = match nz(input.get("root")).and_then(Value::as_str).filter(|s| !s.is_empty()) {
        Some(r) => resolve_root_arg(r),
        None => crate::engine_workspace_root::getWorkspaceRoot(db).as_str().unwrap_or_default().to_string(),
    };
    let max_files: i64 = input.get("max_files").and_then(Value::as_i64).unwrap_or(MAX_FILES_DEFAULT);

    // ── scope ──────────────────────────────────────────────────────────────
    let files_mode = input.get("files").and_then(Value::as_array).map(|a| !a.is_empty()).unwrap_or(false);
    let mut paths: Vec<String> = if files_mode {
        input["files"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| match f {
                Value::String(s) => s.clone(),
                other => other.get("path").and_then(Value::as_str).unwrap_or("").to_string(),
            })
            .filter(|s| !s.is_empty())
            .collect()
    } else {
        let folder_rel = {
            let f = nz(input.get("folder")).and_then(Value::as_str).unwrap_or(".").trim().replace('\\', "/");
            if f.is_empty() { ".".to_string() } else { f }
        };
        walk_folder(&root, &folder_rel, max_files)
    };
    // `[...new Set(paths.filter(CODE_RE.test))]` — dedupe preserving first-seen order.
    let mut seen_paths: HashSet<String> = HashSet::new();
    let mut code_paths: Vec<String> = Vec::new();
    for p in paths.drain(..) {
        if code_re().is_match(&p) && seen_paths.insert(p.clone()) { code_paths.push(p); }
    }
    let capped: i64 = (code_paths.len() as i64 - max_files).max(0);
    let scoped: Vec<String> = code_paths
        .into_iter()
        .take(max_files.max(0) as usize)
        .filter(|p| join_root(&root, p).exists())
        .collect();

    // ── signals, cheapest sources first — every one counted, none invented ──
    let metrics_by_file: Map<String, Value> = crate::codebase_atlas_metrics::atlasMetrics(&json!({ "origin": "atlas", "top": 1, "root": root }), db)
        .ok()
        .and_then(|v| v.get("metrics_by_file").and_then(Value::as_object).cloned())
        .unwrap_or_default();

    let mut smells_by_file: HashMap<String, Vec<String>> = HashMap::new();
    if let Ok(r) = crate::codebase_atlas_smells::atlasSmells(&json!({ "origin": "atlas" }), db) {
        if let Some(smells) = r.get("smells").and_then(Value::as_array) {
            for s in smells {
                let kind = s.get("kind").and_then(Value::as_str).unwrap_or("").to_string();
                if let Some(nodes) = s.get("nodes").and_then(Value::as_array) {
                    for n in nodes {
                        let p = match n {
                            Value::String(s) => s.clone(),
                            other => other.get("path").and_then(Value::as_str).unwrap_or("").to_string(),
                        }
                        .replace('\\', "/");
                        if p.is_empty() { continue; }
                        smells_by_file.entry(p).or_default().push(kind.clone());
                    }
                }
            }
        }
    }

    // transitive import mass (hidden complexity): BFS over the file-import graph.
    let id_to_path: HashMap<String, String> = crate::db_json::query_rows(db, "SELECT id, path FROM code_nodes WHERE origin='atlas' AND type='file'", &[])?
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|r| (r.get("id").and_then(Value::as_str).unwrap_or("").to_string(), r.get("path").and_then(Value::as_str).unwrap_or("").to_string()))
        .collect();
    let mut adj: HashMap<String, Vec<String>> = HashMap::new();
    for e in crate::db_json::query_rows(db, &format!("SELECT from_node, to_node FROM code_edges WHERE origin='atlas' AND {}", crate::codebase_atlas_build::RUNTIME_IMPORTS), &[])?
        .as_array()
        .cloned()
        .unwrap_or_default()
    {
        let f = id_to_path.get(e.get("from_node").and_then(Value::as_str).unwrap_or(""));
        let t = id_to_path.get(e.get("to_node").and_then(Value::as_str).unwrap_or(""));
        if let (Some(f), Some(t)) = (f, t) {
            if f == t { continue; }
            adj.entry(f.clone()).or_default().push(t.clone());
        }
    }
    let transitive_of = |start: &str| -> Option<i64> {
        if adj.is_empty() || id_to_path.is_empty() { return None; }
        let mut seen: HashSet<String> = HashSet::new();
        seen.insert(start.to_string());
        let mut queue: std::collections::VecDeque<String> = std::collections::VecDeque::new();
        queue.push_back(start.to_string());
        while let Some(cur) = queue.pop_front() {
            if seen.len() >= 500 { break; }
            if let Some(nexts) = adj.get(&cur) {
                for next in nexts {
                    if seen.len() >= 500 { break; }
                    if !seen.contains(next) { seen.insert(next.clone()); queue.push_back(next.clone()); }
                }
            }
        }
        Some(seen.len() as i64 - 1)
    };

    let git = git_history(&root);

    let mut ledger: HashMap<String, (i64, i64)> = HashMap::new();
    if let Ok(rows) = crate::db_json::query_rows(
        db,
        "SELECT file, COUNT(*) AS n, SUM(CASE WHEN severity='high' THEN 1 ELSE 0 END) AS high FROM judgments.bugfinder_findings WHERE dropped = 0 AND file IS NOT NULL GROUP BY file",
        &[],
    ) {
        for r in rows.as_array().cloned().unwrap_or_default() {
            let file = r.get("file").and_then(Value::as_str).unwrap_or("").replace('\\', "/");
            let n = r.get("n").and_then(Value::as_i64).unwrap_or(0);
            let high = r.get("high").and_then(Value::as_i64).unwrap_or(0);
            ledger.insert(file, (n, high));
        }
    }

    // ── per-unit raw rows ─────────────────────────────────────────────────
    let mut units: Vec<Unit> = scoped
        .iter()
        .map(|p| {
            let m = metrics_by_file.get(p);
            let source = std::fs::read_to_string(join_root(&root, p)).ok();
            let content_hash = source.as_ref().map(|s| {
                let mut hasher = Sha256::new();
                hasher.update(s.as_bytes());
                hex::encode(hasher.finalize())[0..16].to_string()
            });
            let m_loc = m.and_then(|mm| mm.get("loc")).and_then(Value::as_i64).filter(|v| *v != 0);
            let loc = m_loc.or_else(|| source.as_ref().map(|s| s.split('\n').count() as i64));
            let g = git.as_ref().and_then(|gh| gh.per_file.get(p));
            let led = ledger.get(p).copied().unwrap_or((0, 0));
            let smells = smells_by_file.get(p).cloned().unwrap_or_default();
            let is_test = m
                .and_then(|mm| mm.get("is_test"))
                .and_then(Value::as_bool)
                .unwrap_or_else(|| test_dir_re().is_match(p) || test_file_re().is_match(p));
            let folder = Path::new(p).parent().map(|d| d.to_string_lossy().replace('\\', "/")).filter(|s| !s.is_empty()).unwrap_or_else(|| ".".to_string());
            Unit {
                path: p.clone(),
                folder,
                content_hash,
                atlas_known: m.is_some(),
                is_test,
                loc,
                fan_in: m.and_then(|mm| opt_i64(mm.get("fan_in"))),
                fan_out: m.and_then(|mm| opt_i64(mm.get("fan_out"))),
                complexity: m.and_then(|mm| opt_i64(mm.get("complexity"))),
                transitive: if m.is_some() { transitive_of(p) } else { None },
                covered: m.map(|mm| if truthy(mm.get("covered")) { 1 } else { 0 }),
                churn: g.map(|gg| gg.churn).or(if git.is_some() { Some(0) } else { None }),
                authors: g.map(|gg| gg.authors.len() as i64).or(if git.is_some() { Some(0) } else { None }),
                findings: led.0,
                findings_high: led.1,
                smells,
                axes: Value::Null,
                health: None,
                ai_readability: None,
                pct_repo: None,
                pct_folder: None,
            }
        })
        .collect();

    // ── cohort percentiles (scope non-test files) ────────────────────────────
    let sorted_of = |key: fn(&Unit) -> Option<i64>| -> Vec<f64> {
        let mut v: Vec<f64> = units.iter().filter(|u| !u.is_test).filter_map(|u| key(u).map(|x| x as f64)).collect();
        v.sort_by(|a, b| a.partial_cmp(b).unwrap());
        v
    };
    let s_complexity = sorted_of(|u| u.complexity);
    let s_transitive = sorted_of(|u| u.transitive);
    let s_fan_in = sorted_of(|u| u.fan_in);
    let s_fan_out = sorted_of(|u| u.fan_out);
    let s_churn = sorted_of(|u| u.churn);
    let s_loc = sorted_of(|u| u.loc);

    // ── the axes — FORMULA v1, parts recorded per unit (law 1) ──────────────
    for u in units.iter_mut() {
        let pc = pct_of(&s_complexity, u.complexity.map(|x| x as f64));
        let pt = pct_of(&s_transitive, u.transitive.map(|x| x as f64));
        let pfi = pct_of(&s_fan_in, u.fan_in.map(|x| x as f64));
        let pfo = pct_of(&s_fan_out, u.fan_out.map(|x| x as f64));
        let pch = pct_of(&s_churn, u.churn.map(|x| x as f64));
        let ploc = pct_of(&s_loc, u.loc.map(|x| x as f64));
        let god: i64 = if u.smells.iter().any(|s| s == "god-file") { 1 } else { 0 };
        let cycle: i64 = if u.smells.iter().any(|s| s == "cycle") { 1 } else { 0 };

        let mut axes = Map::new();

        if pc.is_some() || pt.is_some() {
            let score = clamp100(100.0 - (55.0 * pc.unwrap_or(0.0) + 25.0 * pt.unwrap_or(0.0) + 20.0 * god as f64));
            axes.insert("structure".into(), json!({ "score": score, "parts": { "pct_complexity": pc, "pct_transitive": pt, "god_file": god } }));
        } else {
            axes.insert("structure".into(), Value::Null);
        }

        if pfi.is_some() || pfo.is_some() {
            let score = clamp100(100.0 - (45.0 * pfi.unwrap_or(0.0) + 35.0 * pfo.unwrap_or(0.0) + 20.0 * cycle as f64));
            axes.insert("coupling".into(), json!({ "score": score, "parts": { "pct_fan_in": pfi, "pct_fan_out": pfo, "cycle": cycle } }));
        } else {
            axes.insert("coupling".into(), Value::Null);
        }

        if u.is_test || u.covered.is_none() {
            axes.insert("tests".into(), Value::Null);
        } else {
            let covered = u.covered.unwrap();
            let score = if covered != 0 { 100 } else { clamp100(100.0 - (45.0 + 45.0 * pfi.unwrap_or(0.0))) };
            axes.insert("tests".into(), json!({ "score": score, "parts": { "covered": covered, "pct_fan_in": pfi } }));
        }

        {
            let score = clamp100(100.0 - (90.0f64).min((u.findings * 15 + u.findings_high * 10) as f64));
            axes.insert("defects".into(), json!({ "score": score, "parts": { "findings": u.findings, "high": u.findings_high } }));
        }

        if pch.is_some() {
            let score = clamp100(100.0 - 90.0 * pch.unwrap() * pc.unwrap_or(0.3).max(0.3));
            axes.insert("change".into(), json!({ "score": score, "parts": { "pct_churn": pch, "pct_complexity": pc } }));
        } else {
            axes.insert("change".into(), Value::Null);
        }

        if let Some(authors) = u.authors {
            let score = if authors >= 3 { 100 } else if authors == 2 { 85 } else { clamp100(100.0 - (40.0 + 30.0 * pch.unwrap_or(0.0))) };
            axes.insert("knowledge".into(), json!({ "score": score, "parts": { "authors": authors, "pct_churn": pch } }));
        } else {
            axes.insert("knowledge".into(), Value::Null);
        }

        let w: [(&str, f64); 6] = [("defects", 0.25), ("tests", 0.2), ("structure", 0.2), ("coupling", 0.15), ("change", 0.1), ("knowledge", 0.1)];
        let mut sum = 0.0;
        let mut wsum = 0.0;
        for (k, wt) in w {
            if let Some(sc) = axes.get(k).and_then(|a| a.get("score")).and_then(Value::as_f64) {
                sum += sc * wt;
                wsum += wt;
            }
        }
        u.health = if wsum > 0.0 { Some(crate::js_math::js_round(sum / wsum) as i64) } else { None };
        u.axes = Value::Object(axes);
        u.ai_readability = if ploc.is_none() && pt.is_none() && pfo.is_none() {
            None
        } else {
            Some(clamp100(100.0 - (45.0 * ploc.unwrap_or(0.0) + 35.0 * pt.unwrap_or(0.0) + 20.0 * pfo.unwrap_or(0.0))))
        };
    }

    // health percentiles: repo cohort + folder siblings
    let mut health_sorted: Vec<f64> = units.iter().filter(|u| !u.is_test).filter_map(|u| u.health.map(|h| h as f64)).collect();
    health_sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let mut by_folder: HashMap<String, Vec<f64>> = HashMap::new();
    for u in units.iter().filter(|u| !u.is_test) {
        if let Some(h) = u.health { by_folder.entry(u.folder.clone()).or_default().push(h as f64); }
    }
    for arr in by_folder.values_mut() { arr.sort_by(|a, b| a.partial_cmp(b).unwrap()); }
    for u in units.iter_mut() {
        u.pct_repo = pct_of(&health_sorted, u.health.map(|h| h as f64));
        u.pct_folder = pct_of(by_folder.get(&u.folder).map(|v| v.as_slice()).unwrap_or(&[]), u.health.map(|h| h as f64));
    }

    // ── persist ───────────────────────────────────────────────────────────
    let mut scope = Map::new();
    scope.insert(
        "folder".into(),
        match nz(input.get("folder")) {
            Some(v) => v.clone(),
            None => if files_mode { Value::Null } else { json!(".") },
        },
    );
    if files_mode { scope.insert("files".into(), json!(scoped.len() as i64)); }
    scope.insert("capped".into(), json!(capped));

    db.execute(
        "INSERT INTO judgments.ultra_runs (source, root, scope_json, status, files, formula_version) VALUES (?, ?, ?, 'running', ?, ?)",
        rusqlite::params![
            nz(input.get("source")).and_then(Value::as_str).unwrap_or("manual"),
            root,
            serde_json::to_string(&Value::Object(scope)).unwrap(),
            units.len() as i64,
            FORMULA_VERSION,
        ],
    )
    .map_err(|e| e.to_string())?;
    let run_id = db.last_insert_rowid();

    {
        let mut ins = db
            .prepare(
                "INSERT INTO judgments.ultra_scores
                   (run_id, path, folder, content_hash, atlas_known, is_test, loc, fan_in, fan_out, complexity, transitive,
                    covered, churn, authors, findings, findings_high, smells_json, axes_json, health, ai_readability, pct_repo, pct_folder)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .map_err(|e| e.to_string())?;
        for u in &units {
            ins.execute(rusqlite::params![
                run_id,
                u.path,
                u.folder,
                u.content_hash,
                u.atlas_known as i64,
                u.is_test as i64,
                u.loc,
                u.fan_in,
                u.fan_out,
                u.complexity,
                u.transitive,
                u.covered,
                u.churn,
                u.authors,
                u.findings,
                u.findings_high,
                serde_json::to_string(&u.smells).unwrap(),
                serde_json::to_string(&u.axes).unwrap(),
                u.health,
                u.ai_readability,
                u.pct_repo,
                u.pct_folder,
            ])
            .map_err(|e| e.to_string())?;
        }
    }

    // temporal coupling over the scoped territory ($0 — same git pass)
    let scope_set: HashSet<String> = units.iter().map(|u| u.path.clone()).collect();
    let co_change: Vec<Value> = match &git {
        Some(gh) => co_change_pairs(&gh.commits, &scope_set, &adj, 3, 20),
        None => Vec::new(),
    };

    let report = crate::ultra_ultra_report::ultraReport(&json!({ "run_id": run_id }), db)?;
    let mut summary_out = report.get("summary").and_then(Value::as_object).cloned().unwrap_or_default();
    summary_out.insert("co_change".into(), Value::Array(co_change));
    let summary_val = Value::Object(summary_out);

    db.execute(
        "UPDATE judgments.ultra_runs SET status='done', totals_json=? WHERE id=?",
        rusqlite::params![serde_json::to_string(&summary_val).unwrap(), run_id],
    )
    .map_err(|e| e.to_string())?;

    let mut out = report.as_object().cloned().unwrap_or_default();
    out.insert("summary".into(), summary_val);
    let mut run_out = out.get("run").and_then(Value::as_object).cloned().unwrap_or_default();
    run_out.insert("status".into(), json!("done"));
    out.insert("run".into(), Value::Object(run_out));
    out.insert("capped".into(), json!(capped));
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn setup_db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS judgments").unwrap();
        c.execute_batch(
            "CREATE TABLE code_nodes (id TEXT, origin TEXT, path TEXT, type TEXT, name TEXT, parent_id TEXT, span_json TEXT, exported INTEGER);
             CREATE TABLE code_edges (origin TEXT, from_node TEXT, to_node TEXT, type TEXT, kind TEXT);
             CREATE TABLE code_signals (origin TEXT, category TEXT, kind TEXT, severity TEXT, path TEXT, line INTEGER, detail TEXT);
             CREATE TABLE workspaces (id INTEGER PRIMARY KEY, name TEXT, root_path TEXT, vcs TEXT, is_active INTEGER, created_at TEXT);",
        )
        .unwrap();
        c
    }

    fn temp_dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ultra-run-rs-test-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn git_history_returns_none_for_a_non_git_directory() {
        let dir = temp_dir("non-git");
        assert!(git_history(&dir.to_string_lossy()).is_none());
    }

    #[test]
    fn walk_folder_walks_real_files_skips_node_modules_and_dotdirs() {
        let dir = temp_dir("walk");
        fs::write(dir.join("a.mjs"), "// a").unwrap();
        fs::write(dir.join("readme.md"), "# not code").unwrap();
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(dir.join("src").join("b.ts"), "export const b = 1;").unwrap();
        fs::create_dir_all(dir.join("node_modules")).unwrap();
        fs::write(dir.join("node_modules").join("skip.js"), "skip").unwrap();
        fs::create_dir_all(dir.join(".git")).unwrap();
        fs::write(dir.join(".git").join("skip2.js"), "skip").unwrap();

        let found = walk_folder(&dir.to_string_lossy(), ".", 400);
        assert_eq!(found, vec!["a.mjs".to_string(), "src/b.ts".to_string()]);
    }

    #[test]
    fn pct_of_matches_the_lower_bound_percentile() {
        let sorted = vec![1.0, 2.0, 2.0, 5.0];
        assert_eq!(pct_of(&sorted, Some(1.0)), Some(0.0));
        assert_eq!(pct_of(&sorted, Some(5.0)), Some(1.0));
        assert_eq!(pct_of(&sorted, None), None);
        assert_eq!(pct_of(&[], Some(1.0)), None);
        assert_eq!(pct_of(&[3.0], Some(3.0)), Some(0.0));
    }

    #[test]
    fn co_change_pairs_finds_non_import_linked_co_changes_and_respects_min_count() {
        let commits = vec![
            vec!["a.mjs".to_string(), "b.mjs".to_string()],
            vec!["a.mjs".to_string(), "b.mjs".to_string()],
            vec!["a.mjs".to_string(), "b.mjs".to_string()],
            vec!["a.mjs".to_string(), "c.mjs".to_string()], // linked via adj → filtered out
            vec!["only-one.mjs".to_string()],                // too small → skipped
        ];
        let scope: HashSet<String> = ["a.mjs", "b.mjs", "c.mjs"].iter().map(|s| s.to_string()).collect();
        let mut adj: HashMap<String, Vec<String>> = HashMap::new();
        adj.insert("a.mjs".to_string(), vec!["c.mjs".to_string()]);
        let out = co_change_pairs(&commits, &scope, &adj, 3, 20);
        assert_eq!(out, vec![json!({ "a": "a.mjs", "b": "b.mjs", "count": 3 })]);
    }

    #[test]
    fn ultra_run_scores_a_real_temp_territory_and_persists() {
        let dir = temp_dir("run");
        fs::write(dir.join("core.mjs"), "export function one() {}\nexport function two() {}\n").unwrap();
        fs::write(dir.join("core.test.mjs"), "// test file\n").unwrap();
        let c = setup_db();
        c.execute(
            "INSERT INTO code_nodes (id, origin, path, type, name, parent_id, span_json) VALUES ('core.mjs@atlas','atlas','core.mjs','file','core.mjs',NULL,'{\"start_line\":1,\"end_line\":2}')",
            [],
        )
        .unwrap();

        let out = ultraRun(&json!({ "root": dir.to_string_lossy(), "source": "test" }), &c).unwrap();
        assert_eq!(out["run"]["status"], json!("done"));
        assert!(out["capped"].as_i64().unwrap() >= 0);
        assert_eq!(out["summary"]["files"], json!(2));
        assert_eq!(out["summary"]["source_files"], json!(1));
        assert_eq!(out["summary"]["test_files"], json!(1));
        assert!(out["summary"]["co_change"].is_array());

        let scored: i64 = c.query_row("SELECT COUNT(*) FROM judgments.ultra_scores WHERE run_id = (SELECT id FROM judgments.ultra_runs ORDER BY id DESC LIMIT 1)", [], |r| r.get(0)).unwrap();
        assert_eq!(scored, 2);
        let status: String = c.query_row("SELECT status FROM judgments.ultra_runs ORDER BY id DESC LIMIT 1", [], |r| r.get(0)).unwrap();
        assert_eq!(status, "done");
    }

    #[test]
    fn ultra_run_explicit_files_list_skips_the_filesystem_walk() {
        let dir = temp_dir("files-mode");
        fs::write(dir.join("only.mjs"), "export const x = 1;\n").unwrap();
        let c = setup_db();
        let out = ultraRun(&json!({ "root": dir.to_string_lossy(), "files": ["only.mjs"] }), &c).unwrap();
        assert_eq!(out["summary"]["files"], json!(1));
        let scope_json: String = c.query_row("SELECT scope_json FROM judgments.ultra_runs ORDER BY id DESC LIMIT 1", [], |r| r.get(0)).unwrap();
        let scope: Value = serde_json::from_str(&scope_json).unwrap();
        assert_eq!(scope["folder"], Value::Null);
        assert_eq!(scope["files"], json!(1));
    }
}
