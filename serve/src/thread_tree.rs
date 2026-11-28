//! A THREAD IS A LINE OF WORK WITH ITS OWN BRANCH AND ITS OWN WORKTREE (operator 2026-11-11: "each thread, not each agent, should be a different worktree").
//!
//! A thread other than Main MAY have a WORKTREE: a checkout of the thread's own branch (`thread-<workspace digest>-<thread>`). It is made when the person asks (`thread_worktree.rs`) or when a team run needs one. What happens in it:
//!   * the thread's agents work in it together; each keeps its own records in a folder of its own (`shared_folder.rs`);
//!   * a team run's parallel workers get short-lived copies cut from the thread's branch, which `integrate` merges back;
//!   * `land` commits what is in the worktree and merges the branch into the workspace (only when the workspace has no uncommitted changes), `discard` throws it away;
//!   * a thread with unmerged work cannot be deleted.
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

fn digest(root: &str) -> String { let mut h: u64 = 0xcbf29ce484222325; for b in root.as_bytes() { h ^= *b as u64; h = h.wrapping_mul(0x100000001b3); } format!("{h:x}").chars().take(8).collect() }
/// The branch and the folder name of a thread's tree. Pure.
pub fn slug(root: &str, thread: &str) -> String {
    let old = format!("thread-{}-{}", digest(root), thread);
    let tag = antiloki_rs::agents_git_manager_git_manager_worktree_ops::project_tag(root);
    // a tree made before the project's name was in it keeps its name
    if tag.is_empty() || (!thread.is_empty() && trees_dir().join(old.to_lowercase()).exists()) { old } else { format!("{tag}-{}-{}", digest(root), thread) }
}
#[cfg(test)] static TEST_BASE: std::sync::Mutex<Option<PathBuf>> = std::sync::Mutex::new(None);   // the tests point the trees at a folder of their own without touching the process environment (other tests own ANTILOKI_HOME)
pub fn trees_dir() -> PathBuf {
    #[cfg(test)] { if let Some(b) = TEST_BASE.lock().unwrap_or_else(|p| p.into_inner()).clone() { return b; } }
    antiloki_rs::agents_git_manager_git_manager_worktree_ops::worktreePathFor(&json!("x")).parent().map(Path::to_path_buf).unwrap_or_default()
}
/// Is this folder one of the trees antiloki cut for a thread (a thread's own tree, or a unit inside one)? Then it is a part of the project's codebase being worked on. Pure over the paths.
pub fn is_tree_dir(root: &str) -> bool {
    let p = Path::new(root.trim_end_matches(['/', '\\'])); let base = trees_dir(); if !p.is_dir() { return false; }
    let (a, b) = (std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf()), std::fs::canonicalize(&base).unwrap_or(base)); a != b && a.starts_with(&b) && a.join(".git").exists()
}
/// The trees of a workspace: (thread, folder) for every thread of it that has a tree. Units (`thread--run-unit`) are not listed: they are boxes inside a run. Blocking.
pub fn trees_of(root: &str) -> Vec<(String, PathBuf)> {
    let (prefix, before) = (slug(root, "").to_lowercase(), format!("thread-{}-", digest(root)));
    let mut v: Vec<(String, PathBuf)> = std::fs::read_dir(trees_dir()).into_iter().flatten().flatten().filter_map(|e| { let n = e.file_name().to_string_lossy().to_string(); let rest = n.strip_prefix(&prefix).or_else(|| n.strip_prefix(&before))?; (!rest.is_empty() && !rest.contains("--") && e.path().is_dir()).then(|| (rest.to_string(), e.path())) }).collect(); v.sort(); v
}
pub fn path_of(root: &str, thread: &str) -> PathBuf { trees_dir().join(slug(root, thread).to_lowercase()) }
pub fn git(args: &[&str], cwd: &Path) -> Result<String, String> { crate::boxes::git(args, &cwd.to_string_lossy()) }
pub const IDENT: [&str; 4] = ["-c", "user.name=antiloki", "-c", "user.email=antiloki@localhost"];

/// Make the thread's tree if it is not there; returns (folder, branch). Main has none (its agents are cut from the workspace as always).
/// Is there nothing of the person's in this folder? (only antiloki's own folders and the agents' brief files). Pure over the names.
pub fn only_ours(names: &[String]) -> bool { names.iter().all(|n| crate::team_run::NOT_WORK.contains(&n.as_str()) || matches!(n.as_str(), ".cursor" | ".gemini" | ".git")) }
/// A thread's tree is a git worktree, so the workspace has to be a git repository. A folder that has nothing of the person's in it yet (a new workspace) is made one: `git init`, antiloki's own folders kept out of it,
/// and one empty commit to cut the trees from. A folder with files in it is NOT touched: committing someone's files for them is not ours to do, so the answer says what to run.
pub fn ensure_repo(root: &str) -> Result<(), String> {
    let r = Path::new(root); if git(&["rev-parse", "--is-inside-work-tree"], r).is_ok() { return Ok(()); }
    let names: Vec<String> = std::fs::read_dir(r).map_err(|e| format!("{root}: {e}"))?.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
    if !only_ours(&names) { return Err(format!("{root} is not a git repository and has files in it: antiloki will not commit them for you. Press `Start Git` on the team's bar (it makes the repository and one commit, leaving out packages, builds and .env files), then run the team again")); }
    git(&["init"], r)?; let _ = git(&["symbolic-ref", "HEAD", "refs/heads/main"], r); exclude_ours(r);
    let mut a: Vec<&str> = IDENT.to_vec(); a.extend(["commit", "--allow-empty", "-m", "antiloki: the workspace's first commit (the teams' trees are cut from it)"]); git(&a, r).map(|_| ())
}
/// What the first `.gitignore` leaves out when the project has none: things that are built, downloaded or secret.
const FIRST_IGNORE: &str = "# written by antiloki when you started Git here: change it as you like\nnode_modules/\ntarget/\ndist/\nbuild/\n.next/\n.venv/\nvenv/\n__pycache__/\n.env\n.env.*\n!.env.example\n*.pem\n*.key\n";
/// START GIT IN THE PERSON'S FOLDER, BECAUSE THE PERSON ASKED (2026-11-27: the button beside "this agent works in your project folder"). A repository, a `.gitignore` if there is none, and one commit of the files as they are.
/// Nothing is uploaded. Refused for a home or a drive's root folder. If the commit cannot be made, the repository that was just made is taken away again. Blocking.
pub fn start_git(root: &str) -> Result<(), String> {
    let r = Path::new(root); if git(&["rev-parse", "--is-inside-work-tree"], r).is_ok() { return Ok(()); }
    let real = r.canonicalize().map_err(|e| format!("{root}: {e}"))?;
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).and_then(|h| PathBuf::from(h).canonicalize().ok());
    if real.parent().is_none() || home.as_deref() == Some(real.as_path()) { return Err("this is your whole home or drive folder: open the project's own folder, then start Git there".into()); }
    if r.join(".git").exists() { return Err("there is a .git here that Git cannot read: fix or remove it first".into()); }
    let ignore = r.join(".gitignore"); let wrote_ignore = !ignore.exists();
    let done = (|| {
        git(&["init"], r)?; let _ = git(&["symbolic-ref", "HEAD", "refs/heads/main"], r); exclude_ours(r);
        exclude_line(r, ".cursor/mcp.json");   // where Cursor keeps its tool servers, antiloki's among them, and often keys: not for a commit made on a button
        if wrote_ignore { std::fs::write(&ignore, FIRST_IGNORE).map_err(|e| format!("could not write .gitignore: {e}"))?; }
        git(&["add", "-A"], r)?;
        // the commit is the person's when Git knows who they are; otherwise antiloki signs it
        let known = git(&["config", "user.name"], r).is_ok() && git(&["config", "user.email"], r).is_ok();
        let mut a: Vec<&str> = if known { vec![] } else { IDENT.to_vec() }; a.extend(["commit", "-q", "--allow-empty", "-m", "start"]); git(&a, r).map(|_| ())
    })();
    if done.is_err() { let _ = std::fs::remove_dir_all(r.join(".git")); if wrote_ignore { let _ = std::fs::remove_file(&ignore); } }
    done
}
/// `POST /api/v1/cockpit/cowork/git/start`: the person's button; it works on the open workspace only.
pub async fn git_start(state: Arc<AppState>) -> Response {
    let root = crate::agents_http::active_root(&state).await;
    match tokio::task::spawn_blocking(move || start_git(&root)).await.unwrap_or_else(|e| Err(e.to_string())) { Ok(()) => reply(json!({ "ok": true })), Err(e) => reply(json!({ "ok": false, "error": e })) }
}
pub fn ensure(root: &str, thread: &str) -> Result<(PathBuf, String), String> {
    if thread == crate::cowork_threads::MAIN { return Err("the Main thread works from the workspace itself".into()); }
    ensure_repo(root)?;
    let (slug, p) = (slug(root, thread), path_of(root, thread));
    if p.join(".git").exists() { return Ok((p, slug)); }
    if p.exists() { let _ = std::fs::remove_dir_all(&p); }
    if let Some(d) = p.parent() { std::fs::create_dir_all(d).map_err(|e| e.to_string())?; }
    let r = Path::new(root);
    let branch_there = git(&["rev-parse", "--verify", "--quiet", &format!("refs/heads/{slug}")], r).is_ok();
    if branch_there { git(&["worktree", "add", &p.to_string_lossy(), &slug], r)?; } else { git(&["worktree", "add", "-b", &slug, &p.to_string_lossy(), "HEAD"], r)?; }
    exclude_ours(&p);
    Ok((p, slug))
}

/// A UNIT OF A TEAM'S WORK (RFC 0008 §4.8): one saga of the pack gets a worktree of its own, on a branch cut from the team thread's branch (`<thread branch>--<unit>`), so two sagas in parallel cannot collide;
/// `integrate(root, thread, [(unit, 0, folder)])` brings a passed one into the team's branch. Made if it is not there; returns (folder, branch). Not for Main.
/// ONE AT A TIME: two parallel branches of a team both ask for the saga's tree and cut their own from it; git's worktree and branch creation on one repo is not safe to race (the second "branch already exists").
static TREES: std::sync::Mutex<()> = std::sync::Mutex::new(());
pub fn ensure_unit(root: &str, thread: &str, unit: &str) -> Result<(PathBuf, String), String> { let _g = TREES.lock().unwrap_or_else(|e| e.into_inner()); ensure_unit_locked(root, thread, unit) }
fn ensure_unit_locked(root: &str, thread: &str, unit: &str) -> Result<(PathBuf, String), String> {
    let (tree, tbranch) = ensure(root, thread)?;
    let unit: String = unit.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '-' }).collect::<String>().trim_matches('-').to_string();
    if unit.is_empty() { return Err("a unit needs a name".into()); }
    let branch = format!("{tbranch}--{unit}"); let p = trees_dir().join(branch.to_lowercase());
    if p.join(".git").exists() { return Ok((p, branch)); }
    if p.exists() { let _ = std::fs::remove_dir_all(&p); }
    if let Some(d) = p.parent() { std::fs::create_dir_all(d).map_err(|e| e.to_string())?; }
    let r = Path::new(root);
    let there = git(&["rev-parse", "--verify", "--quiet", &format!("refs/heads/{branch}")], r).is_ok();
    if there { git(&["worktree", "add", &p.to_string_lossy(), &branch], r)?; } else { git(&["worktree", "add", "-b", &branch, &p.to_string_lossy(), &tbranch], &tree)?; }
    exclude_ours(&p);
    Ok((p, branch))
}
/// A PARALLEL BRANCH OF A UNIT (RFC 0008 §4.8): inside one saga, the nodes of one branch of a fork work in a tree of their own, cut from the saga's branch (`<unit branch>--<sub>`), so two side by side cannot overwrite each other;
/// `integrate_tree` brings them into the saga's tree at the join. Made if it is not there; returns (folder, branch).
pub fn ensure_subunit(root: &str, thread: &str, unit: &str, sub: &str) -> Result<(PathBuf, String), String> {
    let _g = TREES.lock().unwrap_or_else(|e| e.into_inner());
    let (utree, ubranch) = ensure_unit_locked(root, thread, unit)?;
    let sub: String = sub.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '-' }).collect::<String>().trim_matches('-').to_string(); if sub.is_empty() { return Err("a branch needs a name".into()); }
    let branch = format!("{ubranch}--{sub}"); let p = trees_dir().join(branch.to_lowercase());
    if p.join(".git").exists() { return Ok((p, branch)); }
    if p.exists() { let _ = std::fs::remove_dir_all(&p); }
    if let Some(d) = p.parent() { std::fs::create_dir_all(d).map_err(|e| e.to_string())?; }
    let r = Path::new(root); let there = git(&["rev-parse", "--verify", "--quiet", &format!("refs/heads/{branch}")], r).is_ok();
    if there { git(&["worktree", "add", &p.to_string_lossy(), &branch], r)?; } else { git(&["worktree", "add", "-b", &branch, &p.to_string_lossy(), &ubranch], &utree)?; }
    exclude_ours(&p); Ok((p, branch))
}
/// Take a unit's worktree and branch away (after it has been integrated, or when the run is discarded).
pub fn discard_unit(root: &str, thread: &str, unit: &str) {
    let r = Path::new(root); let tbranch = slug(root, thread);
    { let p = trees_dir().join(format!("{tbranch}--{}", unit.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '-' }).collect::<String>().trim_matches('-')).to_lowercase()); if p.exists() { crate::team_archive::store_tree(&crate::team_archive::Ctx { root: root.to_string(), run: format!("thread:{thread}"), team: thread.to_string(), node: unit.to_string() }, &p, "a unit's folder was removed"); } }
    let unit: String = unit.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '-' }).collect::<String>().trim_matches('-').to_string();
    let branch = format!("{tbranch}--{unit}"); let p = trees_dir().join(branch.to_lowercase());
    if p.exists() { if git(&["worktree", "remove", "--force", &p.to_string_lossy()], r).is_err() { let _ = std::fs::remove_dir_all(&p); } let _ = git(&["worktree", "prune"], r); }
    let _ = git(&["branch", "-D", &branch], r);
}

/// One more line in the checkout's local exclude.
pub(crate) fn exclude_line(dir: &Path, line: &str) {
    let Ok(g) = git(&["rev-parse", "--git-path", "info/exclude"], dir) else { return }; let ex = if Path::new(g.trim()).is_absolute() { PathBuf::from(g.trim()) } else { dir.join(g.trim()) };
    if let Some(d) = ex.parent() { let _ = std::fs::create_dir_all(d); } let mut t = std::fs::read_to_string(&ex).unwrap_or_default(); if !t.lines().any(|l| l == line) { t.push_str(&format!("\n{line}\n")); let _ = std::fs::write(&ex, t); }
}
/// antiloki's own files are never anyone's work: the checkout's local exclude lists them, so a plain `git add -A` leaves them out.
fn exclude_ours(dir: &Path) {
    let Ok(g) = git(&["rev-parse", "--git-path", "info/exclude"], dir) else { return }; let ex = if Path::new(g.trim()).is_absolute() { PathBuf::from(g.trim()) } else { dir.join(g.trim()) };
    if let Some(d) = ex.parent() { let _ = std::fs::create_dir_all(d); } let mut t = std::fs::read_to_string(&ex).unwrap_or_default(); let mut dirty = false;
    for n in crate::team_run::NOT_WORK { if !t.lines().any(|l| l == n) { t.push_str(&format!("\n{n}")); dirty = true; } } if dirty { let _ = std::fs::write(&ex, t); }
}
pub fn changed(dir: &Path) -> bool { git(&["status", "--porcelain", "--untracked-files=all"], dir).map(|o| o.lines().any(|l| { let f = l.get(3..).unwrap_or(""); !crate::team_run::NOT_WORK.iter().any(|n| f == *n || f.starts_with(&format!("{n}/"))) })).unwrap_or(false) }
/// What makes a folder "changed", file by file (the same rule as `changed`): the status line's path, in the order git gives them. Blocking.
pub fn dirty_files(dir: &Path) -> Vec<String> {
    git(&["status", "--porcelain", "--untracked-files=all"], dir).map(|o| o.lines().filter_map(|l| { let f = l.get(3..).unwrap_or("").trim(); (!f.is_empty() && !crate::team_run::NOT_WORK.iter().any(|n| f == *n || f.starts_with(&format!("{n}/")))).then(|| f.to_string()) }).collect()).unwrap_or_default()
}
/// THE GIT AUTHOR OF AN AGENT (the operator 2026-11-21: "each agent on antiloki should be a different git author"): its own name and a mail of its own under `agents.antiloki.local`, so the git log, the Authors view and a blame say which agent wrote what. Pure.
pub fn agent_author(who: &str) -> (String, String) {
    let name: String = who.chars().filter(|c| !c.is_control() && !matches!(c, '<' | '>')).collect::<String>().trim().chars().take(60).collect();
    let name = if name.is_empty() { "agent".to_string() } else { name };
    let local: String = name.to_lowercase().chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect::<String>().split('-').filter(|x| !x.is_empty()).collect::<Vec<_>>().join("-");
    (name, format!("{}@agents.antiloki.local", if local.is_empty() { "agent" } else { &local }))
}
/// Commit what an agent has done in its box (never antiloki's own files). Returns whether there was anything. The commit is the agent's: its name is the author, antiloki is only the committer.
pub fn commit_box(dir: &Path, who: &str, thread: &str) -> Result<bool, String> {
    if !changed(dir) { return Ok(false); }
    exclude_ours(dir); git(&["add", "-A"], dir)?;
    let (name, mail) = agent_author(who); let msg = format!("{who}: work in the thread {thread}"); let author = format!("{name} <{mail}>");
    let mut a: Vec<&str> = IDENT.to_vec(); a.extend(["commit", "-q", "--author", &author, "-m", &msg]); git(&a, dir)?; Ok(true)
}
pub fn head(dir: &Path) -> Result<String, String> { git(&["rev-parse", "HEAD"], dir).map(|s| s.trim().to_string()) }
fn conflicted_files(dir: &Path) -> Vec<String> { git(&["diff", "--name-only", "--diff-filter=U"], dir).map(|o| o.lines().map(String::from).collect()).unwrap_or_default() }

/// Bring a thread's agents together. `boxes`: (agent name, pane number, box folder). Returns what merged, what conflicted, and what each box took back.
pub fn integrate(root: &str, thread: &str, boxes: &[(String, i64, PathBuf)]) -> Result<Value, String> {
    let (tree, branch) = ensure(root, thread)?; integrate_tree(&tree, &branch, thread, boxes)
}
/// The same, into any tree: the boxes' work is merged into `branch`, checked out in `tree` (`name` words the commits). A saga's parallel branches are brought into the saga's own tree this way, at a join.
pub fn integrate_tree(tree: &Path, branch: &str, thread: &str, boxes: &[(String, i64, PathBuf)]) -> Result<Value, String> {
    let (tree, branch) = (tree.to_path_buf(), branch.to_string()); if changed(&tree) { return Err("the folder has uncommitted changes: commit or discard them first".into()); }
    let (mut merged, mut conflicts, mut taken, mut errors) = (vec![], vec![], vec![], vec![]);
    for (who, pane, dir) in boxes {
        if dir == &tree { continue; }
        let had = match commit_box(dir, who, thread) { Ok(h) => h, Err(e) => { errors.push(json!({ "agent": who, "pane": pane, "error": e })); continue; } };
        let tip = match head(dir) { Ok(t) => t, Err(e) => { errors.push(json!({ "agent": who, "pane": pane, "error": e })); continue; } };
        // already in the thread's branch: nothing to merge
        if git(&["merge-base", "--is-ancestor", &tip, &branch], &tree).is_ok() { continue; }
        let mut a: Vec<&str> = IDENT.to_vec(); let msg = format!("merge {who} into the thread {thread}"); a.extend(["merge", "--no-ff", "--no-edit", "-m", &msg, &tip]);
        match git(&a, &tree) {
            Ok(_) => merged.push(json!({ "agent": who, "pane": pane, "committed": had })),
            Err(_) => { let files = conflicted_files(&tree); let _ = git(&["merge", "--abort"], &tree); conflicts.push(json!({ "agent": who, "pane": pane, "files": files })); }
        }
    }
    // everyone takes the thread's branch back (a box that has uncommitted work of its own, or a conflict, is told and left)
    for (who, pane, dir) in boxes {
        if dir == &tree || conflicts.iter().any(|c| c["pane"] == json!(pane)) { continue; }
        if changed(dir) { errors.push(json!({ "agent": who, "pane": pane, "error": "it has changes of its own that came after: run integrate again" })); continue; }
        let mut a: Vec<&str> = IDENT.to_vec(); a.extend(["merge", "--no-edit", &branch]);
        match git(&a, dir) { Ok(o) => if !o.contains("Already up to date") { taken.push(json!({ "agent": who, "pane": pane })); }, Err(_) => { let files = conflicted_files(dir); let _ = git(&["merge", "--abort"], dir); errors.push(json!({ "agent": who, "pane": pane, "error": format!("could not take the thread's work: {}", if files.is_empty() { "a conflict".to_string() } else { files.join(", ") }) })); } }
    }
    Ok(json!({ "ok": conflicts.is_empty() && errors.is_empty(), "thread": thread, "branch": branch, "merged": merged, "conflicts": conflicts, "took": taken, "errors": errors, "head": head(&tree).unwrap_or_default() }))
}

/// HOW FAR A THREAD HAS DRIFTED FROM THE WORKSPACE (the operator 2026-11-22: "teams should have a warning when developing for too long, a diff too big, or a worktree too old, to merge with main: keep it up to date, never delete it, update it"): how many commits the workspace has that the thread lacks (`behind`), how many days ago the thread last had the workspace's work (`age_days`), and how big its own change is against that point (`files`, `lines`). Blocking.
pub fn freshness(root: &str, slug: &str) -> Value {
    let r = Path::new(root);
    let behind = git(&["rev-list", "--count", &format!("{slug}..HEAD")], r).ok().and_then(|s| s.trim().parse::<i64>().ok()).unwrap_or(0);
    let age_days = git(&["merge-base", "HEAD", slug], r).ok().and_then(|mb| git(&["log", "-1", "--format=%ct", mb.trim()], r).ok()).and_then(|t| t.trim().parse::<i64>().ok()).map(|t| ((crate::cli::now_ms() as i64 / 1000 - t).max(0)) / 86_400).unwrap_or(0);
    let (files, lines) = git(&["diff", "--shortstat", &format!("HEAD...{slug}")], r).map(|o| parse_shortstat(&o)).unwrap_or((0, 0));
    json!({ "behind": behind, "age_days": age_days, "files": files, "lines": lines })
}
/// `3 files changed, 40 insertions(+), 2 deletions(-)` as (files, lines changed). Pure.
pub fn parse_shortstat(o: &str) -> (i64, i64) {
    let num = |word: &str| o.split(',').find(|p| p.contains(word)).and_then(|p| p.split_whitespace().next()).and_then(|n| n.parse::<i64>().ok()).unwrap_or(0);
    (num("file"), num("insertion") + num("deletion"))
}
/// How far a thread's branch is ahead of the workspace's HEAD, and whether its folder has uncommitted work. None when the thread has no tree.
pub fn state(root: &str, thread: &str) -> Option<Value> {
    let p = path_of(root, thread); if !p.join(".git").exists() { return None; } let slug = slug(root, thread);
    let ahead = git(&["rev-list", "--count", &format!("HEAD..{slug}")], Path::new(root)).ok().and_then(|s| s.trim().parse::<i64>().ok()).unwrap_or(0);
    let mut v = json!({ "worktree": p.to_string_lossy(), "branch": slug, "ahead": ahead, "dirty": changed(&p) }); if let (Some(o), Some(f)) = (v.as_object_mut(), freshness(root, &slug).as_object()) { for (k, x) in f { o.insert(k.clone(), x.clone()); } } Some(v)
}

/// What the sidebar shows of a thread's tree, WITHOUT running git on the request: the last answer (kept up to `TREE_FRESH_S` seconds, and served stale after that) with a refresh started in the background when it is old, or `0` and `false` the very first time. The list is asked every few seconds by every open window, and `git status` over a big worktree takes a second: running it per thread per request kept the serve busy and made the whole page slow. Operations that need the exact answer use `state`.
const TREE_FRESH_S: u64 = 10;
type TreeCache = std::sync::Mutex<std::collections::HashMap<String, (std::time::Instant, i64, bool, Value)>>;
fn tree_cache() -> &'static TreeCache { static C: std::sync::OnceLock<TreeCache> = std::sync::OnceLock::new(); C.get_or_init(Default::default) }
fn refreshing() -> &'static std::sync::Mutex<std::collections::HashSet<String>> { static R: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<String>>> = std::sync::OnceLock::new(); R.get_or_init(Default::default) }
pub fn state_cached(root: &str, thread: &str) -> Option<Value> {
    let p = path_of(root, thread); if !p.join(".git").exists() { return None; } let slug = slug(root, thread); let key = p.to_string_lossy().to_string();
    let hit = tree_cache().lock().unwrap_or_else(|e| e.into_inner()).get(&key).cloned();
    let old = hit.as_ref().map_or(true, |(at, _, _, _)| at.elapsed().as_secs() >= TREE_FRESH_S);
    if old && refreshing().lock().unwrap_or_else(|e| e.into_inner()).insert(key.clone()) {
        let (root2, key2, p2, slug2) = (root.to_string(), key.clone(), p.clone(), slug.clone());
        std::thread::spawn(move || {
            let ahead = git(&["rev-list", "--count", &format!("HEAD..{slug2}")], Path::new(&root2)).ok().and_then(|s| s.trim().parse::<i64>().ok()).unwrap_or(0);
            let dirty = changed(&p2);
            // how far the thread has drifted changes slowly and costs four git calls: asked at most every two minutes, the last answer kept in between (the list is asked every few seconds by every open window)
            let prev = tree_cache().lock().unwrap_or_else(|e| e.into_inner()).get(&key2).cloned(); let fresh = match prev { Some((_, _, _, f)) if f["at_ms"].as_i64().map_or(false, |t| (crate::cli::now_ms() as i64) - t < 120_000) => f, _ => { let mut f = freshness(&root2, &slug2); f["at_ms"] = json!(crate::cli::now_ms() as i64); f } };
            tree_cache().lock().unwrap_or_else(|e| e.into_inner()).insert(key2.clone(), (std::time::Instant::now(), ahead, dirty, fresh)); refreshing().lock().unwrap_or_else(|e| e.into_inner()).remove(&key2);
        });
    }
    let (ahead, dirty, fresh) = hit.map_or((0, false, json!({})), |(_, a, d, f)| (a, d, f));
    let mut v = json!({ "worktree": p.to_string_lossy(), "branch": slug, "ahead": ahead, "dirty": dirty }); if let (Some(o), Some(f)) = (v.as_object_mut(), fresh.as_object()) { for (k, x) in f { o.insert(k.clone(), x.clone()); } } Some(v)
}
/// Forget what was kept for a thread's tree (after an operation that changed it), so the next list asks git again.
pub fn forget_tree(root: &str, thread: &str) { tree_cache().lock().unwrap_or_else(|e| e.into_inner()).remove(&path_of(root, thread).to_string_lossy().to_string()); }

/// UPDATE A THREAD FROM THE WORKSPACE: the workspace's current work (main or master, whatever it has checked out) is merged INTO the thread's own tree, so the thread keeps up; nothing is deleted. Only when the thread's folder has nothing uncommitted; a conflict is undone and reported with the files. Blocking.
pub fn update(root: &str, thread: &str) -> Result<Value, String> {
    let r = Path::new(root); let p = path_of(root, thread); if !p.join(".git").exists() { return Err("that thread has no work of its own to update".into()); }
    if changed(&p) { return Err("the thread's folder has uncommitted changes: integrate its agents or commit them first, then update it".into()); }
    let main = head(r)?; let was = head(&p).unwrap_or_default(); if git(&["merge-base", "--is-ancestor", &main, &was], &p).is_ok() { return Ok(json!({ "ok": true, "updated": thread, "already": true, "head": was })); }
    let mut a: Vec<&str> = IDENT.to_vec(); let msg = "update from the workspace".to_string(); a.extend(["merge", "--no-ff", "--no-edit", "-m", &msg, &main]);
    match git(&a, &p) { Ok(_) => Ok(json!({ "ok": true, "updated": thread, "head": head(&p).unwrap_or_default() })), Err(_) => { let files = conflicted_files(&p); let _ = git(&["merge", "--abort"], &p); Ok(json!({ "ok": false, "error": format!("the thread conflicts with the workspace ({}): nothing was changed. An agent in the thread can resolve it, or land it first", if files.is_empty() { "see git".to_string() } else { files.join(", ") }) })) } }
}
/// Merge the thread into the workspace's own branch: only when the workspace has nothing uncommitted; a conflict is undone and reported.
pub fn land(root: &str, thread: &str) -> Result<Value, String> {
    let slug = slug(root, thread); let r = Path::new(root); let p = path_of(root, thread); if !p.join(".git").exists() { return Err("that thread has no work of its own to land".into()); }
    if changed(&p) { commit_box(&p, "antiloki", thread)?; }   // what the thread's agents wrote in the folder they share
    if changed(r) { let files = dirty_files(r); return Err(format!("the project folder has uncommitted changes ({}{}): commit, stash or discard them there, then merge", files.iter().take(5).cloned().collect::<Vec<_>>().join(", "), if files.len() > 5 { format!(" and {} more", files.len() - 5) } else { String::new() })); }
    let mut a: Vec<&str> = IDENT.to_vec(); let msg = format!("land the thread {thread}"); a.extend(["merge", "--no-ff", "--no-edit", "-m", &msg, &slug]);
    match git(&a, r) { Ok(_) => Ok(json!({ "ok": true, "landed": thread, "head": head(r).unwrap_or_default() })), Err(_) => { let files = conflicted_files(r); let _ = git(&["merge", "--abort"], r); Ok(json!({ "ok": false, "error": format!("it conflicts with the workspace ({}): nothing was changed", if files.is_empty() { "see git".to_string() } else { files.join(", ") }) })) } }
}
/// Throw a thread's tree and branch away (its agents' boxes are not touched).
pub fn discard(root: &str, thread: &str) -> Result<(), String> {
    let slug = slug(root, thread); let r = Path::new(root); let p = path_of(root, thread);
    // NOTHING IS DELETED (the operator 2026-11-15): what the thread's folder holds is saved to the archive before the folder goes
    if p.exists() { let ctx = crate::team_archive::Ctx { root: root.to_string(), run: format!("thread:{thread}"), team: thread.to_string(), node: "discard".into() }; crate::team_archive::store_tree(&ctx, &p, "the thread's work was discarded");
        // the commits in the thread's branch go with the branch: they are saved as a patch first
        if let Ok(patch) = git(&["format-patch", "--stdout", &format!("HEAD..{slug}")], r) { if !patch.trim().is_empty() { crate::team_archive::store_bytes(&ctx, "thread-commits.patch", patch.as_bytes(), "the commits of the thread's branch, before it was deleted"); } } }
    if p.exists() { if git(&["worktree", "remove", "--force", &p.to_string_lossy()], r).is_err() { std::fs::remove_dir_all(&p).map_err(|e| e.to_string())?; } let _ = git(&["worktree", "prune"], r); }
    let _ = git(&["branch", "-D", &slug], r); Ok(())
}

// ── the doors ─────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────

use crate::dispatch::{engine_call, AppState};
use axum::{body::Bytes, http::StatusCode, response::{IntoResponse, Response}, Json};
use std::sync::Arc;
fn reply(v: Value) -> Response { (StatusCode::OK, Json(v)).into_response() }

/// The threads with what each tree holds: `view` plus `tree: {worktree, branch, ahead, dirty}` for the ones that have one.
pub fn decorated(root: &str) -> Value {
    let mut v = crate::cowork_threads::view(root);
    let waiting = crate::thread_worktree::suggestions(root);
    v["git"] = json!(Path::new(root).ancestors().any(|d| d.join(".git").exists()));
    if let Some(ts) = v["threads"].as_array_mut() { for t in ts.iter_mut() { let id = t["id"].as_str().unwrap_or("").to_string(); t["tree"] = state_cached(root, &id).unwrap_or(Value::Null); t["suggest"] = waiting.get(&id).cloned().unwrap_or(Value::Null); } }
    v
}
/// Which of a thread's agents work in a copy of their own (a team member, or an agent opened before copies stopped being made): the bar must not say they are in the thread's folder. Pure.
pub fn mark_copies(view: &mut Value, boxed: &std::collections::HashSet<i64>) {
    for t in view["threads"].as_array_mut().into_iter().flatten() {
        let own: Vec<Value> = t["panes"].as_array().into_iter().flatten().filter(|p| p["paneId"].as_i64().is_some_and(|i| boxed.contains(&i))).map(|p| p["title"].clone()).collect();
        t["copies"] = Value::Array(own);
    }
}
/// The boxes of the agents standing in a thread: (agent name, pane number, folder).
fn boxes_of(state: &Arc<AppState>, root: &str, thread: &str) -> Vec<(String, i64, PathBuf)> {
    let ids: std::collections::HashSet<i64> = crate::cowork_threads::view(root)["threads"].as_array().into_iter().flatten().filter(|t| t["id"] == json!(thread)).flat_map(|t| t["panes"].as_array().cloned().unwrap_or_default()).filter_map(|p| p["paneId"].as_i64()).collect();
    crate::work::boxes_of_project(state).into_iter().filter(|b| ids.contains(&b.pane_id)).map(|b| (b.agent.clone(), b.pane_id, b.dir.clone())).collect()
}
/// What to do with a thread's tree: `integrate`, `land` or `discard`. The answer as JSON (`ok`, and the reason when not).
pub async fn tree_run(state: &Arc<AppState>, op: &str, thread: &str) -> Value {
    let (op, thread) = (op.to_string(), thread.to_string());
    let root = crate::agents_http::active_root(state).await; if root.is_empty() { return json!({ "ok": false, "error": "no workspace is open" }); }
    if !crate::cowork_threads::exists(&root, &thread) { return json!({ "ok": false, "error": "no such thread" }); }
    // its agents leave the folder before it goes
    let mut restart = vec![];
    if op == "discard" { match crate::thread_worktree::bring_back(state, &root, &thread).await { Ok(m) => restart = m, Err(e) => return json!({ "ok": false, "error": e }) } }
    let boxes = boxes_of(state, &root, &thread); let (root_for_forget, thread_for_forget) = (root.clone(), thread.clone());
    let r = tokio::task::spawn_blocking(move || match op.as_str() {
        "integrate" => integrate(&root, &thread, &boxes),
        "land" => land(&root, &thread),
        "update" => update(&root, &thread),
        // the thread's own folder and branch go (saved to the archive first); its agents' boxes are not touched, so a thread whose terminals cannot be closed (a team's Leader) can still have its work discarded
        "discard" => { let _ = &boxes; discard(&root, &thread).map(|_| json!({ "ok": true, "discarded": thread, "archived": true })) }
        other => Err(format!("op is one of: integrate, land, update, discard (not `{other}`)")),
    }).await.unwrap_or_else(|e| Err(e.to_string()));
    { let (r2, t2) = (root_for_forget, thread_for_forget); forget_tree(&r2, &t2); }
    match r { Ok(mut v) => { if !restart.is_empty() { v["restart"] = json!(restart); } v } Err(e) => json!({ "ok": false, "error": e }) }
}
/// `POST /api/v1/cockpit/cowork/threads/tree {op: isolate|dismiss|integrate|update|land|discard, thread, carry?}`. `isolate` and `dismiss` are the person's alone: an agent only suggests.
pub async fn tree_op(state: Arc<AppState>, body: Bytes) -> Response {
    let b: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let (op, thread) = (b["op"].as_str().unwrap_or(""), b["thread"].as_str().unwrap_or(""));
    if matches!(op, "isolate" | "dismiss") {
        let root = crate::agents_http::active_root(&state).await;
        if root.is_empty() || !crate::cowork_threads::exists(&root, thread) { return reply(json!({ "ok": false, "error": "no such thread" })); }
        if op == "dismiss" { return reply(match crate::thread_worktree::suggest_clear(&root, thread) { Ok(()) => json!({ "ok": true }), Err(e) => json!({ "ok": false, "error": e }) }); }
        let v = crate::thread_worktree::isolate(&state, &root, thread, b["carry"].as_bool()).await; forget_tree(&root, thread);
        return reply(v);
    }
    reply(tree_run(&state, op, thread).await)
}
/// Is antiloki's tools helper there to be started: the file it was named as, or a program of that name on the PATH.
fn helper_found(bin: &str) -> bool { Path::new(bin).is_file() || Path::new(&format!("{bin}.exe")).is_file() || (!bin.contains(['/', '\\']) && antiloki_rs::proc::which(bin).is_some()) }
/// What the person is told about the agent just made, "" when all is well. Pure.
fn agent_note(helper: bool) -> String { if helper { String::new() } else { "antiloki's tools helper (antiloki-agent) was not found, so no agent can use antiloki's tools. Build it with `cargo build -p antiloki-agent`, then restart antiloki.".into() } }
/// The other threads that already have agents in the project folder: two lines of work in one folder can overwrite each other. Pure over the threads view and the panes known to work there.
fn others_in_project(view: &Value, thread: &str, in_project: &std::collections::HashSet<i64>) -> Vec<String> {
    view["threads"].as_array().into_iter().flatten().filter(|t| t["id"] != json!(thread) && t["panes"].as_array().is_some_and(|p| p.iter().any(|x| x["paneId"].as_i64().is_some_and(|i| in_project.contains(&i))))).filter_map(|t| t["name"].as_str().map(String::from)).collect()
}

/// `POST /api/v1/cockpit/agent/new {engine, title, thread, session_id}`: an agent opened in a thread. In Main it is cut from the workspace as ever; in another thread from the thread's branch (its tree is made first).
pub async fn agent_new(state: Arc<AppState>, body: Bytes) -> Response {
    let b: Value = serde_json::from_slice(&body).unwrap_or(Value::Null); let bad = |e: &str| reply(json!({ "ok": false, "error": e }));
    let (engine, title, thread) = (b["engine"].as_str().unwrap_or("").to_string(), b["title"].as_str().unwrap_or("").to_string(), b["thread"].as_str().unwrap_or(crate::cowork_threads::MAIN).to_string());
    let Some(sid) = b["session_id"].as_i64().filter(|s| *s > 0) else { return bad("session_id is required") };
    if crate::cli::engine_of(&json!(engine)).is_none() { return bad("not an agent engine"); }
    // THE LIST ON THE SCREEN IS REMEMBERED, SO IT CAN BE OLD: whether the program is still here is looked at when the person picks it (2026-11-27)
    let wanted = engine.clone(); if let Some(why) = tokio::task::spawn_blocking(move || crate::engines::gone(&wanted)).await.ok().flatten() { return bad(&why); }
    let root = crate::agents_http::active_root(&state).await; if !crate::cowork_threads::exists(&root, &thread) { return bad("no such thread"); }
    // AN AGENT GETS NO COPY OF ITS OWN (operator 2026-11-27): it works in its thread's folder, which is the thread's worktree when there is one and the project folder when there is not
    let tree = (thread != crate::cowork_threads::MAIN).then(|| path_of(&root, &thread)).filter(|p| p.join(".git").exists()).map(|p| p.to_string_lossy().into_owned());
    let mut settings = json!({ "title": title, "thread": thread }); if let Some(t) = &tree { settings["folder"] = json!(t); }
    // who else is in the project folder is looked at before this one joins it
    let crowded = if tree.is_some() { vec![] } else {
        let view = crate::cowork_threads::view(&root); let running = tokio::task::spawn_blocking(crate::pty_remote::list_running).await.ok().and_then(|r| r.ok()).unwrap_or_default();
        let mut here = std::collections::HashSet::new();
        for t in running { if let Some(p) = crate::pty_remote::pane_of(&t.id) { if let Some(row) = crate::chat_pane::pane_row(&state, p).await { if row["worktree_path"].as_str().unwrap_or("").is_empty() && crate::shared_folder::folder_of(&root, &row) == root { here.insert(p); } } } }
        others_in_project(&view, &thread, &here)
    };
    let input = json!({ "session_id": sid, "base_agent": "dev", "subagent_name": format!("{engine}-{}", crate::cli::now_ms()), "pane_kind": engine, "scope": [], "grants": ["file.read", "file.write"], "provision": false, "tier": "mid", "settings": settings });
    match engine_call(&state, "system.cockpit.pane.add", input, json!({ "actor": "web-fe", "source": "web-fe" })).await {
        Ok((v, _)) => match v["pane"]["id"].as_i64().or_else(|| v["pane_id"].as_i64()) {
            Some(i) => { let note = tokio::task::spawn_blocking(move || agent_note(helper_found(&crate::mcp_install::agent_bin()))).await.unwrap_or_default(); reply(json!({ "ok": true, "pane_id": i, "session_id": sid, "note": note, "folder": tree.unwrap_or_else(|| root.clone()), "crowded": crowded })) }
            None => bad(v["error"].as_str().or_else(|| v["pane"]["error"].as_str()).unwrap_or("the agent could not be made")),
        },
        Err(e) => bad(&e),
    }
}

#[cfg(test)]
mod tests {
    #[test] fn a_thread_says_which_of_its_agents_have_a_copy_of_their_own() {
        let mut v = json!({ "threads": [{ "panes": [{ "paneId": 1, "title": "Old" }, { "paneId": 2, "title": "New" }] }] });
        mark_copies(&mut v, &[1i64].into_iter().collect()); assert_eq!(v["threads"][0]["copies"], json!(["Old"]));
    }
    #[test]
    fn a_new_workspace_with_nothing_in_it_becomes_a_repository_and_one_with_files_is_left_alone() {
        let _g = LOCK.lock().unwrap_or_else(|p| p.into_inner()); let base = std::env::temp_dir().join(format!("antiloki-ensure-repo-{}", std::process::id())); let _ = std::fs::remove_dir_all(&base);
        let home = base.join("home"); std::fs::create_dir_all(&home).unwrap(); *TEST_BASE.lock().unwrap() = Some(home);
        let (fresh, busy) = (base.join("fresh"), base.join("busy")); std::fs::create_dir_all(fresh.join(".antiloki")).unwrap(); std::fs::create_dir_all(&busy).unwrap(); std::fs::write(busy.join("notes.txt"), "mine").unwrap();
        assert!(only_ours(&[".antiloki".into(), "CLAUDE.md".into()]) && !only_ours(&[".antiloki".into(), "src".into()]));
        ensure_repo(fresh.to_str().unwrap()).unwrap(); assert!(git(&["rev-parse", "HEAD"], &fresh).is_ok(), "one commit to cut the trees from");
        assert!(git(&["status", "--porcelain"], &fresh).unwrap().trim().is_empty(), "antiloki's own folder is kept out of it");
        let (p, slug) = ensure(fresh.to_str().unwrap(), "alpha").unwrap(); assert!(p.join(".git").exists() && !slug.is_empty(), "a thread's tree can now be made");
        let e = ensure_repo(busy.to_str().unwrap()).unwrap_err(); assert!(e.contains("Start Git") && !busy.join(".git").exists(), "{e}");
        let _ = git(&["worktree", "remove", "--force", &p.to_string_lossy()], &fresh); let _ = std::fs::remove_dir_all(&p); *TEST_BASE.lock().unwrap() = None; let _ = std::fs::remove_dir_all(&base);
    }
    use super::*;
    // ANTILOKI_HOME is process-wide: the tests that set it take turns
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    #[test]
    fn the_person_is_told_of_a_missing_helper_and_of_other_work_in_the_same_folder() {
        assert_eq!(agent_note(true), ""); assert!(agent_note(false).contains("antiloki-agent")); assert!(!helper_found("/nowhere/at/all/antiloki-agent"));
        let v = json!({ "threads": [{ "id": "main", "name": "Main", "panes": [{ "paneId": 1 }] }, { "id": "fix", "name": "Fix", "panes": [{ "paneId": 2 }] }, { "id": "idle", "name": "Idle", "panes": [{ "paneId": 3 }] }] });
        let here = [1i64, 2].into_iter().collect();
        assert_eq!(others_in_project(&v, "fix", &here), vec!["Main".to_string()]); assert!(others_in_project(&v, "main", &[1i64].into_iter().collect()).is_empty());
    }
    #[test] fn starting_git_for_the_person_commits_their_files_and_leaves_secrets_and_antilokis_own_out() {
        let d = std::env::temp_dir().join(format!("antiloki-tt-startgit-{}", std::process::id())); let _ = std::fs::remove_dir_all(&d); std::fs::create_dir_all(d.join(".antiloki")).unwrap(); std::fs::create_dir_all(d.join("node_modules")).unwrap(); std::fs::create_dir_all(d.join(".cursor")).unwrap();
        for (f, t) in [("app.js", "x\n"), (".env", "SECRET=1\n"), ("node_modules/dep.js", "y\n"), (".antiloki/state", "z\n"), (".cursor/mcp.json", "{}\n")] { std::fs::write(d.join(f), t).unwrap(); }
        let r = d.to_string_lossy().to_string(); start_git(&r).unwrap();
        let mut files: Vec<String> = git(&["ls-files"], &d).unwrap().lines().map(String::from).collect(); files.sort();
        assert_eq!(files, vec![".gitignore", "app.js"]); assert!(!changed(&d)); assert!(d.join(".env").is_file(), "left out of Git, still in the folder");
        start_git(&r).unwrap();   // a second click changes nothing
        let _ = std::fs::remove_dir_all(&d);
    }
    #[test] fn an_agent_is_its_own_git_author_with_a_mail_of_its_own() {
        assert_eq!(agent_author("Backend (Codex)"), ("Backend (Codex)".to_string(), "backend-codex@agents.antiloki.local".to_string())); assert_eq!(agent_author("  <x>\n").0, "x"); assert_eq!(agent_author("???").1, "agent@agents.antiloki.local"); assert_eq!(agent_author("").0, "agent");
        assert!(agent_author(&"a".repeat(200)).0.len() <= 60);
    }
    fn repo(tag: &str) -> PathBuf { let d = std::env::temp_dir().join(format!("antiloki-tt-{tag}-{}", std::process::id())); let _ = std::fs::remove_dir_all(&d); std::fs::create_dir_all(&d).unwrap(); let g = |a: &[&str]| { git(a, &d).unwrap(); }; g(&["init", "-q", "-b", "main"]); std::fs::write(d.join("a.txt"), "one\n").unwrap(); g(&["add", "-A"]); let mut c: Vec<&str> = IDENT.to_vec(); c.extend(["commit", "-q", "-m", "init"]); git(&c, &d).unwrap(); d }
    #[test]
    fn a_threads_tree_is_named_by_the_workspace_and_the_thread_and_main_has_none() {
        let a = slug("/w/one", "fix"); assert!(a.starts_with("one-") && a.ends_with("-fix"), "the name says the project and the thread"); assert_ne!(a, slug("/w/two", "fix"), "two workspaces' threads never share a tree"); assert_eq!(a, slug("/w/one", "fix"));
        assert!(ensure("/w/one", "main").unwrap_err().contains("Main"));
    }
    #[test] fn a_thread_that_fell_behind_says_so_and_updating_it_from_the_workspace_keeps_its_work_and_deletes_nothing() {
        let _g = LOCK.lock().unwrap_or_else(|p| p.into_inner()); let root = repo("upd"); let home = std::env::temp_dir().join(format!("antiloki-tt-home-upd-{}", std::process::id())); let _ = std::fs::remove_dir_all(&home); *TEST_BASE.lock().unwrap() = Some(home.clone());
        let rs = root.to_string_lossy().to_string(); let (tree, branch) = ensure(&rs, "fix").unwrap();
        std::fs::write(tree.join("mine.txt"), "thread work\n").unwrap(); let mut c: Vec<&str> = IDENT.to_vec(); c.extend(["add", "-A"]); git(&["add", "-A"], &tree).unwrap(); let mut c2: Vec<&str> = IDENT.to_vec(); c2.extend(["commit", "-q", "-m", "thread work"]); git(&c2, &tree).unwrap();
        std::fs::write(root.join("main.txt"), "main moved\n").unwrap(); git(&["add", "-A"], &root).unwrap(); let mut c3: Vec<&str> = IDENT.to_vec(); c3.extend(["commit", "-q", "-m", "main moved"]); git(&c3, &root).unwrap();
        let st = state(&rs, "fix").unwrap(); assert_eq!(st["behind"], json!(1), "{st}"); assert_eq!(st["ahead"], json!(1)); assert_eq!(st["files"], json!(1)); assert!(st["lines"].as_i64().unwrap() >= 1); assert_eq!(st["age_days"], json!(0));
        std::fs::write(tree.join("dirty.txt"), "x").unwrap(); assert!(update(&rs, "fix").unwrap_err().contains("uncommitted")); std::fs::remove_file(tree.join("dirty.txt")).unwrap();
        let r = update(&rs, "fix").unwrap(); assert_eq!(r["ok"], json!(true), "{r}"); assert!(tree.join("main.txt").is_file() && tree.join("mine.txt").is_file(), "the thread has main's work and keeps its own");
        let st = state(&rs, "fix").unwrap(); assert_eq!(st["behind"], json!(0), "{st}"); assert!(update(&rs, "fix").unwrap()["already"] == json!(true));
        assert!(git(&["rev-parse", "--verify", &branch], &root).is_ok(), "the branch is still there");
        let _ = git(&["worktree", "remove", "--force", &tree.to_string_lossy()], &root); let _ = std::fs::remove_dir_all(&home); *TEST_BASE.lock().unwrap() = None;
    }
    #[test] fn a_shortstat_is_read_into_files_and_lines() { assert_eq!(parse_shortstat(" 3 files changed, 40 insertions(+), 2 deletions(-)"), (3, 42)); assert_eq!(parse_shortstat(" 1 file changed, 1 insertion(+)"), (1, 1)); assert_eq!(parse_shortstat(""), (0, 0)); }
    #[test]
    fn two_agents_work_apart_and_integrate_into_the_threads_branch_which_both_then_take_and_the_workspace_is_untouched_until_it_is_landed() {
        let _g = LOCK.lock().unwrap_or_else(|p| p.into_inner()); let root = repo("int"); let home = std::env::temp_dir().join(format!("antiloki-tt-home-{}", std::process::id())); let _ = std::fs::remove_dir_all(&home); *TEST_BASE.lock().unwrap() = Some(home.join("worktrees"));
        let rs = root.to_string_lossy().to_string(); let (tree, branch) = ensure(&rs, "fix").unwrap(); assert!(tree.join("a.txt").is_file() && branch.ends_with("-fix"));
        // two agents' boxes, cut from the thread's branch
        let mk = |n: &str| { let p = home.join(format!("box-{n}")); git(&["worktree", "add", "-b", &format!("box-{n}"), &p.to_string_lossy(), &branch], &root).unwrap(); p };
        let (b1, b2) = (mk("1"), mk("2")); std::fs::write(b1.join("one.txt"), "from one\n").unwrap(); std::fs::write(b2.join("two.txt"), "from two\n").unwrap(); std::fs::create_dir_all(b1.join(".antiloki")).unwrap(); std::fs::write(b1.join(".antiloki/prompts.jsonl"), "x").unwrap();
        let boxes = vec![("one".to_string(), 1, b1.clone()), ("two".to_string(), 2, b2.clone())];
        let r = integrate(&rs, "fix", &boxes).unwrap(); assert_eq!(r["ok"], json!(true), "{r}"); assert_eq!(r["merged"].as_array().unwrap().len(), 2);
        assert!(tree.join("one.txt").is_file() && tree.join("two.txt").is_file() && !tree.join(".antiloki/prompts.jsonl").exists(), "both agents' work is in the thread's tree, and antiloki's own files are not");
        assert!(b1.join("two.txt").is_file() && b2.join("one.txt").is_file(), "each took the other's work back");
        assert!(!root.join("one.txt").exists() && !root.join("two.txt").exists(), "the workspace itself is untouched");
        assert_eq!(integrate(&rs, "fix", &boxes).unwrap()["merged"], json!([]), "a second integrate has nothing to merge");
        let st = state(&rs, "fix").unwrap(); assert_eq!((st["ahead"].clone(), st["dirty"].clone()), (json!(4), json!(false)), "two agents' commits and two merges are ahead of the workspace: {st}");
        // landing: the workspace must be clean
        std::fs::write(root.join("dirty.txt"), "x").unwrap(); assert!(land(&rs, "fix").unwrap_err().contains("project folder has uncommitted changes (dirty.txt)")); std::fs::remove_file(root.join("dirty.txt")).unwrap();
        let l = land(&rs, "fix").unwrap(); assert_eq!(l["ok"], json!(true), "{l}"); assert!(root.join("one.txt").is_file() && root.join("two.txt").is_file());
        std::fs::write(tree.join("late.md"), "the last words").unwrap(); discard(&rs, "fix").unwrap(); assert!(!tree.exists()); assert!(crate::team_archive::list(&rs, "thread:fix").iter().any(|f| f["path"] == json!("late.md")), "what the folder held was saved before it went"); *TEST_BASE.lock().unwrap() = None; let _ = std::fs::remove_dir_all(&home); let _ = std::fs::remove_dir_all(&root);
    }
    #[test]
    fn a_conflict_between_agents_is_reported_and_undone_never_forced() {
        let _g = LOCK.lock().unwrap_or_else(|p| p.into_inner()); let root = repo("conf"); let home = std::env::temp_dir().join(format!("antiloki-tt-home2-{}", std::process::id())); let _ = std::fs::remove_dir_all(&home); *TEST_BASE.lock().unwrap() = Some(home.join("worktrees"));
        let rs = root.to_string_lossy().to_string(); let (tree, branch) = ensure(&rs, "c").unwrap();
        let mk = |n: &str| { let p = home.join(format!("cbox-{n}")); git(&["worktree", "add", "-b", &format!("cbox-{n}"), &p.to_string_lossy(), &branch], &root).unwrap(); p };
        let (b1, b2) = (mk("1"), mk("2")); std::fs::write(b1.join("a.txt"), "agent one's line\n").unwrap(); std::fs::write(b2.join("a.txt"), "agent two's line\n").unwrap();
        let r = integrate(&rs, "c", &[("one".into(), 1, b1.clone()), ("two".into(), 2, b2.clone())]).unwrap();
        assert_eq!(r["ok"], json!(false)); assert_eq!(r["merged"].as_array().unwrap().len(), 1, "the first got in"); assert_eq!(r["conflicts"][0]["agent"], json!("two")); assert_eq!(r["conflicts"][0]["files"], json!(["a.txt"]));
        assert_eq!(std::fs::read_to_string(tree.join("a.txt")).unwrap(), "agent one's line\n", "the thread's tree holds the first, whole: nothing half-merged"); assert!(!changed(&tree), "the failed merge was undone");
        assert_eq!(std::fs::read_to_string(b2.join("a.txt")).unwrap(), "agent two's line\n", "the agent that conflicted keeps its own work");
        discard(&rs, "c").unwrap(); *TEST_BASE.lock().unwrap() = None; let _ = std::fs::remove_dir_all(&home); let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_threads_tree_is_a_tree_dir_and_listed_with_the_threads_of_its_space_but_units_are_not() {
        let _g = LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let base = std::env::temp_dir().join(format!("tt-trees-{}", std::process::id())); let _ = std::fs::remove_dir_all(&base); std::fs::create_dir_all(&base).unwrap(); *TEST_BASE.lock().unwrap() = Some(base.clone());
        let rs = "/space/one"; let mk = |name: &str| { let p = base.join(name.to_lowercase()); std::fs::create_dir_all(p.join(".git")).unwrap(); p };
        let a = mk(&slug(rs, "video-editing")); let _unit = mk(&format!("{}--123-unit", slug(rs, "video-editing"))); let _other = mk(&slug("/space/two", "x"));
        let listed = trees_of(rs); assert_eq!(listed.len(), 1, "{listed:?}"); assert_eq!(listed[0].0, "video-editing"); assert_eq!(listed[0].1, a);
        assert!(is_tree_dir(&a.to_string_lossy()) && is_tree_dir(&_unit.to_string_lossy()), "a thread's tree and a unit are trees"); assert!(!is_tree_dir(&base.to_string_lossy()), "the folder of the trees is not one"); assert!(!is_tree_dir("/tmp") && !is_tree_dir("/nonexistent/x"));
        *TEST_BASE.lock().unwrap() = None; let _ = std::fs::remove_dir_all(&base);
    }
    #[test]
    fn the_sidebars_tree_state_never_waits_for_git_and_catches_up() {
        let _g = LOCK.lock().unwrap_or_else(|p| p.into_inner());   // the other tests point the trees at a folder of their own
        let base = std::env::temp_dir().join(format!("tt-cache-base-{}", std::process::id())); let _ = std::fs::remove_dir_all(&base); std::fs::create_dir_all(&base).unwrap(); *TEST_BASE.lock().unwrap() = Some(base.clone());   // and so does this one: its tree is looked for in the same place the whole way through (a test that left another base behind made the lookup miss, found 2026-11-19)
        let rs = std::env::temp_dir().join(format!("tt-cache-{}", std::process::id())); let _ = std::fs::remove_dir_all(&rs); std::fs::create_dir_all(&rs).unwrap(); let rs = rs.to_string_lossy().to_string();
        assert!(state_cached(&rs, "nothing").is_none(), "a thread without a tree has no state");
        let p = path_of(&rs, "t1"); std::fs::create_dir_all(p.join(".git")).unwrap();
        let first = state_cached(&rs, "t1").unwrap(); assert_eq!((first["ahead"].clone(), first["dirty"].clone()), (json!(0), json!(false)), "the first answer is immediate and empty");
        for _ in 0..50 { if tree_cache().lock().unwrap().contains_key(&p.to_string_lossy().to_string()) { break; } std::thread::sleep(std::time::Duration::from_millis(100)); }
        assert!(tree_cache().lock().unwrap().contains_key(&p.to_string_lossy().to_string()), "the refresh ran in the background");
        // a fresh answer is served as it is (no git, no waiting); after `forget_tree` the next answer starts from nothing again
        tree_cache().lock().unwrap().insert(p.to_string_lossy().to_string(), (std::time::Instant::now(), 3, true, json!({})));
        let kept = state_cached(&rs, "t1").unwrap(); assert_eq!((kept["ahead"].clone(), kept["dirty"].clone()), (json!(3), json!(true)));
        forget_tree(&rs, "t1"); let cold = state_cached(&rs, "t1").unwrap(); assert_eq!((cold["ahead"].clone(), cold["dirty"].clone()), (json!(0), json!(false)));
        let _ = std::fs::remove_dir_all(&rs); *TEST_BASE.lock().unwrap() = None; let _ = std::fs::remove_dir_all(&base);
    }
}
