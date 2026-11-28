//! goal-derive.rs — twin of skills/swarm/goal-derive.mjs (no-db compute). NAMES ARE CONTRACT:
//! deriveGoal (no-db helper) · goalDerive (default, db lane; ignores db — pure derivation).
//! Derives the single measurable goal_line from machine-decidable acceptance criteria
//! (file-exists / grep / a runnable test command); non-decidable (prose) kinds are excluded.
//! COVERAGE mode (scope.coverage === true) reframes each clause as what a PLAN must cover.
use regex::Regex;
use rusqlite::Connection;
use serde_json::{json, Value};
use std::sync::OnceLock;

fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}
// String(v ?? '')
fn str_qq(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(),
        Some(o) => o.to_string(),
    }
}
// String(v) (no ??)
fn js_string(v: &Value) -> String {
    match v {
        Value::Null => "null".into(),
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Array(a) => a.iter().map(js_string).collect::<Vec<_>>().join(","),
        Value::Object(_) => "[object Object]".into(),
    }
}
fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) }

// norm(p) = String(p ?? '').trim().replace(/^\.?\//, '') — strips one leading "./" or "/".
fn norm(s: &str) -> String {
    let t = s.trim();
    if let Some(rest) = t.strip_prefix("./") { rest.to_string() }
    else if let Some(rest) = t.strip_prefix('/') { rest.to_string() }
    else { t.to_string() }
}

fn file_matches(acc_path: &str, scope_file: &str) -> bool {
    let a = norm(acc_path);
    let f = norm(scope_file);
    if a.is_empty() || f.is_empty() { return false; }
    a == f || a.ends_with(&format!("/{f}")) || f.ends_with(&format!("/{a}"))
}

// PATH_TOKEN_RE = /(?:^|[\s`"'(])((?:[\w.@-]+\/)+[\w.@-]+\.\w{1,6})(?=$|[\s`"'),.:;])/ — the
// `regex` crate has no look-around (gotcha #7). Hand-rolled: try every start position whose
// preceding char (or start-of-string) is an allowed border, anchor-match the core token there,
// then accept only if the char right after the match is itself an allowed trailing border.
fn path_token_core() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"^(?:[\w.@-]+/)+[\w.@-]+\.\w{1,6}").unwrap())
}
fn is_path_prefix_border(c: char) -> bool { c.is_whitespace() || matches!(c, '`' | '"' | '\'' | '(') }
fn is_path_suffix_border(c: char) -> bool { c.is_whitespace() || matches!(c, '`' | '"' | '\'' | ')' | '.' | ',' | ':' | ';') }
fn path_token_re(text: &str) -> Option<String> {
    let chars: Vec<char> = text.chars().collect();
    for i in 0..chars.len() {
        if i > 0 && !is_path_prefix_border(chars[i - 1]) { continue; }
        let rest: String = chars[i..].iter().collect();
        let m = match path_token_core().find(&rest) { Some(m) => m, None => continue };
        let matched_len = m.as_str().chars().count();
        let next = chars.get(i + matched_len).copied();
        if next.map_or(true, is_path_suffix_border) { return Some(m.as_str().to_string()); }
    }
    None
}

// REFINE's acceptance shape carries no path/cmd fields — the file target lives inside the
// done_criteria/intent prose. Pull the first path-looking token out as a fallback target.
fn path_from_acceptance(ac: &Value) -> String {
    for key in ["path", "file"] {
        if let Some(v) = ac.get(key) {
            if truthy(Some(v)) { return js_string(v); }
        }
    }
    for key in ["done_criteria", "intent"] {
        let s = str_qq(ac.get(key));
        if let Some(p) = path_token_re(&s) { return p; }
    }
    String::new()
}

// GREP_RE = /\bgrep\b[^\n]*?(['"])([\s\S]*?)\1\s+(\S+)/ — backreference \1 (unsupported by
// `regex`, gotcha #7). Hand-rolled: find "grep", scan the SAME line for the first quote char,
// then grow the search window to the nearest LATER occurrence of that same quote char whose
// tail also matches `\s+\S+` (mirrors lazy-backtracking: keep extending until the tail fits).
fn word_grep() -> &'static Regex { static R: OnceLock<Regex> = OnceLock::new(); R.get_or_init(|| Regex::new(r"\bgrep\b").unwrap()) }
fn grep_re_exec(cmd: &str) -> Option<(String, String)> {
    let chars: Vec<char> = cmd.chars().collect();
    for m in word_grep().find_iter(cmd) {
        let start = cmd[..m.end()].chars().count();
        let mut i = start;
        let mut qpos = None;
        let mut qchar = '\0';
        while i < chars.len() && chars[i] != '\n' {
            if chars[i] == '\'' || chars[i] == '"' { qpos = Some(i); qchar = chars[i]; break; }
            i += 1;
        }
        let qpos = match qpos { Some(p) => p, None => continue };
        let mut from = qpos + 1;
        while from < chars.len() {
            let rel = match chars[from..].iter().position(|&c| c == qchar) { Some(r) => r, None => break };
            let close = from + rel;
            let pattern: String = chars[qpos + 1..close].iter().collect();
            let mut j = close + 1;
            let ws_start = j;
            while j < chars.len() && chars[j].is_whitespace() { j += 1; }
            if j > ws_start {
                let path_start = j;
                while j < chars.len() && !chars[j].is_whitespace() { j += 1; }
                if j > path_start {
                    let path: String = chars[path_start..j].iter().collect();
                    return Some((pattern, path));
                }
            }
            from = close + 1;
        }
    }
    None
}

pub fn deriveGoal(acceptance: &Value, scope: &Value) -> Value {
    let acc: Vec<Value> = acceptance.as_array().cloned().unwrap_or_default();
    let kind = match nz(scope.get("kind")) { Some(v) => js_string(v), None => "card".to_string() };
    let file: Value = nz(scope.get("file")).cloned().unwrap_or(Value::Null);
    let file_str = str_qq(Some(&file));
    let file_scoped = kind == "microtask" || kind == "snippet";
    let mut clauses: Vec<Value> = Vec::new();

    for ac in &acc {
        let k = str_qq(ac.get("kind")).to_lowercase();
        if k == "file_exists" || k == "file-exists" {
            let p = norm(&path_from_acceptance(ac));
            if p.is_empty() { continue; }
            if file_scoped && !file_matches(&p, &file_str) { continue; }
            clauses.push(json!({ "kind": "file-exists", "text": format!("the file {p} exists"), "coverage": format!("the design creates the file {p}") }));
        } else if k == "grep" {
            let p = norm(&path_from_acceptance(ac));
            if file_scoped && !p.is_empty() && !file_matches(&p, &file_str) { continue; }
            let crit_val = nz(ac.get("done_criteria")).or_else(|| nz(ac.get("intent")));
            let crit_raw = str_qq(crit_val);
            let crit: String = crit_raw.trim().chars().take(140).collect();
            if crit.is_empty() { continue; }
            let target = if p.is_empty() { "the change".to_string() } else { p };
            clauses.push(json!({ "kind": "grep", "text": format!("{target} satisfies: {crit}"), "coverage": format!("the design covers: {crit}") }));
        } else if k == "test" {
            let cmd_val = nz(ac.get("cmd")).or_else(|| nz(ac.get("command")));
            let cmd = str_qq(cmd_val);
            if let Some((pattern, path)) = grep_re_exec(&cmd) {
                if file_scoped && !file_matches(&path, &file_str) { continue; }
                let p = norm(&path);
                let pat_json = serde_json::to_string(&Value::String(pattern)).unwrap_or_default();
                clauses.push(json!({ "kind": "grep", "text": format!("{p} contains {pat_json}"), "coverage": format!("the design has {p} provide {pat_json}") }));
            } else if !file_scoped {
                let c = cmd.trim();
                if !c.is_empty() {
                    clauses.push(json!({ "kind": "command", "text": format!("`{c}` exits 0"), "coverage": format!("the design includes the work to make `{c}` pass") }));
                }
            }
        }
        // other kinds: not machine-decidable -> excluded from the measurable goal
    }

    let use_coverage = matches!(scope.get("coverage"), Some(Value::Bool(true)));
    let goal_line = clauses.iter().map(|c| {
        let field = if use_coverage { "coverage" } else { "text" };
        c.get(field).and_then(|v| v.as_str()).unwrap_or("").to_string()
    }).collect::<Vec<_>>().join(" AND ");
    let microtask_id = nz(scope.get("microtask_id")).cloned().unwrap_or(Value::Null);
    let measurable = !clauses.is_empty();

    json!({
        "goal_line": goal_line,
        "clauses": clauses,
        "measurable": measurable,
        "scope": { "kind": kind, "file": file, "microtask_id": microtask_id },
    })
}

pub fn goalDerive(input: &Value, _db: &Connection) -> Result<Value, String> {
    let acceptance = match input.get("acceptance") {
        Some(v @ Value::Array(_)) => v.clone(),
        _ => return Err("goal-derive: acceptance is required".into()),
    };
    let scope = nz(input.get("scope")).cloned().unwrap_or_else(|| json!({}));
    Ok(deriveGoal(&acceptance, &scope))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn acceptance5() -> Value {
        json!([
            { "kind": "file-exists", "path": "src/config.js" },
            { "kind": "file-exists", "path": "src/server.js" },
            { "kind": "test", "cmd": "grep -q '\"strict\": true' src/config.js" },
            { "kind": "test", "cmd": "cd app && npm run build", "expect_exit": 0 },
            { "kind": "behavior", "text": "the server feels fast" }
        ])
    }

    #[test]
    fn refine_shaped_acceptance_extracts_path_from_prose() {
        let refine_acc = json!([
            { "id": "ac1", "kind": "file-exists", "intent": "Create the component", "done_criteria": "File src/components/ClickMeButton.jsx exists" },
            { "id": "ac2", "kind": "grep", "intent": "Import React and styled-components", "done_criteria": "React and styled-components are imported at the top of src/components/ClickMeButton.jsx" },
            { "id": "ac3", "kind": "grep", "intent": "centered container", "done_criteria": "Styled component centers content using flexbox" }
        ]);
        let g = deriveGoal(&refine_acc, &json!({ "kind": "card", "coverage": true }));
        assert_eq!(g["measurable"], json!(true));
        assert_eq!(g["clauses"].as_array().unwrap().len(), 3);
        assert!(g["goal_line"].as_str().unwrap().contains("the design creates the file src/components/ClickMeButton.jsx"));
        assert!(g["goal_line"].as_str().unwrap().contains("the design covers: React and styled-components are imported"));
        assert!(g["goal_line"].as_str().unwrap().contains("the change satisfies") == false); // grep target found for ac2
    }

    #[test]
    fn unlocatable_file_target_never_yields_empty_clause() {
        let g = deriveGoal(&json!([{ "kind": "file-exists", "intent": "make it", "done_criteria": "the thing exists somewhere" }]), &json!({ "kind": "card", "coverage": true }));
        assert_eq!(g["measurable"], json!(false));
        assert_eq!(g["clauses"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn file_scoped_uses_only_matching_acceptances() {
        let fs = deriveGoal(&acceptance5(), &json!({ "kind": "microtask", "file": "src/config.js" }));
        assert_eq!(fs["measurable"], json!(true));
        assert!(fs["goal_line"].as_str().unwrap().contains("the file src/config.js exists"));
        assert!(fs["goal_line"].as_str().unwrap().contains("src/config.js contains"));
        assert!(!fs["goal_line"].as_str().unwrap().contains("server.js"));
        assert!(!fs["goal_line"].as_str().unwrap().contains("npm run build"));
    }

    #[test]
    fn card_scope_includes_global_command() {
        let cs = deriveGoal(&acceptance5(), &json!({ "kind": "card" }));
        assert!(cs["goal_line"].as_str().unwrap().contains("`cd app && npm run build` exits 0"));
        assert!(cs["goal_line"].as_str().unwrap().contains("the file src/server.js exists"));
        assert!(!cs["goal_line"].as_str().unwrap().contains("feels fast"));
    }

    #[test]
    fn coverage_mode_reframes_runtime_clauses() {
        let g = deriveGoal(&acceptance5(), &json!({ "kind": "card", "coverage": true }));
        assert!(g["goal_line"].as_str().unwrap().contains("the design creates the file src/server.js"));
        assert!(g["goal_line"].as_str().unwrap().contains("the design includes the work to make `cd app && npm run build` pass"));
        assert!(!g["goal_line"].as_str().unwrap().contains("exits 0"));
    }

    #[test]
    fn test_kind_with_no_shell_command_is_skipped() {
        let g = deriveGoal(&json!([{ "id": "ac6", "kind": "test", "done_criteria": "Atlas verification shows 0 broken transitions" }]), &json!({ "kind": "card" }));
        assert_eq!(g["goal_line"], json!(""));
        assert_eq!(g["measurable"], json!(false));
    }

    #[test]
    fn scope_omitted_defaults_like_js_default_param() {
        let with_scope = deriveGoal(&acceptance5(), &json!({ "kind": "card" }));
        let without_scope = deriveGoal(&acceptance5(), &Value::Null);
        assert_eq!(with_scope, without_scope);
    }

    #[test]
    fn default_export_requires_acceptance_array_and_delegates() {
        let conn = Connection::open_in_memory().unwrap();
        assert!(goalDerive(&json!({}), &conn).is_err());
        let g = goalDerive(&json!({ "acceptance": acceptance5(), "scope": { "kind": "snippet", "file": "src/server.js" } }), &conn).unwrap();
        assert_eq!(g["goal_line"], json!("the file src/server.js exists"));
    }
}
