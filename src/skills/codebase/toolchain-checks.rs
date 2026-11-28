//! toolchain-checks.rs — THE CHECKS FOR EVERY LANGUAGE THE ATLAS PARSES (operator 2026-09-19:
//! "code checks, type checks and linters must exist for all supported languages — today only two").
//!
//! TypeScript (tsc) and Python (pyright) keep their own adapters. Every other parsed language —
//! Rust, Go, Java, C#, C, C++, PHP, Ruby, Kotlin, Swift, Scala, Lua, Elixir, Bash — is checked with
//! the toolchain it already ships with: the compiler's own syntax/type pass (`cargo check`, `go vet`,
//! `javac`, `dotnet build`, `gcc -fsyntax-only`, `php -l`, `ruby -c`, …) and the community linter
//! (`cargo clippy`, `golangci-lint`, `rubocop`, `phpcs`, `shellcheck`, …). Nothing is downloaded for
//! them: a tool is either on PATH or the row says how to get it (`install_hint`). Runs are project-
//! level in the workspace root, gated on the language's marker file (Cargo.toml, go.mod, …), and
//! every tool's plain-text output is parsed by the one `parse_diagnostics` below — the classic
//! `file:line:col: message`, `file(line,col): error CSxxxx: message` (dotnet) and `file:line: error:
//! message` (javac) shapes.
use serde_json::{json, Value};
use std::path::Path;

#[derive(Clone, Copy, Debug)]
pub struct Tool {
    pub id: &'static str,
    pub label: &'static str,
    pub lang: &'static str,
    pub kind: &'static str, // "check" | "lint"
    pub exts: &'static [&'static str],
    pub markers: &'static [&'static str], // any present at the root → the project speaks this language
    pub bin: &'static str,
    pub argv: &'static [&'static str],
    pub install_hint: &'static str,
}

pub const TOOLS: &[Tool] = &[
    Tool { id: "rust", label: "Rust · cargo check", lang: "rust", kind: "check", exts: &[".rs"], markers: &["Cargo.toml"], bin: "cargo", argv: &["check", "--message-format=short", "--quiet"], install_hint: "rustup: https://rustup.rs" },
    Tool { id: "clippy", label: "Rust · clippy", lang: "rust", kind: "lint", exts: &[".rs"], markers: &["Cargo.toml"], bin: "cargo", argv: &["clippy", "--message-format=short", "--quiet"], install_hint: "rustup component add clippy" },
    Tool { id: "go", label: "Go · go vet", lang: "go", kind: "check", exts: &[".go"], markers: &["go.mod"], bin: "go", argv: &["vet", "./..."], install_hint: "https://go.dev/dl" },
    Tool { id: "golangci-lint", label: "Go · golangci-lint", lang: "go", kind: "lint", exts: &[".go"], markers: &["go.mod"], bin: "golangci-lint", argv: &["run", "--out-format", "line-number", "./..."], install_hint: "go install github.com/golangci/golangci-lint/cmd/golangci-lint@latest" },
    Tool { id: "java", label: "Java · javac", lang: "java", kind: "check", exts: &[".java"], markers: &["pom.xml", "build.gradle", "build.gradle.kts"], bin: "javac", argv: &["-Xlint:all", "-d", ".antiloki/tmp-javac", "@.antiloki/javac-sources.txt"], install_hint: "a JDK (https://adoptium.net)" },
    Tool { id: "checkstyle", label: "Java · checkstyle", lang: "java", kind: "lint", exts: &[".java"], markers: &["pom.xml", "build.gradle", "build.gradle.kts"], bin: "checkstyle", argv: &["-c", "/google_checks.xml", "src"], install_hint: "https://checkstyle.org (brew install checkstyle)" },
    Tool { id: "dotnet", label: "C# · dotnet build", lang: "csharp", kind: "check", exts: &[".cs"], markers: &["*.csproj", "*.sln"], bin: "dotnet", argv: &["build", "--nologo", "-v", "q"], install_hint: "https://dotnet.microsoft.com/download" },
    Tool { id: "dotnet-format", label: "C# · dotnet format", lang: "csharp", kind: "lint", exts: &[".cs"], markers: &["*.csproj", "*.sln"], bin: "dotnet", argv: &["format", "--verify-no-changes", "--verbosity", "quiet"], install_hint: "ships with the .NET SDK" },
    Tool { id: "gcc", label: "C · gcc -fsyntax-only", lang: "c", kind: "check", exts: &[".c", ".h"], markers: &["Makefile", "CMakeLists.txt", "meson.build"], bin: "gcc", argv: &["-fsyntax-only", "@.antiloki/c-sources.txt"], install_hint: "gcc / clang (apt install build-essential · xcode-select --install)" },
    Tool { id: "gxx", label: "C++ · g++ -fsyntax-only", lang: "cpp", kind: "check", exts: &[".cpp", ".cc", ".cxx", ".hpp"], markers: &["Makefile", "CMakeLists.txt", "meson.build"], bin: "g++", argv: &["-fsyntax-only", "-std=c++17", "@.antiloki/cpp-sources.txt"], install_hint: "g++ / clang++" },
    Tool { id: "clang-tidy-c", label: "C · clang-tidy", lang: "c", kind: "lint", exts: &[".c", ".h"], markers: &["compile_commands.json"], bin: "clang-tidy", argv: &["-p", ".", "@.antiloki/c-sources.txt"], install_hint: "clang-tidy (apt install clang-tidy · brew install llvm)" },
    Tool { id: "clang-tidy", label: "C++ · clang-tidy", lang: "cpp", kind: "lint", exts: &[".cpp", ".cc", ".cxx", ".hpp"], markers: &["compile_commands.json"], bin: "clang-tidy", argv: &["-p", ".", "@.antiloki/cpp-sources.txt"], install_hint: "clang-tidy (apt install clang-tidy · brew install llvm)" },
    Tool { id: "php", label: "PHP · php -l", lang: "php", kind: "check", exts: &[".php"], markers: &["composer.json"], bin: "php", argv: &["-l"], install_hint: "https://www.php.net/downloads" },
    Tool { id: "phpcs", label: "PHP · phpcs", lang: "php", kind: "lint", exts: &[".php"], markers: &["composer.json"], bin: "phpcs", argv: &["--report=emacs", "."], install_hint: "composer global require squizlabs/php_codesniffer" },
    Tool { id: "ruby", label: "Ruby · ruby -c", lang: "ruby", kind: "check", exts: &[".rb"], markers: &["Gemfile"], bin: "ruby", argv: &["-c"], install_hint: "https://www.ruby-lang.org" },
    Tool { id: "rubocop", label: "Ruby · rubocop", lang: "ruby", kind: "lint", exts: &[".rb"], markers: &["Gemfile"], bin: "rubocop", argv: &["--format", "emacs"], install_hint: "gem install rubocop" },
    Tool { id: "kotlin", label: "Kotlin · kotlinc", lang: "kotlin", kind: "check", exts: &[".kt", ".kts"], markers: &["build.gradle.kts", "build.gradle", "settings.gradle.kts"], bin: "kotlinc", argv: &["-nowarn", "-d", ".antiloki/tmp-kotlinc", "@.antiloki/kotlin-sources.txt"], install_hint: "https://kotlinlang.org/docs/command-line.html (brew install kotlin)" },
    Tool { id: "ktlint", label: "Kotlin · ktlint", lang: "kotlin", kind: "lint", exts: &[".kt", ".kts"], markers: &["build.gradle.kts", "build.gradle"], bin: "ktlint", argv: &["--relative"], install_hint: "brew install ktlint · https://pinterest.github.io/ktlint" },
    Tool { id: "swift", label: "Swift · swiftc -typecheck", lang: "swift", kind: "check", exts: &[".swift"], markers: &["Package.swift"], bin: "swiftc", argv: &["-typecheck", "@.antiloki/swift-sources.txt"], install_hint: "Xcode / https://swift.org/install" },
    Tool { id: "swiftlint", label: "Swift · swiftlint", lang: "swift", kind: "lint", exts: &[".swift"], markers: &["Package.swift"], bin: "swiftlint", argv: &["lint", "--quiet"], install_hint: "brew install swiftlint" },
    Tool { id: "scala", label: "Scala · scalac", lang: "scala", kind: "check", exts: &[".scala"], markers: &["build.sbt"], bin: "scalac", argv: &["-d", ".antiloki/tmp-scalac", "@.antiloki/scala-sources.txt"], install_hint: "https://www.scala-lang.org/download (cs install scala3-compiler)" },
    Tool { id: "scalafmt", label: "Scala · scalafmt --test", lang: "scala", kind: "lint", exts: &[".scala"], markers: &["build.sbt", ".scalafmt.conf"], bin: "scalafmt", argv: &["--test"], install_hint: "cs install scalafmt" },
    Tool { id: "lua", label: "Lua · luac -p", lang: "lua", kind: "check", exts: &[".lua"], markers: &["*.rockspec", "init.lua", "main.lua"], bin: "luac", argv: &["-p"], install_hint: "https://www.lua.org (apt install lua5.4)" },
    Tool { id: "luacheck", label: "Lua · luacheck", lang: "lua", kind: "lint", exts: &[".lua"], markers: &["*.rockspec", "init.lua", "main.lua", ".luacheckrc"], bin: "luacheck", argv: &["--formatter", "plain", "--codes", "."], install_hint: "luarocks install luacheck" },
    Tool { id: "elixir", label: "Elixir · mix compile", lang: "elixir", kind: "check", exts: &[".ex", ".exs"], markers: &["mix.exs"], bin: "mix", argv: &["compile", "--warnings-as-errors", "--force"], install_hint: "https://elixir-lang.org/install.html" },
    Tool { id: "credo", label: "Elixir · mix credo", lang: "elixir", kind: "lint", exts: &[".ex", ".exs"], markers: &["mix.exs"], bin: "mix", argv: &["credo", "--format", "flycheck"], install_hint: "add {:credo, \"~> 1.7\", only: [:dev, :test]} to mix.exs" },
    Tool { id: "bash", label: "Shell · bash -n", lang: "bash", kind: "check", exts: &[".sh", ".bash"], markers: &["*.sh"], bin: "bash", argv: &["-n"], install_hint: "bash" },
    Tool { id: "shellcheck", label: "Shell · shellcheck", lang: "bash", kind: "lint", exts: &[".sh", ".bash"], markers: &["*.sh"], bin: "shellcheck", argv: &["--format=gcc"], install_hint: "apt install shellcheck · brew install shellcheck" },
];

pub fn tools_of_kind(kind: &str) -> Vec<&'static Tool> { let k = kind.to_string(); TOOLS.iter().filter(move |t| t.kind == k).collect() }
pub fn tool(id: &str) -> Option<&'static Tool> { TOOLS.iter().find(|t| t.id == id) }
pub fn tool_for_ext(kind: &str, ext: &str) -> Option<&'static Tool> { let e = ext.to_ascii_lowercase(); tools_of_kind(kind).into_iter().find(|t| t.exts.iter().any(|x| *x == e)) }

/// IS THIS TOOL RUNNABLE — resolved on the PATH, with no child process. Pure over the machine, no DB.
///
/// This used to probe `bin --version` (then `-v` / `-version` / `--help`) and accept ANY output as proof of
/// life. On Windows that probe ran through `cmd /C`, and `cmd` answers a name it cannot find by writing
/// *"'kotlinc' is not recognized as an internal or external command"* — **to stderr**. A non-empty stderr was
/// the success condition, so every MISSING tool reported installed. The Windows box that found this
/// (2026-09-25) claimed go, javac, gcc, g++, php, ruby, kotlinc, swiftc, scalac, luac, mix and bash were all
/// present when only cargo and dotnet were: twelve false positives, each one a spawn `run_tool` below then
/// made for nothing, and an `installed: true` the wizard showed the operator.
///
/// `proc::which` already answers exactly this question — it walks PATHEXT, so a `.cmd`/`.bat` shim is found
/// where `bin` alone is not, and it spawns nothing at all.
pub fn on_path(bin: &str) -> bool { crate::proc::which(bin).is_some() }

/// Does the project at `root` speak the tool's language? A marker file at the root (globs allowed:
/// `*.csproj`), or — for marker-less languages — any source file with the extension at the root level.
pub fn project_has(root: &Path, t: &Tool) -> bool {
    for m in t.markers {
        if let Some(suffix) = m.strip_prefix('*') {
            if let Ok(rd) = std::fs::read_dir(root) { if rd.flatten().any(|e| e.file_name().to_string_lossy().ends_with(suffix)) { return true; } }
        } else if root.join(m).exists() { return true; }
    }
    false
}

/// ONE parser for every tool's plain text. Recognized shapes, in order:
///   `file:line:col: severity: message`   gcc · clang · rustc(short) · go vet · shellcheck(gcc) · ruff-like
///   `file:line:col: message`             phpcs(emacs) · rubocop(emacs) · luacheck · credo(flycheck) · ktlint
///   `file(line,col): error CS1234: message`   dotnet
///   `file:line: error: message`          javac · php -l ("PHP Parse error: … in file on line N" handled too)
///   `file:line: message`                 luac · bash -n ("file: line N: message")
/// A line that matches none is ignored (progress noise). Pure, tested.
pub fn parse_diagnostics(text: &str, tool_id: &str) -> Vec<Value> {
    use regex::Regex;
    use std::sync::OnceLock;
    static RES: OnceLock<Vec<(Regex, [usize; 5])>> = OnceLock::new();
    // (regex, [file, line, col, severity, message]) — 0 = absent
    let res = RES.get_or_init(|| vec![
        (Regex::new(r"^\s*(?:PHP )?(?P<f>[^:\s(][^:(]*?):(?P<l>\d+):(?P<c>\d+):\s*(?P<s>error|warning|note|info|help|fatal|E|W|C|R|F|N)?[:\s]*(?P<m>.+)$").unwrap(), [1, 2, 3, 4, 5]),
        (Regex::new(r"^\s*(?P<f>[^:\s(][^:(]*?)\((?P<l>\d+),(?P<c>\d+)\):\s*(?P<s>error|warning)\s*(?P<m>.+)$").unwrap(), [1, 2, 3, 4, 5]),
        (Regex::new(r"^\s*(?P<f>[^:\s(][^:(]*?):(?P<l>\d+):\s*(?P<s>error|warning|note)?:?\s*(?P<m>.+)$").unwrap(), [1, 2, 0, 4, 5]),
        (Regex::new(r"^\s*(?:PHP )?(?P<s>Parse error|Fatal error|Warning):\s*(?P<m>.+?) in (?P<f>\S+) on line (?P<l>\d+)\s*$").unwrap(), [3, 4, 0, 1, 2]),
        (Regex::new(r"^\s*(?P<f>[^:\s]+): line (?P<l>\d+):\s*(?P<m>.+)$").unwrap(), [1, 2, 0, 0, 3]),
    ]);
    let sev_of = |s: &str| -> &'static str {
        let l = s.to_ascii_lowercase();
        if l.starts_with("warn") || l == "w" || l == "c" || l == "r" || l == "n" || l == "note" || l == "info" || l == "help" { "warning" } else { "error" }
    };
    let mut out = Vec::new();
    for line in text.lines() {
        if line.trim().is_empty() || line.starts_with("   ") && line.trim_start().starts_with("Compiling") { continue; }
        for (re, _) in res {
            if let Some(c) = re.captures(line) {
                let f = c.name("f").map(|m| m.as_str().trim().to_string()).unwrap_or_default();
                if f.is_empty() || f.starts_with("warning") || f.starts_with("error") { break; }
                let l = c.name("l").and_then(|m| m.as_str().parse::<i64>().ok());
                let col = c.name("c").and_then(|m| m.as_str().parse::<i64>().ok());
                let sev = c.name("s").map(|m| sev_of(m.as_str())).unwrap_or("error");
                let mut msg = c.name("m").map(|m| m.as_str().trim().to_string()).unwrap_or_default();
                // rustc's `error[E0308]: …` — the severity took `error`, the bracketed code leads the rest
                let code = if msg.starts_with('[') { msg.find("]:").map(|i| { let code = msg[1..i].to_string(); msg = msg[i + 2..].trim().to_string(); code }) } else { None };
                if msg.is_empty() { break; }
                out.push(json!({ "file": f, "line": l, "col": col, "severity": sev, "code": code.unwrap_or_else(|| tool_id.to_string()), "message": msg }));
                break;
            }
        }
    }
    out
}

/// Run one tool in `root`. `@.antiloki/<x>-sources.txt` argv entries are materialised from the
/// project's files of the tool's extensions (the compilers that take a file list); `-l` / `-c` / `-p`
/// single-file tools get every source file appended. Returns `{installed, ran, findings, log}`.
pub fn run_tool(root: &Path, t: &Tool, timeout_ms: u64) -> Value {
    if !on_path(t.bin) { return json!({ "installed": false, "ran": false, "findings": [], "reason": format!("{} not on PATH — {}", t.bin, t.install_hint) }); }
    let mut argv: Vec<String> = Vec::new();
    let sources = || -> Vec<String> { list_sources(root, t.exts, 4000) };
    let mut appended = false;
    for a in t.argv {
        if let Some(rest) = a.strip_prefix('@') {
            let list = root.join(rest);
            let _ = std::fs::create_dir_all(list.parent().unwrap_or(root));
            let _ = std::fs::write(&list, sources().join("\n"));
            argv.push(format!("@{}", list.to_string_lossy()));
            appended = true;
        } else { argv.push((*a).to_string()); }
    }
    if !appended && matches!(t.id, "php" | "ruby" | "lua" | "bash") { for s in sources() { argv.push(s); } }
    for d in [".antiloki/tmp-javac", ".antiloki/tmp-kotlinc", ".antiloki/tmp-scalac"] { if t.argv.iter().any(|a| *a == d) { let _ = std::fs::create_dir_all(root.join(d)); } }
    let r = crate::codebase__helpers_run_cmd::runCmd(&json!(t.bin), &json!(argv), &json!({ "cwd": root.to_string_lossy(), "timeoutMs": timeout_ms }));
    let stdout = r.get("stdout").and_then(Value::as_str).unwrap_or_default();
    let stderr = r.get("stderr").and_then(Value::as_str).unwrap_or_default();
    let combined = format!("{stdout}\n{stderr}");
    let findings = parse_diagnostics(&combined, t.id);
    json!({ "installed": true, "ran": true, "findings": findings, "exit": r.get("code").cloned().unwrap_or(Value::Null), "log": combined.chars().take(4000).collect::<String>() })
}

fn list_sources(root: &Path, exts: &[&str], cap: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(d) = stack.pop() {
        if out.len() >= cap { break; }
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            let name = e.file_name().to_string_lossy().to_string();
            if p.is_dir() {
                if !matches!(name.as_str(), "node_modules" | "target" | ".git" | "vendor" | "build" | "dist" | ".antiloki" | "bin" | "obj" | "_build" | "deps") { stack.push(p); }
            } else if exts.iter().any(|x| name.to_ascii_lowercase().ends_with(x)) {
                out.push(p.strip_prefix(root).map(|q| q.to_string_lossy().to_string()).unwrap_or_else(|_| p.to_string_lossy().to_string()));
            }
        }
    }
    out.sort();
    out
}

/// The catalog row of a tool for the settings screen: present? (marker or files), installed? (PATH).
pub fn describe(root: &Path, t: &Tool, file_count: i64) -> Value {
    json!({
        "id": t.id, "label": t.label, "lang": t.lang, "kind": t.kind, "via": "path", "bin": t.bin, "exts": t.exts,
        "install_hint": t.install_hint, "present": file_count > 0 || project_has(root, t), "file_count": file_count, "installed": on_path(t.bin),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tool_that_is_not_installed_is_not_reported_as_on_the_path() {
        // THE WINDOWS FALSE POSITIVE (2026-09-25). The old `cmd /C <bin> --version` probe accepted a
        // non-empty stderr as proof the tool existed, and that is exactly what cmd writes when it cannot
        // find the name. This assertion passed on Linux (direct spawn → ENOENT → false) and failed on the
        // platform the branch was written for, which is why it is here: it is the shape of the bug, not a
        // restatement of the fix.
        assert!(!on_path("antiloki-no-such-tool-6f3a9c"), "a name nothing installs must not resolve");
        // and the negative is not vacuous — the shell that ran this test is on the PATH by definition
        let present = if cfg!(windows) { "cmd" } else { "sh" };
        assert!(on_path(present), "{present} must resolve");
    }

    #[test]
    fn every_parsed_language_has_a_check_and_a_lint_and_extensions_never_collide_within_a_kind() {
        for lang in ["rust", "go", "java", "csharp", "c", "cpp", "php", "ruby", "kotlin", "swift", "scala", "lua", "elixir", "bash"] {
            assert!(tools_of_kind("check").iter().any(|t| t.lang == lang), "no check for {lang}");
            assert!(tools_of_kind("lint").iter().any(|t| t.lang == lang), "no lint for {lang}");
        }
        for kind in ["check", "lint"] {
            let mut seen: std::collections::HashMap<&str, &str> = Default::default();
            for t in tools_of_kind(kind) { for e in t.exts { if let Some(prev) = seen.insert(e, t.id) { assert!(prev == t.id, "{kind}: {e} claimed by {prev} and {}", t.id); } } }
        }
        assert_eq!(tool_for_ext("check", ".RS").map(|t| t.id), Some("rust"));
        assert_eq!(tool_for_ext("lint", ".sh").map(|t| t.id), Some("shellcheck"));
        assert!(tool("nope").is_none());
    }

    #[test]
    fn parse_diagnostics_reads_the_classic_shapes() {
        let text = "\
src/main.rs:12:5: error[E0308]: mismatched types
src/lib.rs:3:1: warning: unused import: `std::fs`
   Compiling foo v0.1.0
Program.cs(10,13): error CS0103: The name 'x' does not exist
Foo.java:7: error: cannot find symbol
PHP Parse error:  syntax error, unexpected '}' in /app/x.php on line 9
script.sh: line 4: syntax error near unexpected token `fi'
app/models/user.rb:5:3: C: Style/Frozen: missing magic comment
random progress line";
        let d = parse_diagnostics(text, "t");
        let rows: Vec<(String, i64, Option<i64>, String)> = d.iter().map(|x| (x["file"].as_str().unwrap().to_string(), x["line"].as_i64().unwrap(), x["col"].as_i64(), x["severity"].as_str().unwrap().to_string())).collect();
        assert_eq!(rows, vec![
            ("src/main.rs".into(), 12, Some(5), "error".into()),
            ("src/lib.rs".into(), 3, Some(1), "warning".into()),
            ("Program.cs".into(), 10, Some(13), "error".into()),
            ("Foo.java".into(), 7, None, "error".into()),
            ("/app/x.php".into(), 9, None, "error".into()),
            ("script.sh".into(), 4, None, "error".into()),
            ("app/models/user.rb".into(), 5, Some(3), "warning".into()),
        ]);
        assert_eq!(d[0]["message"], "mismatched types"); assert_eq!(d[0]["code"], "E0308"); // rustc's code is kept as the code
        assert_eq!(d[2]["code"], "t"); // no bracketed code → the tool id
        assert!(d[3]["message"].as_str().unwrap().contains("cannot find symbol"));
    }

    #[test]
    fn project_has_reads_marker_files_and_globs() {
        let dir = std::env::temp_dir().join(format!("antiloki-toolchain-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir); std::fs::create_dir_all(&dir).unwrap();
        assert!(!project_has(&dir, tool("rust").unwrap()));
        std::fs::write(dir.join("Cargo.toml"), "[package]").unwrap();
        assert!(project_has(&dir, tool("rust").unwrap()));
        std::fs::write(dir.join("App.csproj"), "<Project/>").unwrap();
        assert!(project_has(&dir, tool("dotnet").unwrap()));
        assert!(!project_has(&dir, tool("go").unwrap()));
        let r = run_tool(&dir, &Tool { id: "nope", label: "", lang: "x", kind: "check", exts: &[".x"], markers: &[], bin: "antiloki-no-such-binary-xyz", argv: &[], install_hint: "get it" }, 1000);
        assert_eq!(r["installed"], false); assert!(r["reason"].as_str().unwrap().contains("get it"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
