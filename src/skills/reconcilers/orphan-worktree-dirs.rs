//! orphan-worktree-dirs.rs — twin of skills/reconcilers/orphan-worktree-dirs.mjs
//! (reconciler-object lane). NAMES ARE CONTRACT: the units are the default object's
//! METHODS `detect(db, { root })` and `fix(finding, db)`.
//!
//! Sweeps `.antiloki/worktrees/<card_slug>/` (git worktrees, full checkouts) for dirs
//! whose card has no ACTIVE contract (non-terminal status + worktree_path still set)
//! pointing at them, and removes them via `git worktree remove --force` on fix.
//!
//! BESPOKE-NATIVE, not conform-db fixture-tested:
//! - `detect()` needs BOTH a seeded `contracts` table AND a real directory listing. The
//!   harness's two lanes are mutually exclusive for that combination: the reconciler-
//!   object dispatch convention (sig "db-last") calls `detect(db)` with NO extra arg, so
//!   a fixture can never inject `root`; the FS-INJECTION lane injects `root` but always
//!   hands the skill a bare, UNSEEDED in-memory db (conform-db.mjs's `isFs` branch skips
//!   attachDbs/seedDb entirely — see conform-db.mjs run), so the `contracts` predicate
//!   could never be exercised either way. Since the real JS DOES expose an injectable
//!   `root` param (unlike the orphan-sandbox-dirs sibling, which has none), this twin
//!   keeps `root` as an explicit parameter and is proven directly by #[cfg(test)]
//!   against a real tempdir + real db, mirroring orphan-worktree-dirs.test.mjs's own
//!   root-injection seam case-for-case.
//! - `fix()` shells out to a REAL `git worktree remove --force <path>` subprocess (via
//!   git-manager-worktree-ops.mjs's `worktreeRemove`, ported inline per lesson 6/11 —
//!   no Rust twin of that module exists yet to call). Written as real, functioning
//!   code; only its SAFE early-return branches (missing card_slug, worktree already
//!   absent on disk) are exercised by #[cfg(test)] — the actual `git worktree remove`
//!   invocation and its failure-message branch are real but untested here (would
//!   require a real git repo + a live worktree registration: a genuine,
//!   unreachable-via-fixture scope limit, not a silent gap).
//!
//! Root resolution mirrors skills/engine/roots.mjs: `HOME_DIR` = `$ANTILOKI_HOME` (env
//! override) else `join(homedir(), '.antiloki')`; `WORKTREES_DIR = join(HOME_DIR,
//! 'worktrees')`. The `git` subprocess cwd mirrors skills/engine/workspace-root.mjs's
//! `getWorkspaceRoot`: the active `workspaces` row's `root_path`, else `SUBSTRATE_ROOT`
//! (`$ANTILOKI_SUBSTRATE_ROOT` env override, else — in the real JS — the engine's own
//! parent dir, derived from import.meta.url; this twin has no equivalent self-location,
//! so an unconfigured fallback uses the process cwd, a divergence production never hits
//! because one of the two overrides is always set).

use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};

const TERMINAL_STATUSES: [&str; 3] = ["done", "rejected", "superseded"];

#[cfg(windows)]
fn homedir() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("USERPROFILE") {
        if !p.is_empty() {
            return Some(PathBuf::from(p));
        }
    }
    let drive = std::env::var("HOMEDRIVE").ok().filter(|s| !s.is_empty());
    let path = std::env::var("HOMEPATH").ok().filter(|s| !s.is_empty());
    match (drive, path) {
        (Some(d), Some(p)) => Some(PathBuf::from(format!("{d}{p}"))),
        _ => None,
    }
}

#[cfg(not(windows))]
fn homedir() -> Option<PathBuf> {
    std::env::var("HOME").ok().filter(|s| !s.is_empty()).map(PathBuf::from)
}

// roots.mjs: HOME_DIR = resolve($ANTILOKI_HOME) if set+truthy, else join(homedir(), '.antiloki');
// WORKTREES_DIR = join(HOME_DIR, 'worktrees').
fn worktrees_dir() -> PathBuf {
    let home_dir = match std::env::var("ANTILOKI_HOME") {
        Ok(v) if !v.is_empty() => PathBuf::from(v),
        _ => homedir().unwrap_or_default().join(".antiloki"),
    };
    home_dir.join("worktrees")
}

// roots.mjs: SUBSTRATE_ROOT = resolve($ANTILOKI_SUBSTRATE_ROOT) if set+truthy, else the
// engine's own parent dir (no Rust equivalent — falls back to cwd; see module doc).
fn substrate_root() -> PathBuf {
    match std::env::var("ANTILOKI_SUBSTRATE_ROOT") {
        Ok(v) if !v.is_empty() => PathBuf::from(v),
        _ => std::env::current_dir().unwrap_or_default(),
    }
}

// workspace-root.mjs: getWorkspaceRoot(db) = getActiveWorkspace(db)?.root_path || SUBSTRATE_ROOT.
// A query error (table absent, pre-migration db) behaves as "no active workspace", mirroring
// getActiveWorkspace's `catch { return null }`.
fn workspace_root(db: &Connection) -> PathBuf {
    let root_path: Option<String> = db
        .query_row(
            "SELECT root_path FROM workspaces WHERE is_active = 1 LIMIT 1",
            [],
            |r| r.get::<usize, Option<String>>(0),
        )
        .ok()
        .flatten();
    match root_path {
        Some(p) if !p.is_empty() => PathBuf::from(p),
        _ => substrate_root(),
    }
}

/// JS: `async detect(db, { root = WORKTREE_ROOT } = {})`. `root` defaults to the live
/// WORKTREES_DIR for production callers; tests inject a tempdir directly (the same seam
/// the JS test uses).
pub fn detect(db: &Connection, root: Option<&Path>) -> Value {
    let owned_root;
    let root = match root {
        Some(r) => r,
        None => {
            owned_root = worktrees_dir();
            &owned_root
        }
    };
    if !root.exists() {
        return json!([]);
    }
    let mut entries: Vec<(String, PathBuf)> = match std::fs::read_dir(root) {
        Ok(rd) => rd
            .filter_map(|e| e.ok())
            .filter(|e| e.path().is_dir())
            .map(|e| (e.file_name().to_string_lossy().into_owned(), e.path()))
            .collect(),
        Err(_) => return json!([]),
    };
    // JS readdirSync order is OS-unspecified; sorted here for deterministic tests — safe
    // since this twin is proven via bespoke native tests, not a byte-for-byte cross-lane diff.
    entries.sort_by(|a, b| a.0.cmp(&b.0));

    let mut findings: Vec<Value> = Vec::new();
    for (card_slug, path) in entries {
        // worktreePathFor lowercases, so the dir name IS the lowercased card slug already —
        // the SQL side does the LOWER(), matching a mixed-case db value against this raw name.
        let path_str = path.to_string_lossy().into_owned();
        let row: Option<(String, Option<String>)> = match db.query_row(
            "SELECT status, worktree_path FROM contracts
             WHERE LOWER(card_slug) = ?1 AND superseded_by IS NULL
             ORDER BY id DESC LIMIT 1",
            rusqlite::params![card_slug],
            |r| Ok((r.get::<usize, String>(0)?, r.get::<usize, Option<String>>(1)?)),
        ) {
            Ok(v) => Some(v),
            Err(rusqlite::Error::QueryReturnedNoRows) => None,
            Err(_) => continue, // JS: catch { continue }
        };

        // JS: `!!row.worktree_path` — an empty string is falsy, same as null/missing.
        let active = row.as_ref().map_or(false, |(status, wt)| {
            !TERMINAL_STATUSES.contains(&status.as_str()) && wt.as_deref().map_or(false, |s| !s.is_empty())
        });
        if active {
            continue;
        }

        let mut detail = Map::new();
        detail.insert("path".into(), json!(path_str));
        detail.insert("card_slug".into(), json!(card_slug));
        detail.insert(
            "reason".into(),
            json!(match &row {
                Some((status, _)) => format!("status={status}"),
                None => "no contract row".to_string(),
            }),
        );

        let message = match &row {
            Some((status, wt)) => format!(
                "worktree '{card_slug}' belongs to a non-active contract (status='{status}', worktree_path={})",
                if wt.as_deref().map_or(false, |s| !s.is_empty()) { "set" } else { "cleared" }
            ),
            None => format!("worktree '{card_slug}' has no contract row (crashed/abandoned)"),
        };

        let mut f = Map::new();
        f.insert("kind".into(), json!("orphan-worktree-dir"));
        f.insert("severity".into(), json!("cleanup"));
        f.insert("message".into(), json!(message));
        f.insert("contract_slug".into(), json!(card_slug));
        f.insert("detail".into(), Value::Object(detail));
        f.insert("suggested_action".into(), json!("git worktree remove --force"));
        findings.push(Value::Object(f));
    }
    json!(findings)
}

/// JS: `async fix(finding, db)` → `worktreeRemove({ card_slug, force: true }, { db })`.
pub fn fix(finding: &Value, db: &Connection) -> Value {
    let card_slug = finding.get("detail").and_then(|d| d.get("card_slug")).and_then(|v| v.as_str());
    let card_slug = match card_slug {
        Some(s) if !s.is_empty() => s,
        _ => return json!({ "applied": false, "reason": "missing card_slug" }),
    };

    // worktreePathFor(card_slug) = resolve(WORKTREE_ROOT, String(card_slug).toLowerCase()).
    let path = worktrees_dir().join(card_slug.to_lowercase());
    if !path.exists() {
        // worktreeRemove: { removed: false, reason: 'worktree does not exist' } →
        // fix(): { applied: false, reason: r.reason ?? 'worktree not removed' }.
        return json!({ "applied": false, "reason": "worktree does not exist" });
    }

    let cwd = workspace_root(db);
    let out = crate::proc::cmd("git")
        .args(["worktree", "remove", "--force", &path.to_string_lossy()])
        .current_dir(&cwd)
        .output();

    match out {
        Ok(o) if o.status.success() => {
            // changes.worktree_removed uses the FINDING's own recorded path (as JS does), not
            // the path this fn just (re)computed from card_slug.
            let path_val = finding
                .get("detail")
                .and_then(|d| d.get("path"))
                .cloned()
                .unwrap_or_else(|| json!(path.to_string_lossy().into_owned()));
            json!({ "applied": true, "changes": { "worktree_removed": [path_val] } })
        }
        Ok(o) => {
            // JS: `(r.stderr || '').slice(0, 200)` — UTF-16 code-unit slicing; chars().take(200)
            // is the ASCII/BMP approximation (disclosed gap per playbook gotcha 5).
            let stderr = String::from_utf8_lossy(&o.stderr);
            let msg: String = stderr.chars().take(200).collect();
            json!({ "applied": false, "reason": format!("git-manager-worktree-remove: git worktree remove failed: {msg}") })
        }
        Err(e) => json!({ "applied": false, "reason": e.to_string() }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::Mutex;

    // SAFETY: ANTILOKI_HOME is process-global and also read by other converted twins'
    // tests (e.g. orphan-sandbox-dirs.rs) running in the same `cargo test` process —
    // serialize any test that touches it so parallel threads never race on its value.
    // shared crate-wide lock (test_env_lock.rs) — a local mutex here would not prevent a race with OTHER files' tests over the same real env var.

    fn with_home<T>(antiloki_home: &str, f: impl FnOnce() -> T) -> T {
        let _guard = crate::test_env_lock::ENV_LOCK.lock().unwrap();
        unsafe {
            std::env::set_var("ANTILOKI_HOME", antiloki_home);
        }
        let result = f();
        unsafe {
            std::env::remove_var("ANTILOKI_HOME");
        }
        result
    }

    fn temp_root(tag: &str) -> PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!("antiloki-orphan-worktree-dirs-rs-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn mk_worktree(root: &Path, slug: &str) {
        fs::create_dir_all(root.join(slug)).unwrap();
    }

    fn test_db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE contracts (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                slug TEXT, card_slug TEXT, status TEXT,
                worktree_path TEXT, superseded_by TEXT
            )",
        )
        .unwrap();
        c
    }

    fn insert_contract(
        db: &Connection,
        card_slug: &str,
        status: &str,
        worktree_path: Option<&str>,
        superseded_by: Option<&str>,
    ) {
        db.execute(
            "INSERT INTO contracts (card_slug, status, worktree_path, superseded_by) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![card_slug, status, worktree_path, superseded_by],
        )
        .unwrap();
    }

    #[test]
    fn detect_missing_root_yields_empty_array() {
        let dir = temp_root("absent-marker");
        let absent = dir.join("does-not-exist");
        let db = test_db();
        let out = detect(&db, Some(&absent));
        assert_eq!(out, json!([]));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn detect_flags_every_orphan_leaves_active_alone() {
        let root = temp_root("mixed");
        let db = test_db();

        // crashed: dir on disk, NO contract row → orphan
        mk_worktree(&root, "card-crashed");
        // terminal: latest contract done → orphan
        insert_contract(&db, "card-done", "done", Some(".antiloki/worktrees/card-done"), None);
        mk_worktree(&root, "card-done");
        // cleared: non-terminal but worktree_path cleared → orphan
        insert_contract(&db, "card-cleared", "impl", None, None);
        mk_worktree(&root, "card-cleared");
        // active: non-terminal + worktree_path set → NOT an orphan
        insert_contract(&db, "card-active", "impl", Some(".antiloki/worktrees/card-active"), None);
        mk_worktree(&root, "card-active");

        let out = detect(&db, Some(&root));
        let arr = out.as_array().unwrap();
        let flagged: std::collections::HashSet<&str> =
            arr.iter().map(|f| f["detail"]["card_slug"].as_str().unwrap()).collect();
        assert!(flagged.contains("card-crashed"), "no contract row → orphan");
        assert!(flagged.contains("card-done"), "terminal contract → orphan");
        assert!(flagged.contains("card-cleared"), "cleared worktree_path → orphan");
        assert!(!flagged.contains("card-active"), "active contract → NOT flagged");
        assert_eq!(arr.len(), 3);
        for f in arr {
            assert_eq!(f["kind"], json!("orphan-worktree-dir"));
            assert_eq!(f["suggested_action"], json!("git worktree remove --force"));
            let path = f["detail"]["path"].as_str().unwrap();
            assert!(
                path.ends_with("card-crashed") || path.ends_with("card-done") || path.ends_with("card-cleared"),
                "unexpected path: {path}"
            );
        }
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn detect_superseded_contract_is_ignored() {
        let root = temp_root("chain");
        let db = test_db();
        // c1 looks active but is superseded by c2 (rejected, no worktree) → orphan
        insert_contract(&db, "card-chain", "rejected", None, None); // c2
        insert_contract(
            &db,
            "card-chain",
            "impl",
            Some(".antiloki/worktrees/card-chain"),
            Some("card-chain-c2"),
        ); // c1, superseded
        mk_worktree(&root, "card-chain");

        let out = detect(&db, Some(&root));
        let arr = out.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["detail"]["card_slug"], json!("card-chain"));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn detect_case_insensitive_lowercased_dir_resolves_mixed_case_card_slug() {
        let root = temp_root("mixedcase");
        let db = test_db();
        insert_contract(&db, "CARD-Mixed", "impl", Some(".antiloki/worktrees/card-mixed"), None);
        mk_worktree(&root, "card-mixed"); // worktreePathFor lowercases dir names

        let out = detect(&db, Some(&root));
        let arr = out.as_array().unwrap();
        assert_eq!(arr.len(), 0, "active contract resolved via LOWER(card_slug) → not flagged");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn fix_missing_card_slug_is_a_noop_failure() {
        let r = fix(&json!({}), &Connection::open_in_memory().unwrap());
        assert_eq!(r, json!({ "applied": false, "reason": "missing card_slug" }));

        let r2 = fix(&json!({ "detail": {} }), &Connection::open_in_memory().unwrap());
        assert_eq!(r2, json!({ "applied": false, "reason": "missing card_slug" }));
    }

    #[test]
    fn fix_worktree_already_absent_on_disk_does_not_shell_out() {
        // Point ANTILOKI_HOME (and so worktrees_dir()) at a fresh empty tempdir with no
        // 'worktrees' subdir at all — the existsSync guard fires before any subprocess is
        // spawned (the SAFE branch), deterministically, with no dependence on machine state.
        let dir = temp_root("fix-absent");
        let r = with_home(dir.to_str().unwrap(), || {
            fix(
                &json!({ "detail": { "card_slug": "some-slug" } }),
                &Connection::open_in_memory().unwrap(),
            )
        });
        assert_eq!(r, json!({ "applied": false, "reason": "worktree does not exist" }));
        let _ = fs::remove_dir_all(&dir);
    }
}
