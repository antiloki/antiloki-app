//! PORTABILITY LINT (PLAN-THE-TESTS phase 6). The serve ships on Windows, macOS and Linux, and it assumed POSIX more than once (`sh -c`, `which`, raw
//! `Command::new` with a program name only Unix resolves, single-quoted shell arguments): each of those was a real bug on someone's machine. Every child
//! process goes through `crate::proc` (which resolves `.cmd`/`.exe`, hides the console window and kills the tree). This scans the NON-TEST source of serve/src
//! and fails on a raw spawn or a shell-only construct, naming the file and line. (`cargo xwin check` proves it compiles for Windows; this proves it stays portable.)
use std::path::PathBuf;

fn sources() -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/src")).unwrap().flatten().map(|e| e.path()).filter(|p| p.extension().map_or(false, |x| x == "rs")).collect();
    v.sort(); v
}
/// the file's production code: every line EXCEPT the body of a `#[cfg(test)] mod ... { }` block (found by matching braces, string and char literals ignored),
/// so code that follows a test module - blueprint.rs has some - is still read
fn production(text: &str) -> Vec<(usize, &str)> {
    let lines: Vec<&str> = text.lines().collect();
    let mut out = vec![]; let mut i = 0;
    let braces = |l: &str| -> i32 {
        let mut d = 0; let mut in_str = false; let mut prev = ' ';
        for c in l.chars() {
            if c == '"' && prev != '\\' { in_str = !in_str }
            if !in_str { if c == '{' { d += 1 } if c == '}' { d -= 1 } }
            prev = c;
        }
        d
    };
    while i < lines.len() {
        if lines[i].trim_start().starts_with("#[cfg(test)]") && lines.get(i + 1).map_or(false, |n| n.trim_start().starts_with("mod ") || n.trim_start().starts_with("pub mod ")) {
            let mut depth = 0; let mut j = i + 1;
            while j < lines.len() { depth += braces(lines[j]); if depth <= 0 && lines[j].contains('}') { break } j += 1; }
            i = j + 1; continue
        }
        out.push((i + 1, lines[i])); i += 1;
    }
    out
}
fn is_comment(l: &str) -> bool { let t = l.trim_start(); t.starts_with("//") || t.starts_with("///") || t.starts_with("//!") }

/// the constructs that are not portable, as (needle, why)
const FORBIDDEN: &[(&str, &str)] = &[
    ("Command::new(", "spawn through crate::proc::cmd / tokio_cmd (it resolves .cmd/.exe shims, hides the console window, kills the tree)"),
    ("\"sh\", \"-c\"", "`sh -c` does not exist on Windows - build the argv"),
    ("\"bash\", \"-c\"", "`bash -c` does not exist on Windows - build the argv"),
    ("\"cmd\", \"/C\"", "cmd.exe is Windows only - go through crate::proc"),
    ("cmd(\"sh\")", "`sh` does not exist on Windows"),
    ("cmd(\"bash\")", "`bash` does not exist on Windows"),
    ("cmd(\"which\")", "`which` is not a program on Windows - use crate::proc::which"),
    ("cmd(\"where\")", "`where` is Windows only - use crate::proc::which"),
];

fn violations(name: &str, text: &str) -> Vec<String> {
    // proc.rs / proc_async.rs ARE the sanctioned wrappers
    if name == "proc.rs" || name == "proc_async.rs" { return vec![] }
    production(text).into_iter().filter(|(_, l)| !is_comment(l))
        .flat_map(|(n, l)| FORBIDDEN.iter().filter(move |(needle, _)| l.contains(needle)).map(move |(needle, why)| format!("{name}:{n}: `{needle}` - {why}")))
        .collect()
}

#[test]
fn no_source_file_spawns_a_process_or_a_shell_in_a_way_only_unix_understands() {
    let mut all = vec![];
    let files = sources();
    assert!(files.len() > 50, "the source walk found {} files - the walk is broken", files.len());
    for f in &files {
        let text = std::fs::read_to_string(f).unwrap();
        all.extend(violations(&f.file_name().unwrap().to_string_lossy(), &text));
    }
    assert!(all.is_empty(), "portability violations ({}):\n{}", all.len(), all.join("\n"));
}

#[test]
fn the_lint_itself_catches_what_it_is_for() {
    let bad = "fn a() {\n    let o = std::process::Command::new(\"git\").output();\n    let s = cmd(\"sh\").args([\"-c\", \"x\"]);\n}\n";
    assert_eq!(violations("x.rs", bad).len(), 2, "{:?}", violations("x.rs", bad));
    assert!(violations("proc.rs", bad).is_empty(), "the wrappers are exempt");
    assert!(violations("x.rs", "// Command::new(\"git\") is what proc replaces\nfn ok() {}\n").is_empty(), "comments are not code");
    assert!(violations("x.rs", "fn ok() {}\n#[cfg(test)]\nmod tests { fn t() { std::process::Command::new(\"git\"); } }\n").is_empty(), "test code may use git directly");
    // code AFTER a test module is still read (blueprint.rs has some): the block is skipped by its braces, not by "everything after the attribute"
    let after = "fn a() {}\n#[cfg(test)]\nmod tests {\n    fn t() { let s = \"}\"; }\n}\nfn later() { cmd(\"sh\"); }\n";
    assert_eq!(violations("x.rs", after).len(), 1, "{:?}", violations("x.rs", after));
}
