//! GIT NODES (BL-20): a team works with its own branch and worktrees as steps, no model: `use_tool` type `git`, the tool name is the operation. Everything happens inside the team's OWN thread tree and its units (the branches
//! `<team branch>--<name>` the saga runner already makes); a node can never push, fetch, check out the workspace's branch, rewrite history or run an arbitrary git command: the operations are a closed list, the arguments are
//! checked, and what lands in the person's workspace stays the person's merge gate.
use serde_json::{json, Value};
use std::path::Path;
use crate::thread_tree as tt;

pub const OPS: [&str; 8] = ["status", "diff", "log", "branch", "worktree_add", "worktree_remove", "commit", "merge"];
const MAX_TEXT: usize = 20_000;
/// What an operation does to the world: reading, or changing the team's own branches and trees. Pure.
pub fn risk(op: &str) -> &'static str { if ["status", "diff", "log"].contains(&op) { "read" } else { "write" } }
/// A unit/branch name from the node's args: letters, digits, `-` and `_`, 1 to 40 characters. Pure.
pub fn clean_name(s: &str) -> Result<String, String> {
    let s = s.trim(); if s.is_empty() || s.len() > 40 || !s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') { return Err(format!("`{s}` is not a name: letters, digits, - and _ (at most 40)")); } Ok(s.to_string())
}
/// A commit message: one to three lines, no leading dash (git would read it as an option), at most 300 characters. Pure.
pub fn clean_message(s: &str) -> Result<String, String> {
    let m: String = s.trim().chars().take(300).collect(); if m.is_empty() { return Err("a commit needs a message".into()); } if m.starts_with('-') { return Err("a commit message does not start with a dash".into()); } Ok(m)
}
fn cap(s: String) -> String { s.chars().take(MAX_TEXT).collect() }

/// Run one operation for `team` in `root`. `args`: `unit` (work in that unit's tree instead of the team's), `name` (branch/unit), `message`, `n` (log length), `against` (diff). Blocking: call it off the async runtime.
pub fn run(root: &str, team: &str, op: &str, args: &Value) -> Result<Value, String> {
    let thread = crate::team_graph::slug(team);
    let (tree, branch) = tt::ensure(root, &thread)?;
    // the tree an operation works in: the team's own, or one of its units
    let unit = args["unit"].as_str().filter(|u| !u.trim().is_empty()).map(clean_name).transpose()?;
    let (cwd, cur) = match &unit { Some(u) => tt::ensure_unit(root, &thread, u)?, None => (tree.clone(), branch.clone()) };
    let say = |text: String, extra: Value| { let mut v = json!({ "ok": true, "op": op, "branch": cur, "text": cap(text) }); if let Value::Object(e) = extra { for (k, x) in e { v[k] = x; } } v };
    match op {
        "status" => { let t = tt::git(&["status", "--porcelain=v1", "-b"], &cwd)?; let changed = t.lines().skip(1).count(); Ok(say(t, json!({ "changed": changed, "clean": changed == 0 }))) }
        "diff" => {
            let t = if args["against"].as_str() == Some("workspace") { let base = tt::head(Path::new(root))?; tt::git(&["diff", "--stat", "--patch", &format!("{base}...HEAD")], &cwd)? } else { tt::git(&["diff", "--stat", "--patch", "HEAD"], &cwd)? };
            Ok(say(t.clone(), json!({ "empty": t.trim().is_empty() })))
        }
        "log" => { let n = args["n"].as_u64().unwrap_or(10).clamp(1, 50).to_string(); Ok(say(tt::git(&["log", "--oneline", "-n", &n], &cwd)?, json!({}))) }
        "branch" => { let name = clean_name(args["name"].as_str().unwrap_or(""))?; let b = format!("{branch}--{name}"); tt::git(&["branch", &b, &cur], &cwd)?; Ok(say(format!("branch {b} made from {cur}"), json!({ "created": b }))) }
        "worktree_add" => { let name = clean_name(args["name"].as_str().unwrap_or(""))?; let (dir, b) = tt::ensure_unit(root, &thread, &name)?; Ok(say(format!("worktree for {b} at {}", dir.display()), json!({ "unit": name, "branch": b, "dir": dir.to_string_lossy() }))) }
        "worktree_remove" => { let name = clean_name(args["name"].as_str().unwrap_or(""))?; tt::discard_unit(root, &thread, &name); Ok(say(format!("worktree {name} removed"), json!({ "unit": name }))) }
        "commit" => {
            let msg = clean_message(args["message"].as_str().unwrap_or(""))?;
            if !tt::changed(&cwd) { return Ok(say("nothing to commit".into(), json!({ "committed": false }))); }
            tt::git(&["add", "-A"], &cwd)?; let mut a: Vec<&str> = tt::IDENT.to_vec(); a.extend(["commit", "-q", "-m", &msg]); tt::git(&a, &cwd)?;
            Ok(say(format!("committed on {cur}: {msg}"), json!({ "committed": true, "head": tt::head(&cwd).unwrap_or_default() })))
        }
        "merge" => {
            // a unit into the team's branch (never anything into the workspace: that is the person's gate)
            let name = clean_name(args["name"].as_str().or(args["unit"].as_str()).unwrap_or(""))?; let (dir, _) = tt::ensure_unit(root, &thread, &name)?;
            let r = tt::integrate(root, &thread, &[(name.clone(), 0, dir)])?; let ok = r["ok"] == json!(true);
            Ok(say(if ok { format!("{name} merged into {branch}") } else { format!("{name} did not merge cleanly: {}", r["conflicts"]) }, json!({ "merged": ok, "result": r })))
        }
        other => Err(format!("`{other}` is not a git operation here: {}", OPS.join(", "))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn names_messages_and_risks_are_checked_before_git_is_touched() {
        assert_eq!((risk("status"), risk("diff"), risk("log"), risk("commit"), risk("merge"), risk("branch")), ("read", "read", "read", "write", "write", "write"));
        assert_eq!(clean_name(" feat-1_a ").unwrap(), "feat-1_a"); for bad in ["", "a b", "../x", "x;rm", "-x y", &"a".repeat(41), "a/b"] { assert!(clean_name(bad).is_err(), "{bad:?}"); }
        assert_eq!(clean_message("  add the cart ").unwrap(), "add the cart"); assert!(clean_message("").is_err() && clean_message("--amend").is_err()); assert_eq!(clean_message(&"x".repeat(400)).unwrap().len(), 300);
        assert!(run("/nonexistent", "T", "push", &json!({})).is_err(), "the operations are a closed list");
    }
}
