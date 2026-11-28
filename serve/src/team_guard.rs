//! THE GATE THAT KEEPS A TEAM'S PLANNER AND QA FROM EDITING CODE (operator 2026-11-10: "the planner and QA don't edit or change the code, just the worker does ... I just tested a planner that did change the code and ignored the worker ...
//! gates, deterministic, to see diffs violations and rollback in case of jail break").
//!
//! Two layers. PREVENTION: a read-only marker in the member's box (`.antiloki/readonly`) makes the engine's own tool hook refuse every write (agent/src/hook.rs). DETECTION AND ROLLBACK, for what prevention cannot see (an engine with no hooks,
//! a command the hook did not read as a write): the box's state when the member was linked is its BASELINE (the commit it stood on, and whatever uncommitted work it already had); anything else is a violation, and the box is put back to
//! the baseline - the commit, the files, the untracked ones - with the person's own earlier work kept. Every violation is recorded, said in the saga, and told to the agent that made it.
use crate::team_run::{fingerprint, git, patch_of, NOT_WORK};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

fn dot(dir: &Path) -> PathBuf { dir.join(".antiloki") }
pub fn marker_path(dir: &Path) -> PathBuf { dot(dir).join("readonly") }
fn meta_path(dir: &Path) -> PathBuf { dot(dir).join("team-baseline.json") }
fn patch_path(dir: &Path) -> PathBuf { dot(dir).join("team-baseline.patch") }

#[derive(Clone, Debug, PartialEq)]
pub struct Baseline { pub head: String, pub fp: String }

pub fn load_baseline(dir: &Path) -> Option<Baseline> {
    let v: Value = serde_json::from_str(&std::fs::read_to_string(meta_path(dir)).ok()?).ok()?;
    Some(Baseline { head: v["head"].as_str()?.to_string(), fp: v["fp"].as_str().unwrap_or("").to_string() })
}

/// Hold a box read-only: write the marker the hook reads and take the baseline (the commit it stands on, the work it already has). Called when a pane is linked as a planner or a QA.
pub fn hold(dir: &Path, role: &str, team: &str) -> Result<Baseline, String> {
    std::fs::create_dir_all(dot(dir)).map_err(|e| e.to_string())?;
    let head = git(dir, &["rev-parse", "HEAD"])?.trim().to_string();
    let patch = patch_of(dir)?; let fp = fingerprint(&patch);
    std::fs::write(patch_path(dir), &patch).map_err(|e| e.to_string())?;
    std::fs::write(meta_path(dir), json!({ "head": head, "fp": fp }).to_string()).map_err(|e| e.to_string())?;
    std::fs::write(marker_path(dir), json!({ "role": role, "team": team }).to_string()).map_err(|e| e.to_string())?;
    Ok(Baseline { head, fp })
}
/// Let a box write again (the pane left the team). Its baseline files go too.
pub fn release(dir: &Path) { for p in [marker_path(dir), meta_path(dir), patch_path(dir)] { let _ = std::fs::remove_file(p); } }
/// The role a held box is held to (`planner`, `qa`, `side`), or None when it is not held.
pub fn role_of(dir: &Path) -> Option<String> { std::fs::read_to_string(marker_path(dir)).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()).and_then(|v| v["role"].as_str().map(String::from)) }
pub fn is_held(dir: &Path) -> bool { marker_path(dir).exists() }

/// What a read-only box has done that it may not: the files it changed, and whether it moved its commit.
#[derive(Clone, Debug, PartialEq)]
pub struct Violation { pub files: Vec<String>, pub moved_head: bool }

/// The files a patch touches, from its `diff --git a/x b/x` lines. Pure.
pub fn files_of(patch: &str) -> Vec<String> {
    let mut v: Vec<String> = patch.lines().filter_map(|l| l.strip_prefix("diff --git a/")).filter_map(|rest| rest.split(" b/").next().map(String::from)).collect(); v.sort(); v.dedup(); v
}

/// A patch by file: each `diff --git a/x b/x` section keyed by its path. Pure.
pub fn sections(patch: &str) -> std::collections::BTreeMap<String, String> {
    let mut out = std::collections::BTreeMap::new(); let mut cur: Option<String> = None; let mut buf = String::new();
    for line in patch.lines() {
        if let Some(rest) = line.strip_prefix("diff --git a/") { if let Some(c) = cur.take() { out.insert(c, std::mem::take(&mut buf)); } cur = rest.split(" b/").next().map(String::from); buf.clear(); }
        buf.push_str(line); buf.push('\n');
    }
    if let Some(c) = cur { out.insert(c, buf); } out
}
/// The files whose change differs between two patches (changed, added or dropped): what a box did BEYOND its baseline. Pure.
pub fn changed_files(base: &str, now: &str) -> Vec<String> {
    let (a, b) = (sections(base), sections(now)); let mut v: Vec<String> = a.keys().chain(b.keys()).filter(|k| a.get(*k) != b.get(*k)).cloned().collect(); v.sort(); v.dedup(); v
}

/// Compare a held box with its baseline. `None` when it is as it was.
pub fn check(dir: &Path) -> Result<Option<Violation>, String> {
    let Some(base) = load_baseline(dir) else { return Ok(None) };
    let head = git(dir, &["rev-parse", "HEAD"])?.trim().to_string();
    let patch = patch_of(dir)?; let moved = head != base.head;
    if !moved && fingerprint(&patch) == base.fp { return Ok(None); }
    let base_patch = std::fs::read_to_string(patch_path(dir)).unwrap_or_default();
    let mut files = changed_files(&base_patch, &patch);
    if moved { if let Ok(names) = git(dir, &["diff", "--name-only", &format!("{}..HEAD", base.head)]) { files.extend(names.lines().map(String::from)); files.sort(); files.dedup(); } }
    Ok(Some(Violation { files, moved_head: moved }))
}

/// Put a held box back to its baseline: the commit it stood on, the tracked files, no stray new ones, and the work it already had when it was linked.
pub fn restore(dir: &Path) -> Result<(), String> {
    let base = load_baseline(dir).ok_or("this box has no baseline to go back to")?;
    git(dir, &["reset", "--hard", "-q", &base.head])?;
    let mut clean: Vec<String> = vec!["clean".into(), "-fdq".into()]; for n in NOT_WORK { clean.push("-e".into()); clean.push(n.into()); }
    git(dir, &clean.iter().map(String::as_str).collect::<Vec<_>>())?;
    let patch = std::fs::read_to_string(patch_path(dir)).unwrap_or_default();
    if !patch.trim().is_empty() {
        let mut child = antiloki_rs::proc::cmd("git").args(["apply", "--whitespace=nowarn", "-"]).current_dir(dir).stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::piped()).spawn().map_err(|e| e.to_string())?;
        use std::io::Write; child.stdin.take().ok_or("no stdin")?.write_all(patch.as_bytes()).map_err(|e| e.to_string())?;
        let o = child.wait_with_output().map_err(|e| e.to_string())?; if !o.status.success() { return Err(format!("the work the box had when it was linked could not be put back: {}", String::from_utf8_lossy(&o.stderr).trim())); }
    }
    Ok(())
}

/// What the offender is told. It starts with `[team]` so the team's watcher never takes it for the person talking. Pure.
pub fn notice(role: &str, v: &Violation) -> String {
    if role == "side" { return format!("[side] Your changes were REVERTED: you are a side-question helper and you never change code. {}{} Answer in words, and say what should change so the person can tell the agent you are helping.", if v.files.is_empty() { "A change to the box".to_string() } else { format!("Reverted: {}.", v.files.iter().take(6).cloned().collect::<Vec<_>>().join(", ")) }, if v.moved_head { " It also moved the commit, which was put back." } else { "" }); }
    format!("[team] Your changes were REVERTED: you are the team's {role}, and a {role} never edits code (the worker does). {}{} Put what you want changed in your reply instead — the plan for the planner, the verdict for the QA.",
        if v.files.is_empty() { "A change to the box".to_string() } else { format!("Reverted: {}.", v.files.iter().take(6).cloned().collect::<Vec<_>>().join(", ")) }, if v.moved_head { " It also moved the commit, which was put back." } else { "" })
}

pub fn log_path(root: &str) -> PathBuf { Path::new(root).join(".antiloki").join("team-guard.json") }
pub fn read_log(root: &str) -> Vec<Value> { std::fs::read_to_string(log_path(root)).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default() }
/// Record a violation (newest last, the last 200 kept).
pub fn append_log(root: &str, entry: Value) { let mut l = read_log(root); l.push(entry); let n = l.len(); if n > 200 { l.drain(0..n - 200); } let p = log_path(root); if let Some(d) = p.parent() { let _ = std::fs::create_dir_all(d); } let _ = std::fs::write(p, serde_json::to_string_pretty(&l).unwrap_or_default()); }

#[cfg(test)]
mod tests {
    use super::*;

    fn repo(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("antiloki-guard-{tag}-{}", std::process::id())); let _ = std::fs::remove_dir_all(&d); std::fs::create_dir_all(&d).unwrap();
        let g = |a: &[&str]| { let o = std::process::Command::new("git").args(["-c", "user.email=e@x", "-c", "user.name=e"]).args(a).current_dir(&d).output().unwrap(); assert!(o.status.success(), "{a:?}: {}", String::from_utf8_lossy(&o.stderr)); };
        g(&["init", "-q"]); std::fs::write(d.join("a.txt"), "one\n").unwrap(); std::fs::write(d.join("b.txt"), "bee\n").unwrap(); g(&["add", "-A"]); g(&["commit", "-q", "-m", "init"]); d
    }
    fn git_ok(d: &Path, a: &[&str]) { let o = std::process::Command::new("git").args(["-c", "user.email=e@x", "-c", "user.name=e"]).args(a).current_dir(d).output().unwrap(); assert!(o.status.success(), "{a:?}: {}", String::from_utf8_lossy(&o.stderr)); }

    #[test]
    fn a_held_box_that_is_left_alone_is_clean_and_one_that_is_written_to_is_caught_and_put_back() {
        let d = repo("a"); let b = hold(&d, "planner", "k").unwrap(); assert!(is_held(&d) && marker_path(&d).exists() && b.fp.is_empty());
        assert_eq!(check(&d).unwrap(), None, "nothing happened");
        std::fs::write(d.join(".antiloki/prompts.jsonl"), "x").unwrap(); std::fs::write(d.join("CLAUDE.md"), "brief").unwrap(); assert_eq!(check(&d).unwrap(), None, "the agent's own files are not code");
        // a planner edits a file, adds one, and deletes another
        std::fs::write(d.join("a.txt"), "CHANGED\n").unwrap(); std::fs::write(d.join("new.rs"), "fn x() {}\n").unwrap(); std::fs::remove_file(d.join("b.txt")).unwrap();
        let v = check(&d).unwrap().unwrap(); assert_eq!(v.files, vec!["a.txt", "b.txt", "new.rs"]); assert!(!v.moved_head);
        restore(&d).unwrap();
        assert_eq!(std::fs::read_to_string(d.join("a.txt")).unwrap(), "one\n"); assert_eq!(std::fs::read_to_string(d.join("b.txt")).unwrap(), "bee\n"); assert!(!d.join("new.rs").exists(), "the stray file is gone");
        assert_eq!(check(&d).unwrap(), None, "back to the baseline"); assert!(d.join(".antiloki/prompts.jsonl").exists(), "the agent's own notes were not touched");
        release(&d); assert!(!is_held(&d) && load_baseline(&d).is_none()); let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_commit_is_a_violation_too_and_the_commit_is_undone() {
        let d = repo("b"); let b = hold(&d, "qa", "k").unwrap();
        std::fs::write(d.join("a.txt"), "sneaky\n").unwrap(); git_ok(&d, &["add", "-A"]); git_ok(&d, &["commit", "-q", "-m", "jailbreak"]);
        let v = check(&d).unwrap().unwrap(); assert!(v.moved_head && v.files.contains(&"a.txt".to_string()), "{v:?}");
        restore(&d).unwrap(); assert_eq!(git(&d, &["rev-parse", "HEAD"]).unwrap().trim(), b.head); assert_eq!(std::fs::read_to_string(d.join("a.txt")).unwrap(), "one\n"); assert_eq!(check(&d).unwrap(), None); let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn the_work_a_box_already_had_when_it_was_linked_is_its_baseline_and_survives_a_rollback() {
        let d = repo("c"); std::fs::write(d.join("a.txt"), "the person's own edit\n").unwrap(); std::fs::write(d.join("mine.txt"), "mine\n").unwrap();
        hold(&d, "planner", "k").unwrap(); assert_eq!(check(&d).unwrap(), None, "what was there is not a violation");
        std::fs::write(d.join("a.txt"), "the planner's edit\n").unwrap(); let v = check(&d).unwrap().unwrap(); assert_eq!(v.files, vec!["a.txt"]);
        restore(&d).unwrap(); assert_eq!(std::fs::read_to_string(d.join("a.txt")).unwrap(), "the person's own edit\n", "the person's work is kept"); assert_eq!(std::fs::read_to_string(d.join("mine.txt")).unwrap(), "mine\n"); assert_eq!(check(&d).unwrap(), None); let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn the_offender_is_told_what_was_reverted_and_where_to_say_it_instead() {
        let n = notice("planner", &Violation { files: vec!["a.txt".into(), "b.txt".into()], moved_head: true }); assert!(n.starts_with("[team] Your changes were REVERTED") && n.contains("a.txt, b.txt") && n.contains("moved the commit") && n.contains("the plan for the planner"), "{n}");
        assert!(notice("qa", &Violation { files: vec![], moved_head: false }).contains("the verdict for the QA"));
        assert_eq!(files_of("diff --git a/x/y.rs b/x/y.rs\n+1\ndiff --git a/z b/z\n"), vec!["x/y.rs", "z"]);
        assert_eq!(changed_files("diff --git a/m b/m\n+1\ndiff --git a/a b/a\n+x\n", "diff --git a/m b/m\n+1\ndiff --git a/a b/a\n+y\ndiff --git a/n b/n\n+2\n"), vec!["a", "n"], "what differs from the baseline, not the baseline itself");
    }

    #[test]
    fn the_log_keeps_the_last_violations() {
        let d = std::env::temp_dir().join(format!("antiloki-guard-log-{}", std::process::id())); let _ = std::fs::remove_dir_all(&d); let root = d.to_string_lossy().to_string();
        assert!(read_log(&root).is_empty()); for i in 0..205 { append_log(&root, json!({ "n": i })); } let l = read_log(&root); assert_eq!((l.len(), l[0]["n"].clone(), l[199]["n"].clone()), (200, json!(5), json!(204))); let _ = std::fs::remove_dir_all(&d);
    }
}
