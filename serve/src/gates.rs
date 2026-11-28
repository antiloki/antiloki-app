//! THE THREE DETERMINISTIC GATES — test · build · lint (operator 2026-09-24: "at heimdall menu on agents add
//! another button to click test and build and lint gates. 3 buttons. They turn on and off 3 more gates
//! deterministically to run after the agent claim completition and force re-run in case of error with the error
//! logs").
//!
//! An agent saying "done" is a claim. These three are the cheapest ways to find out: the project's own test
//! runner, its own build, its own linter — whatever the repo already uses, never a command antiloki invents.
//! A gate with nothing to run says so and is skipped; it never passes by default, because "no linter configured"
//! and "the lint passed" are different facts and only one of them is evidence.
//!
//! On a failure the agent gets the logs back and is asked to fix it, ONCE per gate per turn. Once, because a
//! model that could not make the build green with the compiler's own error in front of it will not get there on
//! the fourth reading of the same error, and a loop that keeps trying spends the operator's money proving it.
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Which gate. `key()` is what the pane's settings store and what the UI toggles.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Gate { Test, Build, Lint, Laws }
impl Gate {
    pub fn key(self) -> &'static str { match self { Gate::Test => "test", Gate::Build => "build", Gate::Lint => "lint", Gate::Laws => "laws" } }
    pub fn of(k: &str) -> Option<Gate> { match k.trim().to_ascii_lowercase().as_str() { "test" => Some(Gate::Test), "build" => Some(Gate::Build), "lint" => Some(Gate::Lint), "laws" => Some(Gate::Laws), _ => None } }
    /// what the operator is told this gate proved, in the story and the log
    pub fn proves(self) -> &'static str {
        match self {
            Gate::Test => "the project's own tests, over the files this turn changed",
            Gate::Build => "the project's own build — it compiles as shipped, not just as edited",
            Gate::Lint => "the project's own linter — the house rules, not a model's taste",
            // THE LAWS GATE (operator 2026-09-25): "a gate validation about the laws x-ray has on the new code,
            // it should be seconds because is deterministic and only on the diff". Only the per-file detectors —
            // regex, absent, path, import — because a cycle cannot be seen in one file.
            Gate::Laws => "X-ray's own code laws, judged on the files this turn changed",
        }
    }
    pub const ALL: [Gate; 4] = [Gate::Test, Gate::Build, Gate::Lint, Gate::Laws];
}

/// The three toggles as they sit on a pane's settings: `settings.gates = { test: true, … }`. Absent = off, so a
/// pane that has never been configured runs nothing extra and nobody is surprised by a build starting itself.
pub fn enabled_on(settings: &Value) -> Vec<Gate> {
    let g = settings.get("gates");
    Gate::ALL.into_iter().filter(|x| {
        // THE LAWS GATE DEFAULTS ON (operator 2026-09-25, after finding no pane had any gate switched on and
        // the record therefore held zero gated rows). The reason the other three default to off is that they
        // start a build or a test suite nobody asked for — minutes of machine time, and on a small plan that
        // matters. None of it applies here: the laws gate is regexes over the files this turn already changed,
        // in process, with no subprocess and no model. It costs milliseconds and it is the only deterministic
        // quality signal live use produces, so an operator who never opens the menu still gets one.
        let dflt = *x == Gate::Laws;
        g.and_then(|g| g.get(x.key())).and_then(Value::as_bool).unwrap_or(dflt)
    }).collect()
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct GateRun {
    pub gate: String, pub cmd: String, pub passed: bool, pub exit: i32, pub ms: i64, pub tail: String,
    /// why it did not run, when it did not — a skipped gate is never a pass
    pub skipped: Option<String>,
}
impl GateRun {
    fn skip(gate: Gate, why: String) -> Self { GateRun { gate: gate.key().into(), cmd: String::new(), passed: false, exit: 0, ms: 0, tail: String::new(), skipped: Some(why) } }
    pub fn failed(&self) -> bool { self.skipped.is_none() && !self.passed }
}

fn pm_of(lock: &str) -> &'static str { if lock.contains("pnpm") { "pnpm" } else if lock.contains("yarn") { "yarn" } else { "npm" } }

/// WHAT IS ACTUALLY IN THE BOX (operator 2026-09-24, from a Windows install: "antiloki didnt discovery how to
/// run, build and test scripts automatically and features depending on that didnt work").
///
/// Discovery used to read one file, `package.json`, so every gate in a repo that is not a Node project skipped
/// with "no package.json in the box" — correct, and useless. A repo states how it is built in its own manifest;
/// this reads the manifests that exist and nothing else. It still never invents a command: each field below is
/// a fact about the box, and `gate_cmd` turns facts into the project's own entry points.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Project {
    pub pkg: Option<String>,      // package.json, verbatim
    pub lock: String,             // which lockfile is present — picks the package manager
    pub node_modules: bool,
    pub cargo: Option<String>,    // Cargo.toml, verbatim
    pub clippy: bool,             // this project declares it lints: clippy.toml, or [lints] in Cargo.toml
    pub make: Option<String>,     // Makefile, verbatim — its targets are read, not guessed
    pub go: bool,
    pub py: Option<String>,       // pyproject.toml / setup.cfg / tox.ini, concatenated
    pub dotnet: bool,
    pub gradle: bool,
    pub maven: bool,
}

/// WHAT TECHNOLOGY THIS PROJECT IS, for the benchmark's technology axis.
///
/// Operator 2026-09-25: *"the type of tasks they change, they are different from technology to technology, and
/// we should test not just on React but on back end as well … we need to have different benchmarks for
/// different technologies using the same tasks."*
///
/// This is NOT `roles::domain_of`, and conflating them would lose both. That one says what KIND of work a
/// change is — backend, frontend, tests, design — from the paths it touched. This says what the project is
/// BUILT IN, from its own manifests. A React frontend and a Rust backend are different technologies; a Rust
/// backend and a Rust CLI are the same one doing different kinds of work. The router needs both axes.
///
/// A repo with two stacks says `mixed` rather than picking a winner, because that is what it is. Pure — tested.
pub fn stack_of(p: &Project) -> &'static str {
    let mut found: Vec<&'static str> = vec![];
    if let Some(pkg) = p.pkg.as_deref() {
        // react/vue/svelte in the manifest is the difference between a frontend and a node service, and it is
        // the project's own declaration rather than a guess from file names
        let ui = ["\"react\"", "\"vue\"", "\"svelte\"", "\"@angular/core\"", "\"solid-js\""].iter().any(|k| pkg.contains(k));
        found.push(if ui { "web" } else { "node" });
    }
    if p.cargo.is_some() { found.push("rust") }
    if p.go { found.push("go") }
    if p.py.is_some() { found.push("python") }
    if p.dotnet { found.push("dotnet") }
    if p.gradle || p.maven { found.push("jvm") }
    match found.len() { 0 => "any", 1 => found[0], _ => "mixed" }
}

/// The technology of a directory, read once per root. The probe touches the filesystem; a call path must not
/// pay for that on every turn, and a project does not change what it is built in while the app is open.
pub fn stack_at(root: &str) -> &'static str {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};
    static CACHE: OnceLock<Mutex<HashMap<String, &'static str>>> = OnceLock::new();
    let m = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(v) = m.lock().ok().and_then(|g| g.get(root).copied()) { return v }
    let v = stack_of(&probe(std::path::Path::new(root)));
    if let Ok(mut g) = m.lock() { g.insert(root.to_string(), v); }
    v
}

/// Read the box. The only impure half of discovery; `gate_cmd` below is pure over the result.
pub fn probe(root: &std::path::Path) -> Project {
    let read = |n: &str| std::fs::read_to_string(root.join(n)).ok();
    let cargo = read("Cargo.toml");
    let py = ["pyproject.toml", "setup.cfg", "tox.ini"].iter().filter_map(|n| read(n)).collect::<Vec<_>>().join("\n");
    let any = |pat: &str| std::fs::read_dir(root).ok().map(|d| d.flatten().any(|e| e.file_name().to_string_lossy().ends_with(pat))).unwrap_or(false);
    Project {
        pkg: read("package.json"),
        lock: ["pnpm-lock.yaml", "yarn.lock", "package-lock.json"].iter().find(|l| root.join(l).is_file()).copied().unwrap_or("").to_string(),
        node_modules: root.join("node_modules").exists(),
        clippy: root.join("clippy.toml").is_file() || cargo.as_deref().is_some_and(|c| c.contains("[lints")),
        cargo,
        make: read("Makefile").or_else(|| read("makefile")),
        go: root.join("go.mod").is_file(),
        py: if py.trim().is_empty() { None } else { Some(py) },
        dotnet: any(".sln") || any(".csproj"),
        gradle: root.join("gradlew").is_file() || root.join("build.gradle").is_file() || root.join("build.gradle.kts").is_file(),
        maven: root.join("pom.xml").is_file(),
    }
}

/// Does this Makefile declare that target? A target is a line starting at column zero, `name:` and not `:=`.
/// Pure — tested.
pub fn make_target(makefile: &str, target: &str) -> bool {
    makefile.lines().any(|l| {
        let Some(rest) = l.strip_prefix(target) else { return false };
        let rest = rest.trim_start();
        rest.starts_with(':') && !rest.starts_with(":=")
    })
}

fn declares(py: &str, tool: &str) -> bool { py.contains(tool) }

/// The command for one gate, from the project's OWN manifests. `Err` is the honest reason there is nothing to
/// run — it is reported, not swallowed, because "no linter configured" and "the lint passed" are different
/// facts. Pure — tested.
///
/// Order, per gate: the Node script (most specific — the project wrote it), then a Makefile target (an explicit
/// entry point the project declared), then the language toolchain's own verb. A gate only reaches the last one
/// in a repo that has no script and no Makefile, where `cargo test` or `go build` IS how that project is built.
pub fn gate_cmd(gate: Gate, p: &Project, files: &[String]) -> Result<String, String> {
    // THE LAWS GATE IS NOT A COMMAND. It reads the changed files in process and judges them against X-ray's own
    // per-file laws — there is nothing to shell out to, and inventing a command string for it would put a lie
    // in the `cmd` column of the run. `run_one` routes it before ever asking for one.
    if gate == Gate::Laws { return Err("the laws gate runs in process, not as a command".into()) }
    let mut why: Vec<String> = vec![];

    // ── the project's own npm script ────────────────────────────────────────────────────────────────────────
    if let Some(pkg_json) = p.pkg.as_deref() {
        if gate == Gate::Test { return crate::goals::qa_test_cmd(Some(pkg_json), files, p.node_modules, &p.lock) }
        let pkg: Value = serde_json::from_str(pkg_json).map_err(|_| "package.json is not readable".to_string())?;
        let script = |k: &str| pkg.get("scripts").and_then(|s| s.get(k)).and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty());
        let dep = |k: &str| pkg.get("devDependencies").and_then(|d| d.get(k)).is_some() || pkg.get("dependencies").and_then(|d| d.get(k)).is_some();
        let pm = pm_of(&p.lock);
        let key = if gate == Gate::Build { "build" } else { "lint" };
        if script(key).is_some() || (gate == Gate::Lint && dep("eslint")) {
            if !p.node_modules { return Err(format!("the box's dependencies are not installed (no node_modules) — `{pm} install` in the box, then the gate can run")) }
            if script(key).is_some() { return Ok(format!("{pm} run {key}")) }
            // eslint installed but unscripted: lint the files this turn touched, not the whole repo, so a repo
            // with old warnings elsewhere does not fail a turn that never went near them
            let fs = crate::proc::quote_all(files);
            if fs.is_empty() { return Err("no changed files to lint".into()) }
            return Ok(format!("npx eslint {}", fs.join(" ")));
        }
        why.push(format!("no `{key}` script in package.json"));
    }

    // ── a Makefile target the project declared ─────────────────────────────────────────────────────────────
    if let Some(mk) = p.make.as_deref() {
        let t = gate.key();
        if make_target(mk, t) { return Ok(format!("make {t}")) }
        why.push(format!("no `{t}` target in the Makefile"));
    }

    // ── the language toolchain's own verb ──────────────────────────────────────────────────────────────────
    if p.cargo.is_some() {
        return match gate {
            Gate::Test => Ok("cargo test --quiet".into()),
            Gate::Build => Ok("cargo build --quiet".into()),
            // clippy is not part of every Rust install, and a gate that fails because a tool is missing reads
            // as the code being wrong. It runs where the project says it lints, and is skipped where it does not.
            Gate::Lint if p.clippy => Ok("cargo clippy --quiet --all-targets -- -D warnings".into()),
            Gate::Lint => Err("this Cargo project declares no lints (no clippy.toml, no [lints]) — nothing to run".into()),
            Gate::Laws => unreachable!("routed to run_laws"),
        };
    }
    if p.go {
        return Ok(match gate { Gate::Test => "go test ./...", Gate::Build => "go build ./...", Gate::Lint => "go vet ./...", Gate::Laws => unreachable!("routed to run_laws") }.into());
    }
    if let Some(py) = p.py.as_deref() {
        return match gate {
            Gate::Test if declares(py, "pytest") => Ok("python -m pytest -q".into()),
            Gate::Lint if declares(py, "ruff") => Ok("python -m ruff check .".into()),
            Gate::Lint if declares(py, "flake8") => Ok("python -m flake8".into()),
            Gate::Build => Err("a Python project has no build to prove".into()),
            _ => Err(format!("this Python project declares no {} tool — nothing to run", gate.key())),
        };
    }
    if p.dotnet { return Ok(match gate { Gate::Test => "dotnet test", Gate::Build => "dotnet build", Gate::Lint => "dotnet format --verify-no-changes", Gate::Laws => unreachable!("routed to run_laws") }.into()) }
    if p.gradle { return match gate { Gate::Laws => unreachable!("routed to run_laws"), Gate::Test => Ok("./gradlew test".into()), Gate::Build => Ok("./gradlew build".into()), Gate::Lint => Err("no lint task assumed for Gradle — add a `lint` target to a Makefile to run one".into()) } }
    if p.maven  { return match gate { Gate::Laws => unreachable!("routed to run_laws"), Gate::Test => Ok("mvn -q -B test".into()), Gate::Build => Ok("mvn -q -B package -DskipTests".into()), Gate::Lint => Err("no lint goal assumed for Maven — add a `lint` target to a Makefile to run one".into()) } }

    Err(if why.is_empty() { format!("nothing in the box says how to {} it — no package.json, Makefile, Cargo.toml, go.mod, pyproject.toml or solution file", gate.key()) }
        else { format!("{} — and no other manifest in the box says how to {} it", why.join(", "), gate.key()) })
}

/// Run one gate in the box. The command is the project's own; `CI=1` so runners do not open a watcher.
pub async fn run_one(cwd: &str, gate: Gate, files: &[String]) -> GateRun {
    if gate == Gate::Laws { return run_laws(cwd, files).await }
    let cmd = match gate_cmd(gate, &probe(std::path::Path::new(cwd)), files) {
        Ok(c) => c, Err(why) => return GateRun::skip(gate, why),
    };
    let t0 = crate::cli::now_ms();
    let out = tokio::time::timeout(std::time::Duration::from_secs(900),
        crate::proc::shell(&cmd).current_dir(cwd).env("CI", "1").output()).await;
    match out {
        Ok(Ok(o)) => {
            let text = format!("{}\n{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
            GateRun { gate: gate.key().into(), cmd, passed: o.status.success(), exit: o.status.code().unwrap_or(-1), ms: crate::cli::now_ms() - t0, tail: tail_of(&text, 4000), skipped: None }
        }
        Ok(Err(e)) => GateRun { gate: gate.key().into(), cmd, passed: false, exit: -1, ms: crate::cli::now_ms() - t0, tail: format!("could not run: {e}"), skipped: None },
        Err(_) => GateRun { gate: gate.key().into(), cmd, passed: false, exit: -1, ms: crate::cli::now_ms() - t0, tail: "timed out after 15 min".into(), skipped: None },
    }
}
/// THE LAWS GATE, in process (operator 2026-09-25). Deterministic and quick: it reads the changed files and
/// runs X-ray's per-file laws over them. Seconds, because there is no subprocess and no model.
///
/// A skip carries its reason like every other gate — "no code laws written yet" and "no law was broken" are
/// opposite facts and the operator must be able to tell them apart.
pub async fn run_laws(cwd: &str, files: &[String]) -> GateRun {
    let t0 = crate::cli::now_ms();
    let (cwd, files) = (cwd.to_string(), files.to_vec());
    let out = tokio::task::spawn_blocking(move || {
        let laws = crate::laws::load(&cwd);
        // the file's text as it stands now — the gate judges what the agent left behind, not the patch
        let read: Vec<(String, String)> = files.iter()
            .filter_map(|f| std::fs::read_to_string(std::path::Path::new(&cwd).join(f)).ok().map(|b| (f.clone(), b)))
            .collect();
        crate::laws::verdict(&laws, &read).map(|b| (crate::laws::brief(&b), b.len()))
    }).await.unwrap_or_else(|_| Err("reading the box panicked".into()));
    let ms = crate::cli::now_ms() - t0;
    match out {
        Err(why) => GateRun::skip(Gate::Laws, why),
        Ok((brief, n)) => GateRun { gate: "laws".into(), cmd: "x-ray laws · per-file detectors".into(), passed: n == 0, exit: n as i32, ms, tail: brief, skipped: None },
    }
}

/// The END of the output, which is where every runner puts the failures. Pure.
pub fn tail_of(text: &str, n: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= n { return text.trim().to_string() }
    chars[chars.len() - n..].iter().collect::<String>().trim().to_string()
}

/// Run the enabled gates, in order, stopping at nothing — the operator wants all three verdicts, not the first.
pub async fn run_all(cwd: &str, gates: &[Gate], files: &[String]) -> Vec<GateRun> {
    let mut out = vec![];
    for g in gates { out.push(run_one(cwd, *g, files).await) }
    out
}

/// WHAT THE AGENT IS TOLD when a gate says no. Its own command, its own exit code, its own log — no paraphrase,
/// because the model fixes the error the compiler printed, not a summary of it. `None` when nothing failed.
/// Pure — tested.
pub fn rerun_brief(gates: &[GateRun]) -> Option<String> {
    let bad: Vec<&GateRun> = gates.iter().filter(|g| g.failed()).collect();
    if bad.is_empty() { return None }
    let which = bad.iter().map(|g| g.gate.as_str()).collect::<Vec<_>>().join(" and ");
    let mut s = format!(
        "You said the work was done, and antiloki ran the project's own {which} to check. {} did not pass.\n\n\
         Fix the cause, not the symptom: do not delete or skip a failing test, do not silence the linter with an \
         ignore comment, and do not change the command. When it is genuinely a pre-existing failure your change \
         did not cause, say so plainly and leave it alone.\n",
        if bad.len() == 1 { "It" } else { "They" });
    for g in bad {
        s.push_str(&format!("\n--- {} · `{}` · exit {} ---\n{}\n", g.gate, g.cmd, g.exit, g.tail));
    }
    Some(s)
}

/// One line per gate for the deterministic log and the turn's story. Pure — tested.
pub fn gate_line(g: &GateRun) -> String {
    match &g.skipped {
        Some(why) => format!("{}: not run — {why}", g.gate),
        None if g.passed => format!("{}: passed · `{}` · {:.0} s", g.gate, g.cmd, g.ms as f64 / 1000.0),
        None => format!("{}: FAILED (exit {}) · `{}` · {:.0} s", g.gate, g.exit, g.cmd, g.ms as f64 / 1000.0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn f(v: &[&str]) -> Vec<String> { v.iter().map(|s| s.to_string()).collect() }

    /// A box described by its manifests. Every field defaults to absent, so each test states only what it means.
    fn node(pkg: &str, lock: &str) -> Project { Project { pkg: Some(pkg.into()), lock: lock.into(), node_modules: true, ..Default::default() } }
    fn q(p: &str) -> String { crate::proc::quote(p).unwrap() }

    #[test]
    fn a_gate_runs_the_projects_own_npm_script_or_says_why_it_cannot() {
        let files = f(&["src/a.ts"]);
        let pkg = r#"{"scripts":{"build":"vite build","lint":"eslint ."},"devDependencies":{"vitest":"^2"}}"#;
        assert_eq!(gate_cmd(Gate::Build, &node(pkg, "pnpm-lock.yaml"), &files), Ok("pnpm run build".into()));
        assert_eq!(gate_cmd(Gate::Lint, &node(pkg, "yarn.lock"), &files), Ok("yarn run lint".into()));
        assert_eq!(gate_cmd(Gate::Test, &node(pkg, ""), &files), Ok(format!("npx vitest related {} --run --passWithNoTests", q("src/a.ts"))));

        // NO SCRIPT IS NOT A PASS. A project with no build has no build to prove, and the gate says exactly that
        // rather than reporting green — "there is no linter here" and "the lint passed" are different facts.
        let bare = node(r#"{"devDependencies":{"vitest":"^2"}}"#, "");
        assert!(gate_cmd(Gate::Build, &bare, &files).unwrap_err().contains("no `build` script in package.json"));
        assert!(gate_cmd(Gate::Lint, &bare, &files).unwrap_err().contains("no `lint` script in package.json"));

        // eslint installed but unscripted: lint what THIS turn touched, not the whole repo
        let unscripted = node(r#"{"devDependencies":{"eslint":"^9"}}"#, "");
        assert_eq!(gate_cmd(Gate::Lint, &unscripted, &files), Ok(format!("npx eslint {}", q("src/a.ts"))));
        assert!(gate_cmd(Gate::Lint, &unscripted, &[]).is_err(), "nothing changed, nothing to lint");

        // deps missing is the operator's problem to fix, and the message says how
        let uninstalled = Project { node_modules: false, ..node(pkg, "") };
        assert!(gate_cmd(Gate::Build, &uninstalled, &files).unwrap_err().contains("npm install"));
    }

    /// THE WINDOWS REPORT (operator 2026-09-24): "antiloki didnt discovery how to run, build and test scripts
    /// automatically". Discovery read package.json and nothing else, so every gate in a repo that is not a Node
    /// project skipped with "no package.json in the box" — accurate, and useless.
    #[test]
    fn a_project_that_is_not_node_still_says_how_it_is_built() {
        let files = f(&["src/main.rs"]);
        let cargo = Project { cargo: Some("[package]\nname = \"x\"".into()), ..Default::default() };
        assert_eq!(gate_cmd(Gate::Test, &cargo, &files), Ok("cargo test --quiet".into()));
        assert_eq!(gate_cmd(Gate::Build, &cargo, &files), Ok("cargo build --quiet".into()));
        // clippy is not in every Rust install: a gate that fails because a TOOL is missing reads as the code
        // being wrong, so it runs only where the project itself says it lints.
        assert!(gate_cmd(Gate::Lint, &cargo, &files).unwrap_err().contains("declares no lints"));
        assert_eq!(gate_cmd(Gate::Lint, &Project { clippy: true, ..cargo.clone() }, &files),
                   Ok("cargo clippy --quiet --all-targets -- -D warnings".into()));

        let go = Project { go: true, ..Default::default() };
        assert_eq!(gate_cmd(Gate::Test, &go, &files), Ok("go test ./...".into()));
        assert_eq!(gate_cmd(Gate::Lint, &go, &files), Ok("go vet ./...".into()), "vet ships with go — it is always there");

        let py = |decl: &str| Project { py: Some(decl.into()), ..Default::default() };
        assert_eq!(gate_cmd(Gate::Test, &py("[tool.pytest.ini_options]"), &files), Ok("python -m pytest -q".into()));
        assert_eq!(gate_cmd(Gate::Lint, &py("[tool.ruff]"), &files), Ok("python -m ruff check .".into()));
        assert!(gate_cmd(Gate::Test, &py("[project]"), &files).unwrap_err().contains("declares no test tool"));

        assert_eq!(gate_cmd(Gate::Test, &Project { dotnet: true, ..Default::default() }, &files), Ok("dotnet test".into()));
        assert_eq!(gate_cmd(Gate::Test, &Project { maven: true, ..Default::default() }, &files), Ok("mvn -q -B test".into()));
        assert_eq!(gate_cmd(Gate::Build, &Project { gradle: true, ..Default::default() }, &files), Ok("./gradlew build".into()));

        // an empty box is still not a pass, and the message names what it looked for
        let nothing = gate_cmd(Gate::Build, &Project::default(), &files).unwrap_err();
        assert!(nothing.contains("Cargo.toml") && nothing.contains("go.mod"), "{nothing}");
    }

    /// THE TECHNOLOGY AXIS, which is not the domain axis (operator 2026-09-25: "different benchmarks for
    /// different technologies using the same tasks"). `roles::domain_of` says what KIND of work a change is,
    /// from the paths; this says what the project is BUILT IN, from its manifests. Both, or the router cannot
    /// tell a React frontend from a Rust backend, nor a Rust backend from a Rust CLI.
    #[test]
    fn the_stack_is_what_the_projects_own_manifests_say() {
        let node = |pkg: &str| Project { pkg: Some(pkg.into()), ..Default::default() };
        assert_eq!(stack_of(&node(r#"{"dependencies":{"react":"^19"}}"#)), "web");
        assert_eq!(stack_of(&node(r#"{"dependencies":{"svelte":"^5"}}"#)), "web");
        // a package.json without a UI framework is a node service, not a frontend — the manifest declares it
        assert_eq!(stack_of(&node(r#"{"dependencies":{"express":"^4"}}"#)), "node");

        assert_eq!(stack_of(&Project { cargo: Some("[package]".into()), ..Default::default() }), "rust");
        assert_eq!(stack_of(&Project { go: true, ..Default::default() }), "go");
        assert_eq!(stack_of(&Project { py: Some("[project]".into()), ..Default::default() }), "python");
        assert_eq!(stack_of(&Project { dotnet: true, ..Default::default() }), "dotnet");
        assert_eq!(stack_of(&Project { maven: true, ..Default::default() }), "jvm");

        // a repo that is two things says so rather than picking a winner — because that is what it is
        assert_eq!(stack_of(&Project { cargo: Some("[package]".into()), ..node(r#"{"dependencies":{"react":"1"}}"#) }), "mixed");
        // and a directory with no manifest at all claims nothing
        assert_eq!(stack_of(&Project::default()), "any");
    }

    #[test]
    fn a_makefile_target_is_an_entry_point_the_project_declared() {
        let files = f(&["a.c"]);
        let mk = "CC = gcc\n\nbuild: deps\n\t$(CC) -o x a.c\n\ntest:\n\t./x --selftest\n";
        assert!(make_target(mk, "build") && make_target(mk, "test"));
        assert!(!make_target(mk, "lint"), "a target that is not declared is not a target");
        assert!(!make_target("CFLAGS := -O2\nbuild_dir := out\n", "build"), "a variable assignment is not a target");
        assert!(!make_target("\tbuild: indented\n", "build"), "a recipe line is not a target");

        let p = Project { make: Some(mk.into()), ..Default::default() };
        assert_eq!(gate_cmd(Gate::Build, &p, &files), Ok("make build".into()));
        assert!(gate_cmd(Gate::Lint, &p, &files).unwrap_err().contains("no `lint` target"));

        // the npm script wins over the Makefile — it is the more specific statement of how THIS is built
        let both = Project { pkg: Some(r#"{"scripts":{"build":"vite build"}}"#.into()), node_modules: true, make: Some(mk.into()), ..Default::default() };
        assert_eq!(gate_cmd(Gate::Build, &both, &files), Ok("npm run build".into()));
        // …but a Node repo whose package.json has no lint script still reaches the Makefile below it
        let fallthrough = Project { pkg: Some(r#"{}"#.into()), node_modules: true, make: Some("lint:\n\truff check .\n".into()), ..Default::default() };
        assert_eq!(gate_cmd(Gate::Lint, &fallthrough, &files), Ok("make lint".into()));
    }

    /// THE FIXTURES (PLAN-THE-LEAN-YEARS I.4). Every test above builds a `Project` by hand, which means they
    /// all assert that `gate_cmd` reasons correctly about a struct I wrote — not that `probe` reads a real
    /// directory correctly. These six are real directories, and this is the only test that crosses that gap.
    ///
    /// They are also the reason the technology axis can be trusted: until they existed, every claim antiloki
    /// made about "your codebase" had been measured against one 19-file React app.
    #[test]
    fn the_fixtures_are_discovered_as_what_they_are() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().join("fixtures");
        assert!(root.is_dir(), "fixtures/ is missing — see fixtures/README.md");
        let at = |n: &str| probe(&root.join(n));
        let files = vec!["src/thing.ts".to_string()];

        // a UI framework in the manifest is a frontend; the same language without one is a service
        let react = at("fx-react");
        assert_eq!(stack_of(&react), "web");
        assert_eq!(stack_of(&at("fx-node-api")), "node");
        // …and both are Node, so both take the npm-script branch
        assert_eq!(gate_cmd(Gate::Build, &Project { node_modules: true, ..react.clone() }, &files), Ok("npm run build".into()));
        // with nothing installed the gate says exactly that, which is a correct answer and not a pass
        assert!(gate_cmd(Gate::Build, &react, &files).unwrap_err().contains("npm install"));

        // Cargo, and a project that DECLARES it lints — which is what makes clippy a gate rather than a guess
        let rust = at("fx-rust");
        assert_eq!(stack_of(&rust), "rust");
        assert!(rust.clippy, "fx-rust declares [lints.clippy] and probe must see it");
        assert_eq!(gate_cmd(Gate::Test, &rust, &files), Ok("cargo test --quiet".into()));
        assert_eq!(gate_cmd(Gate::Lint, &rust, &files), Ok("cargo clippy --quiet --all-targets -- -D warnings".into()));

        let py = at("fx-python");
        assert_eq!(stack_of(&py), "python");
        assert_eq!(gate_cmd(Gate::Test, &py, &files), Ok("python -m pytest -q".into()));
        assert_eq!(gate_cmd(Gate::Lint, &py, &files), Ok("python -m ruff check .".into()));

        let go = at("fx-go");
        assert_eq!(stack_of(&go), "go");
        assert_eq!(gate_cmd(Gate::Test, &go, &files), Ok("go test ./...".into()));

        // two stacks: it says so rather than picking a winner. And with no npm script for `test`, discovery
        // falls through to the Makefile target the project declared — the branch no synthetic Project reaches.
        let mixed = at("fx-mixed");
        assert_eq!(stack_of(&mixed), "mixed");
        assert!(mixed.make.is_some(), "fx-mixed ships a Makefile");
        assert_eq!(gate_cmd(Gate::Lint, &Project { node_modules: true, ..mixed.clone() }, &files), Ok("make lint".into()));
        // …and where the Makefile is silent too, the language's own verb answers
        assert_eq!(gate_cmd(Gate::Build, &Project { node_modules: true, ..mixed }, &files), Ok("cargo build --quiet".into()));
    }

    #[test]
    fn the_toggles_default_to_off_except_the_one_that_costs_nothing() {
        // the three that spend machine time stay off until asked for; the laws gate does not spend any
        assert_eq!(enabled_on(&json!({})), vec![Gate::Laws]);
        assert_eq!(enabled_on(&json!({ "gates": { "laws": false } })), vec![], "and it can still be switched off");
        assert_eq!(enabled_on(&json!({ "gates": { "test": true, "lint": true } })), vec![Gate::Test, Gate::Lint, Gate::Laws]);
        assert_eq!(enabled_on(&json!({ "gates": { "build": false } })), vec![Gate::Laws]);
        assert_eq!(enabled_on(&json!({ "gates": { "nonsense": true } })), vec![Gate::Laws], "a key that is not a gate turns nothing on");
    }

    #[test]
    fn the_brief_hands_the_agent_the_real_log_and_forbids_the_easy_fix() {
        let pass = GateRun { gate: "test".into(), cmd: "npx vitest".into(), passed: true, exit: 0, ms: 1200, tail: "ok".into(), skipped: None };
        let skipped = GateRun::skip(Gate::Lint, "no linter".into());
        assert!(rerun_brief(&[pass.clone(), skipped.clone()]).is_none(), "nothing failed — the agent is not bothered");
        assert!(!skipped.failed(), "a skipped gate is not a failure…");
        assert!(!skipped.passed, "…and it is not a pass either");

        let bad = GateRun { gate: "build".into(), cmd: "pnpm run build".into(), passed: false, exit: 2, ms: 9000, tail: "src/a.ts(3,7): error TS2304: Cannot find name 'foo'.".into(), skipped: None };
        let brief = rerun_brief(&[pass, bad, skipped]).unwrap();
        assert!(brief.contains("error TS2304"), "the compiler's own words reach the model, not a paraphrase");
        assert!(brief.contains("`pnpm run build`"));
        assert!(brief.contains("exit 2"));
        assert!(brief.contains("do not delete or skip a failing test"));
        assert!(brief.contains("say so plainly and leave it alone"), "a pre-existing failure has an honest way out");
        assert!(!brief.contains("npx vitest"), "a gate that passed is not in the brief");
    }

    #[test]
    fn the_log_line_tells_the_three_states_apart() {
        assert_eq!(gate_line(&GateRun { gate: "test".into(), cmd: "npx vitest".into(), passed: true, exit: 0, ms: 1400, tail: String::new(), skipped: None }), "test: passed · `npx vitest` · 1 s");
        assert_eq!(gate_line(&GateRun { gate: "build".into(), cmd: "npm run build".into(), passed: false, exit: 1, ms: 8000, tail: String::new(), skipped: None }), "build: FAILED (exit 1) · `npm run build` · 8 s");
        assert_eq!(gate_line(&GateRun::skip(Gate::Lint, "no linter".into())), "lint: not run — no linter");
    }

    #[test]
    fn the_tail_keeps_the_end_where_the_failures_are() {
        assert_eq!(tail_of("  short  ", 100), "short");
        let long: String = std::iter::repeat_n('x', 50).chain("THE ERROR".chars()).collect();
        let t = tail_of(&long, 12);
        assert!(t.ends_with("THE ERROR"), "{t}");
        assert_eq!(t.chars().count(), 12);
    }
}
