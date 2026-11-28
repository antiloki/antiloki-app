//! constitution-estimate.rs — twin of skills/constitution/constitution-estimate.mjs (db lane,
//! cross-db `laws`+`judgments`). NAMES ARE CONTRACT: `constitutionEstimate` (default). THE
//! WIZARD's estimate-first number (docs/constitution-review-plan.html D6): before a rebuild,
//! how many files are in scope, how many replay from cache (unchanged content AND unchanged
//! constitution), how many must be judged fresh, and a projected time from the last run.
//!
//! CROSS-MODULE INLINING (playbook lesson 6's pattern, applied to a sibling *skill file* rather
//! than a roots.mjs const): the JS imports `composedConstitution` from `./constitution-law.mjs`
//! — a NAMED export, not that module's dispatched default (`constitutionLaw`), and
//! constitution-law.mjs itself has no Rust twin yet. Converting the whole sibling module just to
//! reach one small helper would be a false block, so `composed_constitution` below inlines that
//! helper's exact logic (verbatim SQL + the same `SCOPES` ordering array from schema.mjs,
//! duplicated here since `_helpers/schema.rs`'s copy is private) — it reuses only the
//! ALREADY-CONVERTED twins `ensureConstitutionSchema`/`normalizeScope`, never touching
//! constitution-law.mjs's own op-dispatch surface. Private (no `pub`): it has no direct JS
//! export counterpart from THIS file, so marking it `pub` would falsely trip the names_ok gate
//! (playbook lesson 3).
//!
//! REAL FILESYSTEM (fixture-controllable, stack-facts.rs precedent): `root` is DB-derived via
//! the already-converted `engine_workspace_root::getWorkspaceRoot` twin, so a fixture seeds an
//! active `workspaces` row pointing at a real on-disk testdata dir
//! (`antiloki-crossing/fixtures/testdata/constitution-estimate/`, committed alongside this
//! unit's fixture JSON) and both lanes walk/read the identical bytes. `walk()`'s directory-listing
//! ORDER is never observed — the JS source re-sorts (`out.sort()`) before returning, so as long as
//! fixture testdata stays small enough that the `cap*4` early-exit never trips (verified: this
//! unit's fixture data is 3 code files), OS-level `readdir` enumeration order differences between
//! Node and Rust cannot affect the compared output.
//!
//! GIT-DIFF BRANCH — DISCLOSED UNTESTED (playbook lesson 5's flag-skippable pattern): the JS
//! `execSync('git diff --name-only ...')` branch fires ONLY when `input.base` is truthy; this
//! twin ports it as a real `git` subprocess call (`git_diff_paths`) for production parity, but no
//! fixture case sets `input.base` — a real git diff depends on the actual repo/commit state, which
//! is not deterministically fixture-controllable the way a committed testdata dir is. Minor
//! disclosed gap: the JS embeds `${base}...HEAD` as ONE shell-interpreted string (so a `base`
//! containing whitespace could split into extra shell tokens); this twin passes it as a single
//! `Command` arg (no shell), which is unreachable in practice (git ref names don't contain spaces)
//! and irrelevant since the branch carries no fixture coverage either way.
//!
//! `context.db` missing → JS throws `err.required('constitution-estimate', 'context.db')`. Not
//! modeled: `db: &Connection` is a required, non-nullable reference here — same "guard-throws are
//! not exercised" convention used by every sibling twin.
// QUALITY: 78/100 — real fs walk + read, fixture-backed via a committed testdata dir; one
// disclosed-untested branch (git-diff, flag-skippable per lesson 5)

use rusqlite::Connection;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

fn truthy(v: Option<&Value>) -> bool { match v { None|Some(Value::Null)=>false, Some(Value::Bool(b))=>*b, Some(Value::Number(n))=>n.as_f64().map_or(false,|f|f!=0.0&&!f.is_nan()), Some(Value::String(s))=>!s.is_empty(), Some(_)=>true } }
fn str_qq(v: Option<&Value>) -> String { match v { None|Some(Value::Null)=>String::new(), Some(Value::String(s))=>s.clone(), Some(Value::Bool(b))=>b.to_string(), Some(n@Value::Number(_))=>n.to_string(), Some(o)=>o.to_string() } } // = JS String(v ?? '')
fn js_string(v: &Value) -> String { match v { Value::Null=>"null".into(), Value::String(s)=>s.clone(), Value::Number(n)=>n.to_string(), Value::Bool(b)=>b.to_string(), Value::Array(a)=>a.iter().map(js_string).collect::<Vec<_>>().join(","), Value::Object(_)=>"[object Object]".into() } } // = JS String(v)
fn tmpl(v: Option<&Value>) -> String { match v { None=>"undefined".into(), Some(Value::Null)=>"null".into(), Some(x)=>str_qq(Some(x)) } } // = JS `${v}`
fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) } // JS `??` right operand
fn js_number(v: Option<&Value>) -> f64 { match v { None=>f64::NAN, Some(Value::Null)=>0.0, Some(Value::Bool(b))=>if *b {1.0} else {0.0}, Some(Value::Number(n))=>n.as_f64().unwrap_or(f64::NAN), Some(Value::String(s))=>{let t=s.trim(); if t.is_empty(){0.0}else{t.parse().unwrap_or(f64::NAN)}}, Some(_)=>f64::NAN } } // = JS Number(v)

// JS `Math.round`: floor(x + 0.5) — rounds HALF UP, unlike Rust's half-away-from-zero `.round()`.
fn js_round(x: f64) -> f64 { (x + 0.5).floor() }

// Delegates to the ONE capability list (ast-parse's adapter table filtered to grammars that
// actually have a walk profile). This used to be a private copy of
// `(mjs|cjs|js|jsx|ts|tsx|py)`; four skills each held one, none ever gained `rs`, and the
// review pass walked straight past every Rust file while reporting success.
fn code_re() -> &'static regex::Regex { crate::codebase_ast_parse::analyzable_ext_regex() }
const WALK_SKIP: [&str; 8] = ["node_modules", ".git", "dist", "build", "coverage", ".antiloki", ".next", "out"];
// same order as constitution/_helpers/schema.mjs's SCOPES (schema.rs's copy is private, so this
// unit carries its own — see module doc's cross-module-inlining note).
const SCOPES: [&str; 6] = ["business", "architecture", "project", "framework", "community", "language"];

/// `node:path` win32 `isAbsolute` (leading separator, or drive-letter + `:` + separator) — the
/// dev/CI platform for this substrate; see `engine_workspace_root.rs`'s module doc for the same
/// disclosed win32-only scope (its copy is private, so this unit carries its own).
fn win_is_absolute(p: &str) -> bool {
    let b = p.as_bytes();
    if b.is_empty() { return false; }
    if b[0] == b'/' || b[0] == b'\\' { return true; }
    if b.len() > 2 && b[0].is_ascii_alphabetic() && b[1] == b':' && (b[2] == b'/' || b[2] == b'\\') { return true; }
    false
}

fn abs_path(root: &str, p: &str) -> PathBuf {
    if win_is_absolute(p) { PathBuf::from(p) } else { Path::new(root).join(p) }
}

/// `.slice(0, end)` on a string array (JS negative `end` counts back from the length).
fn js_slice0(arr: Vec<String>, end: i64) -> Vec<String> {
    let len = arr.len() as i64;
    let e = if end < 0 { (len + end).max(0) } else { end.min(len) };
    arr.into_iter().take(e.max(0) as usize).collect()
}

/// `function walk(root, rel, cap)` — bounded DFS via readdir, skipping WALK_SKIP/dot dirs,
/// collecting CODE_RE-matching file names as root-relative forward-slash paths, sorted at the
/// end (traversal order is never observed downstream — see module doc).
fn walk(root: &str, rel_folder: &str, cap: i64) -> Vec<String> {
    let (base_dir, init_prefix) = if rel_folder == "." {
        (PathBuf::from(root), String::new())
    } else {
        (PathBuf::from(root).join(rel_folder), rel_folder.to_string())
    };
    let limit: usize = if cap < 0 { 0 } else { cap.saturating_mul(4).max(0) as usize };
    let mut out: Vec<String> = Vec::new();
    let mut stack: Vec<(PathBuf, String)> = vec![(base_dir, init_prefix)];
    while let Some((dir, prefix)) = stack.pop() {
        if out.len() >= limit { break; }
        let entries = match std::fs::read_dir(&dir) { Ok(e) => e, Err(_) => continue };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
            let child_rel = if prefix.is_empty() { name.clone() } else { format!("{prefix}/{name}") };
            if is_dir {
                if !WALK_SKIP.contains(&name.as_str()) && !name.starts_with('.') {
                    stack.push((dir.join(&name), child_rel));
                }
            } else if code_re().is_match(&name) {
                out.push(child_rel);
            }
        }
    }
    out.sort();
    out
}

/// `String(input?.folder ?? '.').trim() || '.'`
fn resolved_folder(input: &Value) -> String {
    let raw = match nz(input.get("folder")) { Some(v) => js_string(v), None => ".".to_string() };
    let trimmed = raw.trim();
    if trimmed.is_empty() { ".".to_string() } else { trimmed.to_string() }
}

/// `input.files.map((f) => String(typeof f === 'string' ? f : f?.path ?? '')).filter(Boolean)`
fn resolve_files_paths(files: &[Value]) -> Vec<String> {
    files
        .iter()
        .map(|f| match f {
            Value::String(s) => s.clone(),
            _ => match nz(f.get("path")) { Some(v) => js_string(v), None => String::new() },
        })
        .filter(|s| !s.is_empty())
        .collect()
}

/// The `execSync('git diff --name-only <base>...HEAD')` branch — see module doc: real subprocess,
/// disclosed UNTESTED by fixture (git-state dependent, flag-skippable per playbook lesson 5).
fn git_diff_paths(root: &str, base: &str) -> Vec<String> {
    let output = crate::proc::cmd("git")
        .arg("diff")
        .arg("--name-only")
        .arg(format!("{base}...HEAD"))
        .current_dir(root)
        .output();
    match output {
        Ok(o) if o.status.success() => {
            let text = String::from_utf8_lossy(&o.stdout);
            let mut seen = HashSet::new();
            let mut out = Vec::new();
            for line in text.split('\n') {
                let t = line.trim();
                if !t.is_empty() && seen.insert(t.to_string()) {
                    out.push(t.to_string());
                }
            }
            out
        }
        _ => Vec::new(),
    }
}

/// `Number.isInteger(x)`: a JSON number whose value is finite and has no fractional part
/// (strings, bools, etc. are never integers here — matches JS's type-strict check).
fn js_is_integer(v: Option<&Value>) -> bool {
    matches!(v, Some(Value::Number(n)) if n.as_f64().map_or(false, |f| f.is_finite() && f.fract() == 0.0))
}

/// `composedConstitution(db)` (constitution-law.mjs, named export) — inlined here, see module
/// doc. Returns `(laws, exceptions)`, each a `Vec` of row objects (`laws`' `scope` normalized,
/// sorted by SCOPES order → weight desc → key asc; `exceptions` ordered by `law_key`).
fn composed_constitution(db: &Connection) -> Result<(Vec<Value>, Vec<Value>), String> {
    let _ = crate::constitution__helpers_schema::ensureConstitutionSchema(db);
    let mut laws: Vec<Value> = crate::db_json::query_rows(db, "SELECT * FROM laws.review_laws", &[])?
        .as_array()
        .cloned()
        .unwrap_or_default();
    for l in laws.iter_mut() {
        if let Value::Object(map) = l {
            let normalized = crate::constitution__helpers_schema::normalizeScope(map.get("scope").unwrap_or(&Value::Null));
            map.insert("scope".to_string(), normalized);
        }
    }
    laws.sort_by(|a, b| {
        let ai = SCOPES.iter().position(|s| Some(*s) == a.get("scope").and_then(Value::as_str)).map(|x| x as i64).unwrap_or(-1);
        let bi = SCOPES.iter().position(|s| Some(*s) == b.get("scope").and_then(Value::as_str)).map(|x| x as i64).unwrap_or(-1);
        ai.cmp(&bi)
            .then_with(|| {
                let aw = a.get("weight").and_then(Value::as_i64).unwrap_or(0);
                let bw = b.get("weight").and_then(Value::as_i64).unwrap_or(0);
                bw.cmp(&aw)
            })
            .then_with(|| {
                let ak = a.get("key").and_then(Value::as_str).unwrap_or("");
                let bk = b.get("key").and_then(Value::as_str).unwrap_or("");
                ak.cmp(bk)
            })
    });
    let exceptions: Vec<Value> = crate::db_json::query_rows(db, "SELECT * FROM laws.review_law_exceptions ORDER BY law_key", &[])?
        .as_array()
        .cloned()
        .unwrap_or_default();
    Ok((laws, exceptions))
}

/// `function constitutionHash(laws, exceptions)`
fn constitution_hash(laws: &[Value], exceptions: &[Value]) -> String {
    let mut parts: Vec<String> = laws
        .iter()
        .filter(|l| l.get("status").and_then(Value::as_str) == Some("ratified") && l.get("enforcement").and_then(Value::as_str) == Some("score"))
        .map(|l| format!("{}@{}:{}:{}", tmpl(l.get("key")), tmpl(l.get("version")), tmpl(l.get("weight")), tmpl(l.get("detector"))))
        .collect();
    parts.sort();
    let mut ex: Vec<String> = exceptions.iter().map(|e| format!("{}:{}", tmpl(e.get("law_key")), tmpl(e.get("path_glob")))).collect();
    ex.sort();
    let payload = format!("{}#{}", parts.join("|"), ex.join("|"));
    hex::encode(Sha256::digest(payload.as_bytes())).chars().take(16).collect()
}

/// `export default function constitutionEstimate(input, context)`
pub fn constitutionEstimate(input: &Value, db: &Connection) -> Result<Value, String> {
    let _ = crate::constitution__helpers_schema::ensureConstitutionSchema(db);
    let root_v = crate::engine_workspace_root::getWorkspaceRoot(db);
    let root = root_v.as_str().unwrap_or_default().to_string();
    let cap: i64 = if js_is_integer(input.get("max_files")) {
        input.get("max_files").and_then(Value::as_f64).unwrap_or(1200.0) as i64
    } else {
        1200
    };

    let (laws, exceptions) = composed_constitution(db)?;
    let c_hash = constitution_hash(&laws, &exceptions);

    let paths_raw: Vec<String> = if let Some(arr) = input.get("files").and_then(Value::as_array).filter(|a| !a.is_empty()) {
        resolve_files_paths(arr)
    } else if truthy(input.get("base")) {
        // JS: `` `${input.base}...HEAD` `` — a template literal, JS String(v) semantics, not
        // String(v ?? ''). base is guaranteed non-null/non-undefined here (gated by the truthy()
        // check above), so js_string's Array/Object coercion (comma-join / "[object Object]")
        // applies correctly where str_qq's raw-JSON fallback would diverge for those shapes.
        git_diff_paths(&root, &js_string(input.get("base").unwrap()))
    } else {
        walk(&root, &resolved_folder(input), cap)
    };

    let mut seen: HashSet<String> = HashSet::new();
    let deduped: Vec<String> = paths_raw
        .into_iter()
        .filter(|p| code_re().is_match(p))
        .filter(|p| seen.insert(p.clone()))
        .collect();
    let capped = js_slice0(deduped, cap);
    let scoped: Vec<String> = capped.into_iter().filter(|p| abs_path(&root, p).exists()).collect();

    let prior_row: Option<Value> = crate::db_json::query_rows(
        db,
        "SELECT id FROM judgments.review_snapshots WHERE status='done' AND constitution_hash=? ORDER BY id DESC LIMIT 1",
        &[json!(c_hash)],
    )?
    .as_array()
    .and_then(|a| a.first())
    .cloned();
    let prior_id = prior_row.as_ref().and_then(|r| r.get("id")).and_then(Value::as_i64);

    let mut cached: i64 = 0;
    if let Some(pid) = prior_id {
        for p in &scoped {
            let src = match std::fs::read_to_string(abs_path(&root, p)) { Ok(s) => s, Err(_) => continue };
            let h: String = hex::encode(Sha256::digest(src.as_bytes())).chars().take(16).collect();
            let hit = crate::db_json::query_rows(
                db,
                "SELECT 1 FROM judgments.review_scores WHERE snapshot_id=? AND path=? AND content_hash=? LIMIT 1",
                &[json!(pid), json!(p.clone()), json!(h)],
            )?;
            if hit.as_array().map_or(false, |a| !a.is_empty()) {
                cached += 1;
            }
        }
    }
    let compute: i64 = scoped.len() as i64 - cached;

    let last_row: Option<Value> = crate::db_json::query_rows(
        db,
        "SELECT duration_ms, judged FROM judgments.review_snapshots WHERE status='done' AND judged>0 ORDER BY id DESC LIMIT 1",
        &[],
    )?
    .as_array()
    .and_then(|a| a.first())
    .cloned();
    let per_file_ms: f64 = match &last_row {
        Some(row) => {
            let judged = row.get("judged").and_then(Value::as_f64).unwrap_or(1.0);
            js_number(row.get("duration_ms")) / judged
        }
        None => 4.0,
    };
    let est_ms = js_round(compute as f64 * per_file_ms + 40.0) as i64;

    let list_true = matches!(input.get("list"), Some(Value::Bool(true)));
    let files_out: Option<Vec<Value>> = if list_true {
        let snap_id: Option<i64> = crate::db_json::query_rows(
            db,
            "SELECT id FROM judgments.review_snapshots WHERE status='done' ORDER BY id DESC LIMIT 1",
            &[],
        )?
        .as_array()
        .and_then(|a| a.first())
        .and_then(|r| r.get("id"))
        .and_then(Value::as_i64);
        let mut list = Vec::with_capacity(scoped.len());
        for p in &scoped {
            let health = match snap_id {
                Some(sid) => crate::db_json::query_rows(
                    db,
                    "SELECT health FROM judgments.review_scores WHERE snapshot_id=? AND path=? AND unit_kind='file'",
                    &[json!(sid), json!(p.clone())],
                )?
                .as_array()
                .and_then(|a| a.first())
                .and_then(|row| row.get("health"))
                .cloned()
                .unwrap_or(Value::Null),
                None => Value::Null,
            };
            list.push(json!({ "path": p.clone(), "health": health }));
        }
        Some(list)
    } else {
        None
    };

    let reused = match &prior_row {
        Some(row) => format!(
            "{} of {} files replay from snapshot #{} (unchanged + same constitution)",
            cached,
            scoped.len(),
            tmpl(row.get("id"))
        ),
        None => "first rebuild at this constitution — all files compute".to_string(),
    };
    let changed_note: Value = if prior_row.is_some() && compute == 0 { json!("nothing changed — this rebuild is a free refresh") } else { Value::Null };

    let mut out = Map::new();
    out.insert("ok".into(), json!(true));
    out.insert("total".into(), json!(scoped.len() as i64));
    out.insert("cached".into(), json!(cached));
    out.insert("compute".into(), json!(compute));
    out.insert("est_ms".into(), json!(est_ms));
    if let Some(f) = files_out {
        out.insert("files".into(), json!(f));
    }
    out.insert("constitution_hash".into(), json!(c_hash));
    out.insert("reused".into(), json!(reused));
    out.insert("changed_note".into(), changed_note);
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROOT: &str = concat!(env!("ANTILOKI_FIXTURES"), "/constitution-estimate");

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS laws; ATTACH ':memory:' AS judgments").unwrap();
        c.execute_batch(
            "CREATE TABLE workspaces (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL UNIQUE, \
             root_path TEXT NOT NULL, vcs TEXT NOT NULL DEFAULT 'git', is_active INTEGER NOT NULL DEFAULT 0, \
             created_at TEXT NOT NULL DEFAULT '2026-01-01 00:00:00')",
        )
        .unwrap();
        c
    }
    fn seed_active(c: &Connection, root: &str) {
        c.execute("INSERT INTO workspaces (name, root_path, is_active) VALUES ('x', ?1, 1)", rusqlite::params![root]).unwrap();
    }

    #[test]
    fn default_walk_no_prior_snapshot() {
        let c = db();
        seed_active(&c, ROOT);
        let r = constitutionEstimate(&json!({}), &c).unwrap();
        assert_eq!(r["total"], json!(3));
        assert_eq!(r["cached"], json!(0));
        assert_eq!(r["compute"], json!(3));
        assert_eq!(r["est_ms"], json!(52));
        assert_eq!(r["constitution_hash"], json!("334359b90efed75d"));
        assert_eq!(r["reused"], json!("first rebuild at this constitution — all files compute"));
        assert_eq!(r["changed_note"], Value::Null);
        assert!(r.get("files").is_none());
    }

    #[test]
    fn partial_cache_against_prior_snapshot_matching_hash() {
        let c = db();
        seed_active(&c, ROOT);
        crate::constitution__helpers_schema::ensureConstitutionSchema(&c);
        c.execute(
            "INSERT INTO judgments.review_snapshots (id, constitution_hash, status, judged, duration_ms) VALUES (1, '334359b90efed75d', 'done', 50, 1000)",
            [],
        )
        .unwrap();
        c.execute("INSERT INTO judgments.review_scores (snapshot_id, path, content_hash, unit_kind) VALUES (1, 'src/a.mjs', '037ecd1db38c230c', 'file')", []).unwrap();
        c.execute("INSERT INTO judgments.review_scores (snapshot_id, path, content_hash, unit_kind) VALUES (1, 'src/b.ts', 'deadbeefdeadbeef', 'file')", []).unwrap();
        let r = constitutionEstimate(&json!({}), &c).unwrap();
        assert_eq!(r["total"], json!(3));
        assert_eq!(r["cached"], json!(1));
        assert_eq!(r["compute"], json!(2));
        assert_eq!(r["est_ms"], json!(80));
        assert_eq!(r["reused"], json!("1 of 3 files replay from snapshot #1 (unchanged + same constitution)"));
        assert_eq!(r["changed_note"], Value::Null);
    }

    #[test]
    fn list_true_health_from_latest_done_snapshot_independent_of_hash_match() {
        let c = db();
        seed_active(&c, ROOT);
        crate::constitution__helpers_schema::ensureConstitutionSchema(&c);
        c.execute("INSERT INTO judgments.review_snapshots (id, constitution_hash, status) VALUES (1, 'nonmatching-hash', 'done')", []).unwrap();
        c.execute("INSERT INTO judgments.review_scores (snapshot_id, path, unit_kind, health) VALUES (1, 'lib/c.py', 'file', 70)", []).unwrap();
        c.execute("INSERT INTO judgments.review_scores (snapshot_id, path, unit_kind, health) VALUES (1, 'src/a.mjs', 'file', 90)", []).unwrap();
        let r = constitutionEstimate(&json!({ "list": true }), &c).unwrap();
        assert_eq!(
            r["files"],
            json!([
                { "path": "lib/c.py", "health": 70 },
                { "path": "src/a.mjs", "health": 90 },
                { "path": "src/b.ts", "health": null },
            ])
        );
        assert_eq!(r["cached"], json!(0)); // prior (hash-filtered) found nothing
    }

    #[test]
    fn input_files_dedup_existssync_filter_and_non_string_mapping() {
        let c = db();
        seed_active(&c, ROOT);
        let r = constitutionEstimate(
            &json!({ "files": ["src/a.mjs", "src/a.mjs", "not-real.mjs", { "path": "src/b.ts" }, 42] }),
            &c,
        )
        .unwrap();
        assert_eq!(r["total"], json!(2));
        assert_eq!(r["est_ms"], json!(48));
    }

    #[test]
    fn folder_scoping_keeps_root_relative_prefix() {
        let c = db();
        seed_active(&c, ROOT);
        let r = constitutionEstimate(&json!({ "folder": "src" }), &c).unwrap();
        assert_eq!(r["total"], json!(2));
    }

    #[test]
    fn max_files_caps_the_scoped_list() {
        let c = db();
        seed_active(&c, ROOT);
        let r = constitutionEstimate(&json!({ "max_files": 1 }), &c).unwrap();
        assert_eq!(r["total"], json!(1));
        assert_eq!(r["est_ms"], json!(44));
    }

    #[test]
    fn non_integer_max_files_falls_back_to_default_cap() {
        let c = db();
        seed_active(&c, ROOT);
        let r = constitutionEstimate(&json!({ "max_files": "3" }), &c).unwrap();
        assert_eq!(r["total"], json!(3));
    }

    #[test]
    fn full_cache_hit_reports_changed_note() {
        let c = db();
        seed_active(&c, ROOT);
        crate::constitution__helpers_schema::ensureConstitutionSchema(&c);
        c.execute("INSERT INTO judgments.review_snapshots (id, constitution_hash, status) VALUES (1, '334359b90efed75d', 'done')", []).unwrap();
        c.execute("INSERT INTO judgments.review_scores (snapshot_id, path, content_hash, unit_kind) VALUES (1, 'src/a.mjs', '037ecd1db38c230c', 'file')", []).unwrap();
        c.execute("INSERT INTO judgments.review_scores (snapshot_id, path, content_hash, unit_kind) VALUES (1, 'src/b.ts', 'b5d546753d33709d', 'file')", []).unwrap();
        c.execute("INSERT INTO judgments.review_scores (snapshot_id, path, content_hash, unit_kind) VALUES (1, 'lib/c.py', '3466031b6e6cb653', 'file')", []).unwrap();
        let r = constitutionEstimate(&json!({}), &c).unwrap();
        assert_eq!(r["cached"], json!(3));
        assert_eq!(r["compute"], json!(0));
        assert_eq!(r["est_ms"], json!(40));
        assert_eq!(r["changed_note"], json!("nothing changed — this rebuild is a free refresh"));
        assert_eq!(r["reused"], json!("3 of 3 files replay from snapshot #1 (unchanged + same constitution)"));
    }
}
