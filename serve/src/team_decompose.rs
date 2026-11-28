//! THE DECOMPOSE NODE (RFC 0014): a task in, files out. A small local model does the two things code cannot: the PLAN (breaking an idea into parts) and the code of each file.
//! Everything else is plain code: the context (X-ray), the plan's contract, the order, the prompt of each file (its current content, the exports of the files already written), the extraction of the code from the reply, the syntax check, the write.
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::Path;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DecomposeSpec {
    /** the seat that plans, used only when the ask names no file (`local:<ollama tag>`, `endpoint:<alias>`, `claude-code:<model>`) */ pub planner: String,
    /** the seat that writes each file */ pub coder: String,
    /** seats tried when the coder's code does not pass the checks, each stronger than the one before */ #[serde(default, skip_serializing_if = "Vec::is_empty")] pub fallback: Vec<String>,
    #[serde(default = "six")] pub max_subtasks: u32,
    /** the files it may write (patterns); empty = anywhere inside the tree */ #[serde(default, skip_serializing_if = "Vec::is_empty")] pub owns: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub timeout_s: Option<u64>,
    /** a test node (the leader's tests): its files are shown to the coder as the acceptance spec unless the test node is `hidden` */ #[serde(default, skip_serializing_if = "Option::is_none")] pub tests: Option<String>,
    /** a review node (rules written by the leader): its rules are applied to every file this node writes, an error is one exact re-ask */ #[serde(default, skip_serializing_if = "Option::is_none")] pub review: Option<String>,
    /** a contract node (written by the leader) that binds this node: its text is in every coder prompt and code enforces it on every file */ #[serde(default, skip_serializing_if = "Option::is_none")] pub contract: Option<String>,
}
fn six() -> u32 { 6 }
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Subtask { pub path: String, pub what: String }
impl Subtask { pub fn line(&self) -> String { format!("{} :: {}", self.path, self.what) } }

const EXTS: [&str; 22] = ["js", "mjs", "cjs", "ts", "tsx", "jsx", "py", "rs", "go", "java", "rb", "php", "css", "html", "json", "md", "sh", "c", "cpp", "h", "toml", "yaml"];
/// The files the ask names (a token with a known extension), in the order named, without duplicates. Pure.
pub fn named_files(ask: &str) -> Vec<String> {
    let mut out: Vec<String> = vec![];
    for t in ask.split(|c: char| c.is_whitespace() || "()[]{}<>\"'`".contains(c)) {
        let t = t.trim_matches(|c: char| ",;:!?".contains(c)).trim_end_matches('.');
        let Some((stem, ext)) = t.rsplit_once('.') else { continue };
        if stem.is_empty() || !EXTS.contains(&ext) || !stem.chars().all(|c| c.is_ascii_alphanumeric() || "/_-.".contains(c)) { continue; }
        if crate::team_apply::clean(t).is_ok() && !out.iter().any(|o| o == t) { out.push(t.to_string()); }
    }
    out
}
/// `3. path :: what` and `3) path :: what` lose their number; a path that starts with a dot keeps it. Pure.
fn strip_number(l: &str) -> &str { let d = l.chars().take_while(|c| c.is_ascii_digit()).count(); if d > 0 { let r = &l[d..]; if let Some(r) = r.strip_prefix('.').or_else(|| r.strip_prefix(')')) { return r.trim_start(); } } l }
/// A reasoning wrapper some models put round their answer (`<think>…</think>`, also an empty one glued to the first line): removed before anything is parsed. Pure.
pub fn strip_think(text: &str) -> String {
    let mut s = text.to_string();
    while let Some(a) = s.find("<think>") { match s[a..].find("</think>") { Some(b) => s.replace_range(a..a + b + 8, ""), None => { s.truncate(a); break } } }
    s.replace("</think>", "").trim_start().to_string()
}
/// The plan the model gave: one `path :: what` line per file. Refused with the reason when it breaks the contract. Pure.
pub fn parse_plan(text: &str, max: u32) -> Result<Vec<Subtask>, String> {
    let mut out: Vec<Subtask> = vec![];
    for l in text.lines() {
        let l = l.trim().trim_start_matches(['-', '*', '•']).trim(); let l = strip_number(l); if l.is_empty() || l.starts_with("```") { continue; }
        let Some((p, w)) = l.split_once("::") else { continue };
        let p = p.trim().trim_matches('`'); let w = w.trim();
        // a sentence that only mentions `::` is prose, not a plan line: only something that looks like a path (no spaces, a dot or a slash) is one, and then it is held to the path rules
        if p.contains(char::is_whitespace) || !(p.contains('.') || p.contains('/')) { continue; }
        crate::team_apply::clean(p)?; if w.is_empty() { return Err(format!("`{p}` has no instruction after `::`")); }
        if out.iter().any(|s| s.path == p) { return Err(format!("`{p}` appears twice: one line per file")); }
        out.push(Subtask { path: p.to_string(), what: w.to_string() });
    }
    // THE TESTS ARE THE LEADER'S (a test node, written once before the run): a test file in the plan is dropped, never written by a model
    out.retain(|s| !is_test(&s.path));
    if out.is_empty() { return Err("no source file in the plan: answer with one `path :: what` line per SOURCE file (the leader writes the tests)".into()); }
    if out.len() as u32 > max { return Err(format!("{} files: at most {max}", out.len())); }
    Ok(out)
}
fn is_test(p: &str) -> bool { let l = p.to_lowercase(); l.contains("test") || l.contains("spec") }
/// Sources before their tests, otherwise the order given. Pure.
pub fn order(mut v: Vec<Subtask>) -> Vec<Subtask> { v.sort_by_key(|s| is_test(&s.path)); v }
/// The code in a reply: the first fenced block, else the whole text; a first line that only names the file is dropped. Pure.
pub fn extract_code(text: &str, path: &str) -> String {
    let body = if let Some(i) = text.find("```") { let rest = &text[i + 3..]; let rest = rest.split_once('\n').map_or("", |(_, r)| r); rest.split("```").next().unwrap_or(rest).to_string() } else { text.to_string() };
    let mut lines: Vec<&str> = body.lines().collect();
    if let Some(f) = lines.first() { let f = f.trim().trim_start_matches(['/', '#', '*', ' ']).trim().trim_end_matches(':').trim(); if f == path { lines.remove(0); } }
    let mut s = lines.join("\n"); s = s.trim_matches('\n').to_string(); if !s.is_empty() { s.push('\n'); } s
}
/// What a JavaScript or TypeScript file exports, one line each (signatures only): the contract the next files are written against. Pure.
pub fn exports_of(content: &str) -> Vec<String> {
    content.lines().filter_map(|l| { let t = l.trim_start(); if !t.starts_with("export ") { return None; } let cut = t.split(['{', '=']).next().unwrap_or(t).trim(); Some(if t.starts_with("export {") { t.chars().take(120).collect() } else { cut.chars().take(120).collect() }) }).collect()
}
/// A syntax check before anything is written. js/mjs/cjs via `node --check` (a temp .mjs for modules), python via `ast`; other languages are not checked (and the answer says so). Blocking.
pub fn check_syntax(path: &str, content: &str) -> Result<&'static str, String> {
    let ext = path.rsplit('.').next().unwrap_or("");
    let (prog, args, tmp_ext): (&str, Vec<String>, &str) = match ext {
        "js" | "mjs" => ("node", vec!["--check".into()], "mjs"), "cjs" => ("node", vec!["--check".into()], "cjs"),
        "py" => ("python3", vec!["-I".into(), "-c".into(), "import ast,sys;ast.parse(open(sys.argv[1]).read())".into()], "py"),
        _ => return Ok("not checked"),
    };
    let f = std::env::temp_dir().join(format!("antiloki-syn-{}-{}.{tmp_ext}", std::process::id(), content.len())); std::fs::write(&f, content).map_err(|e| e.to_string())?;
    let mut c = antiloki_rs::proc::cmd(prog); c.args(&args).arg(&f); let out = c.output();
    let _ = std::fs::remove_file(&f);
    match out { Ok(o) if o.status.success() => Ok("checked"), Ok(o) => Err(String::from_utf8_lossy(&o.stderr).lines().find(|l| l.contains("Error")).unwrap_or("syntax error").chars().take(240).collect()), Err(_) => Ok("not checked") }
}
/// The files the ask names that the plan leaves out: the contract the model's plan is held to. Pure.
pub fn missing_named(plan: &[Subtask], named: &[String]) -> Vec<String> { named.iter().filter(|n| !plan.iter().any(|s| &s.path == *n)).cloned().collect() }
/// What a JavaScript file exports, by NAME (`default` for a default export). Pure.
/// The names a plain script declares at the top level (column 0): `function f`, `async function f`, `const|let|var f =`, `class F`, and `window.f =` / `globalThis.f =`. Pure.
pub fn global_names(content: &str) -> Vec<String> {
    let mut out: Vec<String> = vec![]; let ident = |r: &str| -> String { r.chars().take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '$').collect() };
    for l in content.lines() {
        if l.starts_with(char::is_whitespace) { continue; }
        let r = l.strip_prefix("async ").unwrap_or(l);
        let name = ["function* ", "function ", "const ", "let ", "var ", "class "].iter().find_map(|kw| r.strip_prefix(kw).map(&ident)).or_else(|| ["window.", "globalThis."].iter().find_map(|p| r.strip_prefix(p).map(&ident)));
        if let Some(n) = name { if !n.is_empty() && !out.contains(&n) { out.push(n); } }
    }
    out
}
pub fn export_names(content: &str) -> Vec<String> {
    let mut out: Vec<String> = vec![];
    // A PLAIN BROWSER SCRIPT has no `export` at all: what it offers is what it declares at the top level (a global function or binding, or `window.x =`).
    if !content.lines().any(|l| l.trim_start().starts_with("export ")) { return global_names(content); }
    for l in content.lines() {
        let t = l.trim_start(); let Some(r) = t.strip_prefix("export ") else { continue };
        let r = r.trim_start();
        if r.starts_with("default") { out.push("default".into()); continue; }
        let r2 = r.strip_prefix("async ").unwrap_or(r);
        for kw in ["function* ", "function ", "const ", "let ", "var ", "class "] { if let Some(n) = r2.strip_prefix(kw) { let n: String = n.chars().take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '$').collect(); if !n.is_empty() { out.push(n); } break; } }
        if let Some(b) = r.strip_prefix('{') { for p in b.split('}').next().unwrap_or("").split(',') { let n = p.rsplit(" as ").next().unwrap_or(p).trim(); if !n.is_empty() { out.push(n.to_string()); } } }
    }
    out
}
/// The conventions of the project, found by code from its files, said in every coder prompt: the module system and the test framework. Pure over what was read.
pub fn conventions_of(package_json: &str, test_samples: &[String]) -> String {
    let esm = package_json.contains("\"type\"") && package_json.contains("\"module\"");
    let node_test = test_samples.iter().any(|t| t.contains("node:test")) || test_samples.is_empty();
    let mut c = vec![if esm { "ES modules (`import`/`export`); a relative import names the file WITH its extension, like `../src/a.js`; use named exports unless the plan says default".to_string() } else { "CommonJS or unknown module system: follow the existing files".to_string() }];
    let _ = node_test;
    c.join("\n")
}
/// The path an import names, relative to the importing file, normalised. None for a package import. Pure.
pub fn resolve_import(from: &str, spec: &str) -> Option<String> {
    if !spec.starts_with('.') { return None; }
    let mut parts: Vec<&str> = from.split('/').collect(); parts.pop();
    for p in spec.split('/') { match p { "." | "" => {}, ".." => { parts.pop(); } x => parts.push(x) } }
    Some(parts.join("/"))
}
/// The imports of a JavaScript file against what the other files export: the contract the plan binds every model to. `exports_of_file` gives a file's export names when it is known (None = unknown, not checked). Pure.
pub fn check_imports(path: &str, content: &str, exports_of_file: &dyn Fn(&str) -> Option<Vec<String>>, conv_node_test: bool) -> Result<(), String> {
    let is_test = is_test(path);
    if is_test && conv_node_test && !content.contains("node:test") { return Err("this project's tests use node:test: `import { test } from 'node:test'` and `import assert from 'node:assert/strict'`, not jest or mocha globals".into()); }
    for l in content.split(|c| c == '\n' || c == ';') {
        let t = l.trim(); let Some(rest) = t.strip_prefix("import ") else { continue };
        let Some((what, from)) = rest.rsplit_once(" from ") else { continue };
        let spec = from.trim().trim_end_matches(';').trim_matches(|c| c == '\'' || c == '"'); let Some(target) = resolve_import(path, spec) else { continue };
        if !target.rsplit('/').next().map_or(false, |f| f.contains('.')) { return Err(format!("`{spec}` has no file extension: write the whole file name, like `{spec}.js`")); }
        let Some(ex) = exports_of_file(&target) else { continue };
        let what = what.trim();
        if what.starts_with('*') { continue; }
        let (default_part, named_part) = match what.split_once('{') { Some((d, n)) => (d.trim().trim_end_matches(',').trim(), Some(n.split('}').next().unwrap_or(""))), None => (what, None) };
        if !default_part.is_empty() && !ex.iter().any(|e| e == "default") { return Err(format!("`{target}` has no default export (it exports: {}): import by name with `{{ … }}`", if ex.is_empty() { "nothing".to_string() } else { ex.join(", ") })); }
        if let Some(n) = named_part { for name in n.split(',') { let name = name.trim().split(" as ").next().unwrap_or("").trim(); if !name.is_empty() && !ex.iter().any(|e| e == name) { return Err(format!("`{target}` does not export `{name}` (it exports: {})", if ex.is_empty() { "nothing".to_string() } else { ex.join(", ") })); } } }
    }
    Ok(())
}
/// THE PLAN FROM A FUNCTION-LEVEL CONTRACT: the source files, in order, each with the functions to write. Pure.
pub fn plan_of_contract(c: &crate::team_contract::ContractSpec) -> Vec<Subtask> {
    order(c.files.iter().filter(|(p, _)| !is_test(p)).map(|(p, f)| Subtask { path: p.clone(), what: if f.functions.is_empty() { f.purpose.clone() } else { format!("write {}", f.functions.iter().map(|x| x.signature.clone()).collect::<Vec<_>>().join(", ")) } }).collect())
}
pub fn contract_prompt(ask: &str, overview: &str, conv: &str, max_files: u32) -> String {
    format!("You are the ARCHITECT of a small project. Decompose the task into SOURCE files (at most {max_files}; the leader writes the tests, never list a test file) and, for every file, the functions it exports, precisely enough that a small model can write ONE function body knowing nothing else. Answer with ONLY a JSON object, no text around it:\n{{\"files\": {{\"<path>\": {{\"purpose\": \"...\", \"functions\": [{{\"signature\": \"name(param1, param2)\", \"does\": \"what it does\", \"returns\": \"the exact shape it returns\", \"examples\": [\"name(1, 2) -> 3\"], \"edge_cases\": [\"what happens when ...\"]}}]}}}}}}\nExact names and parameter lists; the shape of every value that crosses a function boundary; at least one input -> output example per function; the edge cases (empty input, unknown filter, missing id).\n\nConventions: {conv}\n\nTask: {ask}\n\nWhat X-ray knows about the project:\n{overview}")
}
/// A contract drafted by a FRONTIER planner (JSON), checked by code: files and functions in the right shape, at most `max_files` files, no test files. Pure.
pub fn parse_contract(text: &str, conv: &str, max_files: u32) -> Result<crate::team_contract::ContractSpec, String> {
    let v = crate::team_llm::extract_json(text).ok_or("the reply had no JSON object")?; let files = v.get("files").filter(|f| f.is_object()).ok_or("the JSON has no `files` object")?;
    let c: crate::team_contract::ContractSpec = serde_json::from_value(json!({ "conventions": [conv], "files": files, "node_test": true })).map_err(|e| format!("a file or function is not in the shape: {e}"))?;
    let mut c = c; c.files.retain(|p, _| !is_test(p));
    if c.files.is_empty() { return Err("no source file in the contract".into()); } if c.files.len() as u32 > max_files { return Err(format!("{} files: at most {max_files}", c.files.len())); }
    let e = crate::team_contract::check(&c); if !e.is_empty() { return Err(e.join("; ")); }
    if c.files.values().any(|f| f.functions.is_empty()) { return Err("every file lists its functions".into()); }
    Ok(c)
}
pub fn planner_prompt(ask: &str, overview: &str, max: u32, note: &str) -> String {
    format!("You plan code changes for a small project. Split the task into the FILES to create or change. Answer with one line per file, exactly `path :: what to do in that file` (the what has the names, signatures and behaviour, precise enough to write the file alone). At most {max} lines. List SOURCE files only: the leader writes the tests, never put a test file in the plan. No other text.\n\nTask: {ask}\n\nWhat X-ray knows about the project:\n{overview}\n{note}")
}
pub fn coder_prompt(ask: &str, plan: &[Subtask], sub: &Subtask, current: &str, written: &[(String, Vec<String>)], conv: &str, tests_text: &str, note: &str) -> String {
    let wr = if written.is_empty() { "(none yet)".to_string() } else { written.iter().map(|(p, e)| format!("{p}: {}", if e.is_empty() { "(no exports)".to_string() } else { e.join("; ") })).collect::<Vec<_>>().join("\n") };
    format!("You write ONE file of a project. Reply with the COMPLETE new content of `{}` in a single fenced code block and nothing else.\n\nThe whole task: {ask}\nThe plan (every file):\n{}\n\nThis file: {}\nCurrent content of {} (keep what the task does not change):\n```\n{}\n```\n\nFiles already written for this task and what they export (import from these real paths):\n{wr}\n\nConventions and contract (they bind every file of the plan):\n{conv}{}\n\nUse `export` for what other files import.{note}", sub.path, plan.iter().map(|s| s.line()).collect::<Vec<_>>().join("\n"), sub.line(), sub.path, if current.trim().is_empty() { "(this file does not exist yet)" } else { current }, if tests_text.is_empty() { String::new() } else { format!("\n\nThe acceptance tests, written by the leader: your file must make them pass. Never edit them.\n```js\n{tests_text}\n```") })
}

/// What one run of the node did.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Done { pub contract: Option<crate::team_contract::ContractSpec>, pub subtasks: Vec<String>, pub files: Vec<String>, pub failed: Vec<Value>, pub seats: Vec<String>, pub usd: f64, pub checks: Vec<String> }

async fn ask_text(seat: &str, prompt: &str, timeout_s: u64, lctx: Option<crate::ledger::Ctx>) -> Result<(String, f64), String> {
    let s = crate::team_llm::parse_seat(seat)?; let (t, c, _) = crate::team_llm::call_plain(&s, prompt, 1200, timeout_s, lctx).await?; Ok((strip_think(&t), c))
}
/// One file: the coder, then the ladder; each reply must hold code that passes the syntax check, else the same seat is asked once more with the exact error. Returns the code, the seat that gave it and the cost.
async fn one_file(spec: &DecomposeSpec, rung: usize, prompt: &str, path: &str, guard: &(dyn Fn(&str) -> Result<(), String> + Sync), lctx: &Option<crate::ledger::Ctx>) -> Result<(String, String, f64, &'static str), String> {
    let (mut why, mut usd) = (vec![], 0.0); let t = spec.timeout_s.unwrap_or(400);
    for seat in std::iter::once(&spec.coder).chain(spec.fallback.iter()).skip(rung.min(spec.fallback.len())) {
        let mut asked = prompt.to_string();
        for round in 0..2 {
            let (text, c) = match ask_text(seat, &asked, t, lctx.clone()).await { Ok(x) => x, Err(e) => { why.push(format!("{seat}: {e}")); if round == 0 { continue } else { break } } }; usd += c;   // a slow or dropped call is asked once more before the ladder
            let code = extract_code(&text, path);
            if code.trim().is_empty() { why.push(format!("{seat}: no code in the reply")); asked = format!("{prompt}\n\nYour last reply had no code. Reply with the file in ONE fenced code block."); continue; }
            match check_syntax(path, &code).and_then(|how| guard(&code).map(|_| how)) { Ok(how) => return Ok((code, seat.clone(), usd, how)), Err(e) => { why.push(format!("{seat}: {e}")); if round == 0 { let hints = crate::team_recurse::hints_of(&e); asked = format!("{prompt}\n\nYour last answer was refused: {e}{}\nFix exactly that and reply with the whole file in ONE fenced code block.", if hints.is_empty() { String::new() } else { format!("\nWhat it means: {}", hints.join("; ")) }); } } }
        }
    }
    Err(why.join(" | "))
}

/// The node. `redo`: the subtasks of an earlier pass and what went wrong, to redo only those (the recurse node's call); then no plan is made. Writes into `dir`.
pub async fn run(port: u16, dir: &Path, ask: &str, spec: &DecomposeSpec, contract: Option<&crate::team_contract::ContractSpec>, review: Option<&crate::team_review::ReviewSpec>, tests: Option<&crate::team_test::TestSpec>, rung: usize, redo: Option<(Vec<Subtask>, String)>, lctx: Option<crate::ledger::Ctx>) -> Result<Done, String> {
    let mut done = Done::default(); let owns = |p: &str| spec.owns.is_empty() || crate::goals::owns_file(&spec.owns, p);
    let note = redo.as_ref().map(|(_, n)| format!("\n\nThe tests failed with this, and this file must fix it:\n{n}")).unwrap_or_default();
    let (plan, all) = match &redo {
        Some((subs, _)) => (subs.clone(), subs.clone()),
        None => {
            let overview = crate::team_xray::run(port, "antiloki_query", &json!({ "source": "bundle", "question": ask, "token_budget": 3000 })).await.unwrap_or_default();
            // 1. THE LEADER DECOMPOSED THE TASK (a function-level contract): the plan IS the contract, no planner
            if let Some(c) = contract.filter(|c| crate::team_contract::fully_specified(c)) { let p = plan_of_contract(c); (p.clone(), p) } else {
            // 2. A FRONTIER PLANNER (Haiku or the leader's seat) DRAFTS THE FUNCTION-LEVEL CONTRACT, code validates it
            let frontier = crate::team_llm::parse_seat(&spec.planner).map_or(false, |s| crate::team_llm::frontier(&s));
            let mut from_draft: Option<Vec<Subtask>> = None;
            if frontier {
                let conv0 = contract.map(crate::team_contract::text).unwrap_or_else(|| { let pkg = std::fs::read_to_string(dir.join("package.json")).unwrap_or_default(); conventions_of(&pkg, &[]) });
                let mut last = String::new();
                for _ in 0..2 {
                    let p = format!("{}{}", contract_prompt(ask, &overview, &conv0, spec.max_subtasks), if last.is_empty() { String::new() } else { format!("\n\nYour last answer was refused: {last}. Answer again with ONLY the JSON object.") });
                    match ask_text(&spec.planner, &p, spec.timeout_s.unwrap_or(400), lctx.clone()).await { Ok((txt, c)) => { done.usd += c; match parse_contract(&txt, &conv0, spec.max_subtasks) { Ok(c) => { let p = plan_of_contract(&c); done.contract = Some(c); from_draft = Some(p); break } Err(e) => last = e } } Err(e) => { last = e; break } }
                }
            }
            if let Some(p) = from_draft { (p.clone(), p) } else {
            // 3. THE LINE PLAN (a model breaks the idea into files; code holds it to its contract)
            let mut named: Vec<String> = named_files(ask).into_iter().filter(|p| !is_test(p)).collect(); if let Some(c) = contract { for p in c.files.keys().filter(|p| !is_test(p)) { if !named.contains(p) { named.push(p.clone()); } } }
            let plan = {
                let t = spec.timeout_s.unwrap_or(400); let mut last = String::new(); let mut got = None; let mut said = String::new();
                for round in 0..2 {
                    let mut hint = if named.is_empty() { String::new() } else { format!("\nThe task names these files: {}. The plan has a line for each.", named.join(", ")) };
                    if !last.is_empty() { hint.push_str(&format!("\nYour last plan was refused: {last}. Answer again.")); }
                    let p = planner_prompt(ask, &overview, spec.max_subtasks, &hint);
                    match ask_text(&spec.planner, &p, t, lctx.clone()).await {
                        Ok((txt, c)) => { done.usd += c; said = txt.chars().take(400).collect(); match parse_plan(&txt, spec.max_subtasks) { Ok(v) => { let m = missing_named(&v, &named); if m.is_empty() { got = Some(v); break } last = format!("it leaves out {}", m.join(", ")); } Err(e) => last = e } }
                        Err(e) => { last = e; if round == 1 { break } }
                    }
                }
                got.ok_or_else(|| format!("the plan did not meet its contract: {last} (the model said: {})", said.replace('\n', " / ")))?
            };
            (order(plan.clone()), order(plan))
            } }
        }
    };
    // the contract in force: the leader's, else the one the frontier planner drafted
    let drafted_owned: Option<crate::team_contract::ContractSpec> = done.contract.clone();
    let contract: Option<&crate::team_contract::ContractSpec> = contract.or(drafted_owned.as_ref());
    // THE STUBS, RENDERED BY CODE from a function-level contract (a file that is not there yet)
    if let Some(c) = contract { for (p, f) in &c.files { if let Some(code) = crate::team_contract::stub_of(p, f) { let to = dir.join(p); if !to.exists() || std::fs::read_to_string(&to).map_or(true, |t| t.trim().is_empty()) { if let Some(par) = to.parent() { let _ = std::fs::create_dir_all(par); } let _ = std::fs::write(&to, code); } } } }
    let tests_text: String = tests.filter(|t| !t.hidden).map(|t| t.files.iter().map(|(p, c)| format!("// {p}\n{c}")).collect::<Vec<_>>().join("\n").chars().take(3500).collect()).unwrap_or_default();
    done.subtasks = all.iter().map(|s| s.line()).collect();
    // THE BINDING: the leader's contract when there is one, else the conventions the project already has (found by code)
    let conv = match contract { Some(c) => crate::team_contract::text(c), None => { let pkg = std::fs::read_to_string(dir.join("package.json")).unwrap_or_default(); let mut samples: Vec<String> = vec![]; for d in ["test", "tests"] { for e in std::fs::read_dir(dir.join(d)).into_iter().flatten().flatten() { if samples.len() < 3 { if let Ok(t) = std::fs::read_to_string(e.path()) { samples.push(t.chars().take(4000).collect()); } } } } conventions_of(&pkg, &samples) } };
    let node_test = contract.map_or(conv.contains("node:test"), |c| c.node_test);
    let mut written: Vec<(String, Vec<String>)> = vec![];
    for sub in &plan {
        if !owns(&sub.path) { done.failed.push(json!({ "path": sub.path, "why": "not in the node's allow-list of paths" })); continue; }
        let current = std::fs::read_to_string(dir.join(&sub.path)).unwrap_or_default(); let current: String = current.chars().take(20_000).collect();
        let conv_here = match contract { Some(c) => crate::team_contract::text_for(c, &sub.path), None => conv.clone() };
        let prompt = coder_prompt(ask, &all, sub, &current, &written, &conv_here, &tests_text, &note);
        let stub = if crate::team_stubs::is_stub(&current) { Some(current.clone()) } else { None };
        let prompt = if stub.is_some() { format!("{prompt}\n\nThis file is a STUB: replace every `throw new Error(\"TODO\")` body with the real code. Keep every signature and every comment exactly as they are, and make each doc comment's examples true.") } else { prompt };
        let guard = |code: &str| -> Result<(), String> {
            if let Some(s) = &stub { crate::team_stubs::filled_ok(s, code)?; }
            if !matches!(sub.path.rsplit('.').next(), Some("js" | "mjs" | "ts")) { return Ok(()); }
            let known = |t: &str| written.iter().find(|(p, _)| p == t).map(|(_, e)| e.iter().map(|x| x.trim_start_matches("export ").split_whitespace().last().unwrap_or("").trim_matches(|c: char| !c.is_alphanumeric() && c != '_' && c != '$').to_string()).collect::<Vec<_>>()).or_else(|| std::fs::read_to_string(dir.join(t)).ok().map(|c| export_names(&c)));
            let known2 = |t: &str| std::fs::read_to_string(dir.join(t)).ok().map(|c| export_names(&c));
            let _ = known;
            // THE LEADER'S REVIEW RULES: an error is a refusal with its exact line; a warning is not
            if let Some(rv) = review { let errs: Vec<String> = crate::team_review::review_file(rv, &sub.path, code).into_iter().filter(|i| i.severity == "error").map(|i| i.line_text()).collect(); if !errs.is_empty() { return Err(format!("the review rules refuse this file: {}", errs.join("; "))); } }
            match contract { Some(c) => crate::team_contract::enforce_file(c, &sub.path, code, &known2), None => check_imports(&sub.path, code, &known2, node_test) }
        };
        match one_file(spec, rung, &prompt, &sub.path, &guard, &lctx).await {
            Ok((code, seat, usd, how)) => {
                done.usd += usd;
                match crate::team_apply::write(dir, &[(sub.path.clone(), code.clone())], &owns) { Ok(_) => { written.push((sub.path.clone(), exports_of(&code))); done.files.push(sub.path.clone()); done.seats.push(seat); done.checks.push(format!("{}: syntax {how}", sub.path)); } Err(e) => done.failed.push(json!({ "path": sub.path, "why": e })) }
            }
            Err(e) => done.failed.push(json!({ "path": sub.path, "why": e })),
        }
    }
    Ok(done)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn files_the_ask_names_are_found_in_order_without_noise() {
        assert_eq!(named_files("Add farewell to src/greet.js (like greet), and a test in test/greet.test.js."), vec!["src/greet.js", "test/greet.test.js"], "named_files finds every file; the node keeps the sources");
        assert!(named_files("say Goodbye, <name>! e.g. nicely").is_empty()); assert!(missing_named(&[Subtask { path: "a.js".into(), what: "x".into() }], &["a.js".into(), "b.js".into()]) == vec!["b.js".to_string()]); assert_eq!(parse_plan("2) .env.example :: x", 3).is_err(), true); assert_eq!(named_files("edit a.py, a.py and ../x.js and /etc/p.sh"), vec!["a.py"]);
    }
    #[test] fn a_plan_must_meet_its_contract_and_sources_come_before_tests() {
        let p = parse_plan("1. test/a.test.js :: tests for add\n- `src/a.js` :: export add(a,b)\n- src/b.js :: sub\n", 4).unwrap(); let o = order(p); assert_eq!(o.iter().map(|s| s.path.as_str()).collect::<Vec<_>>(), vec!["src/a.js", "src/b.js"], "the leader writes the tests: a test file in the plan is dropped"); assert!(parse_plan("test/a.test.js :: only a test", 4).is_err()); assert_eq!(parse_plan("We need to answer: one line per file, exactly `path :: what`.\nsrc/a.js :: add", 4).unwrap().len(), 1, "prose is skipped");
        for bad in ["", "no lines here", "src/a.js ::", "../x.js :: y", "a.js :: x\na.js :: y", ".git/c :: y", "We need one line per file, exactly `path :: what`"] { assert!(parse_plan(bad, 4).is_err(), "{bad}"); } assert!(parse_plan("a.js :: x\nb.js :: y\nc.js :: z", 2).is_err());
    }
    #[test] fn a_reasoning_wrapper_is_removed_before_parsing() {
        assert_eq!(strip_think("<think>\n</think>src/a.js :: x"), "src/a.js :: x"); assert_eq!(strip_think("<think>hmm</think>\nok"), "ok"); assert_eq!(strip_think("a </think>b"), "a b"); assert_eq!(strip_think("<think>never closed"), "");
        assert_eq!(parse_plan(&strip_think("<think>\n</think>src/a.js :: add\nsrc/b.js :: sub"), 4).unwrap().len(), 2);
    }
    #[test] fn the_code_comes_out_of_a_reply_and_the_exports_are_read() {
        assert_eq!(extract_code("Sure!\n```js\nexport const a = 1;\n```\nDone.", "src/a.js"), "export const a = 1;\n"); assert_eq!(extract_code("src/a.js:\nexport const a = 1;", "src/a.js"), "export const a = 1;\n");
        assert_eq!(extract_code("```javascript\n// src/a.js\nlet x;\n```", "src/a.js"), "let x;\n");
        assert_eq!(exports_of("export function greet(name) { return 1; }\nconst h = 2;\nexport const k = 3;\nexport { a, b };\n"), vec!["export function greet(name)", "export const k", "export { a, b };"]);
    }
    #[test] fn the_syntax_check_refuses_broken_code_and_says_when_it_cannot_check() {
        assert_eq!(check_syntax("a.js", "export const a = 1;\n").unwrap(), "checked"); assert!(check_syntax("a.js", "export const = ;").is_err()); assert_eq!(check_syntax("a.rs", "x").unwrap(), "not checked");
    }
    #[test] fn the_prompts_carry_the_task_the_file_and_what_was_already_written() {
        let plan = vec![Subtask { path: "src/a.js".into(), what: "add".into() }, Subtask { path: "test/a.test.js".into(), what: "test".into() }];
        let p = coder_prompt("do it", &plan, &plan[1], "", &[("src/a.js".into(), vec!["export function add(a, b)".into()])], "ES modules", "", ""); assert!(p.contains("test/a.test.js :: test") && p.contains("src/a.js: export function add(a, b)") && p.contains("does not exist yet"));
        assert!(planner_prompt("t", "ctx", 3, "").contains("At most 3"));
    }

    #[test]
    fn a_plain_script_offers_its_top_level_names_and_a_module_only_its_exports() {
        assert_eq!(export_names("function buildScenes(tl, stage) {\n  const x = 1;\n}\nconst helper = 2;\nwindow.extra = () => 1;\n"), ["buildScenes", "helper", "extra"]);
        assert_eq!(export_names("const hidden = 1;\nexport function shown() {}\n"), ["shown"], "a module keeps ES semantics");
        assert!(export_names("  function nested() {}\n").is_empty(), "an indented declaration is not top level");
    }
}
