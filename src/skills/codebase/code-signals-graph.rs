//! code-signals-graph.rs — twin of skills/codebase/code-signals-graph.mjs. NAMES ARE CONTRACT:
//! `gitHistory` (named, no-db, real git subprocess), `codeSignalsGraph` (default, standard db
//! lane). The DETERMINISTIC WHOLE-ATLAS signals: DEBT (dead-export, orphan-file), HISTORY
//! (bus-factor, co-change, from a shared git log) + the public-API census/drift snapshot
//! (code_api_snapshots). Best-effort throughout — a missing table / non-repo degrades to fewer
//! signals, never a failure.
//!
//! MIXED PER-UNIT CLASSIFICATION (2 census units in this one file — the playbook's per-file
//! bespoke-native rule applied per UNIT, since here only ONE of the two exported fns is
//! git-unconditional):
//!
//! - `gitHistory` is BESPOKE-NATIVE for ITS OWN unit: every success path of this function shells
//!   out to `git log` for real (`git_log_raw`, `std::process::Command`) — no input flag inside
//!   `gitHistory` itself skips it. Its return shape is also a JS `Map`/`Set` composite
//!   (`{byFile: Map, commits: [...]}`), which has no JSON-serializable form a conform-db fixture
//!   could compare anyway (the `transitive` frontier class, playbook rule) — doubly
//!   non-fixturable. So: real correct code below, proven by the native `#[cfg(test)]` tests
//!   (temp-dir precedent, code-git-info.rs), NO conform-db fixture and NO frontier entry for
//!   THIS unit (lib_rs mod line only — already covered by the sibling unit below).
//! - `codeSignalsGraph` IS conditionally fixture-testable: the JS gates its ONE call to
//!   `gitHistory` behind `input?.history !== undefined ? input.history : gitHistory(root)` — a
//!   KEY-PRESENCE check (not `??`), so an explicit `"history": null` in the fixture args takes
//!   the `input.history` branch WITHOUT ever invoking `gitHistory` (constitution-estimate.rs's
//!   `input.base`-gated `git_diff_paths` precedent, same flag-skippable shape). Ported below as a
//!   real, correct call to `gitHistory` on the `None` (key-absent) arm — disclosed
//!   real-but-untested-by-fixture, since every fixture case sets `"history": null` to stay
//!   deterministic (a JS `Map` has no fixture-representable form regardless). The HISTORY
//!   bus-factor/co-change block that consumes a non-null `history` is therefore ALSO
//!   disclosed-untested-by-fixture (though independently native-tested via `codeSignalsGraph`
//!   called directly in Rust with a hand-built `history` `Value`, see tests below) — faithfully
//!   ported including its lack of a JS try/catch (an error here propagates, modeled with `?`).
//!
//! `context.db` missing → JS throws `err.contextMissing('code-signals-graph', 'db')`. Not
//! modeled: `db: &Connection` is a required, non-nullable reference here — same "guard-throws are
//! not exercised" convention used by every sibling twin.
//!
//! Table access is UNQUALIFIED main-schema (code_nodes/code_edges/code_signals/
//! code_entry_points/code_api_snapshots) — same convention as the sibling `feature-inventory.rs`
//! / `code-signals-detect.rs` twins (no `code.` ATTACH prefix; the fixture creates these tables
//! directly on the fixture's main db).
// QUALITY: real git subprocess ported faithfully for `gitHistory` (bespoke-native, no
// fixture/frontier for that unit); `codeSignalsGraph`'s DEBT/WRITE/API-census paths are
// fixture-covered; its HISTORY path is real-but-disclosed-untested-by-fixture (flag-skippable via
// `"history": null`, same shape as constitution-estimate.rs's `git_diff_paths` branch).

use regex::Regex;
use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) } // JS `??` right operand
fn str_qq(v: Option<&Value>) -> String { match v { None|Some(Value::Null)=>String::new(), Some(Value::String(s))=>s.clone(), Some(Value::Bool(b))=>b.to_string(), Some(n@Value::Number(_))=>n.to_string(), Some(o)=>o.to_string() } } // = JS String(v ?? '')
fn tmpl(v: Option<&Value>) -> String { match v { None=>"undefined".into(), Some(Value::Null)=>"null".into(), Some(x)=>str_qq(Some(x)) } } // = JS `${v}`

// `(?:^|[\\/])(?:__tests__|__mocks__)[\\/]|\.(?:test|spec)\.[cm]?[jt]sx?$|(?:^|[\\/])(?:index|main|app|server)\.[cm]?[jt]sx?$` /i
fn orphan_skip_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"(?i)(?:^|[\\/])(?:__tests__|__mocks__)[\\/]|\.(?:test|spec)\.[cm]?[jt]sx?$|(?:^|[\\/])(?:index|main|app|server)\.[cm]?[jt]sx?$").unwrap())
}

// ── gitHistory (bespoke-native for this unit — see module doc) ──────────────────────────────

/// `execFileSync('git', ['-C', root, 'log', '-n', String(limit), '--no-merges',
/// '--format=@%H\t%an', '--name-only'], {...})` — real subprocess; `execFileSync` throws on a
/// nonzero exit or a spawn failure, caught by `gitHistory`'s own try/catch → `None` here.
fn git_log_raw(root: &str, limit: i64) -> Option<String> {
    let output = crate::proc::cmd("git")
        .arg("-C").arg(root)
        .arg("log")
        .arg("-n").arg(limit.to_string())
        .arg("--no-merges")
        .arg("--format=@%H\t%an")
        .arg("--name-only")
        .output();
    match output {
        Ok(o) if o.status.success() => Some(String::from_utf8_lossy(&o.stdout).into_owned()),
        _ => None,
    }
}

/// Pure parse of the `git log` text above into the `(byFile, commits)` shape `gitHistory` builds
/// — split out from the real subprocess call so it is independently testable without a `git`
/// binary (same split as code-file-history.rs's `sig_to_violation`). `by_file`/each commit's
/// `files` preserve first-appearance INSERTION order (`serde_json::Map`/`Vec`, `preserve_order`
/// feature) — the JS `Map`'s iteration order the caller's bus-factor loop depends on for its
/// output row ORDER.
fn parse_git_history(out: &str, churn_limit: i64) -> (Map<String, Value>, Vec<Value>) {
    let mut by_file: Map<String, Value> = Map::new();
    let mut commits: Vec<Value> = Vec::new();
    let mut cur_idx: Option<usize> = None;
    let mut idx: i64 = -1;
    for line in out.split('\n') {
        if let Some(rest) = line.strip_prefix('@') {
            // `const [sha, author] = line.slice(1).split('\t')` — split on EVERY tab, destructure
            // only takes index 0/1 (a 3rd+ tab-separated segment, if any, is silently dropped).
            let parts: Vec<&str> = rest.split('\t').collect();
            let sha = parts.first().copied().unwrap_or("").to_string();
            let author = parts.get(1).map(|s| s.to_string()).unwrap_or_else(|| "?".to_string()); // `author ?? '?'`
            commits.push(json!({ "sha": sha, "author": author, "files": Value::Array(vec![]) }));
            cur_idx = Some(commits.len() - 1);
            idx += 1;
        } else if !line.trim().is_empty() {
            if let Some(ci) = cur_idx {
                let p = line.trim().replace('\\', "/");
                let author = commits[ci].get("author").and_then(Value::as_str).unwrap_or("?").to_string();
                if let Some(files) = commits[ci].get_mut("files").and_then(Value::as_array_mut) {
                    files.push(json!(p.clone()));
                }
                let entry = by_file.entry(p.clone()).or_insert_with(|| json!({ "authors": Value::Array(vec![]), "commits": 0i64 }));
                if let Value::Object(m) = entry {
                    if let Some(Value::Array(authors)) = m.get_mut("authors") {
                        if !authors.iter().any(|a| a.as_str() == Some(author.as_str())) {
                            authors.push(json!(author)); // Set semantics: add is a no-op on a dup
                        }
                    }
                    if idx < churn_limit {
                        let c = m.get("commits").and_then(Value::as_i64).unwrap_or(0) + 1;
                        m.insert("commits".into(), json!(c));
                    }
                }
            }
        }
    }
    (by_file, commits)
}

/// `export function gitHistory(root, limit = 3000, churnLimit = 500)` — see module doc: real
/// subprocess, best-effort → `Value::Null` on any failure (non-repo, missing binary, etc), same
/// as the JS `catch { return null }`.
pub fn gitHistory(root: &str, limit: i64, churn_limit: i64) -> Value {
    match git_log_raw(root, limit) {
        None => Value::Null,
        Some(raw) => {
            let (by_file, commits) = parse_git_history(&raw, churn_limit);
            json!({ "by_file": Value::Object(by_file), "commits": commits })
        }
    }
}

// ── codeSignalsGraph ─────────────────────────────────────────────────────────────────────────

fn write_graph_signals(db: &Connection, origin: &Value, rows: &[Value]) -> Result<(), String> {
    db.execute_batch("SAVEPOINT sig_graph").map_err(|e| e.to_string())?;
    crate::db_json::run_write(db, "DELETE FROM code_signals WHERE origin = ? AND category IN ('debt','history')", &[origin.clone()])?;
    for r in rows {
        crate::db_json::run_write(
            db,
            "INSERT INTO code_signals (origin, category, kind, severity, path, line, detail) VALUES (?,?,?,?,?,?,?)",
            &[
                origin.clone(),
                r.get("category").cloned().unwrap_or(Value::Null),
                r.get("kind").cloned().unwrap_or(Value::Null),
                r.get("severity").cloned().unwrap_or(Value::Null),
                r.get("path").cloned().unwrap_or(Value::Null),
                nz(r.get("line")).cloned().unwrap_or(Value::Null),
                nz(r.get("detail")).cloned().unwrap_or(Value::Null),
            ],
        )?;
    }
    db.execute_batch("RELEASE sig_graph").map_err(|e| e.to_string())?;
    Ok(())
}

/// `key = (d) => \`${d.path}#${d.name}(${d.signature ?? ''})\``
fn api_key(d: &Value) -> String {
    format!("{}#{}({})", tmpl(d.get("path")), tmpl(d.get("name")), str_qq(d.get("signature")))
}

/// The public-API census + drift snapshot try-block, factored out so its own failure (table
/// absent) leaves the caller's `api` at its `{total:0,added:0,removed:0}` default — same as the
/// JS `try { ... api = counts } catch {}`.
fn compute_and_write_api(db: &Connection, origin: &Value) -> Result<Value, String> {
    let defs_arr: Vec<Value> = crate::db_json::query_rows(
        db,
        "SELECT path, name, signature, type FROM code_nodes WHERE origin = ? AND exported = 1 AND type <> 'file' ORDER BY path, name",
        &[origin.clone()],
    )?
    .as_array()
    .cloned()
    .unwrap_or_default();

    let mut prior_order: Vec<String> = Vec::new();
    let mut prior_set: HashSet<String> = HashSet::new();
    if let Ok(prior_rows) = crate::db_json::query_rows(db, "SELECT api_json FROM code_api_snapshots WHERE origin = ? ORDER BY id DESC LIMIT 1", &[origin.clone()]) {
        if let Some(row) = prior_rows.as_array().and_then(|a| a.first()) {
            let api_json_str = row.get("api_json").and_then(Value::as_str).unwrap_or("[]"); // `row?.api_json ?? '[]'`
            if let Ok(parsed) = serde_json::from_str::<Value>(api_json_str) {
                if let Some(arr) = parsed.as_array() {
                    for d in arr {
                        let k = api_key(d);
                        if prior_set.insert(k.clone()) { prior_order.push(k); } // Set semantics
                    }
                }
            }
        }
    }

    let cur: HashSet<String> = defs_arr.iter().map(api_key).collect();
    let added: Vec<Value> = defs_arr
        .iter()
        .filter(|d| !prior_set.contains(&api_key(d)))
        .map(|d| json!({ "path": d.get("path").cloned().unwrap_or(Value::Null), "name": d.get("name").cloned().unwrap_or(Value::Null) }))
        .collect();
    let removed: Vec<Value> = prior_order.iter().filter(|k| !cur.contains(*k)).map(|k| json!({ "sig": k })).collect();

    let api_entries: Vec<Value> = defs_arr
        .iter()
        .map(|d| {
            json!({
                "path": d.get("path").cloned().unwrap_or(Value::Null),
                "name": d.get("name").cloned().unwrap_or(Value::Null),
                "signature": nz(d.get("signature")).cloned().unwrap_or(json!("")),
                "type": d.get("type").cloned().unwrap_or(Value::Null),
            })
        })
        .collect();
    let api_json_str = serde_json::to_string(&Value::Array(api_entries)).map_err(|e| e.to_string())?;
    let counts = json!({ "total": defs_arr.len() as i64, "added": added.len() as i64, "removed": removed.len() as i64 });
    let counts_json_str = serde_json::to_string(&counts).map_err(|e| e.to_string())?;
    let diff_json_str = serde_json::to_string(&json!({ "added": added, "removed": removed })).map_err(|e| e.to_string())?;

    crate::db_json::run_write(
        db,
        "INSERT INTO code_api_snapshots (origin, counts_json, api_json, diff_json) VALUES (?,?,?,?)",
        &[origin.clone(), json!(counts_json_str), json!(api_json_str), json!(diff_json_str)],
    )?;

    Ok(counts)
}

/// `export default function codeSignalsGraph(input, context)`
pub fn codeSignalsGraph(input: &Value, db: &Connection) -> Result<Value, String> {
    let origin: Value = nz(input.get("origin")).cloned().unwrap_or(json!("atlas"));
    let root_v: Value = match nz(input.get("root")) {
        Some(v) => v.clone(),
        None => crate::engine_workspace_root::getWorkspaceRoot(db),
    };
    let root: String = root_v.as_str().unwrap_or_default().to_string();
    // `input?.history !== undefined ? input.history : gitHistory(root)` — a KEY-PRESENCE check
    // (not `??`): see module doc. `input.get("history")` returning `Some(Value::Null)` (an
    // explicit `"history": null` in a fixture) takes this branch WITHOUT calling `gitHistory`.
    let history: Value = match input.get("history") {
        Some(v) => v.clone(),
        None => gitHistory(&root, 3000, 500),
    };

    let mut rows: Vec<Value> = Vec::new();
    let mut dead_exports: i64 = 0;
    let mut orphan_files: i64 = 0;

    // ── DEBT: dead exports + orphan files, one JS try/catch — a mid-block query failure leaves
    // whatever rows/counters were already pushed in place (JS mutations aren't rolled back by a
    // later throw), modeled by mutating `rows`/counters directly inside this closure and
    // swallowing only its `Result`.
    let debt_result: Result<(), String> = (|| -> Result<(), String> {
        let referenced: HashSet<String> = crate::db_json::query_rows(db, "SELECT DISTINCT to_node FROM code_edges WHERE origin = ?", &[origin.clone()])?
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .filter_map(|r| r.get("to_node").and_then(Value::as_str).map(String::from))
            .collect();
        // entryPaths has its OWN inner try/catch in the JS (an IIFE) — degrades to empty on its
        // own, never propagates to the outer catch.
        let entry_paths: HashSet<String> = crate::db_json::query_rows(db, "SELECT DISTINCT path FROM code_entry_points WHERE origin = ?", &[origin.clone()])
            .ok()
            .and_then(|v| v.as_array().cloned())
            .unwrap_or_default()
            .iter()
            .filter_map(|r| r.get("path").and_then(Value::as_str).map(String::from))
            .collect();
        let exported_defs = crate::db_json::query_rows(db, "SELECT id, path, name FROM code_nodes WHERE origin = ? AND exported = 1 AND type <> 'file'", &[origin.clone()])?
            .as_array()
            .cloned()
            .unwrap_or_default();
        for d in &exported_defs {
            let id = d.get("id").and_then(Value::as_str).unwrap_or("");
            let path = d.get("path").and_then(Value::as_str).unwrap_or("");
            if referenced.contains(id) || entry_paths.contains(path) { continue; }
            rows.push(json!({
                "category": "debt", "kind": "dead-export", "severity": "low",
                "path": d.get("path").cloned().unwrap_or(Value::Null), "line": Value::Null,
                "detail": format!("'{}' is exported but never imported or called", tmpl(d.get("name"))),
            }));
            dead_exports += 1;
        }

        let imported_files: HashSet<String> = crate::db_json::query_rows(db, &format!("SELECT DISTINCT to_node FROM code_edges WHERE origin = ? AND {}", crate::codebase_atlas_build::RUNTIME_IMPORTS), &[origin.clone()])?
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .filter_map(|r| r.get("to_node").and_then(Value::as_str).map(String::from))
            .collect();
        let files = crate::db_json::query_rows(db, "SELECT id, path FROM code_nodes WHERE origin = ? AND type = 'file'", &[origin.clone()])?
            .as_array()
            .cloned()
            .unwrap_or_default();
        for f in &files {
            let id = f.get("id").and_then(Value::as_str).unwrap_or("");
            let path = f.get("path").and_then(Value::as_str).unwrap_or("");
            if imported_files.contains(id) || entry_paths.contains(path) { continue; }
            if orphan_skip_re().is_match(path) { continue; }
            rows.push(json!({
                "category": "debt", "kind": "orphan-file", "severity": "low",
                "path": f.get("path").cloned().unwrap_or(Value::Null), "line": Value::Null,
                "detail": "not imported by any file and not an entry point — possibly dead",
            }));
            orphan_files += 1;
        }
        Ok(())
    })();
    let _ = debt_result; // JS `catch { /* no edges/nodes → skip debt-graph */ }`

    // ── HISTORY: bus-factor + co-change — NOT wrapped in try/catch in the JS (a failure here
    // propagates), gated only by `history?.byFile` truthiness. Real-but-disclosed-untested-by-
    // fixture (see module doc); the `?` below matches JS's uncaught-throw propagation.
    let mut bus_factor: i64 = 0;
    let mut co_change: i64 = 0;
    if let Some(by_file) = history.get("by_file").and_then(Value::as_object) {
        let in_atlas: HashSet<String> = crate::db_json::query_rows(db, "SELECT path FROM code_nodes WHERE origin = ? AND type = 'file'", &[origin.clone()])?
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .filter_map(|r| r.get("path").and_then(Value::as_str).map(String::from))
            .collect();
        for (path, e) in by_file.iter() {
            if !in_atlas.contains(path) { continue; }
            let commits_n = e.get("commits").and_then(Value::as_i64).unwrap_or(0);
            let authors = e.get("authors").and_then(Value::as_array).cloned().unwrap_or_default();
            if commits_n >= 5 && authors.len() == 1 {
                let author = authors[0].as_str().unwrap_or("");
                rows.push(json!({
                    "category": "history", "kind": "bus-factor", "severity": "medium",
                    "path": path, "line": Value::Null,
                    "detail": format!("hot file ({commits_n} recent commits) known by ONE author: {author}"),
                }));
                bus_factor += 1;
            }
        }

        let commits_arr = history.get("commits").and_then(Value::as_array).cloned().unwrap_or_default();
        let mut pair_order: Vec<String> = Vec::new();
        let mut pair_count: HashMap<String, i64> = HashMap::new();
        for c in &commits_arr {
            let files_v: Vec<String> = c
                .get("files")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
                .iter()
                .filter_map(|p| p.as_str().map(String::from))
                .filter(|p| in_atlas.contains(p))
                .collect();
            for i in 0..files_v.len() {
                for j in (i + 1)..files_v.len() {
                    if files_v[i] == files_v[j] { continue; }
                    let k = if files_v[i] < files_v[j] { format!("{} {}", files_v[i], files_v[j]) } else { format!("{} {}", files_v[j], files_v[i]) };
                    if !pair_count.contains_key(&k) { pair_order.push(k.clone()); }
                    *pair_count.entry(k).or_insert(0) += 1;
                }
            }
        }
        let mut flagged: Vec<(String, i64)> = pair_order.into_iter().map(|k| { let n = pair_count[&k]; (k, n) }).filter(|(_, n)| *n >= 6).collect();
        flagged.sort_by(|a, b| b.1.cmp(&a.1)); // stable desc — ties keep insertion order (gotcha #8)
        flagged.truncate(40);
        for (k, n) in flagged {
            let mut parts = k.splitn(2, ' ');
            let a = parts.next().unwrap_or("").to_string();
            let b = parts.next().unwrap_or("").to_string();
            rows.push(json!({
                "category": "history", "kind": "co-change", "severity": "low",
                "path": a, "line": Value::Null,
                "detail": format!("changes together with {b} in {n} recent commits (hidden coupling)"),
            }));
            co_change += 1;
        }
    }

    let written = write_graph_signals(db, &origin, &rows).is_ok();

    let mut api: Value = json!({ "total": 0i64, "added": 0i64, "removed": 0i64 });
    if let Ok(counts) = compute_and_write_api(db, &origin) {
        api = counts;
    }

    let mut by_kind = Map::new(); // first-seen kind order (gotcha #1)
    for r in &rows {
        let k = str_qq(r.get("kind"));
        let next = by_kind.get(&k).and_then(Value::as_i64).unwrap_or(0) + 1;
        by_kind.insert(k, json!(next));
    }

    Ok(json!({
        "origin": origin,
        "written": written,
        "signals": rows.len() as i64,
        "by_kind": Value::Object(by_kind),
        "api": api,
        "dead_exports": dead_exports,
        "orphan_files": orphan_files,
        "bus_factor": bus_factor,
        "co_change": co_change,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE code_nodes (id TEXT, origin TEXT NOT NULL, path TEXT NOT NULL, type TEXT NOT NULL, name TEXT, signature TEXT, exported INTEGER); \
             CREATE TABLE code_edges (origin TEXT NOT NULL, from_node TEXT, to_node TEXT NOT NULL, type TEXT NOT NULL, kind TEXT); \
             CREATE TABLE code_entry_points (origin TEXT NOT NULL, path TEXT NOT NULL); \
             CREATE TABLE code_signals (origin TEXT, category TEXT, kind TEXT, severity TEXT, path TEXT, line INTEGER, detail TEXT); \
             CREATE TABLE code_api_snapshots (id INTEGER PRIMARY KEY AUTOINCREMENT, origin TEXT NOT NULL, counts_json TEXT NOT NULL, api_json TEXT NOT NULL, diff_json TEXT)",
        )
        .unwrap();
        c
    }

    #[test]
    fn git_history_returns_null_for_a_non_git_directory() {
        // std::env::temp_dir() is not a git work tree on this dev/CI layout (code-git-info.rs
        // precedent uses the same fact for its own non-repo test).
        let root = std::env::temp_dir().to_string_lossy().into_owned();
        assert_eq!(gitHistory(&root, 3000, 500), Value::Null);
    }

    #[test]
    fn parse_git_history_builds_by_file_and_commits_respecting_churn_limit() {
        let log = "@sha1\tAlice\nsrc/a.js\nsrc/b.js\n@sha2\tBob\nsrc/a.js\n@sha3\tAlice\nsrc/a.js\n";
        let (by_file, commits) = parse_git_history(log, 2);
        assert_eq!(commits.len(), 3);
        assert_eq!(commits[0], json!({ "sha": "sha1", "author": "Alice", "files": ["src/a.js", "src/b.js"] }));
        assert_eq!(commits[1], json!({ "sha": "sha2", "author": "Bob", "files": ["src/a.js"] }));
        assert_eq!(commits[2], json!({ "sha": "sha3", "author": "Alice", "files": ["src/a.js"] }));
        assert_eq!(by_file.get("src/a.js").unwrap()["commits"], json!(2)); // idx0+idx1 counted, idx2 beyond churn_limit=2
        assert_eq!(by_file.get("src/a.js").unwrap()["authors"], json!(["Alice", "Bob"]));
        assert_eq!(by_file.get("src/b.js").unwrap(), &json!({ "authors": ["Alice"], "commits": 1 }));
    }

    #[test]
    fn dead_export_flagged_unless_referenced_or_entry_point_null_name_renders_null() {
        let c = db();
        c.execute_batch(
            "INSERT INTO code_nodes (id, origin, path, type, name, signature, exported) VALUES \
             ('def-foo','atlas','src/a.js','function','foo','()',1), \
             ('def-bar','atlas','src/b.js','function','bar','()',1), \
             ('def-baz','atlas','src/entry.js','function','baz','()',1), \
             ('def-noname','atlas','src/c.js','function',NULL,NULL,1); \
             INSERT INTO code_edges (origin, from_node, to_node, type) VALUES ('atlas','src/b.js','def-bar','calls'); \
             INSERT INTO code_entry_points (origin, path) VALUES ('atlas','src/entry.js');",
        )
        .unwrap();
        let r = codeSignalsGraph(&json!({ "history": null }), &c).unwrap();
        assert_eq!(r["dead_exports"], json!(2));
        assert_eq!(r["by_kind"]["dead-export"], json!(2));
        let rows = crate::db_json::query_rows(&c, "SELECT path, detail FROM code_signals WHERE kind='dead-export' ORDER BY path", &[]).unwrap();
        let arr = rows.as_array().unwrap();
        assert_eq!(arr[0]["path"], json!("src/a.js"));
        assert!(arr[0]["detail"].as_str().unwrap().contains("'foo' is exported"));
        assert_eq!(arr[1]["path"], json!("src/c.js"));
        assert!(arr[1]["detail"].as_str().unwrap().contains("'null' is exported"), "String(null) renders \"null\", not empty");
    }

    #[test]
    fn orphan_file_flagged_unless_imported_entry_point_or_regex_exempt() {
        let c = db();
        c.execute_batch(
            "INSERT INTO code_nodes (id, origin, path, type) VALUES \
             ('file-a','atlas','src/a.js','file'), \
             ('file-b','atlas','src/b.js','file'), \
             ('file-index','atlas','src/index.js','file'), \
             ('file-test','atlas','src/util.test.js','file'), \
             ('file-entry','atlas','src/entry.js','file'), \
             ('file-mocks','atlas','src/__mocks__/thing.js','file'); \
             INSERT INTO code_edges (origin, from_node, to_node, type) VALUES ('atlas','src/consumer.js','file-b','imports'); \
             INSERT INTO code_entry_points (origin, path) VALUES ('atlas','src/entry.js');",
        )
        .unwrap();
        let r = codeSignalsGraph(&json!({ "history": null }), &c).unwrap();
        assert_eq!(r["orphan_files"], json!(1));
        let rows = crate::db_json::query_rows(&c, "SELECT path FROM code_signals WHERE kind='orphan-file'", &[]).unwrap();
        assert_eq!(rows, json!([{ "path": "src/a.js" }]));
    }

    #[test]
    fn write_replaces_debt_and_history_categories_scoped_by_origin_only() {
        let c = db();
        c.execute_batch(
            "INSERT INTO code_signals (origin, category, kind, severity, path, line, detail) VALUES \
             ('atlas','debt','stale-debt','low','stale.js',1,'should be replaced'), \
             ('atlas','history','stale-history','low','stale2.js',1,'should be replaced'), \
             ('atlas','safety','keep-me','high','keep.js',1,'not touched'), \
             ('other','debt','keep-too','low','other.js',1,'different origin');",
        )
        .unwrap();
        let r = codeSignalsGraph(&json!({ "origin": "atlas", "history": null }), &c).unwrap();
        assert_eq!(r["written"], json!(true));
        let stale = crate::db_json::query_rows(&c, "SELECT * FROM code_signals WHERE kind IN ('stale-debt','stale-history')", &[]).unwrap();
        assert_eq!(stale.as_array().unwrap().len(), 0);
        let safety = crate::db_json::query_rows(&c, "SELECT * FROM code_signals WHERE kind='keep-me'", &[]).unwrap();
        assert_eq!(safety.as_array().unwrap().len(), 1);
        let other = crate::db_json::query_rows(&c, "SELECT * FROM code_signals WHERE origin='other'", &[]).unwrap();
        assert_eq!(other.as_array().unwrap().len(), 1);
    }

    #[test]
    fn api_census_first_rebuild_no_prior() {
        let c = db();
        c.execute("INSERT INTO code_nodes (id, origin, path, type, name, signature, exported) VALUES ('def-x','atlas','src/x.js','function','x','',1)", []).unwrap();
        let r = codeSignalsGraph(&json!({ "history": null }), &c).unwrap();
        assert_eq!(r["api"], json!({ "total": 1, "added": 1, "removed": 0 }));
        let snap = crate::db_json::query_rows(&c, "SELECT api_json FROM code_api_snapshots WHERE origin='atlas' ORDER BY id DESC LIMIT 1", &[]).unwrap();
        let api_json = snap.as_array().unwrap()[0]["api_json"].as_str().unwrap();
        let parsed: Value = serde_json::from_str(api_json).unwrap();
        assert_eq!(parsed, json!([{ "path": "src/x.js", "name": "x", "signature": "", "type": "function" }]));
    }

    #[test]
    fn api_census_added_and_removed_against_a_prior_snapshot() {
        let c = db();
        let prior = serde_json::to_string(&json!([
            { "path": "src/old.js", "name": "oldFn", "signature": "", "type": "function" },
            { "path": "src/keep.js", "name": "keepFn", "signature": "x", "type": "function" },
        ]))
        .unwrap();
        c.execute(
            "INSERT INTO code_api_snapshots (origin, counts_json, api_json, diff_json) VALUES ('atlas','{}',?1,'{}')",
            rusqlite::params![prior],
        )
        .unwrap();
        c.execute_batch(
            "INSERT INTO code_nodes (id, origin, path, type, name, signature, exported) VALUES \
             ('def-keep','atlas','src/keep.js','function','keepFn','x',1), \
             ('def-new','atlas','src/new.js','function','newFn','',1);",
        )
        .unwrap();
        let r = codeSignalsGraph(&json!({ "history": null }), &c).unwrap();
        assert_eq!(r["api"]["total"], json!(2));
        assert_eq!(r["api"]["added"], json!(1));
        assert_eq!(r["api"]["removed"], json!(1));
        let snap = crate::db_json::query_rows(&c, "SELECT diff_json FROM code_api_snapshots WHERE origin='atlas' ORDER BY id DESC LIMIT 1", &[]).unwrap();
        let diff_json = snap.as_array().unwrap()[0]["diff_json"].as_str().unwrap();
        let diff: Value = serde_json::from_str(diff_json).unwrap();
        assert_eq!(diff["added"], json!([{ "path": "src/new.js", "name": "newFn" }]));
        assert_eq!(diff["removed"], json!([{ "sig": "src/old.js#oldFn()" }]));
    }

    #[test]
    fn origin_scoping_isolates_nodes_edges_entry_points_and_signals() {
        let c = db();
        c.execute_batch(
            "INSERT INTO code_nodes (id, origin, path, type, name, exported) VALUES \
             ('def-x','atlas','src/x.js','function','x',1), \
             ('def-y','other','src/y.js','function','y',1), \
             ('file-y','other','src/y.js','file',NULL,NULL), \
             ('file-atlas-orphan','atlas','src/atlasonly.js','file',NULL,NULL); \
             INSERT INTO code_entry_points (origin, path) VALUES ('atlas','src/x.js'); \
             INSERT INTO code_signals (origin, category, kind, severity, path, line, detail) VALUES \
             ('atlas','debt','todo','low','keep-atlas.js',1,'should remain untouched');",
        )
        .unwrap();
        let r = codeSignalsGraph(&json!({ "origin": "other", "history": null }), &c).unwrap();
        assert_eq!(r["dead_exports"], json!(1));
        assert_eq!(r["orphan_files"], json!(1));
        let atlas_signals = crate::db_json::query_rows(&c, "SELECT * FROM code_signals WHERE origin='atlas'", &[]).unwrap();
        assert_eq!(atlas_signals.as_array().unwrap().len(), 1);
    }

    #[test]
    fn history_null_skips_bus_factor_and_co_change_cleanly() {
        let c = db();
        c.execute("INSERT INTO code_nodes (id, origin, path, type) VALUES ('file-a','atlas','src/a.js','file')", []).unwrap();
        let r = codeSignalsGraph(&json!({ "history": null }), &c).unwrap();
        assert_eq!(r["bus_factor"], json!(0));
        assert_eq!(r["co_change"], json!(0));
    }

    #[test]
    fn history_provided_directly_drives_bus_factor_and_co_change() {
        // Disclosed real-but-fixture-untested branch (see module doc), exercised here by
        // hand-building the `history` shape `gitHistory` produces (a JS `Map` has no
        // fixture-representable form; this native test IS the coverage for this path).
        let c = db();
        c.execute_batch(
            "INSERT INTO code_nodes (id, origin, path, type) VALUES \
             ('file-a','atlas','src/a.js','file'), \
             ('file-b','atlas','src/b.js','file'), \
             ('file-c','atlas','src/c.js','file');",
        )
        .unwrap();
        let history = json!({
            "by_file": {
                "src/a.js": { "authors": ["Alice"], "commits": 5 },
                "src/not-in-atlas.js": { "authors": ["Alice"], "commits": 9 },
                "src/multi-author.js": { "authors": ["Alice", "Bob"], "commits": 9 },
            },
            "commits": [
                { "sha": "s1", "author": "Alice", "files": ["src/a.js", "src/b.js", "src/c.js"] },
                { "sha": "s2", "author": "Alice", "files": ["src/a.js", "src/b.js"] },
                { "sha": "s3", "author": "Alice", "files": ["src/a.js", "src/b.js"] },
                { "sha": "s4", "author": "Alice", "files": ["src/a.js", "src/b.js"] },
                { "sha": "s5", "author": "Alice", "files": ["src/a.js", "src/b.js"] },
                { "sha": "s6", "author": "Alice", "files": ["src/a.js", "src/b.js"] },
            ],
        });
        let r = codeSignalsGraph(&json!({ "history": history }), &c).unwrap();
        assert_eq!(r["bus_factor"], json!(1), "only src/a.js is in-atlas + >=5 commits + exactly one author");
        assert_eq!(r["co_change"], json!(1), "a.js+b.js co-change 6x (>=6 threshold); c.js pairs stay under 6");
        let co_change_rows = crate::db_json::query_rows(&c, "SELECT path, detail FROM code_signals WHERE kind='co-change'", &[]).unwrap();
        let arr = co_change_rows.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["path"], json!("src/a.js"));
        assert!(arr[0]["detail"].as_str().unwrap().contains("src/b.js in 6 recent commits"));
    }
}
