//! qa-sandbox-promote.rs — twin of skills/contract/qa-sandbox-promote.mjs (BESPOKE-NATIVE).
//! NAMES ARE CONTRACT: `qaSandboxPromote` (default).
//!
//! On QA pass, atomically replaces the contract's original files with their sandbox
//! counterparts: tamper-detect (re-hash every original against `original_shas_json`), stage
//! every sandbox file to `<original>.antiloki-staging` + shadow-copy the pre-promote original
//! (rollback safety), batch-rename staging → original, and on any rename failure roll every
//! already-renamed file back from its shadow. On success it wipes the sandbox folder, clears
//! `contracts.sandbox_path`, and (best-effort) `git add` + `git commit`s the promoted files so
//! the post-QA close path doesn't trip `COMMIT_CODE_BEFORE_AUDIT_COMMIT`.
//!
//! BESPOKE-NATIVE — NOT fixture-testable through the conform-db two-lane harness. Same family +
//! same reasoning as the sibling twins in this folder (qa-sandbox-reset.rs, and
//! agents/tl/tl-sandbox-create.rs): the ENTIRE success path unconditionally mutates the real
//! filesystem (copy → rename → recursive delete), which a two-lane byte-comparison harness
//! cannot safely replay against committed fixture data (the first lane to run would consume/
//! destroy the staged files out from under the second — qa-sandbox-reset.rs's exact precedent).
//! No conform-db fixture / frontier entry is written for this unit; it is proven below via
//! `#[cfg(test)]` native tests, mirroring qa-sandbox-promote.test.mjs's own real-tempdir +
//! real-throwaway-git-repo approach (that JS suite is not a "// SCAFFOLD TEST" — it already does
//! genuine real-fs assertions, so it is left as-is per the playbook's replace-only-scaffolds rule).
//!
//! `context.repo_root`: JS destructures `const repoRoot = context?.repo_root ?? REPO_ROOT;` — a
//! test-only override (FIX-PROMOTE-AUTOCOMMIT-RELPATH) that lets a caller redirect the whole
//! operation into a throwaway git repo instead of the live tree. Modeled as a third
//! `repo_root_override: Option<&str>` parameter — the same "genuine context-sourced knob that
//! doesn't fit the standard 2-arg `(input, db)` shape" pattern as tl-sandbox-create.rs's
//! `dry_run` (itself citing db-reset.rs's `archive_dir_override` precedent). Production callers
//! pass `None`, which falls back to `REPO_ROOT` exactly like JS's `?? REPO_ROOT`.
//!
//! `REPO_ROOT`: `const HERE = dirname(fileURLToPath(import.meta.url)); const REPO_ROOT =
//! resolve(HERE, '..', '..', '..');` — no env override, same as qa-sandbox-reset.mjs. Mirrored
//! via `CARGO_MANIFEST_DIR`'s parent (qa-sandbox-reset.rs's `repo_root()`, copied verbatim).
//!
//! `node_join` (resolve/join normalizing `/` and `\` uniformly, dropping `.` components) copied
//! verbatim from the sandbox-resolve-path.rs / qa-sandbox-reset.rs / tl-sandbox-create.rs
//! precedent — every relpath here is DB-trusted (`contract.files_touched`, repo-relative per the
//! JS docstring), so no traversal-hardening is needed (tl-sandbox-create validates relpaths at
//! sandbox-CREATE time; promote trusts what create already vetted).
//!
//! `<original>.antiloki-staging`: JS builds this via raw string concatenation
//! (`origAbs + STAGING_SUFFIX`), appending onto the FULL path, not just the file name. Mirrored
//! with `PathBuf::as_mut_os_string().push(STAGING_SUFFIX)` (append-in-place on the whole path),
//! not a `.with_extension`/filename-only trick, to match exactly.
//!
//! Realistic-domain simplification (tl-sandbox-create.rs's convention for the sibling module):
//! `contract.files_touched` entries are assumed to be strings — non-string entries are silently
//! dropped rather than reproducing JS's `resolve()` TypeError, since every real `files_touched`
//! array is written by the already-converted, already-validating `tl-sandbox-create` twin.
//!
//! `sha256`: `readFileSync` + `createHash('sha256').update(buf).digest('hex')` →
//! `std::fs::read` + `Sha256::digest` + `hex::encode` (codebase-file-read.rs /
//! tl-sandbox-create.rs precedent).
//!
//! Git subprocess (`git add "<relpaths>"` then `git commit -m <subject> -m <body>`): JS spawns
//! both through ONE shell string via `execSync`, catching either failure into a single
//! `{ok:false, reason}`. This twin issues each as a separate no-shell `std::process::Command`
//! (constitution-estimate.rs's `git_diff_paths` precedent: safer, no quote-injection surface,
//! behaviorally identical for the argv git actually receives) and folds either failure into the
//! same `{ok:false, reason}` shape, `reason` capped to 200 chars via `.chars().take(200)`
//! (gotcha 5: JS `.slice(0,200)` on an ASCII git-error string). Exercised end-to-end by
//! `#[cfg(test)]` `git_auto_commit_real_throwaway_repo` below (real `git init` + real commit in a
//! tempdir), mirroring the JS suite's `FIX-AUTOCOMMIT-RELPATH` test.
//!
//! `datetime('now')` (contracts.updated_at) is left UNFROZEN — same qa-sandbox-reset.rs /
//! tl-sandbox-create.rs precedent: this unit has no JS-lane counterpart run under the
//! frozen-clock conform-db harness to match against, so the honest-for-production translation is
//! the real SQLite clock.
//!
//! Cross-module (already-converted twin, reused not reimplemented): `contractGet`
//! (`contract_contract_get::contractGet`).
//!
//! Throws (`VALIDATION_REQUIRED` / `NOT_FOUND` / `VALIDATION_LOGIC` / `IO_ERROR`) reproduce the
//! JS `AntilokiError` message text (`"qa-sandbox-promote: <cause>"`), matching the project's
//! other bespoke-native twins; `context.db` missing is unreachable here (Rust's `db: &Connection`
//! is a required, non-nullable reference — same convention as every sibling).

use rusqlite::Connection;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
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

const STAGING_SUFFIX: &str = ".antiloki-staging";

// node:path `resolve(base, rel)`/`join(base, rel)` — normalizes `/` and `\` uniformly and drops
// `.` components. See module doc.
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

// See module doc: no env override, CARGO_MANIFEST_DIR's parent mirrors HERE's `../../..`.
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("antiloki-core-rust has a parent dir")
        .to_path_buf()
}

fn sha256_file(path: &Path) -> std::io::Result<String> {
    let buf = std::fs::read(path)?;
    Ok(hex::encode(Sha256::digest(&buf)))
}

fn js_slice_chars(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

struct Staged {
    relpath: String,
    orig_abs: PathBuf,
    staging_abs: PathBuf,
    shadow_abs: PathBuf,
    had_original: bool,
}

fn describe_command_failure(res: &std::io::Result<std::process::Output>) -> String {
    match res {
        Ok(o) => {
            let stderr = String::from_utf8_lossy(&o.stderr);
            let trimmed = stderr.trim();
            if trimmed.is_empty() {
                format!("git exited with status {}", o.status)
            } else {
                trimmed.to_string()
            }
        }
        Err(e) => e.to_string(),
    }
}

// See module doc's "Git subprocess" note: two no-shell `Command`s standing in for JS's one
// shell-string `execSync` pair, folded into the same `{ok, reason|files}` shape.
fn git_commit_promoted(repo_root: &Path, slug: &str, renamed: &[&Staged]) -> Value {
    let relpaths: Vec<&str> = renamed.iter().map(|s| s.relpath.as_str()).collect();

    let add = crate::proc::cmd("git").arg("add").args(&relpaths).current_dir(repo_root).output();
    if !matches!(&add, Ok(o) if o.status.success()) {
        return json!({ "ok": false, "reason": js_slice_chars(&describe_command_failure(&add), 200) });
    }

    let subject = format!("feat(autonomous): {slug} promoted {} file(s)", renamed.len());
    let files_list: String = relpaths.iter().map(|p| format!("- {p}")).collect::<Vec<_>>().join("\n");
    let body = format!(
        "Auto-committed by qa-sandbox-promote so the post-QA close path\n\
(board-keeper.card.status.change) doesn't trip the\n\
COMMIT_CODE_BEFORE_AUDIT_COMMIT law-hook. Files:\n{files_list}"
    );
    let commit = crate::proc::cmd("git")
        .arg("commit")
        .arg("-m")
        .arg(&subject)
        .arg("-m")
        .arg(&body)
        .current_dir(repo_root)
        .output();
    match &commit {
        Ok(o) if o.status.success() => json!({ "ok": true, "files": relpaths }),
        _ => json!({ "ok": false, "reason": js_slice_chars(&describe_command_failure(&commit), 200) }),
    }
}

pub fn qaSandboxPromote(input: &Value, db: &Connection, repo_root_override: Option<&str>) -> Result<Value, String> {
    // JS: `const { db } = context ?? {}; if (!db) throw err.contextMissing(...)` — unreachable
    // here (Rust's `db: &Connection` is a required, non-nullable reference).

    // JS: `typeof input === 'string' ? input : input?.contract_slug ?? input?.slug`
    let slug_v: Value = match input {
        Value::String(s) => Value::String(s.clone()),
        _ => nz(input.get("contract_slug")).or_else(|| nz(input.get("slug"))).cloned().unwrap_or(Value::Null),
    };
    if !truthy(Some(&slug_v)) {
        return Err("qa-sandbox-promote: contract_slug is required".into());
    }
    let slug = slug_v.as_str().unwrap_or("").to_string();

    let contract = crate::contract_contract_get::contractGet(&slug_v, db)?;
    if contract.is_null() {
        return Err(format!("qa-sandbox-promote: contract '{slug}' not found"));
    }
    let sandbox_path_v = contract.get("sandbox_path").cloned().unwrap_or(Value::Null);
    if !truthy(Some(&sandbox_path_v)) {
        return Err(format!("qa-sandbox-promote: contract '{slug}' has no sandbox_path to promote from"));
    }
    let sandbox_path = sandbox_path_v.as_str().unwrap_or("").to_string();

    let last_qa_row = crate::db_json::query_rows(
        db,
        "SELECT id, all_passed FROM qa_runs WHERE contract_slug = ? ORDER BY id DESC LIMIT 1",
        &[json!(slug)],
    )?
    .as_array()
    .and_then(|a| a.first())
    .cloned();
    let last_qa_row = match last_qa_row {
        Some(r) => r,
        None => {
            return Err(format!(
                "qa-sandbox-promote: contract '{slug}' has no qa_runs — cannot promote without QA verdict"
            ))
        }
    };
    let all_passed = last_qa_row.get("all_passed").and_then(Value::as_i64).unwrap_or(0);
    if all_passed != 1 {
        return Err(format!(
            "qa-sandbox-promote: latest qa_run for '{slug}' did not pass (all_passed={all_passed})"
        ));
    }
    let qa_run_id = last_qa_row.get("id").cloned().unwrap_or(Value::Null);

    // JS: `context?.repo_root ?? REPO_ROOT`
    let repo_root_buf: PathBuf = match repo_root_override {
        Some(r) => PathBuf::from(r),
        None => repo_root(),
    };
    let sandbox_abs = node_join(&repo_root_buf, &sandbox_path);

    // JS: `JSON.parse(contract.original_shas_json ?? '{}')` — note this is the RAW _json column
    // (original_shas_json is NOT in contract-get's JSON_COLS, so contractGet leaves it untouched).
    let original_shas_str = match nz(contract.get("original_shas_json")) {
        Some(Value::String(s)) => s.clone(),
        _ => "{}".to_string(),
    };
    let original_shas: Value = serde_json::from_str(&original_shas_str).map_err(|e| {
        format!("qa-sandbox-promote: original_shas_json is invalid JSON for contract '{slug}': {e}")
    })?;

    // JS: `Array.isArray(contract.files_touched) ? contract.files_touched : []` — see module
    // doc's realistic-domain note for the string-only filter.
    let files: Vec<String> = contract
        .get("files_touched")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
        .unwrap_or_default();

    // ── 1. Tamper detection ─────────────────────────────────────────────
    let mut tampered_count: i64 = 0;
    for relpath in &files {
        let orig_abs = node_join(&repo_root_buf, relpath);
        let expected = original_shas.get(relpath).cloned().unwrap_or(Value::Null);
        if expected.is_null() {
            if orig_abs.exists() {
                tampered_count += 1;
            }
            continue;
        }
        if !orig_abs.exists() {
            tampered_count += 1;
            continue;
        }
        let actual = sha256_file(&orig_abs).map_err(|e| e.to_string())?;
        if actual != expected.as_str().unwrap_or("") {
            tampered_count += 1;
        }
    }
    if tampered_count > 0 {
        return Err(format!(
            "qa-sandbox-promote: TAMPERED: {tampered_count} file(s) changed outside the sandbox during DEV/QA. Promote aborted."
        ));
    }

    // ── 2. Stage every sandbox file + SHADOW the original (rollback safety) ──
    let shadow_root = sandbox_abs.join(".shadow");
    let mut staged: Vec<Staged> = Vec::new();

    let stage_result: Result<(), String> = (|| {
        for relpath in &files {
            let sandbox_file_abs = node_join(&sandbox_abs, relpath);
            let orig_abs = node_join(&repo_root_buf, relpath);
            let mut staging_abs = orig_abs.clone();
            staging_abs.as_mut_os_string().push(STAGING_SUFFIX);
            let shadow_abs = node_join(&shadow_root, relpath);

            if !sandbox_file_abs.exists() {
                // DEV never touched this file; it's still bytewise-identical to the original we
                // hashed at TL-create. No-op for promote.
                continue;
            }
            if let Some(parent) = staging_abs.parent() {
                std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            std::fs::copy(&sandbox_file_abs, &staging_abs).map_err(|e| e.to_string())?;
            let had_original = orig_abs.exists();
            if had_original {
                if let Some(parent) = shadow_abs.parent() {
                    std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
                }
                std::fs::copy(&orig_abs, &shadow_abs).map_err(|e| e.to_string())?;
            }
            staged.push(Staged { relpath: relpath.clone(), orig_abs, staging_abs, shadow_abs, had_original });
        }
        Ok(())
    })();

    if let Err(e) = stage_result {
        for s in &staged {
            if s.staging_abs.exists() {
                let _ = std::fs::remove_file(&s.staging_abs);
            }
            if s.shadow_abs.exists() {
                let _ = std::fs::remove_file(&s.shadow_abs);
            }
        }
        return Err(format!("qa-sandbox-promote: failed during stage phase: {e}"));
    }

    // ── 3. Batch-rename staging → original (atomic per-file, rollback-safe) ──
    let mut renamed: Vec<&Staged> = Vec::new();
    let mut rename_error: Option<String> = None;
    for s in &staged {
        match std::fs::rename(&s.staging_abs, &s.orig_abs) {
            Ok(_) => renamed.push(s),
            Err(e) => {
                rename_error = Some(e.to_string());
                break;
            }
        }
    }

    if rename_error.is_some() {
        let mut rollback_failures: i64 = 0;
        for s in &renamed {
            let res: std::io::Result<()> = if s.had_original {
                std::fs::copy(&s.shadow_abs, &s.orig_abs).map(|_| ())
            } else if s.orig_abs.exists() {
                std::fs::remove_file(&s.orig_abs)
            } else {
                Ok(())
            };
            if res.is_err() {
                rollback_failures += 1;
            }
        }
        for s in staged.iter().skip(renamed.len()) {
            if s.staging_abs.exists() {
                let _ = std::fs::remove_file(&s.staging_abs);
            }
        }
        let extra =
            if rollback_failures > 0 { format!("; {rollback_failures} rollback errors") } else { String::new() };
        return Err(format!(
            "qa-sandbox-promote: PROMOTE ROLLED BACK: rename failed at file {}/{}; {} files restored from shadow{extra}.",
            renamed.len() + 1,
            staged.len(),
            renamed.len()
        ));
    }

    // ── 4. Capture post-state shas for the audit ─────────────────────────
    let mut after_shas = Map::new();
    let mut total_bytes: i64 = 0;
    for relpath in &files {
        let orig_abs = node_join(&repo_root_buf, relpath);
        if orig_abs.exists() {
            let sha = sha256_file(&orig_abs).map_err(|e| e.to_string())?;
            let size = std::fs::metadata(&orig_abs).map(|m| m.len()).unwrap_or(0);
            total_bytes += size as i64;
            after_shas.insert(relpath.clone(), json!(sha));
        } else {
            after_shas.insert(relpath.clone(), Value::Null);
        }
    }

    // ── 5. Wipe the sandbox + clear sandbox_path ─────────────────────────
    // JS: `rmSync(sandboxAbs, { recursive: true, force: true })` wrapped in a swallow-all
    // try/catch — non-fatal, originals are already updated.
    let _ = std::fs::remove_dir_all(&sandbox_abs);

    crate::db_json::run_write(
        db,
        "UPDATE contracts SET sandbox_path = NULL, updated_at = datetime('now') WHERE slug = ?",
        &[json!(slug)],
    )?;

    // FEAT-AUTO-COMMIT-PROMOTED-FILES: best-effort; a git failure logs into git_commit_result
    // but doesn't fail promote.
    let git_commit_result =
        if !renamed.is_empty() { git_commit_promoted(&repo_root_buf, &slug, &renamed) } else { Value::Null };

    let mut out = Map::new();
    out.insert("contract_slug".into(), json!(slug));
    out.insert("files_promoted".into(), json!(renamed.len() as i64));
    out.insert("files_unchanged_no_dev_touch".into(), json!(files.len() as i64 - renamed.len() as i64));
    out.insert("total_bytes".into(), json!(total_bytes));
    out.insert("originals_hash_after".into(), Value::Object(after_shas));
    out.insert("qa_run_id".into(), qa_run_id);
    out.insert("git_commit_result".into(), git_commit_result);
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE contracts (slug TEXT PRIMARY KEY, title TEXT, status TEXT, \
               files_touched_json TEXT, sandbox_path TEXT, original_shas_json TEXT, \
               acceptance_json TEXT, naming_json TEXT, edge_cases_json TEXT, must_obey_json TEXT, \
               out_of_scope_json TEXT, reference_artifacts_json TEXT, current_attempt INTEGER, \
               updated_at TEXT);
             CREATE TABLE qa_runs (id INTEGER PRIMARY KEY AUTOINCREMENT, contract_slug TEXT, \
               ts TEXT, verifier TEXT, all_passed INTEGER);",
        )
        .unwrap();
        c
    }

    fn seed_contract(c: &Connection, slug: &str, files: &[&str]) {
        c.execute(
            "INSERT INTO contracts (slug, title, status, files_touched_json, acceptance_json, \
               naming_json, edge_cases_json, must_obey_json, out_of_scope_json, \
               reference_artifacts_json, current_attempt) \
             VALUES (?1, 't', 'tl_approved', ?2, '[]', '{}', '[]', '[]', '[]', '[]', 1)",
            rusqlite::params![slug, serde_json::to_string(files).unwrap()],
        )
        .unwrap();
    }

    fn seed_passing_qa(c: &Connection, slug: &str) {
        c.execute(
            "INSERT INTO qa_runs (contract_slug, ts, verifier, all_passed) VALUES (?1, '2026-01-01 00:00:00', 'agent:qa', 1)",
            rusqlite::params![slug],
        )
        .unwrap();
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!("antiloki-qa-sandbox-promote-rs-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn sha_of(path: &Path) -> String {
        sha256_file(path).unwrap()
    }

    // Sets up: original file(s) written directly under `root`, a sandbox dir at
    // `.antiloki/sandbox/<slug>` under `root` seeded from the originals (mirroring
    // tl-sandbox-create's copy step), contract row w/ original_shas_json captured. Returns root.
    fn setup_contract_with_sandbox(c: &Connection, slug: &str, files: &[&str]) -> PathBuf {
        let root = temp_dir(slug);
        let mut shas = Map::new();
        for f in files {
            let abs = node_join(&root, f);
            if let Some(parent) = abs.parent() {
                fs::create_dir_all(parent).unwrap();
            }
            fs::write(&abs, format!("ORIG:{f}")).unwrap();
            shas.insert(f.to_string(), json!(sha_of(&abs)));
        }
        c.execute(
            "INSERT INTO contracts (slug, title, status, files_touched_json, sandbox_path, \
               original_shas_json, acceptance_json, naming_json, edge_cases_json, must_obey_json, \
               out_of_scope_json, reference_artifacts_json, current_attempt) \
             VALUES (?1, 't', 'tl_approved', ?2, ?3, ?4, '[]', '{}', '[]', '[]', '[]', '[]', 1)",
            rusqlite::params![
                slug,
                serde_json::to_string(files).unwrap(),
                format!(".antiloki/sandbox/{slug}"),
                Value::Object(shas).to_string(),
            ],
        )
        .unwrap();
        let sandbox_dir = root.join(".antiloki").join("sandbox").join(slug);
        fs::create_dir_all(&sandbox_dir).unwrap();
        for f in files {
            let sbox = node_join(&sandbox_dir, f);
            if let Some(parent) = sbox.parent() {
                fs::create_dir_all(parent).unwrap();
            }
            fs::copy(node_join(&root, f), &sbox).unwrap();
        }
        root
    }

    #[test]
    fn missing_slug_errs() {
        let c = db();
        let e = qaSandboxPromote(&json!({}), &c, None).unwrap_err();
        assert!(e.contains("contract_slug"), "{e}");
    }

    #[test]
    fn contract_not_found_errs() {
        let c = db();
        let e = qaSandboxPromote(&json!({ "contract_slug": "NOPE" }), &c, None).unwrap_err();
        assert!(e.contains("not found"), "{e}");
    }

    #[test]
    fn no_sandbox_path_errs() {
        let c = db();
        seed_contract(&c, "NO-SB", &["x.txt"]);
        let e = qaSandboxPromote(&json!({ "contract_slug": "NO-SB" }), &c, None).unwrap_err();
        assert!(e.contains("no sandbox_path"), "{e}");
    }

    #[test]
    fn no_qa_runs_errs() {
        let c = db();
        let slug = "NO-QA";
        let rel = "no-qa.txt";
        let root = setup_contract_with_sandbox(&c, slug, &[rel]);
        let e = qaSandboxPromote(&json!({ "contract_slug": slug }), &c, Some(root.to_str().unwrap())).unwrap_err();
        assert!(e.contains("no qa_runs"), "{e}");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn failed_qa_errs() {
        let c = db();
        let slug = "FAILED-QA";
        let rel = "failed-qa.txt";
        let root = setup_contract_with_sandbox(&c, slug, &[rel]);
        c.execute(
            "INSERT INTO qa_runs (contract_slug, ts, verifier, all_passed) VALUES (?1, '2026-01-01 00:00:00', 'qa', 0)",
            rusqlite::params![slug],
        )
        .unwrap();
        let e = qaSandboxPromote(&json!({ "contract_slug": slug }), &c, Some(root.to_str().unwrap())).unwrap_err();
        assert!(e.contains("did not pass"), "{e}");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn happy_path_promotes_and_wipes_sandbox() {
        let c = db();
        let slug = "HAPPY";
        let rel = "happy.txt";
        let root = setup_contract_with_sandbox(&c, slug, &[rel]);
        // DEV modifies in the sandbox
        fs::write(node_join(&root, &format!(".antiloki/sandbox/{slug}/{rel}")), "PROMOTED").unwrap();
        seed_passing_qa(&c, slug);

        let r = qaSandboxPromote(&json!({ "contract_slug": slug }), &c, Some(root.to_str().unwrap())).unwrap();
        assert_eq!(r["files_promoted"], json!(1));
        assert_eq!(r["files_unchanged_no_dev_touch"], json!(0));
        assert_eq!(fs::read_to_string(node_join(&root, rel)).unwrap(), "PROMOTED");
        assert!(!node_join(&root, &format!(".antiloki/sandbox/{slug}")).exists(), "sandbox folder wiped");
        let sp: Option<String> =
            c.query_row("SELECT sandbox_path FROM contracts WHERE slug = ?1", rusqlite::params![slug], |row| row.get(0)).unwrap();
        assert_eq!(sp, None);
        let staging_leftover = {
            let mut p = node_join(&root, rel);
            p.as_mut_os_string().push(STAGING_SUFFIX);
            p
        };
        assert!(!staging_leftover.exists(), "no leftover staging file");
        let keys: Vec<&String> = r.as_object().unwrap().keys().collect();
        assert_eq!(
            keys,
            vec![
                "contract_slug", "files_promoted", "files_unchanged_no_dev_touch", "total_bytes",
                "originals_hash_after", "qa_run_id", "git_commit_result",
            ]
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn tampered_original_modified_externally_aborts() {
        let c = db();
        let slug = "TAMPER";
        let rel = "tamper.txt";
        let root = setup_contract_with_sandbox(&c, slug, &[rel]);
        fs::write(node_join(&root, &format!(".antiloki/sandbox/{slug}/{rel}")), "DEV-CHANGES").unwrap();
        // External tampering: someone modifies the ORIGINAL after TL-create
        fs::write(node_join(&root, rel), "EXTERNAL-EDIT").unwrap();
        seed_passing_qa(&c, slug);

        let e = qaSandboxPromote(&json!({ "contract_slug": slug }), &c, Some(root.to_str().unwrap())).unwrap_err();
        assert!(e.contains("TAMPERED"), "{e}");
        assert_eq!(fs::read_to_string(node_join(&root, rel)).unwrap(), "EXTERNAL-EDIT", "original left untouched");
        assert!(node_join(&root, &format!(".antiloki/sandbox/{slug}/{rel}")).exists(), "sandbox preserved");
        let sp: Option<String> =
            c.query_row("SELECT sandbox_path FROM contracts WHERE slug = ?1", rusqlite::params![slug], |row| row.get(0)).unwrap();
        assert!(sp.is_some(), "sandbox_path NOT cleared on tamper");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn ignores_files_dev_never_touched_in_sandbox() {
        let c = db();
        let slug = "PARTIAL";
        let touched = "touched.txt";
        let untouched = "untouched.txt";
        let root = setup_contract_with_sandbox(&c, slug, &[touched, untouched]);
        fs::write(node_join(&root, &format!(".antiloki/sandbox/{slug}/{touched}")), "CHANGED").unwrap();
        // Simulate "DEV never touched it": remove the sandbox copy entirely.
        let _ = fs::remove_file(node_join(&root, &format!(".antiloki/sandbox/{slug}/{untouched}")));
        seed_passing_qa(&c, slug);

        let r = qaSandboxPromote(&json!({ "contract_slug": slug }), &c, Some(root.to_str().unwrap())).unwrap();
        assert_eq!(r["files_promoted"], json!(1));
        assert_eq!(r["files_unchanged_no_dev_touch"], json!(1));
        assert_eq!(fs::read_to_string(node_join(&root, touched)).unwrap(), "CHANGED");
        assert_eq!(fs::read_to_string(node_join(&root, untouched)).unwrap(), format!("ORIG:{untouched}"));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn git_auto_commit_real_throwaway_repo() {
        // Mirrors qa-sandbox-promote.test.mjs's FIX-AUTOCOMMIT-RELPATH test: a REAL throwaway git
        // repo via repo_root_override, never the live tree.
        let root = temp_dir("gitcommit");
        let run = |args: &[&str]| {
            let out = crate::proc::cmd("git").args(args).current_dir(&root).output().unwrap();
            assert!(out.status.success(), "git {args:?} failed: {}", String::from_utf8_lossy(&out.stderr));
        };
        run(&["init", "-q"]);
        run(&["config", "user.email", "test@antiloki.local"]);
        run(&["config", "user.name", "antiloki-test"]);

        let rel = "src/feature.txt";
        let abs = node_join(&root, rel);
        fs::create_dir_all(abs.parent().unwrap()).unwrap();
        fs::write(&abs, "ORIGINAL").unwrap();
        run(&["add", "-A"]);
        run(&["commit", "-q", "-m", "initial"]);
        let original_sha = sha_of(&abs);

        let slug = "AUTOCOMMIT";
        let sandbox_rel = format!(".antiloki/sandbox/{slug}");
        let sandbox_file = node_join(&root, &format!("{sandbox_rel}/{rel}"));
        fs::create_dir_all(sandbox_file.parent().unwrap()).unwrap();
        fs::write(&sandbox_file, "PROMOTED-CONTENT").unwrap();

        let c = db();
        let mut shas = Map::new();
        shas.insert(rel.to_string(), json!(original_sha));
        c.execute(
            "INSERT INTO contracts (slug, title, status, files_touched_json, sandbox_path, \
               original_shas_json, acceptance_json, naming_json, edge_cases_json, must_obey_json, \
               out_of_scope_json, reference_artifacts_json, current_attempt) \
             VALUES (?1, 't', 'tl_approved', ?2, ?3, ?4, '[]', '{}', '[]', '[]', '[]', '[]', 1)",
            rusqlite::params![
                slug,
                serde_json::to_string(&[rel]).unwrap(),
                sandbox_rel,
                Value::Object(shas).to_string(),
            ],
        )
        .unwrap();
        seed_passing_qa(&c, slug);

        let r = qaSandboxPromote(&json!({ "contract_slug": slug }), &c, Some(root.to_str().unwrap())).unwrap();
        assert_eq!(r["files_promoted"], json!(1));
        assert_eq!(r["git_commit_result"]["ok"], json!(true), "{:?}", r["git_commit_result"]);
        assert_eq!(r["git_commit_result"]["files"], json!([rel]));
        assert_eq!(fs::read_to_string(&abs).unwrap(), "PROMOTED-CONTENT");

        let log = crate::proc::cmd("git")
            .args(["log", "--oneline", "-1", "--name-only"])
            .current_dir(&root)
            .output()
            .unwrap();
        let log_text = String::from_utf8_lossy(&log.stdout);
        assert!(log_text.contains("promoted 1 file"), "{log_text}");
        assert!(log_text.contains("feature.txt"), "{log_text}");

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn accepts_bare_string_input() {
        let c = db();
        let e = qaSandboxPromote(&json!("SOME-SLUG"), &c, None).unwrap_err();
        assert!(e.contains("not found"), "{e}");
    }
}
