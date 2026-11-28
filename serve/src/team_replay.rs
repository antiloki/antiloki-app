//! THE REPLAY BANK (RFC 0013 §4, step 4): small tasks with deterministic success checks, kept per team OUTSIDE the team's repository, so the improver can never edit the judge.
//! A past run can be saved as a task (the person's act). Every third task goes to the held-out part, the rest to tune: a change is admitted only on held-out tasks (`team_experiment.rs`).
use crate::team_check::{Rule, Verdict};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// A text a person has labelled by hand: it should pass this task's checks, or should not. If the checks say otherwise, the judge is wrong and nothing is admitted on it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Oracle { pub text: String, pub pass: bool }
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Task { #[serde(default)] pub oracle: Vec<Oracle>, pub id: String, pub ask: String, pub checks: Vec<Rule>, #[serde(default)] pub from_run: String, #[serde(default)] pub added_ms: i64, /** "tune" or "heldout", fixed when it is added */ pub split: String }

pub fn bank_path(root: &str, team: &str) -> PathBuf { Path::new(root).join(".antiloki").join("team").join(crate::cowork_threads::slug(team)).join("replay").join("tasks.json") }
pub fn list(root: &str, team: &str) -> Vec<Task> { std::fs::read_to_string(bank_path(root, team)).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default() }
fn save(root: &str, team: &str, v: &[Task]) -> Result<(), String> { let p = bank_path(root, team); std::fs::create_dir_all(p.parent().unwrap()).map_err(|e| e.to_string())?; std::fs::write(p, serde_json::to_string_pretty(v).unwrap_or_default()).map_err(|e| e.to_string()) }
fn fnv(s: &str) -> u64 { s.bytes().fold(0xcbf29ce484222325, |h, b| (h ^ b as u64).wrapping_mul(0x100000001b3)) }

/// Save a task. The checks are required and must be well formed: a task nothing can judge is not a task. Only the person adds tasks.
pub fn add(root: &str, team: &str, ask: &str, checks: Vec<Rule>, from_run: &str, now_ms: i64) -> Result<Task, String> {
    if ask.trim().is_empty() { return Err("a task needs the ask it is run with".into()); }
    let bad = crate::team_check::check(&checks); if !bad.is_empty() { return Err(format!("the checks are not usable: {}", bad.join("; "))); }
    let mut all = list(root, team); let id = format!("t{:x}", fnv(&format!("{ask}\n{from_run}")) & 0xffffff);
    if all.iter().any(|t| t.id == id) { return Err("that task is already in the bank".into()); }
    let split = if all.len() % 3 == 2 { "heldout" } else { "tune" }.to_string();
    let t = Task { oracle: vec![], id, ask: ask.trim().into(), checks, from_run: from_run.into(), added_ms: now_ms, split };
    all.push(t.clone()); save(root, team, &all)?; Ok(t)
}
pub fn remove(root: &str, team: &str, id: &str) -> Result<(), String> { let mut all = list(root, team); let n = all.len(); all.retain(|t| t.id != id); if all.len() == n { return Err(format!("no task {id}")); } save(root, team, &all) }
pub fn tasks(root: &str, team: &str, split: &str) -> Vec<Task> { list(root, team).into_iter().filter(|t| t.split == split).collect() }
/// Judge what a run produced against a task's checks (free, the same answer every time).
pub fn judge(t: &Task, text: &str, files: &[(String, u64)]) -> Verdict { crate::team_check::evaluate(&t.checks, text, files) }
/// Add a hand-labelled text to a task. A label the task's own checks contradict is refused at once: fix the checks, not the label.
pub fn add_oracle(root: &str, team: &str, id: &str, text: &str, pass: bool) -> Result<(), String> {
    let mut all = list(root, team); let t = all.iter_mut().find(|t| t.id == id).ok_or_else(|| format!("no task {id}"))?;
    if text.trim().is_empty() { return Err("an oracle text is not empty".into()); }
    if judge(t, text, &[]).pass != pass { return Err(format!("the task's checks say this text {} but you say it {}: the checks need fixing first", if pass { "fails" } else { "passes" }, if pass { "passes" } else { "fails" })); }
    t.oracle.push(Oracle { text: text.into(), pass }); save(root, team, &all)
}
/// The tasks whose checks contradict a label the person gave (the judge is wrong there). Empty = the judge agrees with every label.
pub fn disagreeing(tasks: &[Task]) -> Vec<String> { tasks.iter().filter(|t| t.oracle.iter().any(|o| judge(t, &o.text, &[]).pass != o.pass)).map(|t| t.id.clone()).collect() }
pub fn counts(root: &str, team: &str) -> (usize, usize) { let a = list(root, team); (a.iter().filter(|t| t.split == "tune").count(), a.iter().filter(|t| t.split == "heldout").count()) }

#[cfg(test)]
mod tests {
    use super::*;
    fn tmp(n: &str) -> String { let p = std::env::temp_dir().join(format!("antiloki-replay-{n}-{}", std::process::id())); let _ = std::fs::remove_dir_all(&p); std::fs::create_dir_all(&p).unwrap(); p.to_string_lossy().to_string() }
    fn words(max: f64) -> Vec<Rule> { vec![Rule { id: "short".into(), kind: "words".into(), max: Some(max), ..Default::default() }] }
    #[test] fn tasks_need_checks_split_one_in_three_to_heldout_and_judge_the_same_way_every_time() {
        let root = tmp("a");
        assert!(add(&root, "T", "  ", words(5.0), "", 1).is_err() && add(&root, "T", "ask", vec![], "", 1).is_err(), "no ask or no checks is no task");
        for i in 0..6 { add(&root, "T", &format!("ask {i}"), words(5.0), "run-1", i).unwrap(); }
        assert_eq!(counts(&root, "T"), (4, 2), "every third task is held out");
        assert!(add(&root, "T", "ask 0", words(5.0), "run-1", 9).is_err(), "no duplicates");
        let t = &list(&root, "T")[0]; assert!(judge(t, "one two three", &[]).pass && !judge(t, "a b c d e f g h", &[]).pass);
        assert!(!bank_path(&root, "T").starts_with(crate::team_repo::repo_dir(&root, "T")), "the bank is outside the team's repo");
        let id = t.id.clone(); assert!(add_oracle(&root, "T", &id, "one two", true).is_ok() && add_oracle(&root, "T", &id, "a b c d e f g h", false).is_ok());
        assert!(add_oracle(&root, "T", &id, "a b c d e f g h", true).is_err(), "a label the checks contradict is refused"); assert!(disagreeing(&list(&root, "T")).is_empty());
        let mut broken = list(&root, "T")[0].clone(); broken.checks = words(1.0); assert_eq!(disagreeing(&[broken]), vec![id.clone()], "checks changed after the labels: the judge now disagrees");
        remove(&root, "T", &id).unwrap(); assert!(remove(&root, "T", &id).is_err() && list(&root, "T").len() == 5);
    }
}
