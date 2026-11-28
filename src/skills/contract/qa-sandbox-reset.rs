//! qa-sandbox-reset.rs — twin of skills/contract/qa-sandbox-reset.mjs (db lane). NAMES ARE
//! CONTRACT: `qaSandboxReset` (default).
//!
//! Wipes a contract's sandbox folder off the REAL filesystem (recursive byte count, then
//! recursive delete) and clears `contracts.sandbox_path`. Idempotent: no contract / no sandbox
//! both return `{reset:false, ...}` no-ops. Optionally records a `correction` memory when a
//! `reason` is given and the contract has a `card_slug` to back-link.
//!
//! BESPOKE-NATIVE — NOT fixture-testable through the conform-db two-lane harness (no
//! real-fs-mutation assertion there; same precedent as orphan-sandbox-dirs.rs `fix()` and
//! god-driver-halt.rs). Unlike a pure-read fs skill (stack-facts.rs, where the committed testdata
//! is only ever READ so it survives repeated lane runs), this unit both READS real byte sizes off
//! disk (`folder_bytes`, mirroring JS `folderBytes`'s readdirSync/statSync walk) AND recursively
//! DELETES the directory (`rmSync` → `remove_dir_all`) — a destructive side effect a two-lane
//! byte-comparison harness cannot verify or safely replay against committed testdata (the first
//! lane to run would delete the fixture out from under the second). No conform-db fixture /
//! frontier entry is written for this unit; it is proven below via `#[cfg(test)]` native tests.
//!
//! `REPO_ROOT`: unlike sibling twins whose JS source imports roots.mjs's `SUBSTRATE_ROOT` (which
//! honors `ANTILOKI_SUBSTRATE_ROOT` — see sandbox-resolve-path.rs's `substrate_root()`),
//! qa-sandbox-reset.mjs computes its OWN inline `REPO_ROOT = resolve(HERE, '..','..','..')` from
//! the module's own file location, with NO env override. This twin mirrors that exactly via
//! `CARGO_MANIFEST_DIR`'s parent — no env check — so, like the JS test suite
//! (qa-sandbox-reset.test.mjs), the native tests below necessarily write/delete real files under
//! THIS checkout's `.antiloki/.test-scratch/` (there is no injectable root to redirect them to a
//! tempdir), using dedicated `qa-sandbox-reset-rs-*` slugs that never collide with production
//! sandbox slugs or the JS suite's own scratch names, with cleanup at the end of every test.
//!
//! `node_join` (resolve/join normalizing `/` and `\` uniformly, dropping `.` components) copied
//! verbatim from sandbox-resolve-path.rs's precedent — `contract.sandbox_path` is a trusted DB
//! column (repo-relative per the JS docstring), so the traversal-hardening applied to a
//! caller-supplied relpath elsewhere is not needed here.
//!
//! `rmSync(path, { recursive: true, force: true })`: `force` means a missing path does not
//! throw. `std::fs::remove_dir_all` DOES error `NotFound` for a missing path — mapped to a
//! successful no-op, matching orphan-sandbox-dirs.rs `fix()`'s exact precedent. This twin also
//! re-checks `.exists()` immediately before removing, mirroring the JS structure of two separate
//! existsSync checks (one inside `folderBytes`, one guarding the `rmSync` call).
//!
//! `context.actor` default `'agent:qa'`: per knowledge-document-add.rs / keeper-memory-capture-
//! correction.rs precedent, the db-lane harness context carries only `db`, so `actor` is always
//! its JS default here too (no caller in this codebase overrides it either —
//! card-rewind-to-stage.mjs calls `qaSandboxReset({...}, { db })` with no `actor`).
//!
//! `datetime('now')` (contracts.updated_at) is left UNFROZEN (real SQLite `now()`), unlike the
//! frozen-clock convention used by conform-db-fixtured twins: this unit has no JS-lane
//! counterpart run to match against, so the honest-for-production translation is the real clock.
//!
//! Cross-module: `contractGet` (already converted, `contract_contract_get::contractGet`).

use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};

fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}
fn nz(v: Option<&Value>) -> Option<&Value> {
    v.filter(|x| !x.is_null())
} // JS `??` left-operand filter

const ACTOR: &str = "agent:qa";

// node:path `resolve(base, rel)`/`join(base, rel)` — normalizes `/` and `\` uniformly and drops
// `.` components. sandbox-resolve-path.rs precedent: PathBuf::push alone keeps an embedded `/`
// literal in one component on Windows, diverging from node:path.
fn node_join(base: &Path, rel: &str) -> PathBuf {
    let mut p = base.to_path_buf();
    for part in rel.split(['/', '\\']) {
        if part.is_empty() || part == "." {
            continue;
        }
        p.push(part);
    }
    p
}

// JS: `const HERE = dirname(fileURLToPath(import.meta.url)); const REPO_ROOT = resolve(HERE,
// '..', '..', '..');` — HERE is antiloki-core/skills/contract, three levels up is this
// checkout's antiloki-core parent. CARGO_MANIFEST_DIR here is antiloki-core-rust; its parent is
// that same directory on this checkout layout (source-list.rs / sandbox-resolve-path.rs
// precedent). Deliberately NO env override — see module doc.
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("antiloki-core-rust has a parent dir")
        .to_path_buf()
}

// JS `folderBytes`: existsSync guard, then an explicit stack-based walk (not recursion) summing
// `st.size` for files, descending into directories. A mid-walk stat/read error is swallowed here
// (skip the entry) rather than propagated — this unit is proven by native tests, not a
// byte-for-byte JS comparison, so favoring robustness over reproducing a JS crash is safe.
fn folder_bytes(root_abs: &Path) -> u64 {
    if !root_abs.exists() {
        return 0;
    }
    let mut total: u64 = 0;
    let mut stack: Vec<PathBuf> = vec![root_abs.to_path_buf()];
    while let Some(p) = stack.pop() {
        let st = match std::fs::metadata(&p) {
            Ok(m) => m,
            Err(_) => continue,
        };
        if st.is_file() {
            total += st.len();
        } else if st.is_dir() {
            if let Ok(rd) = std::fs::read_dir(&p) {
                for entry in rd.flatten() {
                    stack.push(entry.path());
                }
            }
        }
    }
    total
}

pub fn qaSandboxReset(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS: `const { db } = context ?? {}; if (!db) throw err.contextMissing(...)` — unreachable in
    // the db lane (a connection is always injected), mirrors contract-get.rs.

    // JS: `typeof input === 'string' ? input : input?.contract_slug ?? input?.slug`
    let slug_v: Value = match input {
        Value::String(s) => Value::String(s.clone()),
        _ => nz(input.get("contract_slug")).or_else(|| nz(input.get("slug"))).cloned().unwrap_or(Value::Null),
    };
    if !truthy(Some(&slug_v)) {
        return Err("qa-sandbox-reset: contract_slug is required".into());
    }
    // Realistic-domain simplification (every real caller passes a string slug, per census):
    // non-string slug values fall back to "" here rather than JS's full ToString coercion.
    let slug = slug_v.as_str().unwrap_or("").to_string();

    // JS: `input?.reason ?? null`
    let reason = nz(input.get("reason")).cloned().unwrap_or(Value::Null);

    let contract = crate::contract_contract_get::contractGet(&slug_v, db)?;
    if contract.is_null() {
        let mut out = Map::new();
        out.insert("reset".into(), json!(false));
        out.insert("reason".into(), json!(format!("contract '{slug}' not found")));
        out.insert("bytes_removed".into(), json!(0));
        return Ok(Value::Object(out));
    }

    let sandbox_path = contract.get("sandbox_path").cloned().unwrap_or(Value::Null);
    if !truthy(Some(&sandbox_path)) {
        let mut out = Map::new();
        out.insert("reset".into(), json!(false));
        out.insert("reason".into(), json!("contract has no sandbox"));
        out.insert("bytes_removed".into(), json!(0));
        return Ok(Value::Object(out));
    }
    let sandbox_path_str = sandbox_path.as_str().unwrap_or("").to_string();

    let sandbox_abs = node_join(&repo_root(), &sandbox_path_str);
    let bytes = folder_bytes(&sandbox_abs);

    if sandbox_abs.exists() {
        match std::fs::remove_dir_all(&sandbox_abs) {
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {} // JS rmSync force:true parity
            Err(e) => return Err(e.to_string()),
        }
    }

    crate::db_json::run_write(
        db,
        "UPDATE contracts SET sandbox_path = NULL, updated_at = datetime('now') WHERE slug = ?",
        &[json!(slug)],
    )?;

    let mut memory_id = Value::Null;
    let card_slug = contract.get("card_slug").cloned().unwrap_or(Value::Null);
    if truthy(Some(&reason)) && truthy(Some(&card_slug)) {
        let title = format!("sandbox-reset: {slug}");
        let situation = format!("Sandbox for contract '{slug}' was reset during the DEV/QA cycle.");
        let tags_json = json!(["sandbox", "reset", "qa-fail"]).to_string();
        let (_changes, rowid) = crate::db_json::run_write(
            db,
            "INSERT INTO memory.memories
              (type, title, situation, guidance, tags_json, source_card,
               confidence, validated, created_by)
             VALUES ('correction', ?, ?, ?, ?, ?, 0.8, 0, ?)",
            &[json!(title), json!(situation), reason.clone(), json!(tags_json), card_slug, json!(ACTOR)],
        )?;
        memory_id = json!(rowid);
    }

    let mut out = Map::new();
    out.insert("reset".into(), json!(true));
    out.insert("contract_slug".into(), json!(slug));
    out.insert("sandbox_path_before".into(), sandbox_path);
    out.insert("bytes_removed".into(), json!(bytes as i64));
    out.insert("reason".into(), reason);
    out.insert("memory_id".into(), memory_id);
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "ATTACH ':memory:' AS memory;
             CREATE TABLE contracts (slug TEXT, sandbox_path TEXT, card_slug TEXT, updated_at TEXT);
             CREATE TABLE memory.memories (id INTEGER PRIMARY KEY AUTOINCREMENT, type TEXT NOT NULL, \
               title TEXT NOT NULL, situation TEXT NOT NULL, guidance TEXT NOT NULL, \
               tags_json TEXT NOT NULL DEFAULT '[]', source_card TEXT, \
               confidence REAL NOT NULL DEFAULT 0.5, validated INTEGER NOT NULL DEFAULT 0, \
               created_by TEXT NOT NULL);",
        )
        .unwrap();
        c
    }

    fn seed_contract(c: &Connection, slug: &str, sandbox_path: Option<&str>, card_slug: Option<&str>) {
        c.execute(
            "INSERT INTO contracts (slug, sandbox_path, card_slug, updated_at) VALUES (?1, ?2, ?3, '2026-01-01 00:00:00')",
            rusqlite::params![slug, sandbox_path, card_slug],
        )
        .unwrap();
    }

    // repo_root() has no env-override seam (see module doc) — tests write/delete real files
    // under THIS checkout's .antiloki/.test-scratch/, exactly like qa-sandbox-reset.test.mjs's
    // own SCRATCH_REL convention, just under a Rust-specific, PID-tagged subdirectory name so
    // concurrent `cargo test` threads and the JS suite never collide.
    fn scratch_rel(tag: &str) -> String {
        format!(".antiloki/.test-scratch/qa-sandbox-reset-rs-{tag}-{}", std::process::id())
    }
    fn setup_scratch(rel: &str) -> PathBuf {
        let abs = node_join(&repo_root(), rel);
        let _ = fs::remove_dir_all(&abs);
        fs::create_dir_all(abs.join("nested")).unwrap();
        fs::write(abs.join("a.txt"), "hello").unwrap();
        fs::write(abs.join("nested").join("b.txt"), "world!!").unwrap();
        abs
    }
    fn cleanup_scratch(rel: &str) {
        let abs = node_join(&repo_root(), rel);
        let _ = fs::remove_dir_all(&abs);
    }

    #[test]
    fn idempotent_missing_contract_returns_reset_false() {
        let c = db();
        let r = qaSandboxReset(&json!({ "contract_slug": "NOPE-RS" }), &c).unwrap();
        assert_eq!(r["reset"], json!(false));
        assert!(r["reason"].as_str().unwrap().contains("not found"));
        assert_eq!(r["bytes_removed"], json!(0));
        let keys: Vec<&String> = r.as_object().unwrap().keys().collect();
        assert_eq!(keys, vec!["reset", "reason", "bytes_removed"]);
    }

    #[test]
    fn idempotent_no_sandbox_returns_reset_false() {
        let c = db();
        seed_contract(&c, "NO-SB-RS", None, None);
        let r = qaSandboxReset(&json!({ "contract_slug": "NO-SB-RS" }), &c).unwrap();
        assert_eq!(r["reset"], json!(false));
        assert_eq!(r["reason"], json!("contract has no sandbox"));
        assert_eq!(r["bytes_removed"], json!(0));
    }

    #[test]
    fn happy_path_wipes_sandbox_and_clears_column() {
        let c = db();
        let rel = scratch_rel("happy");
        let abs = setup_scratch(&rel);
        assert!(abs.exists());
        seed_contract(&c, "RESET-RS-1", Some(&rel), None);

        let r = qaSandboxReset(&json!({ "contract_slug": "RESET-RS-1" }), &c).unwrap();
        assert_eq!(r["reset"], json!(true));
        assert_eq!(r["contract_slug"], json!("RESET-RS-1"));
        assert_eq!(r["sandbox_path_before"], json!(rel));
        assert!(r["bytes_removed"].as_i64().unwrap() > 0, "should report bytes removed");
        assert_eq!(r["reason"], Value::Null);
        assert_eq!(r["memory_id"], Value::Null);
        let keys: Vec<&String> = r.as_object().unwrap().keys().collect();
        assert_eq!(
            keys,
            vec!["reset", "contract_slug", "sandbox_path_before", "bytes_removed", "reason", "memory_id"]
        );

        assert!(!abs.exists(), "sandbox folder should be gone");
        let sp: Option<String> = c
            .query_row("SELECT sandbox_path FROM contracts WHERE slug='RESET-RS-1'", [], |row| row.get(0))
            .unwrap();
        assert_eq!(sp, None, "sandbox_path cleared in db");

        cleanup_scratch(&rel);
    }

    #[test]
    fn with_reason_and_card_slug_creates_correction_memory() {
        let c = db();
        let rel = scratch_rel("withreason");
        setup_scratch(&rel);
        seed_contract(&c, "RESET-RS-2", Some(&rel), Some("CARD-RESET-RS-2"));

        let r = qaSandboxReset(&json!({ "contract_slug": "RESET-RS-2", "reason": "QA failed 3x; resetting to fresh slate" }), &c).unwrap();
        assert_eq!(r["reset"], json!(true));
        assert!(r["memory_id"].as_i64().is_some(), "memory_id should be set when reason provided");

        let (mtype, title, source_card, situation, guidance): (String, String, Option<String>, String, String) = c
            .query_row(
                "SELECT type, title, source_card, situation, guidance FROM memory.memories WHERE id = ?1",
                rusqlite::params![r["memory_id"].as_i64().unwrap()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
            )
            .unwrap();
        assert_eq!(mtype, "correction");
        assert!(title.contains("sandbox-reset:") && title.contains("RESET-RS-2"));
        assert_eq!(source_card.as_deref(), Some("CARD-RESET-RS-2"));
        assert!(situation.to_lowercase().contains("sandbox") && situation.to_lowercase().contains("reset"));
        assert_eq!(guidance, "QA failed 3x; resetting to fresh slate"); // guidance = the raw input reason

        cleanup_scratch(&rel);
    }

    #[test]
    fn with_reason_but_no_card_slug_writes_no_memory() {
        let c = db();
        let rel = scratch_rel("nocard");
        setup_scratch(&rel);
        seed_contract(&c, "RESET-RS-NOCARD", Some(&rel), None);

        let r = qaSandboxReset(&json!({ "contract_slug": "RESET-RS-NOCARD", "reason": "test" }), &c).unwrap();
        assert_eq!(r["reset"], json!(true));
        assert_eq!(r["memory_id"], Value::Null, "no memory without card_slug to back-link");

        cleanup_scratch(&rel);
    }

    #[test]
    fn calling_twice_on_same_contract_is_idempotent() {
        let c = db();
        let rel = scratch_rel("idemp");
        setup_scratch(&rel);
        seed_contract(&c, "IDEMP-RS", Some(&rel), None);

        let r1 = qaSandboxReset(&json!({ "contract_slug": "IDEMP-RS" }), &c).unwrap();
        assert_eq!(r1["reset"], json!(true));
        let r2 = qaSandboxReset(&json!({ "contract_slug": "IDEMP-RS" }), &c).unwrap();
        assert_eq!(r2["reset"], json!(false));
        assert_eq!(r2["reason"], json!("contract has no sandbox"));

        cleanup_scratch(&rel);
    }

    #[test]
    fn missing_slug_errs() {
        let c = db();
        assert!(qaSandboxReset(&json!({}), &c).is_err());
        assert!(qaSandboxReset(&json!({ "contract_slug": "" }), &c).is_err());
    }

    #[test]
    fn accepts_bare_string_input_and_slug_alias() {
        let c = db();
        seed_contract(&c, "ALIAS-RS", None, None);
        let r1 = qaSandboxReset(&json!("ALIAS-RS"), &c).unwrap();
        assert_eq!(r1["reset"], json!(false));
        assert_eq!(r1["reason"], json!("contract has no sandbox"));
        let r2 = qaSandboxReset(&json!({ "slug": "ALIAS-RS" }), &c).unwrap();
        assert_eq!(r2["reset"], json!(false));
    }
}
