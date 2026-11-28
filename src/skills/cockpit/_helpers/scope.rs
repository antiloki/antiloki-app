//! scope.rs — twin of skills/cockpit/_helpers/scope.mjs (helper module). NAMES ARE CONTRACT:
//! literalBase · fileInGlob · inScope · splitByScope · paneScope(db) · globsOverlap. Edit-whitelist scope.
// QUALITY: 81/100 — literal-prefix containment (manual, no regex), claims-or-legacy pane scope
use rusqlite::Connection;
use serde_json::{json, Value};

fn js_str(v: &Value) -> String { match v { Value::String(s) => s.clone(), Value::Null => "null".into(), o => o.to_string() } }
// literalBase: the prefix before the first wildcard char (* ? [)
fn literal_base_str(g: &Value) -> String {
    let s = js_str(g);
    match s.find(|c| c == '*' || c == '?' || c == '[') { Some(i) => s[..i].to_string(), None => s }
}
// norm: String(p ?? '') → '/'-slashes, strip leading './'
fn norm(p: &Value) -> String {
    let s = if p.is_null() { String::new() } else { js_str(p) };
    let s = s.replace('\\', "/");
    s.strip_prefix("./").map(|x| x.to_string()).unwrap_or(s)
}
fn strip_trailing_slash(s: &str) -> String { s.trim_end_matches('/').to_string() }

pub fn literalBase(g: &Value) -> Value { json!(literal_base_str(g)) }

fn file_in_glob(g: &Value, f: &Value) -> bool {
    let base = strip_trailing_slash(&norm(&json!(literal_base_str(g))));
    let file = norm(f);
    if base.is_empty() { return true; }
    file == base || file.starts_with(&format!("{base}/"))
}
pub fn fileInGlob(g: &Value, f: &Value) -> Value { json!(file_in_glob(g, f)) }

pub fn inScope(globs: &Value, f: &Value) -> Value {
    let empty = vec![];
    json!(globs.as_array().unwrap_or(&empty).iter().any(|g| file_in_glob(g, f)))
}

/// antiloki's OWN files in a box (2026-09-19): the bake writes the context pack, the skills, the MCP
/// wiring and the agent briefs INTO the worktree (`.antiloki/`, `.claude/`, `.agents/`, `.codex/`,
/// `.opencode/`, `.mcp.json`, `opencode.json`, `AGENTS.md`, `CLAUDE.md`). They are never the agent's
/// work: a PR leaves them out, and the scope gate does not count them as out-of-scope.
pub fn is_bake_artifact(f: &str) -> bool {
    let f = f.trim_start_matches("./");
    const DIRS: [&str; 5] = [".antiloki/", ".claude/", ".agents/", ".codex/", ".opencode/"];
    const FILES: [&str; 4] = [".mcp.json", "opencode.json", "AGENTS.md", "CLAUDE.md"];
    // THE SEATS' OWN PLACES (review 2026-10-01, b43e8a9): a Cursor or Grok pane gets its skills in `.cursor/skills` / `.grok/skills` and its rules in `.cursor/rules/antiloki.mdc` /
    // `.grok/rules/antiloki.md`. They were not listed, so `git add -A` staged them as the agent's own work and the scope gate (default `block`) refused the PR for files no agent wrote.
    // Only OUR paths, not `.cursor/` or `.grok/` wholesale: a project keeps its own rules there. Read from the one list the bake writes by, so a new seat cannot be forgotten here.
    use crate::cockpit__helpers_seat_files::{ALL_SKILL_DIRS, EXCLUDED_RULES_FILES};
    DIRS.iter().any(|d| f.starts_with(d)) || FILES.contains(&f)
        || ALL_SKILL_DIRS.iter().any(|d| f.strip_prefix(d).map_or(false, |rest| rest.starts_with('/')))
        || EXCLUDED_RULES_FILES.contains(&f)
}
/// the files that are the agent's work — the bake artifacts removed. Pure.
pub fn without_bake_artifacts(files: &Value) -> Value {
    json!(files.as_array().cloned().unwrap_or_default().into_iter().filter(|f| !f.as_str().map(is_bake_artifact).unwrap_or(false)).collect::<Vec<_>>())
}

pub fn splitByScope(globs: &Value, files: &Value) -> Value {
    let farr = files.as_array().cloned().unwrap_or_default();
    match globs.as_array().filter(|a| !a.is_empty()) {
        None => json!({ "in_scope": farr, "out_of_scope": [] }),
        Some(_) => {
            let (mut in_scope, mut out) = (Vec::new(), Vec::new());
            for f in &farr { if file_in_glob_any(globs, f) { in_scope.push(f.clone()); } else { out.push(f.clone()); } }
            json!({ "in_scope": in_scope, "out_of_scope": out })
        }
    }
}
fn file_in_glob_any(globs: &Value, f: &Value) -> bool {
    globs.as_array().map(|a| a.iter().any(|g| file_in_glob(g, f))).unwrap_or(false)
}

pub fn paneScope(db: &Connection, pane: &Value) -> Value {
    let pane_id = pane.get("id").cloned().unwrap_or(Value::Null);
    if let Ok(rows) = crate::db_json::query_rows(db, "SELECT path_glob FROM cockpit_claims WHERE pane_id = ?", &[pane_id]) {
        let claims: Vec<Value> = rows.as_array().unwrap().iter().filter_map(|r| r.get("path_glob").cloned()).collect();
        if !claims.is_empty() { return json!(claims); }
    }
    // legacy fallback: JSON.parse(pane.scope_json ?? '[]'); Array ? map(String) : []
    match pane.get("scope_json").and_then(|v| v.as_str()) {
        Some(s) => match serde_json::from_str::<Value>(s) {
            Ok(Value::Array(a)) => json!(a.iter().map(|x| json!(js_str(x))).collect::<Vec<_>>()),
            _ => json!([]),
        },
        None => json!([]), // null ?? '[]' → []
    }
}

pub fn globsOverlap(a: &Value, b: &Value) -> Value {
    let aa = strip_trailing_slash(&norm(&json!(literal_base_str(a))));
    let bb = strip_trailing_slash(&norm(&json!(literal_base_str(b))));
    if aa.is_empty() || bb.is_empty() { return json!(true); }
    json!(aa == bb || aa.starts_with(&format!("{bb}/")) || bb.starts_with(&format!("{aa}/")))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn scope_semantics() {
        assert_eq!(literalBase(&json!("src/ui/**")), json!("src/ui/"));
        assert_eq!(fileInGlob(&json!("src/ui/**"), &json!("src/ui/a.ts")), json!(true));
        assert_eq!(fileInGlob(&json!("src/ui/**"), &json!("src/api/a.ts")), json!(false));
        assert_eq!(inScope(&json!(["src/ui/**"]), &json!("src/ui/x")), json!(true));
        assert_eq!(splitByScope(&json!(["src/ui/**"]), &json!(["src/ui/a", "src/api/b"]))["in_scope"], json!(["src/ui/a"]));
        assert_eq!(splitByScope(&json!([]), &json!(["a", "b"]))["in_scope"], json!(["a", "b"]));
        assert_eq!(globsOverlap(&json!("src/ui/**"), &json!("src/**")), json!(true));
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE cockpit_claims (pane_id INTEGER, path_glob TEXT); INSERT INTO cockpit_claims VALUES (1,'src/x/**');").unwrap();
        assert_eq!(paneScope(&c, &json!({ "id": 1 })), json!(["src/x/**"]));
    }

    #[test]
    fn bake_artifacts_are_recognized_and_stripped() {
        for f in [".antiloki/context/INDEX.md", ".claude/skills/x/SKILL.md", ".agents/skills/y/SKILL.md", "CLAUDE.md", "AGENTS.md", ".mcp.json", "./opencode.json"] { assert!(is_bake_artifact(f), "{f}"); }
        for f in ["src/CLAUDE.md.bak", "packages/scheduler/README.md", "claude/x", "docs/AGENTS.md"] { assert!(!is_bake_artifact(f), "{f}"); }
        assert_eq!(without_bake_artifacts(&json!(["src/a.ts", ".antiloki/x", "CLAUDE.md"])), json!(["src/a.ts"]));
    }

    /// A CURSOR OR GROK PANE'S OWN FILES DO NOT TRIP THE SCOPE GATE (review 2026-10-01). pr-open stages everything, strips the bake artifacts, then splits what is left by the pane's
    /// scope; before, `.cursor/skills/**` and `.grok/skills/**` and the two rules files stayed in the list and were "out of scope" for every scoped pane. Static composition of the
    /// two helpers pr-open calls (the git half is not run here); the paths come from the lists the bake itself writes by.
    #[test]
    fn the_seats_own_skill_dirs_and_rules_files_are_bake_artifacts_and_never_out_of_scope() {
        use crate::cockpit__helpers_seat_files::{ALL_SKILL_DIRS, EXCLUDED_RULES_FILES};
        let mut staged: Vec<String> = vec!["src/ui/a.ts".into()];
        for d in ALL_SKILL_DIRS { staged.push(format!("{d}/review-diff/SKILL.md")); }
        for f in EXCLUDED_RULES_FILES { staged.push((*f).into()); }
        for f in &staged[1..] { assert!(is_bake_artifact(f), "{f} is ours and must not ride a PR"); }
        let mine = without_bake_artifacts(&json!(staged));
        assert_eq!(mine, json!(["src/ui/a.ts"]));
        assert_eq!(splitByScope(&json!(["src/ui/**"]), &mine)["out_of_scope"], json!([]), "a scoped Cursor pane's PR is not blocked by antiloki's own files");
        // …and the project's OWN files in the same folders are still the agent's work
        for f in [".cursor/rules/project.mdc", ".grok/config.json", ".cursor/mcp.json", ".cursor/skills-notes.md"] { assert!(!is_bake_artifact(f), "{f}"); }
    }
}
