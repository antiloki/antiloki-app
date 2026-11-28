//! driver-pr-open.rs — twin of skills/agents/git-manager/driver-pr-open.mjs (async default
//! export `driverPrOpen(input, context)`, now sync per the async-lane convention — the JS
//! `await`s `cockpitPrSync`, an already-converted sync twin, so nothing genuinely async
//! remains). NAMES ARE CONTRACT: `driverPrOpen`. FEAT-DRIVER-PR-GATE.
//!
//! BESPOKE-NATIVE (playbook's VET rule) — same class as its own dependency,
//! `cockpit_cockpit_pr_sync::cockpitPrSync` (see that module's doc): this function calls
//! `cockpitPrSync({}, context)` UNCONDITIONALLY as its very first step, with no input flag
//! gating that call. `cockpitPrSync` itself shells out to `git rev-parse --abbrev-ref HEAD`
//! unconditionally on every invocation (`root` — resolved via the already-converted
//! `getWorkspaceRoot` twin — never returns falsy; see cockpit-pr-sync.rs's module doc), so
//! EVERY call to `driverPrOpen` transitively runs a real `git` subprocess before it even
//! reaches its own SQL. Worse for fixture-determinism: the SUCCESS path (a PR row actually
//! exists to return) requires the underlying contract to qualify inside `cockpitPrSync`'s own
//! query, which puts it through the per-contract git diff/merge-tree cluster too — the
//! `changed`/`mergeable` values that block feed directly into this skill's own notification
//! message text (`pr.changed`, `pr.mergeable === 0 ? ', CONFLICTS' : ''`). There is no
//! fixture-safe way to reach the success path without real git subprocess output influencing
//! the observable result, so — per the same rule that routed `cockpit-pr-sync.rs` and
//! `git-manager-branch-init.rs` to bespoke-native — this twin is proven via `#[cfg(test)]`
//! native tests against a REAL temp git repo, not a conform-db fixture: no fixture JSON, no
//! frontier entry, no dispatch arm for this unit.
//!
//! `context.db` missing → JS throws `err.contextMissing('driver-pr-open','db')`. Unreachable
//! here: `db: &Connection` is required/non-nullable, same convention every sibling db-lane
//! twin uses. `!input.contract_slug` → JS throws `err.required('driver-pr-open',
//! 'contract_slug')` (message `"driver-pr-open: contract_slug is required"`) — modeled as
//! `Err`, guard-throw not exercised by the native tests per playbook gotcha #9 (only the
//! success path is a contract this twin owns).
//!
//! `pr-sync produced no open PR for '<slug>'` — the JS `AntilokiError` with
//! `code: VALIDATION_LOGIC, skill: 'driver-pr-open'` renders (via `AntilokiError`'s
//! `skill:`+`cause` message assembly, see `engine/error.mjs`) to exactly
//! `"driver-pr-open: pr-sync produced no open PR for '<slug>' — is the worktree branch
//! alive?"` — ported as a plain `Err(String)` with that literal text. Also a guard-throw
//! (only reachable when the contract fails to qualify in `cockpitPrSync`'s own query), not
//! exercised by the native tests.
//!
//! `pr.changed === 1 ? '' : 's'` — English pluralization ported literally via a Rust
//! conditional, not a helper (single call site).
//!
//! `card_slug ?? slug` (`db.prepare(...).get(slug)?.card_slug ?? slug`): the JS `?.` short-
//! circuits to `undefined` when no `contracts` row matches (impossible here — `pr` already
//! proved a `contracts` row exists via `ref_slug = slug` in `cockpit_pull_requests`, so the
//! `contracts` lookup by the same slug always resolves in practice), then `?? slug` covers a
//! NULL/absent `card_slug` column. Modeled with `nz(...).or(Some(&slug_v))` (the `??`
//! helper), not `truthy`/`||`.
//!
//! `INSERT INTO notifications ...` wrapped in JS `try { ... } catch { /* notifications absent
//! on legacy DBs */ }`: modeled as `let _ = db.execute(...)` — a failed INSERT (e.g. missing
//! table) is swallowed exactly like the JS catch, never aborting the function or altering the
//! returned value.
// QUALITY: bespoke-native (transitively shells to git on every success path via
// cockpitPrSync) — no fixture/frontier entry; correctness proven by the native tests below
// against a real temp-dir git repo.

use rusqlite::{Connection, OptionalExtension};
use serde_json::{json, Map, Value};

fn nz(v: Option<&Value>) -> Option<&Value> {
    v.filter(|x| !x.is_null())
}

const SKILL: &str = "driver-pr-open";

/// `export default async function driverPrOpen(input, context)`.
pub fn driverPrOpen(input: &Value, db: &Connection) -> Result<Value, String> {
    let slug = match input.get("contract_slug").and_then(Value::as_str) {
        Some(s) if !s.is_empty() => s.to_string(),
        _ => return Err(format!("{SKILL}: contract_slug is required")),
    };

    // The sync is idempotent (UPSERT per branch) — it computes the diff, changed-file count,
    // and mergeability for EVERY live worktree, this contract's included. Always shells to
    // git (see module doc).
    crate::cockpit_cockpit_pr_sync::cockpitPrSync(&json!({}), db)?;

    let pr: Option<(i64, String, i64, i64)> = db
        .query_row(
            "SELECT id, branch, changed, mergeable FROM cockpit_pull_requests \
             WHERE source = 'driver' AND ref_slug = ? AND status = 'open' \
             ORDER BY id DESC LIMIT 1",
            rusqlite::params![slug],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()
        .map_err(|e| e.to_string())?;

    let (pr_id, branch, changed, mergeable) = match pr {
        Some(row) => row,
        None => {
            return Err(format!(
                "{SKILL}: pr-sync produced no open PR for '{slug}' — is the worktree branch alive?"
            ));
        }
    };

    let card_slug_row: Option<Value> = db
        .query_row(
            "SELECT card_slug FROM contracts WHERE slug = ?",
            rusqlite::params![slug],
            |r| Ok(r.get::<_, Option<String>>(0)?.map(Value::String).unwrap_or(Value::Null)),
        )
        .optional()
        .map_err(|e| e.to_string())?;
    let slug_v = json!(slug);
    let card_slug = nz(card_slug_row.as_ref()).or(Some(&slug_v)).cloned().unwrap_or(slug_v.clone());

    // kind rides in payload_json.kind_detail — notifications.kind carries a CHECK constraint
    // (same convention as the external-traffic interceptor's 'generic' rows).
    let message = format!(
        "pull request ready: {branch} ({changed} file{}{}) — review + merge on the Pulls screen",
        if changed == 1 { "" } else { "s" },
        if mergeable == 0 { ", CONFLICTS" } else { "" },
    );
    let payload = json!({
        "kind_detail": "pr-open",
        "pr_id": pr_id,
        "contract_slug": slug,
        "branch": branch,
    });
    let payload_str = serde_json::to_string(&payload).unwrap_or_else(|_| "{}".to_string());
    // JS wraps the whole INSERT in try/catch — notifications table absent on legacy DBs is
    // swallowed, never aborting the function.
    let _ = db.execute(
        "INSERT INTO notifications (kind, severity, source, message, related_card_slug, payload_json) \
         VALUES ('generic', 'info', 'driver-pr', ?, ?, ?)",
        rusqlite::params![message, card_slug.as_str(), payload_str],
    );

    let mut out = Map::new();
    out.insert("ok".into(), json!(true));
    out.insert("contract_slug".into(), json!(slug));
    out.insert("pr_id".into(), json!(pr_id));
    out.insert("branch".into(), json!(branch));
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::{Path, PathBuf};

    fn run_git(args: &[&str], cwd: &Path) {
        let o = crate::proc::cmd("git").args(args).current_dir(cwd).output().unwrap();
        assert!(o.status.success(), "git {:?} failed: {}", args, String::from_utf8_lossy(&o.stderr));
    }

    fn make_temp_git_repo(tag: &str) -> PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!("antiloki-driver-pr-open-rs-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        run_git(&["init", "-q"], &dir);
        run_git(&["symbolic-ref", "HEAD", "refs/heads/main"], &dir);
        run_git(&["config", "user.email", "test@test"], &dir);
        run_git(&["config", "user.name", "test"], &dir);
        fs::write(dir.join("file.txt"), "line1\n").unwrap();
        run_git(&["add", "file.txt"], &dir);
        run_git(&["commit", "-q", "-m", "init"], &dir);
        dir
    }

    fn db_with_workspace_and_contract(root: &Path, card_slug: &str, contract_slug: &str) -> Connection {
        let c = Connection::open_in_memory().unwrap();
        crate::cockpit_cockpit_schema::ensureCockpitSchema(&c, &json!(""));
        c.execute_batch(
            "CREATE TABLE workspaces (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL UNIQUE, \
             root_path TEXT NOT NULL, vcs TEXT NOT NULL DEFAULT 'git', is_active INTEGER NOT NULL DEFAULT 0, \
             created_at TEXT NOT NULL DEFAULT '2026-01-01 00:00:00'); \
             CREATE TABLE contracts (slug TEXT PRIMARY KEY, card_slug TEXT, worktree_path TEXT, status TEXT, superseded_by TEXT); \
             CREATE TABLE cards (slug TEXT PRIMARY KEY, status TEXT, title TEXT); \
             CREATE TABLE notifications (id INTEGER PRIMARY KEY AUTOINCREMENT, kind TEXT NOT NULL, severity TEXT NOT NULL, \
             source TEXT, message TEXT, related_card_slug TEXT, payload_json TEXT, \
             created_at TEXT NOT NULL DEFAULT '2026-01-01 00:00:00');",
        )
        .unwrap();
        c.execute(
            "INSERT INTO workspaces (name, root_path, is_active) VALUES ('x', ?1, 1)",
            rusqlite::params![root.to_string_lossy().into_owned()],
        )
        .unwrap();
        c.execute(
            "INSERT INTO cards (slug, status, title) VALUES (?1, 'doing', 'pr me')",
            rusqlite::params![card_slug],
        )
        .unwrap();
        c.execute(
            "INSERT INTO contracts (slug, card_slug, worktree_path, status) VALUES (?1, ?2, '/wt/one', 'done')",
            rusqlite::params![contract_slug, card_slug],
        )
        .unwrap();
        c
    }

    #[test]
    fn creates_the_open_driver_pr_row_and_the_operator_notification() {
        let dir = make_temp_git_repo("happy");
        let c = db_with_workspace_and_contract(&dir, "CARD-PR", "CARD-PR-C1");

        let r = driverPrOpen(&json!({ "contract_slug": "CARD-PR-C1" }), &c).unwrap();
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["contract_slug"], json!("CARD-PR-C1"));
        assert!(r["pr_id"].as_i64().unwrap() > 0);
        assert_eq!(r["branch"], json!("antiloki/card-pr"));

        let (source, ref_slug, status): (String, String, String) = c
            .query_row(
                "SELECT source, ref_slug, status FROM cockpit_pull_requests WHERE id = ?",
                rusqlite::params![r["pr_id"].as_i64().unwrap()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!((source.as_str(), ref_slug.as_str(), status.as_str()), ("driver", "CARD-PR-C1", "open"));

        let (kind, severity, related_card_slug, payload_json): (String, String, String, String) = c
            .query_row(
                "SELECT kind, severity, related_card_slug, payload_json FROM notifications ORDER BY id DESC LIMIT 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(kind, "generic", "notifications.kind is CHECK-constrained — detail rides in the payload");
        assert_eq!(severity, "info", "info — never error (severity=error halts the daemon)");
        assert_eq!(related_card_slug, "CARD-PR");
        let payload: Value = serde_json::from_str(&payload_json).unwrap();
        assert_eq!(payload["kind_detail"], json!("pr-open"));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn idempotent_second_call_updates_the_same_branch_row_no_duplicates() {
        let dir = make_temp_git_repo("idempotent");
        let c = db_with_workspace_and_contract(&dir, "CARD-PR", "CARD-PR-C1");

        let a = driverPrOpen(&json!({ "contract_slug": "CARD-PR-C1" }), &c).unwrap();
        let b = driverPrOpen(&json!({ "contract_slug": "CARD-PR-C1" }), &c).unwrap();
        assert_eq!(a["pr_id"], b["pr_id"]);

        let count: i64 = c
            .query_row(
                "SELECT count(*) FROM cockpit_pull_requests WHERE ref_slug = 'CARD-PR-C1' AND status = 'open'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn contract_slug_required_guard() {
        let c = Connection::open_in_memory().unwrap();
        assert_eq!(
            driverPrOpen(&json!({}), &c),
            Err("driver-pr-open: contract_slug is required".to_string())
        );
    }
}
