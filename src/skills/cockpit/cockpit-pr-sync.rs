//! cockpit-pr-sync.rs — twin of skills/cockpit/cockpit-pr-sync.mjs (async default export →
//! sync twin per the async-lane convention: the JS `await`s nothing genuinely async, so the
//! resolved value is returned directly). NAMES ARE CONTRACT: `cockpitPrSync`. EPIC-VIBE-COCKPIT
//! (pull requests): surface the DRIVER's work as PRs too — every contract with a live worktree
//! becomes a `source='driver'` row in `cockpit_pull_requests`, self-healing (closing) any open
//! driver PR whose contract no longer qualifies.
//!
//! BESPOKE-NATIVE (playbook's VET rule) — stronger case than the sibling flag-skippable git
//! twins (constitution-estimate.rs's `input.base`-gated diff, cockpit-pr-list.rs's `input.id`-
//! gated log, cockpit-pane-diff.rs's `worktree_path`-gated diff/commit/discard): here there is
//! NO input flag at all (`input` is `{}`/unused in the JS body) and the git-shelling gate is
//! `if (root)` where `root` comes from `getWorkspaceRoot(db)` — which per its own twin
//! (`engine_workspace_root::getWorkspaceRoot`) NEVER returns falsy: it falls back to
//! `SUBSTRATE_ROOT` whenever no active workspace row resolves. So the `git rev-parse
//! --abbrev-ref HEAD` call at the top of the function fires unconditionally on EVERY success
//! path (`context.db` present), regardless of `contracts` being empty. This is the exact
//! "every success path unconditionally shells out" case the playbook routes to bespoke-native:
//! real git subprocess code (mirrors `git-manager-branch-init.rs`'s `git()` wrapper /
//! `spawnSync`-never-throws convention), proven via `#[cfg(test)]` native tests against a REAL
//! temp git repo (same temp-dir injection convention as `god-driver-halt.rs` / `stack-facts.rs`
//! / `git-manager-branch-init.rs`) — no fixture JSON, no frontier entry, no dispatch arm for
//! this unit. The source `.test.mjs` itself confirms this: it is NOT a scaffold (left as-is per
//! playbook), and both its cases run real `git` subprocesses against `SUBSTRATE_ROOT` (multi-
//! second wall-clock runtime), never mocking `root` away.
//!
//! `c.card_slug || c.slug` / `c.card_title || c.slug`: JS `||` (falsy fallback: NULL, `''`, `0`
//! all fall through), not `??` — modeled with `truthy()`, not `nz()`.
//!
//! `datetime('now')` → frozen literal `2026-01-01 00:00:00` (self-heal's `UPDATE ... updated_at
//! = datetime('now')` and the upsert's two `datetime('now')` occurrences).
//!
//! Both DB reads that JS wraps in `try { ... } catch { X = [] }` (the qualifying-contracts
//! SELECT, and the self-heal SELECT+loop as a whole — the comment `/* table absent on legacy
//! state */` names the exact reason: `cockpit_pull_requests` may not exist pre-`ensureCockpitSchema`
//! on some legacy state paths) collapse a `query_rows` `Err` to an empty result, same as every
//! sibling twin's `.ok().unwrap_or_default()` pattern.
//!
//! Per-contract `try { git(...) } catch { /* branch missing / not a repo */ }`: `spawnSync`
//! itself never throws (see `git-manager-branch-init.rs`/`cockpit-pane-diff.rs` module docs) —
//! this JS `catch` is defensive/dead in practice, so no special unwind-and-keep-partial-state
//! handling is modeled; a missing/invalid branch simply yields git's normal non-zero-exit /
//! empty-stdout behavior, which this twin already reproduces field-for-field via `git()`.
//!
//! GIT QUIRKS VERIFIED AGAINST REAL `git 2.55` (both confirmed via this unit's own tests, not
//! assumed): (1) `git merge-tree --write-tree --name-only <base> <branch>` for a BRANCH THAT
//! DOESN'T EXIST exits **1** — the SAME exit code as a real merge conflict (`--name-only` alone
//! doesn't distinguish "not something we can merge" from an actual conflict) — so a missing
//! branch reports `mergeable: 0`, not `null`, ported as-is. (2) `--messages` defaults ON for
//! `merge-tree` (`git merge-tree -h`), so the JS's "lines after the first blank line" parse
//! (`lines.indexOf('')` → `slice(b+1)`) captures the human-readable "Auto-merging .../CONFLICT
//! (content): ..." informational block, NOT a bare conflicted-file-path list, despite the JS
//! variable being named `conflicts` — ported verbatim (not "fixed"), same "port the quirk, don't
//! correct it" discipline as `git-manager-branch-init.rs`'s `checked_out`-echoes-input note.
// QUALITY: bespoke-native (real git subprocess on every success path) — no fixture/frontier
// entry; correctness proven by the native tests below against a real temp-dir git repo.

use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::collections::HashSet;

fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}
fn js_string(v: &Value) -> String {
    match v {
        Value::Null => "null".into(),
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Array(a) => a.iter().map(js_string).collect::<Vec<_>>().join(","),
        Value::Object(_) => "[object Object]".into(),
    }
}

/// `const git = (args, cwd) => spawnSync('git', args, { cwd, encoding: 'utf8', maxBuffer: 8*1024*1024 })`
/// — never throws (see module doc); a failed spawn collapses to an empty/status=None result,
/// matching JS's `result.stdout` being `null`/`(x.stdout || '')` in that case.
fn git(args: &[&str], cwd: &str) -> (String, Option<i32>) {
    match crate::proc::cmd("git").args(args).current_dir(cwd).output() {
        Ok(o) => (String::from_utf8_lossy(&o.stdout).into_owned(), o.status.code()),
        Err(_) => (String::new(), None),
    }
}

/// `export default async function cockpitPrSync(input, context)` — `input` is unused (the JS
/// body never reads it).
pub fn cockpitPrSync(_input: &Value, db: &Connection) -> Result<Value, String> {
    crate::cockpit_cockpit_schema::ensureCockpitSchema(db, &json!(""));

    // `root` is DB-derived via the already-converted `getWorkspaceRoot` twin — see module doc:
    // it never returns falsy, so the JS `if (root)` gates below are unconditionally true on
    // every real call. Kept as literal `if !root.is_empty()` checks anyway for structural
    // fidelity with the JS source (and as a defensive no-op should that invariant ever change).
    let root_v = crate::engine_workspace_root::getWorkspaceRoot(db);
    let root = root_v.as_str().unwrap_or_default().to_string();

    let mut base = "main".to_string();
    if !root.is_empty() {
        let (stdout, _status) = git(&["rev-parse", "--abbrev-ref", "HEAD"], &root);
        let b = stdout.trim();
        if !b.is_empty() && b != "HEAD" {
            base = b.to_string();
        }
    }

    // PR-GATE DISCIPLINE (see JS module doc): qualifying = worktree provisioned, contract done,
    // card alive, and this is the card's LAST pending contract.
    let contracts: Vec<Value> = crate::db_json::query_rows(
        db,
        "SELECT c.slug, c.card_slug, c.worktree_path, ca.title AS card_title \
         FROM contracts c LEFT JOIN cards ca ON ca.slug = c.card_slug \
         WHERE c.worktree_path IS NOT NULL \
           AND c.status = 'done' \
           AND COALESCE(ca.status, '') NOT IN ('archived') \
           AND NOT EXISTS ( \
             SELECT 1 FROM contracts sib \
             WHERE sib.card_slug = c.card_slug AND sib.slug != c.slug \
               AND sib.superseded_by IS NULL AND sib.status != 'done' \
           )",
        &[],
    )
    .ok()
    .and_then(|v| v.as_array().cloned())
    .unwrap_or_default();

    // SELF-HEAL: any open driver PR whose contract no longer qualifies closes itself.
    let mut healed: Vec<Value> = Vec::new();
    if let Ok(open_rows) = crate::db_json::query_rows(
        db,
        "SELECT id, ref_slug, branch FROM cockpit_pull_requests WHERE source = 'driver' AND status = 'open'",
        &[],
    ) {
        let qualifying: HashSet<String> = contracts
            .iter()
            .filter_map(|c| c.get("slug").and_then(Value::as_str).map(|s| s.to_string()))
            .collect();
        for s in open_rows.as_array().cloned().unwrap_or_default() {
            let qualifies = s.get("ref_slug").and_then(Value::as_str).map_or(false, |rs| qualifying.contains(rs));
            if qualifies {
                continue;
            }
            // JS wraps the WHOLE self-heal block (SELECT + loop + this UPDATE) in one
            // try/catch — an UPDATE failure here would be swallowed, not propagated, and
            // `healed` would simply stop accumulating (not roll back). `let _ =` mirrors that
            // (vs. `?`, which would abort the whole function — divergent from JS's silent
            // catch). Not reachable by any of this unit's own tests: once the SELECT above
            // succeeded the table exists, so an UPDATE by a freshly-selected `id` cannot
            // realistically fail.
            let id = s.get("id").cloned().unwrap_or(Value::Null);
            let _ = crate::db_json::run_write(
                db,
                "UPDATE cockpit_pull_requests SET status = 'closed', updated_at = datetime('2026-01-01 00:00:00') WHERE id = ?",
                &[id],
            );
            healed.push(s.get("branch").cloned().unwrap_or(Value::Null));
        }
    }
    // else: table absent on legacy state — `healed` stays empty, same as JS's caught catch.

    let mut synced: Vec<Value> = Vec::new();
    for c in &contracts {
        let slug_v = c.get("slug").cloned().unwrap_or(Value::Null);
        let card_slug_v = c.get("card_slug").cloned();
        let card_slug_or_slug = if truthy(card_slug_v.as_ref()) { card_slug_v.unwrap() } else { slug_v.clone() };
        let card_slug_lower = js_string(&card_slug_or_slug).to_lowercase();
        let branch = format!("antiloki/{card_slug_lower}");

        let mut changed: i64 = 0;
        let mut patch = String::new();
        let mut mergeable: Value = Value::Null;
        let mut conflicts: Vec<String> = Vec::new();

        if !root.is_empty() {
            let (diff_out, _s1) = git(&["diff", &base, &branch], &root);
            patch = diff_out.chars().take(200000).collect();

            let (names_out, _s2) = git(&["diff", "--name-only", &base, &branch], &root);
            changed = names_out.split('\n').filter(|s| !s.is_empty()).count() as i64;

            let (mt_out, mt_status) = git(&["merge-tree", "--write-tree", "--name-only", &base, &branch], &root);
            if mt_status == Some(0) {
                mergeable = json!(1);
            } else if mt_status == Some(1) {
                mergeable = json!(0);
                let lines: Vec<&str> = mt_out.split('\n').collect();
                if let Some(b) = lines.iter().position(|l| l.is_empty()) {
                    conflicts = lines[b + 1..].iter().filter(|l| !l.is_empty()).map(|s| s.to_string()).collect();
                }
            }
        }

        let title_v = c.get("card_title").cloned();
        let title = if truthy(title_v.as_ref()) { title_v.unwrap() } else { slug_v.clone() };
        let conflicts_json = serde_json::to_string(&conflicts).unwrap_or_else(|_| "[]".into());

        crate::db_json::run_write(
            db,
            "INSERT INTO cockpit_pull_requests (session_id, source, ref_slug, branch, base_branch, title, status, changed, mergeable, conflicts_json, patch, updated_at) \
             VALUES (0, 'driver', ?, ?, ?, ?, 'open', ?, ?, ?, ?, datetime('2026-01-01 00:00:00')) \
             ON CONFLICT(session_id, branch) DO UPDATE SET ref_slug=excluded.ref_slug, base_branch=excluded.base_branch, title=excluded.title, changed=excluded.changed, mergeable=excluded.mergeable, conflicts_json=excluded.conflicts_json, patch=excluded.patch, updated_at=datetime('2026-01-01 00:00:00')",
            &[slug_v, json!(branch.clone()), json!(base.clone()), title, json!(changed), mergeable, json!(conflicts_json), json!(patch)],
        )?;
        synced.push(json!(branch));
    }

    let mut out = Map::new();
    out.insert("ok".into(), json!(true));
    out.insert("synced".into(), json!(synced));
    out.insert("healed".into(), json!(healed));
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
        dir.push(format!("antiloki-cockpit-pr-sync-rs-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        run_git(&["init", "-q"], &dir);
        // pin the initial branch name regardless of the host's `init.defaultBranch` config.
        run_git(&["symbolic-ref", "HEAD", "refs/heads/main"], &dir);
        run_git(&["config", "user.email", "test@test"], &dir);
        run_git(&["config", "user.name", "test"], &dir);
        fs::write(dir.join("file.txt"), "line1\n").unwrap();
        run_git(&["add", "file.txt"], &dir);
        run_git(&["commit", "-q", "-m", "init"], &dir);
        dir
    }

    fn db_with_workspace(root: &Path) -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE workspaces (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL UNIQUE, \
             root_path TEXT NOT NULL, vcs TEXT NOT NULL DEFAULT 'git', is_active INTEGER NOT NULL DEFAULT 0, \
             created_at TEXT NOT NULL DEFAULT '2026-01-01 00:00:00'); \
             CREATE TABLE contracts (slug TEXT PRIMARY KEY, card_slug TEXT, worktree_path TEXT, status TEXT, superseded_by TEXT); \
             CREATE TABLE cards (slug TEXT PRIMARY KEY, status TEXT, title TEXT);",
        )
        .unwrap();
        c.execute(
            "INSERT INTO workspaces (name, root_path, is_active) VALUES ('x', ?1, 1)",
            rusqlite::params![root.to_string_lossy().into_owned()],
        )
        .unwrap();
        c
    }

    #[test]
    fn syncs_qualifying_contract_clean_merge_and_heals_stale_driver_pr() {
        let dir = make_temp_git_repo("clean");
        run_git(&["checkout", "-q", "-b", "antiloki/card-one"], &dir);
        fs::write(dir.join("feature.txt"), "new file\n").unwrap();
        run_git(&["add", "feature.txt"], &dir);
        run_git(&["commit", "-q", "-m", "feature work"], &dir);
        run_git(&["checkout", "-q", "main"], &dir);

        let c = db_with_workspace(&dir);
        crate::cockpit_cockpit_schema::ensureCockpitSchema(&c, &json!(""));
        c.execute("INSERT INTO cards (slug, status, title) VALUES ('CARD-ONE', 'doing', 'Card One Title')", []).unwrap();
        c.execute(
            "INSERT INTO contracts (slug, card_slug, worktree_path, status) VALUES ('CT-1', 'CARD-ONE', '/wt/one', 'done')",
            [],
        )
        .unwrap();
        c.execute(
            "INSERT INTO cockpit_pull_requests (session_id, source, ref_slug, branch, base_branch, status) VALUES (0, 'driver', 'CT-STALE', 'antiloki/card-stale', 'main', 'open')",
            [],
        )
        .unwrap();

        let r = cockpitPrSync(&json!({}), &c).unwrap();
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["synced"], json!(["antiloki/card-one"]));
        assert_eq!(r["healed"], json!(["antiloki/card-stale"]));

        let row = crate::db_json::query_rows(&c, "SELECT * FROM cockpit_pull_requests WHERE branch = 'antiloki/card-one'", &[])
            .unwrap();
        let row = row.as_array().unwrap().first().unwrap();
        assert_eq!(row["ref_slug"], json!("CT-1"));
        assert_eq!(row["base_branch"], json!("main"));
        assert_eq!(row["title"], json!("Card One Title"));
        assert_eq!(row["status"], json!("open"));
        assert_eq!(row["changed"], json!(1));
        assert_eq!(row["mergeable"], json!(1));
        assert_eq!(row["conflicts_json"], json!("[]"));
        assert!(row["patch"].as_str().unwrap().contains("feature.txt"), "{}", row["patch"]);

        let stale_status: String = c
            .query_row("SELECT status FROM cockpit_pull_requests WHERE ref_slug = 'CT-STALE'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(stale_status, "closed");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn conflicting_merge_marks_mergeable_zero_with_conflicts_and_title_falls_back_to_contract_slug() {
        let dir = make_temp_git_repo("conflict");
        run_git(&["checkout", "-q", "-b", "antiloki/card-two"], &dir);
        fs::write(dir.join("file.txt"), "line1-feature\n").unwrap();
        run_git(&["commit", "-q", "-am", "feature edit"], &dir);
        run_git(&["checkout", "-q", "main"], &dir);
        fs::write(dir.join("file.txt"), "line1-main\n").unwrap();
        run_git(&["commit", "-q", "-am", "main edit"], &dir);

        let c = db_with_workspace(&dir);
        crate::cockpit_cockpit_schema::ensureCockpitSchema(&c, &json!(""));
        c.execute("INSERT INTO cards (slug, status, title) VALUES ('CARD-TWO', 'doing', NULL)", []).unwrap();
        c.execute(
            "INSERT INTO contracts (slug, card_slug, worktree_path, status) VALUES ('CT-2', 'CARD-TWO', '/wt/two', 'done')",
            [],
        )
        .unwrap();

        let r = cockpitPrSync(&json!({}), &c).unwrap();
        assert_eq!(r["synced"], json!(["antiloki/card-two"]));

        let row = crate::db_json::query_rows(&c, "SELECT * FROM cockpit_pull_requests WHERE branch = 'antiloki/card-two'", &[])
            .unwrap();
        let row = row.as_array().unwrap().first().unwrap();
        assert_eq!(row["mergeable"], json!(0));
        assert_eq!(row["title"], json!("CT-2")); // card_title falsy (NULL) -> falls back to c.slug
        // `conflicts` = the lines AFTER the first blank line in `merge-tree --name-only`'s
        // stdout — verified against real git 2.55: that's the "Auto-merging .../CONFLICT ..."
        // informational block, not a bare file-path list (ported verbatim, not "fixed" — see
        // module doc). Assert loosely (git's exact wording isn't a contract this twin owns).
        let conflicts: Vec<String> = serde_json::from_str(row["conflicts_json"].as_str().unwrap()).unwrap();
        assert!(!conflicts.is_empty(), "{:?}", conflicts);
        assert!(conflicts.iter().any(|s| s.contains("file.txt")), "{:?}", conflicts);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn card_slug_missing_falls_back_to_contract_slug_for_the_branch_name() {
        let dir = make_temp_git_repo("no-card-slug");
        let c = db_with_workspace(&dir);
        c.execute(
            "INSERT INTO contracts (slug, card_slug, worktree_path, status) VALUES ('CT-3', NULL, '/wt/three', 'done')",
            [],
        )
        .unwrap();

        let r = cockpitPrSync(&json!({}), &c).unwrap();
        // branch missing on disk: `git diff`/`git diff --name-only` exit 128 with empty stdout
        // (patch='', changed=0); `git merge-tree` exits 1 ("not something we can merge") with
        // empty stdout too — same exit code as a real conflict, so mergeable=0 and the
        // conflicts-parse finds no path lines after the (absent) blank-line separator, staying
        // `[]`. Verified directly against real `git 2.55` — see module doc.
        assert_eq!(r["synced"], json!(["antiloki/ct-3"]));
        let row = crate::db_json::query_rows(&c, "SELECT * FROM cockpit_pull_requests WHERE branch = 'antiloki/ct-3'", &[])
            .unwrap();
        let row = row.as_array().unwrap().first().unwrap();
        assert_eq!(row["changed"], json!(0));
        assert_eq!(row["mergeable"], json!(0));
        assert_eq!(row["conflicts_json"], json!("[]"));
        assert_eq!(row["patch"], json!(""));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn excludes_archived_card_no_worktree_and_sibling_still_pending() {
        let dir = make_temp_git_repo("excludes");
        let c = db_with_workspace(&dir);
        c.execute_batch(
            "INSERT INTO cards (slug, status) VALUES ('CARD-DEAD', 'archived'), ('CARD-MID', 'doing'), ('CARD-MULTI', 'doing');
             INSERT INTO contracts (slug, card_slug, worktree_path, status) VALUES
               ('CARD-DEAD-C1', 'CARD-DEAD', '/wt/dead', 'done'),
               ('CARD-MID-C1', 'CARD-MID', '/wt/mid', 'tl_approved'),
               ('CARD-MULTI-C1', 'CARD-MULTI', '/wt/multi', 'done'),
               ('CARD-MULTI-C2', 'CARD-MULTI', NULL, 'impl');",
        )
        .unwrap();

        let r = cockpitPrSync(&json!({}), &c).unwrap();
        assert_eq!(r["synced"], json!([]));
        assert_eq!(r["healed"], json!([]));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn idempotent_second_call_upserts_rather_than_duplicating() {
        let dir = make_temp_git_repo("idempotent");
        run_git(&["checkout", "-q", "-b", "antiloki/card-one"], &dir);
        fs::write(dir.join("feature.txt"), "v1\n").unwrap();
        run_git(&["add", "feature.txt"], &dir);
        run_git(&["commit", "-q", "-m", "feature work"], &dir);
        run_git(&["checkout", "-q", "main"], &dir);

        let c = db_with_workspace(&dir);
        c.execute("INSERT INTO cards (slug, status, title) VALUES ('CARD-ONE', 'doing', 'Card One Title')", []).unwrap();
        c.execute(
            "INSERT INTO contracts (slug, card_slug, worktree_path, status) VALUES ('CT-1', 'CARD-ONE', '/wt/one', 'done')",
            [],
        )
        .unwrap();

        cockpitPrSync(&json!({}), &c).unwrap();
        cockpitPrSync(&json!({}), &c).unwrap();

        let count: i64 = c
            .query_row("SELECT COUNT(*) FROM cockpit_pull_requests WHERE branch = 'antiloki/card-one'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1);

        let _ = fs::remove_dir_all(&dir);
    }
}
