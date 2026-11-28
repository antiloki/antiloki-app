//! CHECKPOINTS - the box's history of what is DONE, DOING and QUEUED, kept by code, not by any model
//! (operator 2026-09-29: "make impossible the models ignore the checkpoints, each checkpoint should be a snapshot in history of what is done, doing and queue").
//!
//! WHY (run #259, "remove the search"): the chat had written a search, tag chips and a theme toggle into a worktree, all uncommitted. A worker removed the search - correctly.
//! The verifier then judged a diff against `HEAD` (where none of it ever was), rejected, and the retry reverted everything - earlier runs' work included - because nobody had
//! said, in a form no model can argue with, what the box looked like before. A checkpoint says it.
//!
//! WHAT ONE IS. A git commit object that holds the WHOLE working tree at that moment - tracked, modified, deleted and untracked files, `.gitignore` respected - written with a
//! temporary index and `git commit-tree`, so the operator's index, branch and files are never touched and no hook runs. It is kept alive by a ref
//! (`refs/antiloki/cp/<scope>/<n>`) and CHAINED: each checkpoint's parent is the scope's previous one, so `git log refs/antiloki/cp/<scope>/head` IS the history. The commit message
//! carries the state of the queue at that instant (done / doing / queued) as JSON, so the history says what was being done, not only what changed.
//!
//! WHY A MODEL CANNOT IGNORE IT. Nothing here asks a model to do anything: code takes the checkpoint, code measures every run and every attempt against it (`Goals::diff_of`,
//! `changed_since`), and code rejects a result that the checkpoint proves is wrong (`gate` below: nothing changed, the box was wiped back to an older state, lines were only deleted
//! for a task that never asked to remove) BEFORE any model judges it, and puts the box back where the checkpoint says it was. The models are shown the state (`state_block`)
//! so they can plan around it - but the gate does not depend on their reading it.
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// what `take` returns and the run keeps
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Checkpoint { pub rev: String, pub tree: String, pub n: u32, pub label: String, pub t: i64 }

/// paths that are never part of a snapshot, whatever `.gitignore` says: the agents' own logs and build output would otherwise show up as "changes"
const EXCLUDE: &[&str] = &[".antiloki", ".git", "node_modules", "target", "dist", ".next", ".turbo", ".venv", "__pycache__"];

fn git(cwd: &str, index: Option<&std::path::Path>, args: &[&str]) -> Option<String> {
    let mut c = crate::proc::cmd("git");
    c.args(args).current_dir(cwd)
        .env("GIT_AUTHOR_NAME", "antiloki").env("GIT_AUTHOR_EMAIL", "antiloki@localhost").env("GIT_COMMITTER_NAME", "antiloki").env("GIT_COMMITTER_EMAIL", "antiloki@localhost");
    if let Some(i) = index { c.env("GIT_INDEX_FILE", i); }
    c.output().ok().filter(|o| o.status.success()).map(|o| String::from_utf8_lossy(&o.stdout).trim_end().to_string())
}
fn tmp_index(cwd: &str) -> std::path::PathBuf {
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let name = format!("antiloki-index-{}-{}", std::process::id(), N.fetch_add(1, std::sync::atomic::Ordering::Relaxed));
    // 2026-10-13: the process temp dir can be a quota-capped scratch (a desktop serve started inside an app scope). The snapshot then fails and the door says "not a git repository" for a repo that lists and rewinds fine. The git dir is the repo's own writable place.
    let dir = git(cwd, None, &["rev-parse", "--absolute-git-dir"]).map(std::path::PathBuf::from).filter(|p| p.is_absolute()).unwrap_or_else(std::env::temp_dir);
    dir.join(name)
}

/// the tree of the working directory RIGHT NOW (every file the snapshot rules keep), without touching the real index. `None` outside a git repository.
pub fn current_tree(cwd: &str) -> Option<String> {
    git(cwd, None, &["rev-parse", "--git-dir"])?;
    let idx = tmp_index(cwd);
    // start from HEAD's tree so unchanged files cost nothing, then add everything that differs (new, modified, deleted)
    if git(cwd, None, &["rev-parse", "--verify", "-q", "HEAD"]).is_some() { git(cwd, Some(&idx), &["read-tree", "HEAD"])?; }
    let mut add: Vec<String> = vec!["add".into(), "-A".into(), "--".into(), ".".into()];
    add.extend(EXCLUDE.iter().flat_map(|e| [format!(":(exclude,glob)**/{e}/**"), format!(":(exclude,glob)**/{e}")]));
    let a: Vec<&str> = add.iter().map(String::as_str).collect();
    let tree = git(cwd, Some(&idx), &a).and_then(|_| git(cwd, Some(&idx), &["write-tree"]));
    let _ = std::fs::remove_file(&idx);
    tree
}

/// the tree a commit holds
pub fn tree_of(cwd: &str, rev: &str) -> Option<String> { git(cwd, None, &["rev-parse", "--verify", "-q", &format!("{rev}^{{tree}}")]) }
/// the tree of `HEAD` (the last real commit), if there is one
pub fn head_tree(cwd: &str) -> Option<String> { git(cwd, None, &["rev-parse", "--verify", "-q", "HEAD^{tree}"]) }

/// Take a checkpoint of `cwd` for `scope` (a pane, usually: `pane-57`). `state` is the queue's done/doing/queued at this instant. Returns `None` outside a git repository.
pub fn take(cwd: &str, scope: &str, label: &str, state: &Value, now: i64) -> Option<Checkpoint> {
    let tree = current_tree(cwd)?;
    let head_ref = format!("refs/antiloki/cp/{scope}/head");
    let prev = git(cwd, None, &["rev-parse", "--verify", "-q", &head_ref]);
    let parent = prev.clone().or_else(|| git(cwd, None, &["rev-parse", "--verify", "-q", "HEAD"]));
    // n continues the scope's own numbering (the chain also runs through the repo's real commits, so it cannot be counted)
    let n = prev.as_ref().and_then(|p| git(cwd, None, &["log", "-1", "--format=%s", p])).and_then(|s| s.strip_prefix("checkpoint ").and_then(|r| r.split(' ').next().and_then(|n| n.parse::<u32>().ok()))).unwrap_or(0) + 1;
    let msg = format!("checkpoint {n} · {label}\n\n{}", serde_json::to_string(state).unwrap_or_default());
    let mut args: Vec<&str> = vec!["commit-tree", &tree, "-m", &msg];
    if let Some(p) = parent.as_deref() { args.extend(["-p", p]); }
    let rev = git(cwd, None, &args)?;
    git(cwd, None, &["update-ref", &format!("refs/antiloki/cp/{scope}/{n}"), &rev])?;
    git(cwd, None, &["update-ref", &head_ref, &rev])?;
    Some(Checkpoint { rev, tree, n, label: label.into(), t: now })
}

/// A checkpoint's name, the same everywhere it is quoted: `cp:pane-1/15`
pub fn name(scope: &str, n: u32) -> String { format!("cp:{scope}/{n}") }
/// The scope's newest checkpoint, read back from its ref (None before the first).
pub fn head(cwd: &str, scope: &str) -> Option<Checkpoint> {
    let head_ref = format!("refs/antiloki/cp/{scope}/head");
    let rev = git(cwd, None, &["rev-parse", "--verify", "-q", &head_ref])?;
    let subject = git(cwd, None, &["log", "-1", "--format=%s", &rev])?;
    let n = subject.strip_prefix("checkpoint ")?.split(' ').next()?.parse().ok()?;
    Some(Checkpoint { tree: tree_of(cwd, &rev)?, rev, n, label: subject, t: 0 })
}
/// THE BOX RIGHT NOW as an in-memory snapshot: the tree, no commit, no ref. What a run and each of its attempts are measured against - the history holds ONE commit per
/// response (`take`), not one per step. `n` is the newest checkpoint's number, for display.
pub fn snapshot_tree(cwd: &str, scope: &str, now: i64) -> Option<Checkpoint> {
    let tree = current_tree(cwd)?;
    Some(Checkpoint { rev: String::new(), tree, n: head(cwd, scope).map(|h| h.n).unwrap_or(0), label: "in memory".into(), t: now })
}
/// A checkpoint only if the box CHANGED since the scope's last one (someone edited outside the queue: the chat, the operator); otherwise the last one already says it.
pub fn take_if_changed(cwd: &str, scope: &str, label: &str, state: &Value, now: i64) -> Option<Checkpoint> {
    let tree = current_tree(cwd)?;
    match head(cwd, scope) { Some(h) if h.tree == tree => Some(h), _ => take(cwd, scope, label, state, now) }
}

/// the queue state a checkpoint carried, read back from the history
pub fn state_of(cwd: &str, rev: &str) -> Option<Value> {
    let body = git(cwd, None, &["log", "-1", "--format=%B", rev])?;
    serde_json::from_str(body.split_once("\n\n")?.1.trim()).ok()
}
/// the scope's checkpoints, newest first: (n, rev, subject)
pub fn history(cwd: &str, scope: &str, limit: usize) -> Vec<(u32, String, String)> {
    let head_ref = format!("refs/antiloki/cp/{scope}/head");
    git(cwd, None, &["log", &format!("-{}", limit + 50), "--format=%H\t%s", &head_ref]).unwrap_or_default().lines().filter_map(|l| {
        let (rev, subject) = l.split_once('\t')?;
        let n = subject.strip_prefix("checkpoint ")?.split(' ').next()?.parse().ok()?;
        Some((n, rev.to_string(), subject.to_string()))
    }).take(limit).collect()
}

/// What changed between two trees: (status, path) - `A`dded, `M`odified, `D`eleted - and the new blob id, which is a cheap content fingerprint.
pub fn changes(cwd: &str, from_tree: &str, to_tree: &str) -> Vec<(char, String, String)> {
    git(cwd, None, &["diff-tree", "-r", "--no-renames", "--raw", "-z", from_tree, to_tree]).map(|raw| {
        // -z: ":100644 100644 <old> <new> M\0path\0"
        let mut out = vec![]; let mut it = raw.split('\0').filter(|s| !s.is_empty());
        while let Some(meta) = it.next() {
            let Some(path) = it.next() else { break };
            let f: Vec<&str> = meta.trim_start_matches(':').split(' ').collect();
            if f.len() >= 5 { out.push((f[4].chars().next().unwrap_or('M'), path.to_string(), f[3].to_string())); }
        }
        out
    }).unwrap_or_default()
}
/// the unified diff of one file between two trees
pub fn file_diff(cwd: &str, from_tree: &str, to_tree: &str, path: &str) -> String { git(cwd, None, &["diff", "--no-color", from_tree, to_tree, "--", path]).unwrap_or_default() }

/// Line counts of a change, from `git diff --numstat`: (added, removed). Binary files count 0.
pub fn numstat(cwd: &str, from_tree: &str, to_tree: &str) -> (usize, usize) {
    git(cwd, None, &["diff", "--numstat", from_tree, to_tree]).unwrap_or_default().lines().fold((0, 0), |(a, r), l| {
        let mut p = l.split('\t'); (a + p.next().and_then(|x| x.parse::<usize>().ok()).unwrap_or(0), r + p.next().and_then(|x| x.parse::<usize>().ok()).unwrap_or(0))
    })
}

/// PUT THE BOX BACK to what `to_tree` says, file by file: rewrite the ones that differ, delete the ones the checkpoint never had. Only paths in the diff are touched (an ignored
/// or excluded file is never in a tree, so it is never deleted). Returns the number of files it changed.
pub fn restore(cwd: &str, to_tree: &str) -> usize {
    restore_paths(cwd, to_tree, &[])
}

/// Put back only `paths` (worktree-relative) to what `to_tree` says; other files stay as they are. An empty `paths` list restores every differing path (same as `restore`). 2026-10-13: scope fence puts back out-of-scope edits only.
pub fn restore_paths(cwd: &str, to_tree: &str, paths: &[String]) -> usize {
    let Some(cur) = current_tree(cwd) else { return 0 };
    let only: Option<std::collections::HashSet<&str>> = if paths.is_empty() { None } else { Some(paths.iter().map(String::as_str).collect()) };
    let mut n = 0;
    for (st, path, _) in changes(cwd, &cur, to_tree) {
        if only.as_ref().is_some_and(|s| !s.contains(path.as_str())) { continue; }
        // `changes(cur -> to)`: A = in the checkpoint, not in the box (write it back); D = in the box, not in the checkpoint (delete it); M = differs (rewrite)
        let full = std::path::Path::new(cwd).join(&path);
        if st == 'D' { if std::fs::remove_file(&full).is_ok() { n += 1; } continue }
        let Some(blob) = crate::proc::cmd("git").args(["show", &format!("{to_tree}:{path}")]).current_dir(cwd).output().ok().filter(|o| o.status.success()).map(|o| o.stdout) else { continue };
        if let Some(d) = full.parent() { let _ = std::fs::create_dir_all(d); }
        if std::fs::write(&full, blob).is_ok() { n += 1; }
    }
    n
}

/// Short rev for display (7 chars).
pub fn short_rev(rev: &str) -> String { rev.chars().take(7).collect() }
/// Committer time of a checkpoint rev (`%cI`), empty when git cannot say. 2026-10-13.
pub fn committed_at(cwd: &str, rev: &str) -> String { git(cwd, None, &["log", "-1", "--format=%cI", rev]).unwrap_or_default() }

static REDO_TREE: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<String, String>>> = std::sync::OnceLock::new();
fn redo_store() -> &'static std::sync::Mutex<std::collections::HashMap<String, String>> {
    REDO_TREE.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}
/// Remember a tree for `scope` so `redo` can put the box back (in-memory; cleared on redo).
pub fn remember_redo(scope: &str, tree: &str) {
    if tree.is_empty() { return; }
    redo_store().lock().unwrap_or_else(|p| p.into_inner()).insert(scope.to_string(), tree.to_string());
}
pub fn take_redo(scope: &str) -> Option<String> { redo_store().lock().unwrap_or_else(|p| p.into_inner()).remove(scope) }
pub fn peek_redo(scope: &str) -> Option<String> { redo_store().lock().unwrap_or_else(|p| p.into_inner()).get(scope).cloned() }

/// THE VERDICT OF THE GATE on one attempt: `base` is the tree before the attempt, `now` the tree after.
#[derive(Clone, Debug, PartialEq)]
pub enum Gate { Pass, Reject { rule: &'static str, why: String, restore: bool } }
/// The gate. `ask` is what the task was asked; `removes` / `reverts` say whether the ask itself is a removal or an undo (language packs: `lexicon`), `known` are the trees of
/// earlier checkpoints of this scope plus `HEAD`'s. Pure over its inputs - tested. Rules, in order:
///   1. NOTHING CHANGED: a write task that left the tree exactly as it found it has done nothing, whatever its report says.
///   2. WIPED: the tree is now identical to `HEAD`'s or to an earlier checkpoint's, and is not what it was: the attempt threw work away. Unless the ask is itself an undo, that is a revert nobody asked for - rejected, and the box is put back.
///   3. ONLY DELETIONS: lines were removed and none added, for an ask that does not remove anything.
pub fn gate(base: &str, now: &str, added: usize, removed: usize, ask_removes: bool, ask_reverts: bool, known: &[String]) -> Gate {
    if base == now { return Gate::Reject { rule: "nothing-changed", why: "the box is exactly as this attempt found it - no file was changed, whatever the report says".into(), restore: false } }
    if !ask_reverts && known.iter().any(|k| k == now) {
        return Gate::Reject { rule: "wiped", why: "the box was put back to an older state (the last real commit or an earlier checkpoint): work that was there when this attempt began was thrown away, and the ask never said to undo anything".into(), restore: true }
    }
    if removed > 0 && added == 0 && !ask_removes && !ask_reverts {
        return Gate::Reject { rule: "only-deletions", why: format!("{removed} line(s) were deleted and none written, for an ask that does not remove anything"), restore: true }
    }
    Gate::Pass
}

#[cfg(test)]
mod tests {
    use super::*;
    fn repo() -> (std::path::PathBuf, String) {
        static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let d = std::env::temp_dir().join(format!("antiloki-cp-{}-{}", std::process::id(), N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)));
        let _ = std::fs::remove_dir_all(&d); std::fs::create_dir_all(d.join("src")).unwrap();
        let cwd = d.to_string_lossy().to_string();
        for a in [&["init", "-q"][..], &["config", "user.email", "t@t"], &["config", "user.name", "t"]] { git(&cwd, None, a).unwrap(); }
        std::fs::write(d.join("src/App.tsx"), "base\n").unwrap();
        git(&cwd, None, &["add", "-A"]).unwrap(); git(&cwd, None, &["commit", "-qm", "base"]).unwrap();
        (d, cwd)
    }
    #[test]
    fn a_checkpoint_holds_the_whole_box_and_touches_nothing_of_the_operators() {
        let (d, cwd) = repo();
        std::fs::write(d.join("src/App.tsx"), "base\ntheme\nsearch\n").unwrap();          // uncommitted work
        std::fs::write(d.join("src/new.ts"), "untracked\n").unwrap();                     // an untracked file
        std::fs::create_dir_all(d.join(".antiloki")).unwrap(); std::fs::write(d.join(".antiloki/log.jsonl"), "noise").unwrap();
        let status_before = git(&cwd, None, &["status", "--porcelain"]).unwrap();
        let branch_before = git(&cwd, None, &["rev-parse", "HEAD"]).unwrap();
        let state = serde_json::json!({ "done": [], "doing": [{ "id": 1, "goal": "add filters" }], "queued": [{ "id": 2, "goal": "remove the search" }] });
        let c1 = take(&cwd, "pane-1", "before #1", &state, 1).expect("a checkpoint");
        assert_eq!(c1.n, 1);
        // nothing of the operator's moved: same status, same HEAD, no stash, no new branch
        assert_eq!(git(&cwd, None, &["status", "--porcelain"]).unwrap(), status_before);
        assert_eq!(git(&cwd, None, &["rev-parse", "HEAD"]).unwrap(), branch_before);
        assert!(git(&cwd, None, &["branch", "--list"]).unwrap().lines().count() == 1);
        // it holds the uncommitted and the untracked files, and not the agents' logs
        let ls = git(&cwd, None, &["ls-tree", "-r", "--name-only", &c1.tree]).unwrap();
        assert!(ls.contains("src/new.ts") && ls.contains("src/App.tsx") && !ls.contains(".antiloki"), "{ls}");
        assert_eq!(git(&cwd, None, &["show", &format!("{}:src/App.tsx", c1.tree)]).unwrap(), "base\ntheme\nsearch");
        // the history carries the queue as it was
        assert_eq!(state_of(&cwd, &c1.rev).unwrap()["queued"][0]["goal"], "remove the search");
        // a second checkpoint chains onto the first
        std::fs::write(d.join("src/App.tsx"), "base\ntheme\n").unwrap();
        let c2 = take(&cwd, "pane-1", "after #1", &serde_json::json!({}), 2).unwrap();
        assert_eq!(c2.n, 2);
        assert_eq!(git(&cwd, None, &["rev-parse", &format!("{}^", c2.rev)]).unwrap(), c1.rev, "the chain IS the history");
        assert_eq!(history(&cwd, "pane-1", 10).iter().map(|h| h.0).collect::<Vec<_>>(), vec![2, 1]);
        // what changed between them: the search line, nothing else
        let ch = changes(&cwd, &c1.tree, &c2.tree);
        assert_eq!(ch.iter().map(|c| (c.0, c.1.as_str())).collect::<Vec<_>>(), vec![('M', "src/App.tsx")]);
        assert!(file_diff(&cwd, &c1.tree, &c2.tree, "src/App.tsx").contains("-search"));
        assert_eq!(numstat(&cwd, &c1.tree, &c2.tree), (0, 1));
        let _ = std::fs::remove_dir_all(&d);
    }
    /// ONE CHECKPOINT PER RESPONSE (operator 2026-09-29): steps inside a response measure against in-memory snapshots and add nothing to the history; a start that finds the box
    /// unchanged since the last response reuses that response's checkpoint.
    #[test]
    fn one_commit_per_response_and_the_steps_inside_it_leave_no_trace() {
        let (d, cwd) = repo();
        std::fs::write(d.join("src/App.tsx"), "base\nwork\n").unwrap();
        let s = serde_json::json!({});
        let c1 = take(&cwd, "pane-4", "after #1", &s, 1).unwrap();
        // the next item starts on the box exactly as the last response left it: no new commit
        let same = take_if_changed(&cwd, "pane-4", "before #2", &s, 2).unwrap();
        assert_eq!((same.n, same.rev.as_str()), (1, c1.rev.as_str()));
        // in-memory bases add no refs
        let refs_before = git(&cwd, None, &["for-each-ref", "refs/antiloki"]).unwrap();
        let base = snapshot_tree(&cwd, "pane-4", 3).unwrap();
        assert!(base.rev.is_empty() && base.tree == c1.tree && base.n == 1);
        std::fs::write(d.join("src/App.tsx"), "base\nwork\nmore\n").unwrap();
        let attempt = snapshot_tree(&cwd, "pane-4", 4).unwrap(); assert_ne!(attempt.tree, base.tree);
        assert_eq!(git(&cwd, None, &["for-each-ref", "refs/antiloki"]).unwrap(), refs_before, "steps leave no refs");
        // an edit made outside the queue (the chat, the operator) IS a new checkpoint at the next start
        let outside = take_if_changed(&cwd, "pane-4", "before #2 - edits outside the queue", &s, 5).unwrap();
        assert_eq!(outside.n, 2);
        assert_eq!(history(&cwd, "pane-4", 10).len(), 2);
        assert_eq!(name("pane-4", outside.n), "cp:pane-4/2");
        let _ = std::fs::remove_dir_all(&d);
    }
    #[test]
    fn restore_puts_the_box_back_including_untracked_files_and_deletes_what_the_checkpoint_never_had() {
        let (d, cwd) = repo();
        std::fs::write(d.join("src/App.tsx"), "base\ntheme\nsearch\n").unwrap(); std::fs::write(d.join("src/new.ts"), "mine\n").unwrap();
        let cp = take(&cwd, "pane-2", "before", &serde_json::json!({}), 1).unwrap();
        // an attempt wipes everything and leaves a stray file
        git(&cwd, None, &["checkout", "--", "."]).unwrap(); std::fs::remove_file(d.join("src/new.ts")).unwrap(); std::fs::write(d.join("src/stray.ts"), "x\n").unwrap();
        assert_ne!(current_tree(&cwd).unwrap(), cp.tree);
        let n = restore(&cwd, &cp.tree);
        assert!(n >= 3, "{n}");
        assert_eq!(std::fs::read_to_string(d.join("src/App.tsx")).unwrap(), "base\ntheme\nsearch\n");
        assert_eq!(std::fs::read_to_string(d.join("src/new.ts")).unwrap(), "mine\n");
        assert!(!d.join("src/stray.ts").exists());
        assert_eq!(current_tree(&cwd).unwrap(), cp.tree, "the box is byte-for-byte the checkpoint");
        let _ = std::fs::remove_dir_all(&d);
    }
    /// THE #259 SCENARIO, decided by code alone: the checkpoint has the theme, the chips and the search; the first attempt removes the search (passes); the second, told a
    /// false complaint, resets everything to HEAD (rejected, and the box is restored).
    #[test]
    fn the_gate_passes_a_removal_and_rejects_a_wipe_before_any_model_is_asked() {
        let (d, cwd) = repo();
        std::fs::write(d.join("src/App.tsx"), "base\ntheme\nchips\nsearch\n").unwrap();
        let cp = take(&cwd, "pane-3", "before #259", &serde_json::json!({}), 1).unwrap();
        let head = head_tree(&cwd).unwrap();
        let known = vec![head.clone(), cp.tree.clone()];
        // attempt 1: the search goes
        std::fs::write(d.join("src/App.tsx"), "base\ntheme\nchips\n").unwrap();
        let t1 = current_tree(&cwd).unwrap(); let (a, r) = numstat(&cwd, &cp.tree, &t1);
        assert_eq!((a, r), (0, 1));
        assert_eq!(gate(&cp.tree, &t1, a, r, true, false, &known), Gate::Pass, "the ask says remove");
        // the same edit for an ask that does NOT remove anything is refused
        assert!(matches!(gate(&cp.tree, &t1, a, r, false, false, &known), Gate::Reject { rule: "only-deletions", restore: true, .. }));
        // attempt 2 starts from attempt 1's checkpoint and wipes the box back to HEAD
        let cp2 = take(&cwd, "pane-3", "before retry", &serde_json::json!({}), 2).unwrap();
        git(&cwd, None, &["checkout", "--", "."]).unwrap();
        let t2 = current_tree(&cwd).unwrap(); assert_eq!(t2, head);
        let (a2, r2) = numstat(&cwd, &cp2.tree, &t2);
        match gate(&cp2.tree, &t2, a2, r2, true, false, &known) { Gate::Reject { rule: "wiped", restore: true, .. } => {}, other => panic!("{other:?}") }
        // …unless the operator asked for an undo
        assert_eq!(gate(&cp2.tree, &t2, a2, r2, false, true, &known), Gate::Pass);
        // and the box is put back
        restore(&cwd, &cp2.tree);
        assert_eq!(std::fs::read_to_string(d.join("src/App.tsx")).unwrap(), "base\ntheme\nchips\n");
        // an attempt that changes nothing does not pass on its report
        assert!(matches!(gate(&cp2.tree, &cp2.tree, 0, 0, false, false, &known), Gate::Reject { rule: "nothing-changed", restore: false, .. }));
        let _ = std::fs::remove_dir_all(&d);
    }
    /// 2026-10-13: selective restore leaves in-scope edits alone.
    #[test]
    fn restore_paths_puts_back_only_the_paths_asked_for() {
        let (d, cwd) = repo();
        std::fs::write(d.join("src/App.tsx"), "base\ntheme\nsearch\n").unwrap();
        std::fs::write(d.join("src/other.ts"), "keep\n").unwrap();
        let cp = take(&cwd, "pane-2", "before", &serde_json::json!({}), 1).unwrap();
        std::fs::write(d.join("src/App.tsx"), "base\n").unwrap();
        std::fs::write(d.join("src/other.ts"), "changed\n").unwrap();
        let n = restore_paths(&cwd, &cp.tree, &["src/App.tsx".into()]);
        assert_eq!(n, 1);
        assert_eq!(std::fs::read_to_string(d.join("src/App.tsx")).unwrap(), "base\ntheme\nsearch\n");
        assert_eq!(std::fs::read_to_string(d.join("src/other.ts")).unwrap(), "changed\n");
        let _ = std::fs::remove_dir_all(&d);
    }
    /// 2026-10-13: checkpoint door save/rewind/redo never move HEAD.
    #[test]
    fn save_and_rewind_do_not_move_head() {
        let (d, cwd) = repo();
        std::fs::write(d.join("src/App.tsx"), "base\nv1\n").unwrap();
        git(&cwd, None, &["add", "src/App.tsx"]).unwrap();
        git(&cwd, None, &["commit", "-qm", "v1"]).unwrap();
        let head0 = git(&cwd, None, &["rev-parse", "HEAD"]).unwrap();
        std::fs::write(d.join("src/App.tsx"), "base\nv2\n").unwrap();
        let c1 = take(&cwd, "pane-9", "one", &serde_json::json!({}), 1).unwrap();
        assert_eq!(git(&cwd, None, &["rev-parse", "HEAD"]).unwrap(), head0);
        std::fs::write(d.join("src/App.tsx"), "base\nv3\n").unwrap();
        remember_redo("pane-9", &current_tree(&cwd).unwrap());
        restore(&cwd, &c1.tree);
        assert_eq!(std::fs::read_to_string(d.join("src/App.tsx")).unwrap(), "base\nv2\n");
        assert_eq!(git(&cwd, None, &["rev-parse", "HEAD"]).unwrap(), head0);
        if let Some(redo) = take_redo("pane-9") {
            restore(&cwd, &redo);
            assert_eq!(std::fs::read_to_string(d.join("src/App.tsx")).unwrap(), "base\nv3\n");
        }
        assert_eq!(git(&cwd, None, &["rev-parse", "HEAD"]).unwrap(), head0);
        let _ = std::fs::remove_dir_all(&d);
    }
}
