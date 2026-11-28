//! THE STUBS NODE (operator 2026-11-16: "a stub skeleton of the code, added to the contract"): a tiny model writes each contract file as a STUB (every exported function with its exact signature, a doc comment with examples, and a body that only throws TODO), code validates it, and the stubs go into the tree.
//! The decompose node then FILLS them: the coder sees the stub as the file's current content and a guard demands that every signature and comment survive and that no TODO is left. The names, the shapes and the examples are fixed BEFORE any logic is written.
use crate::team_decompose::{check_syntax, export_names, extract_code, strip_think};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::Path;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StubsSpec {
    /** the seat that writes the stubs (`local:<ollama tag>`, `endpoint:<alias>`, `claude-code:<model>`) */ pub author: String,
    /** seats tried when a stub does not pass the checks */ #[serde(default, skip_serializing_if = "Vec::is_empty")] pub fallback: Vec<String>,
    /** the contract node (written by the leader) that names the files and their exports */ pub contract: String,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub timeout_s: Option<u64>,
}
pub const TODO: &str = "throw new Error(\"TODO\")";
/// The header of each exported function (`export function add(title)`, `export const run = (argv, store) =>`), whitespace squeezed: what a stub must keep. Pure.
pub fn signatures_of(js: &str) -> Vec<String> {
    js.lines().filter_map(|l| { let t = l.trim_start(); let r = t.strip_prefix("export ")?; if !(r.starts_with("function") || r.starts_with("async function") || r.starts_with("const ") || r.starts_with("let ")) { return None; }
        let head = r.split('{').next().unwrap_or(r).split("=>").next().unwrap_or(r); Some(format!("export {}", head.split_whitespace().collect::<Vec<_>>().join(" ").trim_end_matches('=').trim())) }).collect()
}
/// The body of every function with its braces, found by balancing them (strings and comments are not parsed: a stub has none that matter). Pure.
fn bodies_of(js: &str) -> Vec<String> {
    let mut out = vec![]; let b = js.as_bytes(); let (mut i, mut depth, mut start) = (0, 0i32, None::<usize>);
    while i < b.len() { match b[i] { b'{' => { if depth == 0 { start = Some(i); } depth += 1; } b'}' => { depth -= 1; if depth == 0 { if let Some(s) = start.take() { out.push(js[s..=i].to_string()); } } } _ => {} } i += 1; }
    out
}
/// Without comments: line and block. Pure.
fn no_comments(s: &str) -> String {
    let mut out = String::new(); let mut it = s.chars().peekable();
    while let Some(c) = it.next() { if c == '/' && it.peek() == Some(&'/') { for d in it.by_ref() { if d == '\n' { out.push('\n'); break; } } } else if c == '/' && it.peek() == Some(&'*') { it.next(); let mut prev = ' '; for d in it.by_ref() { if prev == '*' && d == '/' { break; } prev = d; } } else { out.push(c); } }
    out
}
/// A stub file is valid when it parses, exports at least the names the contract lists, and every function body is only the TODO throw. Blocking (the syntax check runs `node`).
pub fn stub_ok(path: &str, content: &str, want: &[String]) -> Result<(), String> {
    check_syntax(path, content)?;
    let have = export_names(content); let missing: Vec<&String> = want.iter().filter(|w| !have.contains(w)).collect(); if !missing.is_empty() { return Err(format!("the stub must export {}", missing.iter().map(|m| m.as_str()).collect::<Vec<_>>().join(", "))); }
    let code = no_comments(content);
    for body in bodies_of(&code) { let inner: String = body.trim_start_matches('{').trim_end_matches('}').split_whitespace().collect::<Vec<_>>().join(" "); if !inner.starts_with("throw new Error(") || !inner.contains("TODO") || inner.matches(';').count() > 1 { return Err(format!("a stub body is only `{TODO};`, this one has code: `{}`", inner.chars().take(80).collect::<String>())); } }
    if !code.contains("TODO") { return Err("a stub body is `throw new Error(\"TODO\");`".into()); }
    Ok(())
}
/// A filled stub keeps every signature and comment and has no TODO left. `stub` is the file before, `filled` after. Pure.
pub fn filled_ok(stub: &str, filled: &str) -> Result<(), String> {
    let (a, b) = (signatures_of(stub), signatures_of(filled));
    if let Some(m) = a.iter().find(|s| !b.contains(s)) { return Err(format!("the signature `{m}` of the stub was changed or removed: keep every signature exactly")); }
    if no_comments(filled).contains("TODO") { return Err("a `throw new Error(\"TODO\")` body is still there: fill every stub".into()); }
    Ok(())
}
pub fn is_stub(content: &str) -> bool { no_comments(content).contains("TODO") }
pub fn author_prompt(ask: &str, path: &str, exports: &[String], contract_text: &str, overview: &str) -> String {
    format!("You write the STUB of ONE file. A stub has every exported function with its EXACT signature, a doc comment above each with what it does and one or two input -> output examples taken from the task, and the body `{TODO};` and nothing else. No logic. Reply with the whole file in a single fenced code block.\n\nThe task: {ask}\nThe file: {path}\nIt must export: {}\n\nThe contract (binds every file):\n{contract_text}\n\nWhat X-ray knows about the project:\n{overview}", if exports.is_empty() { "(per the task)".to_string() } else { exports.join(", ") })
}

/// What the node did.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Made { pub files: Vec<String>, pub failed: Vec<Value>, pub functions: Vec<String>, pub deterministic: Vec<String>, pub usd: f64 }
/// Write the stub of every non-test contract file into `dir`.
pub async fn run(port: u16, dir: &Path, ask: &str, spec: &StubsSpec, contract: &crate::team_contract::ContractSpec, lctx: Option<crate::ledger::Ctx>) -> Result<Made, String> {
    let mut made = Made::default(); let t = spec.timeout_s.unwrap_or(300);
    let overview = crate::team_xray::run(port, "antiloki_query", &json!({ "source": "bundle", "question": ask, "token_budget": 2000 })).await.unwrap_or_default();
    let ctext = crate::team_contract::text(contract);
    for (path, fc) in contract.files.iter().filter(|(p, _)| { let l = p.to_lowercase(); !(l.contains("test") || l.contains("spec")) }) {
        // THE LEADER DECOMPOSED THE FILE INTO FUNCTIONS: the stub is rendered by code, no model
        if let Some(code) = crate::team_contract::stub_of(path, fc) {
            match stub_ok(path, &code, &fc.export_list()) { Ok(()) => { let to = dir.join(path); if let Some(d) = to.parent() { let _ = std::fs::create_dir_all(d); } std::fs::write(&to, &code).map_err(|e| format!("{path}: {e}"))?; for s in signatures_of(&code) { made.functions.push(format!("{path}: {s}")); } made.files.push(path.clone()); made.deterministic.push(path.clone()); } Err(e) => made.failed.push(json!({ "path": path, "why": format!("the stub rendered from the contract was refused: {e}") })) }
            continue;
        }
        let prompt = author_prompt(ask, path, &fc.export_list(), &ctext, &overview); let mut why = vec![]; let mut done = None;
        'seats: for seat in std::iter::once(&spec.author).chain(spec.fallback.iter()) {
            let mut asked = prompt.clone();
            for round in 0..2 {
                let s = match crate::team_llm::parse_seat(seat) { Ok(s) => s, Err(e) => { why.push(e); continue 'seats } };
                let (text, c) = match crate::team_llm::call_plain(&s, &asked, 1800, t, lctx.clone()).await { Ok((x, c, _)) => (strip_think(&x), c), Err(e) => { why.push(format!("{seat}: {e}")); continue 'seats } }; made.usd += c;
                let code = extract_code(&text, path);
                match stub_ok(path, &code, &fc.export_list()) { Ok(()) => { done = Some(code); break 'seats } Err(e) => { why.push(format!("{seat}: {e}")); if round == 0 { asked = format!("{prompt}\n\nYour last stub was refused: {e}\nFix exactly that and reply with the whole file in ONE fenced code block."); } } }
            }
        }
        match done {
            Some(code) => { let to = dir.join(path); if let Some(d) = to.parent() { let _ = std::fs::create_dir_all(d); } std::fs::write(&to, &code).map_err(|e| format!("{path}: {e}"))?; for s in signatures_of(&code) { made.functions.push(format!("{path}: {s}")); } made.files.push(path.clone()); }
            None => made.failed.push(json!({ "path": path, "why": why.join(" | ") })),
        }
    }
    Ok(made)
}

#[cfg(test)]
mod tests {
    use super::*;
    const STUB: &str = "/** Adds a title. add('a') -> 1 */\nexport function add(title) { throw new Error(\"TODO\"); }\n/** Lists. */\nexport const list = (filter) => { throw new Error(\"TODO\"); };\n";
    #[test] fn a_stub_keeps_only_signatures_and_throws() {
        assert!(stub_ok("src/a.js", STUB, &["add".into(), "list".into()]).is_ok());
        assert!(stub_ok("src/a.js", STUB, &["add".into(), "remove".into()]).unwrap_err().contains("must export remove"));
        let logic = "export function add(t) { const x = 1; return x; }\n"; assert!(stub_ok("src/a.js", logic, &["add".into()]).is_err());
        assert!(stub_ok("src/a.js", "export function add(t) { throw new Error(\"TODO\")\n", &["add".into()]).is_err(), "it must parse");
        assert_eq!(signatures_of(STUB), vec!["export function add(title)".to_string(), "export const list = (filter)".to_string()]);
    }
    #[test] fn a_filled_stub_keeps_every_signature_and_has_no_todo_left() {
        let ok = "/** Adds a title. */\nexport function add(title) { return 1; }\n/** Lists. */\nexport const list = (filter) => { return []; };\n"; assert!(filled_ok(STUB, ok).is_ok());
        assert!(filled_ok(STUB, &ok.replace("add(title)", "add(t, extra)")).unwrap_err().contains("add(title)")); assert!(filled_ok(STUB, STUB).unwrap_err().contains("TODO"));
        assert!(is_stub(STUB) && !is_stub(ok) && !is_stub("// TODO later\nexport const a = 1;\n"), "a TODO in a comment is not a stub");
    }
    #[test] fn the_author_is_told_the_contract_and_the_file() { let p = author_prompt("do it", "src/a.js", &["add".into()], "- named exports", "ctx"); assert!(p.contains("src/a.js") && p.contains("It must export: add") && p.contains("named exports") && p.contains("TODO")); }
}
