//! LANDING WITHOUT THE STALL (the operator 2026-11-15: the merge card said "the workspace has uncommitted changes" only AFTER the click): what in the person's workspace would stop a land, a stash made for them by one click, and the stash put back once the land is done.
//! The stash is this run's: a message that names the run, a note in the run's folder, and only that one is ever popped. Nothing here runs without the person's word: the stash is an op only a person can call.
use serde_json::{json, Value};
use std::path::Path;

const MARK: &str = "antiloki: before landing ";
fn note(root: &str, run: &str) -> std::path::PathBuf { crate::team_graph_run::run_dir(root, run).join("stash.json") }
/// The files that would stop a land, and whether this run already stashed them. Blocking.
pub fn state(root: &str, run: &str) -> Value { let files = crate::thread_tree::dirty_files(Path::new(root)); json!({ "dirty": files, "stashed": note(root, run).exists() }) }
/// Set the person's uncommitted work aside under a message that names the run. A half-added file (git's intent-to-add) is un-marked first, or git refuses to stash. Blocking.
pub fn stash(root: &str, run: &str) -> Result<Value, String> {
    let r = Path::new(root); if crate::thread_tree::dirty_files(r).is_empty() { return Ok(json!({ "ok": true, "stashed": false, "said": "nothing to set aside" })); }
    let msg = format!("{MARK}{run}"); let args = ["stash", "push", "-u", "-m", msg.as_str()];
    let out = match crate::thread_tree::git(&args, r) { Ok(o) => Ok(o), Err(e) if e.contains("not uptodate") => { let _ = crate::thread_tree::git(&["reset", "-q"], r); crate::thread_tree::git(&args, r) } Err(e) => Err(e) }?;
    if !out.contains("Saved") { return Err(format!("git did not stash: {}", out.trim())); }
    let _ = std::fs::create_dir_all(crate::team_graph_run::run_dir(root, run)); let _ = std::fs::write(note(root, run), json!({ "message": msg }).to_string());
    Ok(json!({ "ok": true, "stashed": true }))
}
/// Put this run's stash back (the one whose message names the run), and forget it. An error leaves the stash where it is, so nothing is lost. Blocking.
pub fn restore(root: &str, run: &str) -> Result<Value, String> {
    if !note(root, run).exists() { return Ok(json!({ "ok": true, "restored": false })); } let r = Path::new(root); let msg = format!("{MARK}{run}");
    let list = crate::thread_tree::git(&["stash", "list", "--format=%gd\t%gs"], r)?;
    let Some(rf) = list.lines().find_map(|l| { let (n, s) = l.split_once('\t')?; s.ends_with(&msg).then(|| n.to_string()) }) else { let _ = std::fs::remove_file(note(root, run)); return Ok(json!({ "ok": true, "restored": false, "said": "the stash is gone" })) };
    crate::thread_tree::git(&["stash", "pop", &rf], r).map_err(|e| format!("your changes are still in `{rf}` ({msg}): {e}"))?; let _ = std::fs::remove_file(note(root, run)); Ok(json!({ "ok": true, "restored": true }))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn a_stash_for_a_land_names_the_run_and_comes_back_whole() {
        let dir = std::env::temp_dir().join(format!("antiloki-land-{}", std::process::id())); let _ = std::fs::remove_dir_all(&dir); std::fs::create_dir_all(&dir).unwrap(); let root = dir.to_string_lossy().to_string();
        let g = |a: &[&str]| { let o = std::process::Command::new("git").args(a).current_dir(&dir).output().unwrap(); String::from_utf8_lossy(&o.stdout).to_string() };
        g(&["init", "-q", "-b", "main"]); g(&["config", "user.email", "t@t"]); g(&["config", "user.name", "t"]); std::fs::write(dir.join("a.txt"), "one\n").unwrap(); g(&["add", "-A"]); g(&["commit", "-q", "-m", "init"]);
        assert_eq!(state(&root, "r-1")["dirty"].as_array().unwrap().len(), 0); assert_eq!(stash(&root, "r-1").unwrap()["stashed"], json!(false), "nothing to set aside");
        std::fs::write(dir.join("a.txt"), "two\n").unwrap(); std::fs::write(dir.join("new.md"), "hi\n").unwrap(); g(&["add", "-N", "new.md"]);   // the half-added state that blocks a plain stash
        let s = state(&root, "r-1"); assert_eq!(s["dirty"].as_array().unwrap().len(), 2, "{s}"); assert_eq!(s["stashed"], json!(false));
        assert_eq!(stash(&root, "r-1").unwrap()["stashed"], json!(true)); assert!(state(&root, "r-1")["dirty"].as_array().unwrap().is_empty(), "the workspace is clean"); assert_eq!(state(&root, "r-1")["stashed"], json!(true)); assert!(g(&["stash", "list"]).contains("antiloki: before landing r-1"));
        assert_eq!(restore(&root, "r-2").unwrap()["restored"], json!(false), "another run's stash is not touched"); assert_eq!(g(&["stash", "list"]).lines().count(), 1);
        assert_eq!(restore(&root, "r-1").unwrap()["restored"], json!(true)); assert_eq!(std::fs::read_to_string(dir.join("a.txt")).unwrap(), "two\n"); assert_eq!(std::fs::read_to_string(dir.join("new.md")).unwrap(), "hi\n"); assert_eq!(g(&["stash", "list"]).trim(), "", "nothing left in the stash"); assert!(!state(&root, "r-1")["stashed"].as_bool().unwrap());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
