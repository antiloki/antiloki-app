//! THE TEST NODE (operator 2026-11-16): a test written in CODE, before the run, by the leader, that approves or disapproves what the node before it made. A guard, not a model's opinion.
//! The tests live in the team's recipe (data), not in the run's tree: the coder never sees them and the improver cannot change them (a test is a judge: RFC 0013). They run in a SANDBOX COPY of the tree with the leader's files written over it, by a runner from a short list.
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TestSpec {
    /** the test files, path → source, written by the leader before the run */ pub files: BTreeMap<String, String>,
    /** the runner, by NAME (a graph names a tool, never a command line): one of `RUNNERS` */ #[serde(default = "node_test")] pub runner: String,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub timeout_s: Option<u64>,
    /** a HOLDOUT: the coder never sees these files, the judge still runs them (a guard against code that only special-cases the visible tests) */ #[serde(default, skip_serializing_if = "std::ops::Not::not")] pub hidden: bool,
}
fn node_test() -> String { "node_test".into() }
/// The runners a test node may name, and the command line the engine runs for each (the leader never writes one).
pub const RUNNERS: [(&str, &str); 7] = [("node_test", "node --test --test-reporter=spec"), ("npm_test", "npm test"), ("pytest", "python3 -m pytest"), ("unittest", "python3 -m unittest"), ("cargo_test", "cargo test"), ("go_test", "go test ./..."), ("video_check", "node .antiloki-video-check.mjs video-check.json")];
/// THE VIDEO CHECK (VID-9): a fixed script, shipped with the serve and written into the sandbox next to the leader's `video-check.json` (thresholds only). The leader never writes pixel code.
const VIDEO_CHECK_JS: &str = include_str!("team_video_check.mjs");
pub fn command_of(runner: &str) -> Option<&'static str> { RUNNERS.iter().find(|(n, _)| *n == runner).map(|(_, c)| *c) }
/// What is wrong with a spec, if anything. Pure.
pub fn check(sp: &TestSpec) -> Vec<String> {
    let mut e = vec![];
    if sp.files.is_empty() { e.push("a test node holds at least one test file".into()); }
    for (p, c) in &sp.files { if let Err(x) = crate::team_apply::clean(p) { e.push(x); } if c.trim().is_empty() { e.push(format!("{p} is empty")); } if c.len() > 200_000 { e.push(format!("{p} is over 200 KB")); } }
    if sp.runner == "video_check" { match sp.files.get("video-check.json") { None => e.push("the runner `video_check` reads its thresholds from a file called video-check.json".into()), Some(c) => if serde_json::from_str::<serde_json::Value>(c).map_or(true, |v| !v.is_object()) { e.push("video-check.json is a JSON object of thresholds".into()); } } }
    if command_of(&sp.runner).is_none() { e.push(format!("the runner is one of: {}", RUNNERS.iter().map(|(n, _)| *n).collect::<Vec<_>>().join(", "))); }
    if sp.timeout_s.map_or(false, |t| t == 0 || t > 1800) { e.push("`timeout_s` is 1 to 1800".into()); }
    e
}
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Results { pub ok: bool, pub exit: i32, pub total: u32, pub passed: u32, pub failed: Vec<String>, pub tail: String }
/// What the node:test spec reporter said: the counts and the names of the tests that failed. Other runners give the exit code and the tail only. Pure.
pub fn parse_node_test(out: &str) -> (u32, u32, Vec<String>) {
    let num = |key: &str| out.lines().find_map(|l| l.trim().strip_prefix(&format!("ℹ {key} ")).and_then(|v| v.trim().parse::<u32>().ok())).unwrap_or(0);
    let mut failed: Vec<String> = vec![];
    for l in out.lines() { let t = l.trim_start(); if let Some(n) = t.strip_prefix("✖ ") { let n = n.rsplit_once(" (").map_or(n, |(a, b)| if b.ends_with("ms)") { a } else { n }).trim().to_string(); if !n.is_empty() && !n.starts_with("failing tests") && !failed.contains(&n) { failed.push(n); } } }
    (num("tests"), num("pass"), failed)
}
/// Make the results of a finished run. Pure.
pub fn results_of(exit: i32, out: &str) -> Results {
    let (total, passed, failed) = parse_node_test(out); let tail: String = { let t: Vec<&str> = out.lines().collect(); t[t.len().saturating_sub(40)..].join("\n") }; let tail = tail.chars().rev().take(3000).collect::<String>().chars().rev().collect();
    Results { ok: exit == 0, exit, total, passed, failed, tail }
}
fn copy_tree(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for e in std::fs::read_dir(from)? { let e = e?; let n = e.file_name(); let n_s = n.to_string_lossy(); if [".git", ".antiloki", "node_modules", "target"].contains(&n_s.as_ref()) { continue; }
        let (p, q) = (e.path(), to.join(&n)); if e.file_type()?.is_dir() { copy_tree(&p, &q)?; } else { std::fs::copy(&p, &q)?; } }
    Ok(())
}
/// Run the leader's tests over a sandbox copy of `dir`.
pub async fn run(dir: &Path, sp: &TestSpec) -> Result<Results, String> {
    let e = check(sp); if !e.is_empty() { return Err(e.join("; ")); }
    let tmp = std::env::temp_dir().join(format!("antiloki-test-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0)));
    copy_tree(dir, &tmp).map_err(|e| format!("no sandbox: {e}"))?;
    for (p, c) in &sp.files { let to = tmp.join(p); if let Some(d) = to.parent() { std::fs::create_dir_all(d).map_err(|e| e.to_string())?; } std::fs::write(&to, c).map_err(|e| e.to_string())?; }
    if sp.runner == "video_check" { std::fs::write(tmp.join(".antiloki-video-check.mjs"), VIDEO_CHECK_JS).map_err(|e| e.to_string())?; }
    let t = std::time::Duration::from_secs(sp.timeout_s.unwrap_or(120));
    let out = tokio::time::timeout(t, crate::proc_async::shell(command_of(&sp.runner).unwrap_or("true")).current_dir(&tmp).output()).await;
    let _ = std::fs::remove_dir_all(&tmp);
    match out { Ok(Ok(o)) => Ok(results_of(o.status.code().unwrap_or(-1), &format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr)))), Ok(Err(e)) => Err(format!("the runner did not start: {e}")), Err(_) => Err(format!("the tests ran out of time ({}s)", t.as_secs())) }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn spec(files: &[(&str, &str)]) -> TestSpec { TestSpec { files: files.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect(), runner: node_test(), timeout_s: Some(60), hidden: false } }
    #[test] fn a_spec_is_refused_with_the_reason() {
        assert!(check(&spec(&[("test/a.test.js", "x")])).is_empty());
        assert!(!check(&spec(&[])).is_empty()); assert!(!check(&spec(&[("../x.test.js", "x")])).is_empty()); assert!(!check(&spec(&[("a.test.js", " ")])).is_empty());
        for bad in ["rm -rf /", "node --test", "node_test; rm x", "curl", ""] { let mut s = spec(&[("a.test.js", "x")]); s.runner = bad.into(); assert!(!check(&s).is_empty(), "{bad}"); }
    }
    #[test] fn the_video_check_needs_its_thresholds_and_ships_its_own_script() {
        let mut sp = spec(&[("video-check.json", "{}")]); sp.runner = "video_check".into(); assert!(check(&sp).is_empty());
        let mut no = spec(&[("other.json", "{}")]); no.runner = "video_check".into(); assert!(check(&no).iter().any(|m| m.contains("video-check.json")));
        let mut bad = spec(&[("video-check.json", "[1]")]); bad.runner = "video_check".into(); assert!(!check(&bad).is_empty());
        assert!(VIDEO_CHECK_JS.contains("ffprobe") && VIDEO_CHECK_JS.contains("process.exit"));
    }
    /// The script on a real clip, when node and ffmpeg are here: a bright centred box passes, a plain black frame fails (skipped where the tools are missing).
    #[tokio::test] async fn the_video_check_passes_a_centred_picture_and_fails_a_black_one() {
        let ok = |p: &str| std::process::Command::new(p).arg("-version").output().map(|o| o.status.success()).unwrap_or(false);
        if !ok("node") || !ok("ffmpeg") { return; }
        let d = std::env::temp_dir().join(format!("antiloki-vc-{}", std::process::id())); let _ = std::fs::remove_dir_all(&d); std::fs::create_dir_all(d.join("out")).unwrap();
        let mk = |vf: &str, f: &str| { let o = std::process::Command::new("ffmpeg").args(["-v", "error", "-y", "-f", "lavfi", "-i", "color=c=black:s=1920x1080:r=30:d=5", "-vf", vf, "-pix_fmt", "yuv420p", f]).output().unwrap(); assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr)); };
        mk("drawbox=x=600+t*0:y=300:w=700:h=400:c=white:t=fill,drawbox=x=100+80*t:y=900:w=200:h=60:c=gray:t=fill", d.join("out/video.mp4").to_str().unwrap());
        let mut sp = spec(&[("video-check.json", "{}")]); sp.runner = "video_check".into(); sp.timeout_s = Some(60);
        let r = run(&d, &sp).await.unwrap(); assert!(r.ok || r.failed.iter().all(|f| f.contains("moves")), "{r:?}");
        mk("null", d.join("out/video.mp4").to_str().unwrap());
        let r = run(&d, &sp).await.unwrap(); assert!(!r.ok && r.failed.iter().any(|f| f.contains("black")), "{r:?}");
        let _ = std::fs::remove_dir_all(&d);
    }
    #[test] fn the_node_test_report_is_read_by_code() {
        let out = "▶ greet\n  ✔ greets (0.8ms)\n  ✖ farewell (1.2ms)\n✖ failing tests:\nℹ tests 2\nℹ suites 1\nℹ pass 1\nℹ fail 1\n";
        let r = results_of(1, out); assert!(!r.ok && r.total == 2 && r.passed == 1); assert_eq!(r.failed, vec!["farewell".to_string()]);
        assert!(results_of(0, "ℹ tests 1\nℹ pass 1\n").ok);
    }
    #[tokio::test] async fn the_leaders_tests_run_over_a_copy_and_the_tree_is_left_as_it_was() {
        let d = std::env::temp_dir().join(format!("antiloki-tt-{}", std::process::id())); let _ = std::fs::remove_dir_all(&d); std::fs::create_dir_all(d.join("src")).unwrap();
        std::fs::write(d.join("package.json"), "{\"type\":\"module\"}").unwrap(); std::fs::write(d.join("src/a.js"), "export const a = () => 1;\n").unwrap();
        let t = "import { test } from 'node:test'; import assert from 'node:assert/strict'; import { a } from '../src/a.js'; test('a is one', () => assert.equal(a(), 1)); test('a is two', () => assert.equal(a(), 2));\n";
        let r = run(&d, &spec(&[("test/a.test.js", t)])).await.unwrap(); assert!(!r.ok && r.total == 2 && r.passed == 1 && r.failed == vec!["a is two".to_string()], "{r:?}");
        assert!(!d.join("test").exists(), "the sandbox was a copy: the tree has no test folder");
        let ok = run(&d, &spec(&[("test/b.test.js", "import { test } from 'node:test'; test('fine', () => {});\n")])).await.unwrap(); assert!(ok.ok && ok.passed == 1);
    }
}
