//! THE REVIEW NODE (RFC 0014; operator 2026-11-16: "deterministic review"): X-ray's rules applied to the code a node just made, so the mistakes code can see never reach a test.
//! Two sources, one verdict: (1) X-ray's own laws, through the same `antiloki_check` an agent calls (exact lines, introduced vs remaining); (2) the rules the LEADER adds before the run, as data: forbid and require patterns, limits on function length, parameters and file length, and "no const that is reassigned".
//! A model never decides a rule. An error leaves by `failed` with one line per issue (path, line, rule), which a recurse node feeds back and an inline guard in decompose turns into one exact re-ask.
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ReviewRule {
    pub id: String,
    /** forbid | require | max_function_lines | max_params | max_file_lines | no_const_reassign */ pub kind: String,
    #[serde(default, skip_serializing_if = "String::is_empty")] pub message: String,
    /** a regex (forbid, require) */ #[serde(default, skip_serializing_if = "String::is_empty")] pub pattern: String,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub max: Option<u32>,
    /** error | warn */ #[serde(default = "error")] pub severity: String,
    /** the rule applies to files whose path contains one of these (empty = every source file) */ #[serde(default, skip_serializing_if = "Vec::is_empty")] pub applies: Vec<String>,
}
fn error() -> String { "error".into() }
fn yes() -> bool { true }
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ReviewSpec {
    /** also run X-ray's laws on each file (`antiloki_check`) */ #[serde(default = "yes")] pub xray: bool,
    #[serde(default)] pub rules: Vec<ReviewRule>,
    /** rules in X-RAY'S OWN FORMAT (`key`, `title`, `rule`, `detector`, `config`, `severity`, `applies`), written by the leader before the run: judged by X-ray's engine (regex, absent, import, ast, stmt) for this review only, never saved into the project's laws */ #[serde(default, skip_serializing_if = "Vec::is_empty")] pub laws: Vec<serde_json::Value>,
}
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Issue { pub path: String, pub line: u32, pub rule: String, pub severity: String, pub msg: String }
impl Issue { pub fn line_text(&self) -> String { format!("{}:{} [{}] {}", self.path, self.line, self.rule, self.msg) } }

pub const KINDS: [&str; 6] = ["forbid", "require", "max_function_lines", "max_params", "max_file_lines", "no_const_reassign"];
/// What is wrong with a spec. Pure.
pub fn check(sp: &ReviewSpec) -> Vec<String> {
    let mut e = vec![];
    if !sp.xray && sp.rules.is_empty() && sp.laws.is_empty() { e.push("a review has X-ray's laws on or at least one rule".into()); }
    for l in &sp.laws { let k = l["key"].as_str().unwrap_or(""); if k.is_empty() || l["title"].as_str().map_or(true, str::is_empty) || l["rule"].as_str().map_or(true, str::is_empty) { e.push(format!("law `{k}`: `key`, `title` and `rule` are required")); } if !["regex", "absent", "import", "ast", "stmt"].contains(&l["detector"].as_str().unwrap_or("")) { e.push(format!("law `{k}`: `detector` is regex, absent, import, ast or stmt (what a content check can judge)")); } if !l["config"].is_object() { e.push(format!("law `{k}`: `config` is an object (see X-ray's detectors)")); } }
    for r in &sp.rules {
        if r.id.trim().is_empty() { e.push("a rule has an id".into()); }
        if !KINDS.contains(&r.kind.as_str()) { e.push(format!("{}: the kind is one of {}", r.id, KINDS.join(", "))); }
        if matches!(r.kind.as_str(), "forbid" | "require") { match Regex::new(&r.pattern) { Ok(_) if !r.pattern.is_empty() => {}, _ => e.push(format!("{}: `pattern` is a regular expression", r.id)) } }
        if matches!(r.kind.as_str(), "max_function_lines" | "max_params" | "max_file_lines") && r.max.map_or(true, |m| m == 0) { e.push(format!("{}: `max` is a number above 0", r.id)); }
        if !["error", "warn"].contains(&r.severity.as_str()) { e.push(format!("{}: severity is error or warn", r.id)); }
    }
    e
}
fn is_source(path: &str) -> bool { matches!(path.rsplit('.').next(), Some("js" | "mjs" | "cjs" | "ts" | "tsx" | "jsx" | "py")) }
/// A function found in JavaScript: its name, first and last line (1-based) and how many parameters it takes. Declarations and `const name = (…) =>`. Pure.
pub fn functions(js: &str) -> Vec<(String, u32, u32, u32)> {
    let head = Regex::new(r"^\s*(?:export\s+)?(?:async\s+)?(?:function\*?\s+([A-Za-z_$][\w$]*)\s*\(([^)]*)\)|(?:const|let)\s+([A-Za-z_$][\w$]*)\s*=\s*(?:async\s*)?\(([^)]*)\)\s*=>)").unwrap();
    let lines: Vec<&str> = js.lines().collect(); let mut out = vec![];
    for (i, l) in lines.iter().enumerate() {
        let Some(c) = head.captures(l) else { continue };
        let (name, params) = match (c.get(1), c.get(3)) { (Some(n), _) => (n.as_str(), c.get(2).map_or("", |m| m.as_str())), (_, Some(n)) => (n.as_str(), c.get(4).map_or("", |m| m.as_str())), _ => continue };
        let np = if params.trim().is_empty() { 0 } else { params.split(',').count() as u32 };
        let (mut depth, mut seen, mut end) = (0i32, false, i);
        'scan: for (j, lj) in lines.iter().enumerate().skip(i) { for ch in lj.chars() { match ch { '{' => { depth += 1; seen = true; } '}' => { depth -= 1; if seen && depth == 0 { end = j; break 'scan; } } _ => {} } } if !seen && lj.trim_end().ends_with(';') { end = j; break; } end = j; }
        out.push((name.to_string(), i as u32 + 1, end as u32 + 1, np));
    }
    out
}
/// One file against the leader's rules. Pure.
pub fn review_file(sp: &ReviewSpec, path: &str, content: &str) -> Vec<Issue> {
    let mut out = vec![]; if !is_source(path) { return out; }
    let lines: Vec<&str> = content.lines().collect();
    for r in &sp.rules {
        if !r.applies.is_empty() && !r.applies.iter().any(|a| path.contains(a.as_str())) { continue; }
        let msg = |d: String| if r.message.is_empty() { d } else { format!("{} ({d})", r.message) };
        let push = |out: &mut Vec<Issue>, line: u32, d: String| out.push(Issue { path: path.into(), line, rule: r.id.clone(), severity: r.severity.clone(), msg: msg(d) });
        match r.kind.as_str() {
            "forbid" => if let Ok(re) = Regex::new(&r.pattern) { for (i, l) in lines.iter().enumerate() { if re.is_match(l) { push(&mut out, i as u32 + 1, format!("`{}` is not allowed", l.trim().chars().take(60).collect::<String>())); } } },
            "require" => if let Ok(re) = Regex::new(&r.pattern) { if !re.is_match(content) { push(&mut out, 1, format!("`{}` must appear in the file", r.pattern)); } },
            "max_file_lines" => { let m = r.max.unwrap_or(u32::MAX); if lines.len() as u32 > m { push(&mut out, m + 1, format!("the file has {} lines, at most {m}", lines.len())); } }
            "max_function_lines" => { let m = r.max.unwrap_or(u32::MAX); for (n, a, b, _) in functions(content) { if b - a + 1 > m { push(&mut out, a, format!("`{n}` is {} lines long, at most {m}", b - a + 1)); } } }
            "max_params" => { let m = r.max.unwrap_or(u32::MAX); for (n, a, _, p) in functions(content) { if p > m { push(&mut out, a, format!("`{n}` takes {p} parameters, at most {m}")); } } }
            "no_const_reassign" => {
                let decl = Regex::new(r"^\s*(?:export\s+)?const\s+([A-Za-z_$][\w$]*)\s*=").unwrap();
                for (i, l) in lines.iter().enumerate() { let Some(c) = decl.captures(l) else { continue }; let name = c.get(1).unwrap().as_str();
                    // another declaration or a parameter of the same name may shadow it: then it is not judged (no false alarm)
                    let other = Regex::new(&format!(r"(?:let|var)\s+{n}\b|function[^(]*\([^)]*\b{n}\b|\(([^)]*\b{n}\b[^)]*)\)\s*=>|const\s+{n}\b", n = regex::escape(name))).unwrap();
                    if lines.iter().enumerate().filter(|(j, x)| *j != i && other.is_match(x)).count() > 0 { continue; }
                    let assign = Regex::new(&format!(r"(?:^|[^\w$.])({n})\s*(?:=[^=>]|\+=|-=|\*=|/=|\+\+|--)|(?:\+\+|--)\s*({n})\b", n = regex::escape(name))).unwrap();
                    for (j, x) in lines.iter().enumerate() { if j != i && assign.is_match(x) { push(&mut out, j as u32 + 1, format!("`{name}` is a const (declared on line {}) and is assigned here: declare it with `let`", i + 1)); } } }
            }
            _ => {}
        }
    }
    out
}
/// The files a node changed in `dir`: new or modified source files, by `git status`. Blocking.
pub fn changed_files(dir: &Path) -> Vec<String> {
    let out = antiloki_rs::proc::cmd("git").args(["status", "--porcelain", "-uall"]).current_dir(dir).output();
    out.ok().map(|o| String::from_utf8_lossy(&o.stdout).lines().filter_map(|l| l.get(3..).map(|p| p.trim().trim_matches('"').to_string())).filter(|p| is_source(p)).collect()).unwrap_or_default()
}
/// X-ray's laws on one file's content (`antiloki_check`): the markdown it answers and whether it introduced violations. The summary line is `check: <before> → <after> violations`.
pub async fn xray_check(port: u16, path: &str, content: &str, laws: &[serde_json::Value]) -> Result<(String, bool), String> {
    let r = reqwest::Client::new().post(format!("http://127.0.0.1:{port}/api/v1/cockpit/pane/context/tool")).json(&serde_json::json!({ "pane_id": 0, "tool": "antiloki_check", "args": { "path": path, "content": content, "extra_laws": laws } })).timeout(std::time::Duration::from_secs(60)).send().await.map_err(|e| e.to_string())?;
    let v: serde_json::Value = r.json().await.map_err(|e| e.to_string())?;
    if v["ok"] == serde_json::json!(false) { return Err(v["error"].as_str().unwrap_or("check failed").to_string()); }
    let sum = v["summary"].as_str().unwrap_or(""); let nums: Vec<u32> = sum.split(|c: char| !c.is_ascii_digit()).filter_map(|x| x.parse().ok()).collect();
    Ok((v["markdown"].as_str().unwrap_or("").to_string(), nums.len() >= 2 && nums[1] > nums[0]))
}
/// The review of a tree: the leader's rules on every changed source file, then X-ray's laws on each. Errors first.
pub async fn run(port: u16, dir: &Path, sp: &ReviewSpec) -> Result<Vec<Issue>, String> {
    let d2 = dir.to_path_buf(); let files = tokio::task::spawn_blocking(move || changed_files(&d2)).await.map_err(|e| e.to_string())?;
    let mut issues = vec![];
    for f in &files {
        let Ok(content) = std::fs::read_to_string(dir.join(f)) else { continue };
        issues.extend(review_file(sp, f, &content));
        if sp.xray || !sp.laws.is_empty() { if let Ok((md, introduced)) = xray_check(port, f, &content, &sp.laws).await { if introduced { issues.push(Issue { path: f.clone(), line: 1, rule: "xray".into(), severity: "error".into(), msg: format!("X-ray's laws: this file introduces violations. {}", md.lines().filter(|l| !l.trim().is_empty()).take(6).collect::<Vec<_>>().join(" | ").chars().take(400).collect::<String>()) }); } } }
    }
    issues.sort_by(|a, b| (a.severity != "error").cmp(&(b.severity != "error")).then(a.path.cmp(&b.path)).then(a.line.cmp(&b.line))); Ok(issues)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn rule(id: &str, kind: &str) -> ReviewRule { ReviewRule { id: id.into(), kind: kind.into(), message: String::new(), pattern: String::new(), max: None, severity: "error".into(), applies: vec![] } }
    fn spec(rules: Vec<ReviewRule>) -> ReviewSpec { ReviewSpec { xray: false, rules, laws: vec![] } }
    #[test] fn a_spec_is_refused_with_the_reason() {
        assert!(check(&spec(vec![ReviewRule { pattern: "console\\.log".into(), ..rule("no-log", "forbid") }])).is_empty());
        assert!(!check(&spec(vec![])).is_empty()); assert!(!check(&spec(vec![rule("x", "magic")])).is_empty()); assert!(!check(&spec(vec![ReviewRule { pattern: "(".into(), ..rule("x", "forbid") }])).is_empty()); assert!(!check(&spec(vec![rule("x", "max_params")])).is_empty());
    }
    #[test] fn the_leaders_laws_are_in_xrays_own_format() {
        let good = serde_json::json!({ "key": "no-console", "title": "No debug output", "rule": "do not leave console.log in the code", "detector": "regex", "config": { "pattern": "console\\.log" }, "severity": "error" });
        assert!(check(&ReviewSpec { xray: false, rules: vec![], laws: vec![good.clone()] }).is_empty());
        let bad = serde_json::json!({ "key": "x", "title": "t", "rule": "r", "detector": "metric", "config": {} }); assert!(!check(&ReviewSpec { xray: false, rules: vec![], laws: vec![bad] }).is_empty(), "metric cannot be judged on content");
        assert!(!check(&ReviewSpec { xray: false, rules: vec![], laws: vec![serde_json::json!({ "key": "x" })] }).is_empty());
    }
    #[test] fn functions_are_found_with_their_size_and_parameters() {
        let js = "export function add(a, b) {\n  return a + b;\n}\nexport const run = (argv, store) => {\n  const x = 1;\n  return x;\n};\nconst one = (a) => a;\n";
        let f = functions(js); assert_eq!(f.iter().map(|x| (x.0.as_str(), x.1, x.2, x.3)).collect::<Vec<_>>(), vec![("add", 1, 3, 2), ("run", 4, 7, 2), ("one", 8, 8, 1)]);
    }
    #[test] fn the_leaders_rules_find_the_mistakes_code_can_see() {
        let sp = spec(vec![ReviewRule { pattern: "console\\.log".into(), message: "use the logger".into(), ..rule("no-log", "forbid") }, ReviewRule { max: Some(3), ..rule("short", "max_function_lines") }, ReviewRule { max: Some(2), ..rule("few-params", "max_params") }, rule("const", "no_const_reassign"), ReviewRule { max: Some(5), severity: "warn".into(), ..rule("small-file", "max_file_lines") }]);
        let js = "const items = [];\nexport function add(a, b, c) {\n  console.log(a);\n  items = [a];\n  return b;\n}\n";
        let issues = review_file(&sp, "src/a.js", js); let by = |r: &str| issues.iter().find(|i| i.rule == r);
        assert!(by("no-log").unwrap().msg.contains("use the logger") && by("no-log").unwrap().line == 3); assert_eq!(by("short").unwrap().line, 2); assert!(by("few-params").unwrap().msg.contains("3 parameters"));
        assert!(by("const").unwrap().line == 4 && by("const").unwrap().msg.contains("declared on line 1")); assert_eq!(by("small-file").unwrap().severity, "warn");
        assert!(review_file(&sp, "README.md", js).is_empty(), "only source files are reviewed");
        let fine = "const items = [];\nexport function add(a) {\n  items.push(a);\n  return items;\n}\n"; assert!(review_file(&spec(vec![rule("const", "no_const_reassign")]), "src/b.js", fine).is_empty(), "push is not a reassignment");
        let shadow = "const n = 1;\nexport function f(n) {\n  n = 2;\n  return n;\n}\n"; assert!(review_file(&spec(vec![rule("const", "no_const_reassign")]), "src/c.js", shadow).is_empty(), "a parameter of the same name shadows it: no false alarm");
    }
}
