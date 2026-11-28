//! pr-test-run.rs — twin of skills/cockpit/pr-test-run.mjs (db WRITE lane). NAMES ARE CONTRACT:
//! `prTestRun` (default). The PR as an ISOLATED TEST ENVIRONMENT: `op:'run'` runs env/deps/build/
//! test checks + one row per acceptance item against the contract's worktree, persisted to
//! `pr_test_runs`; `op:'confirm'` overlays a human verdict onto one check; `op:'latest'` returns
//! the newest run; `op:'suggest'` drafts unit tests via ONE `aiCall`; `op:'gate-build'`/
//! `op:'gate-test'` delegate to the already-converted `devBuildGate`/`devTestGate` twins;
//! `op:'file'`/`op:'tree'`/`op:'diff'`/`op:'edit'`/`op:'regenerate'` are the PR review IDE's
//! worktree read/write/diff/regenerate ops, delegating to already-converted twins
//! (`codebaseFileRead`, `fileWrite`, `codeRegenerate`) or real `git`/`fs` calls.
//!
//! PARITY-DEFERRED: `op:'suggest'` ends in `crate::ai_ai_call::aiCall` (the deferred network-AI
//! stub — see ai-call.rs's module doc), so it always propagates `Err` once the real, ported
//! surface above it (prompt ensure/bootstrap via the already-converted `ai_prompt_find`/
//! `ai_prompt_add` twins, package.json stack sniff, input_kv assembly) has run — mirrors
//! book-run.rs's `fac_call` pattern. Everything else in this file is REAL: spawned `npm`/`git`
//! child processes (`sh`, mirroring the JS `spawnSync(cmd, args, {shell: win32})`), real
//! filesystem walks (`grep_worktree`, the worktree tree listing), and real `pr_test_runs` /
//! `contracts` reads/writes.
//!
//! `context.db` missing → JS throws `err.contextMissing('pr-test-run', 'db')`. Unreachable here:
//! `db: &Connection` is a required, non-nullable reference, same convention as every sibling
//! db-lane twin. `context.provider_fn` (a JS function reference for provider selection/DI) has no
//! Rust equivalent to carry — threaded as `null` into the constructed `aiCall` input, same as
//! book-run.rs's `provider_fn`. `new Date().toISOString()` (op:'confirm' `human_ts`) is the
//! frozen-clock literal `"2026-01-01T00:00:00.000Z"` (playbook convention, e.g.
//! cockpit-script-run.rs's `started_at`).
// QUALITY: mixed unit — deterministic op-guard/summary/grep/tree surface (fully ported,
// fixture-reachable) + real spawn/fs/git side effects (disclosed, not fixture-exercised —
// same convention as cockpit-script-run.rs) + 1 aiCall dispatch (deferred, no fixture).

use rusqlite::Connection;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

// ── standard helpers (playbook) ─────────────────────────────────────────────────────────────
fn str_qq(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(),
        Some(o) => o.to_string(),
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
fn first(rows: &Value) -> Option<Value> { rows.as_array().and_then(|a| a.first()).cloned() }

// `JSON.parse(String(s ?? '[]'))`, Array.isArray guard, `[]` on any failure.
fn parse_arr(v: Option<&str>) -> Vec<Value> {
    match v.and_then(|s| serde_json::from_str::<Value>(s).ok()) {
        Some(Value::Array(a)) => a,
        _ => vec![],
    }
}
// `JSON.parse(String(s ?? '{}'))`, typeof === 'object' guard, `{}` on any failure.
fn parse_obj(v: Option<&str>) -> Value {
    match v.and_then(|s| serde_json::from_str::<Value>(s).ok()) {
        Some(o @ Value::Object(_)) => o,
        _ => json!({}),
    }
}

struct ShResult { status: Option<i32>, stdout: String, stderr: String }

// `spawnSync(cmd, args, { cwd, encoding: 'utf8', timeout, shell: process.platform === 'win32',
// env: { ...process.env, CI: '1' } })`. On Windows, shell:true routes through cmd.exe (same
// platform-branch convention as cockpit-script-run.rs's `spawn_script`).
fn sh(cmd: &str, args: &[&str], cwd: &str) -> ShResult {
    let mut command = if cfg!(windows) {
        let mut c = crate::proc::cmd("cmd");
        let joined = std::iter::once(cmd).chain(args.iter().copied()).collect::<Vec<_>>().join(" ");
        c.args(["/C", &joined]);
        c
    } else {
        let mut c = crate::proc::cmd(cmd);
        c.args(args);
        c
    };
    command.current_dir(cwd).env("CI", "1");
    match command.output() {
        Ok(o) => ShResult {
            status: o.status.code(),
            stdout: String::from_utf8_lossy(&o.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&o.stderr).into_owned(),
        },
        Err(e) => ShResult { status: None, stdout: String::new(), stderr: e.to_string() },
    }
}
// `const tail = (r, n = 900) => { const t = ...; return t.length > n ? '…' + t.slice(-n) : t || '(no output)'; }`
fn tail(r: &ShResult, n: usize) -> String {
    let t = format!("{}\n{}", r.stdout, r.stderr);
    let t = t.trim();
    let chars: Vec<char> = t.chars().collect();
    if chars.len() > n {
        format!("…{}", chars[chars.len() - n..].iter().collect::<String>())
    } else if t.is_empty() {
        "(no output)".to_string()
    } else {
        t.to_string()
    }
}

const GREP_EXTS: &[&str] = &[".js", ".jsx", ".ts", ".tsx", ".mjs", ".css", ".html", ".json", ".py"];

// bounded source scan for evidence tokens: src-ish files, ≤300 files, ≤200KB each, depth ≤6.
fn grep_worktree(root: &str, tokens: &[String]) -> HashMap<String, Vec<String>> {
    let mut hits: HashMap<String, Vec<String>> = tokens.iter().map(|t| (t.clone(), Vec::new())).collect();
    let mut files: Vec<PathBuf> = Vec::new();
    fn walk(dir: &Path, depth: u32, files: &mut Vec<PathBuf>) {
        if depth > 6 || files.len() >= 300 { return; }
        let entries = match std::fs::read_dir(dir) { Ok(e) => e, Err(_) => return };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name == "node_modules" || name.starts_with('.') { continue; }
            let p = entry.path();
            let meta = match entry.metadata() { Ok(m) => m, Err(_) => continue };
            if meta.is_dir() {
                walk(&p, depth + 1, files);
            } else {
                let ext_ok = GREP_EXTS.iter().any(|ext| name.ends_with(ext));
                if ext_ok && meta.len() <= 200_000 { files.push(p); }
            }
            if files.len() >= 300 { return; }
        }
    }
    walk(Path::new(root), 0, &mut files);
    let root_len = root.len();
    for f in &files {
        let txt = match std::fs::read_to_string(f) { Ok(t) => t, Err(_) => continue };
        for t in tokens {
            let bucket = hits.get_mut(t).unwrap();
            if bucket.len() < 3 && txt.contains(t.as_str()) {
                let line = txt.split('\n').position(|l| l.contains(t.as_str())).map(|i| i + 1).unwrap_or(0);
                let rel = f.to_string_lossy().into_owned();
                let rel = if rel.len() > root_len { rel[root_len + 1..].to_string() } else { rel };
                bucket.push(format!("{rel}:{line}"));
            }
        }
    }
    hits
}

fn summarize(checks: &[Value]) -> Value {
    let eff = |c: &Value| -> String {
        let h = c.get("human_verdict").and_then(Value::as_str);
        h.map(str::to_string).unwrap_or_else(|| c.get("status").and_then(Value::as_str).unwrap_or("").to_string())
    };
    let count = |s: &str| checks.iter().filter(|c| eff(c) == s).count();
    json!({
        "pass": count("pass"), "fail": count("fail"),
        "needs_human": count("needs_human"), "skip": count("skip"),
        "total": checks.len(),
    })
}

pub fn prTestRun(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS: `const { db, provider_fn = null } = context ?? {}; if (!db) throw err.contextMissing(...)`
    // — unreachable here (db: &Connection is never absent), same convention as every sibling twin.
    let provider_fn = Value::Null;
    let op = { let o = str_qq(input.get("op")); if o.is_empty() { "run".to_string() } else { o } };

    if op == "confirm" {
        let run_id = js_number(input.get("run_id")) as i64;
        let rows = crate::db_json::query_rows(db, "SELECT id, checks_json FROM pr_test_runs WHERE id = ?", &[json!(run_id)])?;
        let Some(run) = first(&rows) else {
            return Ok(json!({ "ok": true, "skipped": true, "reason": "run not found" }));
        };
        let verdict = str_qq(input.get("verdict"));
        if verdict != "pass" && verdict != "fail" {
            return Err(format!("pr-test-run: verdict must be one of: pass, fail (got '{verdict}')"));
        }
        let checks_json = run.get("checks_json").and_then(Value::as_str);
        let mut checks = parse_arr(checks_json);
        let check_id = str_qq(input.get("check_id"));
        let Some(idx) = checks.iter().position(|c| c.get("id").and_then(Value::as_str) == Some(check_id.as_str())) else {
            return Ok(json!({ "ok": true, "skipped": true, "reason": "check not found" }));
        };
        {
            let c = checks[idx].as_object_mut().unwrap();
            c.insert("human_verdict".into(), json!(verdict));
            let note = input.get("note");
            c.insert("human_note".into(), if note.is_some() && !note.unwrap().is_null() {
                json!(str_qq(note).chars().take(300).collect::<String>())
            } else { Value::Null });
            c.insert("human_ts".into(), json!(crate::clock::now_iso_nodb()));
        }
        let summary = summarize(&checks);
        let run_id_v = run.get("id").cloned().unwrap_or(json!(run_id));
        let checks_str = serde_json::to_string(&checks).map_err(|e| e.to_string())?;
        let summary_str = serde_json::to_string(&summary).map_err(|e| e.to_string())?;
        crate::db_json::run_write(db, "UPDATE pr_test_runs SET checks_json = ?, summary_json = ? WHERE id = ?", &[json!(checks_str), json!(summary_str), run_id_v.clone()])?;
        return Ok(json!({ "ok": true, "confirmed": true, "run_id": run_id_v, "summary": summary, "checks": checks }));
    }

    let ref_slug = str_qq(input.get("ref_slug"));
    if ref_slug.is_empty() {
        return Err("pr-test-run: ref_slug is required".to_string());
    }

    if op == "latest" {
        let rows = crate::db_json::query_rows(db, "SELECT * FROM pr_test_runs WHERE ref_slug = ? ORDER BY id DESC LIMIT 1", &[json!(ref_slug.clone())])?;
        return Ok(match first(&rows) {
            Some(row) => {
                let summary = parse_obj(row.get("summary_json").and_then(Value::as_str));
                let checks = parse_arr(row.get("checks_json").and_then(Value::as_str));
                let mut out = row.as_object().cloned().unwrap_or_default();
                out.insert("summary".into(), summary);
                out.insert("checks".into(), json!(checks));
                json!({ "ok": true, "run": Value::Object(out) })
            }
            None => json!({ "ok": true, "run": Value::Null }),
        });
    }

    let contract_rows = crate::db_json::query_rows(
        db,
        "SELECT slug, card_slug, worktree_path, acceptance_json, naming_json FROM contracts WHERE slug = ?",
        &[json!(ref_slug.clone())],
    )?;
    let Some(contract) = first(&contract_rows) else {
        return Ok(json!({ "ok": true, "skipped": true, "reason": format!("contract '{ref_slug}' not found") }));
    };
    let contract_slug = contract.get("slug").and_then(Value::as_str).unwrap_or("").to_string();
    let wt = contract.get("worktree_path").and_then(Value::as_str).map(str::to_string);

    // GATE TERMINALS: delegate to the already-converted build/test gate twins.
    if op == "gate-build" || op == "gate-test" {
        let wt_ok = wt.as_deref().map(|w| !w.is_empty() && Path::new(w).exists()).unwrap_or(false);
        if !wt_ok {
            let reason = match &wt {
                Some(w) if !w.is_empty() => format!("worktree gone on disk: {w}"),
                _ => "no worktree on this PR (merged or cleaned) — nothing to run".to_string(),
            };
            return Ok(json!({ "ok": true, "skipped": true, "reason": reason }));
        }
        let gate_input = json!({ "contract_slug": contract_slug });
        let result = if op == "gate-build" {
            crate::agents_dev_dev_build_gate::devBuildGate(&gate_input, db)
        } else {
            crate::agents_dev_dev_test_gate::devTestGate(&gate_input, db)
        };
        let (passed, result_v) = match result {
            Ok(v) => (v.get("passed").cloned().unwrap_or(Value::Null), v),
            Err(e) => (Value::Null, json!({ "ok": false, "error": e })),
        };
        return Ok(json!({
            "ok": true, "op": op, "contract_slug": contract_slug,
            "phase": if op == "gate-build" { "build" } else { "test" },
            "passed": passed, "result": result_v,
        }));
    }

    // SCOPED FILE READ.
    if op == "file" {
        let wt_ok = wt.as_deref().map(|w| !w.is_empty() && Path::new(w).exists()).unwrap_or(false);
        if !wt_ok {
            return Ok(json!({ "ok": true, "skipped": true, "reason": "no worktree on this PR (merged or cleaned)" }));
        }
        let relpath = str_qq(input.get("path"));
        if relpath.is_empty() {
            return Err("pr-test-run: path is required".to_string());
        }
        let files_touched: Vec<String> = {
            let r = crate::db_json::query_rows(db, "SELECT files_touched_json FROM contracts WHERE slug = ?", &[json!(contract_slug.clone())])?;
            let raw = first(&r).and_then(|row| row.get("files_touched_json").and_then(Value::as_str).map(str::to_string));
            parse_arr(raw.as_deref()).into_iter().filter_map(|v| v.as_str().map(str::to_string)).collect()
        };
        let read_input = json!({ "path": relpath, "root": wt.clone() });
        return Ok(match crate::codebase_codebase_file_read::codebaseFileRead(&read_input, db) {
            Ok(file) => {
                let mut out = file.as_object().cloned().unwrap_or_default();
                out.insert("ok".into(), json!(true));
                out.insert("op".into(), json!("file"));
                out.insert("path".into(), json!(relpath));
                out.insert("in_scope".into(), json!(files_touched.iter().any(|f| f == &relpath)));
                Value::Object(out)
            }
            Err(e) => {
                let msg: String = e.chars().take(300).collect();
                json!({ "ok": false, "op": "file", "path": relpath, "error": msg })
            }
        });
    }

    // WORKTREE TREE.
    if op == "tree" {
        let wt_ok = wt.as_deref().map(|w| !w.is_empty() && Path::new(w).exists()).unwrap_or(false);
        if !wt_ok {
            return Ok(json!({ "ok": true, "skipped": true, "reason": "no worktree on this PR (merged or cleaned)" }));
        }
        let root = wt.clone().unwrap();
        const SKIP: &[&str] = &["node_modules", "build", "dist", "coverage", "out"];
        let mut files: Vec<String> = Vec::new();
        let mut truncated = false;
        fn walk(dir: &Path, rel: &str, depth: u32, files: &mut Vec<String>, truncated: &mut bool) {
            if depth > 8 || files.len() >= 3000 { *truncated = true; return; }
            let entries = match std::fs::read_dir(dir) { Ok(e) => e, Err(_) => return };
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                if SKIP.contains(&name.as_str()) || name.starts_with('.') || name.starts_with("node_modules.") { continue; }
                let p = entry.path();
                let r = if rel.is_empty() { name.clone() } else { format!("{rel}/{name}") };
                let meta = match entry.metadata() { Ok(m) => m, Err(_) => continue };
                if meta.is_dir() {
                    walk(&p, &r, depth + 1, files, truncated);
                } else {
                    files.push(r);
                    if files.len() >= 3000 { *truncated = true; return; }
                }
            }
        }
        walk(Path::new(&root), "", 0, &mut files, &mut truncated);
        files.sort();
        let files_touched: Vec<String> = {
            let r = crate::db_json::query_rows(db, "SELECT files_touched_json FROM contracts WHERE slug = ?", &[json!(contract_slug.clone())])?;
            let raw = first(&r).and_then(|row| row.get("files_touched_json").and_then(Value::as_str).map(str::to_string));
            parse_arr(raw.as_deref()).into_iter().filter_map(|v| v.as_str().map(str::to_string)).collect()
        };
        return Ok(json!({ "ok": true, "op": "tree", "files": files, "files_touched": files_touched, "truncated": truncated }));
    }

    // PER-FILE DIFF.
    if op == "diff" {
        let wt_ok = wt.as_deref().map(|w| !w.is_empty() && Path::new(w).exists()).unwrap_or(false);
        if !wt_ok {
            return Ok(json!({ "ok": true, "skipped": true, "reason": "no worktree on this PR (merged or cleaned)" }));
        }
        let root = wt.clone().unwrap();
        let relpath = str_qq(input.get("path"));
        let base = { let b = str_qq(input.get("base")); if b.is_empty() { "main".to_string() } else { b } };
        let range = format!("{base}...HEAD");
        let mut spec: Vec<&str> = Vec::new();
        if !relpath.is_empty() { spec.push("--"); spec.push(&relpath); }
        let committed_args: Vec<&str> = std::iter::once("diff").chain(std::iter::once(range.as_str())).chain(spec.iter().copied()).collect();
        let committed = git(&committed_args, &root);
        let dirt_args: Vec<&str> = std::iter::once("diff").chain(std::iter::once("HEAD")).chain(spec.iter().copied()).collect();
        let dirt = git(&dirt_args, &root);
        let patch = format!("{}{}", committed.stdout, dirt.stdout);
        if patch.is_empty() && committed.status != Some(0) {
            let msg: String = committed.stderr.chars().take(200).collect();
            return Ok(json!({ "ok": false, "op": "diff", "error": format!("git diff failed (base '{base}' unknown?): {msg}") }));
        }
        let path_out = if relpath.is_empty() { Value::Null } else { json!(relpath) };
        return Ok(json!({ "ok": true, "op": "diff", "path": path_out, "base": base, "patch": patch }));
    }

    // SCOPED EDIT.
    if op == "edit" {
        let relpath = str_qq(input.get("path"));
        if relpath.is_empty() {
            return Err("pr-test-run: path is required".to_string());
        }
        let content = match input.get("content") {
            Some(Value::String(s)) => s.clone(),
            _ => return Err("pr-test-run: content is required".to_string()),
        };
        let write_input = json!({ "path": relpath, "content": content, "contract_slug": contract_slug });
        return Ok(match crate::external_file_write::fileWrite(&write_input, db) {
            Ok(_) => json!({ "ok": true, "op": "edit", "path": relpath, "bytes": content.len() }),
            Err(e) => {
                let denied = e.contains("PERMISSION_DENIED");
                let msg: String = e.chars().take(200).collect();
                json!({ "ok": false, "op": "edit", "path": relpath, "denied": denied, "error": msg })
            }
        });
    }

    // REGENERATE (READ-ONLY proposal).
    if op == "regenerate" {
        let wt_ok = wt.as_deref().map(|w| !w.is_empty() && Path::new(w).exists()).unwrap_or(false);
        if !wt_ok {
            return Ok(json!({ "ok": false, "op": "regenerate", "reason": "no worktree on this PR" }));
        }
        let relpath = str_qq(input.get("path"));
        if relpath.is_empty() {
            return Err("pr-test-run: path is required".to_string());
        }
        let regen_input = json!({
            "path": relpath, "start_line": input.get("start_line").cloned().unwrap_or(Value::Null),
            "end_line": input.get("end_line").cloned().unwrap_or(Value::Null),
            "instruction": input.get("instruction").cloned().unwrap_or(Value::Null),
            "root": wt.clone(),
        });
        return Ok(crate::codebase_code_regenerate::codeRegenerate(&regen_input, db));
    }

    let card = {
        let card_slug = contract.get("card_slug").and_then(Value::as_str).unwrap_or("");
        if card_slug.is_empty() { None } else {
            let rows = crate::db_json::query_rows(db, "SELECT slug, title FROM cards WHERE slug = ?", &[json!(card_slug)])?;
            first(&rows)
        }
    };
    let acceptance = parse_arr(contract.get("acceptance_json").and_then(Value::as_str));
    let naming = parse_obj(contract.get("naming_json").and_then(Value::as_str));

    if op == "suggest" {
        ensure_suggest_prompt(db)?;
        let mut stack = "javascript".to_string();
        if let Some(w) = &wt {
            if let Ok(txt) = std::fs::read_to_string(Path::new(w).join("package.json")) {
                if let Ok(pkg) = serde_json::from_str::<Value>(&txt) {
                    let mut deps: Vec<String> = Vec::new();
                    for key in ["dependencies", "devDependencies"] {
                        if let Some(Value::Object(m)) = pkg.get(key) {
                            deps.extend(m.keys().cloned());
                        }
                    }
                    deps.dedup();
                    let joined = deps.into_iter().take(12).collect::<Vec<_>>().join(", ");
                    if !joined.is_empty() { stack = joined; }
                }
            }
        }
        let acceptance_lines: Vec<String> = acceptance.iter().take(12).enumerate().map(|(i, a)| {
            let id = a.get("id").and_then(Value::as_str).map(str::to_string).unwrap_or_else(|| format!("ac{}", i + 1));
            let text_v = a.get("intent").or_else(|| a.get("text")).or_else(|| a.get("id")).cloned().unwrap_or_else(|| json!(a.to_string()));
            let text: String = str_qq(Some(&text_v)).chars().take(200).collect();
            format!("{id}. {text}")
        }).collect();
        let acceptance_str = if acceptance_lines.is_empty() { "(none)".to_string() } else { acceptance_lines.join("\n") };
        let naming_str: String = serde_json::to_string(&naming).unwrap_or_default().chars().take(1200).collect();
        let card_title = card.as_ref().and_then(|c| c.get("title").and_then(Value::as_str)).map(str::to_string).unwrap_or_else(|| ref_slug.clone());

        let input_kv = json!([
            { "key": "card_title", "value": card_title },
            { "key": "acceptance", "value": acceptance_str },
            { "key": "naming", "value": naming_str },
            { "key": "stack", "value": stack },
        ]);
        let ai_input = json!({
            "prompt_name": SUGGEST_PROMPT, "input_kv": input_kv,
            "tier": "high", "temperature": "cold", "phase_flow": "single_only",
            "two_phase": false, "structured_output": false, "provider_fn": provider_fn,
        });
        let res = crate::ai_ai_call::aiCall(&ai_input, db)?;
        let md = res.get("output_kv").and_then(Value::as_array)
            .and_then(|arr| arr.iter().find(|o| o.get("value_text").map_or(false, |v| !v.is_null())))
            .and_then(|o| o.get("value_text")).cloned().unwrap_or(json!(""));
        let last = crate::db_json::query_rows(db, "SELECT id FROM pr_test_runs WHERE ref_slug = ? ORDER BY id DESC LIMIT 1", &[json!(ref_slug.clone())])?;
        let last_row = first(&last);
        if let Some(row) = &last_row {
            let id = row.get("id").cloned().unwrap_or(Value::Null);
            let md_str = str_qq(Some(&md));
            crate::db_json::run_write(db, "UPDATE pr_test_runs SET suggestion_md = ? WHERE id = ?", &[json!(md_str), id])?;
        }
        let ai_call_id = res.get("ai_call_id").cloned().unwrap_or(Value::Null);
        return Ok(json!({
            "ok": true, "suggested": true, "suggestion_md": md,
            "run_id": last_row.and_then(|r| r.get("id").cloned()).unwrap_or(Value::Null),
            "suggest_ai_call_id": ai_call_id,
        }));
    }

    // ── op 'run': the deterministic suite ──
    let mut checks: Vec<Value> = Vec::new();
    let push = |checks: &mut Vec<Value>, id: &str, label: &str, kind: &str, status: &str, proof: &str| {
        let proof_t: String = proof.chars().take(1200).collect();
        checks.push(json!({ "id": id, "label": label, "kind": kind, "status": status, "proof": proof_t }));
    };

    let wt_ok = wt.as_deref().map(|w| !w.is_empty() && Path::new(w).exists()).unwrap_or(false);
    if !wt_ok {
        let reason = match &wt {
            Some(w) if !w.is_empty() => format!("worktree path missing on disk: {w}"),
            _ => "no worktree on this contract — the PR environment is gone (merged or cleaned)".to_string(),
        };
        push(&mut checks, "env", "isolated environment (worktree)", "env", "fail", &reason);
    } else {
        let wt_path = wt.clone().unwrap();
        push(&mut checks, "env", "isolated environment (worktree)", "env", "pass", &wt_path);
        let pkg_path = Path::new(&wt_path).join("package.json");
        let has_pkg = pkg_path.exists();
        let pkg: Value = std::fs::read_to_string(&pkg_path).ok()
            .and_then(|s| serde_json::from_str(&s).ok()).unwrap_or(json!({}));
        if has_pkg {
            let node_modules_present = Path::new(&wt_path).join("node_modules").exists();
            push(&mut checks, "deps", "dependencies installed", "env",
                if node_modules_present { "pass" } else { "fail" },
                if node_modules_present { "node_modules present" } else { "node_modules MISSING — run npm install in the worktree (new worktrees now install automatically)" });
            let build_script = pkg.get("scripts").and_then(|s| s.get("build")).and_then(Value::as_str);
            if let Some(build_cmd) = build_script {
                let r = sh("npm", &["run", "build"], &wt_path);
                push(&mut checks, "build", &format!("build — {build_cmd}"), "build", if r.status == Some(0) { "pass" } else { "fail" }, &tail(&r, 900));
            } else {
                push(&mut checks, "build", "build", "build", "skip", "no build script in package.json");
            }
            let test_script = pkg.get("scripts").and_then(|s| s.get("test")).and_then(Value::as_str);
            if let Some(test_cmd) = test_script {
                let r = sh("npm", &["test", "--", "--watchAll=false"], &wt_path);
                push(&mut checks, "tests", &format!("existing test suite — {test_cmd}"), "tests", if r.status == Some(0) { "pass" } else { "fail" }, &tail(&r, 900));
            } else {
                push(&mut checks, "tests", "existing test suite", "tests", "skip", "no test script — ✨ suggest unit tests below (proposed as a card, never auto-written)");
            }
        } else {
            push(&mut checks, "deps", "dependencies", "env", "skip", "no package.json — non-node capability");
        }

        // acceptance rows: structured kinds run; prose gets deterministic EVIDENCE (grep) and a
        // needs_human verdict — determinism argues, the human rules.
        let mut name_tokens: Vec<String> = Vec::new();
        fn walk_names(v: &Value, out: &mut Vec<String>) {
            match v {
                Value::String(s) => {
                    let t = s.trim();
                    if is_name_token(t) { out.push(t.to_string()); }
                }
                Value::Array(a) => for x in a { walk_names(x, out); },
                Value::Object(m) => for x in m.values() { walk_names(x, out); },
                _ => {}
            }
        }
        walk_names(&naming, &mut name_tokens);

        for (i, a) in acceptance.iter().take(20).enumerate() {
            let id = a.get("id").and_then(Value::as_str).map(str::to_string).unwrap_or_else(|| format!("ac{}", i + 1));
            let text: String = {
                let v = a.get("intent").or_else(|| a.get("text")).or_else(|| a.get("title"));
                str_qq(v).chars().take(200).collect()
            };
            let kind = a.get("kind").or_else(|| a.get("type")).and_then(Value::as_str);
            let path_v = a.get("path").and_then(Value::as_str);
            let cmd_v = a.get("cmd").and_then(Value::as_str);

            if kind == Some("file-exists") && path_v.is_some() {
                let p = path_v.unwrap();
                let exists = Path::new(&wt_path).join(p).exists();
                let label = if text.is_empty() { format!("file exists: {p}") } else { text.clone() };
                push(&mut checks, &id, &label, "acceptance", if exists { "pass" } else { "fail" }, p);
            } else if kind == Some("test") && cmd_v.is_some() {
                let cmd_full = cmd_v.unwrap();
                let mut parts = cmd_full.split(' ');
                let prog = parts.next().unwrap_or("");
                let rest: Vec<&str> = parts.collect();
                let r = sh(prog, &rest, &wt_path);
                let label = if text.is_empty() { cmd_full.to_string() } else { text.clone() };
                push(&mut checks, &id, &label, "acceptance", if r.status == Some(0) { "pass" } else { "fail" }, &tail(&r, 900));
            } else {
                let mut toks: Vec<String> = Vec::new();
                for m in find_capitalized_tokens(&text) { if !toks.contains(&m) { toks.push(m); } }
                for t in &name_tokens { if !toks.contains(t) { toks.push(t.clone()); } }
                toks.truncate(6);
                let hits = if !toks.is_empty() { grep_worktree(&wt_path, &toks) } else { HashMap::new() };
                let mut found: Vec<String> = Vec::new();
                for t in &toks {
                    if let Some(v) = hits.get(t) {
                        if !v.is_empty() { found.push(format!("{t} → {}", v[0])); }
                    }
                }
                let label = if text.is_empty() { id.clone() } else { text.clone() };
                let proof = if !found.is_empty() {
                    format!("evidence in the worktree:\n{}\n— confirm with your eyes; presence ≠ correctness", found.join("\n"))
                } else {
                    "no deterministic evidence found — inspect and rule".to_string()
                };
                push(&mut checks, &id, &label, "acceptance", "needs_human", &proof);
            }
        }
    }

    let summary = summarize(&checks);
    let checks_str = serde_json::to_string(&checks).map_err(|e| e.to_string())?;
    let summary_str = serde_json::to_string(&summary).map_err(|e| e.to_string())?;
    let branch = input.get("branch").cloned().unwrap_or(Value::Null);
    crate::db_json::run_write(
        db,
        "INSERT INTO pr_test_runs (ref_slug, branch, worktree_path, duration_ms, summary_json, checks_json) VALUES (?, ?, ?, ?, ?, ?)",
        &[json!(ref_slug), branch, wt.clone().map_or(Value::Null, |w| json!(w)), json!(0), json!(summary_str), json!(checks_str)],
    )?;
    let run_id = db.last_insert_rowid();
    Ok(json!({ "ok": true, "run_id": run_id, "summary": summary, "checks": checks, "duration_ms": 0 }))
}

// `/^[A-Za-z_$][A-Za-z0-9_$.-]{3,60}$/`
fn is_name_token(s: &str) -> bool {
    let b = s.as_bytes();
    if b.is_empty() { return false; }
    let first_ok = b[0].is_ascii_alphabetic() || b[0] == b'_' || b[0] == b'$';
    if !first_ok { return false; }
    let rest = &s[1..];
    if !(3..=60).contains(&rest.chars().count()) { return false; }
    rest.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$' || c == '.' || c == '-')
}

// `text.match(/[A-Z][A-Za-z0-9]{3,}/g) ?? []` — a capital letter then 3+ more alnum chars.
fn find_capitalized_tokens(text: &str) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i].is_ascii_uppercase() {
            let mut j = i + 1;
            while j < chars.len() && chars[j].is_ascii_alphanumeric() { j += 1; }
            if j - i >= 4 { out.push(chars[i..j].iter().collect()); }
            i = j.max(i + 1);
        } else {
            i += 1;
        }
    }
    out
}

fn git(args: &[&str], cwd: &str) -> ShResult {
    match crate::proc::cmd("git").args(args).current_dir(cwd).output() {
        Ok(o) => ShResult {
            status: o.status.code(),
            stdout: String::from_utf8_lossy(&o.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&o.stderr).into_owned(),
        },
        Err(e) => ShResult { status: None, stdout: String::new(), stderr: e.to_string() },
    }
}

const SUGGEST_PROMPT: &str = "pr.tests.suggest.v1";
const SUGGEST_TEMPLATE: &str = "Draft UNIT TESTS for the acceptance criteria below. The tests are a PROPOSAL for a human to review — they will NOT be written into the repo by you.\n\nRules: one describe/test block per acceptance item, named after its id · use the project's visible stack ({{stack}}) · deterministic assertions only · no snapshots · reply with ONLY a markdown section: a one-line rationale, then one fenced code block with the complete test file.\n\n═══ CARD ═══\n{{card_title}}\n\n═══ ACCEPTANCE ═══\n{{acceptance}}\n\n═══ NAMES ALREADY DECIDED (use them exactly) ═══\n{{naming}}";

// mirrors JS `ensureSuggestPrompt`: `try { ex = promptFind(...) } catch { ex = null }`, skip
// re-registering when the template text is unchanged, else `promptAdd`.
fn ensure_suggest_prompt(db: &Connection) -> Result<(), String> {
    let existing = crate::ai_prompt_find::promptFind(&json!({ "name": SUGGEST_PROMPT }), db).ok();
    if let Some(row) = existing.as_ref() {
        if row.get("template_md").and_then(Value::as_str) == Some(SUGGEST_TEMPLATE) {
            return Ok(());
        }
    }
    crate::ai_prompt_add::promptAdd(
        &json!({
            "name": SUGGEST_PROMPT, "template_md": SUGGEST_TEMPLATE,
            "default_tier": "high", "default_temperature": "cold", "max_output_tokens": 3000,
            "output_fields": [{ "field_name": "tests_md", "field_type": "string" }],
            "judging_criteria": [
                "One test block per acceptance item, named by its id.",
                "Uses only decided names and the visible stack.",
                "Deterministic assertions, no snapshots.",
                "Exactly one fenced code block.",
            ],
        }),
        db,
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS brain;").unwrap();
        c.execute_batch(
            "CREATE TABLE pr_test_runs (id INTEGER PRIMARY KEY AUTOINCREMENT, ref_slug TEXT NOT NULL, branch TEXT, worktree_path TEXT, started_at TEXT NOT NULL DEFAULT (datetime('now')), duration_ms INTEGER, summary_json TEXT, checks_json TEXT, suggestion_md TEXT);
             CREATE TABLE contracts (id INTEGER PRIMARY KEY AUTOINCREMENT, slug TEXT NOT NULL UNIQUE, title TEXT NOT NULL DEFAULT '', card_slug TEXT, worktree_path TEXT, acceptance_json TEXT NOT NULL DEFAULT '[]', naming_json TEXT NOT NULL DEFAULT '{}', files_touched_json TEXT NOT NULL DEFAULT '[]');
             CREATE TABLE cards (id INTEGER PRIMARY KEY AUTOINCREMENT, slug TEXT NOT NULL UNIQUE, title TEXT NOT NULL DEFAULT '');
             CREATE TABLE brain.prompt_templates ( id INTEGER PRIMARY KEY, name TEXT NOT NULL, version INTEGER NOT NULL, template_md TEXT NOT NULL, default_tier TEXT NOT NULL, default_temperature TEXT NOT NULL, max_output_tokens INTEGER NOT NULL DEFAULT 1024, superseded_by INTEGER, created_at TEXT NOT NULL DEFAULT (datetime('now')), cross_field_rules_json TEXT, judging_criteria_json TEXT, system_md TEXT, phase_flow TEXT NOT NULL DEFAULT 'single', UNIQUE (name, version) );
             CREATE TABLE brain.prompt_output_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, grounded_refs_json TEXT, UNIQUE (prompt_template_id, field_name) );
             CREATE TABLE brain.prompt_input_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, UNIQUE (prompt_template_id, field_name) );",
        ).unwrap();
        c
    }

    #[test]
    fn ref_slug_required_for_non_confirm_non_latest_ops() {
        let c = setup();
        assert!(prTestRun(&json!({ "op": "run" }), &c).unwrap_err().contains("ref_slug is required"));
    }

    #[test]
    fn latest_returns_null_when_no_runs() {
        let c = setup();
        let r = prTestRun(&json!({ "op": "latest", "ref_slug": "x" }), &c).unwrap();
        assert_eq!(r, json!({ "ok": true, "run": null }));
    }

    #[test]
    fn latest_returns_newest_run_with_parsed_summary_and_checks() {
        let c = setup();
        c.execute("INSERT INTO pr_test_runs (ref_slug, summary_json, checks_json) VALUES ('x', '{\"total\":1}', '[{\"id\":\"a\"}]')", []).unwrap();
        let r = prTestRun(&json!({ "op": "latest", "ref_slug": "x" }), &c).unwrap();
        assert_eq!(r["run"]["summary"], json!({ "total": 1 }));
        assert_eq!(r["run"]["checks"], json!([{ "id": "a" }]));
    }

    #[test]
    fn run_skips_when_contract_not_found() {
        let c = setup();
        let r = prTestRun(&json!({ "op": "run", "ref_slug": "nope" }), &c).unwrap();
        assert_eq!(r, json!({ "ok": true, "skipped": true, "reason": "contract 'nope' not found" }));
    }

    #[test]
    fn run_fails_env_check_when_worktree_missing() {
        let c = setup();
        c.execute("INSERT INTO contracts (slug, worktree_path) VALUES ('pr1', NULL)", []).unwrap();
        let r = prTestRun(&json!({ "op": "run", "ref_slug": "pr1" }), &c).unwrap();
        assert_eq!(r["ok"], json!(true));
        let checks = r["checks"].as_array().unwrap();
        assert_eq!(checks[0]["id"], json!("env"));
        assert_eq!(checks[0]["status"], json!("fail"));
        assert_eq!(r["summary"]["fail"], json!(1));
        assert_eq!(r["summary"]["total"], json!(1));
        // persisted
        let count: i64 = c.query_row("SELECT COUNT(*) FROM pr_test_runs WHERE ref_slug = 'pr1'", [], |row| row.get(0)).unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn run_reports_worktree_path_missing_on_disk_reason() {
        let c = setup();
        c.execute("INSERT INTO contracts (slug, worktree_path) VALUES ('pr2', 'C:/nope/does/not/exist')", []).unwrap();
        let r = prTestRun(&json!({ "op": "run", "ref_slug": "pr2" }), &c).unwrap();
        let checks = r["checks"].as_array().unwrap();
        assert!(checks[0]["proof"].as_str().unwrap().contains("worktree path missing on disk"));
    }

    #[test]
    fn confirm_skips_when_run_not_found() {
        let c = setup();
        let r = prTestRun(&json!({ "op": "confirm", "run_id": 999, "verdict": "pass" }), &c).unwrap();
        assert_eq!(r, json!({ "ok": true, "skipped": true, "reason": "run not found" }));
    }

    #[test]
    fn confirm_rejects_bad_verdict() {
        let c = setup();
        c.execute("INSERT INTO pr_test_runs (ref_slug, checks_json) VALUES ('x', '[]')", []).unwrap();
        let rid = c.last_insert_rowid();
        let err = prTestRun(&json!({ "op": "confirm", "run_id": rid, "verdict": "maybe" }), &c).unwrap_err();
        assert!(err.contains("verdict must be one of: pass, fail"));
    }

    #[test]
    fn confirm_skips_when_check_not_found() {
        let c = setup();
        c.execute("INSERT INTO pr_test_runs (ref_slug, checks_json) VALUES ('x', '[{\"id\":\"a\",\"status\":\"pass\"}]')", []).unwrap();
        let rid = c.last_insert_rowid();
        let r = prTestRun(&json!({ "op": "confirm", "run_id": rid, "check_id": "zzz", "verdict": "pass" }), &c).unwrap();
        assert_eq!(r, json!({ "ok": true, "skipped": true, "reason": "check not found" }));
    }

    #[test]
    fn confirm_overlays_human_verdict_and_recomputes_summary() {
        let c = setup();
        c.execute("INSERT INTO pr_test_runs (ref_slug, checks_json) VALUES ('x', '[{\"id\":\"a\",\"status\":\"needs_human\"},{\"id\":\"b\",\"status\":\"pass\"}]')", []).unwrap();
        let rid = c.last_insert_rowid();
        let r = prTestRun(&json!({ "op": "confirm", "run_id": rid, "check_id": "a", "verdict": "fail", "note": "nope" }), &c).unwrap();
        assert_eq!(r["confirmed"], json!(true));
        assert_eq!(r["summary"], json!({ "pass": 1, "fail": 1, "needs_human": 0, "skip": 0, "total": 2 }));
        let checks = r["checks"].as_array().unwrap();
        let a = checks.iter().find(|c| c["id"] == json!("a")).unwrap();
        assert_eq!(a["human_verdict"], json!("fail"));
        assert_eq!(a["human_note"], json!("nope"));
        assert_eq!(a["human_ts"], json!("2026-01-01T00:00:00.000Z"));
        // persisted
        let stored: String = c.query_row("SELECT summary_json FROM pr_test_runs WHERE id = ?", [rid], |row| row.get(0)).unwrap();
        assert!(stored.contains("\"fail\":1"));
    }

    #[test]
    fn confirm_note_null_when_absent() {
        let c = setup();
        c.execute("INSERT INTO pr_test_runs (ref_slug, checks_json) VALUES ('x', '[{\"id\":\"a\",\"status\":\"pass\"}]')", []).unwrap();
        let rid = c.last_insert_rowid();
        let r = prTestRun(&json!({ "op": "confirm", "run_id": rid, "check_id": "a", "verdict": "pass" }), &c).unwrap();
        let checks = r["checks"].as_array().unwrap();
        assert_eq!(checks[0]["human_note"], Value::Null);
    }

    #[test]
    fn gate_ops_skip_when_no_worktree() {
        let c = setup();
        c.execute("INSERT INTO contracts (slug, worktree_path) VALUES ('pr3', NULL)", []).unwrap();
        let r = prTestRun(&json!({ "op": "gate-build", "ref_slug": "pr3" }), &c).unwrap();
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["skipped"], json!(true));
    }

    #[test]
    fn file_op_requires_path() {
        let c = setup();
        c.execute("INSERT INTO contracts (slug, worktree_path) VALUES ('pr4', 'C:/does/not/exist')", []).unwrap();
        let r = prTestRun(&json!({ "op": "file", "ref_slug": "pr4" }), &c).unwrap();
        assert_eq!(r["skipped"], json!(true));
    }

    #[test]
    fn edit_requires_path_and_content() {
        let c = setup();
        c.execute("INSERT INTO contracts (slug, worktree_path) VALUES ('pr5', NULL)", []).unwrap();
        assert!(prTestRun(&json!({ "op": "edit", "ref_slug": "pr5" }), &c).unwrap_err().contains("path is required"));
        assert!(prTestRun(&json!({ "op": "edit", "ref_slug": "pr5", "path": "a.txt" }), &c).unwrap_err().contains("content is required"));
    }

    #[test]
    fn regenerate_requires_worktree_then_path() {
        let c = setup();
        c.execute("INSERT INTO contracts (slug, worktree_path) VALUES ('pr6', NULL)", []).unwrap();
        let r = prTestRun(&json!({ "op": "regenerate", "ref_slug": "pr6" }), &c).unwrap();
        assert_eq!(r, json!({ "ok": false, "op": "regenerate", "reason": "no worktree on this PR" }));
    }

    #[test]
    fn run_supports_file_exists_acceptance_kind_against_a_missing_worktree_file() {
        // worktree missing → env fails before acceptance loop runs at all (only the env check fires)
        let c = setup();
        c.execute(
            "INSERT INTO contracts (slug, worktree_path, acceptance_json) VALUES ('pr7', NULL, '[{\"id\":\"ac1\",\"kind\":\"file-exists\",\"path\":\"x.txt\"}]')",
            [],
        ).unwrap();
        let r = prTestRun(&json!({ "op": "run", "ref_slug": "pr7" }), &c).unwrap();
        let checks = r["checks"].as_array().unwrap();
        assert_eq!(checks.len(), 1);
        assert_eq!(checks[0]["id"], json!("env"));
    }

    // ── pure helpers ────────────────────────────────────────────────────────────────────────
    #[test]
    fn is_name_token_matches_js_regex() {
        assert!(is_name_token("fooBar"));
        assert!(is_name_token("a.b-c_d"));
        assert!(!is_name_token("ab")); // too short (needs 4+ total)
        assert!(!is_name_token("1abc")); // must start letter/_/$
    }

    #[test]
    fn find_capitalized_tokens_matches_js_regex() {
        assert_eq!(find_capitalized_tokens("The FooBar class uses Widget42 and Ab"), vec!["FooBar", "Widget42"]);
        assert_eq!(find_capitalized_tokens("nothing here"), Vec::<String>::new());
    }

    #[test]
    fn summarize_counts_by_effective_status() {
        let checks = vec![
            json!({ "status": "pass" }),
            json!({ "status": "needs_human", "human_verdict": "fail" }),
            json!({ "status": "skip" }),
        ];
        assert_eq!(summarize(&checks), json!({ "pass": 1, "fail": 1, "needs_human": 0, "skip": 1, "total": 3 }));
    }

    #[test]
    fn tail_truncates_from_the_end_and_labels_empty_output() {
        let r = ShResult { status: Some(0), stdout: String::new(), stderr: String::new() };
        assert_eq!(tail(&r, 900), "(no output)");
        let long = ShResult { status: Some(0), stdout: "x".repeat(10), stderr: String::new() };
        assert_eq!(tail(&long, 5), "…xxxxx");
    }
}
