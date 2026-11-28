//! atlas-smells.rs — twin of skills/codebase/atlas-smells.mjs (db READ lane). NAMES ARE CONTRACT:
//! `atlasSmells` (default export). Structural defects, each a finding backed by evidence nodes —
//! pure queries over the atlas import graph, no AI. Detectors: cycle (Tarjan SCC size >= 2, high),
//! orphan (no imports in or out, low), unreferenced-file (non-entry file nobody imports, medium),
//! test-gap (top-quartile fan-in + uncovered, high), god-file (top-decile fan-in AND complexity,
//! medium).
//!
//! DEPENDENCY SCOPE (playbook lesson 6 / atlas-score.rs precedent: inline what the caller actually
//! reads rather than convert the whole dependency): no atlas-metrics.rs twin exists yet.
//! atlas-smells.mjs calls `atlasMetrics({origin, top:100000}, {db})` but only ever reads FOUR fields
//! off `metrics_by_file` — `is_test`, `fan_in`, `complexity`, `covered` — so `gather_metrics` below
//! recomputes exactly those four, with the SAME SQL atlas-metrics.mjs uses for each, instead of
//! reimplementing the whole metrics surface (fan_out/loc/symbol_count/churn/cascade/gravity/lens are
//! never touched by atlas-smells and are omitted). `is_test`/`covered` reuse the already-converted
//! `codebase_atlas_test_map` twin verbatim (its own `isTestPath`/`coveredPaths`).
//!
//! CHURN/GIT — not reproduced, and it doesn't need to be (verified by reading atlas-smells.mjs, not
//! assumed): atlas-smells.mjs's own return value never reads `x.churn`, so atlas-metrics.mjs's
//! internal `execFileSync('git', ...)` churn branch (see atlas-score.rs's module doc for that
//! branch's exact conditions) has zero effect on atlas-smells' output regardless of whether it fires
//! in the real JS. This twin never invokes git at all — the field is dead code from atlas-smells'
//! point of view, so there is nothing to disclose as a skipped branch here.
//!
//! THROW FIDELITY: the JS `if (!db) throw err.contextMissing('atlas-smells', 'db')` guard (verified
//! message: "atlas-smells: context.db is required") is unreachable in the db lane — the harness
//! always injects a live `&Connection`, same pattern as atlas-score.rs / atlas-get.rs — so this twin
//! has no corresponding `Err` path for it.
use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;
use regex::Regex;

fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) } // JS `??` right operand

// = playbook's js_number_to_string: mirrors JS Number-to-string for a raw f64 embedded directly
// into an output STRING (the god-file detail text), where the harness's numeric epsilon can't apply.
fn js_number_to_string(f: f64) -> String {
    if f.is_nan() { return "NaN".to_string(); }
    if f == 0.0 { return "0".to_string(); }
    let neg = f.is_sign_negative();
    let abs = f.abs();
    if abs.is_infinite() { return if neg { "-Infinity".into() } else { "Infinity".into() }; }
    let sci = format!("{:e}", abs);
    let (mantissa, exp_str) = sci.split_once('e').unwrap();
    let exp: i32 = exp_str.parse().unwrap();
    let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    let k = digits.len() as i32;
    let body = if (-6..=20).contains(&exp) {
        if exp >= 0 {
            if exp + 1 >= k { format!("{digits}{}", "0".repeat((exp + 1 - k) as usize)) }
            else { let (i, f) = digits.split_at((exp + 1) as usize); format!("{i}.{f}") }
        } else { format!("0.{}{digits}", "0".repeat((-exp - 1) as usize)) }
    } else {
        let m = if k == 1 { digits.clone() } else { format!("{}.{}", &digits[..1], &digits[1..]) };
        format!("{m}e{}{}", if exp >= 0 { "+" } else { "-" }, exp.abs())
    };
    if neg { format!("-{body}") } else { body }
}

// = atlas-smells.mjs's ENTRY_RE: entry-ish basenames (a file nobody imports may legitimately be an
// entry point, not dead).
fn entry_re() -> &'static Regex {
    static C: OnceLock<Regex> = OnceLock::new();
    C.get_or_init(|| Regex::new(r"(?i)(^|/)(index|main|cli|bin|app|server|mod|__init__|program|setup|conftest)\b").unwrap())
}

// = atlas-smells.mjs's q(arr, p): arr must already be sorted ascending.
fn quantile(arr: &[f64], p: f64) -> f64 {
    if arr.is_empty() { 0.0 } else {
        let idx = (p * (arr.len() as f64 - 1.0)).floor() as usize;
        arr[idx.min(arr.len() - 1)]
    }
}

// Tarjan SCC over the file-level imports graph. Translates atlas-smells.mjs's `tarjanSCC` 1:1: the
// outer loop and each node's successor list are plain arrays/Vecs (order-preserving by construction,
// never enumerated as a HashMap), so index/low/onStack can be ordinary HashMap/HashSet — they are
// only ever point-queried (.get/.has/.set), never iterated as a whole collection, so Rust's HashMap
// iteration-order difference from JS's Map never surfaces in the output.
struct Tarjan<'a> {
    adj: &'a HashMap<String, Vec<String>>,
    index: HashMap<String, i64>,
    low: HashMap<String, i64>,
    on_stack: HashSet<String>,
    stack: Vec<String>,
    sccs: Vec<Vec<String>>,
    idx: i64,
}

impl<'a> Tarjan<'a> {
    fn new(adj: &'a HashMap<String, Vec<String>>) -> Self {
        Tarjan { adj, index: HashMap::new(), low: HashMap::new(), on_stack: HashSet::new(), stack: Vec::new(), sccs: Vec::new(), idx: 0 }
    }

    fn strongconnect(&mut self, v: &str) {
        self.index.insert(v.to_string(), self.idx);
        self.low.insert(v.to_string(), self.idx);
        self.idx += 1;
        self.stack.push(v.to_string());
        self.on_stack.insert(v.to_string());
        if let Some(succs) = self.adj.get(v) {
            let succs = succs.clone(); // decouple the &self.adj borrow from the &mut self recursion below
            for w in &succs {
                if !self.index.contains_key(w) {
                    self.strongconnect(w);
                    let lv = *self.low.get(v).unwrap();
                    let lw = *self.low.get(w).unwrap();
                    self.low.insert(v.to_string(), lv.min(lw));
                } else if self.on_stack.contains(w) {
                    let lv = *self.low.get(v).unwrap();
                    let iw = *self.index.get(w).unwrap();
                    self.low.insert(v.to_string(), lv.min(iw));
                }
            }
        }
        if self.low.get(v) == self.index.get(v) {
            let mut comp = Vec::new();
            loop {
                let w = self.stack.pop().unwrap();
                self.on_stack.remove(&w);
                let done = w == v;
                comp.push(w);
                if done { break; }
            }
            self.sccs.push(comp);
        }
    }

    fn run(mut self, nodes: &[String]) -> Vec<Vec<String>> {
        for v in nodes {
            if !self.index.contains_key(v) { self.strongconnect(v); }
        }
        self.sccs
    }
}

struct FileMetric { path: String, fan_in: i64, complexity: f64, is_test: bool, covered: bool }

// Recomputes the minimal subset of atlas-metrics.mjs's metrics_by_file that atlas-smells.mjs reads
// (fan_in, complexity, is_test, covered) — see module doc for the dependency-scope rationale. Order
// matches the JS `Object.entries(metrics_by_file)` order: the SQL `files` scan order (no ORDER BY),
// same precedent as atlas-score.rs's gather_metrics.
fn gather_metrics(db: &Connection, origin: &str) -> Result<Vec<FileMetric>, String> {
    let files = crate::db_json::query_rows(db, "SELECT id, path, span_json FROM code_nodes WHERE origin = ? AND type = 'file'", &[json!(origin)])?;
    let files = files.as_array().cloned().unwrap_or_default();

    let covered_arr = crate::codebase_atlas_test_map::coveredPaths(db, origin);
    let covered: HashSet<String> = covered_arr.as_array()
        .map(|a| a.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect())
        .unwrap_or_default();

    let mut out: Vec<FileMetric> = Vec::new();
    for f in &files {
        let id = f.get("id").cloned().unwrap_or(Value::Null);
        let path = f.get("path").and_then(|v| v.as_str()).unwrap_or("").to_string();

        let fan_in_rows = crate::db_json::query_rows(db, &format!("SELECT count(DISTINCT from_node) n FROM code_edges WHERE origin = ? AND to_node = ? AND {}", crate::codebase_atlas_build::RUNTIME_IMPORTS), &[json!(origin), id.clone()])?;
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

        out.push(FileMetric { path, fan_in, complexity, is_test, covered: is_covered });
    }
    Ok(out)
}

pub fn atlasSmells(input: &Value, db: &Connection) -> Result<Value, String> {
    let origin = nz(input.get("origin")).and_then(|v| v.as_str()).unwrap_or("atlas").to_string();

    let files = gather_metrics(db, &origin)?;
    let sources: Vec<&FileMetric> = files.iter().filter(|x| !x.is_test).collect();

    // file import adjacency (path -> [imported paths]) + the set of imported files — atlas-smells.mjs's
    // OWN two queries (verbatim SQL text), independent of gather_metrics' `files` query above.
    let id_rows = crate::db_json::query_rows(db, "SELECT id, path FROM code_nodes WHERE origin = ? AND type='file'", &[json!(&origin)])?;
    let id_rows = id_rows.as_array().cloned().unwrap_or_default();
    let mut id_to_path: HashMap<String, String> = HashMap::new();
    let mut all_file_paths: Vec<String> = Vec::new();
    for r in &id_rows {
        let id = r.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let path = r.get("path").and_then(|v| v.as_str()).unwrap_or("").to_string();
        id_to_path.insert(id, path.clone());
        all_file_paths.push(path);
    }

    let edge_rows = crate::db_json::query_rows(db, &format!("SELECT from_node, to_node FROM code_edges WHERE origin = ? AND {}", crate::codebase_atlas_build::RUNTIME_IMPORTS), &[json!(&origin)])?;
    let edge_rows = edge_rows.as_array().cloned().unwrap_or_default();
    let mut adj: HashMap<String, Vec<String>> = HashMap::new();
    let mut has_importer: HashSet<String> = HashSet::new();
    for e in &edge_rows {
        let from_id = e.get("from_node").and_then(|v| v.as_str()).unwrap_or("");
        let to_id = e.get("to_node").and_then(|v| v.as_str()).unwrap_or("");
        if let (Some(f), Some(t)) = (id_to_path.get(from_id), id_to_path.get(to_id)) {
            if f == t { continue; }
            adj.entry(f.clone()).or_default().push(t.clone());
            has_importer.insert(t.clone());
        }
    }

    let mut smells: Vec<Value> = Vec::new();

    // 1. cycles
    let sccs = Tarjan::new(&adj).run(&all_file_paths);
    for comp in sccs {
        if comp.len() >= 2 {
            let mut sorted = comp.clone();
            sorted.sort();
            smells.push(json!({
                "kind": "cycle", "severity": "high",
                "nodes": sorted,
                "detail": format!("circular dependency among {} files", comp.len()),
            }));
        }
    }

    // 2/3. orphan + unreferenced-file
    for x in &sources {
        let out = adj.get(&x.path).map(|v| v.len()).unwrap_or(0);
        let imported = has_importer.contains(&x.path);
        if out == 0 && !imported {
            smells.push(json!({ "kind": "orphan", "severity": "low", "nodes": [x.path.clone()], "detail": "no imports in or out — an isolated file" }));
        } else if !imported && out > 0 && !entry_re().is_match(&x.path) {
            smells.push(json!({ "kind": "unreferenced-file", "severity": "medium", "nodes": [x.path.clone()], "detail": "nothing imports this file (its exports look unused)" }));
        }
    }

    // distributions for the threshold-based detectors.
    let mut fanins: Vec<f64> = sources.iter().map(|x| x.fan_in as f64).collect();
    fanins.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let mut cxs: Vec<f64> = sources.iter().map(|x| x.complexity).collect();
    cxs.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let q3fanin = quantile(&fanins, 0.75);
    let d9fanin = quantile(&fanins, 0.9);
    let d9cx = quantile(&cxs, 0.9);

    for x in &sources {
        // 4. test-gap: central + uncovered
        if (x.fan_in as f64) >= q3fanin.max(1.0) && !x.covered {
            smells.push(json!({ "kind": "test-gap", "severity": "high", "nodes": [x.path.clone()], "detail": format!("central (fan_in {}) but no test covers it", x.fan_in) }));
        }
        // 5. god-file: top-decile fan_in AND complexity
        if d9fanin > 0.0 && d9cx > 0.0 && (x.fan_in as f64) >= d9fanin && x.complexity >= d9cx {
            smells.push(json!({ "kind": "god-file", "severity": "medium", "nodes": [x.path.clone()], "detail": format!("high fan_in ({}) + high complexity ({}) — a change magnet", x.fan_in, js_number_to_string(x.complexity)) }));
        }
    }

    let mut counts: Map<String, Value> = Map::new();
    for s in &smells {
        let kind = s.get("kind").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let entry = counts.entry(kind).or_insert(json!(0));
        let n = entry.as_i64().unwrap_or(0) + 1;
        *entry = json!(n);
    }
    counts.insert("total".to_string(), json!(smells.len() as i64));

    let file_count = sources.len() as i64;
    Ok(json!({ "origin": origin, "smells": smells, "counts": counts, "file_count": file_count }))
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
    fn detects_cycle_orphan_unreferenced_file_and_test_gap() {
        let c = db();
        c.execute_batch("
            INSERT INTO code_nodes (id,origin,path,type,parent_id,span_json) VALUES
              ('a.ts@atlas','atlas','a.ts','file',NULL,NULL),
              ('b.ts@atlas','atlas','b.ts','file',NULL,NULL),
              ('core.ts@atlas','atlas','core.ts','file',NULL,NULL),
              ('user.ts@atlas','atlas','user.ts','file',NULL,NULL),
              ('island.ts@atlas','atlas','island.ts','file',NULL,NULL),
              ('unused.ts@atlas','atlas','unused.ts','file',NULL,NULL),
              ('dep.ts@atlas','atlas','dep.ts','file',NULL,NULL),
              ('core.test.ts@atlas','atlas','core.test.ts','file',NULL,NULL);
            INSERT INTO code_edges (origin,from_node,to_node,type) VALUES
              ('atlas','a.ts@atlas','b.ts@atlas','imports'),
              ('atlas','b.ts@atlas','a.ts@atlas','imports'),
              ('atlas','user.ts@atlas','core.ts@atlas','imports'),
              ('atlas','unused.ts@atlas','dep.ts@atlas','imports');
        ").unwrap();
        let r = atlasSmells(&json!({}), &c).unwrap();
        let smells = r["smells"].as_array().unwrap();
        let of = |k: &str| -> Vec<&Value> { smells.iter().filter(|s| s["kind"] == json!(k)).collect() };

        let cyc = of("cycle");
        assert_eq!(cyc.len(), 1);
        assert_eq!(cyc[0]["nodes"], json!(["a.ts", "b.ts"]));
        assert_eq!(cyc[0]["severity"], json!("high"));

        assert!(of("orphan").iter().any(|s| s["nodes"][0] == json!("island.ts")));
        assert!(of("unreferenced-file").iter().any(|s| s["nodes"][0] == json!("unused.ts")));
        assert!(of("test-gap").iter().any(|s| s["nodes"][0] == json!("core.ts")));
        assert_eq!(r["counts"]["total"], json!(smells.len() as i64));
    }

    #[test]
    fn clean_graph_with_a_covered_source_has_no_cycle_or_orphan() {
        let c = db();
        c.execute_batch("
            INSERT INTO code_nodes (id,origin,path,type,parent_id,span_json) VALUES
              ('x.ts@atlas','atlas','x.ts','file',NULL,NULL),
              ('x.test.ts@atlas','atlas','x.test.ts','file',NULL,NULL);
            INSERT INTO code_edges (origin,from_node,to_node,type) VALUES
              ('atlas','x.test.ts@atlas','x.ts@atlas','imports');
        ").unwrap();
        let r = atlasSmells(&json!({}), &c).unwrap();
        let smells = r["smells"].as_array().unwrap();
        assert_eq!(smells.iter().filter(|s| s["kind"] == json!("cycle")).count(), 0);
        assert_eq!(smells.iter().filter(|s| s["kind"] == json!("orphan")).count(), 0);
        assert_eq!(r["file_count"], json!(1));
    }
}
