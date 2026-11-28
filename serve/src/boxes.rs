//! A WORKTREE PER AGENT, A BRANCH PER EPISODE (operator 2026-09-24: *"worktrees exist to solve parallel work from
//! different agents… So the episodes could be just like new branches in their worktrees?"*).
//!
//! It was not solved. Nine agents shared `/home/user/Projects/ministore`, each carrying a `branch` field naming a
//! ref that did not exist, while the directory itself sat on `main` — so the isolation was a database column with
//! no git behind it, and two agents editing one file would simply overwrite each other.
//!
//! The shape here is the operator's, and it is the right one: **one worktree per agent** (a checkout is expensive,
//! and per-episode would make the cost unbounded), **one branch per episode** (branches are free, and it makes
//! closing an episode a literal `git merge` rather than a metaphor). `node_modules` is linked from the host, not
//! copied, because a per-agent install is minutes and gigabytes each and without it the test/build/lint gates
//! cannot run in a box at all.
//!
//! Opt-in per agent. The default is still the shared tree, because that is what makes a change visible in the
//! operator's running preview the second an agent writes it — the best moment in the product, and one an isolated
//! box takes away until the merge.
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

/// `~/.antiloki/agents` (or `$ANTILOKI_HOME/agents`) — beside `worktrees/`, not inside it, so the old
/// per-pane boxes and these can be told apart by a human looking at the directory.
pub fn boxes_dir() -> PathBuf {
    if let Ok(h) = std::env::var("ANTILOKI_HOME") { if !h.is_empty() { return PathBuf::from(h).join("agents") } }
    let home = std::env::var("HOME").or_else(|_| std::env::var("USERPROFILE")).unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(".antiloki").join("agents")
}

/// A slug as a path and a ref may both hold it. Git refs forbid a lot; agents are named by people. Pure — tested.
pub fn safe_slug(slug: &str) -> String {
    let mut out = String::new();
    for c in slug.trim().chars() {
        let c = if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c.to_ascii_lowercase() } else { '-' };
        // runs collapse: "Kimi × DeepSeek" turns three characters into three dashes otherwise
        if c == '-' && out.ends_with('-') { continue }
        out.push(c);
    }
    let out = out.trim_matches('-').to_string();
    if out.is_empty() { "agent".into() } else { out.chars().take(48).collect::<String>().trim_matches('-').to_string() }
}
/// Two workspaces can both have a "backender". The box is keyed by the workspace as well, so they never share
/// a checkout. Pure — tested.
pub fn box_of(root: &str, slug: &str) -> PathBuf {
    let key = {
        // a short stable digest of the workspace path — FNV-1a, because this needs to be stable across runs and
        // across versions, and a hash that changes would strand every existing box
        let mut h: u64 = 0xcbf29ce484222325;
        for b in root.as_bytes() { h ^= *b as u64; h = h.wrapping_mul(0x100000001b3); }
        format!("{h:x}")
    };
    let name = Path::new(root).file_name().and_then(|x| x.to_str()).unwrap_or("workspace");
    boxes_dir().join(format!("{}-{}", safe_slug(name), &key[..8])).join(safe_slug(slug))
}
/// The agent's long-lived branch — where its episodes merge back to.
///
/// It is `antiloki/<slug>/main`, not `antiloki/<slug>`, and the extra segment is load-bearing: git refs are a
/// directory tree on disk, so `refs/heads/antiloki/dani` is a FILE and `refs/heads/antiloki/dani/ep-1` can never
/// exist beside it — git refuses with "cannot lock ref … exists". Giving the agent's own branch a leaf name makes
/// `antiloki/dani` a directory, and every episode fits under it. Pure — tested.
pub fn agent_branch(slug: &str) -> String { format!("antiloki/{}/main", safe_slug(slug)) }
/// One episode's branch. Pure — tested.
pub fn episode_branch(slug: &str, n: usize) -> String { format!("antiloki/{}/ep-{n}", safe_slug(slug)) }

/// THE COMMITS THIS PANE MADE, parsed from one `git log`.
///
/// Operator 2026-09-25: *"on the sidebar right of the agent pane should have the icon to see the diff done on
/// that pane, and another to see the commits done on that pane. Only on that pane."*
///
/// `%x1f` (unit separator) between fields and `%x1e` (record separator) between commits, because a subject
/// line may contain anything — a tab, a pipe, a newline of its own — and splitting on a character an operator
/// can type is how a log parser starts inventing commits. Pure — tested.
pub fn parse_commits(out: &str) -> Vec<Value> {
    out.split('\x1e').filter_map(|rec| {
        let rec = rec.trim_start_matches('\n');
        if rec.trim().is_empty() { return None }
        let f: Vec<&str> = rec.split('\x1f').collect();
        if f.len() < 4 { return None }
        Some(json!({ "sha": f[0].trim(), "when": f[1].trim(), "who": f[2].trim(), "subject": f[3].trim() }))
    }).collect()
}

/// The last `n` commits on the branch this worktree is standing on.
pub fn commits(cwd: &str, n: usize) -> Vec<Value> {
    let fmt = format!("--pretty=format:%h%x1f%ad%x1f%an%x1f%s%x1e");
    let out = crate::proc::cmd("git")
        .args(["log", &format!("-{n}"), "--date=format:%Y-%m-%d %H:%M", &fmt])
        .current_dir(cwd).output();
    match out { Ok(o) if o.status.success() => parse_commits(&String::from_utf8_lossy(&o.stdout)), _ => vec![] }
}

pub(crate) fn git(args: &[&str], cwd: &str) -> Result<String, String> {
    let out = crate::proc::cmd("git").args(args).current_dir(cwd).output()
        .map_err(|e| format!("git could not run: {e}"))?;
    let so = String::from_utf8_lossy(&out.stdout).to_string();
    if out.status.success() { return Ok(so) }
    Err(format!("git {}: {}", args.first().copied().unwrap_or(""), String::from_utf8_lossy(&out.stderr).trim().chars().take(300).collect::<String>()))
}
fn git_ok(args: &[&str], cwd: &str) -> bool { git(args, cwd).is_ok() }

/// The paths antiloki writes into somebody's repo — never part of what an agent did, and never its commit. Used
/// as git exclude pathspecs when LISTING what an episode changed; for staging, the box's own `info/exclude` does
/// the work (a pathspec that names an ignored path makes git refuse the add outright).
pub const OURS: &[&str] = &[":!AGENTS.md", ":!CLAUDE.md", ":!opencode.json", ":!.claude", ":!.codex", ":!.opencode", ":!.agents", ":!.antiloki", ":!.antiloki-*"];

/// IS THIS PATH ONE OF THE FILES ANTILOKI ITSELF WROTE INTO THE PROJECT? (2026-10-09: the QA judge was shown "WHAT CHANGED: only .agents and .cursor skill files" on every round of a minimalist-design
/// run - antiloki's own context skills, untracked, sorted before `src/` and cut into the 6000-character window - so it answered "the source was not changed" five times.) Only OUR files: a project's own
/// `.cursor/rules/my.mdc` is not one. Pure - tested.
pub fn is_ours(rel: &str) -> bool {
    let p = rel.trim().trim_start_matches("./");
    if matches!(p, "AGENTS.md" | "CLAUDE.md" | "opencode.json" | ".cursor/mcp.json" | ".grok/config.toml" | ".codex/hooks.json" | ".claude/settings.local.json") { return true }
    p.split('/').any(|c| c.starts_with("antiloki") || c.starts_with(".antiloki"))
}

/// The branch the workspace is on — what an episode merges back into. Falls back to `main`.
pub fn base_branch(root: &str) -> String {
    git(&["rev-parse", "--abbrev-ref", "HEAD"], root).map(|s| s.trim().to_string())
        .ok().filter(|b| !b.is_empty() && b != "HEAD").unwrap_or_else(|| "main".into())
}

/// THE AGENT'S BOX, made once and reused for every episode.
///
/// `node_modules` is SYMLINKED from the host rather than installed. A per-agent install costs minutes and a
/// gigabyte each, and the whole reason the previous worktree design was abandoned is that its boxes had no
/// dependencies — which silently made the test, build and lint gates unrunnable there.
pub fn ensure_box(root: &str, slug: &str) -> Result<PathBuf, String> { ensure_box_with(root, slug, |_| {}) }

/// …AND THE SAME, SAYING WHAT IT IS DOING (operator 2026-09-27: "a loading should show with real time info of
/// creating the worktree … till the worktree is ready and safe to work on").
///
/// Cutting a box is `git worktree add`, then the `node_modules` symlink, then the local excludes — seconds on a cold
/// repo, during which the tab exists and the box does NOT. The steps are reported as they start, so the screen can
/// say what is actually happening rather than spin. `on_step` is called with a short key; naming it is the caller's
/// business, because a bare function should not decide how a product talks.
pub fn ensure_box_with(root: &str, slug: &str, on_step: impl Fn(&str)) -> Result<PathBuf, String> {
    let path = box_of(root, slug);
    // an existing box in a repository that still has no commits may have been cut empty before seeding existed: it is healed on its next open
    if path.join(".git").exists() { let _ = seed_empty_box(root, &path); on_step("ready"); return Ok(path) }
    std::fs::create_dir_all(path.parent().ok_or("no parent for the box")?).map_err(|e| e.to_string())?;
    let branch = agent_branch(slug);
    let p = path.to_string_lossy().to_string();
    on_step("branch");
    // an existing branch is checked out; a new one is cut from where the workspace is now
    if git_ok(&["rev-parse", "--verify", "--quiet", &branch], root) {
        git(&["worktree", "add", &p, &branch], root)?;
    } else {
        git(&["worktree", "add", "-b", &branch, &p], root)?;
    }
    // A PROJECT THAT HAS NEVER BEEN COMMITTED HAS NOTHING TO CUT A BOX FROM (operator 2026-10-09: a new agent's preview failed with "Could not read package.json" - the workspace's files were all
    // untracked, `git worktree add` took the unborn HEAD, and the box was empty). The box is seeded with the workspace's own files and one starting commit.
    on_step("seed");
    let _ = seed_empty_box(root, &path);
    on_step("deps");
    link_node_modules(root, &path);
    on_step("excludes");
    write_local_excludes(&path);
    on_step("ready");
    Ok(path)
}

/// HEAL A BOX WHERE ITS WORK STARTS (operator 2026-10-09: a fork cut before seeding existed stayed empty, and re-opening its tab did not repair it): find the repository a worktree belongs to and seed the box
/// when that repository has no commits. A folder that is not a worktree of a repository (the shared checkout itself, a plain folder) is left alone. Returns the number of files copied.
pub fn heal_in_place(box_path: &str) -> usize {
    let p = Path::new(box_path);
    if !p.join(".git").is_file() { return 0 }        // a worktree's `.git` is a file; the main checkout's is a directory
    let Ok(common) = git(&["rev-parse", "--path-format=absolute", "--git-common-dir"], box_path) else { return 0 };
    let common = common.trim();
    let Some(root) = Path::new(common).parent().filter(|_| common.ends_with("/.git")).map(|r| r.to_string_lossy().to_string()) else { return 0 };
    write_local_excludes(p);   // an existing box also learns the entries added since it was cut (`.cursor/`)
    seed_empty_box(&root, p)
}

/// Does the repository at `root` have a commit to cut a box from?
pub fn has_commits(root: &str) -> bool { git_ok(&["rev-parse", "--verify", "--quiet", "HEAD"], root) }

/// THE FILES A SEED COPIES: what git would track in `root` right now (tracked + untracked, `.gitignore` honoured), minus antiloki's own files and dependencies - those are the box's to make. Pure over the list - tested.
pub fn seedable(files: &[String]) -> Vec<String> {
    files.iter().filter(|f| {
        let f = f.trim_start_matches("./");
        let top = f.split('/').next().unwrap_or("");
        !f.is_empty() && !matches!(top, "node_modules" | ".git" | ".antiloki" | ".agents" | ".claude" | ".codex" | ".opencode" | ".cursor" | "target" | "dist" | "build" | ".next" | ".cache")
            && !matches!(f, "AGENTS.md" | "CLAUDE.md" | "opencode.json") && !f.starts_with(".antiloki-")
    }).cloned().collect()
}

/// A box cut from a repository with NO commits starts empty: copy the workspace's files into it and make the one commit that gives checkpoints and diffs a baseline. Does nothing (returns 0) when the
/// repository HAS commits (the box is already what HEAD says) or when the box already holds files. Returns the number of files copied.
pub fn seed_empty_box(root: &str, box_path: &Path) -> usize {
    if has_commits(root) { return 0 }
    let b = box_path.to_string_lossy().to_string();
    // already holds the project's own files (antiloki's seat files and the dependency link do not count): nothing to seed
    if std::fs::read_dir(box_path).map(|d| d.flatten().any(|e| !seedable(&[e.file_name().to_string_lossy().to_string()]).is_empty())).unwrap_or(false) { return 0 }
    let Ok(listing) = git(&["ls-files", "--cached", "--others", "--exclude-standard", "-z"], root) else { return 0 };
    let all: Vec<String> = listing.split('\0').filter(|x| !x.is_empty()).map(String::from).collect();
    let mut n = 0;
    for f in seedable(&all) {
        let (from, to) = (Path::new(root).join(&f), box_path.join(&f));
        if !from.is_file() { continue }
        if let Some(d) = to.parent() { let _ = std::fs::create_dir_all(d); }
        if std::fs::copy(&from, &to).is_ok() { n += 1; }
    }
    if n > 0 {
        write_local_excludes(box_path);
        let _ = git(&["add", "-A"], &b);
        let _ = git(&["-c", "user.name=antiloki", "-c", "user.email=antiloki@localhost", "commit", "-q", "-m", "antiloki: starting point - the workspace had no commits"], &b);
    }
    n
}

/// TEACH THE BOX TO IGNORE OUR OWN FILES, locally.
///
/// The first real close failed with `git add: The following paths are ignored by one of your .gitignore files:
/// .antiloki` — an exclude PATHSPEC makes an ignored path explicit, and git then refuses the whole add rather
/// than skipping it. The fix is not a cleverer pathspec: it is for the box to ignore these the way it ignores
/// anything else. `info/exclude` is per-worktree and never touches the operator's `.gitignore`, so a repository
/// that has its own opinion about `.antiloki` keeps it.
pub fn write_local_excludes(box_path: &Path) -> bool {
    let p = box_path.to_string_lossy().to_string();
    let Ok(rel) = git(&["rev-parse", "--git-path", "info/exclude"], &p) else { return false };
    let rel = rel.trim();
    if rel.is_empty() { return false }
    let file = if Path::new(rel).is_absolute() { PathBuf::from(rel) } else { box_path.join(rel) };
    let Some(dir) = file.parent() else { return false };
    let _ = std::fs::create_dir_all(dir);
    let have = std::fs::read_to_string(&file).unwrap_or_default();
    // EVERY ENTRY THE BOX NEEDS, ADDED WHEN MISSING (2026-10-09): the block was written once and never again, so an entry added later (`.cursor/`, which a Cursor agent writes into every box and which the
    // close dialog then listed as "1 uncommitted file") could never reach a box or a workspace that already had the marker.
    let missing: Vec<&str> = LOCAL_EXCLUDES.iter().copied().filter(|e| !have.lines().any(|l| l.trim() == *e)).collect();
    if missing.is_empty() { return true }
    let mut out = have.clone();
    if !have.contains(MARK) { out.push_str(&format!("\n{MARK}\n")); } else if !out.ends_with('\n') { out.push('\n'); }
    for e in missing { out.push_str(e); out.push('\n'); }
    std::fs::write(&file, out).is_ok()
}
/// What a box ignores locally: antiloki's own files and the seats' own folders - never part of what an agent did.
const LOCAL_EXCLUDES: &[&str] = &["AGENTS.md", "CLAUDE.md", "opencode.json", ".claude/", ".codex/", ".opencode/", ".agents/", ".cursor/", ".antiloki", ".antiloki/", ".antiloki-*", "node_modules"];
const MARK: &str = "# antiloki's own files — never part of what an agent did";

/// THE BOX, READY TO WORK: made if it does not exist, and standing on episode `n`'s own branch.
///
/// `ensure_box` alone leaves the agent on its trunk, and an agent that works on its trunk has nothing to close —
/// the first real run of this ended with three agents' work committed to `antiloki/<slug>/main` and a merge that
/// said "not something we can merge", because episode 1's branch had never been cut. Opening a pane and opening
/// its first episode are one act.
pub fn ensure_box_for(root: &str, slug: &str, n: usize) -> Result<PathBuf, String> {
    let path = ensure_box(root, slug)?;
    start_episode(&path, slug, n)?;
    Ok(path)
}

/// Link the host's dependencies into the box. Best effort: a project without `node_modules` is not an error, and
/// a platform that can link neither way just means the gates report "dependencies are not installed", which is
/// honest.
pub fn link_node_modules(root: &str, box_path: &Path) -> bool {
    let src = Path::new(root).join("node_modules");
    let dst = box_path.join("node_modules");
    if !src.is_dir() || dst.exists() { return false }
    #[cfg(unix)]
    { std::os::unix::fs::symlink(&src, &dst).is_ok() }
    #[cfg(windows)]
    { link_dir_windows(&src, &dst) }
    #[cfg(not(any(unix, windows)))]
    { let _ = (src, dst); false }
}

/// A DIRECTORY LINK ON WINDOWS, without asking the user for privileges.
///
/// This arm used to be `false`: nothing was linked, so an agent's box on Windows had NO dependencies at all and
/// the test, build and lint gates could not run inside it — precisely the failure the previous worktree design
/// was abandoned over (see `ensure_box`), reintroduced on the one platform the operator actually runs. Found
/// 2026-09-25 by `an_agent_gets_one_box_and_an_episode_gets_one_branch` failing here.
///
/// A symlink is tried first: it is free and spawns nothing. But `CreateSymbolicLinkW` demands
/// SeCreateSymbolicLinkPrivilege, which an ordinary account holds only with Developer Mode switched on — so on
/// most machines that attempt fails and is not the answer. A JUNCTION needs no privilege whatsoever and is what
/// `mklink /J` makes; for a `node_modules` on a local disk the two are equivalent. `mklink` is a `cmd` builtin,
/// hence the `/C`, and it goes through `proc::cmd` so the console it would otherwise flash stays hidden.
#[cfg(windows)]
fn link_dir_windows(src: &Path, dst: &Path) -> bool {
    if std::os::windows::fs::symlink_dir(src, dst).is_ok() { return true }
    // mklink's argument order is LINK then TARGET, which is the reverse of `ln -s`
    let made = crate::proc::cmd("cmd").arg("/C").arg("mklink").arg("/J").arg(dst).arg(src)
        .output().map(|o| o.status.success()).unwrap_or(false);
    made && dst.exists()
}

/// Open an episode: a branch of its own, cut from wherever the agent's branch is now.
pub fn start_episode(box_path: &Path, slug: &str, n: usize) -> Result<String, String> {
    let p = box_path.to_string_lossy().to_string();
    let b = episode_branch(slug, n);
    if git_ok(&["rev-parse", "--verify", "--quiet", &b], &p) { git(&["checkout", &b], &p)?; }
    else { git(&["checkout", "-b", &b], &p)?; }
    Ok(b)
}

/// WHAT AN EPISODE DID, ready to be merged. Uncommitted work is committed first — an agent does not commit, and
/// an episode's changes are worthless as a merge while they sit in the working tree.
pub fn seal_episode(box_path: &Path, slug: &str, n: usize, title: &str) -> Result<Value, String> {
    let p = box_path.to_string_lossy().to_string();
    // STAND ON THE EPISODE BEFORE COMMITTING IT. A box can be on the wrong branch — a close that failed halfway,
    // or a box made before episodes had branches — and committing there puts the work on the agent's trunk, where
    // it is safe but has nothing left to merge. `start_episode` checks out or creates, carrying the working tree
    // with it, so sealing is self-healing rather than dependent on the box being where we left it.
    start_episode(box_path, slug, n)?;
    let dirty = !git(&["status", "--porcelain", "--untracked-files=all"], &p)?.trim().is_empty();
    if dirty {
        // the box ignores antiloki's own files locally (see `write_local_excludes`), so a plain add is correct
        // and cannot be refused for naming an ignored path. Boxes made before that existed get it now.
        write_local_excludes(box_path);
        git(&["add", "-A", "--", "."], &p)?;
        let msg = format!("{}\n\nepisode {n} of the agent {slug}, closed by antiloki.", if title.trim().is_empty() { format!("episode {n}") } else { title.trim().chars().take(72).collect::<String>() });
        git(&["commit", "-m", &msg], &p)?;
    }
    let branch = episode_branch(slug, n);
    let spec = format!("{}...{branch}", agent_branch(slug));
    let mut args: Vec<&str> = vec!["diff", "--name-only", &spec, "--", "."];
    args.extend(OURS.iter().copied());
    let files: Vec<String> = git(&args, &p)
        .unwrap_or_default().lines().map(str::trim).filter(|l| !l.is_empty()).map(String::from).collect();
    Ok(json!({ "branch": branch, "committed": dirty, "files": files }))
}

/// CLOSING AN EPISODE IS A MERGE (the operator's own words). Into the agent's branch first — that always
/// fast-forwards, because nothing else writes there — and then, if asked, into the workspace's own branch, which
/// is where a conflict can genuinely happen and is reported rather than forced.
pub fn merge_episode(root: &str, box_path: &Path, slug: &str, n: usize, to_workspace: bool) -> Result<Value, String> {
    let p = box_path.to_string_lossy().to_string();
    let (agent, ep) = (agent_branch(slug), episode_branch(slug, n));
    if !git_ok(&["rev-parse", "--verify", "--quiet", &ep], &p) {
        // the agent worked on its own trunk — there is no episode branch to merge, and git's own message for
        // that ("not something we can merge") tells the operator nothing about what actually happened
        return Ok(json!({ "ok": false, "merged_into": Value::Null, "workspace": Value::Null,
            "note": format!("episode {n} was never opened as a branch, so there is nothing to merge. The work is committed on {agent} and is not lost.") }));
    }
    git(&["checkout", &agent], &p)?;
    git(&["merge", "--no-edit", &ep], &p).map_err(|e| format!("the episode would not merge into the agent's own branch: {e}"))?;
    if !to_workspace { return Ok(json!({ "ok": true, "merged_into": agent, "workspace": Value::Null })) }
    let base = base_branch(root);
    // A DIRTY TREE IS NOT A CONFLICT (2026-09-24). Git refuses to merge into a working tree with uncommitted
    // changes, and reporting that as "the episode conflicts" sends the operator looking for a collision that does
    // not exist. It is their own unsaved work, and the fix is theirs to choose.
    if let Ok(st) = git(&["status", "--porcelain", "--untracked-files=no"], root) {
        if !st.trim().is_empty() {
            return Ok(json!({ "ok": false, "merged_into": agent, "workspace": base, "dirty": true,
                "note": format!("your workspace has uncommitted changes, so git will not merge into it. The episode is safe on {agent} — commit or stash what you have, then: git merge {agent}") }));
        }
    }
    match git(&["merge", "--no-edit", &agent], root) {
        Ok(_) => Ok(json!({ "ok": true, "merged_into": agent, "workspace": base })),
        Err(e) => {
            // leave the workspace exactly as it was: a half-merged tree is the worst possible thing to hand back
            let _ = git(&["merge", "--abort"], root);
            Ok(json!({ "ok": false, "merged_into": agent, "workspace": base, "conflict": e,
                       "note": format!("the episode is safe on {agent}; the workspace was left untouched. Merge it yourself when you are ready: git merge {agent}") }))
        }
    }
}

/// Give the box back. The branches stay — they are the record of what the agent did.
pub fn remove_box(root: &str, slug: &str) -> Result<(), String> {
    let path = box_of(root, slug);
    if !path.exists() { return Ok(()) }
    git(&["worktree", "remove", "--force", &path.to_string_lossy()], root).map(|_| ())
}

#[cfg(test)]
mod tests {

    /// A SUBJECT LINE CAN CONTAIN ANYTHING — this is why the format uses separators an operator cannot type.
    #[test]
    fn commits_parse_even_when_the_message_contains_the_obvious_delimiters() {
        let out = "a1b2c3\x1f2026-09-25 14:02\x1fdani\x1ffix: the gate | tab\there, and a pipe\x1e\nd4e5f6\x1f2026-09-25 13:40\x1fdani\x1fplain subject\x1e";
        let c = parse_commits(out);
        assert_eq!(c.len(), 2);
        assert_eq!(c[0]["sha"], json!("a1b2c3"));
        assert_eq!(c[0]["when"], json!("2026-09-25 14:02"));
        assert_eq!(c[0]["who"], json!("dani"));
        assert_eq!(c[0]["subject"], json!("fix: the gate | tab\there, and a pipe"),
            "a pipe or a tab in the subject must not split a commit in two");
        assert_eq!(c[1]["subject"], json!("plain subject"));
    }

    #[test]
    fn a_worktree_with_no_commits_is_an_empty_list_not_a_phantom_one() {
        assert!(parse_commits("").is_empty());
        assert!(parse_commits("\n").is_empty());
        // a truncated record — git killed mid-write — is dropped rather than half-read
        assert!(parse_commits("a1b2c3\x1f2026-09-25\x1e").is_empty());
    }
    use super::*;

    #[test]
    fn the_local_excludes_gain_an_entry_added_later_without_duplicating_the_old_ones() {
        let d = std::env::temp_dir().join(format!("antiloki-excl-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        std::fs::create_dir_all(&d).unwrap(); let r = d.to_string_lossy().to_string();
        git(&["init", "-q", "-b", "main"], &r).unwrap();
        let file = d.join(".git/info/exclude");
        // a box written before `.cursor/` was on the list: marker and the old entries present
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, format!("# mine\n*.log\n\n{MARK}\nAGENTS.md\nCLAUDE.md\nopencode.json\n.claude/\n.codex/\n.opencode/\n.agents/\n.antiloki\n.antiloki/\n.antiloki-*\nnode_modules\n")).unwrap();
        assert!(write_local_excludes(&d));
        let now = std::fs::read_to_string(&file).unwrap();
        assert!(now.lines().any(|l| l == ".cursor/"), "the new entry arrives: {now}");
        assert!(now.contains("*.log") && now.matches("node_modules").count() == 1 && now.matches(MARK).count() == 1, "the operator's own line stays, nothing is duplicated: {now}");
        let before = now.clone(); assert!(write_local_excludes(&d)); assert_eq!(std::fs::read_to_string(&file).unwrap(), before, "idempotent");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn an_empty_box_cut_before_seeding_existed_is_healed_even_with_the_agents_seat_files_in_it() {
        let d = std::env::temp_dir().join(format!("antiloki-heal-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        let root = d.join("proj"); std::fs::create_dir_all(&root).unwrap(); let r = root.to_string_lossy().to_string();
        git(&["init", "-q", "-b", "main"], &r).unwrap(); std::fs::write(root.join("package.json"), "{}").unwrap();
        let b = d.join("box"); let bs = b.to_string_lossy().to_string();
        git(&["worktree", "add", "-b", "antiloki/a/main", &bs], &r).unwrap();
        std::fs::write(b.join("AGENTS.md"), "x").unwrap(); std::fs::write(b.join("opencode.json"), "{}").unwrap(); std::fs::create_dir_all(b.join("node_modules")).unwrap();   // what the stray agent's box held
        assert_eq!(seed_empty_box(&r, &b), 1);
        assert!(b.join("package.json").is_file());
        let _ = git(&["worktree", "remove", "--force", &bs], &r); let _ = std::fs::remove_dir_all(&d);
    }
    #[test]
    fn a_box_is_healed_in_place_from_its_own_worktree_without_being_told_the_root() {
        let d = std::env::temp_dir().join(format!("antiloki-inplace-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        let root = d.join("proj"); std::fs::create_dir_all(&root).unwrap(); let r = root.to_string_lossy().to_string();
        git(&["init", "-q", "-b", "main"], &r).unwrap(); std::fs::write(root.join("package.json"), "{}").unwrap();
        let b = d.join("box"); let bs = b.to_string_lossy().to_string();
        git(&["worktree", "add", "-b", "antiloki/a/main", &bs], &r).unwrap();
        assert_eq!(heal_in_place(&bs), 1);
        assert!(b.join("package.json").is_file());
        assert_eq!(heal_in_place(&r), 0, "the main checkout is not a worktree: left alone");
        assert_eq!(heal_in_place(&d.to_string_lossy()), 0, "a plain folder is left alone");
        let _ = git(&["worktree", "remove", "--force", &bs], &r); let _ = std::fs::remove_dir_all(&d);
    }
    #[test]
    fn seedable_keeps_the_projects_files_and_drops_antilokis_and_the_dependencies() {
        let v = |x: &[&str]| x.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(seedable(&v(&["package.json", "src/App.tsx", "node_modules/x/index.js", ".antiloki/db/a", "AGENTS.md", ".cursor/rules", "dist/a.js", "./index.html", ".antiloki-tmp", "README.md"])),
                   v(&["package.json", "src/App.tsx", "./index.html", "README.md"]));
    }
    /// the operator's report: a new agent in a never-committed project had an empty box, so its preview died on "Could not read package.json"
    #[test]
    fn a_box_cut_from_a_repository_with_no_commits_is_seeded_with_the_workspaces_files_and_one_commit() {
        let d = std::env::temp_dir().join(format!("antiloki-seed-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        let root = d.join("proj"); std::fs::create_dir_all(root.join("src")).unwrap(); std::fs::create_dir_all(root.join("node_modules/dep")).unwrap();
        let r = root.to_string_lossy().to_string();
        git(&["init", "-q", "-b", "main"], &r).unwrap();
        std::fs::write(root.join("package.json"), "{\"name\":\"x\"}").unwrap(); std::fs::write(root.join("src/App.tsx"), "export {}").unwrap();
        std::fs::write(root.join(".gitignore"), "secret.txt\n").unwrap(); std::fs::write(root.join("secret.txt"), "no").unwrap(); std::fs::write(root.join("node_modules/dep/i.js"), "x").unwrap();
        assert!(!has_commits(&r));
        let b = d.join("box"); let bs = b.to_string_lossy().to_string();
        git(&["worktree", "add", "-b", "antiloki/a/main", &bs], &r).unwrap();
        assert!(!b.join("package.json").exists(), "the unborn HEAD gave an empty box - the reported bug");
        assert_eq!(seed_empty_box(&r, &b), 3, "package.json, src/App.tsx and .gitignore");
        assert!(b.join("package.json").is_file() && b.join("src/App.tsx").is_file());
        assert!(!b.join("secret.txt").exists() && !b.join("node_modules").exists(), "ignored files and dependencies are not copied");
        assert!(has_commits(&bs), "one starting commit, so checkpoints have a baseline");
        assert_eq!(seed_empty_box(&r, &b), 0, "a box that already holds files is left alone");
        let _ = git(&["worktree", "remove", "--force", &bs], &r); let _ = std::fs::remove_dir_all(&d);
    }
    #[test]
    fn a_repository_with_commits_is_not_seeded() {
        let d = std::env::temp_dir().join(format!("antiloki-noseed-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        std::fs::create_dir_all(&d).unwrap(); let r = d.to_string_lossy().to_string();
        git(&["init", "-q", "-b", "main"], &r).unwrap(); std::fs::write(d.join("a.txt"), "a").unwrap();
        git(&["add", "-A"], &r).unwrap(); git(&["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qm", "base"], &r).unwrap();
        assert!(has_commits(&r));
        let b = d.join("box"); std::fs::create_dir_all(&b).unwrap();
        assert_eq!(seed_empty_box(&r, &b), 0);
        let _ = std::fs::remove_dir_all(&d);
    }


    static NTH: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    fn repo() -> PathBuf {
        let n = NTH.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let d = std::env::temp_dir().join(format!("antiloki-box-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("src")).unwrap();
        let r = |a: &[&str]| { let _ = crate::proc::cmd("git").args(a).current_dir(&d).output(); };
        r(&["init", "-q", "-b", "main"]); r(&["config", "user.email", "t@t"]); r(&["config", "user.name", "t"]);
        // …and the line endings, for the same reason as the identity: this repo must not inherit the developer's
        // config. `core.autocrlf` is TRUE by default in Git for Windows, so every checkout git made below
        // rewrote the fixtures' "\n" to "\r\n" and the byte-exact assertions failed on that machine alone
        // (2026-09-25) — the merge semantics they are actually about were correct throughout.
        r(&["config", "core.autocrlf", "false"]);
        std::fs::write(d.join("src/a.ts"), "export const a = 1;\n").unwrap();
        r(&["add", "-A"]); r(&["commit", "-qm", "base"]);
        d
    }

    #[test]
    fn names_survive_being_turned_into_paths_and_refs() {
        assert_eq!(safe_slug("Daniboy"), "daniboy");
        assert_eq!(safe_slug("  Kimi × DeepSeek Flash · editor-L "), "kimi-deepseek-flash-editor-l");
        assert_eq!(safe_slug("../../etc"), "etc", "a slug can never climb out of the boxes directory");
        assert_eq!(safe_slug(""), "agent");
        assert_eq!(safe_slug("!!!"), "agent");
        assert_eq!(agent_branch("Daniboy"), "antiloki/daniboy/main");
        // the leaf matters: refs are a directory tree, so a branch named `antiloki/daniboy` would make every
        // `antiloki/daniboy/ep-N` impossible — git refuses to create a ref under a ref that is a file
        assert!(episode_branch("Daniboy", 1).starts_with("antiloki/daniboy/"));
        assert_ne!(agent_branch("Daniboy"), "antiloki/daniboy");
        assert_eq!(episode_branch("Daniboy", 3), "antiloki/daniboy/ep-3");
        // two workspaces, the same agent name, two different boxes
        assert_ne!(box_of("/home/a/ministore", "backender"), box_of("/home/b/ministore", "backender"));
        assert_eq!(box_of("/x/ministore", "backender"), box_of("/x/ministore", "Backender"), "and the same one twice");
    }

    #[test]
    fn an_agent_gets_one_box_and_an_episode_gets_one_branch() {
        let d = repo(); let root = d.to_string_lossy().to_string();
        std::fs::create_dir_all(d.join("node_modules/left-pad")).unwrap();

        let b = ensure_box(&root, "Daniboy").expect("the box is made");
        assert!(b.join("src/a.ts").is_file(), "the box is a real checkout");
        assert!(b.join("node_modules/left-pad").exists(), "the host's dependencies are linked, so the gates can run");
        assert_eq!(ensure_box(&root, "Daniboy").unwrap(), b, "asking twice returns the same box, it is not remade");

        let br = start_episode(&b, "Daniboy", 1).unwrap();
        assert_eq!(br, "antiloki/daniboy/ep-1");
        // …and unlike the branch fields this replaces, the ref actually exists
        assert!(git_ok(&["rev-parse", "--verify", "--quiet", &br], &b.to_string_lossy()));

        let _ = remove_box(&root, "Daniboy");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn closing_an_episode_is_a_merge_and_the_workspace_keeps_working_meanwhile() {
        let d = repo(); let root = d.to_string_lossy().to_string();
        let b = ensure_box(&root, "dani").unwrap();
        start_episode(&b, "dani", 1).unwrap();

        // the agent works in its box; the operator edits a DIFFERENT file in the workspace at the same time
        std::fs::write(b.join("src/agent.ts"), "export const fromAgent = 1;\n").unwrap();
        std::fs::write(d.join("src/human.ts"), "export const fromHuman = 1;\n").unwrap();
        let _ = crate::proc::cmd("git").args(["add", "-A"]).current_dir(&d).output();
        let _ = crate::proc::cmd("git").args(["commit", "-qm", "the human's own commit"]).current_dir(&d).output();

        let sealed = seal_episode(&b, "dani", 1, "added the agent file").unwrap();
        assert_eq!(sealed["committed"], json!(true));
        assert_eq!(sealed["files"], json!(["src/agent.ts"]), "only what the episode did");

        let m = merge_episode(&root, &b, "dani", 1, true).unwrap();
        assert_eq!(m["ok"], json!(true), "{m}");
        assert!(d.join("src/agent.ts").is_file(), "the agent's work arrived in the workspace");
        assert!(d.join("src/human.ts").is_file(), "and the operator's own work is still there");

        let _ = remove_box(&root, "dani");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_conflict_leaves_the_workspace_exactly_as_it_was() {
        let d = repo(); let root = d.to_string_lossy().to_string();
        let b = ensure_box(&root, "dani").unwrap();
        start_episode(&b, "dani", 1).unwrap();

        // both touch the same line — the one case a merge cannot decide
        std::fs::write(b.join("src/a.ts"), "export const a = 2; // the agent\n").unwrap();
        std::fs::write(d.join("src/a.ts"), "export const a = 3; // the human\n").unwrap();
        let _ = crate::proc::cmd("git").args(["commit", "-qam", "the human's edit"]).current_dir(&d).output();

        seal_episode(&b, "dani", 1, "changed a").unwrap();
        let m = merge_episode(&root, &b, "dani", 1, true).unwrap();
        assert_eq!(m["ok"], json!(false));
        assert!(m["conflict"].as_str().unwrap().len() > 0);
        assert!(m["note"].as_str().unwrap().contains("the workspace was left untouched"));
        // the operator's file is untouched, and git is not left mid-merge
        assert_eq!(std::fs::read_to_string(d.join("src/a.ts")).unwrap(), "export const a = 3; // the human\n");
        assert!(!d.join(".git/MERGE_HEAD").exists(), "no half-finished merge left behind");
        // and the work is not lost — it is on the agent's branch, which is what the note promises
        assert!(git_ok(&["rev-parse", "--verify", "--quiet", "antiloki/dani/main"], &root));

        let _ = remove_box(&root, "dani");
        let _ = std::fs::remove_dir_all(&d);
    }

    // THE GAP A REAL RUN FOUND (2026-09-24): three agents opened, worked, and closed — and every close failed
    // with git's "not something we can merge", because a box stands on its trunk until somebody cuts the
    // episode's branch and nobody did. Opening the box and opening episode 1 are one act now.
    #[test]
    fn a_box_is_opened_standing_on_an_episode_not_on_its_own_trunk() {
        let d = repo(); let root = d.to_string_lossy().to_string();
        let b = ensure_box_for(&root, "dani", 1).unwrap();
        let on = git(&["rev-parse", "--abbrev-ref", "HEAD"], &b.to_string_lossy()).unwrap();
        assert_eq!(on.trim(), "antiloki/dani/ep-1");

        std::fs::write(b.join("src/a.ts"), "export const a = 9;\n").unwrap();
        seal_episode(&b, "dani", 1, "t").unwrap();
        assert_eq!(merge_episode(&root, &b, "dani", 1, true).unwrap()["ok"], json!(true));

        // and an episode nobody opened says so in words instead of handing back git's error
        let m = merge_episode(&root, &b, "dani", 7, true).unwrap();
        assert_eq!(m["ok"], json!(false));
        assert!(m["note"].as_str().unwrap().contains("never opened as a branch"));
        assert!(m["note"].as_str().unwrap().contains("is not lost"));

        let _ = remove_box(&root, "dani"); let _ = std::fs::remove_dir_all(&d);
    }

    // BOTH FOUND BY THE FIRST REAL RUN (2026-09-24), not by imagination.
    #[test]
    fn an_episode_commits_the_work_and_never_antilokis_own_files() {
        let d = repo(); let root = d.to_string_lossy().to_string();
        let b = ensure_box_for(&root, "dani", 1).unwrap();
        std::fs::write(b.join("src/a.ts"), "export const a = 2;\n").unwrap();     // theirs
        std::fs::write(b.join("AGENTS.md"), "# ours\n").unwrap();                 // ours
        std::fs::write(b.join("CLAUDE.md"), "# ours\n").unwrap();                 // ours
        let sealed = seal_episode(&b, "dani", 1, "t").unwrap();
        assert_eq!(sealed["files"], json!(["src/a.ts"]), "only the operator's code is the episode");
        // …and the box was taught to ignore them, so git never even offers them — which is what stops `git add`
        // refusing the whole staging with "the following paths are ignored", the way the first real close did
        std::fs::write(b.join("AGENTS.md"), "# ours again\n").unwrap();
        let st = git(&["status", "--porcelain", "--untracked-files=all"], &b.to_string_lossy()).unwrap();
        assert!(!st.contains("AGENTS.md"), "the box does not even see it as a change: {st:?}");
        merge_episode(&root, &b, "dani", 1, true).unwrap();
        assert!(!d.join("AGENTS.md").exists(), "antiloki's own file never reaches the operator's history");
        assert!(d.join("src/a.ts").is_file());
        let _ = remove_box(&root, "dani"); let _ = std::fs::remove_dir_all(&d);
    }

    // A BOX ON THE WRONG BRANCH HEALS ITSELF (2026-09-24). The real loop hit this: a close that failed left the
    // box on the agent's trunk, and the next seal committed there — safe, but with nothing left to merge.
    #[test]
    fn sealing_puts_the_work_on_the_episode_even_if_the_box_drifted() {
        let d = repo(); let root = d.to_string_lossy().to_string();
        let b = ensure_box_for(&root, "dani", 1).unwrap();
        let p = b.to_string_lossy().to_string();
        git(&["checkout", &agent_branch("dani")], &p).unwrap();          // drifted onto the trunk
        std::fs::write(b.join("src/a.ts"), "export const a = 5;\n").unwrap();

        let sealed = seal_episode(&b, "dani", 2, "drifted").unwrap();
        assert_eq!(sealed["branch"], json!("antiloki/dani/ep-2"));
        assert_eq!(git(&["rev-parse", "--abbrev-ref", "HEAD"], &p).unwrap().trim(), "antiloki/dani/ep-2");
        assert_eq!(sealed["files"], json!(["src/a.ts"]), "and the work came with it");
        assert_eq!(merge_episode(&root, &b, "dani", 2, true).unwrap()["ok"], json!(true));

        let _ = remove_box(&root, "dani"); let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn uncommitted_work_in_the_workspace_is_reported_as_itself_not_as_a_conflict() {
        let d = repo(); let root = d.to_string_lossy().to_string();
        let b = ensure_box_for(&root, "dani", 1).unwrap();
        std::fs::write(b.join("src/agentfile.ts"), "export const x = 1;\n").unwrap();
        seal_episode(&b, "dani", 1, "t").unwrap();
        // the operator has unsaved work — on a file the episode never touched, so there is no collision at all
        std::fs::write(d.join("src/a.ts"), "export const a = 99; // mine, unsaved\n").unwrap();

        let m = merge_episode(&root, &b, "dani", 1, true).unwrap();
        assert_eq!(m["ok"], json!(false));
        assert_eq!(m["dirty"], json!(true));
        assert!(m["conflict"].is_null(), "it is not a conflict and must not be called one");
        assert!(m["note"].as_str().unwrap().contains("uncommitted changes"));
        assert!(m["note"].as_str().unwrap().contains("commit or stash"));
        assert_eq!(std::fs::read_to_string(d.join("src/a.ts")).unwrap(), "export const a = 99; // mine, unsaved\n");

        // commit it, and the same merge goes through
        for a in [vec!["add", "-A"], vec!["commit", "-qm", "mine"]] { let _ = crate::proc::cmd("git").args(&a).current_dir(&d).output(); }
        assert_eq!(merge_episode(&root, &b, "dani", 1, true).unwrap()["ok"], json!(true));
        let _ = remove_box(&root, "dani"); let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn two_agents_work_at_the_same_time_without_seeing_each_other() {
        let d = repo(); let root = d.to_string_lossy().to_string();
        let a = ensure_box(&root, "alice").unwrap();
        let c = ensure_box(&root, "bob").unwrap();
        assert_ne!(a, c);
        start_episode(&a, "alice", 1).unwrap();
        start_episode(&c, "bob", 1).unwrap();

        // each writes the SAME path — the exact case that silently overwrote before
        std::fs::write(a.join("src/shared.ts"), "export const who = \"alice\";\n").unwrap();
        std::fs::write(c.join("src/shared.ts"), "export const who = \"bob\";\n").unwrap();
        assert_eq!(std::fs::read_to_string(a.join("src/shared.ts")).unwrap(), "export const who = \"alice\";\n",
                   "bob's write did not reach alice's box");

        seal_episode(&a, "alice", 1, "alice").unwrap();
        let m1 = merge_episode(&root, &a, "alice", 1, true).unwrap();
        assert_eq!(m1["ok"], json!(true), "the first one in merges cleanly");
        seal_episode(&c, "bob", 1, "bob").unwrap();
        let m2 = merge_episode(&root, &c, "bob", 1, true).unwrap();
        // the second collides, is refused, and says where its work is — nothing is lost and nothing is forced
        assert_eq!(m2["ok"], json!(false), "the second one's collision is surfaced, not silently resolved");
        assert_eq!(std::fs::read_to_string(d.join("src/shared.ts")).unwrap(), "export const who = \"alice\";\n");

        let _ = remove_box(&root, "alice"); let _ = remove_box(&root, "bob");
        let _ = std::fs::remove_dir_all(&d);
    }
}

#[cfg(test)]
mod ours_tests {
    use super::is_ours;
    #[test]
    fn antilokis_own_files_are_recognised_and_a_projects_files_are_not() {
        for ours in ["AGENTS.md", "./CLAUDE.md", "opencode.json", ".cursor/mcp.json", ".cursor/rules/antiloki.mdc", ".agents/skills/antiloki-context/SKILL.md", ".cursor/skills/antiloki-context/.antiloki-materialized",
                     ".antiloki-nm-host-aside/x", ".antiloki/db/state.sqlite", ".opencode/plugins/antiloki-context.js", ".grok/rules/antiloki.md", ".grok/config.toml"] { assert!(is_ours(ours), "{ours}"); }
        for theirs in ["src/App.tsx", "src/App.css", "index.html", "README.md", ".cursor/rules/my-style.mdc", ".agents/notes.md", "docs/AGENTS-guide.md", ".github/workflows/ci.yml", "package.json"] { assert!(!is_ours(theirs), "{theirs}"); }
    }
}
