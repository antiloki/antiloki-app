//! THE LINT NODE'S CHECKERS (operator 2026-11-21: "code checks: node lint already does code checks, right? Download lints etc. to work deterministically"): the `lint` skill detects what the project is written in, DOWNLOADS the pinned checker it needs the first time (into `<antiloki home>/tools/lint`, never into the project), and runs it.
//! Pinned versions, so the same code gives the same verdict on every machine. A project that has its own `lint` script uses it. No model is called. `plan` is pure over the folder; `run` installs and runs.
use std::path::{Path, PathBuf};

pub const TYPESCRIPT: &str = "5.9.3";
pub const ESLINT: &str = "9.39.1";
pub const RUFF: &str = "0.14.5";

/// One check: what it is, how to get it when it is missing (a program and its arguments), the file that says it is there, and how to run it.
#[derive(Clone, Debug, PartialEq)]
pub struct Step { pub name: String, pub install: Option<(String, Vec<String>)>, pub probe: Option<PathBuf>, pub run: (String, Vec<String>), pub env: Vec<(String, String)> }
fn s(x: &str) -> String { x.to_string() }
fn any(dir: &Path, names: &[&str]) -> bool { names.iter().any(|n| dir.join(n).is_file()) }
fn has_py(dir: &Path) -> bool { any(dir, &["pyproject.toml", "requirements.txt", "setup.py", "setup.cfg"]) || std::fs::read_dir(dir).map_or(false, |r| r.flatten().any(|e| e.path().extension().map_or(false, |x| x == "py"))) }
/// What to run in this folder, in order. `tools` is where downloaded checkers live. Pure over the folder.
pub fn plan(dir: &Path, tools: &Path) -> Vec<Step> {
    let mut v = vec![]; let lintdir = tools.join("lint"); let node_bin = |pkg: &str, file: &str| lintdir.join("node_modules").join(pkg).join(file);
    let pkg = std::fs::read_to_string(dir.join("package.json")).ok().and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok());
    let own_lint = pkg.as_ref().and_then(|p| p["scripts"]["lint"].as_str()).is_some();
    if own_lint { v.push(Step { name: "the project's own lint".into(), install: None, probe: None, run: (s("npm"), vec![s("run"), s("lint"), s("--silent")]), env: vec![] }); }
    else {
        let npm_i = |spec: &str| Some((s("npm"), vec![s("install"), s("--prefix"), lintdir.to_string_lossy().to_string(), s("--no-audit"), s("--no-fund"), s("--silent"), spec.to_string()]));
        if dir.join("tsconfig.json").is_file() { v.push(Step { name: format!("TypeScript {TYPESCRIPT}"), install: npm_i(&format!("typescript@{TYPESCRIPT}")), probe: Some(node_bin("typescript", "bin/tsc")), run: (s("node"), vec![node_bin("typescript", "bin/tsc").to_string_lossy().to_string(), s("--noEmit"), s("-p"), dir.to_string_lossy().to_string()]), env: vec![] }); }
        if any(dir, &["eslint.config.js", "eslint.config.mjs", "eslint.config.cjs", "eslint.config.ts", ".eslintrc", ".eslintrc.json", ".eslintrc.js", ".eslintrc.cjs", ".eslintrc.yml"]) { v.push(Step { name: format!("ESLint {ESLINT}"), install: npm_i(&format!("eslint@{ESLINT}")), probe: Some(node_bin("eslint", "bin/eslint.js")), run: (s("node"), vec![node_bin("eslint", "bin/eslint.js").to_string_lossy().to_string(), s(".")]), env: vec![] }); }
    }
    if dir.join("Cargo.toml").is_file() { v.push(Step { name: "Clippy".into(), install: Some((s("rustup"), vec![s("component"), s("add"), s("clippy")])), probe: None, run: (s("cargo"), vec![s("clippy"), s("--quiet")]), env: vec![] }); }
    if has_py(dir) { let py = lintdir.join("py"); v.push(Step { name: format!("Ruff {RUFF}"), install: Some((s("python3"), vec![s("-m"), s("pip"), s("install"), s("--quiet"), s("--target"), py.to_string_lossy().to_string(), format!("ruff=={RUFF}")])), probe: Some(py.join("ruff")), run: (s("python3"), vec![s("-m"), s("ruff"), s("check"), s(".")]), env: vec![(s("PYTHONPATH"), py.to_string_lossy().to_string())] }); }
    if dir.join("go.mod").is_file() { v.push(Step { name: "go vet".into(), install: None, probe: None, run: (s("go"), vec![s("vet"), s("./...")]), env: vec![] }); }
    v
}
/// Where the downloaded checkers live: beside the other machine-wide files, under `tools`.
pub fn tools_dir() -> PathBuf { antiloki_rs::cockpit__helpers_webapps::store_path().with_file_name("tools") }
/// Install what is missing, run every step, and say what happened: (worst exit code, the text). A step whose checker could not be fetched says so and counts as a failure (no silent pass). Blocking.
pub fn run(dir: &Path, tools: &Path) -> (i64, String) {
    let steps = plan(dir, tools); if steps.is_empty() { return (0, "nothing to lint: this project has no TypeScript, ESLint, Rust, Python or Go to check".into()); }
    let mut text = String::new(); let mut worst = 0i64;
    for st in steps {
        text.push_str(&format!("== {}\n", st.name));
        if let (Some((prog, args)), true) = (&st.install, st.probe.as_ref().map_or(true, |p| !p.exists())) {
            let need = st.probe.is_some() || crate::proc::cmd(&st.run.0).args(["clippy", "--version"]).output().map_or(true, |o| !o.status.success());
            if need { text.push_str(&format!("(downloading {})\n", st.name)); let o = crate::proc::cmd(prog).args(args).current_dir(dir).output();
                match o { Ok(o) if o.status.success() => {}, Ok(o) => { text.push_str(&format!("could not download it: {}\n", String::from_utf8_lossy(&o.stderr).lines().last().unwrap_or("").trim())); worst = worst.max(2); continue; }, Err(e) => { text.push_str(&format!("could not download it: {e}\n")); worst = worst.max(2); continue; } } }
        }
        let mut c = crate::proc::cmd(&st.run.0); c.args(&st.run.1).current_dir(dir); for (k, v) in &st.env { c.env(k, v); }
        match c.output() { Ok(o) => { let code = o.status.code().unwrap_or(-1) as i64; text.push_str(&String::from_utf8_lossy(&o.stdout)); text.push_str(&String::from_utf8_lossy(&o.stderr)); text.push_str(&format!("(exit {code})\n")); if code != 0 { worst = worst.max(code.max(1)); } }
            Err(e) => { text.push_str(&format!("could not run it: {e}\n")); worst = worst.max(2); } }
    }
    (worst, text)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn tmp(tag: &str) -> PathBuf { let d = std::env::temp_dir().join(format!("antiloki-lint-{tag}-{}", std::process::id())); let _ = std::fs::remove_dir_all(&d); std::fs::create_dir_all(&d).unwrap(); d }
    #[test] fn a_project_is_checked_by_what_it_is_written_in_with_pinned_downloads() {
        let d = tmp("plan"); let t = tmp("tools"); assert!(plan(&d, &t).is_empty());
        std::fs::write(d.join("tsconfig.json"), "{}").unwrap(); std::fs::write(d.join("eslint.config.js"), "").unwrap(); std::fs::write(d.join("Cargo.toml"), "").unwrap(); std::fs::write(d.join("a.py"), "").unwrap(); std::fs::write(d.join("go.mod"), "").unwrap();
        let p = plan(&d, &t); assert_eq!(p.iter().map(|x| x.name.clone()).collect::<Vec<_>>(), vec!["TypeScript 5.9.3", "ESLint 9.39.1", "Clippy", "Ruff 0.14.5", "go vet"]);
        let ts = &p[0]; assert!(ts.install.as_ref().unwrap().1.contains(&"typescript@5.9.3".to_string()), "pinned, never latest"); assert!(ts.install.as_ref().unwrap().1.iter().any(|a| a.starts_with(t.to_string_lossy().as_ref())), "downloaded outside the project"); assert!(ts.probe.as_ref().unwrap().starts_with(&t));
        assert!(p[3].install.as_ref().unwrap().1.contains(&"ruff==0.14.5".to_string()));
    }
    #[test] fn the_projects_own_lint_script_wins_and_nothing_is_downloaded_for_it() {
        let d = tmp("own"); let t = tmp("tools2"); std::fs::write(d.join("package.json"), r#"{"scripts":{"lint":"eslint ."}}"#).unwrap(); std::fs::write(d.join("tsconfig.json"), "{}").unwrap();
        let p = plan(&d, &t); assert_eq!(p.len(), 1); assert!(p[0].install.is_none()); assert_eq!(p[0].run.1, vec!["run", "lint", "--silent"]);
    }
    #[test] fn an_empty_project_says_there_is_nothing_to_lint_and_passes() { let d = tmp("none"); let (c, t) = run(&d, &tmp("tools3")); assert_eq!(c, 0); assert!(t.contains("nothing to lint")); }
}
