//! A TEAM'S OWN GIT (RFC 0013 §2): every team has a private repository at `.antiloki/team/<slug>/repo/` (its own git dir, never the project's) holding `team.recipe.toml`.
//! Every save of the graph is a commit, so history, "go back to version 7" and, later, improvement branches come from git itself. `main` is the live version.
//!
//!   commit_graph(root, &Graph, message) -> Some(short id) when something changed, None when the recipe is identical to the last version
//!   history(root, team)                 -> newest first: {id, ts, message, n}      recipe_at(root, team, id) -> Graph
use crate::team_graph::Graph;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

const FILE: &str = "team.recipe.toml";
const NOTES: &str = "TEAM.md";
/// The team's other configuration (the improver's settings, the approved packages), next to the recipe, in the same commits: see `team_settings`.
const SETTINGS: &str = "team.settings.toml";
const IDENT: [&str; 4] = ["-c", "user.name=antiloki", "-c", "user.email=team@antiloki.local"];

pub fn repo_dir(root: &str, team: &str) -> PathBuf { Path::new(root).join(".antiloki").join("team").join(crate::cowork_threads::slug(team)).join("repo") }
fn git(dir: &Path, args: &[&str]) -> Result<String, String> { let mut a: Vec<&str> = IDENT.to_vec(); a.extend_from_slice(args); crate::thread_tree::git(&a, dir) }
fn ensure(dir: &Path) -> Result<(), String> {
    if dir.join(".git").exists() { return Ok(()); }
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    git(dir, &["init", "-q", "-b", "main"]).map(|_| ())
}
static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
fn lock() -> std::sync::MutexGuard<'static, ()> { LOCK.lock().unwrap_or_else(|p| p.into_inner()) }
/// Write the recipe and TEAM.md of a graph into the working folder (the traps of the earlier TEAM.md are kept) and stage them.
fn write_files(dir: &Path, g: &Graph) -> Result<(), String> {
    std::fs::write(dir.join(FILE), crate::team_recipe::to_recipe(g)?).map_err(|e| e.to_string())?;
    let notes = dir.join(NOTES); let traps = std::fs::read_to_string(&notes).map(|t| crate::team_notes::traps_of(&t)).unwrap_or_default();
    std::fs::write(&notes, crate::team_notes::team_md(g, &traps)).map_err(|e| e.to_string())?;
    git(dir, &["add", FILE, NOTES]).map(|_| ())
}
/// The team's settings file, from where the app keeps them; staged with the recipe.
fn write_settings(dir: &Path, root: &str, team: &str) -> Result<(), String> {
    std::fs::write(dir.join(SETTINGS), crate::team_settings::render(root, team)).map_err(|e| e.to_string())?;
    // the branches this team uses are VENDORED into its repo (like a lock file): what runs is what is in the team's own git
    let src = crate::team_graph_run::team_files(root, team); let mut files = vec![SETTINGS.to_string()];
    for name in ["branches.lock.json"] { if src.join(name).is_file() { let _ = std::fs::copy(src.join(name), dir.join(name)); files.push(name.to_string()); } }
    if src.join("branches").is_dir() { let dst = dir.join("branches"); let _ = std::fs::create_dir_all(&dst); for e in std::fs::read_dir(src.join("branches")).into_iter().flatten().flatten() { let _ = std::fs::copy(e.path(), dst.join(e.file_name())); } files.push("branches".to_string()); }
    let mut args = vec!["add"]; args.extend(files.iter().map(|f| f.as_str())); git(dir, &args).map(|_| ())
}
/// Write the graph's recipe into the team's repository and commit it. None = nothing changed.
pub fn commit_graph(root: &str, g: &Graph, message: &str) -> Result<Option<String>, String> {
    let _l = lock(); let dir = repo_dir(root, &g.team); ensure(&dir)?;
    write_files(&dir, g)?; write_settings(&dir, root, &g.team)?;
    if git(&dir, &["status", "--porcelain", FILE, NOTES, SETTINGS, "branches", "branches.lock.json"])?.trim().is_empty() { return Ok(None); }
    git(&dir, &["commit", "-q", "-m", if message.is_empty() { "save" } else { message }])?;
    Ok(Some(git(&dir, &["rev-parse", "--short", "HEAD"])?.trim().to_string()))
}
/// A branch off `main` holding the graph as a candidate version (an improvement request). `main` stays checked out and untouched. Returns the commit.
pub fn open_branch(root: &str, g: &Graph, branch: &str, message: &str) -> Result<String, String> {
    let _l = lock(); let dir = repo_dir(root, &g.team); if !dir.join(".git").exists() { return Err("this team has no versions yet".into()); }
    git(&dir, &["checkout", "-q", "-b", branch, "main"])?;
    let r = write_files(&dir, g).and_then(|_| git(&dir, &["commit", "-q", "--allow-empty", "-m", message])).and_then(|_| git(&dir, &["rev-parse", "--short", "HEAD"]).map(|t| t.trim().to_string()));
    let back = git(&dir, &["checkout", "-q", "-f", "main"]); r.and_then(|id| back.map(|_| id))
}
/// Make `main` the branch, when main has not moved since it was cut (fast-forward only). Returns the new head.
pub fn merge_branch(root: &str, team: &str, branch: &str) -> Result<String, String> {
    let _l = lock(); let dir = repo_dir(root, team);
    git(&dir, &["merge", "-q", "--ff-only", branch]).map_err(|_| "the team changed after this request was made: ask for another try".to_string())?;
    git(&dir, &["rev-parse", "--short", "HEAD"]).map(|t| t.trim().to_string())
}
pub fn rename_branch(root: &str, team: &str, from: &str, to: &str) -> Result<(), String> { let _l = lock(); git(&repo_dir(root, team), &["branch", "-m", from, to]).map(|_| ()) }
pub fn head(root: &str, team: &str) -> String { git(&repo_dir(root, team), &["rev-parse", "--short", "main"]).map(|t| t.trim().to_string()).unwrap_or_default() }
/// The recipe on a branch (the candidate of a request).
pub fn recipe_on(root: &str, team: &str, branch: &str) -> Result<Graph, String> { crate::team_recipe::from_recipe(&git(&repo_dir(root, team), &["show", &format!("{branch}:{FILE}")]).map_err(|_| format!("no branch {branch}"))?) }
/// The branches of a team that start with `prefix` (`thread/`), with the short head of each.
pub fn branches(root: &str, team: &str, prefix: &str) -> Vec<(String, String)> {
    let dir = repo_dir(root, team); if !dir.join(".git").exists() { return vec![]; }
    git(&dir, &["branch", "--list", &format!("{prefix}*"), "--format=%(refname:short)\t%(objectname:short)"]).unwrap_or_default().lines().filter_map(|l| l.split_once('\t').map(|(a, b)| (a.to_string(), b.to_string()))).collect()
}
/// Save the graph as a new version ON a branch (a thread). `main` stays checked out. None = nothing changed.
pub fn commit_on(root: &str, g: &Graph, branch: &str, message: &str) -> Result<Option<String>, String> {
    let _l = lock(); let dir = repo_dir(root, &g.team); if !dir.join(".git").exists() { return Err("this team has no versions yet".into()); }
    git(&dir, &["checkout", "-q", branch]).map_err(|_| format!("no branch {branch}"))?;
    let r = write_files(&dir, g).and_then(|_| if git(&dir, &["status", "--porcelain", FILE, NOTES])?.trim().is_empty() { Ok(None) } else { git(&dir, &["commit", "-q", "-m", message])?; Ok(Some(git(&dir, &["rev-parse", "--short", "HEAD"])?.trim().to_string())) });
    let back = git(&dir, &["checkout", "-q", "-f", "main"]); r.and_then(|id| back.map(|_| id))
}
/// The versions of a team, newest first; `n` counts from 1 at the oldest.
pub fn history(root: &str, team: &str) -> Vec<Value> {
    let dir = repo_dir(root, team); if !dir.join(".git").exists() { return vec![]; }
    // subject AND body (the reason, who, the evidence run, what the change cost): fields split by 0x1f, records by 0x1e
    let out = git(&dir, &["log", "--format=%h%x1f%ct%x1f%s%x1f%b%x1e"]).unwrap_or_default();
    let rows: Vec<&str> = out.split('\u{1e}').map(|l| l.trim_matches('\n')).filter(|l| !l.trim().is_empty()).collect(); let total = rows.len();
    rows.iter().enumerate().map(|(i, l)| { let p: Vec<&str> = l.splitn(4, '\u{1f}').collect(); json!({ "id": p.first().unwrap_or(&""), "ts": p.get(1).and_then(|t| t.parse::<i64>().ok()).unwrap_or(0), "message": p.get(2).unwrap_or(&""), "body": p.get(3).map(|b| b.trim()).unwrap_or(""), "n": total - i }) }).collect()
}
/// A change of the team's SETTINGS (the improver's mode, an approved package) is a version too, with its own reason. None = nothing changed.
pub fn commit_settings(root: &str, team: &str, message: &str) -> Result<Option<String>, String> {
    let g = crate::team_graph_run::load_graph(root, team)?; commit_graph(root, &g, message)
}
/// The settings file as it was at a version (empty id = the head), checked against its own hash.
pub fn settings_at(root: &str, team: &str, id: &str) -> Result<Value, String> {
    if !id.is_empty() && !id.chars().all(|c| c.is_ascii_hexdigit()) { return Err("that is not a version id".into()); }
    let dir = repo_dir(root, team); if !dir.join(".git").exists() { return Err("this team has no versions yet".into()); }
    crate::team_settings::from_text(&git(&dir, &["show", &format!("{}:{SETTINGS}", if id.is_empty() { "HEAD" } else { id })]).map_err(|_| "no settings at that version (the team was saved before settings were versioned)".to_string())?)
}
/// The graph as it was at a version.
pub fn recipe_at(root: &str, team: &str, id: &str) -> Result<Graph, String> {
    if id.is_empty() || !id.chars().all(|c| c.is_ascii_hexdigit()) { return Err("that is not a version id".into()); }
    let dir = repo_dir(root, team); if !dir.join(".git").exists() { return Err("this team has no versions yet".into()); }
    crate::team_recipe::from_recipe(&git(&dir, &["show", &format!("{id}:{FILE}")]).map_err(|_| format!("no version {id}"))?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::team_graph::template;
    fn tmp(n: &str) -> String { let p = std::env::temp_dir().join(format!("antiloki-repo-{n}-{}", std::process::id())); let _ = std::fs::remove_dir_all(&p); std::fs::create_dir_all(&p).unwrap(); p.to_string_lossy().to_string() }
    #[test] fn a_settings_change_is_a_version_of_its_own_and_an_old_version_still_shows_the_old_settings() {
        use crate::team_improve::{save_config, Config, Mode};
        let root = tmp("s"); let g = template("blank", "Set", 2.0).unwrap();
        let mut c = Config::default(); c.mode = Mode::Suggest; save_config(&root, "Set", &c).unwrap();
        let v1 = commit_graph(&root, &g, "first").unwrap().unwrap();
        assert_eq!(settings_at(&root, "Set", &v1).unwrap()["improver"]["mode"], json!("suggest"));
        assert_eq!(commit_graph(&root, &g, "again").unwrap(), None, "nothing changed: no version");
        c.mode = Mode::AutoLowRisk; save_config(&root, "Set", &c).unwrap();
        let v2 = commit_graph(&root, &g, "settings: improver mode=auto_low_risk\n\nby: person").unwrap().expect("the same graph with other settings is a new version");
        assert_ne!(v1, v2);
        assert_eq!(settings_at(&root, "Set", &v2).unwrap()["improver"]["mode"], json!("auto_low_risk"));
        assert_eq!(settings_at(&root, "Set", &v1).unwrap()["improver"]["mode"], json!("suggest"), "the old version keeps the old settings");
        assert_eq!(settings_at(&root, "Set", "").unwrap()["improver"]["mode"], json!("auto_low_risk"), "the head");
        assert_eq!(recipe_at(&root, "Set", &v1).unwrap().budget_usd, recipe_at(&root, "Set", &v2).unwrap().budget_usd, "the graph did not move");
        let h = history(&root, "Set"); assert!(h[0]["body"].as_str().unwrap().contains("by: person"));
    }
    #[test] fn a_version_keeps_its_reason_and_the_head_names_the_recipe_a_run_will_use() {
        let root = tmp("b"); let mut g = template("blank", "Why", 2.0).unwrap();
        assert_eq!(head(&root, "Why"), "", "no versions yet");
        let v1 = commit_graph(&root, &g, "save").unwrap().unwrap(); assert_eq!(head(&root, "Why"), v1);
        g.budget_usd = 3.0; let v2 = commit_graph(&root, &g, "improve: the plan ignored the contract\n\nreason: the plan ignored the contract\nby: leader\nevidence-run: r-1\nimprovement-cost-usd: 0.0326").unwrap().unwrap();
        let h = history(&root, "Why"); assert_eq!(h[0]["id"], json!(v2)); assert_eq!(h[0]["message"], json!("improve: the plan ignored the contract"));
        assert!(h[0]["body"].as_str().unwrap().contains("evidence-run: r-1") && h[0]["body"].as_str().unwrap().contains("improvement-cost-usd: 0.0326"), "the body keeps the numbers");
        assert_eq!(h[1]["body"], json!(""), "a plain save has no body");
    }
    #[test] fn each_save_that_changes_the_recipe_is_a_version_and_an_old_one_comes_back() {
        let root = tmp("a"); let mut g = template("blank", "Demo", 2.0).unwrap();
        let v1 = commit_graph(&root, &g, "first").unwrap().expect("the first save is a version");
        assert_eq!(commit_graph(&root, &g, "again").unwrap(), None, "an identical recipe is not a new version");
        g.budget_usd = 5.0; let v2 = commit_graph(&root, &g, "more budget").unwrap().expect("a change is a version"); assert_ne!(v1, v2);
        let h = history(&root, "Demo"); assert_eq!(h.len(), 2); assert_eq!(h[0]["message"], json!("more budget")); assert_eq!(h[0]["n"], json!(2)); assert_eq!(h[1]["n"], json!(1));
        assert_eq!(recipe_at(&root, "Demo", &v1).unwrap().budget_usd, 2.0); assert_eq!(recipe_at(&root, "Demo", &v2).unwrap().budget_usd, 5.0);
        assert!(recipe_at(&root, "Demo", "zz").is_err() && recipe_at(&root, "Nope", &v1).is_err());
        assert!(repo_dir(&root, "Demo").join(".git").exists(), "the team has its own git dir");
        let md = std::fs::read_to_string(repo_dir(&root, "Demo").join("TEAM.md")).unwrap(); assert!(md.contains("# Demo") && md.contains("$5.00"), "TEAM.md follows the graph");
    }
}
