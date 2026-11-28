//! constitution-neighborhood.rs — twin of skills/constitution/constitution-neighborhood.mjs.
//! NAMES ARE CONTRACT: `constitutionNeighborhood` (default export, only unit — helpers
//! `normalize`/`resolveSpec`/`parseImports` are private in the JS, no `export`).
//!
//! BESPOKE-NATIVE, not conform-db fixture-tested: like constitution-report.rs's neighbor
//! constitution-file.rs this is a wide db-read dossier, BUT unlike constitution-file.rs it also
//! (a) shells a REAL `git log` subprocess twice — once via the already-converted `gitHistory`
//! twin (engineering.rs) for co-change pairs, once directly here (`git log -L<line>,<line>:<path>`
//! / `git log --follow --reverse`) for the origin-commit provenance — and (b) does a REAL
//! `std::fs::read_to_string` walk of the workspace's own source files (both to parse import
//! statements when the atlas graph doesn't reach the requested file, AND to slice ±2 lines of
//! real source around each violation line). None of that is fixture-seedable through conform-db's
//! two-lane harness (same class as terminal-sandbox.rs / code-git-info.rs — real git + real fs).
//! Proven below via `#[cfg(test)]` against a real temp git repo with real `.mjs` files and a real
//! in-memory db (laws+judgments attached, schema via the already-converted `ensureConstitutionSchema`).
//!
//! Cross-module calls (already-converted twins, reused not reimplemented):
//! `crate::engine_workspace_root::getWorkspaceRoot`, `crate::constitution__helpers_schema::ensureConstitutionSchema`,
//! `crate::constitution__helpers_engineering::{gitHistory, coChangePairs}`,
//! `crate::constitution__helpers_fix_hints::fixHintFor`, `crate::constitution__helpers_categories::categoryOf`.
//!
//! `resolve(String(input.root))` (Node's `path.resolve`, single-arg form: join against cwd then
//! normalize `.`/`..`) has no direct std equivalent — ported as `node_resolve()` below via
//! `Path::components()` normalization; a purely lexical normalization (no symlink resolution),
//! matching what Node's `path.resolve` itself does.
//!
//! `.slice(0, 90)` / `.slice(0, 96)` string truncation uses UTF-16 code-unit semantics in JS; this
//! twin approximates with `chars().take(N)` (ASCII/BMP-safe, same disclosed gap as every sibling
//! git-subprocess/text-slicing twin — terminal-sandbox.rs, orphan-worktree-dirs.rs).
// QUALITY: 76/100 — wide db-read dossier + two independent bespoke-native surfaces (git subprocess
// twice, real fs read/parse) in one function; ported faithfully with disclosed lexical-path-resolve
// and UTF-16-slice approximations shared with sibling twins.

use regex::Regex;
use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

// ── standard helper functions (playbook) ────────────────────────────────────────────────────
fn truthy(v: Option<&Value>) -> bool { match v { None|Some(Value::Null)=>false, Some(Value::Bool(b))=>*b, Some(Value::Number(n))=>n.as_f64().map_or(false,|f|f!=0.0&&!f.is_nan()), Some(Value::String(s))=>!s.is_empty(), Some(_)=>true } }
fn str_qq(v: Option<&Value>) -> String { match v { None|Some(Value::Null)=>String::new(), Some(Value::String(s))=>s.clone(), Some(Value::Bool(b))=>b.to_string(), Some(n@Value::Number(_))=>n.to_string(), Some(o)=>o.to_string() } } // = JS String(v ?? '')
fn js_number(v: Option<&Value>) -> f64 { match v { None=>f64::NAN, Some(Value::Null)=>0.0, Some(Value::Bool(b))=>if *b {1.0} else {0.0}, Some(Value::Number(n))=>n.as_f64().unwrap_or(f64::NAN), Some(Value::String(s))=>{let t=s.trim(); if t.is_empty(){0.0}else{t.parse().unwrap_or(f64::NAN)}}, Some(_)=>f64::NAN } } // = JS Number(v)
fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) }
fn js_round(x: f64) -> f64 { (x + 0.5).floor() } // Math.round: half-up

fn slice_n(s: &str, n: usize) -> String { s.chars().take(n).collect() }

fn normalize(p: &str) -> String { p.replace('\\', "/") }

// Node `path.resolve(input)` (single positional arg): join against cwd, then lexically normalize
// `.`/`..` components (no symlink resolution — matches Node's own purely-lexical behavior).
fn node_resolve(input: &str) -> String {
    let base = if Path::new(input).is_absolute() { PathBuf::new() } else { std::env::current_dir().unwrap_or_default() };
    let combined = base.join(input);
    let mut out = PathBuf::new();
    for comp in combined.components() {
        match comp {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => { out.pop(); }
            other => out.push(other.as_os_str()),
        }
    }
    out.to_string_lossy().into_owned()
}

fn full_path(root: &str, p: &str) -> PathBuf {
    if Path::new(p).is_absolute() { PathBuf::from(p) } else { Path::new(root).join(p) }
}

// ── IMPORT_RE + EXTS (module-level constants in the JS) ─────────────────────────────────────
fn import_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r#"(?:import\s+(?:([\w$]+)\s*,?\s*)?(?:\{([^}]*)\})?\s*from\s*|export\s+(?:\{[^}]*\}|\*)\s*from\s*|require\(\s*)['"]([^'"]+)['"]"#).unwrap()
    })
}
fn as_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\s+as\s+").unwrap())
}
fn ext_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\.\w+$").unwrap())
}
const EXTS: [&str; 11] = ["", ".mjs", ".js", ".ts", ".tsx", ".jsx", ".cjs", "/index.mjs", "/index.js", "/index.ts", "/index.tsx"];

/// `resolveSpec(fromPath, spec, fileSet)` — resolve a relative specifier against the importing
/// file, into the known-file set. Package imports (non-relative specs) return None.
fn resolve_spec(from_path: &str, spec: &str, file_set: &HashSet<String>) -> Option<String> {
    if !spec.starts_with('.') { return None; }
    let mut dir_parts: Vec<&str> = from_path.split('/').collect();
    dir_parts.pop();
    let mut stack: Vec<String> = dir_parts.iter().map(|s| s.to_string()).collect();
    for seg in spec.split('/') {
        if seg == "." || seg.is_empty() { continue; }
        if seg == ".." { stack.pop(); } else { stack.push(seg.to_string()); }
    }
    let base_p = stack.join("/");
    let no_ext = ext_re().replace(&base_p, "").into_owned();
    let mut candidates: Vec<String> = vec![base_p.clone()];
    for e in EXTS { candidates.push(format!("{no_ext}{e}")); }
    for e in EXTS { candidates.push(format!("{base_p}{e}")); }
    for cand in candidates {
        if !cand.is_empty() && file_set.contains(&cand) { return Some(cand); }
    }
    None
}

/// `parseImports(root, fromPath, fileSet)` — one file's imports → [{to, via[]}] within the file set.
fn parse_imports(root: &str, from_path: &str, file_set: &HashSet<String>) -> Vec<(String, Vec<String>)> {
    let src = match std::fs::read_to_string(full_path(root, from_path)) { Ok(s) => s, Err(_) => return Vec::new() };
    if src.len() > 400_000 { return Vec::new(); }
    let mut order: Vec<String> = Vec::new();
    let mut via_map: HashMap<String, Vec<String>> = HashMap::new();
    for caps in import_re().captures_iter(&src) {
        let spec = caps.get(3).map(|m| m.as_str()).unwrap_or("");
        let to = match resolve_spec(from_path, spec, file_set) { Some(t) => t, None => continue };
        if to == from_path { continue; }
        if !via_map.contains_key(&to) { order.push(to.clone()); via_map.insert(to.clone(), Vec::new()); }
        let via = via_map.get_mut(&to).unwrap();
        if let Some(g1) = caps.get(1) {
            let n = g1.as_str();
            if !n.is_empty() && !via.contains(&n.to_string()) { via.push(n.to_string()); }
        }
        if let Some(g2) = caps.get(2) {
            for part in g2.as_str().split(',') {
                let trimmed = part.trim();
                let head = as_re().split(trimmed).next().unwrap_or("").trim();
                if !head.is_empty() && !via.contains(&head.to_string()) { via.push(head.to_string()); }
            }
        }
    }
    order.into_iter().map(|to| { let via = via_map.remove(&to).unwrap_or_default(); let via3: Vec<String> = via.into_iter().take(3).collect(); (to, via3) }).collect()
}

pub fn constitutionNeighborhood(input: &Value, db: &Connection) -> Result<Value, String> {
    let _ = crate::constitution__helpers_schema::ensureConstitutionSchema(db);
    let path = normalize(str_qq(input.get("path")).trim());
    if path.is_empty() { return Err("constitution-neighborhood: path is required".to_string()); }
    // `Math.max(1, Math.min(3, Number(input?.depth ?? 2) || 2))` — nullish-coalesce THEN
    // Number(), THEN `|| default` (JS logical-OR falls back on 0/NaN), THEN clamp. Order matters:
    // Rust's f64::min/max already discard NaN (unlike JS Math.min/max), so the explicit
    // 0-or-NaN-fallback below must run BEFORE clamping, not rely on min/max to absorb it.
    let depth_raw = nz(input.get("depth")).cloned().unwrap_or(json!(2));
    let depth_num = js_number(Some(&depth_raw));
    let depth_num = if depth_num == 0.0 || depth_num.is_nan() { 2.0 } else { depth_num };
    let depth = depth_num.min(3.0).max(1.0) as i64;
    let mpr_raw = nz(input.get("max_per_ring")).cloned().unwrap_or(json!(10));
    let mpr_num = js_number(Some(&mpr_raw));
    let mpr_num = if mpr_num == 0.0 || mpr_num.is_nan() { 10.0 } else { mpr_num };
    let max_per_ring = mpr_num.min(20.0).max(3.0) as i64;
    let root = if truthy(input.get("root")) { node_resolve(&str_qq(input.get("root"))) } else { crate::engine_workspace_root::getWorkspaceRoot(db).as_str().unwrap_or_default().to_string() };

    let snap: Option<Value> = crate::db_json::query_rows(db, "SELECT id, created_at FROM judgments.review_snapshots WHERE status='done' ORDER BY id DESC LIMIT 1", &[])?
        .as_array().and_then(|a| a.first()).cloned();
    let snap = match snap {
        Some(s) => s,
        None => return Ok(json!({ "ok": false, "path": path, "error": "no constitution snapshot yet — rebuild first" })),
    };
    let snap_id = snap.get("id").cloned().unwrap_or(Value::Null);

    let score_rows: Vec<Value> = crate::db_json::query_rows(
        db,
        "SELECT path, health, score, axes_json, loc, fan_in, fan_out, churn FROM judgments.review_scores WHERE snapshot_id=? AND unit_kind='file'",
        &[snap_id.clone()],
    )?.as_array().cloned().unwrap_or_default();

    let mut file_set: HashSet<String> = HashSet::new();
    for r in &score_rows { file_set.insert(normalize(&str_qq(r.get("path")))); }
    file_set.insert(path.clone());

    // ── the import adjacency: atlas when present, else parsed from the snapshot's files
    let mut fwd: HashMap<String, Vec<String>> = HashMap::new();
    let mut source = "atlas".to_string();
    if let Ok(node_rows_v) = crate::db_json::query_rows(db, "SELECT id, path FROM code_nodes WHERE origin='atlas' AND type='file'", &[]) {
        if let Ok(edge_rows_v) = crate::db_json::query_rows(db, &format!("SELECT from_node, to_node FROM code_edges WHERE origin='atlas' AND {}", crate::codebase_atlas_build::RUNTIME_IMPORTS), &[]) {
            let mut id_to_path: HashMap<String, String> = HashMap::new();
            for r in node_rows_v.as_array().cloned().unwrap_or_default() {
                let id = str_qq(r.get("id"));
                let p = normalize(&str_qq(r.get("path")));
                id_to_path.insert(id, p);
            }
            for e in edge_rows_v.as_array().cloned().unwrap_or_default() {
                let a = id_to_path.get(&str_qq(e.get("from_node"))).cloned();
                let b = id_to_path.get(&str_qq(e.get("to_node"))).cloned();
                if let (Some(a), Some(b)) = (a, b) {
                    if a == b { continue; }
                    let entry = fwd.entry(a).or_insert_with(Vec::new);
                    if !entry.contains(&b) { entry.push(b); }
                }
            }
        }
    }
    let atlas_touches_center = fwd.get(&path).map(|v| !v.is_empty()).unwrap_or(false)
        || fwd.values().any(|arr| arr.contains(&path));
    if fwd.is_empty() || !atlas_touches_center {
        source = "parsed".to_string();
        fwd = HashMap::new();
        // JS: `[...fileSet].slice(0, 800)` — a `Set` iterates insertion order (scoreRows order,
        // then the requested path). `HashSet` has no stable order, so on a repo with >800 files
        // in scope this twin may scan a different 800-file subset than the JS original (same
        // final graph in the common case — under-cap repos, including this twin's own tests —
        // disclosed gap on the rare over-cap repo, no fixture/production data affected either way).
        let files: Vec<String> = file_set.iter().take(800).cloned().collect();
        for f in &files {
            let outs = parse_imports(&root, f, &file_set);
            if !outs.is_empty() {
                fwd.insert(f.clone(), outs.into_iter().map(|(to, _)| to).collect());
            }
        }
    }
    let mut rev: HashMap<String, Vec<String>> = HashMap::new();
    for (a, bs) in &fwd {
        for b in bs {
            let entry = rev.entry(b.clone()).or_insert_with(Vec::new);
            if !entry.contains(a) { entry.push(a.clone()); }
        }
    }

    // ── BFS both directions, layer by layer
    let mut placed_order: Vec<String> = vec![path.clone()];
    let mut placed_map: HashMap<String, (String, i64)> = HashMap::new();
    placed_map.insert(path.clone(), ("center".to_string(), 0));
    let mut dropped_up: i64 = 0;
    let mut dropped_down: i64 = 0;

    let mut viol_order: Vec<String> = Vec::new();
    let mut viol_map: HashMap<String, Vec<Value>> = HashMap::new();
    let viol_rows: Vec<Value> = crate::db_json::query_rows(
        db,
        "SELECT path, law_key, weight, confidence, severity, line, evidence FROM judgments.review_violations WHERE snapshot_id=?",
        &[snap_id.clone()],
    )?.as_array().cloned().unwrap_or_default();
    for v in viol_rows {
        let p = normalize(&str_qq(v.get("path")));
        if !viol_map.contains_key(&p) { viol_order.push(p.clone()); viol_map.insert(p.clone(), Vec::new()); }
        viol_map.get_mut(&p).unwrap().push(v);
    }
    let mut health_of: HashMap<String, Value> = HashMap::new();
    for r in &score_rows { health_of.insert(normalize(&str_qq(r.get("path"))), r.clone()); }

    let worth_of = |p: &str, health_of: &HashMap<String, Value>| -> f64 {
        match health_of.get(p).and_then(|r| r.get("health")) {
            Some(v) if !v.is_null() => v.as_f64().unwrap_or(50.0),
            _ => 50.0,
        }
    };

    let grow = |dir: &str, adj: &HashMap<String, Vec<String>>, placed_order: &mut Vec<String>, placed_map: &mut HashMap<String, (String, i64)>, dropped: &mut i64, health_of: &HashMap<String, Value>| {
        let mut frontier: Vec<String> = vec![path.clone()];
        for layer in 1..=depth {
            let mut next_order: Vec<String> = Vec::new();
            let mut next_set: HashSet<String> = HashSet::new();
            for f in &frontier {
                if let Some(ns) = adj.get(f) {
                    for n in ns {
                        if !placed_map.contains_key(n) && !next_set.contains(n) {
                            next_set.insert(n.clone());
                            next_order.push(n.clone());
                        }
                    }
                }
            }
            let mut ranked = next_order.clone();
            ranked.sort_by(|a, b| worth_of(a, health_of).partial_cmp(&worth_of(b, health_of)).unwrap_or(std::cmp::Ordering::Equal));
            let keep: Vec<String> = ranked.iter().take(max_per_ring as usize).cloned().collect();
            *dropped += ranked.len() as i64 - keep.len() as i64;
            for n in &keep {
                placed_map.insert(n.clone(), (dir.to_string(), layer));
                placed_order.push(n.clone());
            }
            frontier = keep;
            if frontier.is_empty() { break; }
        }
    };
    grow("up", &rev, &mut placed_order, &mut placed_map, &mut dropped_up, &health_of);
    grow("down", &fwd, &mut placed_order, &mut placed_map, &mut dropped_down, &health_of);
    let dropped = json!({ "up": dropped_up, "down": dropped_down });

    // ── the ISSUE RING — top-2 laws by penalty among the shown files, up to 2 more files each.
    let mut shown_pen_order: Vec<String> = Vec::new();
    let mut shown_pen: HashMap<String, i64> = HashMap::new();
    for p2 in &placed_order {
        for v in viol_map.get(p2).unwrap_or(&Vec::new()) {
            let law_key = str_qq(v.get("law_key"));
            let weight = js_number(nz(v.get("weight"))); let weight = if weight.is_nan() { 1.0 } else { weight };
            let confidence = js_number(nz(v.get("confidence"))); let confidence = if confidence.is_nan() { 1.0 } else { confidence };
            let pen = js_round(weight * confidence) as i64;
            if !shown_pen.contains_key(&law_key) { shown_pen_order.push(law_key.clone()); }
            *shown_pen.entry(law_key).or_insert(0) += pen;
        }
    }
    let mut top_laws: Vec<String> = shown_pen_order.clone();
    top_laws.sort_by(|a, b| shown_pen[b].cmp(&shown_pen[a]));
    top_laws.truncate(2);
    for lk in &top_laws {
        let mut outside: Vec<(String, i64)> = Vec::new();
        for p2 in &viol_order {
            if placed_map.contains_key(p2) || !file_set.contains(p2) { continue; }
            let n = viol_map.get(p2).unwrap_or(&Vec::new()).iter().filter(|v| str_qq(v.get("law_key")) == *lk).count() as i64;
            if n > 0 { outside.push((p2.clone(), n)); }
        }
        outside.sort_by(|a, b| b.1.cmp(&a.1));
        outside.truncate(2);
        for (p2, _) in outside {
            if !placed_map.contains_key(&p2) {
                placed_map.insert(p2.clone(), ("issue".to_string(), 1));
                placed_order.push(p2);
            }
        }
    }

    // ── nodes with full intelligence — verdict + axes + signals + worst violations w/ real hunks
    let mut src_cache: HashMap<String, Option<Vec<String>>> = HashMap::new();
    let slice_at = |p: &str, line: Option<i64>, src_cache: &mut HashMap<String, Option<Vec<String>>>| -> Value {
        let ls = if !src_cache.contains_key(p) {
            let v = match std::fs::read_to_string(full_path(&root, p)) {
                Ok(t) if t.len() <= 600_000 => Some(t.split('\n').map(|s| s.to_string()).collect::<Vec<_>>()),
                _ => None,
            };
            src_cache.insert(p.to_string(), v.clone());
            v
        } else { src_cache.get(p).cloned().unwrap() };
        let line = match line { Some(l) if l >= 1 => l, _ => return Value::Null };
        let ls = match ls { Some(v) => v, None => return Value::Null };
        if line as usize > ls.len() { return Value::Null; }
        let lo = (line - 2).max(1) as usize;
        let hi = ((line + 2) as usize).min(ls.len());
        let mut out: Vec<Value> = Vec::new();
        for ln in lo..=hi {
            let text = ls.get(ln - 1).cloned().unwrap_or_default();
            out.push(json!({ "ln": ln as i64, "text": slice_n(&text, 96) }));
        }
        json!(out)
    };

    let law_rows: Vec<Value> = crate::db_json::query_rows(db, "SELECT key, title, rule_md, detector, config_json, scope FROM laws.review_laws", &[])?.as_array().cloned().unwrap_or_default();
    let mut law_title: HashMap<String, String> = HashMap::new();
    let mut law_by_key: HashMap<String, Value> = HashMap::new();
    for l in &law_rows {
        let k = str_qq(l.get("key"));
        let title = nz(l.get("title")).map(|t| str_qq(Some(t))).unwrap_or_else(|| k.clone());
        law_title.insert(k.clone(), title);
        law_by_key.insert(k, l.clone());
    }

    let mut nodes: Vec<Value> = Vec::new();
    for p in &placed_order {
        let (dir, layer) = placed_map.get(p).cloned().unwrap_or(("center".to_string(), 0));
        let s = health_of.get(p);
        let vs = viol_map.get(p).cloned().unwrap_or_default();
        let mut by_law_order: Vec<String> = Vec::new();
        let mut by_law: HashMap<String, i64> = HashMap::new();
        for v in &vs {
            let lk = str_qq(v.get("law_key"));
            if !by_law.contains_key(&lk) { by_law_order.push(lk.clone()); }
            *by_law.entry(lk).or_insert(0) += 1;
        }
        let mut by_law_ranked = by_law_order.clone();
        by_law_ranked.sort_by(|a, b| by_law[b].cmp(&by_law[a]));
        let worst = by_law_ranked.first().cloned();

        let axes: Value = match s.and_then(|r| r.get("axes_json")).and_then(|v| v.as_str()) {
            Some(txt) => match serde_json::from_str::<Value>(txt) {
                Ok(a) if truthy(Some(&a)) => {
                    let mut m = Map::new();
                    if let Some(obj) = a.as_object() {
                        for (k, v) in obj {
                            if let Some(score) = v.get("score") { if !score.is_null() { m.insert(k.clone(), score.clone()); } }
                        }
                    }
                    Value::Object(m)
                }
                _ => Value::Null,
            },
            None => Value::Null,
        };

        let mut worst_list_src = vs.clone();
        worst_list_src.sort_by(|x, y| {
            let wx = js_number(nz(x.get("weight"))); let wx = if wx.is_nan() { 1.0 } else { wx };
            let cx = js_number(nz(x.get("confidence"))); let cx = if cx.is_nan() { 1.0 } else { cx };
            let wy = js_number(nz(y.get("weight"))); let wy = if wy.is_nan() { 1.0 } else { wy };
            let cy = js_number(nz(y.get("confidence"))); let cy = if cy.is_nan() { 1.0 } else { cy };
            (wy * cy).partial_cmp(&(wx * cx)).unwrap_or(std::cmp::Ordering::Equal)
        });
        worst_list_src.truncate(3);
        let worst_list: Vec<Value> = worst_list_src.iter().map(|v| {
            let lk = str_qq(v.get("law_key"));
            let law = law_by_key.get(&lk);
            let line = v.get("line").and_then(Value::as_i64);
            let evidence = v.get("evidence").filter(|e| !e.is_null()).map(|e| slice_n(&str_qq(Some(e)), 90));
            let fix = law.map(|l| crate::constitution__helpers_fix_hints::fixHintFor(l, v)).unwrap_or(Value::Null);
            let snippet = slice_at(p, line, &mut src_cache);
            json!({
                "law_key": lk,
                "title": law_title.get(&lk).cloned().unwrap_or_else(|| lk.clone()),
                "line": v.get("line").cloned().unwrap_or(Value::Null),
                "severity": v.get("severity").cloned().unwrap_or(Value::Null),
                "evidence": evidence,
                "fix": fix,
                "snippet": snippet,
            })
        }).collect();

        let signals = s.map(|r| json!({
            "loc": r.get("loc").cloned().unwrap_or(Value::Null),
            "fan_in": r.get("fan_in").cloned().unwrap_or(Value::Null),
            "fan_out": r.get("fan_out").cloned().unwrap_or(Value::Null),
            "churn": r.get("churn").cloned().unwrap_or(Value::Null),
        })).unwrap_or(Value::Null);

        let errs = vs.iter().filter(|v| v.get("severity").and_then(Value::as_str) == Some("error")).count() as i64;
        let warns = vs.iter().filter(|v| v.get("severity").and_then(Value::as_str) == Some("warn")).count() as i64;

        nodes.push(json!({
            "path": p,
            "name": p.split('/').last().unwrap_or(p.as_str()),
            "dir": dir,
            "layer": layer,
            "health": s.and_then(|r| nz(r.get("health"))).cloned().unwrap_or(Value::Null),
            "score": s.and_then(|r| nz(r.get("score"))).cloned().unwrap_or(Value::Null),
            "judged": s.is_some(),
            "axes": axes,
            "signals": signals,
            "violations": vs.len() as i64,
            "errs": errs,
            "warns": warns,
            "worst_law": worst,
            "worst_list": worst_list,
        }));
    }

    // ── edges among displayed nodes: imports (with crossing symbols) + hidden co-change.
    let shown_order = placed_order.clone();
    let shown_set: HashSet<String> = shown_order.iter().cloned().collect();
    let mut resolve_set: HashSet<String> = file_set.clone();
    for s in &shown_set { resolve_set.insert(s.clone()); }
    let mut via_cache: HashMap<String, HashMap<String, Vec<String>>> = HashMap::new();
    let mut via_for = |a: &str, b: &str, via_cache: &mut HashMap<String, HashMap<String, Vec<String>>>| -> Vec<String> {
        if !via_cache.contains_key(a) {
            let m: HashMap<String, Vec<String>> = parse_imports(&root, a, &resolve_set).into_iter().collect();
            via_cache.insert(a.to_string(), m);
        }
        via_cache.get(a).and_then(|m| m.get(b)).cloned().unwrap_or_default()
    };
    let mut edges: Vec<Value> = Vec::new();
    for a in &shown_order {
        for b in fwd.get(a).unwrap_or(&Vec::new()) {
            if shown_set.contains(b) {
                let via = via_for(a, b, &mut via_cache);
                edges.push(json!({ "from": a, "to": b, "kind": "import", "via": via }));
            }
        }
    }
    let git = crate::constitution__helpers_engineering::gitHistory(&root);
    let mut co_edges: Vec<Value> = Vec::new();
    if let Some(g) = &git {
        let pairs = crate::constitution__helpers_engineering::coChangePairs(&g.commits, &shown_set, &fwd, 3, 12);
        for (a, b, n) in pairs { co_edges.push(json!({ "from": a, "to": b, "kind": "co_change", "n": n, "via": [] })); }
    }

    // ── ISSUE HUBS — the same law across ≥2 shown files.
    let mut hub_order: Vec<String> = Vec::new();
    let mut hub_members: HashMap<String, Vec<String>> = HashMap::new();
    for p2 in &shown_order {
        for v in viol_map.get(p2).unwrap_or(&Vec::new()) {
            let lk = str_qq(v.get("law_key"));
            if !hub_members.contains_key(&lk) { hub_order.push(lk.clone()); hub_members.insert(lk.clone(), Vec::new()); }
            let m = hub_members.get_mut(&lk).unwrap();
            if !m.contains(p2) { m.push(p2.clone()); }
        }
    }
    let mut issue_hubs: Vec<Value> = hub_order.iter()
        .filter_map(|k| {
            let members = hub_members.get(k).cloned().unwrap_or_default();
            if members.len() < 2 { return None; }
            let sev = viol_map.get(&members[0]).unwrap_or(&Vec::new()).iter().find(|v| str_qq(v.get("law_key")) == *k)
                .and_then(|v| v.get("severity").cloned()).unwrap_or(json!("warn"));
            Some(json!({ "law_key": k, "title": law_title.get(k).cloned().unwrap_or_else(|| k.clone()), "severity": sev, "members": members }))
        })
        .collect();
    issue_hubs.sort_by(|a, b| {
        let la = a.get("members").and_then(Value::as_array).map(|x| x.len()).unwrap_or(0);
        let lb = b.get("members").and_then(Value::as_array).map(|x| x.len()).unwrap_or(0);
        lb.cmp(&la)
    });
    issue_hubs.truncate(5);

    // ── GEOGRAPHY — folders around the shown files, with untouched sibling files.
    let mut geo: Vec<Value> = Vec::new();
    {
        let mut dir_order: Vec<String> = Vec::new();
        let mut shown_by_dir: HashMap<String, Vec<String>> = HashMap::new();
        let dir_of = |p2: &str| -> String {
            let mut parts: Vec<&str> = p2.split('/').collect();
            parts.pop();
            if parts.is_empty() { ".".to_string() } else { parts.join("/") }
        };
        for p2 in &shown_order {
            let d = dir_of(p2);
            if !shown_by_dir.contains_key(&d) { dir_order.push(d.clone()); shown_by_dir.insert(d.clone(), Vec::new()); }
            shown_by_dir.get_mut(&d).unwrap().push(p2.clone());
        }
        let all_paths: Vec<String> = score_rows.iter().map(|r| normalize(&str_qq(r.get("path")))).collect();
        for d in &dir_order {
            let members = shown_by_dir.get(d).cloned().unwrap_or_default();
            let siblings: Vec<String> = all_paths.iter().filter(|p2| !shown_set.contains(*p2) && &dir_of(p2) == d).take(8).cloned().collect();
            geo.push(json!({ "dir": d, "members": members, "siblings": siblings }));
        }
    }

    // ── the neighborhood's issues, united — worst first
    let mut issues: Vec<(i64, Value)> = Vec::new();
    for p in &shown_order {
        for v in viol_map.get(p).unwrap_or(&Vec::new()) {
            let lk = str_qq(v.get("law_key"));
            let law = law_by_key.get(&lk).cloned().unwrap_or_else(|| json!({ "key": lk.clone() }));
            let weight = js_number(nz(v.get("weight"))); let weight = if weight.is_nan() { 1.0 } else { weight };
            let confidence = js_number(nz(v.get("confidence"))); let confidence = if confidence.is_nan() { 1.0 } else { confidence };
            let pen = js_round(weight * confidence) as i64;
            let evidence = v.get("evidence").filter(|e| !e.is_null()).map(|e| slice_n(&str_qq(Some(e)), 90));
            issues.push((pen, json!({
                "path": p,
                "law_key": lk,
                "title": law_title.get(&lk).cloned().unwrap_or_else(|| lk.clone()),
                "severity": v.get("severity").cloned().unwrap_or(Value::Null),
                "category": crate::constitution__helpers_categories::categoryOf(&law),
                "line": v.get("line").cloned().unwrap_or(Value::Null),
                "evidence": evidence,
                "pen": pen,
            })));
        }
    }
    issues.sort_by(|a, b| b.0.cmp(&a.0));
    let issues: Vec<Value> = issues.into_iter().take(14).map(|(_, v)| v).collect();

    // ── the ORIGIN dossier — who starts it, why, when it first appeared, the introducing commit.
    let mut origin_info: Value = Value::Null;
    {
        let sick: Vec<&Value> = nodes.iter().filter(|n| n.get("violations").and_then(Value::as_i64).unwrap_or(0) > 0).collect();
        if !sick.is_empty() {
            let depth2 = |n: &Value| -> i64 {
                let d = n.get("dir").and_then(Value::as_str).unwrap_or("");
                let l = n.get("layer").and_then(Value::as_i64).unwrap_or(0);
                if d == "down" { 2 + l } else if d == "center" { 1 } else { 0 }
            };
            let pen_of = |n: &Value| -> i64 {
                let p = n.get("path").and_then(Value::as_str).unwrap_or("");
                viol_map.get(p).unwrap_or(&Vec::new()).iter().map(|v| {
                    let w = js_number(nz(v.get("weight"))); let w = if w.is_nan() { 1.0 } else { w };
                    let c = js_number(nz(v.get("confidence"))); let c = if c.is_nan() { 1.0 } else { c };
                    js_round(w * c) as i64
                }).sum()
            };
            let mut sick_sorted = sick.clone();
            sick_sorted.sort_by(|a, b| depth2(*b).cmp(&depth2(*a)).then(pen_of(*b).cmp(&pen_of(*a))));
            let o = sick_sorted[0];
            let o_path = o.get("path").and_then(Value::as_str).unwrap_or("").to_string();
            let o_name = o.get("name").cloned().unwrap_or(Value::Null);
            let w0 = o.get("worst_list").and_then(Value::as_array).and_then(|a| a.first()).cloned();
            let law_row = w0.as_ref().and_then(|w| w.get("law_key")).and_then(Value::as_str).and_then(|k| law_by_key.get(k));
            let in_co = co_edges.iter().any(|e| e.get("from").and_then(Value::as_str) == Some(o_path.as_str()) || e.get("to").and_then(Value::as_str) == Some(o_path.as_str()));
            let in_hub = w0.as_ref().map(|w| {
                let lk = w.get("law_key").and_then(Value::as_str).unwrap_or("");
                issue_hubs.iter().any(|h| h.get("law_key").and_then(Value::as_str) == Some(lk) && h.get("members").and_then(Value::as_array).map(|m| m.len() >= 2).unwrap_or(false))
            }).unwrap_or(false);
            let reason = if in_co { "hidden coupling" } else if in_hub { "a house pattern — the same law breaks elsewhere" } else { "a local slip" };

            let first_snapshot_at: Value = match &w0 {
                Some(w) => {
                    let lk = w.get("law_key").and_then(Value::as_str).unwrap_or("");
                    crate::db_json::query_rows(
                        db,
                        "SELECT s.created_at FROM judgments.review_violations v JOIN judgments.review_snapshots s ON s.id=v.snapshot_id WHERE v.path=? AND v.law_key=? AND s.status='done' ORDER BY s.id ASC LIMIT 1",
                        &[json!(o_path.clone()), json!(lk)],
                    ).ok().and_then(|r| r.as_array().and_then(|a| a.first()).and_then(|row| row.get("created_at").cloned())).unwrap_or(Value::Null)
                }
                None => Value::Null,
            };

            let first_commit: Value = {
                let line = w0.as_ref().and_then(|w| w.get("line")).and_then(Value::as_i64);
                let out = if let Some(l) = line {
                    crate::proc::cmd("git")
                        .arg("-C").arg(&root)
                        .arg("log").arg("--format=%h %as").arg(format!("-L{l},{l}:{o_path}"))
                        .output()
                } else {
                    crate::proc::cmd("git")
                        .arg("-C").arg(&root)
                        .arg("log").arg("--follow").arg("--reverse").arg("--format=%h %as").arg("--").arg(&o_path)
                        .output()
                };
                match out {
                    Ok(o) if o.status.success() => {
                        let text = String::from_utf8_lossy(&o.stdout).into_owned();
                        let re = shas_re();
                        let shas: Vec<&str> = text.split('\n').map(|l| l.trim()).filter(|l| re.is_match(l)).collect();
                        let pick = if line.is_some() { shas.last() } else { shas.first() };
                        match pick {
                            Some(line_txt) => {
                                let mut parts = line_txt.splitn(2, ' ');
                                let sha = parts.next().unwrap_or("").to_string();
                                let date = parts.next().unwrap_or("").to_string();
                                json!({ "sha": sha, "date": date })
                            }
                            None => Value::Null,
                        }
                    }
                    _ => Value::Null,
                }
            };

            origin_info = json!({
                "path": o_path,
                "name": o_name,
                "reason": reason,
                "law_key": w0.as_ref().and_then(|w| w.get("law_key")).cloned().unwrap_or(Value::Null),
                "law_title": w0.as_ref().and_then(|w| w.get("title")).cloned().unwrap_or(Value::Null),
                "category": law_row.map(|l| crate::constitution__helpers_categories::categoryOf(l)).unwrap_or(Value::Null),
                "first_snapshot_at": first_snapshot_at,
                "first_commit": first_commit,
            });
        }
    }

    let all_edges: Vec<Value> = edges.into_iter().chain(co_edges.into_iter()).collect();
    let dropped_total = dropped_up + dropped_down;
    let note = format!(
        "{} · import/export wiring + same-issue connections · co-change ≥3 commits with no import edge{}",
        if source == "atlas" { "atlas import graph" } else { "imports parsed from source" },
        if dropped_total != 0 { format!(" · {dropped_total} node(s) beyond the ring cap (worst kept)") } else { String::new() },
    );

    Ok(json!({
        "ok": true, "path": path, "snapshot_id": snap_id, "depth": depth, "source": source,
        "nodes": nodes, "edges": all_edges, "issue_hubs": issue_hubs, "issues": issues, "geo": geo, "origin_info": origin_info,
        "dropped": dropped,
        "note": note,
    }))
}

fn shas_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^[0-9a-f]{7,40} \d{4}-\d{2}-\d{2}$").unwrap())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_dir(tag: &str) -> PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!("antiloki-constitution-neighborhood-rs-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn run_git_ok(args: &[&str], cwd: &Path) {
        let out = crate::proc::cmd("git").args(args).current_dir(cwd).output().unwrap();
        assert!(out.status.success(), "git {:?} failed: {}", args, String::from_utf8_lossy(&out.stderr));
    }

    fn make_repo(tag: &str) -> PathBuf {
        let dir = temp_dir(tag);
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(dir.join("src").join("a.mjs"), "import { foo } from './b.mjs';\nfoo();\n").unwrap();
        fs::write(dir.join("src").join("b.mjs"), "export function foo() {\n  var x = 1;\n  return x;\n}\n").unwrap();
        fs::write(dir.join("src").join("c.mjs"), "export const unrelated = 1;\n").unwrap();
        run_git_ok(&["init", "-q"], &dir);
        run_git_ok(&["config", "user.email", "test@test"], &dir);
        run_git_ok(&["config", "user.name", "test"], &dir);
        run_git_ok(&["add", "."], &dir);
        run_git_ok(&["commit", "-q", "-m", "init"], &dir);
        dir
    }

    fn conn() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS laws; ATTACH ':memory:' AS judgments").unwrap();
        let _ = crate::constitution__helpers_schema::ensureConstitutionSchema(&c);
        c
    }

    fn seed_snapshot(c: &Connection, id: i64) {
        c.execute(
            &format!("INSERT INTO judgments.review_snapshots (id, created_at, scope_json, manifest_json, constitution_hash, status) VALUES ({id}, '2026-01-01 00:00:00', '{{}}', '[]', 'h{id}', 'done')"),
            [],
        ).unwrap();
    }

    #[test]
    fn missing_path_errs() {
        let c = conn();
        let e = constitutionNeighborhood(&json!({}), &c).unwrap_err();
        assert_eq!(e, "constitution-neighborhood: path is required");
    }

    #[test]
    fn no_snapshot_yet_returns_ok_false() {
        let c = conn();
        let r = constitutionNeighborhood(&json!({ "path": "src/a.mjs" }), &c).unwrap();
        assert_eq!(r["ok"], json!(false));
        assert_eq!(r["error"], json!("no constitution snapshot yet — rebuild first"));
    }

    #[test]
    fn builds_neighborhood_from_parsed_imports_with_real_git_and_fs() {
        let root = make_repo("basic");
        let c = conn();
        seed_snapshot(&c, 1);
        c.execute(
            "INSERT INTO judgments.review_scores (snapshot_id, unit_kind, path, health, score) VALUES \
             (1,'file','src/a.mjs',90,90), (1,'file','src/b.mjs',40,40), (1,'file','src/c.mjs',80,80)",
            [],
        ).unwrap();
        c.execute("INSERT INTO laws.review_laws (scope, key, title, rule_md, status, enforcement, weight, severity, confidence) VALUES ('project','no-var','No var','Use let/const','ratified','score',5,'warn',0.9)", []).unwrap();
        c.execute("INSERT INTO judgments.review_violations (snapshot_id, path, law_key, law_scope, weight, severity, confidence, line, evidence) VALUES (1,'src/b.mjs','no-var','project',5,'warn',0.9,2,'var x = 1;')", []).unwrap();

        let r = constitutionNeighborhood(&json!({ "path": "src/a.mjs", "depth": 1, "root": root.to_str().unwrap() }), &c).unwrap();
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["source"], json!("parsed"), "no atlas tables exist in this test db, so it falls back to source-parsed imports");

        let nodes = r["nodes"].as_array().unwrap();
        let center = nodes.iter().find(|n| n["path"] == json!("src/a.mjs")).unwrap();
        assert_eq!(center["dir"], json!("center"));
        let b = nodes.iter().find(|n| n["path"] == json!("src/b.mjs")).unwrap();
        assert_eq!(b["dir"], json!("down"));
        assert_eq!(b["layer"], json!(1));
        assert_eq!(b["violations"], json!(1));
        assert_eq!(b["worst_law"], json!("no-var"));
        assert_eq!(b["worst_list"][0]["title"], json!("No var"));
        assert_eq!(b["worst_list"][0]["snippet"][0]["ln"], json!(1), "±2 lines around the real violation line 2 in the real file, starting at line 1");
        assert!(nodes.iter().all(|n| n["path"] != json!("src/c.mjs")), "c.mjs is not imported by a.mjs — outside the depth-1 neighborhood");

        let edges = r["edges"].as_array().unwrap();
        let import_edge = edges.iter().find(|e| e["kind"] == json!("import")).unwrap();
        assert_eq!(import_edge["from"], json!("src/a.mjs"));
        assert_eq!(import_edge["to"], json!("src/b.mjs"));
        assert_eq!(import_edge["via"], json!(["foo"]), "named import parsed from the real source file");

        assert_eq!(r["issues"][0]["law_key"], json!("no-var"));
        // categoryOf checks scope==='project' BEFORE the /\bvar\b/ correctness regex (verified via
        // real JS: categoryOf({key:'no-var',scope:'project'}) === 'project'), so scope wins here.
        assert_eq!(r["issues"][0]["category"], json!("project"));

        let origin = &r["origin_info"];
        assert_eq!(origin["path"], json!("src/b.mjs"));
        assert_eq!(origin["law_key"], json!("no-var"));
        // `git log -L2,2:src/b.mjs` finds the ONE real commit that introduced line 2 (`var x = 1;`)
        // — no frozen clock here (unlike conform-db fixtures), so assert shape + a real 7-40 hex sha,
        // not a specific wall-clock date.
        let sha = origin["first_commit"]["sha"].as_str().unwrap();
        assert!(sha.len() >= 7 && sha.chars().all(|c| c.is_ascii_hexdigit()), "{sha}");
        assert!(origin["first_commit"]["date"].as_str().unwrap().len() == 10);
    }
}
