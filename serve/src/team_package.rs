//! THE PACKAGE NODE, first slice (N14, operator 2026-11-16: a team step that brings in a GitHub repo, an npm or a pip package, safely).
//! This file only DECIDES: is the request safe enough to ask the person about, and what exactly would run. It installs nothing and runs nothing.
//! Rules (supply chain): a version is pinned exactly (no ranges, no `latest`, no branch), a hash is given, install scripts are off, it lands in the run's own folder (never the machine), and the person is asked once per version.
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Spec { /** npm | pip | git */ pub source: String, pub name: String, /** exact version, or for git the full 40-hex commit */ pub version: String, /** sha256 of the artifact (npm/pip) or empty for git (the commit is the pin) */ #[serde(default)] pub sha256: String, /** install scripts, off unless the person allows them */ #[serde(default)] pub scripts: bool }
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Plan { pub spec: Spec, pub dir: String, pub argv: Vec<String>, /** what the person is told before allowing */ pub notes: Vec<String>, /** the key the approval is remembered under: a new version or hash is a new key */ pub approval_key: String }

fn hex(s: &str, n: usize) -> bool { s.len() == n && s.chars().all(|c| c.is_ascii_hexdigit()) }
fn name_ok(source: &str, n: &str) -> bool {
    if n.is_empty() || n.len() > 214 || n.contains("..") || n.starts_with('-') || n.contains(char::is_whitespace) { return false; }
    match source { "npm" => n.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "-._@/~".contains(c)), "pip" => n.chars().all(|c| c.is_ascii_alphanumeric() || "-._".contains(c)),
        // a GitHub repo as owner/name (never a URL with credentials or another host)
        "git" => n.split('/').count() == 2 && n.split('/').all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_alphanumeric() || "-._".contains(c))), _ => false }
}
/// The folder a package lands in: inside the run, under the node. Pure over its parts.
pub fn sandbox_dir(root: &str, run: &str, node: &str) -> PathBuf { Path::new(root).join(".antiloki").join("team").join(run).join("packages").join(node) }

/// Refuse what cannot be made safe; otherwise say exactly what would run. Pure.
pub fn plan(spec: &Spec, root: &str, run: &str, node: &str) -> Result<Plan, Vec<String>> {
    let mut e = vec![];
    if !["npm", "pip", "git"].contains(&spec.source.as_str()) { e.push(format!("the source is npm, pip or git, not \"{}\"", spec.source)); }
    if !name_ok(&spec.source, &spec.name) { e.push(format!("\"{}\" is not a package name this source accepts", spec.name)); }
    let v = spec.version.trim();
    match spec.source.as_str() {
        "git" => if !hex(v, 40) { e.push("a git source is pinned to the full 40-character commit, not a branch or a tag".into()); },
        _ => { if v.is_empty() || v == "latest" || v.chars().any(|c| "^~*<>=| ,".contains(c)) || v.split(|c| c == '.' || c == '-').any(|p| p.eq_ignore_ascii_case("x")) || !v.chars().next().map_or(false, |c| c.is_ascii_digit()) { e.push("pin one exact version (like 4.0.231): no ranges, no latest".into()); }
              if !hex(&spec.sha256, 64) { e.push("give the sha256 of the artifact (64 hex characters): without it a changed package would go unseen".into()); } }
    }
    if !run.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') || !node.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') { e.push("the run and node names are plain words".into()); }
    if !e.is_empty() { return Err(e); }
    let dir = sandbox_dir(root, run, node); let d = dir.to_string_lossy().to_string();
    let argv: Vec<String> = match spec.source.as_str() {
        "npm" => { let mut a: Vec<String> = ["npm", "install", "--no-save", "--no-audit", "--no-fund", "--prefix", &d].iter().map(|s| s.to_string()).collect(); if !spec.scripts { a.push("--ignore-scripts".into()); } a.push(format!("{}@{}", spec.name, v)); a }
        "pip" => ["pip", "install", "--target", &d, "--no-deps", "--require-hashes", "--only-binary=:all:"].iter().map(|s| s.to_string()).chain([format!("{}=={} --hash=sha256:{}", spec.name, v, spec.sha256)]).collect(),
        _ => ["git", "clone", "--no-checkout", &format!("https://github.com/{}.git", spec.name), &d].iter().map(|s| s.to_string()).collect(),
    };
    let mut notes = vec![format!("lands in {d} (the run's folder, not your machine)"), if spec.scripts { "install scripts are ON: they run code on install".to_string() } else { "install scripts are off".to_string() }];
    if spec.source == "git" { notes.push(format!("then checks out commit {v} only")); }
    Ok(Plan { approval_key: format!("{}:{}@{}:{}", spec.source, spec.name, v, spec.sha256), spec: spec.clone(), dir: d, argv, notes })
}


// ---- THE PACKAGE STEP (2026-11-17): the second slice. `package` is a TOOL TYPE of the tool step: it fetches a pinned git source into the run's folder, builds a venv with exactly pinned wheel-only dependencies, and runs ONE entry of a fixed registry (a graph names an entry, never a command line).
/// The entries a package step may name: (name, python module, the arguments; `{input}` and `{out}` are the step's two paths inside the run's tree).
pub const ENTRIES: [(&str, &str, &[&str]); 1] = [("rea_video", "rea_video.cli", &["decompose", "{input}", "-o", "{out}"])];
#[derive(Clone, Debug, PartialEq)]
pub struct Run { pub spec: Spec, pub entry: &'static str, pub module: &'static str, pub deps: Vec<String>, pub input: String, pub out: String }
fn rel_ok(p: &str) -> bool { !p.is_empty() && p.len() < 200 && !p.starts_with('/') && !p.starts_with('\\') && !p.contains("..") && !p.contains(':') && !p.split('/').any(|c| c == ".git" || c == ".antiloki" || c.starts_with(".env")) }
/// What is wrong with a package step's arguments, if anything; else the run. Pure. `args`: {source:"git", name:"owner/repo", version:<40 hex>, deps:["numpy==2.5.5"], input, out}.
pub fn check_run(entry: &str, args: &serde_json::Value) -> Result<Run, Vec<String>> {
    let mut e = vec![];
    let Some((en, module, _)) = ENTRIES.iter().find(|(n, _, _)| *n == entry) else { return Err(vec![format!("the package entry is one of: {}", ENTRIES.iter().map(|(n, _, _)| *n).collect::<Vec<_>>().join(", "))]) };
    let g = |k: &str| args[k].as_str().unwrap_or("").to_string();
    let spec = Spec { source: if g("source").is_empty() { "git".into() } else { g("source") }, name: g("name"), version: g("version"), sha256: g("sha256"), scripts: false };
    if spec.source != "git" { e.push("a package step fetches a git source (pinned to its full commit)".into()); }
    if let Err(x) = plan(&spec, "/w", "r", "n") { e.extend(x); }
    let mut deps = vec![];
    for d in args["deps"].as_array().cloned().unwrap_or_default() {
        let d = d.as_str().unwrap_or("").to_string(); let (n, v) = d.split_once("==").unwrap_or(("", ""));
        let ok = !n.is_empty() && n.chars().all(|c| c.is_ascii_alphanumeric() || "-._".contains(c)) && !v.is_empty() && v.chars().all(|c| c.is_ascii_alphanumeric() || ".+!".contains(c));
        if ok { deps.push(d); } else { e.push(format!("a dependency is `name==exact.version`, not \"{d}\"")); }
    }
    if deps.len() > 40 { e.push("at most 40 dependencies".into()); }
    let (input, out) = (g("input"), g("out"));
    if !rel_ok(&input) { e.push("`input` is a path inside the run's tree".into()); }
    if !rel_ok(&out) { e.push("`out` is a path inside the run's tree".into()); }
    if !e.is_empty() { return Err(e); }
    Ok(Run { spec, entry: en, module, deps, input, out })
}
/// The commands the step runs, as (program, arguments) in order, given the sandbox folder, the tree and the platform. Pure.
pub fn steps(r: &Run, dir: &str, tree: &str, windows: bool) -> Vec<(String, Vec<String>)> {
    let src = format!("{dir}/src"); let venv = format!("{dir}/venv");
    let py = if windows { format!("{venv}/Scripts/python.exe") } else { format!("{venv}/bin/python") };
    let mut v: Vec<(String, Vec<String>)> = vec![
        ("git".into(), vec!["clone".into(), "--no-checkout".into(), format!("https://github.com/{}.git", r.spec.name), src.clone()]),
        ("git".into(), vec!["-C".into(), src.clone(), "checkout".into(), "--quiet".into(), r.spec.version.clone()]),
        (if windows { "python".into() } else { "python3".into() }, vec!["-m".into(), "venv".into(), venv.clone()]),
    ];
    if !r.deps.is_empty() { let mut a: Vec<String> = ["-m", "pip", "install", "--quiet", "--no-input", "--disable-pip-version-check", "--only-binary=:all:"].iter().map(|s| s.to_string()).collect(); a.extend(r.deps.iter().cloned()); v.push((py.clone(), a)); }
    let args = ENTRIES.iter().find(|(n, _, _)| *n == r.entry).map(|(_, _, a)| *a).unwrap_or(&[]);
    let mut run: Vec<String> = vec!["-m".into(), r.module.to_string()];
    for a in args { run.push(a.replace("{input}", &format!("{tree}/{}", r.input)).replace("{out}", &format!("{tree}/{}", r.out))); }
    v.push((py, run)); v
}
/// Has the person approved exactly this package (name, commit, hash)? The list lives in the team's folder; only an op the person calls adds to it.
pub fn approved(list: &serde_json::Value, key: &str) -> bool { list.as_array().map_or(false, |a| a.iter().any(|k| k.as_str() == Some(key))) }

#[cfg(test)]
mod tests {
    use super::*;
    fn s(src: &str, n: &str, v: &str, h: &str) -> Spec { Spec { source: src.into(), name: n.into(), version: v.into(), sha256: h.into(), scripts: false } }
    const H: &str = "7f3a1c9e7f3a1c9e7f3a1c9e7f3a1c9e7f3a1c9e7f3a1c9e7f3a1c9e7f3a1c9e";
    #[test] fn an_exact_pinned_hashed_package_gets_a_plan_that_lands_in_the_run_with_scripts_off() {
        let p = plan(&s("npm", "remotion", "4.0.231", H), "/w", "r1", "pkg").unwrap();
        assert!(p.argv.contains(&"--ignore-scripts".to_string()) && p.argv.last().unwrap() == "remotion@4.0.231" && p.dir.ends_with("team/r1/packages/pkg"), "{:?}", p.argv);
        assert!(p.approval_key.contains("4.0.231") && p.approval_key.ends_with(H)); assert_ne!(p.approval_key, plan(&s("npm", "remotion", "4.0.232", H), "/w", "r1", "pkg").unwrap().approval_key, "a new version asks again");
        let mut on = s("npm", "a", "1.0.0", H); on.scripts = true; let q = plan(&on, "/w", "r1", "pkg").unwrap(); assert!(!q.argv.contains(&"--ignore-scripts".to_string()) && q.notes[1].contains("ON"));
        let g = plan(&s("git", "owner/repo", &"a".repeat(40), ""), "/w", "r1", "pkg").unwrap(); assert!(g.argv.iter().any(|a| a == "https://github.com/owner/repo.git"));
        assert!(plan(&s("pip", "rich", "13.7.0", H), "/w", "r1", "pkg").unwrap().argv.iter().any(|a| a.contains("--hash=sha256:")));
    }
    #[test] fn ranges_latest_missing_hashes_branches_and_odd_names_are_refused_with_the_reason() {
        assert!(plan(&s("npm", "a", "2.0.0-experimental", H), "/w", "r", "n").is_ok());
        for v in ["^4.0.0", "~1.2.3", "latest", "1.x", ">=2", "", "*"] { assert!(plan(&s("npm", "a", v, H), "/w", "r", "n").is_err(), "{v}"); }
        assert!(plan(&s("npm", "a", "1.0.0", ""), "/w", "r", "n").unwrap_err()[0].contains("sha256"));
        assert!(plan(&s("git", "o/r", "main", ""), "/w", "r", "n").unwrap_err()[0].contains("40-character"));
        for n in ["../x", "a b", "-rf", "https://evil.com/x", "o/r/extra"] { assert!(plan(&s("git", n, &"a".repeat(40), ""), "/w", "r", "n").is_err(), "{n}"); }
        assert!(plan(&s("cargo", "a", "1.0.0", H), "/w", "r", "n").is_err()); assert!(plan(&s("npm", "a", "1.0.0", H), "/w", "../r", "n").is_err());
    }

    #[test]
    fn a_package_step_is_pinned_registered_and_inside_the_tree() {
        let sha = "07dd587119e884f7c50fbfae92e47840de86bea5"; let a = |v: &str, deps: serde_json::Value, input: &str| serde_json::json!({ "source": "git", "name": "JuneYaooo/rea-video", "version": v, "deps": deps, "input": input, "out": "reference/analysis" });
        let ok = check_run("rea_video", &a(sha, serde_json::json!(["numpy==2.5.5", "opencv-python-headless==4.14.0.94"]), "reference/ref.mp4")).unwrap();
        assert_eq!(ok.module, "rea_video.cli"); assert_eq!(ok.deps.len(), 2);
        assert!(check_run("rm_rf", &a(sha, serde_json::json!([]), "x")).is_err(), "an entry outside the registry");
        assert!(check_run("rea_video", &a("main", serde_json::json!([]), "x")).is_err(), "a branch is not a pin");
        assert!(check_run("rea_video", &a(sha, serde_json::json!(["numpy>=1"]), "x")).is_err(), "a range is not a pin");
        assert!(check_run("rea_video", &a(sha, serde_json::json!([]), "../x.mp4")).is_err() && check_run("rea_video", &a(sha, serde_json::json!([]), "/etc/passwd")).is_err());
        let st = steps(&ok, "/d", "/t", false);
        assert_eq!(st.len(), 5); assert_eq!(st[0].0, "git"); assert!(st[1].1.contains(&sha.to_string())); assert_eq!(st[3].1.last().map(|s| s.as_str()), Some("opencv-python-headless==4.14.0.94"));
        assert_eq!(st[4].0, "/d/venv/bin/python"); assert_eq!(st[4].1, ["-m", "rea_video.cli", "decompose", "/t/reference/ref.mp4", "-o", "/t/reference/analysis"]);
        assert_eq!(steps(&ok, "C:/d", "C:/t", true)[4].0, "C:/d/venv/Scripts/python.exe");
        assert!(approved(&serde_json::json!(["a", "k"]), "k") && !approved(&serde_json::json!(["a"]), "k"));
    }
}
