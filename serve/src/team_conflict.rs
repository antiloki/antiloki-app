//! THE CONFLICT RADAR (UI-15, operator: "we use worktrees, but for sure we should have a deterministic way to check conflicts in real time"): which parallel worktrees touch the same files, and the same lines, before anything is joined.
//! Pure core (`overlaps`) over what each tree changed; `scan` reads it from git (committed and uncommitted work against the merge-base with the main tree). No model, nothing is written.
use serde::Serialize;
use std::collections::BTreeMap;

/// One tree's changes: for each file, the changed line ranges in the NEW file (1-based, inclusive). An empty list means the whole file changed (created, deleted or binary).
#[derive(Clone, Debug, Default)]
pub struct Unit { pub name: String, pub files: BTreeMap<String, Vec<(u32, u32)>> }
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Conflict { pub a: String, pub b: String, pub path: String, /** the lines both changed overlap (a likely merge conflict); false = same file, different places */ pub same_lines: bool }

fn meet(x: &[(u32, u32)], y: &[(u32, u32)]) -> bool { x.is_empty() || y.is_empty() || x.iter().any(|(a, b)| y.iter().any(|(c, d)| a <= d && c <= b)) }
/// Every pair of trees that changed the same file. Sorted by path, lines-overlapping first. Pure.
pub fn overlaps(units: &[Unit]) -> Vec<Conflict> {
    let mut out = vec![];
    for (i, u) in units.iter().enumerate() { for v in &units[i + 1..] { for (p, r) in &u.files { if let Some(r2) = v.files.get(p) { out.push(Conflict { a: u.name.clone(), b: v.name.clone(), path: p.clone(), same_lines: meet(r, r2) }); } } } }
    out.sort_by(|x, y| y.same_lines.cmp(&x.same_lines).then(x.path.cmp(&y.path))); out
}
/// The changed line ranges in a `git diff -U0` text: `@@ -a,b +c,d @@` gives c..c+d-1 (d missing = 1; d = 0 is a pure deletion, kept as the line it sits at). Per file. Pure.
pub fn parse_diff(text: &str) -> BTreeMap<String, Vec<(u32, u32)>> {
    let mut m: BTreeMap<String, Vec<(u32, u32)>> = BTreeMap::new(); let mut cur: Option<String> = None;
    for l in text.lines() {
        if let Some(p) = l.strip_prefix("+++ ") { cur = p.strip_prefix("b/").map(String::from); if let Some(c) = &cur { m.entry(c.clone()).or_default(); } else if p == "/dev/null" { cur = None; } }
        else if let Some(p) = l.strip_prefix("--- ") { if let (Some(f), true) = (p.strip_prefix("a/"), true) { m.entry(f.to_string()).or_default(); cur = Some(f.to_string()); } }
        else if l.starts_with("@@") { if let (Some(f), Some(plus)) = (&cur, l.split_whitespace().find(|t| t.starts_with('+'))) { let t = plus.trim_start_matches('+'); let (s, n) = t.split_once(',').map_or((t, "1"), |(a, b)| (a, b)); if let (Ok(s), Ok(n)) = (s.parse::<u32>(), n.parse::<u32>()) { m.entry(f.clone()).or_default().push((s, (s + n.max(1) - 1).max(s))); } } }
    }
    m
}
/// The trees of a repository (its worktrees other than the main one) and what each changed against `base`. A tree that cannot be read is left out.
pub fn scan(repo: &str, base: &str) -> Result<Vec<Unit>, String> {
    let list = crate::boxes::git(&["worktree", "list", "--porcelain"], repo)?; let mut units = vec![]; let mut path = String::new(); let mut branch = String::new();
    let mut flush = |p: &str, b: &str, units: &mut Vec<Unit>| { if p.is_empty() || p == repo || b.is_empty() { return; }
        let mb = crate::boxes::git(&["merge-base", base, b], repo).map(|t| t.trim().to_string()).unwrap_or_default(); if mb.is_empty() { return; }
        // committed work on the branch since the merge-base, plus what is not committed yet in its tree
        let mut files = parse_diff(&crate::boxes::git(&["diff", "-U0", "--no-color", &format!("{mb}..{b}")], repo).unwrap_or_default());
        for (f, r) in parse_diff(&crate::boxes::git(&["diff", "-U0", "--no-color", "HEAD"], p).unwrap_or_default()) { files.entry(f).or_default().extend(r); }
        if !files.is_empty() { units.push(Unit { name: b.trim_start_matches("refs/heads/").to_string(), files }); } };
    for l in list.lines().chain(std::iter::once("")) { if let Some(p) = l.strip_prefix("worktree ") { path = p.to_string(); branch.clear(); } else if let Some(b) = l.strip_prefix("branch ") { branch = b.to_string(); } else if l.is_empty() { flush(&path, &branch, &mut units); path.clear(); branch.clear(); } }
    Ok(units)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn u(n: &str, f: &[(&str, &[(u32, u32)])]) -> Unit { Unit { name: n.into(), files: f.iter().map(|(p, r)| (p.to_string(), r.to_vec())).collect() } }
    #[test] fn two_trees_on_the_same_file_conflict_when_their_lines_meet_and_only_warn_when_they_do_not() {
        let c = overlaps(&[u("a", &[("x.rs", &[(10, 20)]), ("y.rs", &[(1, 2)])]), u("b", &[("x.rs", &[(15, 16)]), ("y.rs", &[(50, 60)]), ("z.rs", &[])]), u("c", &[("z.rs", &[(5, 5)])])]);
        assert_eq!(c.len(), 3, "x.rs and y.rs between a and b, z.rs between b and c");
        assert!(c[0].same_lines && c[0].path == "x.rs"); assert!(!c.iter().find(|k| k.path == "y.rs").unwrap().same_lines);
        assert!(c.iter().find(|k| k.path == "z.rs").unwrap().same_lines, "a whole-file change meets any line");
        assert!(overlaps(&[u("a", &[("p", &[(1, 1)])]), u("b", &[("q", &[(1, 1)])])]).is_empty());
    }
    #[test] fn a_zero_context_diff_gives_the_changed_lines_of_each_file() {
        let d = "diff --git a/s.rs b/s.rs\n--- a/s.rs\n+++ b/s.rs\n@@ -3,2 +3,4 @@ fn\n+x\n@@ -20 +22 @@\n+y\ndiff --git a/n.txt b/n.txt\n--- /dev/null\n+++ b/n.txt\n@@ -0,0 +1,3 @@\n";
        let m = parse_diff(d); assert_eq!(m["s.rs"], vec![(3, 6), (22, 22)]); assert_eq!(m["n.txt"], vec![(1, 3)]);
    }
}
