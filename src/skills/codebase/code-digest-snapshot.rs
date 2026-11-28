//! code-digest-snapshot.rs — twin of skills/codebase/code-digest-snapshot.mjs (db lane, dispatcher).
//! NAMES ARE CONTRACT: `codeDigestSnapshot` (default). Computes a DETERMINISTIC full code-review of
//! the atlas (reusing atlas-metrics/score/smells/test-map + the file→file import-graph shape) and
//! PERSISTS it as one code_digests row (INSERT driven by the digest-spec column list). Every section
//! is graph-derived except narrative_md — the one templated AI field, which routes through
//! architecture-digest's PARITY-DEFERRED aiCall stub and degrades to null on any failure, exactly
//! like the JS's `try { ... } catch { narrative = null }`. Best-effort persist: a missing/read-only
//! code_digests table still returns the live sections (INSERT failure is swallowed, matching the
//! JS's own `try { INSERT } catch {}`).
//!
//! `async`/`await` in the JS source is "async over sync" (per CONVERSION-PLAYBOOK §72): every await
//! resolves a sync db call or the deferred aiCall stub, so this twin is an ordinary sync `pub fn`.
use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::collections::{HashMap, HashSet};

fn round_d(x: f64, d: i32) -> f64 {
    let p = 10f64.powi(d);
    (x * p).round() / p
}
// `/\.[^./\\]+$/` — last dot-extension, lowercase; else '(none)'.
fn ext_of(path: &str) -> String {
    let bytes: Vec<char> = path.chars().collect();
    let mut i = bytes.len();
    while i > 0 {
        i -= 1;
        let c = bytes[i];
        if c == '.' {
            let tail: String = bytes[i..].iter().collect();
            if tail.len() > 1 && !tail[1..].contains(['.', '/', '\\']) {
                return tail.to_lowercase();
            }
            return "(none)".to_string();
        }
        if c == '/' || c == '\\' { break; }
    }
    "(none)".to_string()
}

// ── the file→file import graph (both endpoints in-scope file nodes) ────────────────────────────
struct Graph {
    file_ids: Vec<String>,
    paths: HashMap<String, String>,
    adj: HashMap<String, Vec<String>>,
    fin: HashMap<String, i64>,
    fout: HashMap<String, i64>,
    edges_count: i64,
}

fn import_graph(db: &Connection, origin: &str) -> Result<Graph, String> {
    let files_rows = crate::db_json::query_rows(db, "SELECT id, path FROM code_nodes WHERE origin = ? AND type = 'file'", &[json!(origin)])?;
    let files_rows = files_rows.as_array().cloned().unwrap_or_default();
    let mut file_ids: Vec<String> = Vec::new();
    let mut paths: HashMap<String, String> = HashMap::new();
    let mut file_id_set: HashSet<String> = HashSet::new();
    let mut adj: HashMap<String, Vec<String>> = HashMap::new();
    for f in &files_rows {
        let id = f.get("id").and_then(Value::as_str).unwrap_or("").to_string();
        let path = f.get("path").and_then(Value::as_str).unwrap_or("").to_string();
        file_id_set.insert(id.clone());
        file_ids.push(id.clone());
        paths.insert(id.clone(), path);
        adj.insert(id, Vec::new());
    }
    let edges_rows = crate::db_json::query_rows(db, "SELECT from_node, to_node FROM code_edges WHERE origin = ? AND type = 'imports'", &[json!(origin)])?;
    let edges_rows = edges_rows.as_array().cloned().unwrap_or_default();
    let mut fin: HashMap<String, i64> = HashMap::new();
    let mut fout: HashMap<String, i64> = HashMap::new();
    let mut edges_count = 0i64;
    for e in &edges_rows {
        let from = e.get("from_node").and_then(Value::as_str).unwrap_or("").to_string();
        let to = e.get("to_node").and_then(Value::as_str).unwrap_or("").to_string();
        if file_id_set.contains(&from) && file_id_set.contains(&to) {
            adj.entry(from.clone()).or_default().push(to.clone());
            *fout.entry(from).or_insert(0) += 1;
            *fin.entry(to).or_insert(0) += 1;
            edges_count += 1;
        }
    }
    Ok(Graph { file_ids, paths, adj, fin, fout, edges_count })
}

// Longest import chain (cycle-safe: a back-edge contributes 0). Memoized DFS with an on-stack guard.
fn max_depth(g: &Graph) -> i64 {
    fn lp(id: &str, g: &Graph, memo: &mut HashMap<String, i64>, stack: &mut HashSet<String>) -> i64 {
        if let Some(v) = memo.get(id) { return *v; }
        if stack.contains(id) { return 0; }
        stack.insert(id.to_string());
        let mut best = 0i64;
        if let Some(kids) = g.adj.get(id) {
            for t in kids { best = best.max(1 + lp(t, g, memo, stack)); }
        }
        stack.remove(id);
        memo.insert(id.to_string(), best);
        best
    }
    let mut memo: HashMap<String, i64> = HashMap::new();
    let mut stack: HashSet<String> = HashSet::new();
    let mut best = 0i64;
    for f in &g.file_ids { best = best.max(lp(f, g, &mut memo, &mut stack)); }
    best
}

// Number of import cycles = strongly-connected components of size > 1 (+ self-loops). Recursive
// Tarjan — the SCC classification (and therefore the count) is order-independent of iterative-vs-
// recursive traversal style; fixtures stay small so recursion depth is not a concern.
#[allow(clippy::too_many_arguments)]
fn strongconnect(
    v: &str, g: &Graph, index: &mut i64,
    idx: &mut HashMap<String, i64>, low: &mut HashMap<String, i64>,
    on_stack: &mut HashSet<String>, comp_stack: &mut Vec<String>, cycles: &mut i64,
) {
    idx.insert(v.to_string(), *index);
    low.insert(v.to_string(), *index);
    *index += 1;
    comp_stack.push(v.to_string());
    on_stack.insert(v.to_string());
    if let Some(kids) = g.adj.get(v).cloned() {
        for w in &kids {
            if !idx.contains_key(w) {
                strongconnect(w, g, index, idx, low, on_stack, comp_stack, cycles);
                let lw = *low.get(w).unwrap();
                let lv = *low.get(v).unwrap();
                low.insert(v.to_string(), lv.min(lw));
            } else if on_stack.contains(w) {
                let iw = *idx.get(w).unwrap();
                let lv = *low.get(v).unwrap();
                low.insert(v.to_string(), lv.min(iw));
            }
        }
    }
    if low.get(v) == idx.get(v) {
        let mut comp: Vec<String> = Vec::new();
        loop {
            let w = comp_stack.pop().unwrap();
            on_stack.remove(&w);
            let is_v = w == v;
            comp.push(w);
            if is_v { break; }
        }
        if comp.len() > 1 { *cycles += 1; }
        else if g.adj.get(v).map(|k| k.contains(&v.to_string())).unwrap_or(false) { *cycles += 1; }
    }
}
fn cycle_count(g: &Graph) -> i64 {
    let mut index = 0i64;
    let mut idx: HashMap<String, i64> = HashMap::new();
    let mut low: HashMap<String, i64> = HashMap::new();
    let mut on_stack: HashSet<String> = HashSet::new();
    let mut comp_stack: Vec<String> = Vec::new();
    let mut cycles = 0i64;
    for f in g.file_ids.clone() {
        if !idx.contains_key(&f) {
            strongconnect(&f, g, &mut index, &mut idx, &mut low, &mut on_stack, &mut comp_stack, &mut cycles);
        }
    }
    cycles
}

// The ordered code_digests column list (mirrors digest-spec.mjs DIGEST_COLUMNS' `col` field —
// the INSERT and the row shape are driven by this list, same as the JS import).
const DIGEST_COLS: [&str; 26] = [
    "total_files_num", "total_symbols_num", "total_edges_num", "total_loc_num", "languages_json",
    "avg_fan_in_num", "avg_fan_out_num", "max_depth_num", "cycles_num", "orphans_json", "hubs_json",
    "health_score_num", "risk_score_num", "top_central_json", "top_risky_json", "top_complex_json",
    "coverage_pct", "tested_files_num", "untested_files_num", "test_files_num",
    "entry_points_json", "smells_total_num", "smells_by_kind_json",
    "largest_files_json", "top_churn_json", "narrative_md",
];

pub fn codeDigestSnapshot(input: &Value, db: &Connection) -> Result<Value, String> {
    let origin = input.get("origin").filter(|v| !v.is_null()).and_then(Value::as_str).unwrap_or("atlas").to_string();
    let root: Value = input.get("root").cloned().unwrap_or(Value::Null);
    const TOP: usize = 8;

    // ── deterministic signals (reuse the existing read-only layer) ──
    let m = crate::codebase_atlas_metrics::atlasMetrics(&json!({ "origin": origin, "root": root, "top": TOP as i64 }), db)?;
    let file_count = m.get("file_count").and_then(Value::as_i64).unwrap_or(0);
    if file_count == 0 {
        return Ok(json!({ "ok": false, "origin": origin, "error": format!("no atlas for origin '{origin}' — ingest/rebuild first") }));
    }
    let score = crate::codebase_atlas_score::atlasScore(&json!({ "origin": origin, "root": root, "top": TOP as i64 }), db)?;
    let smells = crate::codebase_atlas_smells::atlasSmells(&json!({ "origin": origin }), db)?;
    let cov = crate::codebase_atlas_test_map::atlasTestMap(&json!({ "origin": origin }), db)?;
    let g = import_graph(db, &origin)?;

    let metrics_by_file = m.get("metrics_by_file").and_then(Value::as_object).cloned().unwrap_or_default();
    let is_test_set: HashSet<String> = metrics_by_file.iter()
        .filter(|(_, v)| v.get("is_test").and_then(Value::as_bool).unwrap_or(false))
        .map(|(k, _)| k.clone()).collect();

    // LOC: atlas file nodes carry no span, so count lines from source directly. Bounded guard;
    // unreadable files are skipped (mirrors the JS try/catch per file).
    const LOC_CAP: usize = 6000;
    let mut loc_by_path: Vec<(String, i64)> = Vec::new();
    let mut total_loc: i64 = 0;
    if g.file_ids.len() <= LOC_CAP {
        for id in &g.file_ids {
            let path = g.paths.get(id).cloned().unwrap_or_default();
            if let Ok(r) = crate::codebase_codebase_file_read::codebaseFileRead(&json!({ "path": path }), db) {
                if let Some(src) = r.get("content").and_then(Value::as_str) {
                    let n = if src.is_empty() { 0 } else { src.split('\n').count() as i64 };
                    loc_by_path.push((path.clone(), n));
                    total_loc += n;
                }
            }
        }
    }

    let total_symbols = crate::db_json::query_rows(db, "SELECT count(*) n FROM code_nodes WHERE origin = ? AND type <> 'file'", &[json!(origin)])?
        .as_array().and_then(|a| a.first()).and_then(|r| r.get("n")).and_then(Value::as_i64).unwrap_or(0);
    let total_edges = crate::db_json::query_rows(db, "SELECT count(*) n FROM code_edges WHERE origin = ?", &[json!(origin)])?
        .as_array().and_then(|a| a.first()).and_then(|r| r.get("n")).and_then(Value::as_i64).unwrap_or(0);

    // languages by extension (over file nodes)
    let mut langs: Map<String, Value> = Map::new();
    for id in &g.file_ids {
        let path = g.paths.get(id).cloned().unwrap_or_default();
        let e = ext_of(&path);
        let c = langs.get(&e).and_then(Value::as_i64).unwrap_or(0) + 1;
        langs.insert(e, json!(c));
    }

    // structure
    let mut orphans: Vec<String> = g.file_ids.iter()
        .filter(|id| !(g.fin.get(*id).copied().unwrap_or(0) > 0) && !(g.fout.get(*id).copied().unwrap_or(0) > 0))
        .map(|id| g.paths.get(id).cloned().unwrap_or_default())
        .collect();
    orphans.sort();
    let mut hubs: Vec<(String, i64)> = g.file_ids.iter()
        .map(|id| (g.paths.get(id).cloned().unwrap_or_default(), g.fin.get(id).copied().unwrap_or(0)))
        .filter(|(_, fan_in)| *fan_in > 0)
        .collect();
    hubs.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    hubs.truncate(12);

    // surface (external touch points) — best-effort (table may be absent)
    let mut surface: Map<String, Value> = Map::new();
    surface.insert("route".into(), json!(0));
    surface.insert("ui".into(), json!(0));
    surface.insert("data".into(), json!(0));
    surface.insert("total".into(), json!(0));
    if let Ok(rows) = crate::db_json::query_rows(db, "SELECT kind, count(*) n FROM code_entry_points WHERE origin = ? GROUP BY kind", &[json!(origin)]) {
        let mut total = 0i64;
        for r in rows.as_array().cloned().unwrap_or_default() {
            let kind = r.get("kind").and_then(Value::as_str).unwrap_or("").to_string();
            let n = r.get("n").and_then(Value::as_i64).unwrap_or(0);
            surface.insert(kind, json!(n));
            total += n;
        }
        surface.insert("total".into(), json!(total));
    }

    // smells by kind
    let smells_arr = smells.get("smells").and_then(Value::as_array).cloned().unwrap_or_default();
    let mut by_kind: Map<String, Value> = Map::new();
    for s in &smells_arr {
        let kind = s.get("kind").and_then(Value::as_str).unwrap_or("").to_string();
        let c = by_kind.get(&kind).and_then(Value::as_i64).unwrap_or(0) + 1;
        by_kind.insert(kind, json!(c));
    }

    // narrative — the ONE templated AI field. Use a passed-in narrative else compute best-effort.
    // skip_narrative forces deterministic-only. Never fails the snapshot.
    let skip_narrative = truthy(input.get("skip_narrative"));
    let mut narrative: Value = input.get("narrative_md").cloned().unwrap_or(Value::Null);
    if narrative.is_null() && !skip_narrative {
        narrative = match crate::codebase_architecture_digest::architectureDigest(&json!({ "origin": origin }), db) {
            Ok(d) if d.get("ok").and_then(Value::as_bool).unwrap_or(false) => d.get("digest_md").cloned().unwrap_or(Value::Null),
            _ => Value::Null,
        };
    }

    let covered_total = cov.get("covered_count").and_then(Value::as_i64).unwrap_or(0) + cov.get("uncovered_count").and_then(Value::as_i64).unwrap_or(0);

    let baseTop = |arr: &Value, n: usize| -> Vec<Value> {
        arr.as_array().cloned().unwrap_or_default().into_iter().take(n).collect()
    };
    let top_central: Vec<Value> = baseTop(m.get("top_central").unwrap_or(&Value::Null), TOP).iter()
        .map(|e| json!({ "path": e.get("path").cloned().unwrap_or(Value::Null), "fan_in": e.get("fan_in").cloned().unwrap_or(Value::Null) })).collect();
    let top_risky: Vec<Value> = baseTop(m.get("top_risky").unwrap_or(&Value::Null), TOP).iter()
        .map(|e| json!({ "path": e.get("path").cloned().unwrap_or(Value::Null), "fan_in": e.get("fan_in").cloned().unwrap_or(Value::Null) })).collect();
    let top_complex: Vec<Value> = baseTop(m.get("top_complex").unwrap_or(&Value::Null), TOP).iter()
        .map(|e| json!({ "path": e.get("path").cloned().unwrap_or(Value::Null), "complexity": e.get("complexity").cloned().unwrap_or(Value::Null) })).collect();
    let top_churn: Vec<Value> = baseTop(m.get("top_churn").unwrap_or(&Value::Null), TOP).iter()
        .map(|e| json!({ "path": e.get("path").cloned().unwrap_or(Value::Null), "churn": e.get("churn").cloned().unwrap_or(Value::Null) })).collect();

    let mut largest_sorted: Vec<(String, i64)> = loc_by_path.into_iter().filter(|(p, _)| !is_test_set.contains(p)).collect();
    largest_sorted.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    largest_sorted.truncate(TOP);
    let largest_files: Vec<Value> = largest_sorted.into_iter().map(|(path, loc)| json!({ "path": path, "loc": loc })).collect();

    let project_health = score.get("project").and_then(|p| p.get("health")).cloned().unwrap_or(json!(0));
    let project_risk = score.get("project").and_then(|p| p.get("risk")).cloned().unwrap_or(json!(0));
    let files_len = g.file_ids.len() as f64;
    let avg_fan = if files_len > 0.0 { round_d(g.edges_count as f64 / files_len, 2) } else { round_d(0.0, 2) };

    // ── the row: one value per spec column ──
    let mut row: Map<String, Value> = Map::new();
    row.insert("total_files_num".into(), json!(file_count));
    row.insert("total_symbols_num".into(), json!(total_symbols));
    row.insert("total_edges_num".into(), json!(total_edges));
    row.insert("total_loc_num".into(), json!(total_loc));
    row.insert("languages_json".into(), Value::Object(langs));
    row.insert("avg_fan_in_num".into(), json!(avg_fan));
    row.insert("avg_fan_out_num".into(), json!(avg_fan));
    row.insert("max_depth_num".into(), json!(max_depth(&g)));
    row.insert("cycles_num".into(), json!(cycle_count(&g)));
    row.insert("orphans_json".into(), json!(orphans.into_iter().take(50).collect::<Vec<_>>()));
    row.insert("hubs_json".into(), json!(hubs.into_iter().map(|(path, fan_in)| json!({ "path": path, "fan_in": fan_in })).collect::<Vec<_>>()));
    row.insert("health_score_num".into(), json!(round_d(js_number(Some(&project_health)), 1)));
    row.insert("risk_score_num".into(), json!(round_d(js_number(Some(&project_risk)), 1)));
    row.insert("top_central_json".into(), json!(top_central));
    row.insert("top_risky_json".into(), json!(top_risky));
    row.insert("top_complex_json".into(), json!(top_complex));
    row.insert("coverage_pct".into(), json!(if covered_total > 0 { round_d((cov.get("covered_count").and_then(Value::as_i64).unwrap_or(0) as f64 / covered_total as f64) * 100.0, 1) } else { round_d(0.0, 1) }));
    row.insert("tested_files_num".into(), cov.get("covered_count").cloned().unwrap_or(json!(0)));
    row.insert("untested_files_num".into(), cov.get("uncovered_count").cloned().unwrap_or(json!(0)));
    row.insert("test_files_num".into(), cov.get("test_count").cloned().unwrap_or(json!(0)));
    row.insert("entry_points_json".into(), Value::Object(surface));
    row.insert("smells_total_num".into(), json!(smells_arr.len() as i64));
    row.insert("smells_by_kind_json".into(), Value::Object(by_kind));
    row.insert("largest_files_json".into(), json!(largest_files));
    row.insert("top_churn_json".into(), json!(top_churn));
    row.insert("narrative_md".into(), narrative);

    // INSERT generated from the spec column list (best-effort — a read-only / missing code_digests
    // still returns the live sections below, matching the JS's own swallowed catch).
    let mut digest_id: Value = Value::Null;
    let mut created_at: Value = Value::Null;
    let cols_csv = DIGEST_COLS.join(", ");
    let placeholders = DIGEST_COLS.iter().map(|_| "?").collect::<Vec<_>>().join(", ");
    let sql = format!("INSERT INTO code_digests (origin, root, {cols_csv}) VALUES (?, ?, {placeholders})");
    let mut params: Vec<Value> = vec![json!(origin), root.clone()];
    for col in DIGEST_COLS {
        let v = row.get(col).cloned().unwrap_or(Value::Null);
        if col.ends_with("_json") {
            params.push(json!(serde_json::to_string(&v).unwrap_or_else(|_| "null".to_string())));
        } else {
            params.push(v);
        }
    }
    if let Ok((_, rowid)) = crate::db_json::run_write(db, &sql, &params) {
        digest_id = json!(rowid);
        if let Ok(rows) = crate::db_json::query_rows(db, "SELECT created_at FROM code_digests WHERE id = ?", &[json!(rowid)]) {
            created_at = rows.as_array().and_then(|a| a.first()).and_then(|r| r.get("created_at")).cloned().unwrap_or(Value::Null);
        }
    }

    let sections = crate::codebase_digest_spec::sectionsFromRow(&Value::Object(row));
    Ok(json!({ "ok": true, "digest_id": digest_id, "origin": origin, "created_at": created_at, "sections": sections }))
}

fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}
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

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS brain;").unwrap();
        c.execute_batch(
            "CREATE TABLE workspaces (id INTEGER PRIMARY KEY, root_path TEXT, is_active INTEGER);
             CREATE TABLE code_nodes (id TEXT PRIMARY KEY, origin TEXT, type TEXT, path TEXT, parent_id TEXT, span_json TEXT);
             CREATE TABLE code_edges (id INTEGER PRIMARY KEY AUTOINCREMENT, origin TEXT, type TEXT, from_node TEXT, to_node TEXT, kind TEXT);
             CREATE TABLE code_signals (id INTEGER PRIMARY KEY, origin TEXT, path TEXT, category TEXT);
             CREATE TABLE code_digests (
               id INTEGER PRIMARY KEY, origin TEXT, root TEXT, created_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')),
               total_files_num REAL, total_symbols_num REAL, total_edges_num REAL, total_loc_num REAL, languages_json TEXT,
               avg_fan_in_num REAL, avg_fan_out_num REAL, max_depth_num REAL, cycles_num REAL, orphans_json TEXT, hubs_json TEXT,
               health_score_num REAL, risk_score_num REAL, top_central_json TEXT, top_risky_json TEXT, top_complex_json TEXT,
               coverage_pct REAL, tested_files_num REAL, untested_files_num REAL, test_files_num REAL,
               entry_points_json TEXT, smells_total_num REAL, smells_by_kind_json TEXT,
               largest_files_json TEXT, top_churn_json TEXT, narrative_md TEXT
             );
             CREATE TABLE brain.prompt_templates ( id INTEGER PRIMARY KEY, name TEXT NOT NULL, version INTEGER NOT NULL, template_md TEXT NOT NULL, default_tier TEXT NOT NULL, default_temperature TEXT NOT NULL, max_output_tokens INTEGER NOT NULL DEFAULT 1024, superseded_by INTEGER, created_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')), cross_field_rules_json TEXT, judging_criteria_json TEXT, system_md TEXT, phase_flow TEXT NOT NULL DEFAULT 'single', UNIQUE (name, version) );
             CREATE TABLE brain.prompt_output_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, grounded_refs_json TEXT, UNIQUE (prompt_template_id, field_name) );
             CREATE TABLE brain.prompt_input_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, UNIQUE (prompt_template_id, field_name) );",
        ).unwrap();
        c
    }

    fn seed_files(c: &Connection) {
        c.execute_batch(
            "INSERT INTO code_nodes (id, origin, type, path) VALUES
               ('f1','atlas','file','src/index.mjs'),
               ('f2','atlas','file','src/engine.mjs'),
               ('f3','atlas','file','src/util.test.mjs');
             INSERT INTO code_edges (origin, type, from_node, to_node) VALUES
               ('atlas','imports','f1','f2'),
               ('atlas','imports','f3','f2');",
        ).unwrap();
    }

    #[test]
    fn no_atlas_returns_ok_false() {
        let c = setup();
        let r = codeDigestSnapshot(&json!({ "origin": "atlas" }), &c).unwrap();
        assert_eq!(r["ok"], json!(false));
        assert!(r["error"].as_str().unwrap().contains("no atlas"));
    }

    #[test]
    fn snapshot_persists_and_reflects_sections() {
        let c = setup();
        seed_files(&c);
        let r = codeDigestSnapshot(&json!({ "origin": "atlas", "skip_narrative": true }), &c).unwrap();
        assert_eq!(r["ok"], json!(true));
        assert!(r["digest_id"].as_i64().unwrap() > 0);
        assert!(r["created_at"].is_string());
        let sections = r["sections"].as_array().unwrap();
        let files_sec = sections.iter().find(|s| s["col"] == json!("total_files_num")).unwrap();
        // JS has no int/float distinction; row.total_files_num is built in-process (not
        // round-tripped through the REAL column) so serde_json stores it as an integer Number —
        // compare numerically rather than assuming a float literal.
        assert_eq!(files_sec["value"].as_f64().unwrap(), 3.0);
        let narrative_sec = sections.iter().find(|s| s["col"] == json!("narrative_md")).unwrap();
        assert_eq!(narrative_sec["value"], Value::Null);
        // persisted row is readable back
        let cnt = crate::db_json::query_rows(&c, "SELECT count(*) n FROM code_digests", &[]).unwrap();
        assert_eq!(cnt[0]["n"], json!(1));
    }

    #[test]
    fn passed_in_narrative_skips_the_ai_call() {
        let c = setup();
        seed_files(&c);
        let r = codeDigestSnapshot(&json!({ "origin": "atlas", "narrative_md": "hand-written" }), &c).unwrap();
        let sections = r["sections"].as_array().unwrap();
        let narrative_sec = sections.iter().find(|s| s["col"] == json!("narrative_md")).unwrap();
        assert_eq!(narrative_sec["value"], json!("hand-written"));
    }
}
