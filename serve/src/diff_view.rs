//! THE DIFF PANE'S DATA (operator 2026-11-10: "Diff pane: deterministic, git-based, no AI ... fix and test for real"): what an agent's box changed, as git says it, with the patch of each file. No model, no guess: the
//! box's working tree against the commit it was cut from (the merge-base with the main worktree's branch), so uncommitted work counts, and files the box made and never added are listed too.
//!
//!   GET /api/v1/diff?pane_id=N               { base, ahead, files: [{path, status, add, del, binary}], hidden }
//!   GET /api/v1/diff/patch?pane_id=N&file=P  { patch }  (a unified diff; an untracked file as an addition)
//!
//! Files antiloki writes into a box itself (its context pack, the engines' settings, the MCP config) are not the agent's work: they are counted in `hidden` and not listed. Pure parts are tested; git is run
//! with `-C <box>` and a fixed environment, never through a shell.
use crate::dispatch::AppState;
use axum::{http::StatusCode, response::{IntoResponse, Response}, Json};
use serde_json::{json, Value};
use std::path::Path;
use std::sync::Arc;

const MAX_PATCH: usize = 240_000;
fn reply(v: Value) -> Response { (StatusCode::OK, Json(v)).into_response() }

/// A path antiloki itself puts in a box. Pure - tested.
pub fn is_artifact(p: &str) -> bool {
    let first = p.split('/').next().unwrap_or("");
    [".antiloki", ".claude", ".codex", ".cursor", ".gemini", ".opencode", ".agents"].contains(&first) || matches!(p, ".mcp.json" | "opencode.json" | "CLAUDE.md" | "AGENTS.md" | "GEMINI.md")
}
/// `git diff --numstat` lines: `12\t3\tpath` (binary: `-\t-\tpath`; a rename: `1\t0\told => new` or `{a => b}`). Pure - tested.
pub fn parse_numstat(out: &str) -> Vec<(String, i64, i64, bool)> {
    out.lines().filter_map(|l| { let mut it = l.splitn(3, '\t'); let (a, d, p) = (it.next()?, it.next()?, it.next()?); let bin = a == "-" && d == "-"; Some((rename_target(p), a.parse().unwrap_or(0), d.parse().unwrap_or(0), bin)) }).collect()
}
/// The path a numstat rename line ends at: `old => new`, or `dir/{a => b}/x` -> `dir/b/x`. Pure - tested.
pub fn rename_target(p: &str) -> String {
    if let (Some(a), Some(b)) = (p.find('{'), p.find('}')) { if let Some(arrow) = p[a..b].find(" => ") { let (l, r) = (&p[a + 1..a + arrow], &p[a + arrow + 4..b]); return format!("{}{}{}", &p[..a], r, &p[b + 1..]).replace("//", "/").trim_start_matches('/').to_string().replace(l, l).to_string(); } }
    match p.split_once(" => ") { Some((_, n)) => n.to_string(), None => p.to_string() }
}
/// `git diff --name-status` lines -> (status letter, path). Pure - tested.
pub fn parse_name_status(out: &str) -> Vec<(String, String)> {
    out.lines().filter_map(|l| { let parts: Vec<&str> = l.split('\t').collect(); if parts.len() < 2 { return None; } let st = parts[0].chars().next()?.to_string(); Some((st, parts.last()?.to_string())) }).collect()
}
fn git(dir: &str, args: &[&str]) -> Result<(i32, String), String> {
    let o = crate::proc::cmd("git").arg("-C").arg(dir).args(["-c", "core.quotepath=off"]).args(args).env("GIT_OPTIONAL_LOCKS", "0").env("LC_ALL", "C").output().map_err(|e| format!("git: {e}"))?;
    Ok((o.status.code().unwrap_or(-1), String::from_utf8_lossy(&o.stdout).to_string()))
}
/// The commit a box was cut from: the merge-base of its HEAD with the branch checked out in the main worktree (itself when the box is the main worktree). Falls back to HEAD.
fn base_of(dir: &str) -> String {
    let main = git(dir, &["worktree", "list", "--porcelain"]).ok().and_then(|(_, t)| t.lines().find_map(|l| l.strip_prefix("branch ").map(str::to_string)));
    if let Some(m) = main { if let Ok((0, b)) = git(dir, &["merge-base", "HEAD", &m]) { let b = b.trim().to_string(); if !b.is_empty() { return b; } } }
    git(dir, &["rev-parse", "HEAD"]).map(|(_, t)| t.trim().to_string()).unwrap_or_default()
}
pub fn files_of(dir: &str) -> Result<Value, String> {
    if !Path::new(dir).join(".git").exists() && git(dir, &["rev-parse", "--git-dir"])?.0 != 0 { return Err("this folder is not a git checkout".into()); }
    let base = base_of(dir); if base.is_empty() { return Err("the box has no commits yet".into()); }
    let status = parse_name_status(&git(dir, &["diff", "--name-status", "-M", &base])?.1);
    let nums = parse_numstat(&git(dir, &["diff", "--numstat", "-M", &base])?.1);
    let mut files: Vec<Value> = vec![]; let mut hidden = 0;
    for (path, add, del, bin) in nums {
        if is_artifact(&path) { hidden += 1; continue; }
        let st = status.iter().find(|(_, p)| *p == path).map(|(s, _)| s.clone()).unwrap_or_else(|| "M".into());
        files.push(json!({ "path": path, "status": st, "add": add, "del": del, "binary": bin }));
    }
    for p in git(dir, &["ls-files", "--others", "--exclude-standard"])?.1.lines().map(str::trim).filter(|p| !p.is_empty()) {
        if is_artifact(p) { hidden += 1; continue; }
        let lines = std::fs::read(Path::new(dir).join(p)).map(|b| (b.iter().filter(|c| **c == b'\n').count() as i64 + if b.last().map_or(false, |c| *c != b'\n') { 1 } else { 0 }, b.contains(&0))).unwrap_or((0, false));
        files.push(json!({ "path": p, "status": "?", "add": lines.0, "del": 0, "binary": lines.1 }));
    }
    files.sort_by(|a, b| a["path"].as_str().cmp(&b["path"].as_str()));
    let ahead = git(dir, &["rev-list", "--count", &format!("{base}..HEAD")]).ok().and_then(|(_, t)| t.trim().parse::<i64>().ok()).unwrap_or(0);
    Ok(json!({ "ok": true, "base": base.chars().take(10).collect::<String>(), "ahead": ahead, "files": files, "hidden": hidden }))
}
pub fn patch_of(dir: &str, file: &str) -> Result<String, String> {
    if file.is_empty() || file.starts_with('/') || file.split('/').any(|s| s == "..") || file.contains('\0') { return Err("that is not a path in the box".into()); }
    let base = base_of(dir);
    let tracked = git(dir, &["ls-files", "--error-unmatch", "--", file]).map(|(c, _)| c == 0).unwrap_or(false);
    let out = if tracked || git(dir, &["cat-file", "-e", &format!("{base}:{file}")]).map(|(c, _)| c == 0).unwrap_or(false) { git(dir, &["diff", "--no-color", "-U4", "-M", &base, "--", file])?.1 }
        else { git(dir, &["diff", "--no-color", "-U4", "--no-index", "--", "/dev/null", file])?.1 };
    Ok(if out.len() > MAX_PATCH { format!("{}\n… cut at {} bytes", &out[..MAX_PATCH.min(out.len())], MAX_PATCH) } else { out })
}

async fn box_of(state: &Arc<AppState>, q: &std::collections::HashMap<String, String>) -> Result<String, Response> {
    let id = q.get("pane_id").and_then(|v| v.parse::<i64>().ok()).unwrap_or(0);
    if id <= 0 { return Err(reply(json!({ "ok": false, "error": "say which pane" }))); }
    match crate::chat_pane::pane_row(state, id).await { Some(p) => { let d = crate::shared_folder::working_dir(state, &p).await; if d.is_empty() || !Path::new(&d).exists() { Err(reply(json!({ "ok": false, "error": "this pane has no box on disk" }))) } else { Ok(d) } } None => Err(reply(json!({ "ok": false, "error": "no such pane" }))) }
}
pub async fn files_http(state: Arc<AppState>, q: std::collections::HashMap<String, String>) -> Response {
    let dir = match box_of(&state, &q).await { Ok(d) => d, Err(r) => return r };
    match tokio::task::spawn_blocking(move || files_of(&dir)).await { Ok(Ok(v)) => reply(v), Ok(Err(e)) => reply(json!({ "ok": false, "error": e })), Err(e) => reply(json!({ "ok": false, "error": e.to_string() })) }
}
pub async fn patch_http(state: Arc<AppState>, q: std::collections::HashMap<String, String>) -> Response {
    let dir = match box_of(&state, &q).await { Ok(d) => d, Err(r) => return r }; let file = q.get("file").cloned().unwrap_or_default();
    match tokio::task::spawn_blocking(move || patch_of(&dir, &file)).await { Ok(Ok(p)) => reply(json!({ "ok": true, "patch": p })), Ok(Err(e)) => reply(json!({ "ok": false, "error": e })), Err(e) => reply(json!({ "ok": false, "error": e.to_string() })) }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn run(d: &Path, a: &[&str]) { let o = std::process::Command::new("git").arg("-C").arg(d).args(["-c", "user.email=t@t", "-c", "user.name=t"]).args(a).output().unwrap(); assert!(o.status.success(), "{a:?}: {}", String::from_utf8_lossy(&o.stderr)); }
    #[test]
    fn what_antiloki_writes_into_a_box_is_not_the_agents_work_and_the_git_lines_are_read() {
        for p in [".antiloki/prompts.jsonl", ".claude/settings.local.json", ".mcp.json", "CLAUDE.md", "AGENTS.md"] { assert!(is_artifact(p), "{p}"); }
        for p in ["src/a.ts", "docs/CLAUDE.md", "README.md", ".github/ci.yml"] { assert!(!is_artifact(p), "{p}"); }
        assert_eq!(parse_numstat("12\t3\tsrc/a.ts\n-\t-\timg.png\n1\t0\told.ts => new.ts\n2\t2\tdir/{a => b}/x.ts\n"), vec![("src/a.ts".into(), 12, 3, false), ("img.png".into(), 0, 0, true), ("new.ts".into(), 1, 0, false), ("dir/b/x.ts".into(), 2, 2, false)]);
        assert_eq!(parse_name_status("M\tsrc/a.ts\nA\tb.ts\nR100\told.ts\tnew.ts\n"), vec![("M".into(), "src/a.ts".into()), ("A".into(), "b.ts".into()), ("R".into(), "new.ts".into())]);
    }
    #[test]
    fn a_box_shows_committed_uncommitted_and_new_files_against_where_it_was_cut_and_never_its_own_artifacts() {
        let t = std::env::temp_dir().join(format!("antiloki-diffview-{}", std::process::id())); let _ = std::fs::remove_dir_all(&t); let main = t.join("main"); std::fs::create_dir_all(&main).unwrap();
        run(&main, &["init", "-q", "-b", "main"]); std::fs::write(main.join("a.txt"), "one\ntwo\n").unwrap(); std::fs::write(main.join("b.txt"), "b\n").unwrap(); run(&main, &["add", "-A"]); run(&main, &["commit", "-q", "-m", "init"]);
        let bx = t.join("box"); run(&main, &["worktree", "add", "-q", "-b", "agent", bx.to_str().unwrap()]);
        std::fs::write(bx.join("a.txt"), "one\nTWO\nthree\n").unwrap(); run(&bx, &["commit", "-q", "-am", "edit a"]);          // committed
        std::fs::write(bx.join("b.txt"), "b changed\n").unwrap();                                                              // uncommitted
        std::fs::write(bx.join("new.ts"), "export const x = 1;\n").unwrap();                                                   // never added
        std::fs::create_dir_all(bx.join(".antiloki")).unwrap(); std::fs::write(bx.join(".antiloki/x.json"), "{}").unwrap(); std::fs::write(bx.join("CLAUDE.md"), "# brief\n").unwrap();   // antiloki's own
        let v = files_of(bx.to_str().unwrap()).unwrap(); let files = v["files"].as_array().unwrap();
        let by = |p: &str| files.iter().find(|f| f["path"] == json!(p)).cloned().unwrap_or(Value::Null);
        assert_eq!((by("a.txt")["add"].clone(), by("a.txt")["del"].clone(), by("a.txt")["status"].clone()), (json!(2), json!(1), json!("M")), "{v}");
        assert_eq!(by("b.txt")["add"], json!(1)); assert_eq!((by("new.ts")["status"].clone(), by("new.ts")["add"].clone()), (json!("?"), json!(1)));
        assert!(by(".antiloki/x.json").is_null() && by("CLAUDE.md").is_null(), "{v}"); assert_eq!(v["hidden"], json!(2), "two of antiloki's own files are counted, not listed: {v}"); assert_eq!(v["ahead"], json!(1));
        assert!(patch_of(bx.to_str().unwrap(), "a.txt").unwrap().contains("+TWO"), "a committed change is in the patch"); assert!(patch_of(bx.to_str().unwrap(), "b.txt").unwrap().contains("+b changed"), "and an uncommitted one");
        let np = patch_of(bx.to_str().unwrap(), "new.ts").unwrap(); assert!(np.contains("+export const x = 1;") && np.contains("new file"), "{np}");
        for bad in ["../x", "/etc/passwd", ""] { assert!(patch_of(bx.to_str().unwrap(), bad).is_err(), "{bad}"); }
        let _ = std::fs::remove_dir_all(&t);
    }
}
