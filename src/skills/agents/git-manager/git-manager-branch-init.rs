//! git-manager-branch-init.rs — twin of skills/agents/git-manager/git-manager-branch-init.mjs.
//! NAMES ARE CONTRACT: `gitManagerBranchInit` (default), `PREFIX_BY_KIND` (named data export).
//!
//! BESPOKE-NATIVE (playbook's VET rule): every success path of this function unconditionally
//! shells out — the very first thing it does is `git rev-parse --verify --quiet
//! refs/heads/<branch>` to test branch existence, before any input-flag branch. There is no
//! conditionally-avoidable path (unlike constitution-estimate.rs's `input.base`-gated git-diff
//! branch), so per the playbook this is proven via `#[cfg(test)]` native tests against a REAL temp
//! git repo (see god-driver-halt.rs / stack-facts.rs for the same temp-dir injection convention),
//! NOT a conform-db fixture — no fixture JSON, no frontier entry, no dispatch arm for this unit.
//!
//! `root` is DB-derived via the already-converted `engine_workspace_root::getWorkspaceRoot` twin
//! (reused, not reimplemented — playbook's cross-module rule), so a test seeds an active
//! `workspaces` row pointing at a real on-disk temp git repo and runs REAL `git` subprocess calls
//! against it, mirroring the JS `spawnSync('git', args, { cwd, encoding: 'utf8' })` exactly.
//!
//! QUIRK CONFIRMED AGAINST REAL JS (via this unit's own `.test.mjs`, added because the source
//! .test.mjs was a scaffold): `checkout` (default `true`) only gates the ALREADY-EXISTING-branch
//! path (`else if (checkout) { git checkout <branch> }`). For a brand-new branch, `git checkout -b
//! <branch>` runs UNCONDITIONALLY and always switches HEAD — so `checked_out` in the returned
//! object is the INPUT value echoed verbatim, not a report of whether HEAD actually moved. Ported
//! faithfully, not "fixed": `checked_out` below is the raw `checkout_v`, same as JS's `{ ...,
//! checked_out: checkout }`.
//!
//! DEFAULTS ARE "MISSING KEY ONLY", NOT `??`: JS destructuring `{ kind = 'feat', checkout = true,
//! allow_existing = true } = input ?? {}` applies the default ONLY when the key is `undefined`
//! (i.e. absent from the JSON object) — an explicit `null` (or any other falsy value) is NOT
//! replaced by the default, it flows through as-is (then gets truthy-tested at its use site, and
//! echoed verbatim in the returned object). Modeled as `input.get(key).cloned().unwrap_or(default)`
//! — `None` (missing) takes the default, `Some(_)` (present, including `Value::Null`) keeps the
//! literal value — NOT the `nz`/`??` helper, which would also override an explicit `null`.
//!
//! `String(kind).toLowerCase()` / `String(card_slug).toLowerCase()` = plain `String(x)` (no `??`)
//! = `js_string`, not `str_qq` — `kind`/`card_slug` are guaranteed defined by this point (kind via
//! its own default, card_slug via the required-guard above), so `js_string`'s `Object`/`Array`
//! coercion applies correctly for a non-string value passed through (e.g. `kind: 42` → `"42"`).
//!
//! `.slice(0, 200)` on `stderr`/`stdout` (playbook gotcha #5, UTF-16 code units): ported as
//! `.chars().take(200)` — real `git` stderr/stdout is ASCII/BMP in practice, so this is exact, not
//! an approximation, for every case this unit's own tests exercise.
// QUALITY: 80/100 — bespoke-native (unconditional subprocess), real git calls proven via temp-dir
// native tests; the checked_out-echoes-input quirk was caught by writing a real JS test first.

use rusqlite::Connection;
use serde_json::{json, Value};

fn truthy(v: Option<&Value>) -> bool { match v { None|Some(Value::Null)=>false, Some(Value::Bool(b))=>*b, Some(Value::Number(n))=>n.as_f64().map_or(false,|f|f!=0.0&&!f.is_nan()), Some(Value::String(s))=>!s.is_empty(), Some(_)=>true } }
fn js_string(v: &Value) -> String { match v { Value::Null=>"null".into(), Value::String(s)=>s.clone(), Value::Number(n)=>n.to_string(), Value::Bool(b)=>b.to_string(), Value::Array(a)=>a.iter().map(js_string).collect::<Vec<_>>().join(","), Value::Object(_)=>"[object Object]".into() } } // = JS String(v)

const SKILL: &str = "git-manager-branch-init";

// `const PREFIX_BY_KIND = { feat:'feat', feature:'feat', fix:'fix', bug:'fix', refactor:'refactor',
// chore:'chore', docs:'docs', test:'test' }` — same key order as the JS object literal.
pub const PREFIX_BY_KIND: &[(&str, &str)] = &[
    ("feat", "feat"), ("feature", "feat"),
    ("fix", "fix"), ("bug", "fix"),
    ("refactor", "refactor"),
    ("chore", "chore"),
    ("docs", "docs"),
    ("test", "test"),
];

fn prefix_for_kind(kind_lower: &str) -> &'static str {
    PREFIX_BY_KIND.iter().find(|(k, _)| *k == kind_lower).map(|(_, v)| *v).unwrap_or("feat")
}

struct GitResult { status: Option<i32>, stdout: String, stderr: String }

/// `function git(args, cwd) { return spawnSync('git', args, { cwd, encoding: 'utf8' }); }` —
/// a failed spawn (e.g. `git` not on PATH) mirrors spawnSync's `status: null`.
fn git(args: &[&str], cwd: &str) -> GitResult {
    match crate::proc::cmd("git").args(args).current_dir(cwd).output() {
        Ok(o) => GitResult {
            status: o.status.code(),
            stdout: String::from_utf8_lossy(&o.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&o.stderr).into_owned(),
        },
        Err(_) => GitResult { status: None, stdout: String::new(), stderr: String::new() },
    }
}

fn slice200(s: &str) -> String { s.chars().take(200).collect() }

/// `export default function gitManagerBranchInit(input, context)`
pub fn gitManagerBranchInit(input: &Value, db: &Connection) -> Result<Value, String> {
    let root_v = crate::engine_workspace_root::getWorkspaceRoot(db);
    let root = root_v.as_str().unwrap_or_default().to_string();

    let card_slug_v = input.get("card_slug").cloned();
    if !truthy(card_slug_v.as_ref()) {
        return Err(format!("{SKILL}: card_slug is required"));
    }
    let card_slug = card_slug_v.unwrap();

    // defaults apply only when the key is MISSING — see module doc.
    let kind_v = input.get("kind").cloned().unwrap_or_else(|| json!("feat"));
    let checkout_v = input.get("checkout").cloned().unwrap_or_else(|| json!(true));
    let allow_existing_v = input.get("allow_existing").cloned().unwrap_or_else(|| json!(true));

    let kind_lower = js_string(&kind_v).to_lowercase();
    let prefix = prefix_for_kind(&kind_lower);
    let card_slug_lower = js_string(&card_slug).to_lowercase();
    let branch = format!("{prefix}/{card_slug_lower}");

    let refspec = format!("refs/heads/{branch}");
    let exists = git(&["rev-parse", "--verify", "--quiet", refspec.as_str()], &root);
    let already = exists.status == Some(0);

    if already && !truthy(Some(&allow_existing_v)) {
        return Err(format!("{SKILL}: branch '{branch}' already exists"));
    }

    if !already {
        let create = git(&["checkout", "-b", branch.as_str()], &root);
        if create.status != Some(0) {
            let msg: &str = if !create.stderr.is_empty() { create.stderr.as_str() } else if !create.stdout.is_empty() { create.stdout.as_str() } else { "" };
            return Err(format!("{SKILL}: git checkout -b failed: {}", slice200(msg)));
        }
    } else if truthy(Some(&checkout_v)) {
        let co = git(&["checkout", branch.as_str()], &root);
        if co.status != Some(0) {
            let msg: &str = if !co.stderr.is_empty() { co.stderr.as_str() } else { "" };
            return Err(format!("{SKILL}: git checkout failed: {}", slice200(msg)));
        }
    }

    Ok(json!({
        "card_slug": card_slug,
        "kind": kind_v,
        "branch": branch,
        "created": !already,
        "already_existed": already,
        "checked_out": checkout_v,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::{Path, PathBuf};

    fn run_git(args: &[&str], cwd: &Path) {
        let status = crate::proc::cmd("git").args(args).current_dir(cwd).output().unwrap();
        assert!(status.status.success(), "git {:?} failed: {}", args, String::from_utf8_lossy(&status.stderr));
    }

    fn make_temp_git_repo(tag: &str) -> PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!("antiloki-git-branch-init-rs-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        run_git(&["init", "-q"], &dir);
        run_git(&["config", "user.email", "test@test"], &dir);
        run_git(&["config", "user.name", "test"], &dir);
        fs::write(dir.join("README.md"), "# test\n").unwrap();
        run_git(&["add", "README.md"], &dir);
        run_git(&["commit", "-q", "-m", "init"], &dir);
        dir
    }

    fn current_branch(dir: &Path) -> String {
        let o = crate::proc::cmd("git").args(["rev-parse", "--abbrev-ref", "HEAD"]).current_dir(dir).output().unwrap();
        String::from_utf8_lossy(&o.stdout).trim().to_string()
    }

    fn db_with_workspace(root: &Path) -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE workspaces (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL UNIQUE, \
             root_path TEXT NOT NULL, vcs TEXT NOT NULL DEFAULT 'git', is_active INTEGER NOT NULL DEFAULT 0, \
             created_at TEXT NOT NULL DEFAULT '2026-01-01 00:00:00')",
        ).unwrap();
        c.execute(
            "INSERT INTO workspaces (name, root_path, is_active) VALUES ('x', ?1, 1)",
            rusqlite::params![root.to_string_lossy().into_owned()],
        ).unwrap();
        c
    }

    #[test]
    fn card_slug_is_required() {
        let c = Connection::open_in_memory().unwrap(); // no workspaces table needed — errors before any git call
        let e = gitManagerBranchInit(&json!({}), &c).unwrap_err();
        assert_eq!(e, "git-manager-branch-init: card_slug is required");
    }

    #[test]
    fn creates_and_checks_out_a_new_branch_default_kind_feat() {
        let dir = make_temp_git_repo("basic");
        let c = db_with_workspace(&dir);
        let r = gitManagerBranchInit(&json!({ "card_slug": "FEAT-CARD-ONE" }), &c).unwrap();
        assert_eq!(
            r,
            json!({
                "card_slug": "FEAT-CARD-ONE", "kind": "feat", "branch": "feat/feat-card-one",
                "created": true, "already_existed": false, "checked_out": true
            })
        );
        assert_eq!(current_branch(&dir), "feat/feat-card-one");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn idempotent_second_call_does_not_error() {
        let dir = make_temp_git_repo("idempotent");
        let c = db_with_workspace(&dir);
        gitManagerBranchInit(&json!({ "card_slug": "FEAT-CARD-ONE" }), &c).unwrap();
        let r = gitManagerBranchInit(&json!({ "card_slug": "FEAT-CARD-ONE" }), &c).unwrap();
        assert_eq!(r["created"], json!(false));
        assert_eq!(r["already_existed"], json!(true));
        assert_eq!(r["checked_out"], json!(true));
        assert_eq!(current_branch(&dir), "feat/feat-card-one");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn kind_alias_and_unknown_kind_fallback() {
        let dir = make_temp_git_repo("kind-alias");
        let c = db_with_workspace(&dir);
        let r1 = gitManagerBranchInit(&json!({ "card_slug": "BUG-ONE", "kind": "bug" }), &c).unwrap();
        assert_eq!(r1["branch"], json!("fix/bug-one"));
        assert_eq!(r1["kind"], json!("bug")); // echoed verbatim, not the resolved prefix
        let r2 = gitManagerBranchInit(&json!({ "card_slug": "ODD-ONE", "kind": "nonsense" }), &c).unwrap();
        assert_eq!(r2["branch"], json!("feat/odd-one"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn checkout_false_on_new_branch_still_switches_head_but_echoes_input() {
        // See module doc QUIRK: `git checkout -b` is unconditional for a NEW branch.
        let dir = make_temp_git_repo("no-checkout-new");
        let c = db_with_workspace(&dir);
        let r = gitManagerBranchInit(&json!({ "card_slug": "CHORE-NO-CHECKOUT", "kind": "chore", "checkout": false }), &c).unwrap();
        assert_eq!(r["branch"], json!("chore/chore-no-checkout"));
        assert_eq!(r["created"], json!(true));
        assert_eq!(r["checked_out"], json!(false)); // raw input, not real git state
        assert_eq!(current_branch(&dir), "chore/chore-no-checkout"); // HEAD moved anyway
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn checkout_false_on_existing_branch_does_not_switch_head() {
        let dir = make_temp_git_repo("no-checkout-existing");
        let c = db_with_workspace(&dir);
        let initial = current_branch(&dir);
        gitManagerBranchInit(&json!({ "card_slug": "EXIST-ONE" }), &c).unwrap(); // creates + switches
        run_git(&["checkout", &initial], &dir); // switch back off the branch
        let r = gitManagerBranchInit(&json!({ "card_slug": "EXIST-ONE", "checkout": false }), &c).unwrap();
        assert_eq!(r["already_existed"], json!(true));
        assert_eq!(r["created"], json!(false));
        assert_eq!(r["checked_out"], json!(false));
        assert_eq!(current_branch(&dir), initial); // untouched
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn allow_existing_false_throws_when_branch_already_exists() {
        let dir = make_temp_git_repo("no-allow-existing");
        let c = db_with_workspace(&dir);
        gitManagerBranchInit(&json!({ "card_slug": "DUP-ONE" }), &c).unwrap();
        let e = gitManagerBranchInit(&json!({ "card_slug": "DUP-ONE", "allow_existing": false }), &c).unwrap_err();
        assert_eq!(e, "git-manager-branch-init: branch 'feat/dup-one' already exists");
        let _ = fs::remove_dir_all(&dir);
    }
}
