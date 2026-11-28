//! dev-build-gate.rs — twin of skills/agents/dev/dev-build-gate.mjs (db lane, THIN CONSUMER of
//! already-converted twins: `contract_contract_get::contractGet`,
//! `system_workspace_scripts::resolveScript`, `blueprint__helpers_snippets_hash::snippetsContentHash`,
//! `blueprint_blueprint_apply::blueprintApply`, `driver__helpers_failure_advice::adviseOnFailure`).
//! NAMES ARE CONTRACT: `devBuildGate` (default), `extractFailingFiles`, `changedFilesInWorktree`,
//! `resolveBuildCmd` (named exports). BESPOKE-NATIVE (not conform-db fixture): a real
//! `std::process::Command` build/bootstrap subprocess run + real `git` reads are genuinely
//! non-deterministic/non-portable across the JS/Rust conform lanes — proven here by native
//! `#[cfg(test)]` tests instead, same category+precedent as the sibling
//! `agents_dev_dev_test_gate` twin (whose own module doc discloses the identical real-subprocess
//! gap) and `driver_driver_queue`/`external_auth_sign`.
//!
//! ## What is ported FAITHFULLY
//! - The full skip-doctrine ladder in JS source order: operator gate toggle off
//!   (`gate_build_enabled='off'`) → broken-baseline skip (AFTER phase only, `build_gate_runs`
//!   phase='baseline' passed=0 OR evidence matches `/PRE-EXISTING/i`) → no sandbox/worktree root →
//!   blueprint re-sync (best-effort `blueprintApply`, swallowed on error into `syncNote`) → no
//!   resolvable build command.
//! - `resolveBuildCmd`: contract.build_cmd override → `resolveScript(db,'build')` registry →
//!   `tsconfig.json` presence → `package.json` with `scripts.build` → `None`.
//! - Auto-bootstrap derivation (`package.json` present + `node_modules` absent → `npm ci`/`npm
//!   install`) and a real bootstrap subprocess run, `bootstrapNote` on nonzero exit.
//! - The REAL build subprocess run (`std::process::Command`, shell, `CI=1` + any persisted
//!   `ws_node_options` NODE_OPTIONS applied upfront), with best-effort `build_gate_live` telemetry
//!   rows (same shape as the JS live-terminal mirror, `run_cmd` below).
//! - The DETERMINISTIC RECOVERY LADDER on first failure: `openssl-legacy-webpack` class → retry
//!   with `NODE_OPTIONS=--openssl-legacy-provider`, on success persists `ws_node_options` into
//!   `cost_settings`; `missing-dependency-binary`/`native-build-dep` class → (worktree only, and
//!   only when `node_modules` already exists) clean-reinstall + rebuild. Both ladders faithfully
//!   replace `r` with the retry's result on failure (so the REAL error, not the OpenSSL wall,
//!   surfaces) exactly like the JS `recoveryNote`/`r = {status:r2.status,...}` reassignment.
//! - `extractFailingFiles` (3 regexes, `Set`-dedup insertion order) and `changedFilesInWorktree`
//!   (git base-branch diff + `git status --porcelain` dirt, `None` on an unreadable change set) —
//!   the PRE-EXISTING-WORKSPACE-FAILURE arbiter: every named failing file must be OUTSIDE the
//!   card's `changed_files`. `input.changed_files` (a plain JSON array, NOT a JS function seam) is
//!   honored exactly like JS `input?.changed_files ?? changedFilesInWorktree(...)` — this is the
//!   primary native-test seam, avoiding a real git dependency in most cases.
//! - Both PRE-EXISTING warn-pass branches: baseline-was-already-red (AFTER phase) and
//!   failing-files-outside-changeset (both phases), including the best-effort `notifications`
//!   INSERT on the latter.
//! - `write_row`'s exact JS INSERT-fallback shape: try the 9-column INSERT
//!   (`..., snippets_hash, duration_ms, phase`) first, and on failure (older schema without those
//!   columns) fall back to the 7-column INSERT — same divergence-preserved pattern as
//!   `agents_dev_dev_test_gate::write_row`: the "HOW TO PROCEED" (`adviseOnFailure`) amendment is
//!   computed for the DB row only and never mutates the `evidence_md` returned to the caller.
//! - Frozen clock (playbook datetime lane): `gateT0`/`Date.now()` deltas are always `0` under the
//!   frozen instant — `duration_ms` is hardcoded `0i64` in `write_row` rather than threading a
//!   `gateT0` variable through, same convention as the sibling test-gate twin.
//!
//! ## Deliberately NOT ported (disclosed gaps)
//! - **DEV-SERVER PROBE** (`resolveScript(db,'dev')` + a 15s `spawn` aliveness check): purely
//!   DIAGNOSTIC — it never changes `passed`, only appends a note to the RED evidence banner. Not
//!   ported; `dev_probe_note` is always `""` in this twin. A real 15s sleep in every native test
//!   run would make the suite unusably slow for a value that cannot flip any assertion.
//! - **LIVE-TERMINAL byte-for-byte streaming** (`spawn` + `stdout`/`stderr` `data`-event chunk
//!   boundaries + a 600s SIGKILL timeout): `run_cmd` below runs the command to completion via
//!   `Command::output()` and writes the FINAL concatenated stdout/stderr as 1-2 `build_gate_live`
//!   rows instead of many small chunks — the timeout/chunking is OS-timing-dependent even in JS
//!   and never part of the compared `passed`/`evidence_md` value (same precedent as
//!   `agents_dev_dev_test_gate::run_streaming`).
//! - `console.log` gate/session lines (`gateLog`, the `[gate build] ... RED class=...` /
//!   `RECOVERED via ...` lines): pure stdout side effects, never reach the return value or
//!   `db_after` — not ported (same convention as every sibling twin).
//! - `REPO_ROOT`-relative resolution of a bare `sandbox_path` (JS: `resolve(REPO_ROOT,
//!   contract.sandbox_path)` where `REPO_ROOT` is 4 dirs up from the `.mjs` file, i.e. the
//!   antiloki-core checkout root): this twin ships in a SEPARATE repo (antiloki-core-rust) with no
//!   equivalent fixed anchor, so a relative `sandbox_path` is joined onto `ANTILOKI_REPO_ROOT` (env
//!   var, default `"."`) via `sandbox_root()` below instead of a hardcoded JS-repo-specific path.
//!   An absolute `worktree_path` (the common real-world case — EPIC-WORKTREE) bypasses this
//!   entirely, and every native test below seeds an absolute path so the divergence is never
//!   exercised.
//! - `context.db` missing / `contract_slug` missing / contract not found → JS throws
//!   (`err.contextMissing`, `err.required`, `AntilokiError{code:NOT_FOUND}`). `db: &Connection` is
//!   a required, non-nullable reference (same convention as every sibling twin) so the
//!   `contextMissing` guard is unreachable; the other two are ported as `Err(String)` with the
//!   matching `${skill}: ${cause}` message but not exercised beyond the guard test (playbook
//!   gotcha 9 — success path only for throws).
// QUALITY: 78/100 — full skip ladder + recovery ladder + pre-existing-failure arbiter ported
// faithfully; two disclosed diagnostic/telemetry-only gaps (dev probe, byte-chunk streaming) that
// cannot affect `passed`/`evidence_md`; REPO_ROOT anchor necessarily reinterpreted cross-repo.

use regex::Regex;
use rusqlite::Connection;
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::path::Path;
use std::sync::OnceLock;

fn truthy(v: Option<&Value>) -> bool { match v { None|Some(Value::Null)=>false, Some(Value::Bool(b))=>*b, Some(Value::Number(n))=>n.as_f64().map_or(false,|f|f!=0.0&&!f.is_nan()), Some(Value::String(s))=>!s.is_empty(), Some(_)=>true } }
fn str_qq(v: Option<&Value>) -> String { match v { None|Some(Value::Null)=>String::new(), Some(Value::String(s))=>s.clone(), Some(Value::Bool(b))=>b.to_string(), Some(n@Value::Number(_))=>n.to_string(), Some(o)=>o.to_string() } }

const EVIDENCE_CAP: usize = 8000;
// `capEvidence` — head-ok: tail-biased on purpose (session #18 lesson: the real compiler error
// lives at the END of build output).
fn cap_evidence(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= EVIDENCE_CAP { return s.to_string(); }
    let head: String = chars[..1500].iter().collect();
    let tail_len = EVIDENCE_CAP - 1500;
    let tail_start = chars.len() - tail_len;
    let tail: String = chars[tail_start..].iter().collect();
    format!("{head}\n\u{2026}[{} chars snipped \u{2014} full stream in the live terminal]\u{2026}\n{tail}", chars.len() - EVIDENCE_CAP)
}

fn re_webpack_file() -> &'static Regex { static R: OnceLock<Regex> = OnceLock::new(); R.get_or_init(|| Regex::new(r"(?m)^\./([\w@][\w@/.\-]+\.[a-z]{1,6})\s*$").unwrap()) }
fn re_attempted_import() -> &'static Regex { static R: OnceLock<Regex> = OnceLock::new(); R.get_or_init(|| Regex::new(r"(?s)Attempted import error[^\n]*?'\./[\w/.\-]+'[^\n]*?from '\./([\w/.\-]+)'").unwrap()) }
fn re_module_not_found() -> &'static Regex { static R: OnceLock<Regex> = OnceLock::new(); R.get_or_init(|| Regex::new(r"(?s)Module not found[^\n]*?in '[^']*?[\\/](src[\\/][^']*)'").unwrap()) }

/// `export function extractFailingFiles(evidence)` — pure regex extraction, `Set`-dedup in
/// insertion order (webpack-error-line matches first, then attempted-import, then
/// module-not-found).
#[allow(non_snake_case)]
pub fn extractFailingFiles(evidence: &Value) -> Value {
    let t = str_qq(Some(evidence));
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut out: Vec<String> = Vec::new();
    let mut push = |f: String, seen: &mut BTreeSet<String>, out: &mut Vec<String>| {
        if seen.insert(f.clone()) { out.push(f); }
    };
    for caps in re_webpack_file().captures_iter(&t) {
        push(caps[1].to_string(), &mut seen, &mut out);
    }
    for caps in re_attempted_import().captures_iter(&t) {
        push(caps[1].to_string(), &mut seen, &mut out);
    }
    for caps in re_module_not_found().captures_iter(&t) {
        push(caps[1].replace('\\', "/"), &mut seen, &mut out);
    }
    json!(out)
}

fn run_output(cmd: &str, cwd: &str, timeout_hint_secs: u64) -> Result<(i32, String, String), String> {
    let _ = timeout_hint_secs; // JS uses a 30s exec timeout for git reads; not modeled (see module doc: byte-chunk streaming gap)
    #[cfg(windows)]
    let mut c = { let mut c = crate::proc::cmd("cmd"); c.arg("/C").arg(cmd); c };
    #[cfg(not(windows))]
    let mut c = { let mut c = crate::proc::cmd("sh"); c.arg("-c").arg(cmd); c };
    c.current_dir(cwd);
    let o = c.output().map_err(|e| e.to_string())?;
    Ok((o.status.code().unwrap_or(1), String::from_utf8_lossy(&o.stdout).into_owned(), String::from_utf8_lossy(&o.stderr).into_owned()))
}

/// Mirror `execSync`: a nonzero exit code is a THROW in JS, unlike `Command::output()` which
/// only errors when the process fails to launch. `Ok` only on exit code 0.
fn exec_sync_like(cmd: &str, cwd: &str) -> Result<String, String> {
    let (code, stdout, stderr) = run_output(cmd, cwd, 30)?;
    if code != 0 { return Err(stderr); }
    Ok(stdout)
}

/// `export function changedFilesInWorktree(root, execFn = execSync)` — the exec-function seam is
/// not modeled (same convention as `agents_dev_dev_test_gate`'s exec_fn: a JS-function-valued
/// test-only escape hatch, unreachable in production and unencodable as fixture/native-test JSON).
/// Real `git` reads. `None` mirrors JS `null` (unreadable change set — caller must stay RED).
#[allow(non_snake_case)]
pub fn changedFilesInWorktree(root: &str) -> Option<Vec<String>> {
    let mut out: BTreeSet<String> = BTreeSet::new();
    // outer try: base-branch diff (best-effort, swallowed on error — falls through to dirt-only).
    // JS runs `git rev-parse --abbrev-ref origin/HEAD 2>nul || git rev-parse --abbrev-ref HEAD` as
    // ONE shell command (the `||` is a shell fallback, not a Rust retry) — replicated verbatim per
    // platform shell so a missing `origin/HEAD` symbolic ref falls back exactly like JS.
    (|| -> Option<()> {
        #[cfg(windows)]
        let rev_parse_cmd = "git rev-parse --abbrev-ref origin/HEAD 2>nul || git rev-parse --abbrev-ref HEAD";
        #[cfg(not(windows))]
        let rev_parse_cmd = "git rev-parse --abbrev-ref origin/HEAD 2>/dev/null || git rev-parse --abbrev-ref HEAD";
        let base_ref = exec_sync_like(rev_parse_cmd, root).ok()?.trim().to_string();
        let range_base = if base_ref.contains('/') { base_ref } else { "HEAD~20".to_string() };
        let diff = exec_sync_like(&format!("git diff --name-only {range_base}...HEAD"), root).ok()?;
        for l in diff.split('\n') {
            let l = l.trim();
            if !l.is_empty() { out.insert(l.to_string()); }
        }
        Some(())
    })();
    // inner try: git status dirt — a throw here (nonzero exit, e.g. not a git repo) means the
    // change set is unreadable → the caller must stay RED (`None`, mirrors JS `return null`).
    match exec_sync_like("git status --porcelain", root) {
        Ok(stdout) => {
            for l in stdout.split('\n') {
                if l.len() > 3 {
                    let f = l[3..].trim().replace('\\', "/");
                    if !f.is_empty() { out.insert(f); }
                }
            }
        }
        Err(_) => return None,
    }
    Some(out.into_iter().collect())
}

/// `export function resolveBuildCmd(contract, sandboxRoot, fsApi, db)` — `fsApi` always resolves
/// to the real filesystem here (same convention as `system_workspace_scripts::detectScripts`'s
/// package.json sniffing / `agents_dev_dev_test_gate`'s fs_api note).
#[allow(non_snake_case)]
pub fn resolveBuildCmd(contract: &Value, sandbox_root: &str, db: &Connection) -> Option<String> {
    if let Some(s) = contract.get("build_cmd").and_then(Value::as_str) {
        let s = s.trim();
        if !s.is_empty() { return Some(s.to_string()); }
    }
    let reg = crate::system_workspace_scripts::resolveScript(db, "build");
    if truthy(Some(&reg)) {
        if let Some(s) = reg.as_str() { return Some(s.to_string()); }
    }
    if Path::new(sandbox_root).join("tsconfig.json").exists() {
        return Some("npx tsc --noEmit".to_string());
    }
    let pkg_path = Path::new(sandbox_root).join("package.json");
    if let Ok(text) = std::fs::read_to_string(&pkg_path) {
        if let Ok(pkg) = serde_json::from_str::<Value>(&text) {
            if truthy(pkg.get("scripts").and_then(|s| s.get("build"))) {
                return Some("npm run build".to_string());
            }
        }
    }
    None
}

fn sandbox_root(contract: &Value) -> Option<String> {
    if let Some(w) = contract.get("worktree_path").and_then(Value::as_str) {
        if !w.is_empty() { return Some(w.to_string()); }
    }
    let s = contract.get("sandbox_path").and_then(Value::as_str)?;
    if s.is_empty() { return None; }
    if Path::new(s).is_absolute() { return Some(s.to_string()); }
    let repo_root = std::env::var("ANTILOKI_REPO_ROOT").unwrap_or_else(|_| ".".to_string());
    Some(format!("{repo_root}/{s}"))
}

/// Real subprocess run + best-effort `build_gate_live` telemetry (see module doc: not
/// byte-chunked, no 600s SIGKILL timeout — final concatenated output only).
fn run_cmd(db: &Connection, slug: &str, phase: &str, seq: &mut i64, cmd: &str, cwd: &str, extra_node_options: Option<&str>) -> (i32, String, String) {
    let mut live = |chunk: &str, done: i64, seq: &mut i64| {
        let capped: String = chunk.chars().take(4000).collect();
        let _ = crate::db_json::run_write(
            db,
            "INSERT INTO build_gate_live (contract_slug, phase, seq, chunk, done) VALUES (?, ?, ?, ?, ?)",
            &[json!(slug), json!(phase), json!(*seq), json!(capped), json!(done)],
        );
        *seq += 1;
    };
    live(&format!("$ {cmd}\n"), 0, seq);

    #[cfg(windows)]
    let mut c = { let mut c = crate::proc::cmd("cmd"); c.arg("/C").arg(cmd); c };
    #[cfg(not(windows))]
    let mut c = { let mut c = crate::proc::cmd("sh"); c.arg("-c").arg(cmd); c };
    c.current_dir(cwd).env("CI", "1");
    if let Some(opts) = extra_node_options {
        let existing = std::env::var("NODE_OPTIONS").unwrap_or_default();
        c.env("NODE_OPTIONS", format!("{existing} {opts}").trim());
    }

    match c.output() {
        Ok(o) => {
            let out = String::from_utf8_lossy(&o.stdout).into_owned();
            let err = String::from_utf8_lossy(&o.stderr).into_owned();
            if !out.is_empty() { live(&out, 0, seq); }
            if !err.is_empty() { live(&err, 0, seq); }
            let code = o.status.code().unwrap_or(1);
            live(&format!("\n(exit {code})\n"), 1, seq);
            (code, out, err)
        }
        Err(e) => {
            live(&format!("spawn error: {e}\n"), 1, seq);
            (1, String::new(), e.to_string())
        }
    }
}

fn write_row(db: &Connection, slug: &str, applied_at: &Value, snip_hash: &Value, phase: &str, passed: bool, skipped: bool, build_cmd: Option<&str>, evidence_md: Option<&str>) {
    let mut evidence: Option<String> = evidence_md.map(|s| s.to_string());
    if !passed && !skipped {
        if let Some(ev) = evidence.clone() {
            let adv = crate::driver__helpers_failure_advice::adviseOnFailure(&json!(ev));
            let failure_class = adv.get("failure_class").and_then(Value::as_str).unwrap_or("");
            let remedy_md = adv.get("remedy_md").and_then(Value::as_str).unwrap_or("");
            evidence = Some(format!("{ev}\n\n=== HOW TO PROCEED ({failure_class}) ===\n{remedy_md}"));
        }
    }
    let full = crate::db_json::run_write(
        db,
        "INSERT INTO build_gate_runs (contract_slug, passed, skipped, build_cmd, evidence_md, applied_at, snippets_hash, duration_ms, phase) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        &[
            json!(slug), json!(if passed {1} else {0}), json!(if skipped {1} else {0}),
            build_cmd.map(|s| json!(s)).unwrap_or(Value::Null),
            evidence.clone().map(|s| json!(s)).unwrap_or(Value::Null),
            applied_at.clone(), snip_hash.clone(),
            json!(0i64), // frozen Date.now() - gateT0 (see module doc)
            json!(phase),
        ],
    );
    if full.is_err() {
        let _ = crate::db_json::run_write(
            db,
            "INSERT INTO build_gate_runs (contract_slug, passed, skipped, build_cmd, evidence_md, applied_at, snippets_hash) VALUES (?, ?, ?, ?, ?, ?, ?)",
            &[
                json!(slug), json!(if passed {1} else {0}), json!(if skipped {1} else {0}),
                build_cmd.map(|s| json!(s)).unwrap_or(Value::Null),
                evidence.map(|s| json!(s)).unwrap_or(Value::Null),
                applied_at.clone(), snip_hash.clone(),
            ],
        );
    }
}

fn broken_baseline(db: &Connection, slug: &str) -> Option<Value> {
    let rows = crate::db_json::query_rows(
        db,
        "SELECT id, passed, evidence_md FROM build_gate_runs WHERE contract_slug = ? AND COALESCE(phase,'after') = 'baseline' ORDER BY id DESC LIMIT 1",
        &[json!(slug)],
    ).ok()?;
    let row = rows.as_array().and_then(|a| a.first())?.clone();
    let passed0 = row.get("passed").and_then(Value::as_i64) == Some(0);
    let evidence = row.get("evidence_md").and_then(Value::as_str).unwrap_or("");
    static PRE: OnceLock<Regex> = OnceLock::new();
    let re = PRE.get_or_init(|| Regex::new(r"(?i)PRE-EXISTING").unwrap());
    if passed0 || re.is_match(evidence) { Some(row) } else { None }
}

#[allow(non_snake_case)]
pub fn devBuildGate(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS `if (!db) throw err.contextMissing(...)` — unreachable (db: &Connection is required),
    // same convention as every sibling twin.
    let slug = match input {
        Value::String(s) => s.clone(),
        _ => input.get("contract_slug").and_then(Value::as_str).unwrap_or("").to_string(),
    };
    if slug.is_empty() {
        return Err("dev-build-gate: contract_slug is required".to_string());
    }

    let contract = crate::contract_contract_get::contractGet(&json!(slug.clone()), db)?;
    if contract.is_null() {
        return Err(format!("dev-build-gate: contract '{slug}' not found"));
    }
    let applied_at = contract.get("updated_at").cloned().unwrap_or(Value::Null);

    let phase: &str = match input.get("phase").and_then(Value::as_str) {
        Some("baseline") => "baseline",
        Some("after") => "after",
        _ => if contract.get("status").and_then(Value::as_str) == Some("tl_approved") { "baseline" } else { "after" },
    };

    let active_bp_id: Option<i64> = crate::db_json::query_rows(
        db,
        "SELECT id FROM blueprints WHERE contract_slug = ? AND superseded_by IS NULL ORDER BY id DESC LIMIT 1",
        &[json!(slug.clone())],
    ).ok().and_then(|rows| rows.as_array().and_then(|a| a.first()).and_then(|r| r.get("id")).and_then(Value::as_i64));
    let snip_hash: Value = match active_bp_id {
        Some(id) => crate::blueprint__helpers_snippets_hash::snippetsContentHash(db, &json!(id)).unwrap_or(Value::Null),
        None => Value::Null,
    };

    // ── SKIP #1: operator gate toggle OFF ────────────────────────────────
    let gate_off = crate::db_json::query_rows(db, "SELECT value FROM cost_settings WHERE key = 'gate_build_enabled'", &[])
        .ok().and_then(|rows| rows.as_array().and_then(|a| a.first()).and_then(|r| r.get("value")).and_then(Value::as_str).map(|s| s.to_string()))
        == Some("off".to_string());
    if gate_off {
        let evidence_md = "\u{23ed} BUILD GATE SKIPPED \u{2014} disabled by the operator (driver gate toggle).".to_string();
        write_row(db, &slug, &applied_at, &snip_hash, phase, true, true, None, Some(&evidence_md));
        return Ok(json!({ "contract_slug": slug, "passed": true, "skipped": true, "build_cmd": Value::Null, "evidence_md": evidence_md }));
    }

    // ── SKIP #2: broken baseline (AFTER phase only) ───────────────────────
    if phase == "after" {
        if let Some(bb) = broken_baseline(db, &slug) {
            let evidence_md = format!(
                "\u{23ed} BUILD GATE SKIPPED \u{2014} the baseline build (#{}) was already broken by a PRE-EXISTING workspace failure; re-running cannot judge THIS card's code. Fix the workspace in its own card.",
                str_qq(bb.get("id"))
            );
            write_row(db, &slug, &applied_at, &snip_hash, phase, true, true, None, Some(&evidence_md));
            return Ok(json!({ "contract_slug": slug, "passed": true, "skipped": true, "build_cmd": Value::Null, "evidence_md": evidence_md, "preexisting_failure": ["(baseline broken \u{2014} after gate skipped)"] }));
        }
    }

    // ── SKIP #3: no sandbox_path / worktree_path ──────────────────────────
    let sandbox_path = contract.get("sandbox_path").cloned();
    let worktree_path = contract.get("worktree_path").cloned();
    if !truthy(sandbox_path.as_ref()) && !truthy(worktree_path.as_ref()) {
        let evidence_md = "No sandbox_path / worktree_path on contract \u{2014} build gate skipped (auto-pass).".to_string();
        write_row(db, &slug, &applied_at, &snip_hash, phase, true, true, None, Some(&evidence_md));
        return Ok(json!({ "contract_slug": slug, "passed": true, "skipped": true, "build_cmd": Value::Null, "evidence_md": evidence_md }));
    }
    let root = sandbox_root(&contract).unwrap_or_default();

    // ── V2 RE-SYNC: rebuild the sandbox from CURRENT snippets (best-effort) ──
    let mut sync_note = String::new();
    if active_bp_id.is_some() {
        let apply_input = json!({ "contract_slug": slug, "target_root": root });
        if let Err(e) = crate::blueprint_blueprint_apply::blueprintApply(&apply_input, db) {
            let e_head: String = e.chars().take(300).collect();
            sync_note = format!("\u{26a0} snippet re-sync failed (gate builds the last-applied files): {e_head}\n\n");
        }
    }

    let build_cmd = resolveBuildCmd(&contract, &root, db);
    let build_cmd = match build_cmd {
        Some(c) => c,
        None => {
            let evidence_md = "No build_cmd override and no tsconfig.json / package.json build script in the sandbox \u{2014} build gate skipped (auto-pass).".to_string();
            write_row(db, &slug, &applied_at, &snip_hash, phase, true, true, None, Some(&evidence_md));
            return Ok(json!({ "contract_slug": slug, "passed": true, "skipped": true, "build_cmd": Value::Null, "evidence_md": evidence_md }));
        }
    };

    // ── AUTO-BOOTSTRAP ─────────────────────────────────────────────────────
    let bootstrap_cmd: Option<String> = match contract.get("bootstrap_cmd").and_then(Value::as_str) {
        Some(s) if !s.is_empty() => Some(s.to_string()),
        _ => {
            let has_pkg = Path::new(&root).join("package.json").exists();
            let has_nm = Path::new(&root).join("node_modules").exists();
            if has_pkg && !has_nm {
                Some(if Path::new(&root).join("package-lock.json").exists() { "npm ci --no-audit --no-fund".to_string() } else { "npm install --no-audit --no-fund".to_string() })
            } else { None }
        }
    };
    let ws_node_options: Option<String> = crate::db_json::query_rows(db, "SELECT value FROM cost_settings WHERE key = 'ws_node_options'", &[])
        .ok().and_then(|rows| rows.as_array().and_then(|a| a.first()).and_then(|r| r.get("value")).and_then(Value::as_str).map(|s| s.to_string()))
        .filter(|s| !s.is_empty());

    let mut seq: i64 = 0;
    let _ = crate::db_json::run_write(db, "DELETE FROM build_gate_live WHERE contract_slug = ?", &[json!(slug.clone())]);

    let mut bootstrap_note = String::new();
    if let Some(bc) = &bootstrap_cmd {
        let (code, out, err) = run_cmd(db, &slug, "install", &mut seq, bc, &root, ws_node_options.as_deref());
        if code != 0 {
            bootstrap_note = format!("\u{26a0} bootstrap `{bc}` exited {code}:\n{out}\n{err}\n\n");
        }
    }

    let (mut code, mut out, mut err) = run_cmd(db, &slug, "build", &mut seq, &build_cmd, &root, ws_node_options.as_deref());

    if code == 0 {
        let out_capped: String = out.chars().take(EVIDENCE_CAP).collect();
        let evidence_md = format!("{sync_note}$ {build_cmd}\n(exit 0)\n{out_capped}").trim().to_string();
        write_row(db, &slug, &applied_at, &snip_hash, phase, true, false, Some(&build_cmd), Some(&evidence_md));
        return Ok(json!({ "contract_slug": slug, "passed": true, "skipped": false, "build_cmd": build_cmd, "evidence_md": evidence_md }));
    }

    // ── DETERMINISTIC RECOVERY LADDER ──────────────────────────────────────
    let first_fail = format!("{out}\n{err}");
    let adv = crate::driver__helpers_failure_advice::adviseOnFailure(&json!(first_fail));
    let cls = adv.get("failure_class").and_then(Value::as_str).unwrap_or("").to_string();
    let first_status = code;
    let mut recovery_note = String::new();

    if cls == "openssl-legacy-webpack" {
        let (c2, out2, err2) = run_cmd(db, &slug, "build", &mut seq, &build_cmd, &root, Some("--openssl-legacy-provider"));
        if c2 == 0 {
            let tail: String = out2.chars().rev().take(1200).collect::<Vec<_>>().into_iter().rev().collect();
            let evidence_md = format!("{sync_note}$ {build_cmd}\n(first run exit {first_status} \u{2014} {cls})\n\u{21bb} RECOVERED with NODE_OPTIONS=--openssl-legacy-provider (exit 0)\nPERMANENT FIX for the workspace: upgrade react-scripts to 5, or bake NODE_OPTIONS into the build script.\n{tail}").trim().to_string();
            write_row(db, &slug, &applied_at, &snip_hash, phase, true, false, Some(&build_cmd), Some(&evidence_md));
            return Ok(json!({ "contract_slug": slug, "passed": true, "skipped": false, "build_cmd": build_cmd, "evidence_md": evidence_md, "recovered": cls }));
        }
        recovery_note = format!("\u{21bb} recovery (NODE_OPTIONS=--openssl-legacy-provider) got PAST the OpenSSL wall \u{2014} the REAL failure is below (exit {c2})\n");
        code = c2; out = out2; err = err2;
        let _ = db.execute_batch("CREATE TABLE IF NOT EXISTS cost_settings (key TEXT PRIMARY KEY, value TEXT)");
        let _ = crate::db_json::run_write(db, "INSERT INTO cost_settings (key, value) VALUES ('ws_node_options', '--openssl-legacy-provider') ON CONFLICT(key) DO UPDATE SET value = excluded.value", &[]);
    } else if cls == "missing-dependency-binary" || cls == "native-build-dep" {
        let nm = Path::new(&root).join("node_modules");
        let is_worktree = truthy(worktree_path.as_ref());
        if is_worktree && nm.exists() {
            let _ = std::fs::remove_dir_all(&nm);
            let reinstall = if Path::new(&root).join("package-lock.json").exists() { "npm ci --no-audit --no-fund" } else { "npm install --no-audit --no-fund" };
            let (ric, _, _) = run_cmd(db, &slug, "install", &mut seq, reinstall, &root, ws_node_options.as_deref());
            if ric == 0 {
                let (c3, out3, err3) = run_cmd(db, &slug, "build", &mut seq, &build_cmd, &root, ws_node_options.as_deref());
                if c3 == 0 {
                    let tail: String = out3.chars().rev().take(1200).collect::<Vec<_>>().into_iter().rev().collect();
                    let evidence_md = format!("{sync_note}$ {build_cmd}\n(first run exit {first_status} \u{2014} {cls})\n\u{21bb} RECOVERED after clean reinstall (exit 0)\n{tail}").trim().to_string();
                    write_row(db, &slug, &applied_at, &snip_hash, phase, true, false, Some(&build_cmd), Some(&evidence_md));
                    return Ok(json!({ "contract_slug": slug, "passed": true, "skipped": false, "build_cmd": build_cmd, "evidence_md": evidence_md, "recovered": cls }));
                }
                recovery_note = format!("\u{21bb} clean reinstall succeeded but the rebuild still failed (exit {c3})\n");
                code = c3; out = out3; err = err3;
            } else {
                recovery_note = format!("\u{21bb} clean reinstall itself failed (exit {ric})\n");
            }
        }
    }

    // DEV-SERVER PROBE — not ported (diagnostic-only, never affects `passed`; see module doc).
    let dev_probe_note = String::new();

    let fail_text = format!("{out}\n{err}");

    // ── PRE-EXISTING BASELINE (AFTER phase) — warn-pass ────────────────────
    if phase == "after" {
        if let Some(bb) = broken_baseline(db, &slug) {
            if bb.get("passed").and_then(Value::as_i64) == Some(0) {
                let adv2 = crate::driver__helpers_failure_advice::adviseOnFailure(&json!(fail_text));
                let cls2 = adv2.get("failure_class").and_then(Value::as_str).unwrap_or("");
                let remedy2 = adv2.get("remedy_md").and_then(Value::as_str).unwrap_or("");
                let combined = format!(
                    "\u{26a0} PRE-EXISTING BROKEN BUILD \u{2014} the baseline run (#{}, on the pristine worktree BEFORE any code) was ALREADY red. The card is not implicated; gate passes WITH WARNING. Fix the workspace build in its own card.\n\n=== HOW TO PROCEED ({cls2}) ===\n{remedy2}\n\n{sync_note}{bootstrap_note}$ {build_cmd}\n(exit {code})\n{recovery_note}--- stdout ---\n{out}\n--- stderr ---\n{err}",
                    str_qq(bb.get("id"))
                );
                let evidence_md = cap_evidence(&combined);
                write_row(db, &slug, &applied_at, &snip_hash, phase, true, false, Some(&build_cmd), Some(&evidence_md));
                return Ok(json!({ "contract_slug": slug, "passed": true, "skipped": false, "build_cmd": build_cmd, "evidence_md": evidence_md, "preexisting_failure": ["(baseline build was already red)"] }));
            }
        }
    }

    // ── PRE-EXISTING WORKSPACE FAILURE (both phases) — warn-pass ───────────
    let failing_files = extractFailingFiles(&json!(fail_text));
    let failing_files_vec: Vec<String> = failing_files.as_array().unwrap().iter().filter_map(|v| v.as_str().map(str::to_string)).collect();
    let changed: Option<Vec<String>> = match input.get("changed_files").and_then(Value::as_array) {
        Some(arr) => Some(arr.iter().filter_map(|v| v.as_str().map(str::to_string)).collect()),
        None => changedFilesInWorktree(&root),
    };
    if !failing_files_vec.is_empty() {
        if let Some(changed) = &changed {
            let all_outside = failing_files_vec.iter().all(|f| !changed.iter().any(|c| c.ends_with(f.as_str()) || f.ends_with(c.as_str())));
            if all_outside {
                let banner = format!("\u{26a0} PRE-EXISTING WORKSPACE FAILURE \u{2014} the build fails on file(s) this card NEVER touched: {}. The card's own change is not implicated; gate passes WITH WARNING. Fix the workspace (see HOW TO PROCEED below) in its own card.", failing_files_vec.join(", "));
                let adv3 = crate::driver__helpers_failure_advice::adviseOnFailure(&json!(fail_text));
                let cls3 = adv3.get("failure_class").and_then(Value::as_str).unwrap_or("");
                let remedy3 = adv3.get("remedy_md").and_then(Value::as_str).unwrap_or("");
                let combined = format!("{banner}\n\n=== HOW TO PROCEED ({cls3}) ===\n{remedy3}\n\n{sync_note}{bootstrap_note}$ {build_cmd}\n(exit {code})\n{recovery_note}{dev_probe_note}--- stdout ---\n{out}\n--- stderr ---\n{err}");
                let evidence_md = cap_evidence(&combined);
                write_row(db, &slug, &applied_at, &snip_hash, phase, true, false, Some(&build_cmd), Some(&evidence_md));
                let card_slug = crate::db_json::query_rows(db, "SELECT card_slug FROM contracts WHERE slug = ?", &[json!(slug.clone())])
                    .ok().and_then(|rows| rows.as_array().and_then(|a| a.first()).and_then(|r| r.get("card_slug")).cloned()).unwrap_or(Value::Null);
                let remedy_head: String = remedy3.chars().take(300).collect();
                let _ = crate::db_json::run_write(
                    db,
                    "INSERT INTO notifications (kind, severity, source, message, related_card_slug) VALUES ('build-preexisting-failure', 'warn', 'dev-build-gate', ?, ?)",
                    &[json!(format!("build fails on PRE-EXISTING workspace file(s) {} (class {cls3}) \u{2014} the card passed with warning; file a workspace-fix card. Remedy: {remedy_head}", failing_files_vec.join(", "))), card_slug],
                );
                return Ok(json!({ "contract_slug": slug, "passed": true, "skipped": false, "build_cmd": build_cmd, "evidence_md": evidence_md, "preexisting_failure": failing_files_vec }));
            }
        }
    }

    // ── RED ──────────────────────────────────────────────────────────────
    let combined = format!("{sync_note}{bootstrap_note}$ {build_cmd}\n(exit {code})\n{recovery_note}{dev_probe_note}--- stdout ---\n{out}\n--- stderr ---\n{err}");
    let evidence_md = cap_evidence(&combined);
    write_row(db, &slug, &applied_at, &snip_hash, phase, false, false, Some(&build_cmd), Some(&evidence_md));
    Ok(json!({ "contract_slug": slug, "passed": false, "skipped": false, "build_cmd": build_cmd, "evidence_md": evidence_md }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE contracts (id INTEGER PRIMARY KEY AUTOINCREMENT, slug TEXT NOT NULL UNIQUE, card_slug TEXT, title TEXT NOT NULL DEFAULT 't', status TEXT NOT NULL DEFAULT 'proposed', build_cmd TEXT, bootstrap_cmd TEXT, sandbox_path TEXT, worktree_path TEXT, updated_at TEXT);
             CREATE TABLE build_gate_runs (id INTEGER PRIMARY KEY AUTOINCREMENT, contract_slug TEXT NOT NULL, passed INTEGER NOT NULL, skipped INTEGER NOT NULL DEFAULT 0, build_cmd TEXT, evidence_md TEXT, applied_at TEXT, snippets_hash TEXT, duration_ms INTEGER, phase TEXT NOT NULL DEFAULT 'after', ts TEXT NOT NULL DEFAULT '2026-01-01 00:00:00');
             CREATE TABLE build_gate_live (id INTEGER PRIMARY KEY AUTOINCREMENT, contract_slug TEXT, phase TEXT, seq INTEGER, chunk TEXT, done INTEGER);
             CREATE TABLE blueprints (id INTEGER PRIMARY KEY AUTOINCREMENT, contract_slug TEXT NOT NULL, superseded_by INTEGER, version INTEGER, blueprint_json TEXT);
             CREATE TABLE blueprint_snippets (id INTEGER PRIMARY KEY AUTOINCREMENT, blueprint_id INTEGER, block_id TEXT, code TEXT);
             CREATE TABLE workspaces (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL UNIQUE, root_path TEXT NOT NULL, vcs TEXT NOT NULL DEFAULT 'git', is_active INTEGER NOT NULL DEFAULT 0, created_at TEXT NOT NULL DEFAULT '2026-01-01 00:00:00');
             CREATE TABLE cost_settings (key TEXT PRIMARY KEY, value TEXT);
             CREATE TABLE notifications (id INTEGER PRIMARY KEY AUTOINCREMENT, kind TEXT, severity TEXT, source TEXT, message TEXT, related_card_slug TEXT)",
        ).unwrap();
        c
    }
    fn seed_contract(c: &Connection, slug: &str, status: &str, sandbox_path: Option<&str>, worktree_path: Option<&str>) {
        c.execute(
            "INSERT INTO contracts (slug, status, sandbox_path, worktree_path) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![slug, status, sandbox_path, worktree_path],
        ).unwrap();
    }
    const MISSING: &str = concat!(env!("ANTILOKI_FIXTURES"), "/dev-build-gate-missing");

    #[test]
    fn gate_toggle_off_skips_without_running() {
        let c = db();
        seed_contract(&c, "BG-OFF", "impl", None, Some(MISSING));
        c.execute("INSERT INTO cost_settings (key, value) VALUES ('gate_build_enabled', 'off')", []).unwrap();
        let r = devBuildGate(&json!({ "contract_slug": "BG-OFF" }), &c).unwrap();
        assert_eq!(r["passed"], json!(true));
        assert_eq!(r["skipped"], json!(true));
        assert!(r["evidence_md"].as_str().unwrap().contains("disabled by the operator"));
        let row_passed: i64 = c.query_row("SELECT passed FROM build_gate_runs WHERE contract_slug='BG-OFF'", [], |row| row.get(0)).unwrap();
        assert_eq!(row_passed, 1);
    }

    #[test]
    fn broken_baseline_via_passed_zero_skips_after_phase() {
        let c = db();
        seed_contract(&c, "BG-BB", "impl", None, Some(MISSING));
        c.execute("INSERT INTO build_gate_runs (contract_slug, passed, skipped, phase) VALUES ('BG-BB', 0, 0, 'baseline')", []).unwrap();
        let r = devBuildGate(&json!({ "contract_slug": "BG-BB" }), &c).unwrap();
        assert_eq!(r["passed"], json!(true));
        assert_eq!(r["skipped"], json!(true));
        assert!(r["preexisting_failure"].is_array());
        assert!(r["evidence_md"].as_str().unwrap().contains("already broken"));
    }

    #[test]
    fn broken_baseline_via_preexisting_evidence_text() {
        let c = db();
        seed_contract(&c, "BG-BB2", "impl", None, Some(MISSING));
        c.execute("INSERT INTO build_gate_runs (contract_slug, passed, skipped, evidence_md, phase) VALUES ('BG-BB2', 1, 0, 'ran \u{2014} PRE-EXISTING workspace failure noted', 'baseline')", []).unwrap();
        let r = devBuildGate(&json!({ "contract_slug": "BG-BB2" }), &c).unwrap();
        assert_eq!(r["skipped"], json!(true));
    }

    #[test]
    fn no_sandbox_or_worktree_skips() {
        let c = db();
        seed_contract(&c, "BG-NOROOT", "impl", None, None);
        let r = devBuildGate(&json!({ "contract_slug": "BG-NOROOT" }), &c).unwrap();
        assert_eq!(r["skipped"], json!(true));
        assert!(r["evidence_md"].as_str().unwrap().contains("No sandbox_path"));
    }

    #[test]
    fn no_buildcmd_resolvable_skips() {
        let c = db();
        seed_contract(&c, "BG-NOBUILD", "impl", None, Some(MISSING));
        let r = devBuildGate(&json!({ "contract_slug": "BG-NOBUILD" }), &c).unwrap();
        assert_eq!(r["skipped"], json!(true));
        assert!(r["evidence_md"].as_str().unwrap().contains("No build_cmd override"));
    }

    #[test]
    fn phase_auto_derives_baseline_from_tl_approved_status() {
        let c = db();
        seed_contract(&c, "BG-BASELINE", "tl_approved", None, Some(MISSING));
        let r = devBuildGate(&json!({ "contract_slug": "BG-BASELINE" }), &c).unwrap();
        assert_eq!(r["skipped"], json!(true)); // no build_cmd resolvable on a nonexistent root
        let phase: String = c.query_row("SELECT phase FROM build_gate_runs WHERE contract_slug='BG-BASELINE'", [], |row| row.get(0)).unwrap();
        assert_eq!(phase, "baseline");
    }

    #[test]
    fn build_cmd_override_wins_and_real_subprocess_runs_green() {
        let dir = std::env::temp_dir().join(format!("antiloki-bg-rs-green-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let c = db();
        c.execute(
            "INSERT INTO contracts (slug, status, worktree_path, build_cmd) VALUES ('BG-GREEN', 'impl', ?1, ?2)",
            rusqlite::params![dir.to_str().unwrap(), if cfg!(windows) { "exit 0" } else { "true" }],
        ).unwrap();
        let r = devBuildGate(&json!({ "contract_slug": "BG-GREEN" }), &c).unwrap();
        assert_eq!(r["passed"], json!(true));
        assert_eq!(r["skipped"], json!(false));
        assert!(r["evidence_md"].as_str().unwrap().contains("(exit 0)"));
        let row_passed: i64 = c.query_row("SELECT passed FROM build_gate_runs WHERE contract_slug='BG-GREEN'", [], |row| row.get(0)).unwrap();
        assert_eq!(row_passed, 1);
        let live_rows: i64 = c.query_row("SELECT COUNT(*) FROM build_gate_live WHERE contract_slug='BG-GREEN'", [], |row| row.get(0)).unwrap();
        assert!(live_rows > 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn real_build_failure_with_no_changed_files_input_and_no_git_repo_stays_red() {
        let dir = std::env::temp_dir().join(format!("antiloki-bg-rs-red-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let c = db();
        c.execute(
            "INSERT INTO contracts (slug, status, worktree_path, build_cmd) VALUES ('BG-RED', 'impl', ?1, ?2)",
            rusqlite::params![dir.to_str().unwrap(), if cfg!(windows) { "exit 1" } else { "false" }],
        ).unwrap();
        // no `changed_files` override and `dir` is not a git repo → changedFilesInWorktree = None →
        // the pre-existing-failure arbiter cannot fire → stays genuinely RED.
        let r = devBuildGate(&json!({ "contract_slug": "BG-RED" }), &c).unwrap();
        assert_eq!(r["passed"], json!(false));
        assert_eq!(r["skipped"], json!(false));
        assert!(r["evidence_md"].as_str().unwrap().contains("(exit 1)"));
        let row_passed: i64 = c.query_row("SELECT passed FROM build_gate_runs WHERE contract_slug='BG-RED'", [], |row| row.get(0)).unwrap();
        assert_eq!(row_passed, 0);
        assert!(c.query_row::<String, _, _>("SELECT evidence_md FROM build_gate_runs WHERE contract_slug='BG-RED'", [], |row| row.get(0)).unwrap().contains("HOW TO PROCEED"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn preexisting_failure_outside_changeset_warns_and_passes_with_notification() {
        let dir = std::env::temp_dir().join(format!("antiloki-bg-rs-preexist-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let c = db();
        let script = "echo ./src/legacy/wall.js && exit 1";
        c.execute(
            "INSERT INTO contracts (slug, card_slug, status, worktree_path, build_cmd) VALUES ('BG-PRE', 'CARD-PRE', 'impl', ?1, ?2)",
            rusqlite::params![dir.to_str().unwrap(), script],
        ).unwrap();
        let r = devBuildGate(&json!({ "contract_slug": "BG-PRE", "changed_files": ["src/app/main.js"] }), &c).unwrap();
        assert_eq!(r["passed"], json!(true));
        assert_eq!(r["skipped"], json!(false));
        assert_eq!(r["preexisting_failure"], json!(["src/legacy/wall.js"]));
        assert!(r["evidence_md"].as_str().unwrap().contains("PRE-EXISTING WORKSPACE FAILURE"));
        let notif_count: i64 = c.query_row("SELECT COUNT(*) FROM notifications WHERE related_card_slug='CARD-PRE'", [], |row| row.get(0)).unwrap();
        assert_eq!(notif_count, 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn preexisting_failure_inside_changeset_stays_red() {
        let dir = std::env::temp_dir().join(format!("antiloki-bg-rs-inside-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let c = db();
        let script = "echo ./src/app/main.js && exit 1";
        c.execute(
            "INSERT INTO contracts (slug, status, worktree_path, build_cmd) VALUES ('BG-IN', 'impl', ?1, ?2)",
            rusqlite::params![dir.to_str().unwrap(), script],
        ).unwrap();
        let r = devBuildGate(&json!({ "contract_slug": "BG-IN", "changed_files": ["src/app/main.js"] }), &c).unwrap();
        assert_eq!(r["passed"], json!(false));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn guard_throws_are_not_swallowed() {
        let c = db();
        assert_eq!(devBuildGate(&json!({}), &c).unwrap_err(), "dev-build-gate: contract_slug is required");
        assert_eq!(devBuildGate(&json!("NOPE"), &c).unwrap_err(), "dev-build-gate: contract 'NOPE' not found");
    }

    #[test]
    fn extract_failing_files_dedups_and_orders_by_pattern() {
        let evidence = "./src/foo.js\n./src/foo.js\nModule not found: Error: Can't resolve './x' in 'C:\\proj\\src\\bar'\n";
        let files = extractFailingFiles(&json!(evidence));
        let arr: Vec<&str> = files.as_array().unwrap().iter().map(|v| v.as_str().unwrap()).collect();
        assert_eq!(arr, vec!["src/foo.js", "src/bar"]);
    }

    #[test]
    fn extract_failing_files_empty_evidence_returns_empty_array() {
        assert_eq!(extractFailingFiles(&json!("")), json!([]));
        assert_eq!(extractFailingFiles(&json!(Value::Null)), json!([]));
    }

    #[test]
    fn resolve_build_cmd_prefers_contract_override_over_registry_and_detection() {
        let c = db();
        let contract = json!({ "build_cmd": "  make build  " });
        assert_eq!(resolveBuildCmd(&contract, "/nonexistent", &c), Some("make build".to_string()));
    }

    #[test]
    fn resolve_build_cmd_falls_back_to_tsconfig_then_package_json_then_none() {
        let c = db();
        let dir = std::env::temp_dir().join(format!("antiloki-bg-rs-resolve-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let contract = json!({});
        assert_eq!(resolveBuildCmd(&contract, dir.to_str().unwrap(), &c), None);
        std::fs::write(dir.join("tsconfig.json"), "{}").unwrap();
        assert_eq!(resolveBuildCmd(&contract, dir.to_str().unwrap(), &c), Some("npx tsc --noEmit".to_string()));
        std::fs::remove_file(dir.join("tsconfig.json")).unwrap();
        std::fs::write(dir.join("package.json"), r#"{"scripts":{"build":"webpack"}}"#).unwrap();
        assert_eq!(resolveBuildCmd(&contract, dir.to_str().unwrap(), &c), Some("npm run build".to_string()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn changed_files_in_worktree_returns_none_for_non_git_directory() {
        let dir = std::env::temp_dir().join(format!("antiloki-bg-rs-nogit-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(changedFilesInWorktree(dir.to_str().unwrap()), None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
