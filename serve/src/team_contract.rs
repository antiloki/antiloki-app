//! THE CONTRACT NODE (RFC 0014; operator 2026-11-16): "the contract binds all models to follow a plan. It is written by the leader, but enforced by the specialist node."
//! The leader writes it BEFORE the run as data in the recipe: the conventions every file follows, and for each file the names it exports. The node BINDS (its text goes into every coder prompt of the decompose node that names it) and ENFORCES (by code, over what was written):
//! every contract file exists, exports at least the listed names, imports resolve with their extension to files that export what is imported, tests use the project's test framework. A violation is a `failed` verdict carrying the exact lines.
use crate::team_decompose::{check_imports, export_names};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

/// ONE FUNCTION, decomposed by the LEADER (or a frontier planner), precisely enough that a small local model can write only its body: the exact signature, what it does, the shape it returns, examples, edge cases.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FnContract {
    /** the exact name and parameters: `add(title)` */ pub signature: String,
    #[serde(default)] pub does: String,
    /** the shape it returns: `number (a new unique id)`, `{id:number,title:string,done:boolean}[]` */ #[serde(default, skip_serializing_if = "String::is_empty")] pub returns: String,
    /** input -> output examples: `add('buy milk') -> 1` */ #[serde(default, skip_serializing_if = "Vec::is_empty")] pub examples: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")] pub edge_cases: Vec<String>,
    /** the functions this one must call (checked by code against the real body) */ #[serde(default, skip_serializing_if = "Vec::is_empty")] pub calls: Vec<String>,
}
impl FnContract {
    /// The name of the function: what stands before the `(`. Pure.
    pub fn name(&self) -> String { self.signature.split('(').next().unwrap_or("").trim().trim_start_matches("async ").trim_start_matches("function ").trim().to_string() }
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FileContract {
    /** the names the file exports (`default` for a default export); derived from `functions` when they are given */ #[serde(default)] pub exports: Vec<String>,
    /** what the file is for, one line (goes into the prompts) */ #[serde(default, skip_serializing_if = "String::is_empty")] pub purpose: String,
    /** the functions of the file, decomposed (a function-level contract) */ #[serde(default, skip_serializing_if = "Vec::is_empty")] pub functions: Vec<FnContract>,
}
impl FileContract {
    /// What the file must export: the functions' names when it has them, else the listed names. Pure.
    pub fn export_list(&self) -> Vec<String> { if self.functions.is_empty() { self.exports.clone() } else { self.functions.iter().map(|f| f.name()).collect() } }
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ContractSpec {
    /** rules every file follows, in words the coder reads (module system, import style, naming) */ #[serde(default)] pub conventions: Vec<String>,
    /** the files of the plan and what each exports */ #[serde(default)] pub files: BTreeMap<String, FileContract>,
    /** tests use `node:test` (checked by code) */ #[serde(default = "yes")] pub node_test: bool,
}
fn yes() -> bool { true }
pub fn check(sp: &ContractSpec) -> Vec<String> {
    let mut e = vec![];
    if sp.files.is_empty() && sp.conventions.is_empty() { e.push("a contract names at least a file or a convention".into()); }
    for (p, f) in &sp.files {
        if let Err(x) = crate::team_apply::clean(p) { e.push(x); } if f.exports.iter().any(|n| n.trim().is_empty()) { e.push(format!("{p}: an export name is empty")); }
        let mut seen: Vec<String> = vec![];
        for fnc in &f.functions {
            let ok = { let s = fnc.signature.trim(); s.contains('(') && s.ends_with(')') && !fnc.name().is_empty() && fnc.name().chars().all(|c| c.is_alphanumeric() || c == '_' || c == '$') };
            if !ok { e.push(format!("{p}: `{}` is not a signature like `name(params)`", fnc.signature)); continue; }
            if fnc.does.trim().is_empty() { e.push(format!("{p}: `{}` says what it does", fnc.name())); }
            if seen.contains(&fnc.name()) { e.push(format!("{p}: `{}` twice", fnc.name())); } seen.push(fnc.name());
        }
    }
    e
}
/// The contract as the coder reads it: the conventions, then each file with its functions in full. Pure.
pub fn text(sp: &ContractSpec) -> String {
    let mut t = sp.conventions.iter().map(|c| format!("- {c}")).collect::<Vec<_>>();
    for (p, f) in &sp.files {
        let ex = f.export_list(); t.push(format!("- {p}{}: exports {}", if f.purpose.is_empty() { String::new() } else { format!(" ({})", f.purpose) }, if ex.is_empty() { "(per the plan)".to_string() } else { ex.join(", ") }));
        for fnc in &f.functions {
            t.push(format!("  - `{}`: {}", fnc.signature, fnc.does)); if !fnc.returns.is_empty() { t.push(format!("    returns {}", fnc.returns)); }
            for e in &fnc.examples { t.push(format!("    example: {e}")); } for e in &fnc.edge_cases { t.push(format!("    edge case: {e}")); } if !fnc.calls.is_empty() { t.push(format!("    must call: {}", fnc.calls.join(", "))); }
        }
    }
    t.join("\n")
}
/// The contract as ONE file's coder reads it: this file in full, every other file only by the names it exports (what it may import). A smaller prompt for a small model. Pure.
pub fn text_for(sp: &ContractSpec, path: &str) -> String {
    let mut t = sp.conventions.iter().map(|c| format!("- {c}")).collect::<Vec<_>>();
    for (p, f) in &sp.files {
        let ex = f.export_list();
        if p == path { t.push(format!("- {p}{} (THIS FILE): exports {}", if f.purpose.is_empty() { String::new() } else { format!(" ({})", f.purpose) }, ex.join(", ")));
            for fnc in &f.functions { t.push(format!("  - `{}`: {}", fnc.signature, fnc.does)); if !fnc.returns.is_empty() { t.push(format!("    returns {}", fnc.returns)); } for e in &fnc.examples { t.push(format!("    example: {e}")); } for e in &fnc.edge_cases { t.push(format!("    edge case: {e}")); } if !fnc.calls.is_empty() { t.push(format!("    must call: {}", fnc.calls.join(", "))); } }
        } else if !is_test_path(p) { t.push(format!("- {p} exports {} (you may import these)", if ex.is_empty() { "(per the plan)".to_string() } else { ex.join(", ") })); }
    }
    t.join("\n")
}
fn is_test_path(p: &str) -> bool { let l = p.to_lowercase(); l.contains("test") || l.contains("spec") }
/// The stub of a file the contract decomposes into functions, rendered by CODE (no model): every function with its exact signature, a doc comment from the contract and the TODO body. None when the file has no functions or is not JavaScript. Pure.
pub fn stub_of(path: &str, fc: &FileContract) -> Option<String> {
    if fc.functions.is_empty() || !matches!(path.rsplit('.').next(), Some("js" | "mjs" | "ts")) { return None; }
    let mut out = String::new();
    for f in &fc.functions {
        out.push_str("/**\n"); out.push_str(&format!(" * {}\n", if f.does.is_empty() { f.name() } else { f.does.replace('\n', " ") }));
        if !f.returns.is_empty() { out.push_str(&format!(" * @returns {}\n", f.returns)); }
        for e in &f.examples { out.push_str(&format!(" * @example {}\n", e.replace("*/", "* /"))); } for e in &f.edge_cases { out.push_str(&format!(" * Edge case: {}\n", e.replace("*/", "* /"))); }
        out.push_str(" */\n"); out.push_str(&format!("export function {}\n{{ throw new Error(\"TODO\"); }}\n\n", f.signature.trim().trim_start_matches("async ").trim_start_matches("function ").trim()).replace("\n{ throw", " { throw"));
    }
    Some(out)
}
/// Every non-test file of the contract is decomposed into functions: the plan is the contract itself and no planner is needed. Pure.
pub fn fully_specified(sp: &ContractSpec) -> bool { let src: Vec<_> = sp.files.iter().filter(|(p, _)| { let l = p.to_lowercase(); !(l.contains("test") || l.contains("spec")) }).collect(); !src.is_empty() && src.iter().all(|(_, f)| !f.functions.is_empty()) }
/// What a file the contract names must export, if the contract says. Pure.
pub fn declared(sp: &ContractSpec, path: &str) -> Option<Vec<String>> { sp.files.get(path).map(|f| f.export_list()).filter(|e| !e.is_empty()) }
/// One file against the contract: it exports what the contract says and its imports hold. `known` gives the export names of another file (the contract first, then the tree). Pure over `known`.
pub fn enforce_file(sp: &ContractSpec, path: &str, content: &str, known: &dyn Fn(&str) -> Option<Vec<String>>) -> Result<(), String> {
    if let Some(want) = declared(sp, path) { let have = export_names(content); let missing: Vec<&String> = want.iter().filter(|w| !have.contains(w)).collect(); if !missing.is_empty() { return Err(format!("the contract says `{path}` exports {}; it does not export {}", want.join(", "), missing.iter().map(|m| m.as_str()).collect::<Vec<_>>().join(", "))); } }
    let both = |t: &str| declared(sp, t).or_else(|| known(t));
    check_imports(path, content, &both, sp.node_test)
}
/// Every problem of the tree against the contract, one line each (path first, so a recurse node can blame the file). Reads `dir`.
pub fn enforce_tree(dir: &Path, sp: &ContractSpec) -> Vec<String> {
    let read = |p: &str| std::fs::read_to_string(dir.join(p)).ok();
    let known = |t: &str| read(t).map(|c| export_names(&c));
    let mut issues = vec![];
    for (p, _) in &sp.files { match read(p) { None => issues.push(format!("{p}: the contract names this file and it does not exist")), Some(c) => if let Err(e) = enforce_file(sp, p, &c, &known) { issues.push(format!("{p}: {e}")); } } }
    issues
}

#[cfg(test)]
mod tests {
    use super::*;
    fn spec() -> ContractSpec { ContractSpec { conventions: vec!["ES modules, named exports".into()], files: [("src/a.js".to_string(), FileContract { exports: vec!["add".into(), "sub".into()], purpose: "arithmetic".into(), functions: vec![] }), ("test/a.test.js".to_string(), FileContract { exports: vec![], purpose: String::new(), functions: vec![] })].into_iter().collect(), node_test: true } }
    #[test] fn the_contract_is_read_by_the_coder_and_checked_by_code() {
        let sp = spec(); assert!(check(&sp).is_empty() && !check(&ContractSpec { conventions: vec![], files: Default::default(), node_test: true }).is_empty());
        let t = text(&sp); assert!(t.contains("named exports") && t.contains("src/a.js (arithmetic): exports add, sub"));
        let none = |_: &str| None;
        assert!(enforce_file(&sp, "src/a.js", "export function add(a, b) { return a + b; }\nexport const sub = (a, b) => a - b;\n", &none).is_ok());
        assert!(enforce_file(&sp, "src/a.js", "export function add(a, b) { return a + b; }\n", &none).unwrap_err().contains("does not export sub"));
        let good = "import { test } from 'node:test'; import assert from 'node:assert/strict'; import { add, sub } from '../src/a.js';\n"; assert!(enforce_file(&sp, "test/a.test.js", good, &none).is_ok());
        assert!(enforce_file(&sp, "test/a.test.js", "import { test } from 'node:test'; import a from '../src/a.js';\n", &none).unwrap_err().contains("no default export"));
        assert!(enforce_file(&sp, "test/a.test.js", "import { test } from 'node:test'; import { mul } from '../src/a.js';\n", &none).unwrap_err().contains("does not export `mul`"));
        assert!(enforce_file(&sp, "test/a.test.js", "import { add } from '../src/a';\n", &none).unwrap_err().contains("no file extension") || enforce_file(&sp, "test/a.test.js", "import { add } from '../src/a';\n", &none).unwrap_err().contains("node:test"));
        assert!(enforce_file(&sp, "test/a.test.js", "describe('x', () => { it('y', () => expect(1).toBe(1)); });\n", &none).unwrap_err().contains("node:test"));
    }
    #[test] fn a_function_level_contract_is_read_rendered_into_stubs_and_enforced_by_code() {
        let fc = FileContract { exports: vec![], purpose: "the store".into(), functions: vec![
            FnContract { signature: "createStore()".into(), does: "makes an empty store".into(), returns: "{ add, list }".into(), examples: vec!["createStore().list() -> []".into()], edge_cases: vec![], calls: vec![] },
            FnContract { signature: "async load(path)".into(), does: "loads".into(), returns: String::new(), examples: vec![], edge_cases: vec!["a missing file -> []".into()], calls: vec![] }] };
        assert_eq!(fc.export_list(), vec!["createStore".to_string(), "load".to_string()]);
        let sp = ContractSpec { conventions: vec![], files: [("src/todo.js".to_string(), fc.clone()), ("test/t.test.js".to_string(), FileContract { exports: vec![], purpose: String::new(), functions: vec![] })].into_iter().collect(), node_test: true };
        assert!(check(&sp).is_empty()); assert!(fully_specified(&sp), "the test file does not count");
        let t = text(&sp); assert!(t.contains("`createStore()`: makes an empty store") && t.contains("example: createStore().list() -> []") && t.contains("edge case: a missing file -> []"));
        let stub = stub_of("src/todo.js", &fc).unwrap(); assert!(stub.contains("export function createStore() { throw new Error(\"TODO\"); }") && stub.contains("export function load(path) { throw") && stub.contains("@example createStore().list() -> []"), "{stub}");
        crate::team_stubs::stub_ok("src/todo.js", &stub, &fc.export_list()).unwrap();
        assert!(stub_of("src/todo.py", &fc).is_none() && stub_of("src/a.js", &FileContract { exports: vec!["a".into()], purpose: String::new(), functions: vec![] }).is_none());
        let mut bad = sp.clone(); bad.files.get_mut("src/todo.js").unwrap().functions[0].signature = "createStore".into(); assert!(!check(&bad).is_empty()); let mut dup = sp.clone(); dup.files.get_mut("src/todo.js").unwrap().functions[1].signature = "createStore()".into(); assert!(check(&dup).iter().any(|e| e.contains("twice")));
        assert!(enforce_file(&sp, "src/todo.js", "export function createStore() { return 1; }\n", &|_| None).unwrap_err().contains("does not export load"));
    }
    #[test] fn one_files_coder_reads_its_own_functions_in_full_and_the_others_by_name() {
        let f = |n: &str| FileContract { exports: vec![], purpose: String::new(), functions: vec![FnContract { signature: format!("{n}(x)"), does: format!("does {n}"), returns: "string".into(), examples: vec![format!("{n}(1) -> 'a'")], edge_cases: vec![], calls: vec![] }] };
        let sp = ContractSpec { conventions: vec!["ESM".into()], files: [("src/a.js".to_string(), f("alpha")), ("src/b.js".to_string(), f("beta")), ("test/t.test.js".to_string(), f("zeta"))].into_iter().collect(), node_test: true };
        let t = text_for(&sp, "src/a.js"); assert!(t.contains("THIS FILE") && t.contains("example: alpha(1)") && t.contains("src/b.js exports beta (you may import these)") && !t.contains("example: beta") && !t.contains("zeta"), "{t}");
        assert!(text_for(&sp, "src/a.js").len() < text(&sp).len(), "smaller than the whole contract");
    }
    #[test] fn the_tree_is_checked_against_the_contract_file_by_file() {
        let d = std::env::temp_dir().join(format!("antiloki-ct-{}", std::process::id())); let _ = std::fs::remove_dir_all(&d); std::fs::create_dir_all(d.join("src")).unwrap(); std::fs::create_dir_all(d.join("test")).unwrap();
        std::fs::write(d.join("src/a.js"), "export function add(a, b) { return a + b; }\n").unwrap();
        let issues = enforce_tree(&d, &spec()); assert!(issues.iter().any(|i| i.starts_with("src/a.js:") && i.contains("does not export sub")) && issues.iter().any(|i| i.starts_with("test/a.test.js:") && i.contains("does not exist")), "{issues:?}");
        std::fs::write(d.join("src/a.js"), "export function add(a, b) { return a + b; }\nexport const sub = (a, b) => a - b;\n").unwrap();
        std::fs::write(d.join("test/a.test.js"), "import { test } from 'node:test'; import { add, sub } from '../src/a.js';\n").unwrap(); assert!(enforce_tree(&d, &spec()).is_empty());
    }
}
