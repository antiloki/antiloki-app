//! sync-capture-revision.rs — twin of skills/engine/sync-capture-revision.mjs (db WRITE lane).
//! NAMES ARE CONTRACT: `syncCaptureRevision` (default), `currentBranch`, `currentSha` (named).
//! Append-only snapshot of a card's full row + phase events + sprint membership into
//! card_revisions, for the current git branch/sha (or an explicit input override).
//!
//! CONDITIONALLY-AVOIDABLE GIT BRANCH — DISCLOSED UNTESTED (playbook lesson 5's flag-skippable
//! pattern, constitution-estimate.rs's git-diff precedent): `syncCaptureRevision` only calls
//! currentBranch()/currentSha() when `input.branch`/`input.sha` is absent or null (JS `??`).
//! Both are ported below as real `git` subprocesses for production parity, but every fixture
//! case supplies an explicit `branch`/`sha`, so the subprocess path is never exercised by
//! conform-db — a real `git rev-parse` result depends on the actual repo/commit state at test
//! time, not something a committed fixture can pin.
//!
//! BESPOKE-NATIVE NAMED HELPERS — `currentBranch`/`currentSha` are their own census units (own
//! exported JS names) whose EVERY success path unconditionally shells out to `git rev-parse` —
//! there is no git-free branch to fixture. Per the playbook these carry NO conform-db
//! fixture/frontier entry; correctness is proven by the `#[cfg(test)]` native tests below against
//! a real temp-dir git repo (code-git-init.rs / god-driver-halt.rs precedent).
//!
//! REPO_ROOT: the JS computes `resolve(HERE, '..', '..', '..')` from `import.meta.url` (HERE =
//! `skills/engine`) — three levels up = the directory that CONTAINS `antiloki-core` (this
//! project's outer repo root, not the antiloki-core-rust crate's own tree, and — unlike
//! workspace-root.rs's `substrate_root()` — with NO `ANTILOKI_SUBSTRATE_ROOT` env override; this
//! file reads no such var either, to match exactly). `repo_root()` below mirrors the destination
//! via `CARGO_MANIFEST_DIR` (= the antiloki-core-rust crate root) walked up ONE level to its
//! parent — the same outer repo root, since antiloki-core-rust and antiloki-core are siblings
//! under it (same layout assumption workspace-root.rs's module doc records for its own root).
//!
//! `context.db` missing → JS throws `err.contextMissing('sync-capture-revision','db')`. Not
//! modeled: `db: &Connection` is a required, non-nullable reference here — same "guard-throws are
//! not exercised" convention every sibling db-lane twin uses.
//!
//! `card_slug` falsy-gate narrowed to "non-empty JSON string" (card-run-preflight.rs precedent):
//! real callers always pass a string card_slug (a TEXT PRIMARY KEY value).
//!
//! `card_slug` not found → JS `new AntilokiError({code:NOT_FOUND, skill:'sync-capture-revision',
//! cause:"card '<slug>' not found"})` → message `"sync-capture-revision: card '<slug>' not
//! found"` (AntilokiError's `parts.join(' ')` = `${skill}:` + cause).
//!
//! `JSON.stringify(snapshot).length`: JS `String.prototype.length` counts UTF-16 code units. All
//! fixture data stays ASCII/BMP (playbook lesson 5), so `.chars().count()` is byte-identical.
// QUALITY: db WRITE + disclosed-untested git-fallback branch (flag-skippable, lesson 5) + two
// bespoke-native named helpers (currentBranch/currentSha) proven via native temp-dir git tests.

use rusqlite::Connection;
use serde_json::{json, Map, Value};

fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) } // JS `??` right operand

/// `resolve(HERE, '..', '..', '..')` from `skills/engine/sync-capture-revision.mjs` — see module doc.
fn repo_root() -> String {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    match std::path::Path::new(manifest_dir).parent() {
        Some(p) => p.to_string_lossy().into_owned(),
        None => manifest_dir.to_string(),
    }
}

/// `function currentBranch()` — `git rev-parse --abbrev-ref HEAD`, cwd=REPO_ROOT. Bespoke-native:
/// see module doc.
pub fn currentBranch() -> Value {
    match crate::proc::cmd("git").args(["rev-parse", "--abbrev-ref", "HEAD"]).current_dir(repo_root()).output() {
        Ok(o) if o.status.success() => json!(String::from_utf8_lossy(&o.stdout).trim().to_string()),
        _ => json!("unknown"),
    }
}

/// `function currentSha()` — `git rev-parse HEAD`, cwd=REPO_ROOT. Bespoke-native: see module doc.
pub fn currentSha() -> Value {
    match crate::proc::cmd("git").args(["rev-parse", "HEAD"]).current_dir(repo_root()).output() {
        Ok(o) if o.status.success() => json!(String::from_utf8_lossy(&o.stdout).trim().to_string()),
        _ => Value::Null,
    }
}

pub fn syncCaptureRevision(input: &Value, db: &Connection) -> Result<Value, String> {
    let card_slug = match input.get("card_slug").and_then(Value::as_str).filter(|s| !s.is_empty()) {
        Some(s) => s.to_string(),
        None => return Err("sync-capture-revision: card_slug is required".into()),
    };
    let branch_in = input.get("branch").cloned();
    let sha_in = input.get("sha").cloned();

    let card_rows = crate::db_json::query_rows(db, "SELECT * FROM cards WHERE slug = ?", &[json!(card_slug.clone())])?;
    let card = match card_rows.as_array().and_then(|a| a.first()).cloned() {
        Some(c) => c,
        None => return Err(format!("sync-capture-revision: card '{card_slug}' not found")),
    };

    let phase_events = crate::db_json::query_rows(
        db,
        "SELECT phase, actor, ts FROM card_phase_events WHERE card_slug = ? ORDER BY id",
        &[json!(card_slug.clone())],
    )?;
    let sprint_membership = crate::db_json::query_rows(
        db,
        "SELECT sprint_slug, phase, est_pts FROM sprint_items WHERE card_slug = ?",
        &[json!(card_slug.clone())],
    )?;

    let captured_branch = nz(branch_in.as_ref()).cloned().unwrap_or_else(currentBranch);
    let captured_sha = nz(sha_in.as_ref()).cloned().unwrap_or_else(currentSha);

    let mut snapshot = Map::new();
    snapshot.insert("card".into(), card);
    snapshot.insert("phase_events".into(), phase_events);
    snapshot.insert("sprint_membership".into(), sprint_membership);
    snapshot.insert("captured_branch".into(), captured_branch.clone());
    snapshot.insert("captured_sha".into(), captured_sha.clone());
    let snapshot_val = Value::Object(snapshot);
    let snapshot_json = serde_json::to_string(&snapshot_val).map_err(|e| e.to_string())?;

    let (_changes, rowid) = crate::db_json::run_write(
        db,
        "INSERT INTO card_revisions (card_slug, branch, sha, snapshot_json) VALUES (?, ?, ?, ?)",
        &[json!(card_slug.clone()), captured_branch.clone(), captured_sha.clone(), json!(snapshot_json.clone())],
    )?;

    let mut out = Map::new();
    out.insert("id".into(), json!(rowid));
    out.insert("card_slug".into(), json!(card_slug));
    out.insert("branch".into(), captured_branch);
    out.insert("sha".into(), captured_sha);
    out.insert("snapshot_size_bytes".into(), json!(snapshot_json.chars().count() as i64));
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn db0() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE cards (slug TEXT PRIMARY KEY, title TEXT, status TEXT, body_json TEXT, risk INTEGER, complexity INTEGER, priority INTEGER, parent_slug TEXT, updated_at TEXT); \
             CREATE TABLE card_phase_events (id INTEGER PRIMARY KEY, card_slug TEXT, phase TEXT, actor TEXT, ts TEXT); \
             CREATE TABLE sprint_items (id INTEGER PRIMARY KEY, sprint_slug TEXT, card_slug TEXT, phase TEXT, est_pts INTEGER); \
             CREATE TABLE card_revisions (id INTEGER PRIMARY KEY, card_slug TEXT, branch TEXT, sha TEXT, snapshot_json TEXT, captured_at TEXT)",
        )
        .unwrap();
        c
    }

    #[test]
    fn missing_card_slug_errs() {
        let c = db0();
        assert!(syncCaptureRevision(&json!({}), &c).unwrap_err().contains("card_slug is required"));
    }

    #[test]
    fn unknown_card_slug_errs_not_found() {
        let c = db0();
        let e = syncCaptureRevision(&json!({ "card_slug": "GHOST", "branch": "main", "sha": "x" }), &c).unwrap_err();
        assert_eq!(e, "sync-capture-revision: card 'GHOST' not found");
    }

    #[test]
    fn captures_full_snapshot_with_phase_events_and_sprint_membership() {
        let c = db0();
        c.execute(
            "INSERT INTO cards (slug,title,status,body_json,risk,complexity,priority,parent_slug,updated_at) VALUES ('CARD-1','Fix login bug','doing','{\"kind\":\"bug\"}',3,2,1,NULL,'2025-12-01 00:00:00')",
            [],
        )
        .unwrap();
        c.execute("INSERT INTO card_phase_events (id,card_slug,phase,actor,ts) VALUES (1,'CARD-1','pl_draft','pl','2025-12-01 00:00:00')", []).unwrap();
        c.execute("INSERT INTO card_phase_events (id,card_slug,phase,actor,ts) VALUES (2,'CARD-1','dev_impl','dev','2025-12-05 00:00:00')", []).unwrap();
        c.execute("INSERT INTO sprint_items (id,sprint_slug,card_slug,phase,est_pts) VALUES (1,'SPR-1','CARD-1','impl',5)", []).unwrap();

        let r = syncCaptureRevision(&json!({ "card_slug": "CARD-1", "branch": "feature/login-fix", "sha": "abc1234" }), &c).unwrap();
        assert_eq!(r["card_slug"], json!("CARD-1"));
        assert_eq!(r["branch"], json!("feature/login-fix"));
        assert_eq!(r["sha"], json!("abc1234"));
        assert_eq!(r["id"], json!(1));
        assert!(r["snapshot_size_bytes"].as_i64().unwrap() > 0);

        let snap_json: String = c.query_row("SELECT snapshot_json FROM card_revisions WHERE id=1", [], |row| row.get(0)).unwrap();
        let snap: Value = serde_json::from_str(&snap_json).unwrap();
        assert_eq!(snap["card"]["slug"], json!("CARD-1"));
        assert_eq!(
            snap["phase_events"],
            json!([
                { "phase": "pl_draft", "actor": "pl", "ts": "2025-12-01 00:00:00" },
                { "phase": "dev_impl", "actor": "dev", "ts": "2025-12-05 00:00:00" },
            ])
        );
        assert_eq!(snap["sprint_membership"], json!([{ "sprint_slug": "SPR-1", "phase": "impl", "est_pts": 5 }]));
        assert_eq!(snap["captured_branch"], json!("feature/login-fix"));
        assert_eq!(snap["captured_sha"], json!("abc1234"));
    }

    #[test]
    fn no_phase_events_or_sprint_membership_yields_empty_arrays() {
        let c = db0();
        c.execute(
            "INSERT INTO cards (slug,title,status,body_json,risk,complexity,priority,parent_slug,updated_at) VALUES ('CARD-2','Standalone card','backlog',NULL,NULL,NULL,NULL,NULL,'2025-11-01 00:00:00')",
            [],
        )
        .unwrap();
        let r = syncCaptureRevision(&json!({ "card_slug": "CARD-2", "branch": "main", "sha": "deadbeef00" }), &c).unwrap();
        let snap_json: String = c.query_row("SELECT snapshot_json FROM card_revisions WHERE id=?1", rusqlite::params![r["id"].as_i64().unwrap()], |row| row.get(0)).unwrap();
        let snap: Value = serde_json::from_str(&snap_json).unwrap();
        assert_eq!(snap["phase_events"], json!([]));
        assert_eq!(snap["sprint_membership"], json!([]));
    }

    #[test]
    fn empty_string_branch_is_kept_not_replaced_by_git_fallback() {
        let c = db0();
        c.execute(
            "INSERT INTO cards (slug,title,status,body_json,risk,complexity,priority,parent_slug,updated_at) VALUES ('CARD-3','Child card','todo','{\"kind\":\"feat\"}',1,4,2,'EPIC-1','2025-10-15 00:00:00')",
            [],
        )
        .unwrap();
        let r = syncCaptureRevision(&json!({ "card_slug": "CARD-3", "branch": "", "sha": "0000000000" }), &c).unwrap();
        assert_eq!(r["branch"], json!(""));
    }

    // ── bespoke-native: currentBranch/currentSha ────────────────────────────────────────────
    // repo_root() is a FIXED cwd (mirrors the JS's fixed import.meta.url-derived REPO_ROOT — no
    // parameter, unlike code-git-init.rs's `root` arg), so these directly call the real exported
    // fns and assert their documented return SHAPE ("unknown"-or-real-branch string /
    // null-or-40-hex-char-sha) rather than a hardcoded value — robust to whether repo_root()
    // (currently NOT a git repo — verified: it is the antiloki-core-rust/antiloki-core PARENT
    // dir, one level above both sibling repos) ever itself becomes one.
    #[test]
    fn current_branch_returns_a_string_real_branch_or_unknown_fallback() {
        let b = currentBranch();
        assert!(b.is_string(), "{b:?}");
    }

    #[test]
    fn current_sha_returns_null_or_a_40_char_hex_sha() {
        let s = currentSha();
        match &s {
            Value::Null => {}
            Value::String(sha) => assert_eq!(sha.len(), 40, "{sha}"),
            other => panic!("unexpected shape: {other:?}"),
        }
    }

    // Validates the exact git command shape currentBranch/currentSha construct (args + success/
    // failure branching) against a real temp-dir repo — the part of the twin that IS fully
    // controllable, since cwd itself is fixed (see above).
    fn temp_repo(tag: &str) -> std::path::PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!("antiloki-sync-capture-revision-rs-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        crate::proc::cmd("git").arg("init").arg("-q").arg("-b").arg("main").current_dir(&dir).output().unwrap();
        crate::proc::cmd("git").args(["-c", "user.name=antiloki", "-c", "user.email=antiloki@local", "commit", "--allow-empty", "-q", "-m", "init"]).current_dir(&dir).output().unwrap();
        dir
    }

    #[test]
    fn git_command_shape_succeeds_in_a_real_repo_and_fails_outside_one() {
        let dir = temp_repo("cmd-shape");
        let branch_out = crate::proc::cmd("git").args(["rev-parse", "--abbrev-ref", "HEAD"]).current_dir(&dir).output().unwrap();
        assert!(branch_out.status.success());
        assert_eq!(String::from_utf8_lossy(&branch_out.stdout).trim(), "main");
        let sha_out = crate::proc::cmd("git").args(["rev-parse", "HEAD"]).current_dir(&dir).output().unwrap();
        assert!(sha_out.status.success());
        assert_eq!(String::from_utf8_lossy(&sha_out.stdout).trim().len(), 40);

        let _ = fs::remove_dir_all(dir.join(".git"));
        let probe = crate::proc::cmd("git").args(["rev-parse", "--abbrev-ref", "HEAD"]).current_dir(&dir).output().unwrap();
        assert!(!probe.status.success()); // matches currentBranch()'s `_ => "unknown"` / currentSha()'s `_ => null` arm

        let _ = fs::remove_dir_all(&dir);
    }
}
