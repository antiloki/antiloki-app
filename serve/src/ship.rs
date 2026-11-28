//! COMMIT AND PUSH, FROM THE CHAT (operator 2026-10-11: "add below the input field at the agent chat a commit and push button, with an edit commit option or auto").
//!
//! The agent never commits (the episode close does, on its own branch); this is the one click that commits what the agent changed in ITS box, with a message the operator
//! either lets us write (AUTO: what the agent was asked, then the files) or writes (EDIT), and pushes the branch to `origin` when asked. Two routes:
//!   GET  /api/v1/cockpit/agent/ship?pane_id=N      what a click would commit and push - a read, nothing changes
//!   POST /api/v1/cockpit/agent/ship {pane_id, push, message?}   the act. No `message` = the auto one.
//! antiloki's own files in the box (briefs, skills, the events log) are never staged: `boxes::is_ours`. A push never prompts for a password - git is told not to ask, so a
//! missing credential is an error sentence, not a hang.
use crate::chat_pane::{events_text, is_agent_pane, pane_row};
use crate::dispatch::AppState;
use axum::{body::Bytes, http::StatusCode, response::{IntoResponse, Response}, Json};
use serde_json::{json, Value};
use std::sync::Arc;

fn gitx(args: &[&str], cwd: &str) -> Result<String, String> { crate::boxes::git(args, cwd) }

/// The files a commit would take: changed or new, minus the ones antiloki wrote itself. Parses `git status --porcelain` (v1: `XY path`, a rename is `old -> new`). Pure - tested.
pub fn changed_files(porcelain: &str) -> Vec<String> {
    let mut out: Vec<String> = vec![];
    for l in porcelain.lines() {
        if l.len() < 4 { continue; }
        let mut p = l[3..].trim();
        if let Some((_, to)) = p.split_once(" -> ") { p = to; }
        let p = p.trim_matches('"');
        if p.is_empty() || crate::boxes::is_ours(p) { continue; }
        if !out.iter().any(|x| x == p) { out.push(p.to_string()); }
    }
    out
}

/// The message we write when the operator does not: the ask that started the work as the subject, the files as the body. Pure - tested.
pub fn auto_message(ask: &str, files: &[String]) -> String {
    let first = ask.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("");
    let first = first.trim_start_matches(|c: char| c == '#' || c == '@' || c == '!' || c.is_whitespace());
    let subject: String = if first.is_empty() {
        match files { [] => "update".to_string(), [one] => format!("update {one}"), _ => format!("update {} files", files.len()) }
    } else {
        let mut t: String = first.chars().take(72).collect();
        if first.chars().count() > 72 { t = t.trim_end().to_string(); t.push('…'); }
        let mut cs = t.chars(); match cs.next() { Some(c) => c.to_lowercase().collect::<String>() + cs.as_str(), None => t }
    };
    let mut m = subject;
    if !files.is_empty() {
        m.push_str("\n\n");
        for f in files.iter().take(12) { m.push_str(&format!("- {f}\n")); }
        if files.len() > 12 { m.push_str(&format!("- …and {} more\n", files.len() - 12)); }
        m.push_str("\nCommitted from antiloki.");
    }
    m
}

fn has_remote(cwd: &str) -> bool { gitx(&["remote", "get-url", "origin"], cwd).is_ok() }
/// Commits on HEAD that no remote has
fn unpushed(cwd: &str) -> usize { gitx(&["rev-list", "--count", "HEAD", "--not", "--remotes"], cwd).ok().and_then(|s| s.trim().parse().ok()).unwrap_or(0) }

/// What a click would do in this folder. `why` is set (and the lists empty) when the folder is not a repository - a read-only chat.
pub fn status(cwd: &str, ask: &str) -> Value {
    if cwd.is_empty() || !std::path::Path::new(cwd).is_dir() { return json!({ "ok": true, "dirty": false, "files": [], "branch": "", "remote": false, "ahead": 0, "message": "", "why": "this agent has no folder to commit in yet" }); }
    if gitx(&["rev-parse", "--is-inside-work-tree"], cwd).map(|s| s.trim() == "true").unwrap_or(false) == false {
        return json!({ "ok": true, "dirty": false, "files": [], "branch": "", "remote": false, "ahead": 0, "message": "", "why": "this agent's folder is not a git repository - there is nothing to commit" });
    }
    let files = changed_files(&gitx(&["status", "--porcelain", "--untracked-files=all"], cwd).unwrap_or_default());
    let branch = gitx(&["rev-parse", "--abbrev-ref", "HEAD"], cwd).map(|s| s.trim().to_string()).unwrap_or_default();
    let remote = has_remote(cwd);
    let ahead = if remote { unpushed(cwd) } else { 0 };
    json!({ "ok": true, "dirty": !files.is_empty(), "message": auto_message(ask, &files), "files": files, "branch": branch, "remote": remote, "ahead": ahead })
}

/// Commit what changed (when anything did) and push when asked. A failed push after a good commit is reported as both: the commit stands.
pub fn commit_and_push(cwd: &str, message: &str, push: bool) -> Value {
    let files = changed_files(&gitx(&["status", "--porcelain", "--untracked-files=all"], cwd).unwrap_or_default());
    let branch = gitx(&["rev-parse", "--abbrev-ref", "HEAD"], cwd).map(|s| s.trim().to_string()).unwrap_or_default();
    let mut out = json!({ "ok": true, "committed": false, "pushed": false, "branch": branch });
    if !files.is_empty() {
        for chunk in files.chunks(150) {
            let mut args: Vec<&str> = vec!["add", "-A", "--"]; args.extend(chunk.iter().map(String::as_str));
            if let Err(e) = gitx(&args, cwd) { out["ok"] = json!(false); out["error"] = json!(e); return out; }
        }
        if let Err(e) = gitx(&["commit", "-m", message], cwd) { out["ok"] = json!(false); out["error"] = json!(e); return out; }
        out["committed"] = json!(true);
        out["sha"] = json!(gitx(&["rev-parse", "--short", "HEAD"], cwd).map(|s| s.trim().to_string()).unwrap_or_default());
        out["message"] = json!(message);
    }
    if push {
        if !has_remote(cwd) { out["push_error"] = json!("this repository has no remote named origin"); return out; }
        let r = crate::proc::cmd("git").args(["push", "-u", "origin", "HEAD"]).env("GIT_TERMINAL_PROMPT", "0").current_dir(cwd).output();
        match r {
            Ok(o) if o.status.success() => { out["pushed"] = json!(true); }
            Ok(o) => { out["push_error"] = json!(String::from_utf8_lossy(&o.stderr).trim().lines().last().unwrap_or("git push failed").chars().take(300).collect::<String>()); }
            Err(e) => { out["push_error"] = json!(format!("git could not run: {e}")); }
        }
    }
    out
}

async fn pane_and_ask(state: &Arc<AppState>, pane_id: i64) -> Result<(String, String), Response> {
    let Some(pane) = pane_row(state, pane_id).await.filter(is_agent_pane) else { return Err((StatusCode::NOT_FOUND, Json(json!({ "ok": false, "error": "no such agent" }))).into_response()) };
    let cwd = crate::shared_folder::working_dir(state, &pane).await;
    let eng = pane.get("pane_kind").and_then(Value::as_str).unwrap_or("").to_string();
    let ask = crate::chat_pane::episodes_of(events_text(&cwd, pane_id).lines(), &eng).last().map(|e| e.first_ask.clone()).unwrap_or_default();
    Ok((cwd, ask))
}

pub async fn http_preview(state: Arc<AppState>, q: std::collections::HashMap<String, String>) -> Response {
    let pane_id = q.get("pane_id").and_then(|v| v.parse::<i64>().ok()).unwrap_or(0);
    let (cwd, ask) = match pane_and_ask(&state, pane_id).await { Ok(x) => x, Err(r) => return r };
    Json(tokio::task::spawn_blocking(move || status(&cwd, &ask)).await.unwrap_or_else(|e| json!({ "ok": false, "error": e.to_string() }))).into_response()
}

pub async fn http_ship(state: Arc<AppState>, body: Bytes) -> Response {
    let b: Value = serde_json::from_slice(&body).unwrap_or(json!({}));
    let pane_id = b.get("pane_id").and_then(Value::as_i64).unwrap_or(0);
    let push = b.get("push").and_then(Value::as_bool).unwrap_or(false);
    let given = b.get("message").and_then(Value::as_str).map(str::trim).filter(|m| !m.is_empty()).map(str::to_string);
    if b.get("message").map_or(false, |m| m.as_str().map_or(true, |t| t.trim().is_empty())) { return (StatusCode::BAD_REQUEST, Json(json!({ "ok": false, "error": "the commit message is empty" }))).into_response(); }
    let (cwd, ask) = match pane_and_ask(&state, pane_id).await { Ok(x) => x, Err(r) => return r };
    let out = tokio::task::spawn_blocking(move || {
        let st = status(&cwd, &ask);
        if let Some(w) = st.get("why").and_then(Value::as_str) { return json!({ "ok": false, "error": w }); }
        let msg = given.unwrap_or_else(|| st.get("message").and_then(Value::as_str).unwrap_or("update").to_string());
        commit_and_push(&cwd, &msg, push)
    }).await.unwrap_or_else(|e| json!({ "ok": false, "error": e.to_string() }));
    let code = if out.get("ok") == Some(&json!(false)) { StatusCode::BAD_REQUEST } else { StatusCode::OK };
    (code, Json(out)).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn the_status_lines_become_files_without_antilokis_own() {
        let p = " M src/a.rs\n?? new.txt\nR  old.rs -> moved.rs\n?? AGENTS.md\n?? .antiloki/agent-events-3.jsonl\n M src/a.rs\n?? \"with space.md\"\n";
        assert_eq!(changed_files(p), vec!["src/a.rs", "new.txt", "moved.rs", "with space.md"]);
        assert!(changed_files("").is_empty());
    }
    #[test]
    fn the_auto_message_is_the_ask_then_the_files() {
        let m = auto_message("  Fix the login redirect\nand the tests", &["a.ts".into(), "b.ts".into()]);
        assert!(m.starts_with("fix the login redirect\n\n- a.ts\n- b.ts\n"), "{m}");
        assert!(auto_message("x".repeat(100).as_str(), &[]).chars().count() <= 73);
        assert_eq!(auto_message("", &["only.rs".into()]), "update only.rs\n\n- only.rs\n\nCommitted from antiloki.");
        assert_eq!(auto_message("", &[]), "update");
        assert!(auto_message("@main fix it", &[]).starts_with("main fix it"));
        let many: Vec<String> = (0..20).map(|i| format!("f{i}")).collect();
        assert!(auto_message("", &many).contains("…and 8 more"));
    }
    fn repo() -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("antiloki-ship-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        std::fs::create_dir_all(&d).unwrap(); let p = d.to_string_lossy().to_string();
        for a in [&["init", "-q", "-b", "main"][..], &["config", "user.email", "t@t"], &["config", "user.name", "t"], &["config", "commit.gpgsign", "false"]] { gitx(a, &p).unwrap(); }
        d
    }
    #[test]
    fn it_commits_what_changed_leaves_antilokis_files_and_pushes_to_a_remote() {
        let d = repo(); let p = d.to_string_lossy().to_string();
        std::fs::write(d.join("a.txt"), "1").unwrap(); std::fs::write(d.join("AGENTS.md"), "brief").unwrap();
        let st = status(&p, "add the file");
        assert_eq!(st["files"], json!(["a.txt"]));
        assert_eq!(st["remote"], json!(false));
        let r = commit_and_push(&p, "add a", false);
        assert_eq!(r["committed"], json!(true), "{r}");
        assert!(gitx(&["ls-files"], &p).unwrap().contains("a.txt") && !gitx(&["ls-files"], &p).unwrap().contains("AGENTS.md"));
        assert_eq!(status(&p, "")["dirty"], json!(false));
        // nothing new: a second click commits nothing and says so
        assert_eq!(commit_and_push(&p, "again", false)["committed"], json!(false));
        // a push with no remote is an error sentence, and the commit stands
        let r = commit_and_push(&p, "x", true);
        assert!(r["push_error"].as_str().unwrap().contains("no remote"), "{r}");
        // with a bare remote it goes through
        let bare = std::env::temp_dir().join(format!("antiloki-ship-bare-{}.git", std::process::id()));
        gitx(&["init", "-q", "--bare", &bare.to_string_lossy()], &p).unwrap();
        gitx(&["remote", "add", "origin", &bare.to_string_lossy()], &p).unwrap();
        assert_eq!(status(&p, "")["ahead"], json!(1));
        std::fs::write(d.join("b.txt"), "2").unwrap();
        let r = commit_and_push(&p, "add b", true);
        assert_eq!((r["committed"].clone(), r["pushed"].clone()), (json!(true), json!(true)), "{r}");
        assert_eq!(status(&p, "")["ahead"], json!(0));
    }
    #[test]
    fn a_folder_that_is_not_a_repository_says_why() {
        let d = std::env::temp_dir().join(format!("antiloki-ship-nogit-{}", std::process::id())); std::fs::create_dir_all(&d).unwrap();
        assert!(status(&d.to_string_lossy(), "")["why"].as_str().unwrap().contains("not a git repository"));
        assert!(status("", "")["why"].is_string());
    }
}
