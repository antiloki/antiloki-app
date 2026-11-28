//! workspace-add.rs — twin of skills/system/workspace-add.mjs. NAMES ARE CONTRACT:
//! `workspaceAdd` (default export).
//!
//! BESPOKE-NATIVE, not conform-db fixture-tested: same class as workspace-provision.rs — this
//! opens a SECOND real sqlite connection directly to the substrate registry (bypassing
//! `context.db`, per the JS module doc: "Workspaces are not in the audited entity set, so a
//! direct write is allowed"), checks real disk via `existsSync`, and cross-calls the
//! already-converted `workspaceProvision` twin (playbook rule 11). None of that is expressible
//! as a conform-db fixture. Proven below via `#[cfg(test)]` against real temp directories.
//!
//! Cross-module call (already-converted twin, reused not reimplemented):
//! `crate::system_workspace_provision::workspaceProvision`.
//!
//! `context.db` in the JS was a live DatabaseSync handle whose ONLY use was a truthiness check
//! (`if (!db) throw err.contextMissing(...)`) — it was never queried. This twin first modeled that
//! as a `context.get("db")` check on the JSON context, which looked faithful and was fatal: the
//! dispatcher hands over `ctx.context_value()` (`{actor, source}`) because a `Connection` cannot
//! ride inside a `Value`, so the check rejected every real dispatch and `system.workspace.add`
//! never once succeeded. The guard is gone — see `workspaceAdd`'s own doc for why nothing is lost.
//!
//! `err.required`/`err.contextMissing`/`AntilokiError{VALIDATION_LOGIC}`/`AntilokiError{NOT_FOUND}`
//! (engine/error.mjs, not yet a twin) all render `"${skill}: ${cause}"` — reproduced inline as
//! exact-message `Err(String)` rather than pulled through a stub twin, same convention as
//! workspace-provision.rs's `err.required` reproduction.

use regex::Regex;
use rusqlite::Connection;
use serde_json::{json, Value};


fn trimmed(v: Option<&Value>) -> Option<String> {
    v.and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).map(str::to_string)
}

/// THE REPO'S OWN `.antiloki/` MUST NOT SHOW UP AS UNTRACKED (operator 2026-10-01: registering a workspace left `.antiloki/` in the project root as untracked files in `git status`).
/// Added to the repo's LOCAL ignore list (`.git/info/exclude`, never the tracked `.gitignore`, so nothing the operator would commit changes) when the root is a git repo and nothing
/// already ignores it - the root `.gitignore` or the exclude file itself. A linked worktree's `.git` is a file; the exclude list lives in the COMMON dir. Idempotent. Returns what
/// happened, for the answer: "added", "already ignored", or "not a git repository".
pub fn ensure_git_exclude(root: &std::path::Path) -> Result<&'static str, String> {
    use std::path::{Path, PathBuf};
    let dot_git = root.join(".git");
    let md = match std::fs::symlink_metadata(&dot_git) { Ok(m) => m, Err(_) => return Ok("not a git repository") };
    // a `.git` that is a link is not this repository's own metadata (audit 2026-10-01, F-17)
    if md.file_type().is_symlink() { return Err(".git is a symlink - not touched".into()) }
    let common: PathBuf = if md.is_dir() { dot_git } else if md.is_file() {
        // THE `gitdir:` POINTER IS NOT TRUSTED (audit 2026-10-01, F-17): a `.git` file can name any folder, and the exclude list would have been created THERE. It must name a git directory
        // that points back at this checkout - a linked worktree's `worktrees/<name>/gitdir` holds the path of this `.git` file, a submodule's config has `worktree =` this folder.
        let text = String::from_utf8_lossy(&std::fs::read(&dot_git).map_err(|e| format!(".git: {e}"))?).into_owned();
        let gitdir = text.lines().find_map(|l| l.trim().strip_prefix("gitdir:")).map(|g| g.trim().to_string()).ok_or("the .git file names no gitdir")?;
        let gitdir = if Path::new(&gitdir).is_absolute() { PathBuf::from(&gitdir) } else { root.join(&gitdir) };
        let gitdir = std::fs::canonicalize(&gitdir).map_err(|e| format!("the gitdir the .git file names cannot be read: {e}"))?;
        let me = std::fs::canonicalize(root).map_err(|e| format!("{}: {e}", root.display()))?;
        if !gitdir.join("HEAD").is_file() { return Err("the .git file names a folder that is not a git directory".into()) }
        let resolve = |p: &str, base: &Path| std::fs::canonicalize(if Path::new(p).is_absolute() { PathBuf::from(p) } else { base.join(p) }).ok();
        let points_back = std::fs::read_to_string(gitdir.join("gitdir")).ok().and_then(|g| resolve(g.trim(), &gitdir)).map_or(false, |p| p == me.join(".git"))
            || std::fs::read_to_string(gitdir.join("config")).ok().map_or(false, |c| c.lines().filter_map(|l| l.trim().strip_prefix("worktree")).filter_map(|r| r.trim_start().strip_prefix('=')).any(|w| resolve(w.trim(), &gitdir).map_or(false, |p| p == me)));
        if !points_back { return Err("the .git file names a git directory that does not belong to this checkout - not touched".into()) }
        match std::fs::read_to_string(gitdir.join("commondir")) {
            Ok(c) => {
                let common = std::fs::canonicalize(gitdir.join(c.trim())).map_err(|e| format!("commondir: {e}"))?;
                if !gitdir.starts_with(&common) || !common.join("HEAD").is_file() { return Err("the worktree's commondir is not the repository it belongs to - not touched".into()) }
                common
            }
            Err(_) => gitdir,
        }
    } else { return Ok("not a git repository") };
    // ANCHORED to the repository root: a bare `.antiloki/` also hides every NESTED `.antiloki/` (a package's own, a vendored repo's) from `git status` (audit 2026-10-01, F-17)
    const NAMES: &[&str] = &[".antiloki", ".antiloki/", "/.antiloki", "/.antiloki/", ".antiloki/*", "/.antiloki/*", ".antiloki/**", "/.antiloki/**"];
    // BYTES, not a String: an exclude file that is not valid UTF-8 (a path in a legacy encoding in a comment) used to read as empty and be overwritten with only our line
    let ignores = |f: &Path| std::fs::read(f).map(|b| String::from_utf8_lossy(&b).lines().map(str::trim).any(|l| NAMES.contains(&l))).unwrap_or(false);
    let exclude = common.join("info").join("exclude");
    if ignores(&root.join(".gitignore")) || ignores(&exclude) { return Ok("already ignored") }
    if std::fs::symlink_metadata(&exclude).map_or(false, |m| m.file_type().is_symlink()) { return Err("info/exclude is a symlink - not touched".into()) }
    std::fs::create_dir_all(exclude.parent().unwrap_or(&common)).map_err(|e| format!("info/exclude: {e}"))?;
    // APPEND ONLY: whatever is in the file stays exactly as it is
    let existing = std::fs::read(&exclude).unwrap_or_default();
    let mut add = String::new();
    if !existing.is_empty() && !existing.ends_with(b"\n") { add.push('\n'); }
    add.push_str("# antiloki's own working folder (added when the workspace was registered)\n/.antiloki/\n");
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(&exclude).map_err(|e| format!("info/exclude: {e}"))?;
    f.write_all(add.as_bytes()).map_err(|e| format!("info/exclude: {e}"))?;
    Ok("added")
}

/// `export default async function workspaceAdd(input, context)`.
///
/// `context` carries the test-only overrides `registryPath` / `sourceRegistryPath`, mirrored from
/// the JS `context?.registryPath`.
///
/// THERE IS NO `context.db` GUARD, deliberately. The JS twin opened with `if (!context?.db) throw`
/// because `db` there was the ambient live handle. A Rust `Connection` cannot travel inside a JSON
/// `Value`, so the dispatcher passes `ctx.context_value()` — `{actor, source}` — and the ported
/// guard could NEVER pass: `system.workspace.add` failed 100% of the time with
/// "context.db is required", which is what "create a workspace" hit in the FE.
///
/// Removing it loses nothing, because this skill never read `db` in the first place: like
/// workspace-list, it opens its OWN connection to the registry (the substrate state DB) rather than
/// the possibly workspace-scoped `context.db`. The real preconditions — name, root_path, slug
/// shape, path exists, not already registered — are all checked below and all still apply.
pub fn workspaceAdd(input: &Value, context: &Value) -> Result<Value, String> {
    let name = trimmed(input.get("name"));
    let root_path = trimmed(input.get("root_path"));
    let name = match name {
        Some(n) => n,
        None => return Err("workspace-add: name is required".to_string()),
    };
    let root_path = match root_path {
        Some(r) => r,
        None => return Err("workspace-add: root_path is required".to_string()),
    };

    let name_re = Regex::new(r"(?i)^[a-z0-9][a-z0-9._-]*$").unwrap();
    if !name_re.is_match(&name) {
        return Err(format!(
            "workspace-add: workspace name must be a simple slug ([a-z0-9._-], no spaces): got '{name}'"
        ));
    }
    if !std::path::Path::new(&root_path).exists() {
        return Err(format!("workspace-add: root_path does not exist on disk: '{root_path}'"));
    }

    let registry_path = context.get("registryPath").and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| crate::engine_open_db::default_state_path().to_string_lossy().into_owned());

    let reg = Connection::open(&registry_path).map_err(|e| e.to_string())?;

    let exists: Option<i64> = reg
        .query_row("SELECT 1 FROM workspaces WHERE name = ?1", rusqlite::params![name], |row| row.get(0))
        .ok();
    if exists.is_some() {
        return Err(format!("workspace-add: workspace '{name}' is already registered"));
    }

    let prov_input = json!({ "name": name, "root_path": root_path });
    let prov_context = json!({ "sourceRegistryPath": registry_path });
    let prov = crate::system_workspace_provision::workspaceProvision(&prov_input, &prov_context)?;
    let db_dir = prov.get("db_dir").and_then(Value::as_str).unwrap_or_default().to_string();
    let actions_copied = prov.get("actions_copied").cloned().unwrap_or(json!(0i64));

    reg.execute(
        "INSERT INTO workspaces (name, root_path, vcs, is_active, db_dir) VALUES (?1, ?2, 'git', 0, ?3)",
        rusqlite::params![name, root_path, db_dir],
    ).map_err(|e| e.to_string())?;

    // best effort: a read-only .git must not fail a registration that has already happened
    let git_exclude = match ensure_git_exclude(std::path::Path::new(&root_path)) { Ok(w) => w.to_string(), Err(e) => format!("not changed: {e}") };

    Ok(json!({
        "ok": true,
        "name": name,
        "root_path": root_path,
        "db_dir": db_dir,
        "actions_copied": actions_copied,
        "git_exclude": git_exclude,
        "active": false,
        "note": "workspace registered. Run system.workspace.activate then restart the substrate to point antiloki at it.",
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::{Path, PathBuf};

    fn temp_dir(tag: &str) -> PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!("antiloki-workspace-add-rs-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    // Minimal live "substrate registry" (state.sqlite with a workspaces table) — enough for
    // workspaceAdd's own SELECT/INSERT; workspaceProvision opens its own tables via openDb.
    fn seed_registry(dir: &Path) -> PathBuf {
        let state_path = dir.join("state.sqlite");
        let reg = Connection::open(&state_path).unwrap();
        reg.execute_batch(
            "CREATE TABLE workspaces (id INTEGER PRIMARY KEY, name TEXT, root_path TEXT, vcs TEXT, is_active INTEGER, db_dir TEXT);
             CREATE TABLE agents (id INTEGER PRIMARY KEY, name TEXT UNIQUE, description TEXT, parent_agent TEXT);
             CREATE TABLE actions (id INTEGER PRIMARY KEY, name TEXT UNIQUE, description TEXT, owner_agent TEXT, steps_json TEXT);
             CREATE TABLE agent_actions (id INTEGER PRIMARY KEY, agent_name TEXT, action_name TEXT, granted_by TEXT, revoked_at TEXT);"
        ).unwrap();
        drop(reg);
        state_path
    }

    fn ctx(registry_path: &Path) -> Value {
        json!({ "db": true, "registryPath": registry_path.to_string_lossy() })
    }

    #[test]
    fn workspace_add_registers_new_workspace_inactive() {
        let reg_dir = temp_dir("basic-reg");
        let root = temp_dir("basic-root");
        let registry_path = seed_registry(&reg_dir);

        let r = workspaceAdd(&json!({ "name": "alfa", "root_path": root.to_string_lossy() }), &ctx(&registry_path)).unwrap();

        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["name"], json!("alfa"));
        assert_eq!(r["active"], json!(false));
        assert!(r["db_dir"].as_str().unwrap().len() > 0);

        let reg = Connection::open(&registry_path).unwrap();
        let (n, is_active): (String, i64) = reg.query_row(
            "SELECT name, is_active FROM workspaces WHERE name = 'alfa'", [], |row| Ok((row.get(0)?, row.get(1)?)),
        ).unwrap();
        assert_eq!(n, "alfa");
        assert_eq!(is_active, 0, "registered but not auto-activated");

        let _ = fs::remove_dir_all(&reg_dir);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn workspace_add_reaches_its_real_checks_with_the_dispatcher_context() {
        // THE REGRESSION THIS PINS: the dispatcher passes `ctx.context_value()` — `{actor, source}`,
        // with no `db` key, because a Connection cannot ride inside a JSON Value. The ported
        // `if (!context.db) throw` guard therefore rejected every real dispatch, so
        // system.workspace.add could not succeed even once. A context shaped like the dispatcher's
        // must now reach the ACTUAL validation instead of dying at the door.
        let dispatcher_ctx = json!({ "actor": "human:dani", "source": "web-fe" });
        let e = workspaceAdd(&json!({ "root_path": "C:/x" }), &dispatcher_ctx).unwrap_err();
        assert_eq!(e, "workspace-add: name is required", "must fail on the real precondition, not on a missing db handle");
    }

    #[test]
    fn workspace_add_missing_name_returns_exact_message() {
        let reg_dir = temp_dir("noname-reg");
        let registry_path = seed_registry(&reg_dir);
        let e = workspaceAdd(&json!({ "root_path": "C:/x" }), &ctx(&registry_path)).unwrap_err();
        assert_eq!(e, "workspace-add: name is required");
        let _ = fs::remove_dir_all(&reg_dir);
    }

    #[test]
    fn workspace_add_missing_root_path_returns_exact_message() {
        let reg_dir = temp_dir("noroot-reg");
        let registry_path = seed_registry(&reg_dir);
        let e = workspaceAdd(&json!({ "name": "alfa" }), &ctx(&registry_path)).unwrap_err();
        assert_eq!(e, "workspace-add: root_path is required");
        let _ = fs::remove_dir_all(&reg_dir);
    }

    #[test]
    fn workspace_add_invalid_name_slug_returns_exact_message() {
        let reg_dir = temp_dir("badslug-reg");
        let root = temp_dir("badslug-root");
        let registry_path = seed_registry(&reg_dir);
        let e = workspaceAdd(&json!({ "name": "bad name!", "root_path": root.to_string_lossy() }), &ctx(&registry_path)).unwrap_err();
        assert_eq!(e, "workspace-add: workspace name must be a simple slug ([a-z0-9._-], no spaces): got 'bad name!'");
        let _ = fs::remove_dir_all(&reg_dir);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn workspace_add_nonexistent_root_path_returns_exact_message() {
        let reg_dir = temp_dir("noroot2-reg");
        let registry_path = seed_registry(&reg_dir);
        let missing = temp_dir("noroot2-missing");
        fs::remove_dir_all(&missing).unwrap();
        let e = workspaceAdd(&json!({ "name": "alfa", "root_path": missing.to_string_lossy() }), &ctx(&registry_path)).unwrap_err();
        assert_eq!(e, format!("workspace-add: root_path does not exist on disk: '{}'", missing.to_string_lossy()));
        let _ = fs::remove_dir_all(&reg_dir);
    }

    #[test]
    fn workspace_add_duplicate_name_returns_exact_message() {
        let reg_dir = temp_dir("dup-reg");
        let root1 = temp_dir("dup-root1");
        let root2 = temp_dir("dup-root2");
        let registry_path = seed_registry(&reg_dir);

        workspaceAdd(&json!({ "name": "alfa", "root_path": root1.to_string_lossy() }), &ctx(&registry_path)).unwrap();
        let e = workspaceAdd(&json!({ "name": "alfa", "root_path": root2.to_string_lossy() }), &ctx(&registry_path)).unwrap_err();
        assert_eq!(e, "workspace-add: workspace 'alfa' is already registered");

        let _ = fs::remove_dir_all(&reg_dir);
        let _ = fs::remove_dir_all(&root1);
        let _ = fs::remove_dir_all(&root2);
    }

    // OPERATOR 2026-10-01: registering a workspace left `.antiloki/` as untracked files in the repo's `git status`.
    #[test]
    fn workspace_add_keeps_antiloki_out_of_git_status_through_the_local_exclude_only() {
        let reg_dir = temp_dir("excl-reg");
        let root = temp_dir("excl-root");
        let registry_path = seed_registry(&reg_dir);
        fs::create_dir_all(root.join(".git").join("info")).unwrap();
        fs::write(root.join(".git").join("info").join("exclude"), "# existing\n*.log").unwrap();   // no trailing newline on purpose
        fs::write(root.join(".gitignore"), "node_modules/\n").unwrap();
        let r = workspaceAdd(&json!({ "name": "alfa", "root_path": root.to_string_lossy() }), &ctx(&registry_path)).unwrap();
        assert_eq!(r["git_exclude"], json!("added"));
        let ex = fs::read_to_string(root.join(".git").join("info").join("exclude")).unwrap();
        assert!(ex.contains("*.log\n") && ex.lines().any(|l| l == "/.antiloki/") && !ex.lines().any(|l| l == ".antiloki/"), "the existing lines survive and ours is anchored on its own line: {ex:?}");
        assert_eq!(fs::read_to_string(root.join(".gitignore")).unwrap(), "node_modules/\n", "the tracked .gitignore is never edited");
        // idempotent
        assert_eq!(ensure_git_exclude(&root).unwrap(), "already ignored");
        assert_eq!(fs::read_to_string(root.join(".git").join("info").join("exclude")).unwrap(), ex);
        let _ = fs::remove_dir_all(&reg_dir);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn the_exclude_is_not_written_when_something_already_ignores_the_folder_or_there_is_no_repo() {
        let plain = temp_dir("excl-plain");
        assert_eq!(ensure_git_exclude(&plain).unwrap(), "not a git repository");
        assert!(!plain.join(".git").exists(), "a folder that is not a repo is not turned into one");
        let ignored = temp_dir("excl-ignored");
        fs::create_dir_all(ignored.join(".git")).unwrap();
        fs::write(ignored.join(".gitignore"), "dist\n/.antiloki/\n").unwrap();
        assert_eq!(ensure_git_exclude(&ignored).unwrap(), "already ignored");
        assert!(!ignored.join(".git").join("info").join("exclude").exists(), "nothing written when the repo already ignores it");
        let _ = fs::remove_dir_all(&plain);
        let _ = fs::remove_dir_all(&ignored);
    }

    #[test]
    fn a_linked_worktree_writes_the_exclude_in_the_common_dir() {
        let main = temp_dir("excl-main");
        let wt = temp_dir("excl-wt");
        let gitdir = main.join(".git").join("worktrees").join("wt");
        fs::create_dir_all(&gitdir).unwrap();
        fs::write(gitdir.join("commondir"), "../..\n").unwrap();
        // what git itself writes for a linked worktree: a HEAD, and the path of the worktree's own `.git` file
        fs::write(main.join(".git").join("HEAD"), "ref: refs/heads/main\n").unwrap();
        fs::write(gitdir.join("HEAD"), "ref: refs/heads/wt\n").unwrap();
        fs::write(gitdir.join("gitdir"), format!("{}\n", wt.join(".git").display())).unwrap();
        fs::write(wt.join(".git"), format!("gitdir: {}\n", gitdir.display())).unwrap();
        assert_eq!(ensure_git_exclude(&wt).unwrap(), "added");
        assert!(fs::read_to_string(main.join(".git").join("info").join("exclude")).unwrap().contains("/.antiloki/"));
        let _ = fs::remove_dir_all(&main);
        let _ = fs::remove_dir_all(&wt);
    }

    // ── F-17 (audit 2026-10-01) ───────────────────────────────────────────────────────────────────────────────────────────────────────────────────────
    #[test]
    fn an_exclude_file_that_is_not_utf8_is_appended_to_never_rewritten() {
        let root = temp_dir("excl-bytes");
        fs::create_dir_all(root.join(".git").join("info")).unwrap();
        let original: Vec<u8> = b"# caf\xe9 (latin-1)\n*.log\nsecret-\xff\xfe.txt".to_vec();   // not valid UTF-8, and no trailing newline
        fs::write(root.join(".git").join("info").join("exclude"), &original).unwrap();
        assert_eq!(ensure_git_exclude(&root).unwrap(), "added");
        let after = fs::read(root.join(".git").join("info").join("exclude")).unwrap();
        assert!(after.starts_with(&original), "every original byte is still there, in place: {:?}", String::from_utf8_lossy(&after));
        assert!(String::from_utf8_lossy(&after[original.len()..]).contains("\n/.antiloki/\n"));
        // and a second call recognises its own line in a file that is still not UTF-8
        assert_eq!(ensure_git_exclude(&root).unwrap(), "already ignored");
        assert_eq!(fs::read(root.join(".git").join("info").join("exclude")).unwrap(), after);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn the_line_is_anchored_so_a_nested_antiloki_folder_is_not_hidden() {
        let root = temp_dir("excl-anchor");
        fs::create_dir_all(root.join(".git")).unwrap();
        ensure_git_exclude(&root).unwrap();
        let ex = fs::read_to_string(root.join(".git").join("info").join("exclude")).unwrap();
        assert!(ex.lines().any(|l| l == "/.antiloki/"), "{ex:?}");
        assert!(!ex.lines().any(|l| l.trim() == ".antiloki/" || l.trim() == ".antiloki"), "an unanchored name also matches packages/x/.antiloki: {ex:?}");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_git_file_that_points_at_a_folder_that_is_not_this_checkouts_own_metadata_is_not_followed() {
        let victim = temp_dir("excl-victim");
        // a real-looking git dir with a HEAD, but nothing ties it to the checkout below
        fs::write(victim.join("HEAD"), "ref: refs/heads/main\n").unwrap();
        let root = temp_dir("excl-hostile");
        fs::write(root.join(".git"), format!("gitdir: {}\n", victim.display())).unwrap();
        let e = ensure_git_exclude(&root).unwrap_err();
        assert!(e.contains("does not belong"), "{e}");
        assert!(!victim.join("info").exists(), "nothing was created in the folder the pointer named");
        // a folder that is not even a git directory
        let plain = temp_dir("excl-plain-target");
        fs::write(root.join(".git"), format!("gitdir: {}\n", plain.display())).unwrap();
        assert!(ensure_git_exclude(&root).unwrap_err().contains("not a git directory"));
        assert!(!plain.join("info").exists());
        // a relative pointer that climbs out, and a commondir that is not the repository above the gitdir
        fs::write(root.join(".git"), "gitdir: ../../../../../../../../tmp\n").unwrap();
        assert!(ensure_git_exclude(&root).is_err());
        let main = temp_dir("excl-main2"); let wt = temp_dir("excl-wt2");
        let gitdir = main.join(".git").join("worktrees").join("wt");
        fs::create_dir_all(&gitdir).unwrap();
        fs::write(gitdir.join("HEAD"), "x").unwrap(); fs::write(gitdir.join("gitdir"), format!("{}\n", wt.join(".git").display())).unwrap();
        fs::write(gitdir.join("commondir"), format!("{}\n", victim.display())).unwrap();     // points at the victim, which is not above the gitdir
        fs::write(wt.join(".git"), format!("gitdir: {}\n", gitdir.display())).unwrap();
        assert!(ensure_git_exclude(&wt).unwrap_err().contains("commondir"));
        assert!(!victim.join("info").exists());
        // a `.git` that is a symlink is not touched either
        #[cfg(unix)] {
            let linked = temp_dir("excl-linked");
            std::os::unix::fs::symlink(&victim, linked.join(".git")).unwrap();
            assert!(ensure_git_exclude(&linked).unwrap_err().contains("symlink"));
            assert!(!victim.join("info").exists());
            let _ = fs::remove_dir_all(&linked);
        }
        for d in [&victim, &root, &plain, &main, &wt] { let _ = fs::remove_dir_all(d); }
    }

    #[cfg(unix)]
    #[test]
    fn an_exclude_file_that_is_a_symlink_is_not_written_through() {
        let root = temp_dir("excl-symlink"); let out = temp_dir("excl-symlink-out");
        fs::create_dir_all(root.join(".git").join("info")).unwrap();
        fs::write(out.join("target"), "precious\n").unwrap();
        std::os::unix::fs::symlink(out.join("target"), root.join(".git").join("info").join("exclude")).unwrap();
        assert!(ensure_git_exclude(&root).unwrap_err().contains("symlink"));
        assert_eq!(fs::read_to_string(out.join("target")).unwrap(), "precious\n");
        let _ = fs::remove_dir_all(&root); let _ = fs::remove_dir_all(&out);
    }
}
