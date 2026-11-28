//! THE LAWS, AS A GATE ON THE DIFF.
//!
//! Operator 2026-09-25: *"add to the queue, also a gate validation about the laws x-ray has on the new code, it
//! should be seconds because is deterministic and only om the diff with approve or reject guard."*
//!
//! Two things called laws live in this product and only one of them belongs here. `laws` (35 rows) are
//! antiloki's own governance — "never falsify protocol state", "history is sacred" — whose clauses are prose
//! antipatterns about how antiloki behaves. `review_laws` are the code-review laws, and those carry a
//! `detector` and a `config_json`: the machine-checkable kind.
//!
//! **Only some detectors can honestly run on a diff.** `regex`, `absent`, `path` and `import` are per-file:
//! they need the changed file's text and nothing else, and they finish in milliseconds. `metric`, `graph`,
//! `ast` and `layer` need the whole code atlas — a cycle cannot be seen in one file — and pretending to judge
//! them from a diff would produce confident nonsense. Those stay in the X-ray report where the atlas is.
//!
//! **A gate with no applicable law is SKIPPED, never passed** — "no laws" and "no violations" are opposite
//! facts, and the other three gates follow the same rule.
//!
//! That skip is the exception rather than the norm: the law book lives in the REPO, at
//! `<worktree>/.antiloki/db/laws.sqlite`, and this machine's repos carry 17 ratified laws of which 9 are
//! regex — console.log, `: any`, `@ts-ignore`, bare `except:`, mutable default arguments, React array-index
//! keys, `dangerouslySetInnerHTML`, sync fs calls, and a critical one for hardcoded secrets. I first reported
//! the table as empty because I had queried the DATA ROOT's copy instead of the repo's; the test at the bottom
//! now loads whichever real book is present, so that claim cannot be made wrongly again.
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A law as the gate needs it. `config` is the detector's own parameters, shape per detector.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Law {
    pub key: String,
    pub title: String,
    pub detector: String,
    pub severity: String,
    pub config: Value,
    /// only files matching this glob-ish suffix list are judged; empty = every changed file
    #[serde(default)] pub applies: Vec<String>,
}

/// One breach, with enough to act on: which law, which file, which line, and the line itself.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Breach { pub law: String, pub title: String, pub severity: String, pub path: String, pub line: usize, pub text: String }

/// The detectors that can be judged from a diff alone. Pure.
pub fn diff_local(detector: &str) -> bool { matches!(detector, "regex" | "absent" | "path" | "import") }

/// The laws this gate may run: diff-local detectors with a usable config. Pure — tested.
pub fn runnable(laws: &[Law]) -> Vec<Law> {
    laws.iter().filter(|l| diff_local(&l.detector) && !l.key.trim().is_empty()).cloned().collect()
}

fn applies_to(l: &Law, path: &str) -> bool {
    if l.applies.is_empty() { return true }
    l.applies.iter().any(|a| { let a = a.trim().trim_start_matches('*'); !a.is_empty() && path.ends_with(a) })
}

/// Judge the changed files against the laws. `files` is (path, contents).
///
/// `regex` — the pattern must NOT appear. `absent` — it MUST appear somewhere in the file. `path` — the file's
/// own path must match. `import` — the file must not import a forbidden module. Every breach names the line, so
/// the agent that has to fix it is handed a location rather than a lecture. Pure — tested.
pub fn check(laws: &[Law], files: &[(String, String)]) -> Vec<Breach> {
    let mut out = vec![];
    for l in runnable(laws) {
        let pat = l.config.get("pattern").and_then(Value::as_str).unwrap_or("").trim().to_string();
        let forbidden: Vec<String> = l.config.get("modules").and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect()).unwrap_or_default();
        // A LAW WITH NO PATTERN CANNOT JUDGE ANYTHING and is skipped rather than matching everything — an
        // empty needle finds a haystack, which would flag every file in the repo on the first run.
        if pat.is_empty() && forbidden.is_empty() { continue }
        let re = if pat.is_empty() { None } else { regex::Regex::new(&pat).ok() };
        if !pat.is_empty() && re.is_none() { continue }   // an unparseable law is broken, not universally broken

        for (path, body) in files.iter().filter(|(p, _)| applies_to(&l, p)) {
            let breach = |line: usize, text: &str| Breach {
                law: l.key.clone(), title: l.title.clone(), severity: l.severity.clone(),
                path: path.clone(), line, text: text.trim().chars().take(140).collect(),
            };
            match l.detector.as_str() {
                "path" => { if let Some(r) = &re { if r.is_match(path) { out.push(breach(0, path)); } } }
                "absent" => { if let Some(r) = &re { if !r.is_match(body) { out.push(breach(0, "the file never does this")); } } }
                "import" => {
                    for (i, line) in body.lines().enumerate() {
                        let looks_like_import = line.contains("import ") || line.contains("require(") || line.trim_start().starts_with("use ");
                        if looks_like_import && forbidden.iter().any(|m| line.contains(m.as_str())) { out.push(breach(i + 1, line)); }
                    }
                }
                _ => {   // regex
                    if let Some(r) = &re {
                        for (i, line) in body.lines().enumerate() { if r.is_match(line) { out.push(breach(i + 1, line)); } }
                    }
                }
            }
        }
    }
    out
}

/// What the gate says. `None` for laws = nothing to run, which is a SKIP and not a pass. Pure — tested.
pub fn verdict(laws: &[Law], files: &[(String, String)]) -> Result<Vec<Breach>, String> {
    if laws.is_empty() { return Err("no code laws written yet — X-ray's review_laws is empty, so there is nothing to judge against".into()) }
    let run = runnable(laws);
    if run.is_empty() {
        return Err(format!("none of the {} law(s) can be judged from a diff — metric, graph, ast and layer detectors need the whole atlas, which the X-ray report has", laws.len()));
    }
    if files.is_empty() { return Err("nothing changed in the box this turn".into()) }
    Ok(check(&run, files))
}

/// The line the operator reads, and the line an agent is handed on a re-run. Pure — tested.
pub fn brief(breaches: &[Breach]) -> String {
    if breaches.is_empty() { return "no law broken in the files this turn touched".into() }
    let hard = breaches.iter().filter(|b| b.severity == "hard" || b.severity == "error").count();
    let head = format!("{} law breach{}{}", breaches.len(), if breaches.len() == 1 { "" } else { "es" },
        if hard > 0 { format!(", {hard} hard") } else { String::new() });
    let lines: Vec<String> = breaches.iter().take(12)
        .map(|b| format!("  {}:{} · {} — {}", b.path, b.line, b.title, b.text)).collect();
    format!("{head}\n{}", lines.join("\n"))
}

/// The code laws as the repo stores them. `<data root>/.antiloki/db/laws.sqlite`, table `review_laws` — the
/// X-ray set, not antiloki's own governance laws in `laws`. A missing table or file is an empty set, which the
/// gate then reports as a skip: a machine with no laws written is the normal state today, not an error.
pub fn load(root: &str) -> Vec<Law> {
    let path = std::path::Path::new(root).join(".antiloki").join("db").join("laws.sqlite");
    let Ok(c) = rusqlite::Connection::open_with_flags(&path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY) else { return vec![] };
    let Ok(mut st) = c.prepare("SELECT key, title, detector, COALESCE(severity,'soft'), COALESCE(config_json,'{}') FROM review_laws WHERE COALESCE(status,'active') NOT IN ('candidate','dismissed','suspended')") else { return vec![] };
    let rows = st.query_map([], |r| {
        let cfg: String = r.get(4)?;
        Ok(Law { key: r.get(0)?, title: r.get(1)?, detector: r.get(2)?, severity: r.get(3)?,
                 config: serde_json::from_str(&cfg).unwrap_or(Value::Null), applies: vec![] })
    });
    rows.map(|it| it.filter_map(Result::ok).collect()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn law(key: &str, detector: &str, config: Value) -> Law {
        Law { key: key.into(), title: format!("law {key}"), detector: detector.into(), severity: "hard".into(), config, applies: vec![] }
    }
    fn f(path: &str, body: &str) -> (String, String) { (path.into(), body.into()) }

    #[test]
    fn only_the_detectors_a_diff_can_answer_are_run() {
        for d in ["regex", "absent", "path", "import"] { assert!(diff_local(d), "{d} is per-file"); }
        // these need the whole atlas — a cycle is not visible in one file, and guessing would be worse than
        // not answering
        for d in ["metric", "graph", "ast", "layer", "none"] { assert!(!diff_local(d), "{d} needs the atlas"); }
        assert_eq!(runnable(&[law("a", "regex", json!({})), law("b", "graph", json!({}))]).len(), 1);
    }

    #[test]
    fn a_regex_law_names_the_file_and_the_line() {
        let l = law("no-console", "regex", json!({ "pattern": "console\\.log" }));
        let b = check(&[l], &[f("src/a.ts", "const x = 1;\nconsole.log(x);\n")]);
        assert_eq!(b.len(), 1);
        assert_eq!((b[0].path.as_str(), b[0].line), ("src/a.ts", 2));
        assert_eq!(b[0].text, "console.log(x);", "the agent is handed the line, not a lecture");
    }

    #[test]
    fn absent_path_and_import_each_judge_their_own_thing() {
        // absent: the file MUST do this somewhere
        let must = law("licence-header", "absent", json!({ "pattern": "^// SPDX" }));
        assert_eq!(check(&[must.clone()], &[f("a.rs", "fn main() {}")]).len(), 1);
        assert!(check(&[must], &[f("a.rs", "// SPDX-License-Identifier: MIT\nfn main() {}")]).is_empty());

        // path: the path itself is the breach
        assert_eq!(check(&[law("no-utils", "path", json!({ "pattern": "/utils?/" }))], &[f("src/util/x.ts", "")]).len(), 1);

        // import: only on lines that look like imports, so the word in a comment is not a breach
        let imp = law("no-lodash", "import", json!({ "modules": ["lodash"] }));
        assert_eq!(check(&[imp.clone()], &[f("a.ts", "import _ from 'lodash';")]).len(), 1);
        assert!(check(&[imp], &[f("a.ts", "// we used to use lodash here")]).is_empty(),
            "a module named in prose is not an import");
    }

    #[test]
    fn a_law_that_cannot_judge_is_skipped_rather_than_matching_everything() {
        // an empty needle finds every haystack: this would flag the whole repo on its first run
        assert!(check(&[law("empty", "regex", json!({}))], &[f("a.ts", "anything")]).is_empty());
        // and an unparseable pattern is a broken law, not a universally broken codebase
        assert!(check(&[law("bad", "regex", json!({ "pattern": "([unclosed" }))], &[f("a.ts", "x")]).is_empty());
    }

    #[test]
    fn applies_narrows_to_the_files_a_law_is_about() {
        let mut l = law("tsx-only", "regex", json!({ "pattern": "TODO" }));
        l.applies = vec!["*.tsx".into()];
        let files = vec![f("a.tsx", "// TODO"), f("b.rs", "// TODO")];
        let b = check(&[l], &files);
        assert_eq!(b.len(), 1);
        assert_eq!(b[0].path, "a.tsx");
    }

    /// NO LAWS AND NO VIOLATIONS ARE OPPOSITE FACTS — the rule every gate here follows.
    #[test]
    fn an_empty_law_set_is_a_skip_and_never_a_pass() {
        let files = vec![f("a.ts", "console.log(1)")];
        let e = verdict(&[], &files).unwrap_err();
        assert!(e.contains("no code laws written yet"), "{e}");

        // laws exist but none can be judged from a diff: also a skip, and it says where they ARE judged
        let e2 = verdict(&[law("cyc", "graph", json!({}))], &files).unwrap_err();
        assert!(e2.contains("need the whole atlas"), "{e2}");

        // nothing changed: nothing to judge
        assert!(verdict(&[law("a", "regex", json!({ "pattern": "x" }))], &[]).is_err());

        // and a real pass says so in words, because an empty list on screen means nothing
        assert_eq!(brief(&[]), "no law broken in the files this turn touched");
    }

    /// END TO END ON A REAL DIRECTORY, because `load` reads sqlite and everything above it is pure: a passing
    /// pure suite proves the judging and nothing about the reading.
    #[test]
    fn a_real_law_table_is_read_and_only_its_diff_local_laws_run() {
        let d = std::env::temp_dir().join(format!("antiloki-laws-{}-{:?}", std::process::id(), std::thread::current().id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join(".antiloki").join("db")).unwrap();
        let c = rusqlite::Connection::open(d.join(".antiloki").join("db").join("laws.sqlite")).unwrap();
        c.execute_batch(r#"CREATE TABLE review_laws (id INTEGER PRIMARY KEY, scope TEXT, key TEXT UNIQUE, title TEXT,
            rule_md TEXT, detector TEXT, config_json TEXT, severity TEXT, status TEXT);
            INSERT INTO review_laws (scope,key,title,rule_md,detector,config_json,severity,status) VALUES
              ('project','no-console','No console.log in shipped code','never','regex','{"pattern":"console\\.log"}','hard','active'),
              ('architecture','no-cycles','No import cycles','never','graph','{}','hard','active'),
              ('project','retired','An old law','never','regex','{"pattern":"x"}','soft','dismissed');"#).unwrap();

        let laws = load(d.to_str().unwrap());
        assert_eq!(laws.len(), 2, "a dismissed law is not enforced: {laws:?}");
        assert_eq!(runnable(&laws).len(), 1, "only the regex law can be judged from a diff");

        let breaches = verdict(&laws, &[f("src/a.ts", "const x = 1;\nconsole.log(x);\n")]).unwrap();
        assert_eq!(breaches.len(), 1);
        assert_eq!(breaches[0].law, "no-console");
        assert_eq!(breaches[0].line, 2);
        assert_eq!(breaches[0].severity, "hard");

        // a clean file passes, and says so in words
        assert_eq!(brief(&verdict(&laws, &[f("src/b.ts", "const y = 2;\n")]).unwrap()),
                   "no law broken in the files this turn touched");

        // and a machine with no law table at all is a skip, which is the state of every machine today
        assert!(load("/nonexistent-root-9f2c").is_empty());
        let _ = std::fs::remove_dir_all(&d);
    }

    /// THE OPERATOR'S OWN LAW BOOK. I claimed this table was empty and it was not — I had queried the data
    /// root, and the laws live in the REPO's own `.antiloki/db/laws.sqlite`, which is exactly where `load`
    /// looks. This runs against whichever real repo is present so the claim is checked rather than repeated.
    #[test]
    fn the_real_law_book_loads_and_its_regex_laws_judge_a_file() {
        let repo = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        if !repo.join(".antiloki/db/laws.sqlite").exists() { return }   // a fresh clone has none yet
        let laws = load(repo.to_str().unwrap());
        assert!(!laws.is_empty(), "the repo has a law book and load() found nothing");
        let run = runnable(&laws);
        assert!(!run.is_empty(), "not one law in the book can be judged from a diff");

        // a file that breaks the one law every JS codebase has an opinion about
        let breaches = check(&run, &[("src/a.ts".into(), "const x = 1;\nconsole.log(x);\n".into())]);
        assert!(breaches.iter().any(|b| b.line == 2), "console.log on line 2 was not caught: {breaches:?}");

        // and a clean file breaks nothing
        assert!(check(&run, &[("src/b.ts".into(), "export const y = 2;\n".into())]).is_empty());
    }

    #[test]
    fn the_brief_leads_with_the_count_and_the_hard_ones() {
        let l = law("no-console", "regex", json!({ "pattern": "console\\.log" }));
        let b = check(&[l], &[f("src/a.ts", "console.log(1)\nconsole.log(2)\n")]);
        let s = brief(&b);
        assert!(s.starts_with("2 law breaches, 2 hard"), "{s}");
        assert!(s.contains("src/a.ts:1"), "{s}");
    }
}
