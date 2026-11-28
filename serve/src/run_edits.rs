//! WHAT THE AGENTS WROTE, AS EVENTS (the Editor view of a team, 2026-11-21): every file an agent creates, reads, edits or runs a command on is one line of `<run>/edits.jsonl`, taken from the CLI's own stream as it happens: `{i, t_ms, node, kind, file, ...}`.
//! The screen replays them with human-like timing derived from the events alone, so a live run and a replay afterwards play the same. Kinds: `write` (a whole file, `text`), `edit` (`old` → `new` in a file), `read` (`file`), `run` (`cmd`). Files are paths inside the run's copy of the project.
//! Pure extraction (tested); `append` and `read_since` touch the disk. Nothing here changes a file.
use serde_json::{json, Value};
use std::path::Path;

const MAX_TEXT: usize = 200_000;
/// A path inside the run's copy (the CLI reports absolute ones): relative to `base`, with forward slashes; a path outside it is left out (None). Pure.
pub fn relative(base: &str, file: &str) -> Option<String> {
    let f = file.replace('\\', "/"); let b = base.replace('\\', "/"); let b = b.trim_end_matches('/');
    let r = if let Some(x) = f.strip_prefix(&format!("{b}/")) { x.to_string() } else if f.starts_with('/') || f.contains(':') { return None } else { f.trim_start_matches("./").to_string() };
    if r.is_empty() || r.split('/').any(|p| p == ".." || p.is_empty()) { None } else { Some(r) }
}
fn cut(s: &str) -> String { s.chars().take(MAX_TEXT).collect() }
/// The edit events one streamed stdout line holds (Claude Code's stream-json: an `assistant` event with `tool_use` items). `base` is the run's folder. Pure.
pub fn events_of_line(line: &str, base: &str) -> Vec<Value> {
    let l = line.trim(); if !l.starts_with('{') || !l.contains("tool_use") { return vec![]; } let Ok(e) = serde_json::from_str::<Value>(l) else { return vec![] };
    if e["type"] != "assistant" { return vec![]; }
    let mut out = vec![];
    for c in e["message"]["content"].as_array().into_iter().flatten().filter(|c| c["type"] == "tool_use") {
        let i = &c["input"]; let name = c["name"].as_str().unwrap_or(""); let file = i["file_path"].as_str().or_else(|| i["path"].as_str()).and_then(|f| relative(base, f));
        match name {
            "Write" => if let (Some(f), Some(t)) = (&file, i["content"].as_str()) { out.push(json!({ "kind": "write", "file": f, "text": cut(t) })); },
            "Edit" => if let (Some(f), Some(o), Some(n)) = (&file, i["old_string"].as_str(), i["new_string"].as_str()) { out.push(json!({ "kind": "edit", "file": f, "old": cut(o), "new": cut(n) })); },
            "MultiEdit" => if let Some(f) = &file { for x in i["edits"].as_array().into_iter().flatten() { if let (Some(o), Some(n)) = (x["old_string"].as_str(), x["new_string"].as_str()) { out.push(json!({ "kind": "edit", "file": f, "old": cut(o), "new": cut(n) })); } } },
            "Read" => if let Some(f) = &file { out.push(json!({ "kind": "read", "file": f })); },
            "Bash" => if let Some(cmd) = i["command"].as_str() { out.push(json!({ "kind": "run", "cmd": cmd.chars().take(300).collect::<String>() })); },
            _ => {}
        }
    }
    out
}
fn file_of(root: &str, run: &str) -> std::path::PathBuf { crate::team_graph_run::run_dir(root, run).join("edits.jsonl") }
/// Append the events of one line to the run's file, numbered in order (`i`), stamped with the time and the node. At most 4 MB of events a run; further ones are dropped (a long run keeps its beginning, the screen says so).
pub fn append(root: &str, run: &str, node: &str, base: &str, line: &str) {
    let evs = events_of_line(line, base); if evs.is_empty() { return; } use std::io::Write;
    let p = file_of(root, run); if let Some(d) = p.parent() { if !d.exists() { return; } }
    if std::fs::metadata(&p).map_or(false, |m| m.len() > 4 * 1024 * 1024) { return; }
    // where the run's files are, for `base_of` (the thread's own work tree)
    if let Some(d) = p.parent() { let w = d.join("work.json"); let want = json!({ "dir": base }).to_string(); if std::fs::read_to_string(&w).map_or(true, |t| t != want) { let _ = std::fs::write(&w, want); } }
    let mut i = std::fs::read_to_string(&p).map(|t| t.lines().count()).unwrap_or(0); let t = crate::cli::now_ms();
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&p) { for mut e in evs { e["i"] = json!(i); e["t_ms"] = json!(t); e["node"] = json!(node); i += 1; let _ = writeln!(f, "{e}"); } }
}
/// The events from number `since` on, and how many there are in all. Pure over the file's text.
pub fn since_of(text: &str, since: usize, limit: usize) -> (Vec<Value>, usize) {
    let all: Vec<&str> = text.lines().collect(); let total = all.len();
    (all.iter().skip(since).take(limit).filter_map(|l| serde_json::from_str::<Value>(l).ok()).collect(), total)
}
/// Where a run's files are (the folder its agents worked in), if any agent has written yet.
pub fn work_dir(root: &str, run: &str) -> Option<String> { std::fs::read_to_string(crate::team_graph_run::run_dir(root, run).join("work.json")).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()).and_then(|v| v["dir"].as_str().map(String::from)) }
pub fn read_since(root: &str, run: &str, since: usize, limit: usize) -> (Vec<Value>, usize) { since_of(&std::fs::read_to_string(file_of(root, run)).unwrap_or_default(), since, limit) }
/// A file as the run started (`git show HEAD:<path>` in the run's folder), "" when it is new. Blocking.
pub fn base_of(dir: &Path, path: &str) -> String {
    if path.split('/').any(|p| p == ".." || p.is_empty()) { return String::new(); }
    crate::proc::cmd("git").arg("-C").arg(dir).args(["show", &format!("HEAD:{path}")]).output().ok().filter(|o| o.status.success()).map(|o| String::from_utf8_lossy(&o.stdout).chars().take(MAX_TEXT).collect()).unwrap_or_default()
}

/// The files of the run's folder, relative paths: what git tracks plus what is new and not ignored (so `node_modules` and build output stay out), at most 4000. Blocking.
pub fn tree_of(dir: &Path) -> Vec<String> {
    let o = crate::proc::cmd("git").arg("-C").arg(dir).args(["ls-files", "-co", "--exclude-standard", "-z"]).output().ok().filter(|o| o.status.success());
    let mut v: Vec<String> = o.map(|o| String::from_utf8_lossy(&o.stdout).split('\0').filter(|p| !p.is_empty() && !p.starts_with(".antiloki/") && !p.split('/').any(|x| x == "..")).take(4000).map(String::from).collect()).unwrap_or_default();
    v.sort(); v.dedup(); v
}

#[cfg(test)]
mod tests {
    use super::*;
    fn line(content: Value) -> String { json!({ "type": "assistant", "message": { "content": content } }).to_string() }
    #[test] fn a_path_is_inside_the_run_or_nothing() {
        assert_eq!(relative("/w/run", "/w/run/src/a.ts").as_deref(), Some("src/a.ts")); assert_eq!(relative("/w/run/", "/w/run/a.ts").as_deref(), Some("a.ts")); assert_eq!(relative("/w/run", "src/a.ts").as_deref(), Some("src/a.ts")); assert_eq!(relative("/w/run", "./a.ts").as_deref(), Some("a.ts"));
        for bad in ["/etc/passwd", "/w/other/a.ts", "../x", "/w/run/../x", "C:\\x", ""] { assert_eq!(relative("/w/run", bad), None, "{bad}"); } assert_eq!(relative("C:\\w\\run", "C:\\w\\run\\src\\a.ts").as_deref(), Some("src/a.ts"));
    }
    #[test] fn the_tools_that_change_or_read_a_file_become_events_and_the_rest_do_not() {
        let l = line(json!([ { "type": "tool_use", "name": "Write", "input": { "file_path": "/w/run/a.js", "content": "x=1\n" } }, { "type": "tool_use", "name": "Edit", "input": { "file_path": "/w/run/a.js", "old_string": "x=1", "new_string": "x=2" } },
            { "type": "tool_use", "name": "MultiEdit", "input": { "file_path": "/w/run/b.js", "edits": [ { "old_string": "a", "new_string": "b" }, { "old_string": "c", "new_string": "d" } ] } }, { "type": "tool_use", "name": "Read", "input": { "file_path": "/w/run/a.js" } },
            { "type": "tool_use", "name": "Bash", "input": { "command": "npm test" } }, { "type": "tool_use", "name": "Grep", "input": { "pattern": "x" } }, { "type": "tool_use", "name": "Write", "input": { "file_path": "/etc/x", "content": "no" } } ]));
        let v = events_of_line(&l, "/w/run"); assert_eq!(v.iter().map(|e| e["kind"].as_str().unwrap()).collect::<Vec<_>>(), vec!["write", "edit", "edit", "edit", "read", "run"]);
        assert_eq!(v[0]["text"], json!("x=1\n")); assert_eq!((v[1]["old"].clone(), v[1]["new"].clone()), (json!("x=1"), json!("x=2"))); assert_eq!(v[2]["file"], json!("b.js")); assert_eq!(v[5]["cmd"], json!("npm test"));
        assert!(events_of_line("not json tool_use", "/w/run").is_empty() && events_of_line(&json!({ "type": "user" }).to_string(), "/w").is_empty());
    }
    #[test] fn the_tree_lists_tracked_and_new_files_but_not_ignored_ones() {
        let d = std::env::temp_dir().join(format!("antiloki-tree-{}", std::process::id())); let _ = std::fs::remove_dir_all(&d); std::fs::create_dir_all(d.join("src")).unwrap(); std::fs::create_dir_all(d.join("node_modules/x")).unwrap();
        let g = |a: &[&str]| { let _ = crate::proc::cmd("git").arg("-C").arg(&d).args(a).output(); }; g(&["init", "-q"]); std::fs::write(d.join(".gitignore"), "node_modules\n").unwrap(); std::fs::write(d.join("src/a.ts"), "1").unwrap(); std::fs::write(d.join("node_modules/x/i.js"), "1").unwrap(); std::fs::write(d.join("b.md"), "1").unwrap();
        if crate::proc::cmd("git").arg("--version").output().is_ok() { let t = tree_of(&d); assert!(t.contains(&"src/a.ts".to_string()) && t.contains(&"b.md".to_string()) && !t.iter().any(|p| p.starts_with("node_modules"))); }
        let _ = std::fs::remove_dir_all(&d);
    }
    #[test] fn events_are_numbered_in_order_and_read_from_any_point() {
        let d = std::env::temp_dir().join(format!("antiloki-edits-{}", std::process::id())); let run = "r1"; let rd = d.join(".antiloki").join("team").join(run); std::fs::create_dir_all(&rd).unwrap(); let root = d.to_string_lossy().to_string();
        append(&root, run, "work", "/w/run", &line(json!([ { "type": "tool_use", "name": "Write", "input": { "file_path": "/w/run/a.js", "content": "1" } }, { "type": "tool_use", "name": "Read", "input": { "file_path": "/w/run/b.js" } } ]))); append(&root, run, "qa", "/w/run", &line(json!([ { "type": "tool_use", "name": "Bash", "input": { "command": "ls" } } ])));
        let (all, n) = read_since(&root, run, 0, 100); assert_eq!(n, 3); assert_eq!(all.iter().map(|e| e["i"].as_u64().unwrap()).collect::<Vec<_>>(), vec![0, 1, 2]); assert_eq!(all[2]["node"], json!("qa")); assert!(all[0]["t_ms"].as_u64().unwrap() > 0);
        let (tail, _) = read_since(&root, run, 2, 100); assert_eq!(tail.len(), 1); assert_eq!(read_since(&root, "nope", 0, 10), (vec![], 0)); let _ = std::fs::remove_dir_all(&d);
    }
}
