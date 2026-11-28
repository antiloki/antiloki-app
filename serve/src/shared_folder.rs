//! AGENTS THAT SHARE THE PROJECT'S FOLDER (no Git: no copy of their own). The CLIs keep their conversations by folder, so "the newest conversation here" was an EARLIER AGENT'S: a new agent's chat view showed it (2026-11-27).
//! Each start leaves a stamp; an agent reads only conversations begun after its own start and before the next agent of the same engine started.
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

/// From (included) and to (excluded), in milliseconds.
pub type Window = (u64, u64);
/// An agent with a folder of its own: every conversation there is its own.
pub const ALL: Window = (0, u64::MAX);
/// An agent in the shared folder that left no stamp yet: nothing is known to be its own.
pub const NONE: Window = (u64::MAX, u64::MAX);
/// Is a conversation begun at `t` this agent's? Pure.
pub fn holds(w: Window, t: u64) -> bool { w == ALL || (t >= w.0 && t < w.1) }
/// When a conversation file began (its creation time, else its last change).
pub fn born(p: &Path) -> u64 { p.metadata().and_then(|m| m.created().or_else(|_| m.modified())).ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_millis() as u64).unwrap_or(0) }

/// Where an agent that shares a folder keeps its own records: its prompts, answers, trace, meter and the settings its CLI is started with. Two agents in one folder never write the same file.
pub fn records(folder: &str, pane: i64) -> PathBuf { Path::new(folder).join(".antiloki").join("agents").join(pane.to_string()) }
/// A pane's records folder, whichever way it works: its own when it shares the folder, else the folder's `.antiloki`.
pub fn records_of(folder: &str, pane: i64) -> PathBuf { let own = records(folder, pane); if own.is_dir() { own } else { Path::new(folder).join(".antiloki") } }
fn dir(root: &str) -> PathBuf { Path::new(root).join(".antiloki").join("panes") }
fn read(p: &Path) -> Option<Value> { serde_json::from_str(&std::fs::read_to_string(p).ok()?).ok() }
fn own(root: &str, pane: i64) -> Option<Value> { read(&dir(root).join(format!("{pane}.json"))) }
/// Written at every start (a start replaces the terminal). `session` is the conversation antiloki named (Claude Code), else empty.
pub fn stamp(root: &str, pane: i64, engine: &str, session: &str, now: u64) { let d = dir(root); let _ = std::fs::create_dir_all(&d); let _ = std::fs::write(d.join(format!("{pane}.json")), json!({ "t": now, "engine": engine, "session": session }).to_string()); }
/// The stamp goes where the agent goes (it was moved into a thread's worktree, or back). Its start time is kept.
pub fn move_stamp(from: &str, to: &str, pane: i64) {
    let (a, b) = (dir(from).join(format!("{pane}.json")), dir(to).join(format!("{pane}.json")));
    if a == b || !a.is_file() { return; }
    let _ = std::fs::create_dir_all(dir(to)); if std::fs::copy(&a, &b).is_ok() { let _ = std::fs::remove_file(a); }
}
/// THE FOLDER AN AGENT WORKS IN: its own copy when it has one; else the thread's worktree it was put in (`settings.folder`); else the project folder. Pure over the row.
pub fn folder_of(project: &str, pane: &Value) -> String {
    if let Some(own) = pane["worktree_path"].as_str().filter(|s| !s.is_empty()) { return own.to_string(); }
    let settings: Value = pane["settings_json"].as_str().and_then(|t| serde_json::from_str(t).ok()).unwrap_or(Value::Null);
    settings["folder"].as_str().filter(|f| !f.is_empty() && Path::new(f).is_dir()).map(String::from).unwrap_or_else(|| project.to_string())
}
/// The folder an agent works in, for what only READS it or writes where the person asked (attachments, the diff, the preview, ship). What commits, restores or merges an agent's files by itself must use its own copy (`worktree_path`) and nothing else.
pub async fn working_dir(state: &std::sync::Arc<crate::dispatch::AppState>, pane: &Value) -> String {
    if let Some(own) = pane["worktree_path"].as_str().filter(|s| !s.is_empty()) { return own.to_string(); }
    folder_of(&crate::agents_http::active_root(state).await, pane)
}
/// What the last start left: (engine, the conversation it named or continued).
pub fn last_start(root: &str, pane: i64) -> Option<(String, String)> { let v = own(root, pane)?; Some((v["engine"].as_str()?.to_string(), v["session"].as_str().unwrap_or("").to_string())) }
/// A start that GOES ON with a conversation keeps its start time: the conversation began after it, so the chat view and the sagas go on reading it.
pub fn stamp_resume(root: &str, pane: i64, engine: &str, session: &str) { let t = own(root, pane).and_then(|v| v["t"].as_u64()).unwrap_or(0); stamp(root, pane, engine, session, t); }
/// Is this conversation another agent's? Two agents never continue one conversation.
pub fn taken(root: &str, pane: i64, session: &str) -> bool {
    std::fs::read_dir(dir(root)).into_iter().flatten().flatten().filter(|e| e.path().file_stem().and_then(|s| s.to_str()) != Some(pane.to_string().as_str())).filter_map(|e| read(&e.path())).any(|v| v["session"] == json!(session))
}
/// The files an agent in a shared folder may write, where its hook looks for them. No limits: no file.
pub fn write_limits(records: &Path, scope: &[String]) {
    let file = records.join("context").join("manifest.json");
    if scope.is_empty() { let _ = std::fs::remove_file(file); return; }
    let _ = std::fs::create_dir_all(records.join("context")); let _ = std::fs::write(file, json!({ "scope": scope }).to_string());
}
/// The conversation antiloki named for this agent, if it did.
pub fn session_of(root: &str, pane: i64) -> Option<String> { own(root, pane)?["session"].as_str().filter(|s| !s.is_empty()).map(String::from) }
/// The conversations that can be this agent's.
pub fn window(root: &str, pane: i64) -> Window {
    let Some(mine) = own(root, pane) else { return NONE }; let from = mine["t"].as_u64().unwrap_or(0);
    let until = std::fs::read_dir(dir(root)).into_iter().flatten().flatten().filter_map(|e| read(&e.path())).filter(|v| v["engine"] == mine["engine"]).filter_map(|v| v["t"].as_u64()).filter(|t| *t > from).min().unwrap_or(u64::MAX);
    (from, until)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn an_agent_works_in_its_copy_else_the_threads_worktree_else_the_project() {
        let there = std::env::temp_dir().to_string_lossy().into_owned();
        assert_eq!(folder_of("/p", &json!({ "worktree_path": "/box" })), "/box");
        assert_eq!(folder_of("/p", &json!({ "worktree_path": null, "settings_json": json!({ "folder": there }).to_string() })), there);
        assert_eq!(folder_of("/p", &json!({ "settings_json": "{\"folder\":\"/no/such/tree\"}" })), "/p", "a worktree that is gone sends the agent home");
        assert_eq!(folder_of("/p", &json!({})), "/p");
    }
    #[test] fn limits_are_written_and_taken_away() {
        let d = std::env::temp_dir().join(format!("al-limits-{}", std::process::id()));
        write_limits(&d, &["docs/**".to_string()]); assert!(std::fs::read_to_string(d.join("context/manifest.json")).unwrap().contains("docs/**"));
        write_limits(&d, &[]); assert!(!d.join("context/manifest.json").exists()); let _ = std::fs::remove_dir_all(d);
    }
    #[test] fn a_continued_conversation_keeps_its_start_and_is_one_agents_only() {
        let d = std::env::temp_dir().join(format!("al-resume-{}", std::process::id())); let r = d.to_string_lossy().to_string();
        assert_eq!(last_start(&r, 1), None);
        stamp(&r, 1, "codex", "", 1000); stamp_resume(&r, 1, "codex", "abc");
        assert_eq!(window(&r, 1), (1000, u64::MAX)); assert_eq!(last_start(&r, 1), Some(("codex".into(), "abc".into())));
        stamp(&r, 2, "codex", "", 2000); assert!(taken(&r, 2, "abc")); assert!(!taken(&r, 1, "abc"));
        let _ = std::fs::remove_dir_all(d);
    }
    #[test] fn an_agent_in_the_shared_folder_reads_only_what_began_in_its_own_time() {
        let d = std::env::temp_dir().join(format!("al-shared-{}", std::process::id())); let r = d.to_string_lossy().to_string();
        assert_eq!(window(&r, 1), NONE); assert!(!holds(NONE, 5000));
        stamp(&r, 1, "cursor", "", 1000); assert_eq!(window(&r, 1), (1000, u64::MAX));
        stamp(&r, 2, "cursor", "", 2000); stamp(&r, 3, "codex", "", 1500);
        assert_eq!(window(&r, 1), (1000, 2000)); assert_eq!(window(&r, 2), (2000, u64::MAX));
        assert!(!holds(window(&r, 2), 1200), "a new agent never shows an earlier one's conversation"); assert!(holds(window(&r, 1), 1200)); assert!(holds(ALL, 0));
        assert_eq!(records_of(&r, 1), Path::new(&r).join(".antiloki"));
        std::fs::create_dir_all(records(&r, 1)).unwrap(); assert_eq!(records_of(&r, 1), records(&r, 1)); assert_ne!(records(&r, 1), records(&r, 2), "never one file for two agents"); assert_eq!(records_of(&r, 2), Path::new(&r).join(".antiloki"));
        stamp(&r, 1, "cursor", "abc", 3000); assert_eq!(window(&r, 1), (3000, u64::MAX), "started again: the newest"); assert_eq!(session_of(&r, 1).as_deref(), Some("abc")); assert_eq!(session_of(&r, 2), None);
        let _ = std::fs::remove_dir_all(d);
    }
}
