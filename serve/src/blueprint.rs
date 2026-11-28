//! BLUEPRINT MODE (operator 2026-09-22: "the orchestration may take a lot of AI when it is more deterministic —
//! create the deterministic gate and use the AI to populate it, not to think about the whole orchestration";
//! "compile and tests are deterministic, any low-cost AI can read them").
//!
//! A goal run in this mode is a pipeline of gates, each one timed and priced on the run (`stages`):
//!
//!   contract   one mid call: the ask → modules, each export's TypeScript signature + doc comment, and every
//!              acceptance line classified compile | test | judgment                       (LLM)
//!   skeleton   the stub files emitted FROM the contract — signatures, docs, `throw` bodies (deterministic)
//!   tests      one low call per module: vitest cases from the `test` acceptance lines               (LLM, cheap)
//!   gate       the skeleton and the tests compile (`tsc --noEmit`)                        (deterministic)
//!   fills      one worker per module, in parallel, at low: "fill the bodies; the signatures are fixed;
//!              these tests must pass"                                                       (LLM, cheap, N×)
//!   check      tsc + vitest on the task's files: green = pass with no LLM; red = one low call turns the
//!              compiler's / runner's output into the retry brief                            (deterministic + cheap)
//!
//! No join (the report is the machine's results), the Opus judge opt-in. The bench of 2026-09-21/22 measured
//! the LLM-orchestrated path at ~2 min and ~$0.75 of models describing the work five times over (plan, 8×
//! verify, join, judge); here the models generate code and read tool output, nothing else.
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::Path;

/// One acceptance line and what checks it.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Acceptance { pub text: String, #[serde(default = "kind_default")] pub kind: String }
fn kind_default() -> String { "judgment".into() }

/// One exported symbol of a module: its declaration (without a body for functions) and its doc comment.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Export {
    pub name: String,
    /// function | hook | component | const | type
    #[serde(default = "kind_function")] pub kind: String,
    /// the full TypeScript declaration: `export function f(a: A): B` (no body) · `export const X: T = …` ·
    /// `export type T = …` — copied verbatim into the skeleton
    pub signature: String,
    #[serde(default)] pub doc: String,
    #[serde(default)] pub acceptance: Vec<Acceptance>,
}
fn kind_function() -> String { "function".into() }

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Module {
    pub path: String,
    #[serde(default)] pub purpose: String,
    /// import lines, verbatim (`import type { Product } from "../products";`)
    #[serde(default)] pub imports: Vec<String>,
    #[serde(default)] pub exports: Vec<Export>,
    /// an EXISTING file to be edited (no skeleton is emitted; the fill edits it against the acceptance)
    #[serde(default)] pub edit: bool,
    /// the vitest file for this module (set by the tests stage)
    #[serde(default)] pub test_file: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Default)]
pub struct Contract { #[serde(default)] pub summary: String, #[serde(default)] pub modules: Vec<Module> }

/// One timed, priced step of the pipeline.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Stage { pub name: String, pub engine: String, pub tier: String, pub started: i64, pub ended: i64, pub ms: i64,
    #[serde(default)] pub tokens_in: i64, #[serde(default)] pub tokens_out: i64, #[serde(default)] pub cost: f64, pub ok: bool, #[serde(default)] pub note: String }

/// `{…}` out of a reply that may wrap it in prose or a fence. Pure.
pub fn parse_contract(text: &str) -> Option<Contract> {
    let (a, b) = (text.find('{'), text.rfind('}'));
    let slice = match (a, b) { (Some(a), Some(b)) if b > a => &text[a..=b], _ => text };
    let mut c: Contract = serde_json::from_str(slice).ok()?;
    c.modules.retain(|m| !m.path.trim().is_empty());
    for m in c.modules.iter_mut() {
        m.path = m.path.trim().trim_start_matches("./").to_string();
        for e in m.exports.iter_mut() { for a in e.acceptance.iter_mut() { let k = a.kind.trim().to_ascii_lowercase(); a.kind = if ["compile", "test", "judgment"].contains(&k.as_str()) { k } else { "judgment".into() }; } }
    }
    if c.modules.is_empty() { None } else { Some(c) }
}

/// THE SKELETON — deterministic: the file a contract's module describes, with every export's doc comment and
/// declaration and a body that throws until a worker fills it. Types and consts are their declarations as
/// written. Pure — tested.
pub fn emit_skeleton(m: &Module) -> String {
    let mut out = String::new();
    out.push_str(&format!("// {} — skeleton emitted from the contract; every `throw new Error(\"stub\")` body is to be filled\n", m.path));
    if !m.purpose.trim().is_empty() { out.push_str(&format!("// {}\n", m.purpose.trim())); }
    for i in &m.imports { let i = i.trim(); if !i.is_empty() { out.push_str(i); if !i.ends_with(';') { out.push(';'); } out.push('\n'); } }
    if !m.imports.is_empty() { out.push('\n'); }
    for e in &m.exports {
        if !e.doc.trim().is_empty() {
            out.push_str("/**\n");
            for line in e.doc.trim().lines() { out.push_str(&format!(" * {}\n", line.trim())); }
            out.push_str(" */\n");
        }
        let sig = e.signature.trim().trim_end_matches(';').trim_end_matches('{').trim_end();
        match e.kind.as_str() {
            "type" | "const" => { out.push_str(sig); out.push_str(";\n\n"); }
            _ => { out.push_str(sig); out.push_str(&format!(" {{\n  throw new Error(\"stub: {}\");\n}}\n\n", e.name)); }
        }
    }
    out
}

/// The `test`-kind acceptance lines of a module, for the tests stage. Pure.
pub fn testable_lines(m: &Module) -> Vec<(String, String)> {
    m.exports.iter().flat_map(|e| e.acceptance.iter().filter(|a| a.kind == "test").map(move |a| (e.name.clone(), a.text.clone()))).collect()
}

/// Where a module's tests live: `src/features/x.ts` → `src/features/x.test.ts` (`.tsx` keeps `.tsx`). Pure.
pub fn test_path_for(path: &str) -> String {
    match path.rsplit_once('.') { Some((stem, ext)) if ext == "ts" || ext == "tsx" => format!("{stem}.test.{ext}"), _ => format!("{path}.test.ts") }
}

/// `tsc --noEmit` on a TypeScript box: the error lines, or None when clean / not a TS box. Pure over the box.
pub async fn tsc(cwd: &str) -> Option<String> {
    let root = Path::new(cwd);
    let tsc = root.join("node_modules").join(".bin").join("tsc");
    let project = ["tsconfig.app.json", "tsconfig.json"].iter().map(|p| root.join(p)).find(|p| p.is_file())?;
    if !tsc.is_file() { return None; }
    let out = tokio::time::timeout(std::time::Duration::from_secs(180), crate::proc::tokio_cmd(&tsc).args(["--noEmit", "-p"]).arg(&project).current_dir(root).output()).await.ok()?.ok()?;
    if out.status.success() { return None; }
    let text = String::from_utf8_lossy(&out.stdout);
    let errs: Vec<&str> = text.lines().filter(|l| l.contains("error TS")).take(12).collect();
    if errs.is_empty() { None } else { Some(errs.join("\n")) }
}

/// `vitest run <files>` in the box: Ok(None) green, Ok(Some(tail)) red with the runner's tail, Err when vitest
/// is not there. 240 s cap.
pub async fn vitest(cwd: &str, files: &[String]) -> Result<Option<String>, String> {
    let root = Path::new(cwd);
    let bin = root.join("node_modules").join(".bin").join("vitest");
    if !bin.is_file() { return Err("vitest is not installed in this box".into()); }
    let mut cmd = crate::proc::tokio_cmd(&bin);
    cmd.arg("run").args(files).current_dir(root).env("CI", "1");
    let out = tokio::time::timeout(std::time::Duration::from_secs(240), cmd.output()).await.map_err(|_| "vitest: timed out after 240 s".to_string())?.map_err(|e| e.to_string())?;
    if out.status.success() { return Ok(None); }
    let text = format!("{}\n{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    let ansi = regex::Regex::new("\x1b\\[[0-9;]*m").unwrap();
    let clean = ansi.replace_all(&text, "");
    let keep: Vec<&str> = clean.lines().filter(|l| { let t = l.trim(); t.starts_with("FAIL") || t.starts_with("×") || t.starts_with("✗") || t.contains("AssertionError") || t.contains("Expected") || t.contains("Received") || t.contains("Tests ") || t.contains("Test Files") || t.contains("Error:") }).take(40).collect();
    Ok(Some(keep.join("\n").chars().take(4000).collect()))
}

/// THE CASES A VITEST RUN SAW FAIL, by name (deterministic QA step 3, 2026-10-09): `FAIL  src/x.test.ts > suite > case` lines of the trimmed output, file dropped, sorted, deduplicated. Pure - tested.
pub fn failing_cases(text: &str) -> Vec<String> {
    let mut v: Vec<String> = text.lines().filter_map(|l| {
        let t = l.trim(); let rest = t.strip_prefix("FAIL")?.trim_start();
        let (_, name) = rest.split_once(" > ")?;
        let name = name.trim(); if name.is_empty() { None } else { Some(name.to_string()) }
    }).collect();
    v.sort(); v.dedup(); v
}
/// The cases that failed on BOTH attempts of one task. Pure.
pub fn disputed(first: &[String], second: &[String]) -> Vec<String> { second.iter().filter(|c| first.contains(c)).cloned().collect() }
/// How many `it(` / `test(` cases a test file declares (skipped ones included). Pure.
pub fn case_count(src: &str) -> usize { src.lines().filter(|l| { let t = l.trim_start(); (t.starts_with("it(") || t.starts_with("test(") || t.starts_with("it.each") || t.starts_with("test.each")) }).count() }
/// MAY THESE FAILURES BE CALLED A BAD TEST? Only when every case that fails NOW also failed the attempt before (a fill that was told the failure and could not satisfy it), there are few of
/// them (at most two) and they are a small share of the file (a quarter or less): a file that fails wholesale is a bad fill, not a bad test. Pure - tested.
pub fn may_dispute(failing_now: &[String], both: &[String], total_cases: usize) -> bool {
    !failing_now.is_empty() && both.len() == failing_now.len() && both.len() <= 2 && total_cases >= 4 && both.len() * 4 <= total_cases
}
/// The test source with the named cases skipped (`it(` -> `it.skip(`), and how many were changed. A case is named by the LAST segment of its `suite > case` path. Pure - tested.
pub fn skip_cases(src: &str, cases: &[String]) -> (String, usize) {
    let mut n = 0; let mut out: Vec<String> = vec![];
    for line in src.lines() {
        let t = line.trim_start();
        let call = ["it(", "test("].iter().find(|c| t.starts_with(**c));
        let hit = call.is_some() && cases.iter().any(|c| { let leaf = c.rsplit(" > ").next().unwrap_or(c); ["\"", "'", "`"].iter().any(|q| t.contains(&format!("{q}{leaf}{q}"))) });
        if hit { let ind = &line[..line.len() - t.len()]; out.push(format!("{ind}{}", t.replacen("it(", "it.skip(", 1).replacen("test(", "test.skip(", 1))); n += 1; } else { out.push(line.to_string()); }
    }
    (out.join("\n") + if src.ends_with('\n') { "\n" } else { "" }, n)
}
/// THE WHOLE DISPUTE DECISION AS ONE PURE FUNCTION (2026-10-10; it was inline in the check stage and never tested as a unit): given how many attempts the task has had, whether the compiler is clean, the cases that
/// failed the attempt before (`prev`) and now, and the test file, either `None` (the failure stands: a first attempt, a compiler error, a NEW failure, too many or too large a share, or a name that does not match exactly
/// the cases to skip) or the test source with the disputed cases skipped and their names. The caller still writes the file, re-runs the tests and puts the original back when it is still red. Pure - tested.
pub fn dispute_plan(attempts: u32, compiles: bool, prev: &[String], now: &[String], test_src: &str) -> Option<(String, Vec<String>)> {
    if attempts < 2 || !compiles { return None }
    let both = disputed(prev, now);
    if !may_dispute(now, &both, case_count(test_src)) { return None }
    let (skipped, n) = skip_cases(test_src, &both);
    if n == both.len() { Some((skipped, both)) } else { None }
}
/// `npm install -D vitest` when the box has none (the orchestrator owns installs — headless workers have no
/// network, 2026-09-22). Ok(true) when it installed, Ok(false) when it was already there.
pub async fn ensure_vitest(cwd: &str) -> Result<bool, String> {
    let root = Path::new(cwd);
    if root.join("node_modules").join(".bin").join("vitest").is_file() { return Ok(false); }
    let mut cmd = crate::proc::tokio_cmd("npm");
    cmd.args(["install", "--no-audit", "--no-fund", "--save-dev", "vitest"]).current_dir(root);
    let out = tokio::time::timeout(std::time::Duration::from_secs(300), cmd.output()).await.map_err(|_| "npm install: timed out".to_string())?.map_err(|e| e.to_string())?;
    if out.status.success() { Ok(true) } else { Err(format!("npm install vitest failed: {}", String::from_utf8_lossy(&out.stderr).chars().take(300).collect::<String>())) }
}

/// The contract call's vars. Pure.
pub fn contract_vars(goal: &str, tree: &str, conventions: &str) -> serde_json::Map<String, Value> {
    [("goal", json!(goal)), ("tree", json!(tree)), ("conventions", json!(conventions))].into_iter().map(|(k, v)| (k.to_string(), v)).collect()
}

/// What the box tells every stage about how it compiles: the tsconfig flags a worker would otherwise never read
/// (2026-09-22: `verbatimModuleSyntax` cost two builds). Pure over the file.
pub fn conventions_of(cwd: &str) -> String {
    let root = Path::new(cwd);
    let mut notes = vec![];
    for p in ["tsconfig.app.json", "tsconfig.json"] {
        if let Ok(t) = std::fs::read_to_string(root.join(p)) {
            for flag in ["verbatimModuleSyntax", "strict", "noUnusedLocals", "noUnusedParameters", "erasableSyntaxOnly", "jsx", "moduleResolution", "target"] {
                if let Some(i) = t.find(&format!("\"{flag}\"")) { let rest: String = t[i..].chars().take(60).collect(); notes.push(rest.split(',').next().unwrap_or("").trim().to_string()); }
            }
            break;
        }
    }
    if let Ok(pkg) = std::fs::read_to_string(root.join("package.json")) { if let Ok(j) = serde_json::from_str::<Value>(&pkg) { if let Some(d) = j.get("dependencies").and_then(Value::as_object) { notes.push(format!("dependencies: {}", d.keys().cloned().collect::<Vec<_>>().join(", "))); } } }
    if notes.iter().any(|n| n.contains("verbatimModuleSyntax\": true")) { notes.push("verbatimModuleSyntax is ON: import types with `import type { … }`, never as values; an unused import is an error".into()); }
    notes.join("\n")
}

/// A short tree of the box's source for the contract call (paths only, ≤ 200). Pure over the box.
pub fn source_tree(cwd: &str) -> String {
    let root = Path::new(cwd);
    let mut out = vec![];
    fn walk(dir: &Path, root: &Path, out: &mut Vec<String>, depth: usize) {
        if depth > 4 || out.len() > 200 { return; }
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        let mut entries: Vec<_> = rd.flatten().collect(); entries.sort_by_key(|e| e.file_name());
        for e in entries {
            let name = e.file_name().to_string_lossy().to_string();
            if name.starts_with('.') || name == "node_modules" || name == "dist" || name == "target" { continue; }
            let p = e.path();
            if p.is_dir() { walk(&p, root, out, depth + 1); } else if let Ok(rel) = p.strip_prefix(root) { out.push(rel.to_string_lossy().to_string()); }
        }
    }
    walk(root, root, &mut out, 0);
    out.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn a_skeleton_is_the_contract_as_code() {
        let m = Module { path: "src/features/sort.ts".into(), purpose: "stable sorting".into(), imports: vec!["import type { Product } from \"../products\"".into()],
            exports: vec![
                Export { name: "SortKey".into(), kind: "type".into(), signature: "export type SortKey = \"price-asc\" | \"price-desc\"".into(), doc: "The sort orders.".into(), acceptance: vec![] },
                Export { name: "sortProducts".into(), kind: "function".into(), signature: "export function sortProducts(products: readonly Product[], key: SortKey): Product[]".into(), doc: "Stable; never mutates.\nReturns a new array.".into(),
                    acceptance: vec![Acceptance { text: "stable on ties".into(), kind: "test".into() }, Acceptance { text: "compiles under strict".into(), kind: "compile".into() }] },
            ], edit: false, test_file: None };
        let s = emit_skeleton(&m);
        assert!(s.contains("import type { Product } from \"../products\";"), "{s}");
        assert!(s.contains("export type SortKey = \"price-asc\" | \"price-desc\";"));
        assert!(s.contains("/**\n * Stable; never mutates.\n * Returns a new array.\n */\nexport function sortProducts(products: readonly Product[], key: SortKey): Product[] {\n  throw new Error(\"stub: sortProducts\");\n}"), "{s}");
        assert_eq!(testable_lines(&m), vec![("sortProducts".to_string(), "stable on ties".to_string())]);
        assert_eq!(test_path_for("src/features/sort.ts"), "src/features/sort.test.ts");
        assert_eq!(test_path_for("src/features/quantity.tsx"), "src/features/quantity.test.tsx");
    }
    #[test]
    fn a_contract_parses_out_of_prose_and_normalises_kinds() {
        let c = parse_contract("Here:\n```json\n{\"summary\":\"two modules\",\"modules\":[{\"path\":\"./src/a.ts\",\"exports\":[{\"name\":\"f\",\"signature\":\"export function f(): void\",\"acceptance\":[{\"text\":\"x\",\"kind\":\"TEST\"},{\"text\":\"y\",\"kind\":\"whatever\"}]}]},{\"path\":\"  \"}]}\n```").unwrap();
        assert_eq!(c.modules.len(), 1); assert_eq!(c.modules[0].path, "src/a.ts");
        assert_eq!(c.modules[0].exports[0].acceptance.iter().map(|a| a.kind.as_str()).collect::<Vec<_>>(), ["test", "judgment"]);
        assert!(parse_contract("no json here").is_none());
    }
}

/// The compile gate of a task: `tsc --noEmit` errors whose paths touch `files` (or all of them for `**`). Pure over the box.
pub async fn compile_errors_for(cwd: &str, files: &[String]) -> Option<String> {
    if !files.iter().any(|f| f.ends_with(".ts") || f.ends_with(".tsx") || f == "**") { return None; }
    let errs = tsc(cwd).await?;
    let mine: Vec<&str> = errs.lines().filter(|l| files.iter().any(|f| { let f = f.trim_start_matches("./"); f == "**" || l.starts_with(f) || l.contains(f) })).take(8).collect();
    if mine.is_empty() { None } else { Some(mine.join(" | ").chars().take(1200).collect()) }
}

/// One claim's verdict from the QA gate.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ClaimCheck { pub task: String, pub path: String, pub claim: String, pub kind: String, pub met: Option<bool>, pub why: String, pub by: String, pub ms: i64, pub cost: f64 }

/// The gate's roll-up: what the run proved, one line per claim.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Default)]
pub struct QaGate { pub met: usize, pub unmet: usize, pub unproven: usize, pub checks: Vec<ClaimCheck>, pub ms: i64, pub cost: f64,
    /// WHICH SEAT JUDGED (`engine:model`, the key its merge row is filed under) - the gate is routed per defect class now, so the seat cannot be recomputed from the job binding later (an undo counts a false accept against THIS seat). Empty on a run from before.
    #[serde(default)] pub judge: String }
impl QaGate {
    pub fn passed(&self) -> bool { self.unmet == 0 && self.met > 0 }
    /// The PR body's table — evidence, not prose. Pure.
    pub fn md(&self) -> String {
        let mark = |m: &Option<bool>| match m { Some(true) => "met", Some(false) => "UNMET", None => "unproven" };
        let rows = self.checks.iter().map(|c| format!("| {} | {} | {} | {} |", c.task, c.claim.chars().take(90).collect::<String>(), mark(&c.met), c.why.chars().take(120).collect::<String>())).collect::<Vec<_>>().join("\n");
        format!("QA gate: {} met · {} unmet · {} unproven ({:.0} s · ${:.3})\n\n| task | claim | verdict | evidence |\n|---|---|---|---|\n{rows}", self.met, self.unmet, self.unproven, self.ms as f64 / 1000.0, self.cost)
    }
}
/// `{"met": …, "why": …}` out of a reply. Pure — tested.
pub fn parse_claim(text: &str) -> Option<(Option<bool>, String)> {
    let (a, b) = (text.find('{'), text.rfind('}'));
    let slice = match (a, b) { (Some(a), Some(b)) if b > a => &text[a..=b], _ => text };
    let j: Value = serde_json::from_str(slice).ok()?;
    let met = match j.get("met") { Some(Value::Bool(b)) => Some(*b), Some(Value::Null) | None => None, Some(v) => v.as_str().and_then(|s| match s.to_ascii_lowercase().as_str() { "true" | "met" | "yes" => Some(true), "false" | "unmet" | "no" => Some(false), _ => None }) };
    Some((met, j.get("why").and_then(Value::as_str).unwrap_or("").trim().chars().take(300).collect()))
}

#[cfg(test)]
mod gate_tests {
    use super::*;
    #[test]
    fn a_claim_parses_and_the_gate_rolls_up() {
        assert_eq!(parse_claim("{\"met\": true, \"why\": \"line 12 clamps to 99\"}"), Some((Some(true), "line 12 clamps to 99".into())));
        assert_eq!(parse_claim("sure:\n{\"met\": \"UNMET\", \"why\": \"no such export\"}").unwrap().0, Some(false));
        assert_eq!(parse_claim("{\"met\": null, \"why\": \"needs the runtime\"}").unwrap().0, None);
        assert_eq!(parse_claim("not json"), None);
        let c = |m: Option<bool>| ClaimCheck { task: "t1".into(), path: "a.ts".into(), claim: "x".into(), kind: "judgment".into(), met: m, why: "w".into(), by: "mid".into(), ms: 1, cost: 0.0 };
        let g = QaGate { met: 2, unmet: 1, unproven: 1, checks: vec![c(Some(true)), c(Some(true)), c(Some(false)), c(None)], ms: 4000, cost: 0.02, judge: String::new() };
        assert!(!g.passed(), "one unmet fails the gate");
        assert!(g.md().contains("2 met · 1 unmet · 1 unproven") && g.md().contains("| t1 | x | UNMET |"), "{}", g.md());
        assert!(QaGate { met: 3, ..Default::default() }.passed());
    }
}

#[cfg(test)]
mod dispute_tests {
    use super::*;
    const OUT: &str = "FAIL  src/features/money.test.ts > formatPrice > does not mutate its input\n  AssertionError: expected 1 to be 2\n FAIL  src/features/money.test.ts > formatPrice > returns a new reference\n× x 2ms\nFAIL  src/features/money.test.ts\n";
    #[test]
    fn failing_cases_are_read_by_name_without_the_file() {
        assert_eq!(failing_cases(OUT), vec!["formatPrice > does not mutate its input".to_string(), "formatPrice > returns a new reference".to_string()]);
        assert!(failing_cases("all green").is_empty());
    }
    #[test]
    fn only_a_case_that_failed_both_times_is_disputed_and_only_when_few_and_a_small_share() {
        let a = vec!["s > x".to_string(), "s > y".to_string()]; let b = vec!["s > y".to_string()];
        assert_eq!(disputed(&a, &b), vec!["s > y".to_string()]);
        assert!(may_dispute(&b, &disputed(&a, &b), 8), "one of eight, failed twice");
        assert!(!may_dispute(&a, &disputed(&b, &a), 8), "a case that failed only now is a new failure: the fill is still wrong");
        let three: Vec<String> = (0..3).map(|i| format!("s > c{i}")).collect();
        assert!(!may_dispute(&three, &three, 40), "three is too many");
        assert!(!may_dispute(&b, &b, 3), "a file of three cases proves too little to drop one");
        assert!(may_dispute(&b, &b, 4), "one of four is exactly a quarter: allowed");
        let two = vec!["s > a".to_string(), "s > b".to_string()];
        assert!(!may_dispute(&two, &two, 7), "two of seven is more than a quarter");
    }
    #[test]
    fn the_dispute_decision_composes_every_rule_and_refuses_otherwise() {
        let src = "describe(\"s\", () => {\n  it(\"a\", () => {});\n  it(\"b\", () => {});\n  it(\"c\", () => {});\n  it(\"d\", () => {});\n  it(\"e\", () => {});\n});\n";
        let (b, a) = (vec!["s > b".to_string()], vec!["s > a".to_string(), "s > b".to_string()]);
        // the one case that failed twice, compiler clean, second attempt, one of five: skipped
        let (out, names) = dispute_plan(2, true, &b, &b, src).expect("disputed");
        assert_eq!(names, b); assert!(out.contains("it.skip(\"b\"") && out.contains("it(\"a\"") && out.contains("it(\"e\""));
        assert!(dispute_plan(1, true, &b, &b, src).is_none(), "a first attempt is never disputed");
        assert!(dispute_plan(2, false, &b, &b, src).is_none(), "a compiler error is the fill's");
        assert!(dispute_plan(2, true, &a, &b, src).is_some(), "b failed in both; a is not failing now");
        assert!(dispute_plan(2, true, &b, &a, src).is_none(), "a is a NEW failure on the second attempt: the fill moved, it is not a bad test");
        assert!(dispute_plan(2, true, &[], &b, src).is_none(), "nothing failed before");
        assert!(dispute_plan(2, true, &b, &b, "it(\"b\", () => {});\n").is_none(), "a file of one case proves too little");
        assert!(dispute_plan(2, true, &b, &b, "describe(\"s\", () => {\n  it(\"x\", () => {});\n  it(\"y\", () => {});\n  it(\"z\", () => {});\n  it(\"w\", () => {});\n});\n").is_none(), "the named case is not in the file: nothing to skip, so the failure stands");
        // two suites sharing a leaf name: both lines would be skipped, which is more than the one case named - refused
        let dup = "describe(\"s\", () => {\n  it(\"b\", () => {});\n  it(\"a\", () => {});\n  it(\"c\", () => {});\n  it(\"d\", () => {});\n});\ndescribe(\"t\", () => {\n  it(\"b\", () => {});\n});\n";
        assert!(dispute_plan(2, true, &b, &b, dup).is_none(), "an ambiguous name is never guessed at");
    }
    #[test]
    fn skipping_marks_exactly_the_named_cases_and_nothing_else() {
        let src = "describe(\"s\", () => {\n  it(\"keeps\", () => {});\n  it(\"does not mutate its input\", () => {});\n  test('other', () => {});\n});\n";
        let (out, n) = skip_cases(src, &["s > does not mutate its input".to_string()]);
        assert_eq!(n, 1);
        assert!(out.contains("it.skip(\"does not mutate its input\"") && out.contains("it(\"keeps\"") && out.contains("test('other'"));
        assert!(out.ends_with("});\n"));
        assert_eq!(skip_cases(src, &["s > nope".to_string()]).1, 0, "a name that is not in the file changes nothing");
        assert_eq!(case_count(src), 3);
    }
}
