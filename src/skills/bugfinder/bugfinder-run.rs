//! bugfinder-run.rs — twin of skills/bugfinder/bugfinder-run.mjs (db, cross-db judgments.*).
//! NAMES ARE CONTRACT: default `bugfinderRun`. The BUG FINDER orchestrator — three waves, cheapest
//! oracle first: WAVE 1 determinism (lint/typecheck/existence/atlas-smells, $0, counted facts) ·
//! WAVE 2 the shard swarm (one narrow AI finder per bug species per file) · WAVE 3 one mid-tier
//! sweep for the unknown unknowns. Advisory forever — this skill records, it never blocks.
//!
//! BESPOKE-NATIVE (real fs directory walk for `{folder}` scope + a real `git` subprocess for the
//! default diff scope + a multi-table judgments.* ledger write burst) — not conform-db fixture-
//! testable, same class as atlas-ingest.rs / code-git-info.rs.
//!
//! Cross-module calls (already-converted twins, reused not reimplemented): `contract_contract_get::
//! contractGet`, `engine_workspace_root::getWorkspaceRoot`, `codebase_lint_run::lintRun`,
//! `codebase_typecheck_run::typecheckRun`, `codebase_atlas_smells::atlasSmells`,
//! `bugfinder_bugfinder_shards::bugfinderShards`, `bugfinder_bugfinder_sweep::bugfinderSweep`,
//! `bugfinder__helpers_schema::ensureBugfinderSchema`.
//!
//! WAVE 1's "existence" rung is WIRED as of 2026-08-09 (bugfinder-existence-check.rs). It had been
//! left stubbed after that twin landed, so the rung silently reported 0 findings on every run — a
//! canary that always says "clean" is worse than one that says "not run". Previously deferred as:
//! (`bugfinder-existence-check.mjs` — hallucinated-
//! import/package/unknown-symbol detection via AST import-edge walking) has NO Rust twin yet (that
//! file has not itself been converted). This twin treats it exactly like a caller-requested
//! `skip:['existence']` would in the JS — an accepted-empty branch, never a panic or a short-circuit
//! of the rest of the run — and records `waves.w1.existence` with an `"omitted"` note instead of
//! running it. Every other check — lint, typecheck, atlas-smells (wave 1), the shard swarm (wave 2,
//! itself PARITY-DEFERRED at its own `aiCall` boundary — see bugfinder-shards.rs's module doc), the
//! sweep (wave 3, same), append-run bookkeeping, the pipeline/files/folder/diff scope resolvers, and
//! the judgments.bugfinder_runs/_findings ledger read/write — is ported faithfully.
//!
//! `context.exec_fn` (a JS DI hook so tests can stub git commands) has no Rust analogue (functions
//! aren't JSON args) — this twin always shells the real `git` binary, same disclosed scope as
//! lint-run.rs's / typecheck-run.rs's own `context.run_cmd`/`context.resolve_bin` omissions.
//! `context.provider_fn` is likewise a JS function reference threaded opaquely into the shard/sweep
//! AI calls — passed through as `null`, mirroring book-run.rs's `provider_fn` handling.
use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

// ── standard helpers (playbook) ─────────────────────────────────────────────────────────────
fn str_qq(v: Option<&Value>) -> String { match v { None|Some(Value::Null)=>String::new(), Some(Value::String(s))=>s.clone(), Some(Value::Bool(b))=>b.to_string(), Some(n@Value::Number(_))=>n.to_string(), Some(o)=>o.to_string() } }
fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) }
fn js_integer(v: Option<&Value>) -> Option<i64> {
    match v {
        Some(Value::Number(n)) => { let f = n.as_f64()?; if f.is_finite() && f.fract() == 0.0 { Some(f as i64) } else { None } }
        _ => None,
    }
}
fn first(rows: &Value) -> Option<Value> { rows.as_array().and_then(|a| a.first()).cloned() }

fn is_code_path(p: &str) -> bool {
    let lower = p.to_ascii_lowercase();
    for ext in ["mjs", "cjs", "js", "jsx", "ts", "tsx", "py"] {
        if lower.ends_with(&format!(".{ext}")) { return true; }
    }
    false
}
const MAX_FILES_DEFAULT: i64 = 40;
const MAX_SOURCE: usize = 400_000;
fn walk_skip() -> &'static HashSet<&'static str> {
    static S: std::sync::OnceLock<HashSet<&'static str>> = std::sync::OnceLock::new();
    S.get_or_init(|| ["node_modules", ".git", "dist", "build", "coverage", ".antiloki", ".next", "out"].into_iter().collect())
}

fn is_win_absolute(p: &str) -> bool {
    let b = p.as_bytes();
    if b.len() >= 2 && b[1] == b':' && b[0].is_ascii_alphabetic() { return true; }
    p.starts_with('\\') || p.starts_with('/')
}
fn join_root(root: &str, rel: &str) -> String {
    if is_win_absolute(rel) { return rel.to_string(); }
    Path::new(root).join(rel.replace('/', "\\")).to_string_lossy().into_owned()
}

// ultra mode: every code file under a folder (folder '.' = the whole project). Mirrors the JS
// stack-based DFS (`out.length < cap * 3` cutoff, final `.sort()`); directory entries are sorted
// by name for deterministic output (std::fs::read_dir order is not guaranteed — same disclosed
// approximation as atlas-ingest.rs's own directory-walk note).
fn walk_folder(root: &str, folder_rel: &str, cap: i64) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let start: PathBuf = if folder_rel == "." { PathBuf::from(root) } else { PathBuf::from(join_root(root, folder_rel)) };
    let mut stack: Vec<PathBuf> = vec![start];
    let cap3 = (cap.max(0) * 3) as usize;
    while let Some(dir) = stack.pop() {
        if out.len() >= cap3 { break; }
        let entries = match fs::read_dir(&dir) { Ok(e) => e, Err(_) => continue };
        let mut names: Vec<(String, PathBuf, bool)> = Vec::new();
        for e in entries.flatten() {
            let is_dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
            let name = e.file_name().to_string_lossy().into_owned();
            names.push((name, e.path(), is_dir));
        }
        names.sort_by(|a, b| a.0.cmp(&b.0));
        for (name, path, is_dir) in names {
            if is_dir {
                if !walk_skip().contains(name.as_str()) && !name.starts_with('.') { stack.push(path); }
            } else if is_code_path(&name) {
                let rel = pathdiff(root, &path);
                out.push(rel);
            }
        }
    }
    out.sort();
    out
}

// `relative(root, join(dir, name)).replace(/\\/g,'/')` — a plain prefix-strip since `path` is
// always constructed as a descendant of `root` by the walk above.
fn pathdiff(root: &str, path: &Path) -> String {
    let root_p = Path::new(root);
    match path.strip_prefix(root_p) {
        Ok(rel) => rel.to_string_lossy().replace('\\', "/"),
        Err(_) => path.to_string_lossy().replace('\\', "/"),
    }
}

fn run_git(args: &[&str], cwd: &str) -> Result<String, String> {
    match crate::proc::cmd("git").args(args).current_dir(cwd).output() {
        Ok(o) if o.status.success() => Ok(String::from_utf8_lossy(&o.stdout).into_owned()),
        Ok(o) => Err(String::from_utf8_lossy(&o.stderr).into_owned()),
        Err(e) => Err(e.to_string()),
    }
}

struct ScopeResult { paths: Vec<String>, base_sha: Option<String> }

fn resolve_scope_paths(input: &Value, root: &str) -> Result<ScopeResult, String> {
    if let Some(files) = input.get("files").and_then(Value::as_array).filter(|a| !a.is_empty()) {
        let paths = files
            .iter()
            .map(|f| match f {
                Value::String(s) => s.clone(),
                Value::Object(_) => str_qq(f.get("path")),
                _ => String::new(),
            })
            .filter(|s| !s.is_empty())
            .collect();
        return Ok(ScopeResult { paths, base_sha: None });
    }
    if let Some(folder) = input.get("folder").and_then(Value::as_str).filter(|s| !s.trim().is_empty()) {
        let cap = js_integer(input.get("max_files")).unwrap_or(MAX_FILES_DEFAULT);
        let f = folder.trim().replace('\\', "/");
        return Ok(ScopeResult { paths: walk_folder(root, &f, cap), base_sha: None });
    }
    let base = { let b = str_qq(input.get("base")); if b.is_empty() { "main".to_string() } else { b } };
    // three-dot = merge-base(base, HEAD)...HEAD — the branch's own changes only
    let diff_out = run_git(&["diff", "--name-only", &format!("{base}...HEAD")], root)
        .map_err(|e| format!("bugfinder-run: scope resolution failed ({}) — pass {{files}} explicitly", e.chars().take(160).collect::<String>()))?;
    let mut paths: Vec<String> = diff_out.split('\n').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
    let dirt = run_git(&["status", "--porcelain"], root)
        .map_err(|e| format!("bugfinder-run: scope resolution failed ({}) — pass {{files}} explicitly", e.chars().take(160).collect::<String>()))?;
    for line in dirt.split('\n') {
        if line.len() < 3 { continue; }
        let rest = line[3..].trim().trim_matches('"');
        let p = rest.split(" -> ").last().unwrap_or(rest);
        if !p.is_empty() { paths.push(p.replace('\\', "/")); }
    }
    let mut seen = HashSet::new();
    let deduped: Vec<String> = paths.into_iter().filter(|p| seen.insert(p.clone())).collect();
    let base_sha = run_git(&["merge-base", &base, "HEAD"], root).ok().map(|s| s.trim().to_string());
    Ok(ScopeResult { paths: deduped, base_sha })
}

fn sev_from_smell(s: Option<&str>) -> &'static str {
    match s { Some("high") => "high", Some("medium") => "medium", Some("low") => "low", _ => "medium" }
}

fn shard_names() -> [&'static str; 6] { ["checking", "algorithm", "timing", "interface", "security", "test-oracle"] }

/// `export default async function bugfinderRun(input, context)`.
pub fn bugfinderRun(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS `const db = context?.db; if (!db) throw err.required(...)` — unreachable here
    // (`db: &Connection` is never absent), same convention as every sibling db-lane twin.
    crate::bugfinder__helpers_schema::ensureBugfinderSchema(db);

    let mut input = input.clone();

    // ── pipeline mode: the contract's build env decides root + base ───────────────────────────
    let contract_slug = input.get("contract_slug").and_then(Value::as_str).filter(|s| !s.is_empty());
    let files_is_array = matches!(input.get("files"), Some(Value::Array(_)));
    let folder_is_str = input.get("folder").and_then(Value::as_str).is_some();
    if let Some(slug) = contract_slug.filter(|_| !files_is_array && !folder_is_str) {
        let slug = slug.to_string();
        let contract = crate::contract_contract_get::contractGet(&json!(slug), db)?;
        if contract.is_null() {
            return Err(format!("bugfinder-run: contract '{slug}' not found"));
        }
        let env = contract.get("worktree_path").and_then(Value::as_str).filter(|s| !s.is_empty())
            .or_else(|| contract.get("sandbox_path").and_then(Value::as_str).filter(|s| !s.is_empty()));
        let env_exists = env.map(|e| Path::new(e).exists()).unwrap_or(false);
        if !env_exists {
            let scope_json = json!({ "files": [] }).to_string();
            let totals_json = json!({
                "kept": 0, "dropped": 0, "by_wave": {}, "by_category": {},
                "note": "no_build_env — scope unjudgeable, run skipped (r37 rule)",
            }).to_string();
            let source = { let s = str_qq(input.get("source")); if s.is_empty() { "pipeline".to_string() } else { s } };
            let (_, run_id) = crate::db_json::run_write(
                db,
                "INSERT INTO judgments.bugfinder_runs (source, contract_slug, scope_json, status, totals_json) VALUES (?, ?, ?, 'done', ?)",
                &[json!(source), json!(slug), json!(scope_json), json!(totals_json)],
            )?;
            return Ok(json!({
                "run_id": run_id, "status": "done",
                "counts": { "kept": 0, "dropped": 0, "by_wave": {}, "by_category": {} },
                "findings": [], "dropped": 0, "cost_usd": 0, "capped": 0, "skipped_files": [],
                "waves": { "note": "no_build_env" },
                "advisory": "no build env for this contract — nothing was judged (never the wrong repo)",
            }));
        }
        let env = env.unwrap().to_string();
        let base_sha_field = contract.get("base_sha").and_then(Value::as_str).filter(|s| !s.is_empty());
        let base = base_sha_field.unwrap_or("HEAD~1").to_string();
        let source = { let s = str_qq(input.get("source")); if s.is_empty() { "pipeline".to_string() } else { s } };
        let mut o = input.as_object().cloned().unwrap_or_default();
        o.insert("root".into(), json!(env));
        o.insert("base".into(), json!(base));
        o.insert("source".into(), json!(source));
        input = Value::Object(o);
    }

    let root = match input.get("root").and_then(Value::as_str).filter(|s| !s.is_empty()) {
        Some(r) => r.to_string(),
        None => str_qq(Some(&crate::engine_workspace_root::getWorkspaceRoot(db))),
    };
    let skip: HashSet<String> = input.get("skip").and_then(Value::as_array).cloned().unwrap_or_default()
        .into_iter().map(|v| str_qq(Some(&v))).collect();
    let max_files = js_integer(input.get("max_files")).unwrap_or(MAX_FILES_DEFAULT);

    // ── scope ──────────────────────────────────────────────────────────────────────────────────
    let scope = resolve_scope_paths(&input, &root)?;
    let mut seen = HashSet::new();
    let code_paths: Vec<String> = scope.paths.into_iter().filter(|p| is_code_path(p) && seen.insert(p.clone())).collect();
    let capped = (code_paths.len() as i64 - max_files).max(0);
    let mut skipped_files: Vec<Value> = Vec::new();
    let mut scoped: Vec<(String, String)> = Vec::new();
    let take_n = if max_files < 0 { 0 } else { max_files as usize };
    for p in code_paths.into_iter().take(take_n) {
        let abs = if is_win_absolute(&p) { p.clone() } else { join_root(&root, &p) };
        if !Path::new(&abs).exists() { skipped_files.push(json!({ "path": p, "reason": "missing" })); continue; }
        let source = match fs::read_to_string(&abs) { Ok(s) => s, Err(_) => { skipped_files.push(json!({ "path": p, "reason": "unreadable" })); continue; } };
        if source.len() > MAX_SOURCE { skipped_files.push(json!({ "path": p, "reason": "too-large" })); continue; }
        scoped.push((p, source));
    }

    let head_sha: Value = match run_git(&["rev-parse", "HEAD"], &root) { Ok(s) => json!(s.trim()), Err(_) => Value::Null };

    // ── the run row ────────────────────────────────────────────────────────────────────────────
    let append_id = js_integer(input.get("run_id"));
    let existing_run: Option<Value> = match append_id {
        Some(id) => first(&crate::db_json::query_rows(db, "SELECT id, cost_usd FROM judgments.bugfinder_runs WHERE id = ?", &[json!(id)])?),
        None => None,
    };
    let run_id: i64;
    if let Some(ref er) = existing_run {
        run_id = er.get("id").and_then(Value::as_i64).unwrap_or(0);
        crate::db_json::run_write(db, "UPDATE judgments.bugfinder_runs SET status='running' WHERE id=?", &[json!(run_id)])?;
    } else {
        let source = { let s = str_qq(input.get("source")); if s.is_empty() { "manual".to_string() } else { s } };
        let session_id = nz(input.get("session_id")).map(|v| json!(str_qq(Some(v)))).unwrap_or(Value::Null);
        let contract_slug_v = nz(input.get("contract_slug")).cloned().unwrap_or(Value::Null);
        let base_sha_v = scope.base_sha.map(|s| json!(s)).unwrap_or(Value::Null);
        let scope_json = json!({
            "files": scoped.iter().map(|(p, _)| p.clone()).collect::<Vec<_>>(),
            "base": nz(input.get("base")).cloned().unwrap_or(Value::Null),
            "capped": capped,
        }).to_string();
        let (_, rid) = crate::db_json::run_write(
            db,
            "INSERT INTO judgments.bugfinder_runs (source, session_id, contract_slug, base_sha, head_sha, scope_json, status) VALUES (?, ?, ?, ?, ?, ?, 'running')",
            &[json!(source), session_id, contract_slug_v, base_sha_v, head_sha.clone(), json!(scope_json)],
        )?;
        run_id = rid;
    }

    let persist = |db: &Connection, wave: i64, f: &Value, verified: i64, dropped: i64, drop_reason: Option<&Value>| -> Result<i64, String> {
        let (_, id) = crate::db_json::run_write(
            db,
            "INSERT INTO judgments.bugfinder_findings (run_id, wave, category, severity, file, line, detail, evidence, model, verified, dropped, drop_reason) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            &[
                json!(run_id), json!(wave),
                f.get("category").cloned().unwrap_or(Value::Null),
                nz(f.get("severity")).cloned().unwrap_or(json!("medium")),
                f.get("file").cloned().unwrap_or(Value::Null),
                f.get("line").cloned().unwrap_or(Value::Null),
                json!(str_qq(f.get("detail")).chars().take(500).collect::<String>()),
                nz(f.get("evidence")).map(|v| json!(str_qq(Some(v)).chars().take(400).collect::<String>())).unwrap_or(Value::Null),
                nz(f.get("model")).cloned().unwrap_or(Value::Null),
                json!(verified), json!(dropped),
                drop_reason.cloned().unwrap_or(Value::Null),
            ],
        )?;
        Ok(id)
    };

    // JS `const waves = { w1: { errors: [] }, w2: null, w3: null };` — all three keys exist from the
    // start (gotcha #1/#3: object key order + a real `null` vs an omitted key), each wave's block
    // below reassigns its own key in place (Map::insert on an existing key keeps its position,
    // mirroring a JS property reassignment) once/if that wave actually runs.
    let mut waves = Map::new();
    waves.insert("w1".into(), json!({ "errors": [] }));
    waves.insert("w2".into(), Value::Null);
    waves.insert("w3".into(), Value::Null);
    let mut w1_errors: Vec<Value> = Vec::new();
    let mut kept: Vec<Value> = Vec::new();
    let mut dropped_count: i64 = 0;
    let mut cost: f64 = 0.0;

    let inline_result: Result<(), String> = (|| {
        // ── WAVE 1 · determinism ($0) — counted facts ───────────────────────────────────────
        let file_args: Vec<Value> = scoped.iter().map(|(p, _)| json!({ "path": p })).collect();
        if !skip.contains("lint") && !file_args.is_empty() {
            match crate::codebase_lint_run::lintRun(&json!({ "files": file_args }), db) {
                Ok(r) => {
                    let findings = r.get("findings").and_then(Value::as_array).cloned().unwrap_or_default();
                    for x in &findings {
                        let code = nz(x.get("code")).map(|v| str_qq(Some(v))).unwrap_or_else(|| "lint".to_string());
                        let detail = format!("{code}: {}", str_qq(x.get("message")));
                        let f = json!({ "category": "lint", "severity": "low", "file": x.get("file").cloned().unwrap_or(Value::Null), "line": x.get("line").cloned().unwrap_or(Value::Null), "detail": detail, "evidence": Value::Null });
                        let id = persist(db, 1, &f, 1, 0, None)?;
                        let mut o = f.as_object().cloned().unwrap_or_default();
                        o.insert("id".into(), json!(id)); o.insert("wave".into(), json!(1));
                        kept.push(Value::Object(o));
                    }
                    let mut w1obj = waves.get("w1").cloned().unwrap_or(json!({})).as_object().cloned().unwrap_or_default();
                    w1obj.insert("lint".into(), json!({ "findings": findings.len() as i64, "ran": r.get("ran").cloned().unwrap_or(Value::Null) }));
                    waves.insert("w1".into(), Value::Object(w1obj));
                }
                Err(e) => w1_errors.push(json!({ "tool": "lint", "error": e.chars().take(200).collect::<String>() })),
            }
        }
        if !skip.contains("typecheck") && !file_args.is_empty() {
            match crate::codebase_typecheck_run::typecheckRun(&json!({ "files": file_args }), db) {
                Ok(r) => {
                    let findings = r.get("findings").and_then(Value::as_array).cloned().unwrap_or_default();
                    for x in &findings {
                        let code = nz(x.get("code")).map(|v| str_qq(Some(v))).unwrap_or_else(|| "type".to_string());
                        let detail = format!("{code}: {}", str_qq(x.get("message")));
                        let f = json!({ "category": "typecheck", "severity": "high", "file": x.get("file").cloned().unwrap_or(Value::Null), "line": x.get("line").cloned().unwrap_or(Value::Null), "detail": detail, "evidence": Value::Null });
                        let id = persist(db, 1, &f, 1, 0, None)?;
                        let mut o = f.as_object().cloned().unwrap_or_default();
                        o.insert("id".into(), json!(id)); o.insert("wave".into(), json!(1));
                        kept.push(Value::Object(o));
                    }
                    let mut w1obj = waves.get("w1").cloned().unwrap_or(json!({})).as_object().cloned().unwrap_or_default();
                    w1obj.insert("typecheck".into(), json!({ "findings": findings.len() as i64 }));
                    waves.insert("w1".into(), Value::Object(w1obj));
                }
                Err(e) => w1_errors.push(json!({ "tool": "typecheck", "error": e.chars().take(200).collect::<String>() })),
            }
        }
        // existence: the hallucinated-symbol rung. The twin DOES exist
        // (bugfinder-existence-check.rs); this site was left stubbed after it landed, so the
        // rung silently reported 0 findings on every run — a canary that always says "clean" is
        // worse than one that says "not run".
        if !skip.contains("existence") && !scoped.is_empty() {
            let files: Vec<Value> = scoped.iter().map(|(p, _)| json!(p.replace('\\', "/"))).collect();
            let mut w1obj = waves.get("w1").cloned().unwrap_or(json!({})).as_object().cloned().unwrap_or_default();
            match crate::bugfinder_bugfinder_existence_check::bugfinderExistenceCheck(
                &json!({ "files": files }),
                db,
            ) {
                Ok(res) => {
                    let findings = res.get("findings").and_then(Value::as_array).cloned().unwrap_or_default();
                    for f in &findings {
                        let id = persist(db, 1, f, 1, 0, None)?;
                        let mut o = f.as_object().cloned().unwrap_or_default();
                        o.insert("id".into(), json!(id));
                        o.insert("wave".into(), json!(1));
                        kept.push(Value::Object(o));
                    }
                    w1obj.insert(
                        "existence".into(),
                        json!({
                            "findings": findings.len() as i64,
                            "checked": res.get("checked").cloned().unwrap_or(json!(0)),
                        }),
                    );
                }
                // Same contract as the sibling rungs: a tool failure is recorded and never blocks
                // the rest of the wave.
                Err(e) => w1_errors.push(json!({ "tool": "existence", "error": e.chars().take(200).collect::<String>() })),
            }
            waves.insert("w1".into(), Value::Object(w1obj));
        }
        if !skip.contains("smells") && !scoped.is_empty() {
            let scope_set: HashSet<String> = scoped.iter().map(|(p, _)| p.replace('\\', "/")).collect();
            match crate::codebase_atlas_smells::atlasSmells(&json!({ "origin": "atlas" }), db) {
                Ok(r) => {
                    let mut n: i64 = 0;
                    for s in r.get("smells").and_then(Value::as_array).cloned().unwrap_or_default() {
                        let nodes: Vec<String> = s.get("nodes").and_then(Value::as_array).cloned().unwrap_or_default()
                            .into_iter()
                            .filter_map(|x| match x { Value::String(p) => Some(p), Value::Object(_) => x.get("path").and_then(Value::as_str).map(|s| s.to_string()), _ => None })
                            .map(|p| p.replace('\\', "/"))
                            .collect();
                        let hit = nodes.iter().find(|p| scope_set.contains(p.as_str()));
                        let Some(hit) = hit else { continue };
                        let detail = nz(s.get("detail")).map(|v| str_qq(Some(v))).unwrap_or_else(|| str_qq(s.get("kind")));
                        let f = json!({
                            "category": format!("structure:{}", str_qq(s.get("kind"))),
                            "severity": sev_from_smell(s.get("severity").and_then(Value::as_str)),
                            "file": hit, "line": Value::Null,
                            "detail": detail.chars().take(500).collect::<String>(),
                            "evidence": nodes.iter().take(8).cloned().collect::<Vec<_>>().join(" · "),
                        });
                        let id = persist(db, 1, &f, 1, 0, None)?;
                        let mut o = f.as_object().cloned().unwrap_or_default();
                        o.insert("id".into(), json!(id)); o.insert("wave".into(), json!(1));
                        kept.push(Value::Object(o));
                        n += 1;
                    }
                    let mut w1obj = waves.get("w1").cloned().unwrap_or(json!({})).as_object().cloned().unwrap_or_default();
                    w1obj.insert("smells".into(), json!({ "findings": n }));
                    waves.insert("w1".into(), Value::Object(w1obj));
                }
                Err(e) => w1_errors.push(json!({ "tool": "smells", "error": e.chars().take(200).collect::<String>() })),
            }
        }
        {
            let mut w1obj = waves.get("w1").cloned().unwrap_or(json!({})).as_object().cloned().unwrap_or_default();
            w1obj.insert("errors".into(), Value::Array(w1_errors.clone()));
            waves.insert("w1".into(), Value::Object(w1obj));
        }

        // ── WAVE 2 · the shard swarm (low tier) — grounded leads only ───────────────────────
        if !skip.contains("shards") && !scoped.is_empty() {
            let files_v: Vec<Value> = scoped.iter().map(|(p, s)| json!({ "path": p, "source": s })).collect();
            let r = crate::bugfinder_bugfinder_shards::bugfinderShards(&json!({ "files": files_v, "shards": input.get("shards").cloned().unwrap_or(Value::Null) }), db)?;
            let r_kept = r.get("kept").and_then(Value::as_array).cloned().unwrap_or_default();
            let r_dropped = r.get("dropped").and_then(Value::as_array).cloned().unwrap_or_default();
            for x in &r_kept {
                let id = persist(db, 2, x, 0, 0, None)?;
                let mut o = x.as_object().cloned().unwrap_or_default();
                o.insert("id".into(), json!(id)); o.insert("wave".into(), json!(2));
                kept.push(Value::Object(o));
            }
            for x in &r_dropped {
                persist(db, 2, x, 0, 1, x.get("drop_reason"))?;
                dropped_count += 1;
            }
            cost += r.get("cost_usd").and_then(Value::as_f64).unwrap_or(0.0);
            waves.insert("w2".into(), json!({
                "shards": r.get("shards").cloned().unwrap_or(Value::Null),
                "calls": r.get("calls").cloned().unwrap_or(Value::Null),
                "kept": r_kept.len() as i64, "dropped": r_dropped.len() as i64,
                "cost_usd": r.get("cost_usd").cloned().unwrap_or(json!(0)),
                "errors": r.get("errors").cloned().unwrap_or(json!([])),
            }));
        }

        // ── WAVE 3 · one mid sweep — the unknown unknowns ────────────────────────────────────
        if !skip.contains("sweep") && !scoped.is_empty() {
            let mut covered: Vec<Value> = shard_names().iter().map(|s| json!(*s)).collect();
            for c in ["lint", "typecheck", "hallucinated-import", "hallucinated-package", "unknown-symbol"] { covered.push(json!(c)); }
            let known_rows = crate::db_json::query_rows(
                db,
                "SELECT file, line, category, detail FROM judgments.bugfinder_findings WHERE run_id = ? AND dropped = 0 AND wave < 3 ORDER BY id LIMIT 80",
                &[json!(run_id)],
            )?;
            let known: Vec<Value> = known_rows.as_array().cloned().unwrap_or_default().into_iter().map(|f| {
                let file = nz(f.get("file")).map(|v| str_qq(Some(v))).unwrap_or_else(|| "?".to_string());
                let line = nz(f.get("line")).map(|v| str_qq(Some(v))).unwrap_or_else(|| "?".to_string());
                let category = str_qq(f.get("category"));
                let detail: String = str_qq(f.get("detail")).chars().take(100).collect();
                json!(format!("{file}:{line} {category} — {detail}"))
            }).collect();
            let files_v: Vec<Value> = scoped.iter().map(|(p, s)| json!({ "path": p, "source": s })).collect();
            let r = crate::bugfinder_bugfinder_sweep::bugfinderSweep(&json!({ "files": files_v, "covered": covered, "known": known }), db)?;
            let r_kept = r.get("kept").and_then(Value::as_array).cloned().unwrap_or_default();
            let r_dropped = r.get("dropped").and_then(Value::as_array).cloned().unwrap_or_default();
            for x in &r_kept {
                let id = persist(db, 3, x, 0, 0, None)?;
                let mut o = x.as_object().cloned().unwrap_or_default();
                o.insert("id".into(), json!(id)); o.insert("wave".into(), json!(3));
                kept.push(Value::Object(o));
            }
            for x in &r_dropped {
                persist(db, 3, x, 0, 1, x.get("drop_reason"))?;
                dropped_count += 1;
            }
            cost += r.get("cost_usd").and_then(Value::as_f64).unwrap_or(0.0);
            waves.insert("w3".into(), json!({
                "calls": r.get("calls").cloned().unwrap_or(Value::Null),
                "kept": r_kept.len() as i64, "dropped": r_dropped.len() as i64,
                "cost_usd": r.get("cost_usd").cloned().unwrap_or(json!(0)),
                "errors": r.get("errors").cloned().unwrap_or(json!([])),
            }));
        }
        Ok(())
    })();

    if let Err(e) = inline_result {
        let partial = existing_run.as_ref().and_then(|er| er.get("cost_usd")).and_then(Value::as_f64).unwrap_or(0.0) + cost;
        let _ = crate::db_json::run_write(
            db,
            "UPDATE judgments.bugfinder_runs SET status='error', error=?, cost_usd=? WHERE id=?",
            &[json!(e.chars().take(500).collect::<String>()), json!(partial), json!(run_id)],
        );
        return Err(e);
    }

    // totals are recomputed from the WHOLE run (all waves that ever wrote to it).
    let run_kept = crate::db_json::query_rows(db, "SELECT wave, category FROM judgments.bugfinder_findings WHERE run_id = ? AND dropped = 0", &[json!(run_id)])?;
    let run_kept_arr = run_kept.as_array().cloned().unwrap_or_default();
    let run_dropped = first(&crate::db_json::query_rows(db, "SELECT COUNT(*) AS n FROM judgments.bugfinder_findings WHERE run_id = ? AND dropped = 1", &[json!(run_id)])?)
        .and_then(|r| r.get("n").and_then(Value::as_i64)).unwrap_or(0);
    let mut by_wave = Map::new();
    let mut by_category = Map::new();
    for f in &run_kept_arr {
        let w = str_qq(f.get("wave"));
        let c = str_qq(f.get("category"));
        let wv = by_wave.get(&w).and_then(Value::as_i64).unwrap_or(0) + 1;
        by_wave.insert(w, json!(wv));
        let cv = by_category.get(&c).and_then(Value::as_i64).unwrap_or(0) + 1;
        by_category.insert(c, json!(cv));
    }
    let totals = json!({ "kept": run_kept_arr.len() as i64, "dropped": run_dropped, "by_wave": by_wave, "by_category": by_category });
    let total_cost = existing_run.as_ref().and_then(|er| er.get("cost_usd")).and_then(Value::as_f64).unwrap_or(0.0) + cost;

    let waves_val = Value::Object(waves.clone());
    let merged_waves: Value = if let Some(_) = existing_run {
        let prev_json = first(&crate::db_json::query_rows(db, "SELECT waves_json FROM judgments.bugfinder_runs WHERE id = ?", &[json!(run_id)])?)
            .and_then(|r| r.get("waves_json").and_then(Value::as_str).map(|s| s.to_string()));
        let prev: Value = prev_json.and_then(|s| serde_json::from_str(&s).ok()).unwrap_or(json!({}));
        let mut merged = prev.as_object().cloned().unwrap_or_default();
        // JS `Object.entries(waves).filter(([, v]) => v && (Array.isArray(v.errors) ? true :
        // Object.keys(v).length))` — every non-null `v` this skill ever builds carries its own
        // `errors: []` array (w1's baseline, w2's/w3's summary object), so that predicate reduces
        // to "v is not null": w2/w3 stay `null` (excluded, `prev`'s value from an earlier
        // wave-pane wins) unless THIS call actually ran that wave.
        for (k, v) in waves.iter() {
            if !v.is_null() { merged.insert(k.clone(), v.clone()); }
        }
        Value::Object(merged)
    } else {
        waves_val.clone()
    };

    crate::db_json::run_write(
        db,
        "UPDATE judgments.bugfinder_runs SET status='done', waves_json=?, totals_json=?, cost_usd=? WHERE id=?",
        &[json!(serde_json::to_string(&merged_waves).map_err(|e| e.to_string())?), json!(serde_json::to_string(&totals).map_err(|e| e.to_string())?), json!(total_cost), json!(run_id)],
    )?;

    let wave_kept = kept.len() as i64;
    Ok(json!({
        "run_id": run_id, "status": "done", "appended": existing_run.is_some(),
        "counts": totals, "findings": kept.into_iter().take(200).collect::<Vec<_>>(),
        "wave_kept": wave_kept, "dropped": dropped_count,
        "cost_usd": total_cost, "wave_cost_usd": cost, "capped": capped,
        "skipped_files": skipped_files, "waves": merged_waves,
        "advisory": "the bug finder records — it never blocks (promotion path: the classification ledger)",
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS judgments; ATTACH ':memory:' AS brain;").unwrap();
        c.execute_batch("CREATE TABLE contracts (slug TEXT, worktree_path TEXT, sandbox_path TEXT, base_sha TEXT, files_touched_json TEXT, acceptance_json TEXT, naming_json TEXT, edge_cases_json TEXT, must_obey_json TEXT, out_of_scope_json TEXT, reference_artifacts_json TEXT, generated_paths_json TEXT);
             CREATE TABLE workspaces (id INTEGER PRIMARY KEY, name TEXT, root_path TEXT, vcs TEXT, is_active INTEGER, created_at TEXT);
             CREATE TABLE brain.prompt_templates ( id INTEGER PRIMARY KEY, name TEXT NOT NULL, version INTEGER NOT NULL, template_md TEXT NOT NULL, default_tier TEXT NOT NULL, default_temperature TEXT NOT NULL, max_output_tokens INTEGER NOT NULL DEFAULT 1024, superseded_by INTEGER, created_at TEXT NOT NULL DEFAULT (datetime('now')), cross_field_rules_json TEXT, judging_criteria_json TEXT, system_md TEXT, phase_flow TEXT NOT NULL DEFAULT 'single', UNIQUE (name, version) );
             CREATE TABLE brain.prompt_output_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, grounded_refs_json TEXT, UNIQUE (prompt_template_id, field_name) );
             CREATE TABLE brain.prompt_input_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, UNIQUE (prompt_template_id, field_name) );
             CREATE TABLE judgments.bugfinder_runs ( id INTEGER PRIMARY KEY, source TEXT, session_id TEXT, contract_slug TEXT, base_sha TEXT, head_sha TEXT, scope_json TEXT, status TEXT, error TEXT, waves_json TEXT, totals_json TEXT, cost_usd REAL DEFAULT 0, created_at TEXT NOT NULL DEFAULT (datetime('now')) );
             CREATE TABLE judgments.bugfinder_findings ( id INTEGER PRIMARY KEY, run_id INTEGER NOT NULL, wave INTEGER, category TEXT, severity TEXT, file TEXT, line INTEGER, detail TEXT, evidence TEXT, model TEXT, verified INTEGER, dropped INTEGER DEFAULT 0, drop_reason TEXT );").unwrap();
        c
    }

    #[test]
    fn empty_files_scope_produces_a_done_run_with_no_findings() {
        let c = setup();
        let r = bugfinderRun(&json!({ "files": ["notreal.mjs"], "root": std::env::temp_dir().to_string_lossy() }), &c).unwrap();
        assert_eq!(r["status"], json!("done"));
        assert_eq!(r["findings"], json!([]));
        assert_eq!(r["skipped_files"][0]["reason"], json!("missing"));
    }

    #[test]
    fn scoped_real_file_runs_lint_typecheck_smells_without_crashing() {
        let c = setup();
        let dir = std::env::temp_dir().join(format!("bugfinder-run-rs-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("sample.mjs");
        std::fs::write(&file, "export const x = 1;\n").unwrap();
        let r = bugfinderRun(&json!({ "files": ["sample.mjs"], "root": dir.to_string_lossy(), "skip": ["shards", "sweep"] }), &c).unwrap();
        assert_eq!(r["status"], json!("done"));
        // the existence rung is wired now: it reports a real `checked` count, not an omission note
        assert!(r["waves"]["w1"]["existence"]["findings"].is_number(), "{}", r["waves"]["w1"]);
        assert!(r["waves"]["w1"]["existence"].get("note").is_none(), "no longer an omission");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn contract_mode_with_no_build_env_records_the_r37_reason() {
        let c = setup();
        c.execute("INSERT INTO contracts (slug) VALUES ('ghost-contract')", []).unwrap();
        let r = bugfinderRun(&json!({ "contract_slug": "ghost-contract" }), &c).unwrap();
        assert_eq!(r["status"], json!("done"));
        assert_eq!(r["waves"]["note"], json!("no_build_env"));
        assert!(r["advisory"].as_str().unwrap().contains("no build env"));
    }

    #[test]
    fn unknown_contract_errors() {
        let c = setup();
        let e = bugfinderRun(&json!({ "contract_slug": "ghost" }), &c).unwrap_err();
        assert!(e.contains("not found"));
    }

    #[test]
    fn append_mode_reuses_run_id_and_accumulates_cost() {
        let c = setup();
        c.execute("INSERT INTO judgments.bugfinder_runs (source, status, cost_usd) VALUES ('manual', 'running', 1.5)", []).unwrap();
        let rid = c.last_insert_rowid();
        let r = bugfinderRun(&json!({ "files": ["notreal.mjs"], "root": std::env::temp_dir().to_string_lossy(), "run_id": rid }), &c).unwrap();
        assert_eq!(r["run_id"], json!(rid));
        assert_eq!(r["appended"], json!(true));
        assert_eq!(r["cost_usd"], json!(1.5));
    }

    #[test]
    fn walk_folder_finds_code_files_recursively() {
        let dir = std::env::temp_dir().join(format!("bugfinder-run-rs-walk-{}", std::process::id()));
        let sub = dir.join("nested");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(dir.join("a.ts"), "").unwrap();
        std::fs::write(dir.join("skip.md"), "").unwrap();
        std::fs::write(sub.join("b.py"), "").unwrap();
        std::fs::create_dir_all(dir.join("node_modules")).unwrap();
        std::fs::write(dir.join("node_modules").join("c.js"), "").unwrap();
        let found = walk_folder(&dir.to_string_lossy(), ".", 40);
        assert_eq!(found, vec!["a.ts", "nested/b.py"]);
        std::fs::remove_dir_all(&dir).ok();
    }
}
