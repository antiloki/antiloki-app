//! THE RECURSE NODE's judgement (RFC 0014): what plain code can tell from a failed judge, so no model is asked to read a failure.
//! It keeps the lines that say what broke, names the files the failure points at, and recognises the same failure coming back (no progress), which ends the recursion.
use serde_json::Value;

const MARKERS: [&str; 9] = ["error", "not ok", "fail", "expected", "actual", "✖", "assert", "cannot", "undefined"];
fn marked(l: &str) -> bool { let t = l.to_lowercase(); MARKERS.iter().any(|m| t.contains(m)) }
/// The lines of a judge's output that say what went wrong (at most 40, 1800 characters), for the worker to read. Pure.
pub fn feedback_of(tail: &str) -> String {
    let mut out: Vec<&str> = tail.lines().map(str::trim_end).filter(|l| marked(l)).take(40).collect(); if out.is_empty() { out = tail.lines().rev().take(12).collect::<Vec<_>>().into_iter().rev().collect(); }
    out.join("\n").chars().take(1800).collect()
}
/// A fingerprint of the failure that ignores numbers and where the files live: the same failure twice gives the same one. Pure.
pub fn signature_of(tail: &str) -> String {
    use std::hash::{Hash, Hasher};
    let norm: Vec<String> = tail.lines().filter(|l| marked(l)).map(|l| l.split_whitespace().map(|w| w.rsplit('/').next().unwrap_or(w)).map(|w| w.chars().map(|c| if c.is_ascii_digit() { '#' } else { c.to_ascii_lowercase() }).collect::<String>()).collect::<Vec<_>>().join(" ")).collect();
    if norm.is_empty() { return String::new(); } let mut h = std::collections::hash_map::DefaultHasher::new(); norm.hash(&mut h); format!("{:x}", h.finish())
}
/// The files (of those written) the failure mentions, by path or by file name. Pure.
pub fn blame(tail: &str, files: &[String]) -> Vec<String> {
    files.iter().filter(|f| { let base = f.rsplit('/').next().unwrap_or(f); tail.contains(f.as_str()) || (base.len() >= 4 && tail.contains(base)) }).cloned().collect()
}
/// The source of the test called `name` in a test file (`test('name', ...)` or `it(...)`), found by balancing the parentheses of the call. Pure.
pub fn test_block(src: &str, name: &str) -> Option<String> {
    for q in ['"', '\'', '`'] {
        let needle = format!("{q}{name}{q}"); let Some(at) = src.find(&needle) else { continue };
        let start = src[..at].rfind("test(").or_else(|| src[..at].rfind("it("))?;
        let mut depth = 0i32; let mut end = None;
        for (i, c) in src[start..].char_indices() { match c { '(' => depth += 1, ')' => { depth -= 1; if depth == 0 { end = Some(start + i + 1); break; } } _ => {} } }
        return Some(src[start..end.unwrap_or(src.len())].trim().chars().take(1500).collect());
    }
    None
}
/// The failing tests' own source, for the worker to read: only the ones that failed, never the whole file. Pure.
pub fn blocks_for(files: &std::collections::BTreeMap<String, String>, failed: &[String]) -> String {
    failed.iter().filter_map(|n| files.values().find_map(|s| test_block(s, n))).collect::<Vec<_>>().join("\n\n")
}
/// The function a failure points into: every `file:line` in the trace that names one of the written files is mapped to the function whose lines contain it. `read` gives a file's text. Pure.
pub fn blame_symbols(tail: &str, files: &[String], read: &dyn Fn(&str) -> Option<String>) -> Vec<(String, String, u32, u32)> {
    let mut out: Vec<(String, String, u32, u32)> = vec![];
    for f in files { let base = f.rsplit('/').next().unwrap_or(f); let Some(text) = read(f) else { continue }; let fns = crate::team_review::functions(&text);
        for l in tail.lines() { let Some(i) = l.find(&format!("{base}:")) else { continue }; let rest = &l[i + base.len() + 1..]; let line: u32 = rest.chars().take_while(|c| c.is_ascii_digit()).collect::<String>().parse().unwrap_or(0); if line == 0 { continue; }
            if let Some((n, a, b, _)) = fns.iter().find(|(_, a, b, _)| *a <= line && line <= *b) { let e = (f.clone(), n.clone(), *a, *b); if !out.contains(&e) { out.push(e); } } }
    }
    out
}
/// Said for the worker: which function and which lines the failure is inside. Pure.
pub fn symbol_note(b: &[(String, String, u32, u32)]) -> String { if b.is_empty() { String::new() } else { format!("The failure is inside: {}", b.iter().map(|(f, n, a, z)| format!("`{n}` in {f} (lines {a} to {z})")).collect::<Vec<_>>().join("; ")) } }
/// What common runtime errors mean and how they are fixed, said plainly and matched by code: the worker reads advice, not only the error. Pure.
pub fn hints_of(tail: &str) -> Vec<String> {
    let table: [(&str, &str); 9] = [
        ("has already been declared", "the same name is declared twice in one scope: in a `switch`, every `case` shares ONE scope, so two `const id` in two cases collide: wrap each case body in braces `{ }`, or use if / else, or use different names"),
        ("Assignment to constant variable", "a variable declared with `const` is assigned again: declare it with `let`, or build a new value instead of assigning"),
        ("is not a function", "something is called that is not a function: check the exact name, the import, and that the object has that method"),
        ("is not defined", "a name is used that was never declared or imported: declare it or import it from the right file"),
        ("Cannot read properties of undefined", "a value is undefined where an object is expected: return it from the function that makes it, and check argument positions"),
        ("does not provide an export named", "an import names something the other file does not export: use the exact exported name"),
        ("Cannot find module", "an import path is wrong: relative imports name the file with its extension, like `../src/a.js`"),
        ("Unexpected token", "a syntax error: check braces, parentheses and commas near the line named"),
        ("must be of type string", "a function returned undefined where text is expected: make it `return` the text"),
    ];
    table.iter().filter(|(k, _)| tail.contains(k)).map(|(k, v)| format!("{k}: {v}")).collect()
}
/// A judge passed when it says ok and (for the tests tool) its exit is 0. Pure.
pub fn judged_ok(out: &Value) -> bool { out["ok"] == Value::Bool(true) && out.get("exit").map_or(true, |e| e.as_i64() == Some(0)) }

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    const TAIL: &str = "▶ greet\n  ✔ greets by name (0.8ms)\n  ✖ farewell (1.2ms)\nAssertionError [ERR_ASSERTION]: Expected values to be strictly equal:\n+ actual - expected\n  at /tmp/work-123/test/greet.test.js:7:3\nℹ tests 2\n";
    #[test] fn the_feedback_keeps_the_lines_that_say_what_broke() { let f = feedback_of(TAIL); assert!(f.contains("farewell") && f.contains("AssertionError") && !f.contains("ℹ tests") && !f.contains("greets by name")); assert_eq!(feedback_of("a\nb\nc"), "a\nb\nc"); assert!(feedback_of(&"error x\n".repeat(500)).chars().count() <= 1800); }
    #[test] fn the_same_failure_has_the_same_signature_wherever_it_ran_and_a_different_one_differs() {
        let again = TAIL.replace("work-123", "work-999").replace("1.2ms", "9.9ms").replace(":7:3", ":8:5"); assert_eq!(signature_of(TAIL), signature_of(&again));
        assert_ne!(signature_of(TAIL), signature_of("✖ other thing broke\nError: nope")); assert_eq!(signature_of("all fine"), "");
    }
    #[test] fn the_files_a_failure_points_at_are_blamed_by_path_or_name() {
        let files = vec!["src/greet.js".to_string(), "test/greet.test.js".to_string(), "src/util.js".to_string()];
        assert_eq!(blame(TAIL, &files), vec!["test/greet.test.js".to_string()]); assert_eq!(blame("at src/util.js:3", &files), vec!["src/util.js".to_string()]); assert!(blame("nothing", &files).is_empty());
    }
    #[test] fn a_failing_tests_own_source_is_found_by_its_name() {
        let src = "import x;\ntest('store: add', () => { assert.equal(f(1), 2); });\ntest(\"cli: list\", () => {\n  assert.equal(run(['list']), '');\n});\n";
        assert_eq!(test_block(src, "store: add").unwrap(), "test('store: add', () => { assert.equal(f(1), 2); })"); assert!(test_block(src, "cli: list").unwrap().contains("run(['list'])") && !test_block(src, "cli: list").unwrap().contains("store: add")); assert!(test_block(src, "nope").is_none());
        let files: std::collections::BTreeMap<String, String> = [("t.js".to_string(), src.to_string())].into_iter().collect(); let b = blocks_for(&files, &["cli: list".to_string(), "missing".to_string()]); assert!(b.contains("cli: list") && !b.contains("store: add"));
    }
    #[test] fn a_trace_line_is_mapped_to_the_function_that_contains_it() {
        let src = "export function a() {\n  return 1;\n}\nexport function b(x) {\n  const y = x;\n  return y.z.q;\n}\n"; let read = |p: &str| (p == "src/m.js").then(|| src.to_string());
        let tail = "TypeError: Cannot read properties of undefined\n    at b (file:///tmp/w/src/m.js:6:12)\n    at other (file:///tmp/w/src/zzz.js:2:1)";
        let b = blame_symbols(tail, &["src/m.js".to_string()], &read); assert_eq!(b, vec![("src/m.js".to_string(), "b".to_string(), 4, 7)]); assert!(symbol_note(&b).contains("`b` in src/m.js (lines 4 to 7)") && symbol_note(&[]).is_empty());
        assert!(blame_symbols("at x (m.js:1:1)", &["src/m.js".to_string()], &read).iter().any(|s| s.1 == "a"));
    }
    #[test] fn common_runtime_errors_come_with_plain_advice() {
        assert!(hints_of("SyntaxError: Identifier 'id' has already been declared")[0].contains("wrap each case body in braces"));
        let h = hints_of("TypeError: Assignment to constant variable.\nError: x is not a function"); assert_eq!(h.len(), 2); assert!(h[0].contains("`let`") && h[1].contains("not a function")); assert!(hints_of("all fine").is_empty());
    }
    #[test] fn a_judge_passes_on_ok_and_a_zero_exit() { assert!(judged_ok(&json!({ "ok": true, "exit": 0 }))); assert!(judged_ok(&json!({ "ok": true }))); assert!(!judged_ok(&json!({ "ok": true, "exit": 1 }))); assert!(!judged_ok(&json!({ "ok": false, "exit": 0 }))); }
}
