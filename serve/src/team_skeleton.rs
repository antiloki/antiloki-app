//! THE SKELETON NODE (RFC 0014; operator 2026-11-16: "an AST node, the original AST merged with the new one, an AST diff"): the PLANNED CHANGE at symbol level, computed by code from the leader's function-level contract and the original code in git.
//! Three views of ONE structure: NEW-ONLY (what the task adds), MERGED (original + planned: the shape at the end, the best context for a small model) and DIFF (the plan against the original). After the coder has filled the stubs it also CHECKS conformance, by code:
//! every planned function exists and is exported, takes the planned number of parameters, calls the callees the leader named, nothing unplanned is exported, and a symbol marked `keep` is byte-identical to the original.
use crate::team_contract::{ContractSpec, FnContract};
use crate::team_review::functions;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SkeletonSpec { /** the contract node (written by the leader) whose function-level contract is the plan */ pub contract: String }
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Sym { pub name: String, pub signature: String, /** add | modify | keep */ pub op: String }
pub type Plan = BTreeMap<String, Vec<Sym>>;

fn is_js(p: &str) -> bool { matches!(p.rsplit('.').next(), Some("js" | "mjs" | "ts")) }
fn nparams(sig: &str) -> u32 { let inner = sig.split_once('(').map(|x| x.1).unwrap_or("").trim_end_matches(')').trim(); if inner.is_empty() { 0 } else { inner.split(',').count() as u32 } }
/// The plan: for each source file of the contract, its planned functions as add (not in the original) or modify (in it), and the original functions the contract does not name as keep. `original` gives a file's original text. Pure.
pub fn plan_of(c: &ContractSpec, original: &dyn Fn(&str) -> Option<String>) -> Plan {
    let mut out = Plan::new();
    for (p, f) in &c.files { if f.functions.is_empty() || !is_js(p) { continue; }
        let orig = original(p).map(|t| functions(&t)).unwrap_or_default(); let mut syms: Vec<Sym> = vec![];
        for fnc in &f.functions { syms.push(Sym { name: fnc.name(), signature: fnc.signature.clone(), op: if orig.iter().any(|o| o.0 == fnc.name()) { "modify" } else { "add" }.into() }); }
        for (n, _, _, np) in &orig { if !f.functions.iter().any(|x| &x.name() == n) { syms.push(Sym { name: n.clone(), signature: format!("{n}({})", (0..*np).map(|i| format!("p{i}")).collect::<Vec<_>>().join(", ")), op: "keep".into() }); } }
        out.insert(p.clone(), syms);
    }
    out
}
/// The three views of the plan as text. Pure.
pub fn views(plan: &Plan) -> (String, String, String) {
    let (mut new_only, mut merged, mut diff) = (String::new(), String::new(), String::new());
    for (p, syms) in plan {
        new_only.push_str(&format!("{p}\n")); merged.push_str(&format!("{p}\n")); diff.push_str(&format!("{p}\n"));
        for s in syms {
            if s.op != "keep" { new_only.push_str(&format!("  + {}\n", s.signature)); }
            merged.push_str(&format!("  {} {}\n", if s.op == "keep" { " " } else { "*" }, s.signature));
            diff.push_str(&format!("  {} {}\n", match s.op.as_str() { "add" => "+", "modify" => "~", _ => "=" }, s.signature));
        }
    }
    (new_only, merged, diff)
}
/// The text of one function from its first line to its last. Pure.
fn body_of(js: &str, name: &str) -> Option<String> { let lines: Vec<&str> = js.lines().collect(); functions(js).into_iter().find(|f| f.0 == name).map(|f| lines[(f.1 as usize - 1)..(f.2 as usize).min(lines.len())].join("\n")) }
/// One file against its planned functions. `orig` is the original text (None for a new file). Pure.
pub fn conform_file(path: &str, content: &str, planned: &[FnContract], orig: Option<&str>, plan: &[Sym]) -> Vec<String> {
    let mut out = vec![]; let fns = functions(content); let exports = crate::team_decompose::export_names(content);
    for fnc in planned {
        let n = fnc.name();
        let Some(f) = fns.iter().find(|f| f.0 == n) else { out.push(format!("{path}: [skeleton] the planned function `{n}` is not there")); continue };
        if !exports.contains(&n) { out.push(format!("{path}:{} [skeleton] `{n}` is planned as an export and is not exported", f.1)); }
        if f.3 != nparams(&fnc.signature) { out.push(format!("{path}:{} [skeleton] `{n}` takes {} parameters, the plan says {} (`{}`)", f.1, f.3, nparams(&fnc.signature), fnc.signature)); }
        if let Some(b) = body_of(content, &n) { for callee in &fnc.calls { if !b.contains(&format!("{callee}(")) && !b.contains(&format!("{callee}.")) { out.push(format!("{path}:{} [skeleton] `{n}` is planned to call `{callee}` and does not", f.1)); } } }
    }
    let planned_names: Vec<String> = planned.iter().map(|f| f.name()).collect();
    for e in &exports { if e != "default" && !planned_names.contains(e) && !plan.iter().any(|s| &s.name == e) { out.push(format!("{path}: [skeleton] `{e}` is exported and was not planned")); } }
    if let Some(o) = orig { for s in plan.iter().filter(|s| s.op == "keep") { if body_of(o, &s.name) != body_of(content, &s.name) { out.push(format!("{path}: [skeleton] `{}` is marked keep and was changed", s.name)); } } }
    out
}
/// The original text of a file: what git has at HEAD of the node's tree. Blocking.
pub fn original_in(dir: &Path, path: &str) -> Option<String> {
    let o = antiloki_rs::proc::cmd("git").args(["show", &format!("HEAD:{path}")]).current_dir(dir).output().ok()?; if o.status.success() { Some(String::from_utf8_lossy(&o.stdout).to_string()) } else { None }
}
/// The plan views and the conformance issues of a tree. A file that is still a stub (or absent) is not judged yet. Blocking.
pub fn run(dir: &Path, c: &ContractSpec) -> (Plan, Vec<String>) {
    let orig = |p: &str| original_in(dir, p); let plan = plan_of(c, &orig); let mut issues = vec![];
    for (p, f) in &c.files { if !is_js(p) || f.functions.is_empty() { continue; }
        let Ok(content) = std::fs::read_to_string(dir.join(p)) else { continue }; if crate::team_stubs::is_stub(&content) { continue; }
        issues.extend(conform_file(p, &content, &f.functions, orig(p).as_deref(), plan.get(p).map(Vec::as_slice).unwrap_or(&[])));
    }
    (plan, issues)
}
pub fn plan_json(plan: &Plan) -> Value { json!(plan) }

#[cfg(test)]
mod tests {
    use super::*;
    use crate::team_contract::FileContract;
    fn fnc(sig: &str, calls: &[&str]) -> FnContract { FnContract { signature: sig.into(), does: "d".into(), returns: String::new(), examples: vec![], edge_cases: vec![], calls: calls.iter().map(|s| s.to_string()).collect() } }
    fn contract() -> ContractSpec { ContractSpec { conventions: vec![], files: [("src/a.js".to_string(), FileContract { exports: vec![], purpose: String::new(), functions: vec![fnc("add(a, b)", &[]), fnc("run(argv)", &["add"])] })].into_iter().collect(), node_test: true } }
    #[test] fn the_plan_marks_add_modify_and_keep_and_has_three_views() {
        let orig = "export function add(a, b) { return a + b; }\nexport function old(x) {\n  return x;\n}\n"; let plan = plan_of(&contract(), &|p| (p == "src/a.js").then(|| orig.to_string()));
        let ops: Vec<(&str, &str)> = plan["src/a.js"].iter().map(|s| (s.name.as_str(), s.op.as_str())).collect(); assert_eq!(ops, vec![("add", "modify"), ("run", "add"), ("old", "keep")]);
        let (n, m, d) = views(&plan); assert!(n.contains("+ run(argv)") && n.contains("+ add(a, b)") && !n.contains("old")); assert!(m.contains("* add(a, b)") && m.contains("  old(p0)")); assert!(d.contains("~ add(a, b)") && d.contains("+ run(argv)") && d.contains("= old(p0)"));
        let fresh = plan_of(&contract(), &|_| None); assert!(fresh["src/a.js"].iter().all(|s| s.op == "add"));
    }
    #[test] fn the_filled_code_is_checked_against_the_plan_by_code() {
        let c = contract(); let f = &c.files["src/a.js"].functions; let plan = plan_of(&c, &|_| None); let p = plan["src/a.js"].as_slice();
        let good = "export function add(a, b) { return a + b; }\nexport function run(argv) { return add(1, 2); }\n"; assert!(conform_file("src/a.js", good, f, None, p).is_empty());
        let bad = "export function add(a) { return a; }\nfunction run(argv) { return 1; }\nexport const extra = 1;\n"; let issues = conform_file("src/a.js", bad, f, None, p); let all = issues.join("\n");
        assert!(all.contains("`add` takes 1 parameters, the plan says 2") && all.contains("`run` is planned as an export and is not exported") && all.contains("planned to call `add` and does not") && all.contains("`extra` is exported and was not planned"), "{all}");
        assert!(conform_file("src/a.js", "export function add(a, b) { return 1; }\n", f, None, p).iter().any(|i| i.contains("`run` is not there")));
        let mut with_keep = c.clone(); with_keep.files.get_mut("src/a.js").unwrap().functions.truncate(1); let orig = "export function add(a, b) { return a + b; }\nexport function old(x) { return x; }\n"; let pl = plan_of(&with_keep, &|_| Some(orig.to_string()));
        assert!(conform_file("src/a.js", "export function add(a, b) { return a + b; }\nexport function old(x) { return x + 1; }\n", &with_keep.files["src/a.js"].functions, Some(orig), &pl["src/a.js"]).iter().any(|i| i.contains("`old` is marked keep and was changed")));
        assert!(conform_file("src/a.js", orig, &with_keep.files["src/a.js"].functions, Some(orig), &pl["src/a.js"]).is_empty());
    }
}
