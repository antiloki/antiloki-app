//! tl-sandbox-create.rs — twin of skills/agents/tl/tl-sandbox-create.mjs (BESPOKE-NATIVE).
//! NAMES ARE CONTRACT: `tlSandboxCreate` (default).
//!
//! TL creates the copy-on-write sandbox for a `tl_approved` contract: copies every file listed
//! in `contract.files_touched` from the (DB-derived) workspace root into
//! `SANDBOX_ROOT/<contract-slug>/<relpath>`, capturing a sha256 of each original at copy-time and
//! persisting both `sandbox_path` + `original_shas_json` on the contract row. Refuses to
//! overwrite an existing sandbox (caller must `qa-sandbox-reset` first).
//!
//! BESPOKE-NATIVE — NOT fixture-testable through the conform-db two-lane harness (no frontier
//! entry / fixture written for this unit; arms/frontier left empty in the manifest for this
//! file). Two real filesystem roots are involved and only ONE of them is fixture-controllable:
//!   - READ side: originals resolve against `getWorkspaceRoot(db)` — DB-derived
//!     (`workspaces.root_path`), same technique as stack-facts.rs / skill-library-materialize.rs.
//!   - WRITE side: sandbox copies land under `SANDBOX_ROOT` = roots.mjs's `SANDBOX_DIR` =
//!     `join(SUBSTRATE_ROOT, '.antiloki', 'sandbox')` — a module-level CONSTANT computed once at
//!     JS import time, honoring `ANTILOKI_SUBSTRATE_ROOT` only if the env var is set BEFORE the
//!     module first loads. conform-db.mjs's JS lane never sets this env var (see `runDbJsLane` —
//!     no env mutation anywhere in that file) and its persistent process imports every unit's
//!     module once, so a conform-db run of this skill would write REAL files into whatever
//!     `SANDBOX_ROOT` the harness process's own real environment resolves to — an unacceptable
//!     side effect for an automated conformance sweep, and exactly the "hardcoded root" case the
//!     crossing playbook calls out as bespoke-native. (Same class as qa-sandbox-reset.rs /
//!     orphan-sandbox-dirs.rs / god-driver-halt.rs — all touch this same SANDBOX/HOME/RUN root
//!     family and are all bespoke-native for the identical reason.)
//! Proven below via `#[cfg(test)]` native tests: `ANTILOKI_SUBSTRATE_ROOT` + a real tempdir
//! (`with_substrate_root()` — the `with_home()` pattern from god-driver-halt.rs /
//! orphan-sandbox-dirs.rs, applied to the sibling env var; see sandbox-resolve-path.rs's
//! `substrate_root()` for the identical non-test resolution logic) for the WRITE side, and a
//! seeded active `workspaces` row pointing at a real on-disk tempdir for the READ side
//! (stack-facts.rs precedent). This skill's own JS test suite (tl-sandbox-create.test.mjs) does
//! the same real-fs thing against the actual checkout's `.antiloki/sandbox/`, confirming this is
//! genuine, intentional real-fs behavior, not an oversight.
//!
//! `context.dryRun`: JS destructures `dryRun` off **context**, not input
//! (`const { db, dryRun = false } = context ?? {}`) — the ONE call-time knob beyond `db` this
//! skill's context carries. Modeled as a third `dry_run: bool` parameter (db-reset.rs's
//! `archive_dir_override` precedent for a genuine context-sourced knob that doesn't fit the
//! standard 2-arg `(input, db)` shape); never dispatched centrally anyway since this unit carries
//! no frontier/arms entry.
//!
//! Cross-module (already-converted twins, reused not reimplemented): `contractGet`
//! (`contract_contract_get::contractGet`), `getWorkspaceRoot`
//! (`engine_workspace_root::getWorkspaceRoot`), `logDecision` (`engine_log_decision::logDecision`).
//!
//! `datetime('now')` (contracts.updated_at) is left UNFROZEN — same qa-sandbox-reset.rs
//! precedent: this unit has no JS-lane counterpart run under the frozen-clock harness to match
//! against, so the honest-for-production translation is the real SQLite clock.
//!
//! sha256: `readFileSync` + `createHash('sha256').update(buf).digest('hex')` → `Sha256::digest` +
//! `hex::encode` (codebase-file-read.rs precedent). `total_bytes` accumulates the SAME buffer's
//! `len()` rather than issuing JS's separate `statSync(...).size` call — behaviorally identical
//! for any file whose content is stable across the two reads (always true in practice/tests), and
//! avoids a redundant syscall.
//!
//! Path validation (`typeof relpath !== 'string' || relpath.includes('..') ||
//! relpath.startsWith('/') || relpath.match(/^[A-Z]:/i)`) ported literally, including the
//! over-inclusive `includes('..')` (rejects e.g. `"foo..bar.txt"`, not just traversal segments —
//! intentional, matches JS exactly). `resolve(root, relpath)` / `join(sandboxAbs, relpath)`
//! ported as `node_join` (qa-sandbox-reset.rs / sandbox-resolve-path.rs precedent — PathBuf::push
//! alone keeps an embedded `/` literal in one component on Windows, diverging from node:path).
//!
//! Throws (CONTEXT_MISSING / NOT_FOUND / VALIDATION_LOGIC / PARSE_ERROR) reproduce the JS
//! AntilokiError message text (`"tl-sandbox-create: <cause>"`) for audit fidelity, matching the
//! project's other bespoke-native twins; `context.db` missing is unreachable here (Rust's
//! `db: &Connection` is a required, non-nullable reference — same convention as every sibling).

use rusqlite::Connection;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

fn truthy(v: Option<&Value>) -> bool { match v { None|Some(Value::Null)=>false, Some(Value::Bool(b))=>*b, Some(Value::Number(n))=>n.as_f64().map_or(false,|f|f!=0.0&&!f.is_nan()), Some(Value::String(s))=>!s.is_empty(), Some(_)=>true } }
fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) }
fn tmpl(v: Option<&Value>) -> String { match v { None=>"undefined".into(), Some(Value::Null)=>"null".into(), Some(Value::String(s))=>s.clone(), Some(Value::Bool(b))=>b.to_string(), Some(n@Value::Number(_))=>n.to_string(), Some(o)=>o.to_string() } }

// node:path `resolve(base, rel)` / `join(base, rel)` — normalizes `/` and `\` uniformly, drops
// `.` components. `rel` here is always already validated (no `..`, not absolute).
fn node_join(base: &Path, rel: &str) -> PathBuf {
    let mut p = base.to_path_buf();
    for part in rel.split(['/', '\\']) {
        if part.is_empty() || part == "." { continue; }
        p.push(part);
    }
    p
}

// JS `/^[A-Z]:/i.test(relpath)` — bare drive-letter prefix.
fn starts_with_drive_letter(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':'
}

// roots.mjs: SUBSTRATE_ROOT = resolve($ANTILOKI_SUBSTRATE_ROOT) if set+truthy, else
// resolve(ENGINE_ROOT, '..') (antiloki-core's parent). SANDBOX_DIR =
// join(SUBSTRATE_ROOT, '.antiloki', 'sandbox'). CARGO_MANIFEST_DIR (antiloki-core-rust) shares
// the same parent as antiloki-core on this checkout layout — sandbox-resolve-path.rs precedent.
fn substrate_root() -> PathBuf {
    if let Ok(v) = std::env::var("ANTILOKI_SUBSTRATE_ROOT") {
        if !v.is_empty() { return PathBuf::from(v); }
    }
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().expect("antiloki-core-rust has a parent dir").to_path_buf()
}
fn sandbox_dir() -> PathBuf { substrate_root().join(".antiloki").join("sandbox") }

pub fn tlSandboxCreate(input: &Value, db: &Connection, dry_run: bool) -> Result<Value, String> {
    // JS: `const { db } = context ?? {}; if (!db) throw err.contextMissing(...)` — unreachable
    // here (Rust's `db: &Connection` is a required, non-nullable reference).

    // JS: `typeof input === 'string' ? input : input?.contract_slug ?? input?.slug`
    let slug_v: Value = match input {
        Value::String(s) => Value::String(s.clone()),
        _ => nz(input.get("contract_slug")).or_else(|| nz(input.get("slug"))).cloned().unwrap_or(Value::Null),
    };
    if !truthy(Some(&slug_v)) {
        return Err("tl-sandbox-create: contract_slug is required".into());
    }
    let slug = slug_v.as_str().unwrap_or("").to_string();

    let contract = crate::contract_contract_get::contractGet(&slug_v, db)?;
    if contract.is_null() {
        return Err(format!("tl-sandbox-create: contract '{slug}' not found"));
    }
    let status = contract.get("status").and_then(Value::as_str).unwrap_or("");
    if status != "tl_approved" {
        return Err(format!(
            "tl-sandbox-create: contract '{slug}' is in status='{status}', expected 'tl_approved'"
        ));
    }
    let existing_sandbox_path = contract.get("sandbox_path").cloned().unwrap_or(Value::Null);
    if truthy(Some(&existing_sandbox_path)) {
        let existing = existing_sandbox_path.as_str().unwrap_or("");
        return Err(format!(
            "tl-sandbox-create: contract '{slug}' already has sandbox at '{existing}'. Run qa-sandbox-reset first if you want a fresh sandbox."
        ));
    }

    // JS: `Array.isArray(contract.files_touched) ? contract.files_touched
    //      : (typeof contract.files_touched_json === 'string' ? JSON.parse(...) : [])`
    let files_touched = contract.get("files_touched").cloned().unwrap_or(Value::Null);
    let files_value: Value = if files_touched.is_array() {
        files_touched
    } else {
        match contract.get("files_touched_json") {
            Some(Value::String(s)) => serde_json::from_str::<Value>(s).map_err(|e| {
                format!("tl-sandbox-create: contract '{slug}' files_touched_json is invalid JSON: {e}")
            })?,
            _ => json!([]),
        }
    };
    let files: &Vec<Value> = match files_value.as_array() {
        Some(a) => a,
        None => return Err(format!("tl-sandbox-create: contract '{slug}' files_touched is not an array")),
    };

    let workspace_root_v = crate::engine_workspace_root::getWorkspaceRoot(db);
    let workspace_root = workspace_root_v.as_str().unwrap_or("");
    let sandbox_path = format!(".antiloki/sandbox/{slug}");
    let sandbox_abs = node_join(&sandbox_dir(), &slug);

    let mut shas = Map::new();
    let mut copied: i64 = 0;
    let mut new_files: i64 = 0;
    let mut total_bytes: i64 = 0;

    if !dry_run {
        std::fs::create_dir_all(&sandbox_abs).map_err(|e| e.to_string())?;
    }

    for item in files {
        let invalid = match item.as_str() {
            None => true,
            Some(s) => s.contains("..") || s.starts_with('/') || starts_with_drive_letter(s),
        };
        if invalid {
            return Err(format!(
                "tl-sandbox-create: invalid relpath in files_touched: '{}' (must be repo-relative, no ..)",
                tmpl(Some(item))
            ));
        }
        let relpath = item.as_str().unwrap();

        let original_abs = node_join(Path::new(workspace_root), relpath);
        let sandbox_file_abs = node_join(&sandbox_abs, relpath);

        if original_abs.exists() {
            let buf = std::fs::read(&original_abs).map_err(|e| e.to_string())?;
            let sha = hex::encode(Sha256::digest(&buf));
            total_bytes += buf.len() as i64;
            shas.insert(relpath.to_string(), json!(sha));
            if !dry_run {
                if let Some(parent) = sandbox_file_abs.parent() {
                    std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
                }
                std::fs::copy(&original_abs, &sandbox_file_abs).map_err(|e| e.to_string())?;
            }
            copied += 1;
        } else {
            shas.insert(relpath.to_string(), Value::Null);
            if !dry_run {
                if let Some(parent) = sandbox_file_abs.parent() {
                    std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
                }
            }
            new_files += 1;
        }
    }

    if !dry_run {
        let shas_json = serde_json::to_string(&Value::Object(shas.clone())).map_err(|e| e.to_string())?;
        crate::db_json::run_write(
            db,
            "UPDATE contracts SET sandbox_path = ?, original_shas_json = ?, updated_at = datetime('now') WHERE slug = ?",
            &[json!(sandbox_path), json!(shas_json), json!(slug)],
        )?;

        let rationale =
            format!("created sandbox at {sandbox_path}; copied={copied} new={new_files} bytes={total_bytes}");
        let mut inputs = Map::new();
        inputs.insert("contract_slug".into(), json!(slug));
        inputs.insert("sandbox_path".into(), json!(sandbox_path));
        inputs.insert("files_copied".into(), json!(copied));
        inputs.insert("files_new".into(), json!(new_files));
        inputs.insert("files_touched_count".into(), json!(shas.len() as i64));
        let log_input = json!({
            "agent": "tl",
            "decision": "sandbox_create",
            "card_slug": slug,
            "rationale": rationale,
            "inputs": Value::Object(inputs),
        });
        let _ = crate::engine_log_decision::logDecision(&log_input, db);
    }

    let mut out = Map::new();
    out.insert("contract_slug".into(), json!(slug));
    out.insert("sandbox_path".into(), json!(sandbox_path));
    out.insert("sandbox_abs".into(), json!(sandbox_abs.to_string_lossy().into_owned()));
    out.insert("files_copied".into(), json!(copied));
    out.insert("files_new".into(), json!(new_files));
    out.insert("total_bytes".into(), json!(total_bytes));
    out.insert("shas".into(), Value::Object(shas));
    out.insert("dry_run".into(), json!(dry_run));
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    // SAFETY: ANTILOKI_SUBSTRATE_ROOT is process-global; serialize via the shared crate-wide lock
    // (test_env_lock.rs) — a local mutex would not prevent a race with OTHER files' tests over the
    // same real env var (e.g. sandbox-resolve-path.rs / workspace-activate.rs also read it).
    fn with_substrate_root<T>(root: &str, f: impl FnOnce() -> T) -> T {
        let _guard = crate::test_env_lock::ENV_LOCK.lock().unwrap();
        unsafe {
            std::env::set_var("ANTILOKI_SUBSTRATE_ROOT", root);
        }
        let result = f();
        unsafe {
            std::env::remove_var("ANTILOKI_SUBSTRATE_ROOT");
        }
        result
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!("antiloki-tl-sandbox-create-rs-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS brain").unwrap();
        c.execute_batch(
            "CREATE TABLE contracts (slug TEXT PRIMARY KEY, status TEXT, files_touched_json TEXT, \
               sandbox_path TEXT, original_shas_json TEXT, updated_at TEXT);
             CREATE TABLE workspaces (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL UNIQUE, \
               root_path TEXT NOT NULL, vcs TEXT NOT NULL DEFAULT 'git', is_active INTEGER NOT NULL DEFAULT 0, \
               created_at TEXT NOT NULL DEFAULT '2026-01-01 00:00:00');
             CREATE TABLE brain.brain_decisions ( id INTEGER PRIMARY KEY AUTOINCREMENT, session_id TEXT NOT NULL, \
               decided_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')), card_slug TEXT, \
               decision TEXT NOT NULL, rationale TEXT NOT NULL, inputs_json TEXT, fire_id INTEGER, \
               CHECK (decision IN ('fire','wait','rollback','escalate','done','pause','session_start','session_end', \
               'goal_set','classify','gate_verdict','rule_match','sandbox_create','review_pass', \
               'schedule_and_dispatch','agent_step')) );",
        )
        .unwrap();
        c
    }

    fn seed_contract(c: &Connection, slug: &str, status: &str, files: &[&str], sandbox_path: Option<&str>) {
        let files_json = serde_json::to_string(files).unwrap();
        c.execute(
            "INSERT INTO contracts (slug, status, files_touched_json, sandbox_path) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![slug, status, files_json, sandbox_path],
        )
        .unwrap();
    }

    fn seed_active_workspace(c: &Connection, root_path: &str) {
        c.execute(
            "INSERT INTO workspaces (name, root_path, vcs, is_active, created_at) VALUES ('self', ?1, 'git', 1, '2026-01-01 00:00:00')",
            rusqlite::params![root_path],
        )
        .unwrap();
    }

    #[test]
    fn missing_slug_errs() {
        let c = db();
        let e = tlSandboxCreate(&json!({}), &c, false).unwrap_err();
        assert!(e.contains("contract_slug"), "{e}");
    }

    #[test]
    fn contract_not_found_errs() {
        let c = db();
        let e = tlSandboxCreate(&json!({ "contract_slug": "NOPE" }), &c, false).unwrap_err();
        assert!(e.contains("not found"), "{e}");
    }

    #[test]
    fn wrong_status_errs() {
        let c = db();
        seed_contract(&c, "WRONG-STATUS", "proposed", &[], None);
        let e = tlSandboxCreate(&json!({ "contract_slug": "WRONG-STATUS" }), &c, false).unwrap_err();
        assert!(e.contains("tl_approved"), "{e}");
    }

    #[test]
    fn already_has_sandbox_errs() {
        let c = db();
        seed_contract(&c, "HAS-SB", "tl_approved", &[], Some(".antiloki/sandbox/HAS-SB"));
        let e = tlSandboxCreate(&json!({ "contract_slug": "HAS-SB" }), &c, false).unwrap_err();
        assert!(e.contains("already has sandbox"), "{e}");
    }

    #[test]
    fn rejects_path_traversal_absolute_and_drive_letter() {
        let c = db();
        seed_contract(&c, "EVIL-1", "tl_approved", &["../../etc/passwd"], None);
        let e = tlSandboxCreate(&json!({ "contract_slug": "EVIL-1" }), &c, true).unwrap_err();
        assert!(e.contains("must be repo-relative"), "{e}");

        let c2 = db();
        seed_contract(&c2, "EVIL-2", "tl_approved", &["/etc/passwd"], None);
        let e2 = tlSandboxCreate(&json!({ "contract_slug": "EVIL-2" }), &c2, true).unwrap_err();
        assert!(e2.contains("must be repo-relative"), "{e2}");

        let c3 = db();
        seed_contract(&c3, "EVIL-3", "tl_approved", &["C:/Windows/system32"], None);
        let e3 = tlSandboxCreate(&json!({ "contract_slug": "EVIL-3" }), &c3, true).unwrap_err();
        assert!(e3.contains("must be repo-relative"), "{e3}");
    }

    #[test]
    fn non_string_relpath_uses_js_string_coercion_in_error() {
        let c = db();
        c.execute(
            "INSERT INTO contracts (slug, status, files_touched_json) VALUES ('BAD-TYPE', 'tl_approved', '[5]')",
            [],
        )
        .unwrap();
        let e = tlSandboxCreate(&json!({ "contract_slug": "BAD-TYPE" }), &c, true).unwrap_err();
        assert!(e.contains("'5'"), "{e}");
    }

    #[test]
    fn dry_run_reports_plan_without_writing() {
        let src_dir = temp_dir("dryrun-src");
        fs::write(src_dir.join("package.json"), "{\"name\":\"x\"}").unwrap();
        let root_dir = temp_dir("dryrun-root");

        let c = db();
        seed_active_workspace(&c, src_dir.to_str().unwrap());
        seed_contract(&c, "SANDBOX-DRYRUN", "tl_approved", &["package.json", "NONEXISTENT-FOR-TEST.txt"], None);

        let r = with_substrate_root(root_dir.to_str().unwrap(), || {
            tlSandboxCreate(&json!({ "contract_slug": "SANDBOX-DRYRUN" }), &c, true).unwrap()
        });
        assert_eq!(r["dry_run"], json!(true));
        assert_eq!(r["files_copied"], json!(1), "package.json exists and is counted as copied");
        assert_eq!(r["files_new"], json!(1), "NONEXISTENT file is counted as new");

        let sp: Option<String> = c
            .query_row("SELECT sandbox_path FROM contracts WHERE slug='SANDBOX-DRYRUN'", [], |row| row.get(0))
            .unwrap();
        assert_eq!(sp, None, "sandbox path NOT persisted in dry run");

        let sandbox_check = root_dir.join(".antiloki").join("sandbox").join("SANDBOX-DRYRUN");
        assert!(!sandbox_check.exists(), "no folder created on disk");

        let _ = fs::remove_dir_all(&src_dir);
        let _ = fs::remove_dir_all(&root_dir);
    }

    #[test]
    fn copies_existing_files_captures_sha256_and_persists() {
        let src_dir = temp_dir("t1-src");
        let content = "{\"name\":\"pkg\",\"version\":\"1.0.0\"}";
        fs::write(src_dir.join("package.json"), content).unwrap();
        let root_dir = temp_dir("t1-root");

        let c = db();
        seed_active_workspace(&c, src_dir.to_str().unwrap());
        seed_contract(&c, "SANDBOX-T1", "tl_approved", &["package.json"], None);

        let r = with_substrate_root(root_dir.to_str().unwrap(), || {
            tlSandboxCreate(&json!({ "contract_slug": "SANDBOX-T1" }), &c, false).unwrap()
        });

        assert_eq!(r["files_copied"], json!(1));
        assert_eq!(r["files_new"], json!(0));
        assert_eq!(r["sandbox_path"], json!(".antiloki/sandbox/SANDBOX-T1"));
        let keys: Vec<&String> = r.as_object().unwrap().keys().collect();
        assert_eq!(
            keys,
            vec![
                "contract_slug", "sandbox_path", "sandbox_abs", "files_copied", "files_new",
                "total_bytes", "shas", "dry_run",
            ]
        );
        let sha = r["shas"]["package.json"].as_str().unwrap();
        assert_eq!(sha.len(), 64, "sha256 hex should be captured");
        assert!(sha.chars().all(|ch| ch.is_ascii_hexdigit()));

        let sandbox_file = root_dir.join(".antiloki").join("sandbox").join("SANDBOX-T1").join("package.json");
        assert!(sandbox_file.exists(), "sandboxed package.json should exist");
        assert_eq!(fs::read_to_string(&sandbox_file).unwrap(), content, "content matches original");

        let (sandbox_path, shas_json): (String, String) = c
            .query_row(
                "SELECT sandbox_path, original_shas_json FROM contracts WHERE slug='SANDBOX-T1'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(sandbox_path, ".antiloki/sandbox/SANDBOX-T1");
        let shas: Value = serde_json::from_str(&shas_json).unwrap();
        assert!(shas["package.json"].as_str().is_some());

        let (decision_count, rationale): (i64, String) = c
            .query_row(
                "SELECT COUNT(*), MAX(rationale) FROM brain.brain_decisions WHERE decision='sandbox_create'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(decision_count, 1, "sandbox_create decision logged");
        assert!(rationale.contains("copied=1") && rationale.contains("new=0"), "{rationale}");

        let _ = fs::remove_dir_all(&src_dir);
        let _ = fs::remove_dir_all(&root_dir);
    }

    #[test]
    fn refuses_to_overwrite_existing_sandbox() {
        let src_dir = temp_dir("t2-src");
        fs::write(src_dir.join("package.json"), "{}").unwrap();
        let root_dir = temp_dir("t2-root");

        let c = db();
        seed_active_workspace(&c, src_dir.to_str().unwrap());
        seed_contract(&c, "SANDBOX-T2", "tl_approved", &["package.json"], None);

        with_substrate_root(root_dir.to_str().unwrap(), || {
            tlSandboxCreate(&json!({ "contract_slug": "SANDBOX-T2" }), &c, false).unwrap();
        });
        let e = with_substrate_root(root_dir.to_str().unwrap(), || {
            tlSandboxCreate(&json!({ "contract_slug": "SANDBOX-T2" }), &c, false).unwrap_err()
        });
        assert!(e.contains("already has sandbox"), "{e}");

        let _ = fs::remove_dir_all(&src_dir);
        let _ = fs::remove_dir_all(&root_dir);
    }

    #[test]
    fn records_null_sha_for_files_that_do_not_exist_yet() {
        let src_dir = temp_dir("t3-src");
        let root_dir = temp_dir("t3-root");

        let c = db();
        seed_active_workspace(&c, src_dir.to_str().unwrap());
        seed_contract(&c, "SANDBOX-T3", "tl_approved", &["THIS-DOES-NOT-EXIST.test-file"], None);

        let r = with_substrate_root(root_dir.to_str().unwrap(), || {
            tlSandboxCreate(&json!({ "contract_slug": "SANDBOX-T3" }), &c, false).unwrap()
        });
        assert_eq!(r["files_new"], json!(1));
        assert_eq!(r["files_copied"], json!(0));
        assert_eq!(r["shas"]["THIS-DOES-NOT-EXIST.test-file"], Value::Null);

        let sandbox_parent = root_dir.join(".antiloki").join("sandbox").join("SANDBOX-T3");
        assert!(sandbox_parent.exists(), "sandbox parent dir should exist so DEV can write later");

        let _ = fs::remove_dir_all(&src_dir);
        let _ = fs::remove_dir_all(&root_dir);
    }

    #[test]
    fn accepts_bare_string_input() {
        let c = db();
        seed_contract(&c, "ALIAS-STR", "tl_approved", &[], None);
        let r = tlSandboxCreate(&json!("ALIAS-STR"), &c, true).unwrap();
        assert_eq!(r["contract_slug"], json!("ALIAS-STR"));
        assert_eq!(r["dry_run"], json!(true));
    }
}
