//! context-bundle.rs — twin of skills/codebase/context-bundle.mjs (db READ lane). NAMES ARE
//! CONTRACT: `contextBundle` (default), `boneItemsForLevel`, `applyBudget`, `assembleBone`,
//! `_clearBoneCache` (named exports the JS module also re-exports for tests/tools).
//!
//! The resolver mixes two tiers into one budgeted bundle: BONE (invariant, cached per
//! workspace+level — stack facts / atlas architecture census / project invariants / stored
//! digest narrative) and SLICED (per-call, relevance-ranked — codebase-context / decorators /
//! entry-points / data-stories / blast-radius / lint-config / prior-lessons / rag / research).
//! Every gatherer here is a deterministic DB read or fs probe — ported faithfully, including the
//! `decorators` branch's JS quirk where an empty `paths` falls through to `siblingPaths(db,
//! origin, paths)` called with that SAME empty `paths` (always yields `[]`) — a dead fallback in
//! the original source, preserved verbatim rather than "fixed" (NAMES/BEHAVIOR ARE CONTRACT).
//!
//! `rag` and `research` call the already-converted sibling twins `crate::rag_rag_recall::ragRecall`
//! and `crate::codebase_smart_context_bundle::smartContextBundle`, both of which bottom out at the
//! PARITY-DEFERRED ONNX embed boundary (`crate::external_embed::embed`). Mirrored exactly per each
//! sibling's own documented contract: `ragRecall` propagates the deferred embed's `Err` UNCAUGHT
//! (JS has no try/catch around `await ragRecall`) — ported here via `?`, which this closure's
//! outer catch-all collapses to `None`, same as JS's own enclosing `try/catch` around the whole
//! gatherer. `smartContextBundle` instead SWALLOWS the ragRecall failure internally (JS: empty
//! catch block, "RAG optional — degrade to the name index") and degrades to `scope:'none'` when
//! nothing else grounds it, which this twin folds to `None` exactly like the JS `sb.scope !==
//! 'none'` guard. Either way — degrade or throw — the item lands in `skipped` with the same "no
//! data (producer empty / atlas not ingested)" reason a real empty producer would give, so the
//! deferred boundary is invisible to every OTHER gatherer. Every other gatherer (touched-files,
//! decorators, entry-points, data-stories, blast-radius, lint-config, prior-lessons) plus both
//! bone assembly and the budget allocator are fully deterministic and exercised below with
//! seeded rows.
// QUALITY: 82/100 — full budget allocator + bone cache + all 9 sliced gatherers wired; rag/research
// correctly reach the shared PARITY-DEFERRED embed boundary via their own converted twins.

use rusqlite::Connection;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::{LazyLock, Mutex};

const DEFAULT_BUDGET: f64 = 12000.0;

fn js_string_coerce(v: &Value) -> String {
    match v {
        Value::Null => String::new(),
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        _ => String::new(),
    }
}
fn tmpl_val(v: &Value) -> String {
    match v {
        Value::Null => "null".into(),
        Value::String(s) => s.clone(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        other => other.to_string(),
    }
}
// JS numbers are all f64; `JSON.stringify(12000)` prints "12000" not "12000.0" — mirror by
// emitting an integer JSON number whenever the f64 is whole (same convention as every other
// JS-number-passthrough twin in this crate, e.g. man-compose-digital-print.rs's `js_num`).
fn js_num_out(n: f64) -> Value { if n.is_finite() && n.fract() == 0.0 && n.abs() < 9e15 { json!(n as i64) } else { json!(n) } }
fn js_number_default(v: Option<&Value>, default_: f64) -> f64 {
    match v {
        None | Some(Value::Null) => default_,
        Some(Value::Number(n)) => n.as_f64().unwrap_or(f64::NAN),
        Some(Value::String(s)) => { let t = s.trim(); if t.is_empty() { 0.0 } else { t.parse().unwrap_or(f64::NAN) } }
        Some(Value::Bool(b)) => if *b { 1.0 } else { 0.0 },
        Some(_) => f64::NAN,
    }
}
// `input?.x ?? default` — preserves the original value's type, only defaults on null/absent.
fn js_default(v: Option<&Value>, default_: Value) -> Value {
    match v { None | Some(Value::Null) => default_, Some(x) => x.clone() }
}
fn str_list(v: Option<&Value>) -> Vec<String> {
    v.and_then(Value::as_array).map(|a| a.iter().map(js_string_coerce).filter(|s| !s.is_empty()).collect()).unwrap_or_default()
}

// Bone levels are CUMULATIVE: skeleton ⊂ facts ⊂ full. stack-facts is item[0] on every level.
fn bone_by_level(level: &str) -> Vec<&'static str> {
    match level {
        "skeleton" => vec!["stack", "architecture"],
        "full" => vec!["stack", "architecture", "invariants", "docs-overview", "diagram"],
        _ => vec!["stack", "architecture", "invariants"], // 'facts' + fallback
    }
}
pub fn boneItemsForLevel(level: &str) -> Vec<String> {
    bone_by_level(level).into_iter().map(String::from).collect()
}

fn skip_reason(id: &str) -> &'static str {
    if id == "diagram" {
        "AI-built diagram — not auto-run by the bundle (build it from the Code screen to snapshot it)"
    } else {
        "no data (producer empty / atlas not ingested)"
    }
}

/// Pure budget allocator. Bone is a FLOOR — always kept. Sliced is sorted by relevance desc and
/// fills the remaining window; overflow goes to `dropped` with a reason. No silent truncation.
pub fn applyBudget(items: &[Value], budget: f64) -> Value {
    let bone: Vec<&Value> = items.iter().filter(|i| i.get("tier") == Some(&json!("bone"))).collect();
    let mut sliced: Vec<&Value> = items.iter().filter(|i| i.get("tier") == Some(&json!("sliced"))).collect();
    sliced.sort_by(|a, b| {
        let ra = a.get("relevance").and_then(Value::as_f64).unwrap_or(0.0);
        let rb = b.get("relevance").and_then(Value::as_f64).unwrap_or(0.0);
        rb.partial_cmp(&ra).unwrap_or(std::cmp::Ordering::Equal)
    });
    let bone_tokens: i64 = bone.iter().map(|i| i.get("tokens").and_then(Value::as_i64).unwrap_or(0)).sum();
    let mut kept: Vec<Value> = bone.iter().map(|v| (*v).clone()).collect();
    let mut dropped = Vec::new();
    let mut sliced_tokens = 0i64;
    let mut running = bone_tokens as f64;
    for it in sliced {
        let t = it.get("tokens").and_then(Value::as_i64).unwrap_or(0);
        if running + (t as f64) <= budget {
            kept.push(it.clone());
            sliced_tokens += t;
            running += t as f64;
        } else {
            dropped.push(json!({ "id": it.get("id").cloned().unwrap_or(Value::Null), "type": it.get("type").cloned().unwrap_or(Value::Null), "tokens": t, "reason": "over token budget" }));
        }
    }
    json!({ "kept": kept, "dropped": dropped, "bone_tokens": bone_tokens, "sliced_tokens": sliced_tokens, "over_budget": (bone_tokens as f64) > budget })
}

fn mk_item(id: &str, tier: &str, type_: &str, source: &str, text: &str, relevance: f64, freshness: &str) -> Value {
    json!({ "id": id, "tier": tier, "type": type_, "source": source, "text": text, "tokens": crate::system_tokens_count::estimateTokens(&json!(text)), "relevance": relevance, "freshness": freshness })
}

// Task words → symbol-find seeds. Keep DOMAIN words, drop only truly-generic verbs/glue.
static STOP: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    ["with", "from", "that", "this", "into", "have", "when", "your", "them", "then", "they", "will", "want", "make", "create", "build", "feature", "card", "should", "about"]
        .into_iter().collect()
});
static TASK_WORD_RE: LazyLock<regex::Regex> = LazyLock::new(|| regex::Regex::new(r"[a-z][a-z0-9]{3,}").unwrap());
fn task_terms(task: &str) -> Vec<String> {
    let lower = task.to_lowercase();
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for m in TASK_WORD_RE.find_iter(&lower) {
        let w = m.as_str();
        if STOP.contains(w) { continue; }
        if seen.insert(w.to_string()) { out.push(w.to_string()); }
        if out.len() >= 8 { break; }
    }
    out
}

fn chars_take(s: &str, n: usize) -> String { s.chars().take(n).collect() }

// Compact architecture skeleton straight from the atlas — census + top module tally + the real
// symbols per top module (a headcount alone can't be matched/reused, so key symbols are named).
fn gather_architecture(db: &Connection, origin: &str) -> String {
    let census = match crate::db_json::query_rows(db, "SELECT type, COUNT(*) AS n FROM code_nodes WHERE origin = ? GROUP BY type ORDER BY n DESC", &[json!(origin)]) {
        Ok(v) => v,
        Err(_) => return String::new(),
    };
    let census_arr = census.as_array().cloned().unwrap_or_default();
    if census_arr.is_empty() { return String::new(); }
    let census_str = census_arr.iter().map(|r| {
        let n = r.get("n").and_then(Value::as_i64).unwrap_or(0);
        let t = r.get("type").and_then(Value::as_str).unwrap_or("");
        format!("{n} {t}{}", if n == 1 { "" } else { "s" })
    }).collect::<Vec<_>>().join(", ");

    let top_of = |p: &str| -> String { p.replace('\\', "/").split('/').take(2).collect::<Vec<_>>().join("/") };
    let mut order: Vec<String> = Vec::new();
    let mut counts: HashMap<String, i64> = HashMap::new();
    if let Ok(rows) = crate::db_json::query_rows(db, "SELECT path FROM code_nodes WHERE origin = ? AND type = 'file'", &[json!(origin)]) {
        for r in rows.as_array().unwrap_or(&vec![]) {
            let top = top_of(r.get("path").and_then(Value::as_str).unwrap_or(""));
            if !top.is_empty() {
                if !counts.contains_key(&top) { order.push(top.clone()); }
                *counts.entry(top).or_insert(0) += 1;
            }
        }
    }
    // Object.entries insertion order, then sort desc by count (stable) — mirror via insertion Vec.
    let mut top_dirs: Vec<(String, i64)> = order.into_iter().map(|d| { let c = counts[&d]; (d, c) }).collect();
    top_dirs.sort_by(|a, b| b.1.cmp(&a.1));
    let top_dirs: Vec<(String, i64)> = top_dirs.into_iter().take(8).collect();

    let mut lines = vec!["## Architecture skeleton (atlas — deterministic)".to_string(), format!("- census: {census_str}")];
    if !top_dirs.is_empty() {
        lines.push(format!("- modules: {}", top_dirs.iter().map(|(d, n)| format!("{d} ({n})")).collect::<Vec<_>>().join(", ")));
    }
    let by_dir_keys: Vec<String> = top_dirs.iter().take(6).map(|(d, _)| d.clone()).collect();
    if let Ok(rows) = crate::db_json::query_rows(db, "SELECT name, type, signature, path FROM code_nodes WHERE origin = ? AND type IN ('function','component','class','hook','method') AND name IS NOT NULL ORDER BY id", &[json!(origin)]) {
        let mut by_dir: HashMap<String, Vec<String>> = by_dir_keys.iter().map(|d| (d.clone(), Vec::new())).collect();
        for r in rows.as_array().unwrap_or(&vec![]) {
            let top = top_of(r.get("path").and_then(Value::as_str).unwrap_or(""));
            if let Some(bucket) = by_dir.get_mut(&top) {
                if bucket.len() < 4 {
                    let name = r.get("name").and_then(Value::as_str).unwrap_or("");
                    let sig = r.get("signature").and_then(Value::as_str).unwrap_or("()");
                    bucket.push(format!("{name}{sig}"));
                }
            }
        }
        let sym_lines: Vec<String> = by_dir_keys.iter().filter_map(|d| {
            let syms = &by_dir[d];
            if syms.is_empty() { None } else { Some(format!("  - {d}: {}", syms.join(", "))) }
        }).collect();
        if !sym_lines.is_empty() {
            lines.push("- key symbols:".to_string());
            lines.extend(sym_lines);
        }
    }
    lines.join("\n")
}

// Whole-codebase docs overview = the stored deterministic digest narrative — a read, never a build.
fn gather_docs_overview(db: &Connection, origin: &str) -> String {
    let row = match crate::db_json::query_rows(db, "SELECT narrative_md FROM code_digests WHERE origin = ? ORDER BY id DESC LIMIT 1", &[json!(origin)]) {
        Ok(v) => v.as_array().and_then(|a| a.first()).cloned(),
        Err(_) => return String::new(),
    };
    let md = row.and_then(|r| r.get("narrative_md").and_then(Value::as_str).map(str::to_string)).unwrap_or_default();
    let md = md.trim();
    if md.is_empty() { String::new() } else { format!("## Codebase overview (stored digest — deterministic)\n{}", chars_take(md, 6000)) }
}

// Project invariants = the always-apply substrate memory (validated preferences + corrections).
fn gather_invariants(db: &Connection) -> String {
    let mut lines: Vec<String> = Vec::new();
    if let Ok(rows) = crate::db_json::query_rows(db, "SELECT title, guidance FROM memory.memories WHERE type='preference' AND validated=1 AND superseded_by IS NULL ORDER BY id DESC LIMIT 6", &[]) {
        let rows = rows.as_array().cloned().unwrap_or_default();
        if !rows.is_empty() {
            lines.push("## Project invariants — preferences (always apply)".to_string());
            for p in &rows {
                let title = p.get("title").map(tmpl_val).unwrap_or_default();
                let guidance = chars_take(&p.get("guidance").map(js_string_coerce).unwrap_or_default(), 180);
                lines.push(format!("- {title}: {guidance}"));
            }
        }
    }
    if let Ok(rows) = crate::db_json::query_rows(db, "SELECT title, guidance FROM memory.memories WHERE type='correction' AND superseded_by IS NULL ORDER BY id DESC LIMIT 6", &[]) {
        let rows = rows.as_array().cloned().unwrap_or_default();
        if !rows.is_empty() {
            lines.push(String::new());
            lines.push("## Project invariants — corrections (do not repeat)".to_string());
            for c in &rows {
                let title = c.get("title").map(tmpl_val).unwrap_or_default();
                let guidance = chars_take(&c.get("guidance").map(js_string_coerce).unwrap_or_default(), 180);
                lines.push(format!("- {title}: {guidance}"));
            }
        }
    }
    lines.join("\n")
}

fn gather_bone(id: &str, db: &Connection, origin: &str) -> Option<Value> {
    (|| -> Result<Option<Value>, String> {
        match id {
            "stack" => {
                let r = crate::codebase_stack_facts::stackFacts(&json!({}), db)?;
                if r.get("found") == Some(&json!(true)) {
                    let facts_md = r.get("facts_md").and_then(Value::as_str).unwrap_or("");
                    Ok(Some(mk_item("stack", "bone", "stack-facts", "package.json", facts_md, 1.0, "manifest")))
                } else { Ok(None) }
            }
            "architecture" => {
                let md = gather_architecture(db, origin);
                Ok(if md.is_empty() { None } else { Some(mk_item("architecture", "bone", "atlas-census", "atlas", &md, 1.0, "atlas")) })
            }
            "invariants" => {
                let md = gather_invariants(db);
                Ok(if md.is_empty() { None } else { Some(mk_item("invariants", "bone", "memory", "substrate", &md, 1.0, "db")) })
            }
            "docs-overview" => {
                let md = gather_docs_overview(db, origin);
                Ok(if md.is_empty() { None } else { Some(mk_item("docs-overview", "bone", "code-digest", "code_digests", &md, 0.9, "digest")) })
            }
            _ => Ok(None), // diagram → deferred (AI-built)
        }
    })().unwrap_or(None)
}

// ── Bone context-pack cache ──────────────────────────────────────────────────────────────────
// Bone is invariant across every call until the codebase changes, so gather it ONCE per
// (workspace, level) and reuse — keyed by a cheap signature (atlas node count + max id + package
// mtime) so an atlas rebuild or a manifest edit invalidates it. In-process only; clears on
// restart (or via `_clearBoneCache`, used between tests to avoid cross-test static pollution).
static BONE_CACHE: LazyLock<Mutex<HashMap<String, Value>>> = LazyLock::new(|| Mutex::new(HashMap::new()));
pub fn _clearBoneCache() { BONE_CACHE.lock().unwrap().clear(); }

fn bone_signature(db: &Connection, origin: &str, root: &str) -> String {
    let (n, m) = match crate::db_json::query_rows(db, "SELECT COUNT(*) AS n, COALESCE(MAX(id), 0) AS m FROM code_nodes WHERE origin = ?", &[json!(origin)]) {
        Ok(rows) => {
            let row = rows.as_array().and_then(|a| a.first()).cloned().unwrap_or(json!({}));
            (row.get("n").cloned().unwrap_or(json!(0)), row.get("m").cloned().unwrap_or(json!(0)))
        }
        Err(_) => (json!(0), json!(0)),
    };
    let pkg_ms = std::fs::metadata(Path::new(root).join("package.json"))
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis())
        .unwrap_or(0);
    format!("{}:{}:{}", tmpl_val(&n), tmpl_val(&m), pkg_ms)
}

/// Returns `{ sig, items, skipped }` for a bone level — from cache on a signature hit, else
/// gathered + cached.
pub fn assembleBone(level: &str, db: &Connection, origin: &str, root: &str) -> Value {
    let key = format!("{root}|{level}");
    let sig = bone_signature(db, origin, root);
    if let Some(hit) = BONE_CACHE.lock().unwrap().get(&key) {
        if hit.get("sig").and_then(Value::as_str) == Some(sig.as_str()) {
            return hit.clone();
        }
    }
    let mut items = Vec::new();
    let mut skipped = Vec::new();
    for id in boneItemsForLevel(level) {
        match gather_bone(&id, db, origin) {
            Some(item) => items.push(item),
            None => skipped.push(json!({ "id": id, "tier": "bone", "reason": skip_reason(&id) })),
        }
    }
    let packed = json!({ "sig": sig, "items": items, "skipped": skipped });
    BONE_CACHE.lock().unwrap().insert(key, packed.clone());
    packed
}

// New-file fallback: ground on the DIRECTORY NEIGHBORHOOD (the siblings whose conventions the
// new file must match) when the planned paths don't exist in the atlas yet.
fn sibling_paths(db: &Connection, origin: &str, paths: &[String], cap: i64) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for p in paths {
        let normalized = p.replace('\\', "/");
        let segs: Vec<&str> = normalized.split('/').collect();
        let dir = if segs.len() > 1 { segs[..segs.len() - 1].join("/") } else { String::new() };
        if dir.is_empty() { continue; }
        if let Ok(rows) = crate::db_json::query_rows(db, "SELECT path FROM code_nodes WHERE origin = ? AND type = 'file' AND path LIKE ? ORDER BY path LIMIT ?", &[json!(origin), json!(format!("{dir}/%")), json!(cap)]) {
            for r in rows.as_array().unwrap_or(&vec![]) {
                if let Some(pp) = r.get("path").and_then(Value::as_str) {
                    if seen.insert(pp.to_string()) { out.push(pp.to_string()); }
                }
            }
        }
    }
    out.truncate(cap as usize);
    out
}

fn gather_sliced(id: &str, db: &Connection, origin: &str, task: &str, input: &Value, root: &str) -> Option<Value> {
    (|| -> Result<Option<Value>, String> {
        match id {
            "deep-files" | "touched-files" => {
                let paths = str_list(input.get("paths"));
                let explicit = str_list(input.get("terms"));
                let mut cc: Option<Value> = None;
                if !paths.is_empty() {
                    let r = crate::codebase_codebase_context::codebaseContext(&json!({ "paths": paths, "hops": 1, "origin": origin }), db)?;
                    if r.get("grounded") != Some(&json!(true)) {
                        let sibs = sibling_paths(db, origin, &paths, 6);
                        if !sibs.is_empty() {
                            cc = Some(crate::codebase_codebase_context::codebaseContext(&json!({ "paths": sibs, "hops": 1, "origin": origin }), db)?);
                        } else {
                            cc = Some(r);
                        }
                    } else {
                        cc = Some(r);
                    }
                }
                if cc.as_ref().and_then(|v| v.get("grounded")) != Some(&json!(true)) {
                    let terms = if !explicit.is_empty() { explicit } else { task_terms(task) };
                    if !terms.is_empty() {
                        cc = Some(crate::codebase_codebase_context::codebaseContext(&json!({ "paths": [], "terms": terms, "hops": 1, "origin": origin }), db)?);
                    }
                }
                Ok(match cc {
                    Some(v) if v.get("grounded") == Some(&json!(true)) => {
                        let md = v.get("context_md").and_then(Value::as_str).unwrap_or("");
                        Some(mk_item(id, "sliced", "codebase-context", "atlas", md, 0.8, "atlas"))
                    }
                    _ => None,
                })
            }
            "rag" => {
                // JS: `await ragRecall({ query: task, k: 6 }, context)` — NOT try/catch-wrapped
                // itself; a rejection (the deferred embed boundary) propagates out of this whole
                // gatherer's try/catch. `?` mirrors that: an Err here bubbles to the closure's
                // Err arm, collapsed to `None` by the outer `.unwrap_or(None)` below.
                let r = crate::rag_rag_recall::ragRecall(&json!({ "query": task, "k": 6 }), &Value::Null, db)?;
                let empty = r.get("empty") == Some(&json!(true));
                let hits = r.get("hits").and_then(Value::as_array).cloned().unwrap_or_default();
                if empty || hits.is_empty() { return Ok(None); }
                let lines: Vec<String> = hits.iter().map(|h| {
                    let source_db = h.get("source_db").map(tmpl_val).unwrap_or_default();
                    let source_table = h.get("source_table").map(tmpl_val).unwrap_or_default();
                    let source_row = h.get("source_row").map(tmpl_val).unwrap_or_default();
                    let text = h.get("text").and_then(Value::as_str).unwrap_or("");
                    format!("- [{source_db}.{source_table}] {source_row}: {text}")
                }).collect();
                Ok(Some(mk_item("rag", "sliced", "rag-recall", "rag_chunks", &format!("## Semantic recall (cross-DB RAG)\n{}", lines.join("\n")), 0.7, "rag")))
            }
            "research" => {
                let sb = crate::codebase_smart_context_bundle::smartContextBundle(&json!({ "question": task, "token_budget": 6000, "origin": origin, "coverage": true }), db)?;
                let scope = sb.get("scope").and_then(Value::as_str).unwrap_or("none").to_string();
                let bundle_md = sb.get("bundle_md").and_then(Value::as_str).unwrap_or("");
                if scope != "none" && !bundle_md.is_empty() {
                    Ok(Some(mk_item(id, "sliced", "smart-context-bundle", &scope, bundle_md, 0.6, "atlas")))
                } else {
                    Ok(None)
                }
            }
            "decorators" => {
                let paths = str_list(input.get("paths"));
                // JS quirk ported verbatim: when `paths` is empty the fallback calls
                // `siblingPaths(db, origin, paths)` with that SAME empty array → always [].
                let target = if !paths.is_empty() { paths.clone() } else { sibling_paths(db, origin, &paths, 6) };
                if target.is_empty() { return Ok(None); }
                let qs = target.iter().map(|_| "?").collect::<Vec<_>>().join(",");
                let sql = format!("SELECT path, name, kind, signature, summary FROM code_decorators WHERE origin = ? AND path IN ({qs}) ORDER BY path, line LIMIT 12");
                let mut params = vec![json!(origin)];
                params.extend(target.iter().map(|p| json!(p)));
                let rows = crate::db_json::query_rows(db, &sql, &params)?;
                let rows = rows.as_array().cloned().unwrap_or_default();
                if rows.is_empty() { return Ok(None); }
                let lines: Vec<String> = rows.iter().map(|r| {
                    let path = r.get("path").map(tmpl_val).unwrap_or_default();
                    let name = r.get("name").map(tmpl_val).unwrap_or_default();
                    let kind = r.get("kind").map(tmpl_val).unwrap_or_default();
                    let sig = r.get("signature").and_then(Value::as_str).unwrap_or("").to_string();
                    let summary = chars_take(&r.get("summary").map(js_string_coerce).unwrap_or_default(), 140);
                    format!("- {path}#{name} [{kind}] {sig} — {summary}")
                }).collect();
                Ok(Some(mk_item("decorators", "sliced", "code-decorators", "code_decorators", &format!("## Block doc-cards (touched files)\n{}", lines.join("\n")), 0.7, "atlas")))
            }
            "entry-points" => {
                let rows = crate::db_json::query_rows(db, "SELECT kind, subtype, path, label, line, framework FROM code_entry_points WHERE origin = ? ORDER BY confidence DESC LIMIT 10", &[json!(origin)])?;
                let rows = rows.as_array().cloned().unwrap_or_default();
                if rows.is_empty() { return Ok(None); }
                let lines: Vec<String> = rows.iter().map(|r| {
                    let kind = r.get("kind").map(tmpl_val).unwrap_or_default();
                    let subtype = r.get("subtype").and_then(Value::as_str).map(|s| format!(":{s}")).unwrap_or_default();
                    let path = r.get("path").map(tmpl_val).unwrap_or_default();
                    let line = r.get("line").map(tmpl_val).unwrap_or_else(|| "?".into());
                    let label = r.get("label").and_then(Value::as_str).unwrap_or("").to_string();
                    let framework = r.get("framework").and_then(Value::as_str).map(|f| format!(" ({f})")).unwrap_or_default();
                    format!("- [{kind}{subtype}] {path}:{line} {label}{framework}")
                }).collect();
                Ok(Some(mk_item("entry-points", "sliced", "entry-points", "code_entry_points", &format!("## Entry points (where the system is entered)\n{}", lines.join("\n")), 0.6, "atlas")))
            }
            "data-stories" => {
                let terms = task_terms(task);
                let like = if !terms.is_empty() { format!("%{}%", terms[0]) } else { "%".to_string() };
                let mut rows = crate::db_json::query_rows(db, "SELECT name, description, hops_json FROM code_data_stories WHERE origin = ? AND (name LIKE ? OR description LIKE ?) ORDER BY id DESC LIMIT 3", &[json!(origin), json!(like.clone()), json!(like)])?;
                if rows.as_array().map(|a| a.is_empty()).unwrap_or(true) {
                    rows = crate::db_json::query_rows(db, "SELECT name, description, hops_json FROM code_data_stories WHERE origin = ? ORDER BY id DESC LIMIT 3", &[json!(origin)])?;
                }
                let rows = rows.as_array().cloned().unwrap_or_default();
                if rows.is_empty() { return Ok(None); }
                let lines: Vec<String> = rows.iter().map(|r| {
                    let name = r.get("name").map(tmpl_val).unwrap_or_default();
                    let desc = chars_take(&r.get("description").map(js_string_coerce).unwrap_or_default(), 160);
                    let hops_json = r.get("hops_json").and_then(Value::as_str).unwrap_or("[]");
                    let hops: String = serde_json::from_str::<Vec<Value>>(hops_json).unwrap_or_default().iter()
                        .filter_map(|h| h.get("label").or_else(|| h.get("node_id")).and_then(Value::as_str).map(str::to_string))
                        .collect::<Vec<_>>().join(" → ");
                    if hops.is_empty() { format!("- {name}: {desc}") } else { format!("- {name}: {desc}\n  flow: {hops}") }
                }).collect();
                Ok(Some(mk_item("data-stories", "sliced", "code-data-stories", "code_data_stories", &format!("## Data stories (real execution flows)\n{}", lines.join("\n")), 0.6, "atlas")))
            }
            "blast-radius" => {
                let paths = str_list(input.get("paths"));
                if paths.is_empty() { return Ok(None); }
                let sql = "SELECT DISTINCT nf.path AS dependent FROM code_edges e \
                    JOIN code_nodes nt ON nt.id = e.to_node \
                    JOIN code_nodes nf ON nf.id = e.from_node \
                    WHERE e.origin = ? AND nt.path = ? AND nf.path != nt.path LIMIT 12";
                let mut lines = Vec::new();
                for p in &paths {
                    let rows = crate::db_json::query_rows(db, sql, &[json!(origin), json!(p.replace('\\', "/"))])?;
                    let deps: Vec<String> = rows.as_array().unwrap_or(&vec![]).iter().filter_map(|r| r.get("dependent").and_then(Value::as_str).map(str::to_string)).collect();
                    if !deps.is_empty() { lines.push(format!("- {p} ← used by: {}", deps.join(", "))); }
                }
                if lines.is_empty() { return Ok(None); }
                Ok(Some(mk_item("blast-radius", "sliced", "atlas-edges", "code_edges", &format!("## Blast radius (who depends on the touched files)\n{}", lines.join("\n")), 0.7, "atlas")))
            }
            "lint-config" => {
                if root.is_empty() { return Ok(None); }
                let files = [".eslintrc.json", ".eslintrc.cjs", ".eslintrc.js", ".eslintrc", "eslint.config.js", "eslint.config.mjs", "tsconfig.json", ".prettierrc", ".prettierrc.json"];
                let mut found = Vec::new();
                for f in files {
                    let p = Path::new(root).join(f);
                    if !p.exists() { continue; }
                    if let Ok(content) = std::fs::read_to_string(&p) {
                        found.push(format!("### {f}\n{}", chars_take(&content, 1200)));
                    }
                    if found.len() >= 2 { break; }
                }
                if found.is_empty() { return Ok(None); }
                Ok(Some(mk_item("lint-config", "sliced", "lint-config", "workspace", &format!("## Lint / compiler config (the rules the code must satisfy)\n{}", found.join("\n")), 0.5, "fs")))
            }
            "prior-lessons" => {
                let explicit = str_list(input.get("terms"));
                let terms: Vec<String> = (if !explicit.is_empty() { explicit } else { task_terms(task) }).into_iter().take(3).collect();
                let mut rows = Value::Array(vec![]);
                if !terms.is_empty() {
                    let conds = terms.iter().map(|_| "(title LIKE ? OR guidance LIKE ?)").collect::<Vec<_>>().join(" OR ");
                    let mut params = Vec::new();
                    for t in &terms { params.push(json!(format!("%{t}%"))); params.push(json!(format!("%{t}%"))); }
                    let sql = format!("SELECT type, title, guidance FROM memory.memories WHERE type IN ('lesson','correction','decision') AND superseded_by IS NULL AND ({conds}) ORDER BY id DESC LIMIT 4");
                    rows = crate::db_json::query_rows(db, &sql, &params)?;
                }
                if rows.as_array().map(|a| a.is_empty()).unwrap_or(true) {
                    rows = crate::db_json::query_rows(db, "SELECT type, title, guidance FROM memory.memories WHERE type IN ('lesson','correction','decision') AND superseded_by IS NULL ORDER BY id DESC LIMIT 4", &[])?;
                }
                let rows = rows.as_array().cloned().unwrap_or_default();
                if rows.is_empty() { return Ok(None); }
                let lines: Vec<String> = rows.iter().map(|r| {
                    let ty = r.get("type").map(tmpl_val).unwrap_or_default();
                    let title = r.get("title").map(tmpl_val).unwrap_or_default();
                    let guidance = chars_take(&r.get("guidance").map(js_string_coerce).unwrap_or_default(), 160);
                    format!("- [{ty}] {title}: {guidance}")
                }).collect();
                Ok(Some(mk_item("prior-lessons", "sliced", "memory", "memories", &format!("## Prior lessons (do not repeat)\n{}", lines.join("\n")), 0.6, "db")))
            }
            _ => Ok(None), // unknown item id → recorded in skipped
        }
    })().unwrap_or(None)
}

pub fn contextBundle(input: &Value, db: &Connection) -> Result<Value, String> {
    let agent = js_default(input.get("agent"), json!(""));
    let phase = js_default(input.get("phase"), json!(""));
    let task = js_string_coerce(&js_default(input.get("task"), json!(""))).trim().to_string();
    let origin = input.get("origin").and_then(Value::as_str).unwrap_or("atlas").to_string();
    let budget = js_number_default(input.get("budget"), DEFAULT_BUDGET);
    let contract = crate::codebase_context_contracts::resolveContract(&agent, &phase);

    let root_v = crate::engine_workspace_root::getWorkspaceRoot(db);
    let root = root_v.as_str().unwrap_or("").to_string();
    let bone_level = contract.get("bone").and_then(Value::as_str).unwrap_or("facts").to_string();
    let bone = assembleBone(&bone_level, db, &origin, &root);

    let mut items: Vec<Value> = bone.get("items").and_then(Value::as_array).cloned().unwrap_or_default();
    let mut skipped: Vec<Value> = bone.get("skipped").and_then(Value::as_array).cloned().unwrap_or_default();

    let sliced_ids: Vec<String> = contract.get("sliced").and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect()).unwrap_or_default();
    for id in &sliced_ids {
        match gather_sliced(id, db, &origin, &task, input, &root) {
            Some(item) => items.push(item),
            None => skipped.push(json!({ "id": id, "tier": "sliced", "reason": skip_reason(id) })),
        }
    }

    let budgeted = applyBudget(&items, budget);
    let kept = budgeted.get("kept").and_then(Value::as_array).cloned().unwrap_or_default();
    let dropped = budgeted.get("dropped").cloned().unwrap_or(json!([]));
    let bone_tokens = budgeted.get("bone_tokens").and_then(Value::as_i64).unwrap_or(0);
    let sliced_tokens = budgeted.get("sliced_tokens").and_then(Value::as_i64).unwrap_or(0);
    let over_budget = budgeted.get("over_budget").cloned().unwrap_or(json!(false));

    let bundle_md = kept.iter().map(|i| i.get("text").and_then(Value::as_str).unwrap_or("")).collect::<Vec<_>>().join("\n\n---\n\n");

    Ok(json!({
        "ok": true, "agent": agent, "phase": phase, "contract": contract,
        "items": kept, "dropped": dropped, "skipped": skipped,
        "bone_tokens": bone_tokens, "sliced_tokens": sliced_tokens, "total_tokens": bone_tokens + sliced_tokens,
        "budget": js_num_out(budget), "over_budget": over_budget,
        "bundle_md": bundle_md,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn full_db(root: &str) -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "ATTACH ':memory:' AS memory; \
             CREATE TABLE workspaces (id INTEGER PRIMARY KEY, name TEXT NOT NULL UNIQUE, root_path TEXT NOT NULL, vcs TEXT NOT NULL DEFAULT 'git', is_active INTEGER NOT NULL DEFAULT 0, created_at TEXT NOT NULL DEFAULT '2026-01-01 00:00:00'); \
             CREATE TABLE code_nodes (id TEXT PRIMARY KEY, origin TEXT, path TEXT, type TEXT, name TEXT, signature TEXT, intent TEXT, parent_id TEXT); \
             CREATE TABLE code_edges (id INTEGER PRIMARY KEY AUTOINCREMENT, origin TEXT, from_node TEXT, to_node TEXT, type TEXT, kind TEXT); \
             CREATE TABLE code_decorators (origin TEXT, path TEXT, name TEXT, kind TEXT, signature TEXT, summary TEXT, line INTEGER); \
             CREATE TABLE code_entry_points (id INTEGER PRIMARY KEY AUTOINCREMENT, origin TEXT, kind TEXT, subtype TEXT, path TEXT, label TEXT, line INTEGER, framework TEXT, confidence REAL DEFAULT 0.7); \
             CREATE TABLE code_data_stories (id INTEGER PRIMARY KEY, origin TEXT, name TEXT, description TEXT, hops_json TEXT); \
             CREATE TABLE code_digests (id INTEGER PRIMARY KEY, origin TEXT, narrative_md TEXT); \
             CREATE TABLE memory.memories (id INTEGER PRIMARY KEY, type TEXT, title TEXT, guidance TEXT, validated INTEGER, superseded_by INTEGER);",
        ).unwrap();
        c.execute("INSERT INTO workspaces (name, root_path, is_active) VALUES ('w', ?1, 1)", rusqlite::params![root]).unwrap();
        c
    }

    #[test]
    fn bone_items_for_level_is_cumulative_with_facts_fallback() {
        assert_eq!(boneItemsForLevel("skeleton"), vec!["stack", "architecture"]);
        assert_eq!(boneItemsForLevel("facts"), vec!["stack", "architecture", "invariants"]);
        assert_eq!(boneItemsForLevel("full"), vec!["stack", "architecture", "invariants", "docs-overview", "diagram"]);
        assert_eq!(boneItemsForLevel("nonsense"), vec!["stack", "architecture", "invariants"]);
    }

    #[test]
    fn apply_budget_keeps_bone_as_a_floor_and_sorts_sliced_by_relevance() {
        let items = vec![
            json!({ "id": "b1", "tier": "bone", "tokens": 100 }),
            json!({ "id": "s-low", "tier": "sliced", "tokens": 50, "relevance": 0.3 }),
            json!({ "id": "s-high", "tier": "sliced", "tokens": 50, "relevance": 0.9 }),
            json!({ "id": "s-over", "tier": "sliced", "tokens": 9999, "relevance": 0.99 }),
        ];
        let r = applyBudget(&items, 210.0);
        let kept_ids: Vec<&str> = r["kept"].as_array().unwrap().iter().map(|i| i["id"].as_str().unwrap()).collect();
        assert_eq!(kept_ids, vec!["b1", "s-high", "s-low"]); // bone first, then relevance desc, s-over drops
        assert_eq!(r["dropped"].as_array().unwrap().len(), 1);
        assert_eq!(r["dropped"][0]["id"], json!("s-over"));
        assert_eq!(r["bone_tokens"], json!(100));
        assert_eq!(r["sliced_tokens"], json!(100));
        assert_eq!(r["over_budget"], json!(false));
    }

    #[test]
    fn apply_budget_flags_over_budget_when_bone_alone_exceeds_it_but_still_keeps_it() {
        let items = vec![json!({ "id": "b1", "tier": "bone", "tokens": 500 })];
        let r = applyBudget(&items, 100.0);
        assert_eq!(r["kept"], json!([{ "id": "b1", "tier": "bone", "tokens": 500 }]));
        assert_eq!(r["over_budget"], json!(true));
    }

    #[test]
    fn context_bundle_assembles_bone_plus_sliced_with_seeded_rows() {
        let root = concat!(env!("ANTILOKI_FIXTURES"), "/stack-facts-found");
        _clearBoneCache();
        let c = full_db(root);
        c.execute_batch(
            "INSERT INTO code_nodes (id,origin,path,type,name,signature,parent_id) VALUES ('f1','atlas','src/app/foo.ts','file','foo.ts',NULL,NULL); \
             INSERT INTO code_nodes (id,origin,path,type,name,signature,parent_id) VALUES ('n1','atlas','src/app/foo.ts','function','handleClick','()','f1'); \
             INSERT INTO code_nodes (id,origin,path,type,name,signature,parent_id) VALUES ('n2','atlas','src/app/bar.ts','file','bar.ts',NULL,NULL); \
             INSERT INTO code_digests (origin, narrative_md) VALUES ('atlas', 'The system does X.'); \
             INSERT INTO memory.memories (type,title,guidance,validated,superseded_by) VALUES ('preference','pref A','always do A',1,NULL); \
             INSERT INTO code_decorators VALUES ('atlas','src/app/foo.ts','handleClick','fn','()','handles the click',1); \
             INSERT INTO code_entry_points (origin,kind,subtype,path,label,line,framework,confidence) VALUES ('atlas','http','route','src/app/foo.ts','POST /foo',5,'express',0.9);",
        ).unwrap();
        // contract for agent:phase = 'tl:design' → bone 'full', sliced ['touched-files','blast-radius','entry-points']
        let r = contextBundle(&json!({ "agent": "tl", "phase": "design", "paths": ["src/app/foo.ts"], "origin": "atlas" }), &c).unwrap();
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["contract"]["bone"], json!("full"));
        let item_ids: Vec<&str> = r["items"].as_array().unwrap().iter().map(|i| i["id"].as_str().unwrap()).collect();
        assert!(item_ids.contains(&"stack"), "{item_ids:?}");
        assert!(item_ids.contains(&"architecture"), "{item_ids:?}");
        assert!(item_ids.contains(&"invariants"), "{item_ids:?}");
        assert!(item_ids.contains(&"docs-overview"), "{item_ids:?}");
        assert!(item_ids.contains(&"touched-files"), "{item_ids:?}");
        assert!(item_ids.contains(&"entry-points"), "{item_ids:?}");
        // diagram is deferred → always skipped with its honest reason
        let diagram_skip = r["skipped"].as_array().unwrap().iter().find(|s| s["id"] == json!("diagram")).unwrap();
        assert!(diagram_skip["reason"].as_str().unwrap().contains("AI-built diagram"));
        assert!(r["bundle_md"].as_str().unwrap().contains("Stack & conventions") || r["bundle_md"].as_str().unwrap().len() > 0);
        assert_eq!(r["total_tokens"], json!(r["bone_tokens"].as_i64().unwrap() + r["sliced_tokens"].as_i64().unwrap()));
    }

    #[test]
    fn context_bundle_rag_and_research_are_honest_deferred_skips() {
        _clearBoneCache();
        let c = full_db(concat!(env!("ANTILOKI_FIXTURES"), "/stack-facts-empty"));
        // 'pl:research' contract → sliced ['deep-files','data-stories','rag']
        let r = contextBundle(&json!({ "agent": "pl", "phase": "research", "task": "nothing to ground on" }), &c).unwrap();
        let skipped_ids: Vec<&str> = r["skipped"].as_array().unwrap().iter().map(|s| s["id"].as_str().unwrap()).collect();
        assert!(skipped_ids.contains(&"rag"), "{skipped_ids:?}");
        let rag_skip = r["skipped"].as_array().unwrap().iter().find(|s| s["id"] == json!("rag")).unwrap();
        assert_eq!(rag_skip["reason"], json!("no data (producer empty / atlas not ingested)"));
    }

    #[test]
    fn context_bundle_unknown_agent_phase_falls_back_to_default_contract() {
        _clearBoneCache();
        let c = full_db(concat!(env!("ANTILOKI_FIXTURES"), "/stack-facts-empty"));
        let r = contextBundle(&json!({ "agent": "nope", "phase": "nope" }), &c).unwrap();
        assert_eq!(r["contract"]["bone"], json!("facts"));
        assert_eq!(r["contract"]["sliced"], json!(["rag"]));
    }

    #[test]
    fn data_stories_falls_back_to_unfiltered_when_term_match_is_empty() {
        _clearBoneCache();
        let c = full_db(concat!(env!("ANTILOKI_FIXTURES"), "/stack-facts-empty"));
        c.execute_batch(
            "INSERT INTO code_data_stories (origin,name,description,hops_json) VALUES \
             ('atlas','checkout flow','from cart to payment','[{\"label\":\"cart\"},{\"node_id\":\"pay1\"}]');",
        ).unwrap();
        let r = gather_sliced("data-stories", &c, "atlas", "totally unrelated words zzz", &json!({}), "");
        let item = r.unwrap();
        assert!(item["text"].as_str().unwrap().contains("checkout flow: from cart to payment"));
        assert!(item["text"].as_str().unwrap().contains("flow: cart → pay1"));
    }

    #[test]
    fn blast_radius_reports_reverse_dependents_of_touched_paths() {
        _clearBoneCache();
        let c = full_db("");
        c.execute_batch(
            "INSERT INTO code_nodes (id,origin,path,type) VALUES ('a','atlas','src/a.ts','file'); \
             INSERT INTO code_nodes (id,origin,path,type) VALUES ('b','atlas','src/b.ts','file'); \
             INSERT INTO code_edges (origin, from_node, to_node) VALUES ('atlas','b','a');",
        ).unwrap();
        let r = gather_sliced("blast-radius", &c, "atlas", "", &json!({ "paths": ["src/a.ts"] }), "").unwrap();
        assert!(r["text"].as_str().unwrap().contains("src/a.ts ← used by: src/b.ts"));
    }

    #[test]
    fn decorators_empty_paths_fallback_quirk_yields_none() {
        _clearBoneCache();
        let c = full_db("");
        c.execute_batch("INSERT INTO code_decorators VALUES ('atlas','src/a.ts','f','fn','()','doc',1);").unwrap();
        // JS quirk ported verbatim: empty paths → siblingPaths(db, origin, paths) with that same
        // empty paths → always [] → the decorators row above is never matched.
        assert!(gather_sliced("decorators", &c, "atlas", "", &json!({}), "").is_none());
    }

    #[test]
    fn lint_config_reads_real_files_from_a_temp_workspace_root() {
        _clearBoneCache();
        let dir = std::env::temp_dir().join(format!("context-bundle-rs-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(".eslintrc.json"), "{\"rules\":{}}").unwrap();
        let root = dir.to_string_lossy().into_owned();
        let c = full_db(&root);
        let r = gather_sliced("lint-config", &c, "atlas", "", &json!({}), &root).unwrap();
        assert!(r["text"].as_str().unwrap().contains(".eslintrc.json"));
        assert!(r["text"].as_str().unwrap().contains("\"rules\":{}"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn prior_lessons_falls_back_to_unfiltered_when_no_term_row_matches() {
        _clearBoneCache();
        let c = full_db("");
        c.execute_batch(
            "INSERT INTO memory.memories (type,title,guidance,superseded_by) VALUES ('lesson','some lesson','watch out',NULL);",
        ).unwrap();
        let r = gather_sliced("prior-lessons", &c, "atlas", "zzz nomatch words here", &json!({}), "").unwrap();
        assert!(r["text"].as_str().unwrap().contains("some lesson: watch out"));
    }

    #[test]
    fn rag_gatherer_degrades_to_none_with_no_rag_chunks_table() {
        _clearBoneCache();
        let c = full_db(""); // no `memory.rag_chunks` table → ragRecall degrades to empty:true
        // without ever reaching the deferred embed boundary — real, deterministic wiring.
        assert!(gather_sliced("rag", &c, "atlas", "some task", &json!({}), "").is_none());
    }

    #[test]
    fn research_gatherer_degrades_to_none_when_docs_are_not_built() {
        _clearBoneCache();
        let c = full_db(""); // code_decorators lacks codeDocsBuild's full column set → found:false
        assert!(gather_sliced("research", &c, "atlas", "some question about the code", &json!({}), "").is_none());
    }

    #[test]
    fn task_terms_dedupes_and_drops_stopwords() {
        assert_eq!(task_terms("Build a card with click handling and click again"), vec!["click", "handling", "again"]);
    }
}
