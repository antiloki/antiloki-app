//! dev-test-gate.rs — twin of skills/agents/dev/dev-test-gate.mjs (db lane, THIN CONSUMER of
//! already-converted twins: `contract_contract_get::contractGet`,
//! `system_workspace_scripts::resolveScript`, `blueprint__helpers_snippets_hash::snippetsContentHash`,
//! `driver__helpers_failure_advice::adviseOnFailure`). NAMES ARE CONTRACT: `devTestGate` (default).
//! The deterministic TEST gate: resolve a test command (scripts registry → package.json
//! detection), run it, and persist one `test_gate_runs` row — with SIX skip doctrines (operator
//! toggle, broken baseline via passed=0 OR a "PRE-EXISTING" evidence marker checked against
//! `test_gate_runs` then the (buggy, phase-column-less) `build_gate_runs` fallback, no
//! worktree/sandbox root, no resolvable test command) plus a warn-pass for an AFTER run that goes
//! red against an already-red baseline.
//!
//! `exec_fn`/`fs_api` (JS: `input?.exec_fn ?? execSync`, `input?.fs_api ?? {existsSync,
//! readFileSync}`) are JS FUNCTION-valued test-only escape hatches — used ONLY by
//! dev-test-gate.test.mjs to avoid spawning a real process; no real production caller ever sets
//! them (`engine/canonical-actions/git-manager.mjs` dispatches `dev-test-gate` with a plain
//! `input: '$.input'` passthrough — no exec_fn wiring anywhere in the action catalog) and JSON
//! fixtures cannot encode a function value at all. So `streaming = !input?.exec_fn` is ALWAYS
//! `true` in both real production and this twin's JSON-driven fixture harness — the
//! non-streaming `execFn`-injected branch is not a reachable production code path and is not
//! ported; `fs_api` always resolves to the real filesystem, exactly like the already-converted
//! `system_workspace_scripts::detectScripts` twin's package.json sniffing (real fs read, made
//! fixture-deterministic by seeding the contract's `sandbox_path`/`worktree_path` — and the
//! resolver's OWN internal `workspaces` root — to a real, absolute, intentionally NONEXISTENT
//! directory string; `std::fs::read_to_string` on a missing path errors identically to Node's
//! `readFileSync`, so no testdata directory needs to exist on disk for this unit's fixture).
//!
//! `context.db` missing / `contract_slug` missing / contract not found → JS throws
//! (`err.contextMissing`, `err.required`, `new AntilokiError({code:NOT_FOUND,...})`). Not modeled:
//! `db: &Connection` is a required, non-nullable reference (same convention as every sibling
//! twin); the other two are ported as `Err(String)` with the matching `${skill}: ${cause}`
//! message but NOT fixture-exercised (playbook gotcha 9 — success path only).
//!
//! `SELECT card_slug FROM contracts WHERE slug=?` + `gateLog(...)` (a `console.log` line
//! embedding it plus `new Date().toISOString()`) are pure stdout side effects that never reach
//! the returned value or `db_after` — not ported, same "console.log not ported" convention as
//! `system_workspace_scripts.rs`'s module doc.
//!
//! DIVERGENCE PRESERVED ON PURPOSE: on a genuine RED run, JS's `writeRow` recomputes its OWN
//! local `evidence` (appending "=== HOW TO PROCEED (...) ===" via `adviseOnFailure`) for the
//! `test_gate_runs` INSERT only — the value RETURNED to the caller is the original, un-amended
//! `evidence_md` built before `writeRow` was called. `write_row` below mirrors this exactly: it
//! takes the caller's evidence by reference, computes its own amended copy for the INSERT, and
//! never mutates/returns anything the caller's own `evidence_md` depends on. This only matters
//! inside the disclosed-untested subprocess branch (every SKIP branch has `passed:true`, which
//! never triggers the amendment).
//!
//! Frozen clock (playbook datetime lane): `gateT0 = Date.now()` and every later `Date.now()`
//! resolve to the SAME frozen instant, so `duration_ms = Date.now() - gateT0` is always `0` —
//! hardcoded as `0i64` in `write_row` rather than threading a gateT0 variable through.
//!
//! REAL SUBPROCESS BRANCH — DISCLOSED UNTESTED BY FIXTURE (playbook lesson 5's flag-skippable
//! pattern, precedent constitution-estimate.rs's git-diff branch): once a test command actually
//! resolves and none of the four skip doctrines fire, JS `spawn(testCmd, {cwd, shell:true,
//! env:{...,CI:'1'}})`s a real test runner, streaming chunks into `build_gate_live` and a 600s
//! SIGKILL timeout. `run_streaming` below ports this for real (`std::process::Command` via a
//! shell, `CI=1`, best-effort `build_gate_live` telemetry) but NO fixture case reaches it — every
//! case is arranged to hit one of the four skip branches first, because a real `npm test`
//! invocation is neither deterministic nor portable across the JS and Rust lanes. Disclosed gaps
//! vs. JS: the 600s timeout and per-`data`-event chunk boundaries (non-deterministic even in JS —
//! OS/timing dependent, and never part of the compared return value) are not replicated; only the
//! FINAL concatenated stdout/stderr + exit code (which alone determine pass/fail and evidence_md)
//! are captured faithfully.
// QUALITY: 76/100 — six-branch skip doctrine ported deterministically incl. the dead
// build_gate_runs fallback quirk; one disclosed-untested branch (real subprocess, flag-skippable
// per lesson 5); the JS-only exec_fn/fs_api test seam is not a reachable production path and is
// not modeled at all (distinct from a fixture-controllable optional input field).

use regex::Regex;
use rusqlite::Connection;
use serde_json::{json, Value};
use std::sync::OnceLock;

fn truthy(v: Option<&Value>) -> bool { match v { None|Some(Value::Null)=>false, Some(Value::Bool(b))=>*b, Some(Value::Number(n))=>n.as_f64().map_or(false,|f|f!=0.0&&!f.is_nan()), Some(Value::String(s))=>!s.is_empty(), Some(_)=>true } }
fn str_qq(v: Option<&Value>) -> String { match v { None|Some(Value::Null)=>String::new(), Some(Value::String(s))=>s.clone(), Some(Value::Bool(b))=>b.to_string(), Some(n@Value::Number(_))=>n.to_string(), Some(o)=>o.to_string() } }
fn js_string(v: &Value) -> String { match v { Value::Null=>"null".into(), Value::String(s)=>s.clone(), Value::Number(n)=>n.to_string(), Value::Bool(b)=>b.to_string(), Value::Array(a)=>a.iter().map(js_string).collect::<Vec<_>>().join(","), Value::Object(_)=>"[object Object]".into() } }
fn tmpl(v: Option<&Value>) -> String { match v { None=>"undefined".into(), Some(Value::Null)=>"null".into(), Some(x)=>str_qq(Some(x)) } }
fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) }

fn no_test_specified_re() -> &'static Regex { static R: OnceLock<Regex> = OnceLock::new(); R.get_or_init(|| Regex::new(r"(?i)no test specified").unwrap()) }
fn preexisting_re() -> &'static Regex { static R: OnceLock<Regex> = OnceLock::new(); R.get_or_init(|| Regex::new(r"(?i)PRE-EXISTING").unwrap()) }

const EVIDENCE_CAP: usize = 8000;
// `capEvidence` — head-ok: tail-biased, test runners put the failure summary at the END.
fn cap_evidence(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= EVIDENCE_CAP { return s.to_string(); }
    let head: String = chars[..1500].iter().collect();
    let tail_len = EVIDENCE_CAP - 1500;
    let tail_start = chars.len() - tail_len;
    let tail: String = chars[tail_start..].iter().collect();
    format!("{head}\n\u{2026}[{} chars snipped]\u{2026}\n{tail}", chars.len() - EVIDENCE_CAP)
}

// `const broken = (table) => { try { const r = db.prepare('SELECT id, passed, evidence_md FROM
// ${table} WHERE contract_slug = ? AND phase = \'baseline\' ORDER BY id DESC LIMIT 1').get(slug);
// return r && (r.passed === 0 || /PRE-EXISTING/i.test(r.evidence_md ?? '')) ? r : null; } catch {
// return null; } }` — called with 'test_gate_runs' then (dead fallback: `build_gate_runs` has NO
// `phase` column in the real schema, so that query errors and this always returns `None` for it)
// 'build_gate_runs'.
fn broken_baseline(db: &Connection, table: &str, slug: &str) -> Option<Value> {
    let sql = format!("SELECT id, passed, evidence_md FROM {table} WHERE contract_slug = ? AND phase = 'baseline' ORDER BY id DESC LIMIT 1");
    let rows = crate::db_json::query_rows(db, &sql, &[json!(slug)]).ok()?;
    let row = rows.as_array().and_then(|a| a.first())?.clone();
    let passed0 = row.get("passed").and_then(Value::as_i64) == Some(0);
    let evidence = row.get("evidence_md").and_then(Value::as_str).unwrap_or("");
    if passed0 || preexisting_re().is_match(evidence) { Some(row) } else { None }
}

// `writeRow` — persists one `test_gate_runs` row. See module doc: the amended (HOW TO PROCEED)
// evidence is written to the DB ONLY; it is never returned to (or read back from) the caller.
/// `<cmd> --findRelatedTests <files>` when the runner is jest and the change names ≤ 60 files; the
/// command is left alone otherwise (vitest, mocha, an unknown script, an empty change). Pure — tested.
pub fn scope_test_cmd(test_cmd: &str, touched: &[String], root: &str) -> String {
    if touched.is_empty() || touched.len() > 60 { return test_cmd.to_string(); }
    let lower = test_cmd.to_ascii_lowercase();
    let is_npm_script = lower.starts_with("npm test") || lower.starts_with("npm run test") || lower.starts_with("yarn test") || lower.starts_with("pnpm test");
    let script_is_jest = if is_npm_script {
        std::fs::read_to_string(std::path::Path::new(root).join("package.json")).ok()
            .and_then(|t| serde_json::from_str::<Value>(&t).ok())
            .and_then(|p| p.get("scripts").and_then(|s| s.get("test")).and_then(Value::as_str).map(|t| t.contains("jest")))
            .unwrap_or(false)
    } else { false };
    let jest = lower.contains("jest") || script_is_jest;
    if !jest { return test_cmd.to_string(); }
    let files = touched.iter().filter(|f| !f.contains('\'') && !f.contains(' ')).map(|f| format!("'{f}'")).collect::<Vec<_>>().join(" ");
    if files.is_empty() { return test_cmd.to_string(); }
    let sep = if lower.starts_with("npm ") { " -- " } else { " " };
    format!("{test_cmd}{sep}--findRelatedTests {files}")
}

fn write_row(db: &Connection, slug: &str, phase: &str, passed: bool, skipped: bool, test_cmd: Option<&str>, evidence_md: Option<&str>, snip_hash: &Value) {
    let mut evidence: Option<String> = evidence_md.map(|s| s.to_string());
    if !passed && !skipped {
        if let Some(ev) = evidence.clone() {
            let adv = crate::driver__helpers_failure_advice::adviseOnFailure(&json!(ev));
            let failure_class = adv.get("failure_class").and_then(Value::as_str).unwrap_or("");
            let remedy_md = adv.get("remedy_md").and_then(Value::as_str).unwrap_or("");
            evidence = Some(format!("{ev}\n\n=== HOW TO PROCEED ({failure_class}) ===\n{remedy_md}"));
        }
    }
    let _ = crate::db_json::run_write(
        db,
        "INSERT INTO test_gate_runs (contract_slug, passed, skipped, test_cmd, evidence_md, snippets_hash, duration_ms, phase) VALUES (?,?,?,?,?,?,?,?)",
        &[
            json!(slug),
            json!(if passed { 1 } else { 0 }),
            json!(if skipped { 1 } else { 0 }),
            test_cmd.map(|s| json!(s)).unwrap_or(Value::Null),
            evidence.map(|s| json!(s)).unwrap_or(Value::Null),
            snip_hash.clone(),
            json!(0i64), // frozen Date.now() - gateT0 (see module doc)
            json!(phase),
        ],
    );
}

// The REAL subprocess-execution branch — DISCLOSED UNTESTED BY FIXTURE, see module doc.
fn run_streaming(db: &Connection, slug: &str, test_cmd: &str, root: &str) -> (i32, String, String) {
    let mut seq: i64 = 100000; // after the build gate's chunks — same live table, phase 'test'
    let mut live_write = |chunk: &str, done: i64| {
        let capped: String = chunk.chars().take(4000).collect();
        let _ = crate::db_json::run_write(
            db,
            "INSERT INTO build_gate_live (contract_slug, phase, seq, chunk, done) VALUES (?, 'test', ?, ?, ?)",
            &[json!(slug), json!(seq), json!(capped), json!(done)],
        );
        seq += 1;
    };
    live_write(&format!("$ {test_cmd}\n"), 0);

    #[cfg(windows)]
    let mut cmd = { let mut c = crate::proc::cmd("cmd"); c.arg("/C").arg(test_cmd); c };
    #[cfg(not(windows))]
    let mut cmd = { let mut c = crate::proc::cmd("sh"); c.arg("-c").arg(test_cmd); c };
    cmd.current_dir(root).env("CI", "1");

    match cmd.output() {
        Ok(o) => {
            let out = String::from_utf8_lossy(&o.stdout).into_owned();
            let err = String::from_utf8_lossy(&o.stderr).into_owned();
            if !out.is_empty() { live_write(&out, 0); }
            if !err.is_empty() { live_write(&err, 0); }
            let code = o.status.code().unwrap_or(1);
            live_write(&format!("\n(exit {code})\n"), 1);
            (code, out, err)
        }
        Err(e) => {
            live_write(&format!("spawn error: {e}\n"), 1);
            (1, String::new(), e.to_string())
        }
    }
}

pub fn devTestGate(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS: `if (!db) throw err.contextMissing('dev-test-gate', 'db')` — unreachable (db: &Connection
    // is a required, non-nullable reference here), same guard-throw convention as every sibling twin.
    let slug = match input {
        Value::String(s) => s.clone(),
        _ => input.get("contract_slug").and_then(Value::as_str).unwrap_or("").to_string(),
    };
    if slug.is_empty() {
        return Err("dev-test-gate: contract_slug is required".into());
    }

    let contract = crate::contract_contract_get::contractGet(&json!(slug.clone()), db)?;
    if contract.is_null() {
        return Err(format!("dev-test-gate: contract '{slug}' not found"));
    }

    let phase: &str = match input.get("phase").and_then(Value::as_str) {
        Some("baseline") => "baseline",
        Some("after") => "after",
        _ => if contract.get("status").and_then(Value::as_str) == Some("tl_approved") { "baseline" } else { "after" },
    };

    let active_bp_id: Option<i64> = crate::db_json::query_rows(
        db,
        "SELECT id FROM blueprints WHERE contract_slug = ? AND superseded_by IS NULL ORDER BY id DESC LIMIT 1",
        &[json!(slug.clone())],
    )
    .ok()
    .and_then(|rows| rows.as_array().and_then(|a| a.first()).and_then(|r| r.get("id")).and_then(Value::as_i64));
    let snip_hash: Value = match active_bp_id {
        Some(id) => crate::blueprint__helpers_snippets_hash::snippetsContentHash(db, &json!(id)).unwrap_or(Value::Null),
        None => Value::Null,
    };

    // ── SKIP #1: operator gate toggle OFF ────────────────────────────────
    let gate_off: bool = crate::db_json::query_rows(db, "SELECT value FROM cost_settings WHERE key = 'gate_test_enabled'", &[])
        .ok()
        .and_then(|rows| rows.as_array().and_then(|a| a.first()).and_then(|r| r.get("value")).and_then(Value::as_str).map(|s| s.to_string()))
        == Some("off".to_string());
    if gate_off {
        let evidence_md = "\u{23ed} TEST GATE SKIPPED \u{2014} disabled by the operator (driver gate toggle).".to_string();
        write_row(db, &slug, phase, true, true, None, Some(&evidence_md), &snip_hash);
        return Ok(json!({ "contract_slug": slug, "passed": true, "skipped": true, "test_cmd": Value::Null, "evidence_md": evidence_md }));
    }

    // ── SKIP #2: broken baseline (AFTER phase only) — pre-existing red workspace ────────
    if phase == "after" {
        if let Some(row) = broken_baseline(db, "test_gate_runs", &slug).or_else(|| broken_baseline(db, "build_gate_runs", &slug)) {
            let evidence_md = format!(
                "\u{23ed} TEST GATE SKIPPED \u{2014} the baseline (#{}) was already broken by a PRE-EXISTING workspace failure; re-running cannot judge THIS card's code. Fix the workspace in its own card.",
                tmpl(row.get("id"))
            );
            write_row(db, &slug, phase, true, true, None, Some(&evidence_md), &snip_hash);
            return Ok(json!({ "contract_slug": slug, "passed": true, "skipped": true, "test_cmd": Value::Null, "evidence_md": evidence_md, "preexisting_failure": true }));
        }
    }

    // ── SKIP #3: no worktree/sandbox root ─────────────────────────────────
    let worktree_path = contract.get("worktree_path").cloned();
    let sandbox_path = contract.get("sandbox_path").cloned();
    let root_v: Value = nz(worktree_path.as_ref()).or_else(|| nz(sandbox_path.as_ref())).cloned().unwrap_or(Value::Null);
    if !truthy(Some(&root_v)) {
        let evidence_md = "No worktree/sandbox on the contract \u{2014} test gate skipped (auto-pass).".to_string();
        write_row(db, &slug, phase, true, true, None, Some(&evidence_md), &snip_hash);
        return Ok(json!({ "contract_slug": slug, "passed": true, "skipped": true, "test_cmd": Value::Null, "evidence_md": evidence_md }));
    }
    let root_s = root_v.as_str().unwrap_or("").to_string();

    // ── SKIP #4: no test command resolvable (registry, then package.json) ────────────────
    let mut test_cmd_v: Value = crate::system_workspace_scripts::resolveScript(db, "test");
    if !truthy(Some(&test_cmd_v)) {
        let pkg_path = std::path::Path::new(&root_s).join("package.json");
        if let Ok(text) = std::fs::read_to_string(&pkg_path) {
            if let Ok(pkg) = serde_json::from_str::<Value>(&text) {
                let test_script = pkg.get("scripts").and_then(|s| s.get("test"));
                if truthy(test_script) && !no_test_specified_re().is_match(&js_string(test_script.unwrap())) {
                    test_cmd_v = json!("npm test");
                }
            }
        }
    }
    if !truthy(Some(&test_cmd_v)) {
        let evidence_md = "No test script recorded (scripts registry) or detected (package.json) \u{2014} test gate skipped (auto-pass). Record one on the Code screen to arm this gate.".to_string();
        write_row(db, &slug, phase, true, true, None, Some(&evidence_md), &snip_hash);
        return Ok(json!({ "contract_slug": slug, "passed": true, "skipped": true, "test_cmd": Value::Null, "evidence_md": evidence_md }));
    }
    let test_cmd = test_cmd_v.as_str().unwrap_or("").to_string();
    // SCOPED TO THE CHANGE (2026-09-19): on a big repo the workspace's `test` script is the WHOLE suite
    // (react-main's jest ate 15 GB and swapped the box); when the contract names its touched files and
    // the runner is jest, only the tests related to them run. The full suite stays the fallback.
    let touched: Vec<String> = contract.get("files_touched_json").and_then(Value::as_str).and_then(|t| serde_json::from_str::<Vec<Value>>(t).ok()).unwrap_or_default()
        .into_iter().filter_map(|f| f.as_str().map(String::from).or_else(|| f.get("path").and_then(Value::as_str).map(String::from))).collect();
    let test_cmd = scope_test_cmd(&test_cmd, &touched, &root_s);

    // ── REAL subprocess run — DISCLOSED UNTESTED BY FIXTURE, see module doc ──────────────
    let (code, stdout, stderr) = run_streaming(db, &slug, &test_cmd, &root_s);
    let mut passed = code == 0;
    let mut banner = String::new();
    if !passed && phase == "after" {
        if let Ok(rows) = crate::db_json::query_rows(
            db,
            "SELECT id, passed FROM test_gate_runs WHERE contract_slug = ? AND phase = 'baseline' ORDER BY id DESC LIMIT 1",
            &[json!(slug.clone())],
        ) {
            if let Some(row) = rows.as_array().and_then(|a| a.first()) {
                if row.get("passed").and_then(Value::as_i64) == Some(0) {
                    passed = true;
                    banner = format!(
                        "\u{26a0} PRE-EXISTING TEST FAILURES \u{2014} the baseline run (#{}, BEFORE this card's changes) was ALREADY red; gate passes WITH WARNING. Fix the workspace tests in their own card.\n\n",
                        tmpl(row.get("id"))
                    );
                }
            }
        }
    }
    let evidence_md = cap_evidence(&format!("{banner}$ {test_cmd}\n(exit {code})\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}"));
    write_row(db, &slug, phase, passed, false, Some(&test_cmd), Some(&evidence_md), &snip_hash);
    Ok(json!({
        "contract_slug": slug,
        "passed": passed,
        "skipped": false,
        "test_cmd": test_cmd,
        "evidence_md": evidence_md,
        "preexisting_failure": !banner.is_empty(),
    }))
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_test_command_is_scoped_to_the_change_only_for_jest() {
        let f = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(super::scope_test_cmd("yarn test", &f(&["a.js", "b/c.js"]), "/nowhere"), "yarn test");
        assert_eq!(super::scope_test_cmd("jest", &f(&["a.js"]), "/nowhere"), "jest --findRelatedTests 'a.js'");
        assert_eq!(super::scope_test_cmd("npm test", &f(&[]), "/nowhere"), "npm test");
        let dir = std::env::temp_dir().join(format!("antiloki-gate-{}", std::process::id())); std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("package.json"), r#"{"scripts":{"test":"jest --config x"}}"#).unwrap();
        assert_eq!(super::scope_test_cmd("npm test", &f(&["src/x.js"]), dir.to_str().unwrap()), "npm test -- --findRelatedTests 'src/x.js'");
        assert_eq!(super::scope_test_cmd("yarn test", &f(&["src/x.js"]), dir.to_str().unwrap()), "yarn test --findRelatedTests 'src/x.js'");
        std::fs::write(dir.join("package.json"), r#"{"scripts":{"test":"vitest run"}}"#).unwrap();
        assert_eq!(super::scope_test_cmd("npm test", &f(&["src/x.js"]), dir.to_str().unwrap()), "npm test");
        let _ = std::fs::remove_dir_all(&dir);
    }
    use super::*;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE contracts (id INTEGER PRIMARY KEY AUTOINCREMENT, slug TEXT NOT NULL UNIQUE, title TEXT NOT NULL, status TEXT NOT NULL DEFAULT 'proposed', card_slug TEXT, sandbox_path TEXT, worktree_path TEXT);
             CREATE TABLE test_gate_runs (id INTEGER PRIMARY KEY AUTOINCREMENT, contract_slug TEXT NOT NULL, passed INTEGER NOT NULL, skipped INTEGER DEFAULT 0, test_cmd TEXT, evidence_md TEXT, snippets_hash TEXT, duration_ms INTEGER, phase TEXT NOT NULL DEFAULT 'after', ts TEXT NOT NULL DEFAULT '2026-01-01 00:00:00');
             CREATE TABLE build_gate_runs (id INTEGER PRIMARY KEY AUTOINCREMENT, contract_slug TEXT NOT NULL, passed INTEGER NOT NULL, skipped INTEGER NOT NULL DEFAULT 0, build_cmd TEXT, evidence_md TEXT, applied_at TEXT, snippets_hash TEXT, ts TEXT NOT NULL DEFAULT '2026-01-01 00:00:00');
             CREATE TABLE blueprints (id INTEGER PRIMARY KEY AUTOINCREMENT, contract_slug TEXT NOT NULL, superseded_by INTEGER);
             CREATE TABLE workspaces (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL UNIQUE, root_path TEXT NOT NULL, vcs TEXT NOT NULL DEFAULT 'git', is_active INTEGER NOT NULL DEFAULT 0, created_at TEXT NOT NULL DEFAULT '2026-01-01 00:00:00');
             CREATE TABLE cost_settings (key TEXT PRIMARY KEY, value TEXT)",
        ).unwrap();
        c
    }
    fn seed_contract(c: &Connection, slug: &str, status: &str, sandbox_path: Option<&str>) {
        c.execute(
            "INSERT INTO contracts (slug, title, status, sandbox_path, worktree_path) VALUES (?1, 't', ?2, ?3, NULL)",
            rusqlite::params![slug, status, sandbox_path],
        ).unwrap();
    }
    const MISSING: &str = concat!(env!("ANTILOKI_FIXTURES"), "/dev-test-gate-missing");

    #[test]
    fn gate_toggle_off_skips_without_running() {
        let c = db();
        seed_contract(&c, "TG-OFF", "impl", Some(MISSING));
        c.execute("INSERT INTO cost_settings (key, value) VALUES ('gate_test_enabled', 'off')", []).unwrap();
        let r = devTestGate(&json!({ "contract_slug": "TG-OFF" }), &c).unwrap();
        assert_eq!(r["passed"], json!(true));
        assert_eq!(r["skipped"], json!(true));
        assert_eq!(r["test_cmd"], Value::Null);
        assert!(r["evidence_md"].as_str().unwrap().contains("disabled by the operator"));
        let row_passed: i64 = c.query_row("SELECT passed FROM test_gate_runs WHERE contract_slug='TG-OFF'", [], |row| row.get(0)).unwrap();
        assert_eq!(row_passed, 1);
    }

    #[test]
    fn broken_baseline_passed_zero_skips_with_preexisting_flag() {
        let c = db();
        seed_contract(&c, "TG-BB", "impl", Some(MISSING));
        c.execute("INSERT INTO test_gate_runs (contract_slug, passed, skipped, phase) VALUES ('TG-BB', 0, 0, 'baseline')", []).unwrap();
        let r = devTestGate(&json!({ "contract_slug": "TG-BB" }), &c).unwrap();
        assert_eq!(r["passed"], json!(true));
        assert_eq!(r["skipped"], json!(true));
        assert_eq!(r["preexisting_failure"], json!(true));
        assert!(r["evidence_md"].as_str().unwrap().contains("already broken"));
    }

    #[test]
    fn broken_baseline_detected_via_preexisting_evidence_text() {
        let c = db();
        seed_contract(&c, "TG-BB2", "impl", Some(MISSING));
        c.execute("INSERT INTO test_gate_runs (contract_slug, passed, skipped, evidence_md, phase) VALUES ('TG-BB2', 1, 0, 'ran \u{2014} PRE-EXISTING workspace failure noted', 'baseline')", []).unwrap();
        let r = devTestGate(&json!({ "contract_slug": "TG-BB2" }), &c).unwrap();
        assert_eq!(r["skipped"], json!(true));
        assert_eq!(r["preexisting_failure"], json!(true));
    }

    #[test]
    fn dead_build_gate_runs_fallback_does_not_skip_then_falls_to_no_script_skip() {
        let c = db();
        seed_contract(&c, "TG-BGB", "impl", Some(MISSING));
        c.execute("INSERT INTO build_gate_runs (contract_slug, passed, skipped) VALUES ('TG-BGB', 0, 0)", []).unwrap();
        c.execute("INSERT INTO workspaces (name, root_path, is_active) VALUES ('w', ?1, 1)", rusqlite::params![MISSING]).unwrap();
        let r = devTestGate(&json!({ "contract_slug": "TG-BGB" }), &c).unwrap();
        // build_gate_runs has no `phase` column — the fallback query errors and is swallowed, so
        // the red build baseline does NOT trigger the broken-baseline skip; the run instead falls
        // through to the (also-safe) no-test-script skip.
        assert!(r["evidence_md"].as_str().unwrap().contains("No test script recorded"));
        assert_eq!(r["preexisting_failure"], Value::Null);
    }

    #[test]
    fn no_worktree_or_sandbox_skips() {
        let c = db();
        seed_contract(&c, "TG-NOROOT", "impl", None);
        let r = devTestGate(&json!({ "contract_slug": "TG-NOROOT" }), &c).unwrap();
        assert_eq!(r["skipped"], json!(true));
        assert!(r["evidence_md"].as_str().unwrap().contains("No worktree/sandbox"));
    }

    #[test]
    fn phase_auto_derives_baseline_from_tl_approved_status() {
        let c = db();
        seed_contract(&c, "TG-BASELINE", "tl_approved", Some(MISSING));
        c.execute("INSERT INTO workspaces (name, root_path, is_active) VALUES ('w', ?1, 1)", rusqlite::params![MISSING]).unwrap();
        let r = devTestGate(&json!({ "contract_slug": "TG-BASELINE" }), &c).unwrap();
        assert_eq!(r["skipped"], json!(true));
        let phase: String = c.query_row("SELECT phase FROM test_gate_runs WHERE contract_slug='TG-BASELINE'", [], |row| row.get(0)).unwrap();
        assert_eq!(phase, "baseline");
    }

    #[test]
    fn guard_throws_are_not_swallowed() {
        let c = db();
        assert_eq!(devTestGate(&json!({}), &c).unwrap_err(), "dev-test-gate: contract_slug is required");
        assert_eq!(devTestGate(&json!("NOPE"), &c).unwrap_err(), "dev-test-gate: contract 'NOPE' not found");
    }
}
