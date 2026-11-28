//! A WORKTREE IS A THREAD'S, AND ONLY WHEN THE PERSON ASKS (operator 2026-11-27: "worktrees works through threads", "not agents", and: manual by default, an agent may suggest, nothing makes one silently).
//!
//! Every agent works in its thread's folder: the project folder, until the person creates a worktree for the thread. Then the thread's running agents MOVE into it with their conversations (Claude Code resumes its own;
//! the other engines get the handover a provider switch uses), and new ones open there. Uncommitted changes are COPIED across only if the person says so; the project folder is never changed by this.
//! Main works in the project folder. A team run's parallel workers keep their short-lived copies (`thread_tree::ensure_unit`).
use crate::dispatch::{engine_call, AppState};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Copy what is not committed in the project folder into the thread's new worktree: the changes to tracked files as one patch, and the files Git does not follow yet. The project folder is only read.
pub fn carry(root: &str, tree: &Path) -> Result<usize, String> {
    let r = Path::new(root);
    let mut moved = 0;
    let patch = std::env::temp_dir().join(format!("antiloki-carry-{}-{}.patch", std::process::id(), crate::cli::now_ms()));
    let mut diff: Vec<String> = vec!["diff".into(), "--binary".into(), format!("--output={}", patch.to_string_lossy()), "HEAD".into(), "--".into(), ".".into()];
    diff.extend(crate::team_run::NOT_WORK.iter().map(|n| format!(":(exclude){n}")));
    crate::thread_tree::git(&diff.iter().map(String::as_str).collect::<Vec<_>>(), r)?;
    let size = std::fs::metadata(&patch).map(|m| m.len()).unwrap_or(0);
    let applied = if size > 0 { crate::thread_tree::git(&["apply", "--whitespace=nowarn", &patch.to_string_lossy()], tree).map(|_| ()) } else { Ok(()) };
    let _ = std::fs::remove_file(&patch);
    applied.map_err(|e| format!("the uncommitted changes do not fit the thread's worktree, so none were copied: {e}"))?;
    if size > 0 { moved += crate::thread_tree::git(&["diff", "--name-only", "HEAD"], tree).map(|o| o.lines().count()).unwrap_or(0); }
    let others = crate::thread_tree::git(&["-c", "core.quotepath=off", "ls-files", "--others", "--exclude-standard", "-z"], r)?;
    for f in others.split('\0').filter(|f| !f.trim().is_empty()) {
        if crate::team_run::NOT_WORK.iter().any(|n| f == *n || f.starts_with(&format!("{n}/"))) { continue; }
        let (from, to) = (r.join(f), tree.join(f));
        if !from.is_file() || to.exists() { continue; }
        if let Some(d) = to.parent() { std::fs::create_dir_all(d).map_err(|e| e.to_string())?; }
        std::fs::copy(&from, &to).map_err(|e| format!("{f}: {e}"))?;
        moved += 1;
    }
    Ok(moved)
}

fn copy_dir(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for e in std::fs::read_dir(from)?.flatten() { let (a, b) = (e.path(), to.join(e.file_name())); if a.is_dir() { copy_dir(&a, &b)?; } else { std::fs::copy(&a, &b)?; } }
    Ok(())
}

/// Take an agent's own files from one folder to another: its records, its start stamp and, for Claude Code, its conversation, which Claude finds by folder. Blocking.
pub fn move_agent_files(from: &str, to: &str, pane: i64, claude_home: &Path) -> Result<(), String> {
    let (a, b) = (crate::shared_folder::records(from, pane), crate::shared_folder::records(to, pane));
    if a.is_dir() { copy_dir(&a, &b).map_err(|e| e.to_string())?; }
    std::fs::create_dir_all(&b).map_err(|e| e.to_string())?;
    if let Some(session) = crate::shared_folder::session_of(from, pane) {
        let at = |folder: &str| claude_home.join("projects").join(antiloki_rs::context_context_library::claude_project_slug(folder)).join(format!("{session}.jsonl"));
        let (old, new) = (at(from), at(to));
        if old.is_file() { if let Some(d) = new.parent() { std::fs::create_dir_all(d).map_err(|e| e.to_string())?; } std::fs::copy(&old, &new).map_err(|e| e.to_string())?; }
    }
    crate::shared_folder::move_stamp(from, to, pane);
    if a.is_dir() && a != b { let _ = std::fs::remove_dir_all(&a); }
    Ok(())
}

/// The agents of a thread that share a folder (no copy of their own): (pane number, engine, the folder each is in, its row).
async fn agents_of(state: &Arc<AppState>, root: &str, thread: &str) -> Vec<(i64, String, String, Value)> {
    let ids: Vec<i64> = crate::cowork_threads::view(root)["threads"].as_array().into_iter().flatten().filter(|t| t["id"] == json!(thread)).flat_map(|t| t["panes"].as_array().cloned().unwrap_or_default()).filter_map(|p| p["paneId"].as_i64()).collect();
    let mut out = vec![];
    for id in ids {
        let Some(row) = crate::chat_pane::pane_row(state, id).await else { continue };
        let kind = row["pane_kind"].as_str().unwrap_or("").to_string();
        if !row["worktree_path"].as_str().unwrap_or("").is_empty() || crate::cli::engine_of(&json!(kind)).is_none() { continue; }
        out.push((id, kind, crate::shared_folder::folder_of(root, &row), row));
    }
    out
}

/// Which of these agents is at work right now. An agent is never moved in the middle of an answer.
async fn busy(agents: &[(i64, String, String, Value)]) -> Vec<i64> {
    let list: Vec<(i64, String, String)> = agents.iter().map(|a| (a.0, a.1.clone(), a.2.clone())).collect();
    tokio::task::spawn_blocking(move || list.into_iter().filter(|(id, kind, folder)| {
        let rows = crate::pty_remote::screen_rows(*id);
        rows.is_some() && crate::cowork::usage_for_pane(Path::new(folder), *id, kind, rows.as_deref())["working"] == true
    }).map(|a| a.0).collect()).await.unwrap_or_default()
}

/// Move a thread's agents into `to` (a folder, or None: back to the project folder). The screen starts their terminals again; each goes on with its conversation. Returns the ones that moved.
async fn move_agents(state: &Arc<AppState>, root: &str, agents: Vec<(i64, String, String, Value)>, to: Option<&Path>) -> Result<Vec<i64>, String> {
    let target = to.map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|| root.to_string());
    let home = PathBuf::from(std::env::var("HOME").or_else(|_| std::env::var("USERPROFILE")).unwrap_or_default()).join(".claude");
    let mut moved = vec![];
    for (id, kind, from, row) in agents {
        if from == target { continue; }
        // the conversation is read BEFORE the files move: engines other than Claude Code keep it by folder and get it handed over
        if kind != "claude-code" { crate::pane_provider::carry_conversation(state, id, &row, &target).await; }
        let (f, t, h) = (from.clone(), target.clone(), home.clone());
        let engine = kind.clone();
        tokio::task::spawn_blocking(move || -> Result<(), String> {
            move_agent_files(&f, &t, id, &h)?;
            // the other engines keep conversations by folder: in the new one they start a new conversation (with the handover), never an old one of that folder
            if engine != "claude-code" { crate::shared_folder::stamp(&t, id, &engine, "", crate::cli::now_ms() as u64); }
            Ok(())
        }).await.map_err(|e| e.to_string())??;
        let mut settings: Value = serde_json::from_str(row["settings_json"].as_str().unwrap_or("{}")).unwrap_or(json!({}));
        if !settings.is_object() { settings = json!({}); }
        match to { Some(_) => settings["folder"] = json!(target), None => { settings.as_object_mut().map(|o| o.remove("folder")); } }
        engine_call(state, "system.cockpit.pane.update", json!({ "pane_id": id, "settings": settings }), json!({ "actor": "web-fe", "source": "web-fe" })).await?;
        moved.push(id);
    }
    Ok(moved)
}

/// `isolate`: the person asks for a worktree for this thread. `carry`: None = ask first when the project folder has uncommitted changes, true = copy them, false = leave them.
pub async fn isolate(state: &Arc<AppState>, root: &str, thread: &str, with_changes: Option<bool>) -> Value {
    if thread == crate::cowork_threads::MAIN { return json!({ "ok": false, "error": "Main works in the project folder. Make a new thread to give work its own worktree" }); }
    let r = root.to_string();
    let (is_git, dirty) = tokio::task::spawn_blocking(move || { let p = Path::new(&r); (crate::thread_tree::git(&["rev-parse", "--is-inside-work-tree"], p).is_ok(), crate::thread_tree::dirty_files(p)) }).await.unwrap_or((false, vec![]));
    if !is_git { return json!({ "ok": false, "no_git": true, "error": "a worktree needs Git, and this project has none yet" }); }
    let agents = agents_of(state, root, thread).await;
    let at_work = busy(&agents).await;
    if !at_work.is_empty() { return json!({ "ok": false, "busy": at_work, "error": "an agent of this thread is still answering: wait for it, or stop it, then create the worktree" }); }
    let already = crate::thread_tree::path_of(root, thread).join(".git").exists();
    if !already && !dirty.is_empty() && with_changes.is_none() { return json!({ "ok": false, "ask_carry": true, "files": dirty.iter().take(30).collect::<Vec<_>>(), "count": dirty.len() }); }
    let (r, t, take) = (root.to_string(), thread.to_string(), !already && with_changes == Some(true));
    let made = tokio::task::spawn_blocking(move || -> Result<(PathBuf, String, usize), String> {
        let (tree, branch) = crate::thread_tree::ensure(&r, &t)?;
        let copied = if take { match carry(&r, &tree) { Ok(n) => n, Err(e) => { let _ = crate::thread_tree::discard(&r, &t); return Err(e); } } } else { 0 };
        Ok((tree, branch, copied))
    }).await.unwrap_or_else(|e| Err(e.to_string()));
    let (tree, branch, copied) = match made { Ok(m) => m, Err(e) => return json!({ "ok": false, "error": e }) };
    let _ = suggest_clear(root, thread);
    match move_agents(state, root, agents, Some(&tree)).await {
        Ok(moved) => json!({ "ok": true, "worktree": tree.to_string_lossy(), "branch": branch, "copied": copied, "restart": moved }),
        Err(e) => json!({ "ok": false, "error": format!("the worktree was made, but an agent could not be moved into it: {e}") }),
    }
}

/// Before a thread's worktree is discarded, its agents go back to the project folder. Returns the ones that moved, or why none may.
pub async fn bring_back(state: &Arc<AppState>, root: &str, thread: &str) -> Result<Vec<i64>, String> {
    let tree = crate::thread_tree::path_of(root, thread).to_string_lossy().into_owned();
    let agents: Vec<_> = agents_of(state, root, thread).await.into_iter().filter(|a| a.2 == tree).collect();
    if !busy(&agents).await.is_empty() { return Err("an agent of this thread is still answering: wait for it, or stop it, first".into()); }
    move_agents(state, root, agents, None).await
}

/// A TEAM MEMBER WORKS IN A COPY OF ITS OWN: a team rolls back and merges each member's files, which must never happen in a folder it shares. An agent without one is given one now (what was said goes with it as a handover) and its terminal has to start again. Returns whether it was moved.
pub async fn give_own_copy(state: &Arc<AppState>, root: &str, pane_id: i64) -> Result<bool, String> {
    let row = crate::chat_pane::pane_row(state, pane_id).await.ok_or("no such agent")?;
    if !row["worktree_path"].as_str().unwrap_or("").is_empty() { return Ok(false); }
    let kind = row["pane_kind"].as_str().unwrap_or("").to_string();
    if crate::cli::engine_of(&json!(kind)).is_none() { return Err("only an agent can be in a team".into()); }
    let folder = crate::shared_folder::folder_of(root, &row);
    if crate::thread_tree::git(&["rev-parse", "--is-inside-work-tree"], Path::new(&folder)).is_err() { return Err("a team needs Git, so that each member has a copy of its own and its changes can be rolled back".into()); }
    if !busy(&[(pane_id, kind, folder.clone(), row.clone())]).await.is_empty() { return Err("that agent is still answering: wait for it, or stop it, then pair it".into()); }
    // the copy's path is not known yet: it is said as "your own copy"
    crate::pane_provider::carry_conversation(state, pane_id, &row, "your own copy of the project").await;
    // cut from the branch of the folder it was in, so a thread's committed work comes with it
    let from = crate::thread_tree::git(&["rev-parse", "HEAD"], Path::new(&folder))?.trim().to_string();
    let input = json!({ "session_id": row["session_id"], "subagent_name": row["subagent_name"], "pane_kind": row["pane_kind"], "base_agent": "dev", "grants": ["file.read", "file.write"], "provision": true, "from": from });
    let (made, _) = engine_call(state, "system.cockpit.pane.add", input, json!({ "actor": "web-fe", "source": "web-fe" })).await?;
    let after = crate::chat_pane::pane_row(state, pane_id).await.unwrap_or(Value::Null);
    if after["worktree_path"].as_str().unwrap_or("").is_empty() { return Err(made["error"].as_str().or_else(|| made["warning"].as_str()).unwrap_or("its own copy could not be made").to_string()); }
    let mut settings: Value = serde_json::from_str(after["settings_json"].as_str().unwrap_or("{}")).unwrap_or(json!({}));
    if settings.as_object_mut().and_then(|o| o.remove("folder")).is_some() { engine_call(state, "system.cockpit.pane.update", json!({ "pane_id": pane_id, "settings": settings }), json!({ "actor": "web-fe", "source": "web-fe" })).await?; }
    Ok(true)
}

fn suggestions_path(root: &str) -> PathBuf { Path::new(root).join(".antiloki").join("worktree-suggestions.json") }
/// The suggestions waiting for the person, by thread.
pub fn suggestions(root: &str) -> Value { std::fs::read_to_string(suggestions_path(root)).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()).filter(Value::is_object).unwrap_or(json!({})) }
/// An agent says the thread's work should have a worktree. It only asks: the person creates it, or says no.
pub fn suggest(root: &str, pane: i64, reason: &str) -> Result<String, String> {
    let reason: String = reason.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(240).collect();
    if reason.is_empty() { return Err("say why the work should have a worktree (`reason`)".into()); }
    let view = crate::cowork_threads::view(root);
    let thread = view["threads"].as_array().into_iter().flatten().find(|t| t["panes"].as_array().is_some_and(|p| p.iter().any(|x| x["paneId"].as_i64() == Some(pane)))).and_then(|t| t["id"].as_str().map(String::from)).ok_or("you are not in a thread on the screen")?;
    if thread == crate::cowork_threads::MAIN { return Err("Main works in the project folder. Ask the person to move this work to a new thread, then suggest a worktree there".into()); }
    if crate::thread_tree::path_of(root, &thread).join(".git").exists() { return Err("this thread has a worktree already".into()); }
    let mut all = suggestions(root);
    all[&thread] = json!({ "pane_id": pane, "reason": reason, "t": crate::cli::now_ms() });
    let p = suggestions_path(root); if let Some(d) = p.parent() { let _ = std::fs::create_dir_all(d); }
    std::fs::write(p, all.to_string()).map_err(|e| e.to_string())?;
    Ok("suggested: the person now sees Create and Not now on this thread. Nothing was made. Carry on in this folder unless they create it".into())
}
/// The person said no, or the worktree was made.
pub fn suggest_clear(root: &str, thread: &str) -> Result<(), String> {
    let mut all = suggestions(root);
    if all.as_object_mut().and_then(|o| o.remove(thread)).is_none() { return Ok(()); }
    std::fs::write(suggestions_path(root), all.to_string()).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn git(a: &[&str], d: &Path) { crate::thread_tree::git(a, d).unwrap(); }
    fn tmp(tag: &str) -> PathBuf { let d = std::env::temp_dir().join(format!("antiloki-wt-{tag}-{}", std::process::id())); let _ = std::fs::remove_dir_all(&d); std::fs::create_dir_all(&d).unwrap(); d }
    #[test] fn uncommitted_work_is_copied_into_the_worktree_and_the_project_folder_is_left_as_it_was() {
        let base = tmp("carry"); let (root, tree) = (base.join("project"), base.join("tree")); std::fs::create_dir_all(&root).unwrap();
        git(&["init", "-q", "-b", "main"], &root); std::fs::write(root.join("a.txt"), "one\n").unwrap(); std::fs::write(root.join("gone.txt"), "x\n").unwrap(); git(&["add", "-A"], &root);
        git(&["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", "init"], &root);
        git(&["worktree", "add", "-q", "-b", "side", &tree.to_string_lossy()], &root);
        std::fs::write(root.join("a.txt"), "one\ntwo\n").unwrap(); std::fs::remove_file(root.join("gone.txt")).unwrap(); std::fs::create_dir_all(root.join("docs")).unwrap(); std::fs::write(root.join("docs/new plan.md"), "plan\n").unwrap();
        std::fs::create_dir_all(root.join(".antiloki")).unwrap(); std::fs::write(root.join(".antiloki/x"), "ours").unwrap();
        let before = crate::thread_tree::git(&["status", "--porcelain", "--untracked-files=all"], &root).unwrap();
        assert!(carry(&root.to_string_lossy(), &tree).unwrap() >= 3);
        assert_eq!(std::fs::read_to_string(tree.join("a.txt")).unwrap(), "one\ntwo\n"); assert!(!tree.join("gone.txt").exists()); assert_eq!(std::fs::read_to_string(tree.join("docs/new plan.md")).unwrap(), "plan\n"); assert!(!tree.join(".antiloki/x").exists());
        assert_eq!(crate::thread_tree::git(&["status", "--porcelain", "--untracked-files=all"], &root).unwrap(), before, "nothing moved out of the person's folder");
        let _ = std::fs::remove_dir_all(base);
    }
    #[test] fn a_moved_agent_takes_its_records_its_stamp_and_its_claude_conversation() {
        let base = tmp("move"); let (a, b, home) = (base.join("a").to_string_lossy().into_owned(), base.join("b").to_string_lossy().into_owned(), base.join("claude"));
        crate::shared_folder::stamp(&a, 7, "claude-code", "abc-1", 1000);
        let r = crate::shared_folder::records(&a, 7); std::fs::create_dir_all(&r).unwrap(); std::fs::write(r.join("prompts.jsonl"), "p\n").unwrap();
        let slug = |f: &str| home.join("projects").join(antiloki_rs::context_context_library::claude_project_slug(f));
        std::fs::create_dir_all(slug(&a)).unwrap(); std::fs::write(slug(&a).join("abc-1.jsonl"), "said\n").unwrap();
        move_agent_files(&a, &b, 7, &home).unwrap();
        assert_eq!(std::fs::read_to_string(crate::shared_folder::records(&b, 7).join("prompts.jsonl")).unwrap(), "p\n"); assert!(!r.exists());
        assert_eq!(crate::shared_folder::session_of(&b, 7).as_deref(), Some("abc-1")); assert_eq!(crate::shared_folder::session_of(&a, 7), None);
        assert_eq!(std::fs::read_to_string(slug(&b).join("abc-1.jsonl")).unwrap(), "said\n");
        let _ = std::fs::remove_dir_all(base);
    }
    #[test] fn a_suggestion_only_asks_and_main_takes_none() {
        let d = tmp("suggest"); let r = d.to_string_lossy().into_owned();
        crate::cowork_threads::apply(&r, &json!({ "op": "create", "name": "pricing" })).unwrap();
        std::fs::create_dir_all(d.join(".antiloki/threads")).unwrap();
        std::fs::write(crate::cowork_threads::layout_file(&r, "pricing"), json!({ "root": { "type": "leaf", "pane": { "id": "x", "kind": "engine", "paneId": 7 } } }).to_string()).unwrap();
        std::fs::write(crate::cowork_threads::layout_file(&r, "main"), json!({ "root": { "type": "leaf", "pane": { "id": "y", "kind": "engine", "paneId": 8 } } }).to_string()).unwrap();
        assert!(suggest(&r, 7, " ").is_err()); assert!(suggest(&r, 8, "big").unwrap_err().contains("Main")); assert!(suggest(&r, 9, "big").is_err());
        suggest(&r, 7, "a  large\nrewrite").unwrap(); assert_eq!(suggestions(&r)["pricing"]["reason"], json!("a large rewrite"));
        assert!(!crate::thread_tree::path_of(&r, "pricing").exists(), "nothing is made");
        suggest_clear(&r, "pricing").unwrap(); assert_eq!(suggestions(&r), json!({}));
        let _ = std::fs::remove_dir_all(d);
    }
}
