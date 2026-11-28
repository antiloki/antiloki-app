//! interviewer-workspace-brief.rs — twin of skills/agents/in/interviewer-workspace-brief.mjs
//! (db READ lane, cross-db ATTACH). NAMES ARE CONTRACT: `interviewerWorkspaceBrief` (default).
//! Assembles a plain-text SPECIALIST brief about the active workspace: identity (name/root/vcs),
//! Atlas codebase stats + most-depended-on files + entry points, and recent git commits.
//!
//! `context.db` is a required non-nullable reference here (same "guard-throws are not exercised"
//! convention as sibling twins) — the JS `if (!db) return { brief_md: '' }` early-out is not
//! modeled since the Rust standard-lane signature always receives a live `&Connection`.
//!
//! Every section is independently try/caught in JS (a missing table / unattached db just drops
//! that section) — ported as `Result`-returning helpers combined with `if let Ok(..)` so one
//! section's failure never blocks the others, matching the JS try/catch boundaries exactly
//! (the ATLAS block's 3 required counts + top-imports query share ONE `try`, but the nested
//! entry-points query has its OWN inner `try`).
//!
//! `codeGitInfo` (skills/codebase/code-git-info.mjs, twin: `codebase_code_git_info::codeGitInfo`)
//! is called cross-module exactly as the JS `import`s it. That twin is BESPOKE-NATIVE (real `git`
//! subprocess + real fs walk, no fixture/frontier entry of its own) — this fixture only exercises
//! its degrade path by seeding `workspaces.root_path` to a path that does not exist on disk, so
//! both lanes' fs walk / git spawn fail identically (ENOENT) and `is_git` comes back `false`
//! deterministically regardless of the machine or day the harness runs on.
// QUALITY: db-read composition twin — no computed risk score (informational brief assembler)

use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::collections::HashSet;

fn truthy(v: Option<&Value>) -> bool { match v { None|Some(Value::Null)=>false, Some(Value::Bool(b))=>*b, Some(Value::Number(n))=>n.as_f64().map_or(false,|f|f!=0.0&&!f.is_nan()), Some(Value::String(s))=>!s.is_empty(), Some(_)=>true } }
fn tmpl(v: Option<&Value>) -> String { match v { None=>"undefined".into(), Some(Value::Null)=>"null".into(), Some(x)=>str_qq(Some(x)) } } // = JS `${v}`
fn str_qq(v: Option<&Value>) -> String { match v { None|Some(Value::Null)=>String::new(), Some(Value::String(s))=>s.clone(), Some(Value::Bool(b))=>b.to_string(), Some(n@Value::Number(_))=>n.to_string(), Some(o)=>o.to_string() } } // = JS String(v ?? '')
fn js_string(v: &Value) -> String { match v { Value::Null=>"null".into(), Value::String(s)=>s.clone(), Value::Number(n)=>n.to_string(), Value::Bool(b)=>b.to_string(), Value::Array(a)=>a.iter().map(js_string).collect::<Vec<_>>().join(","), Value::Object(_)=>"[object Object]".into() } } // = JS String(v)

// `db.prepare(sql).get()?.c ?? 0` for a bare `COUNT(*) c` query.
fn count_query(db: &Connection, sql: &str) -> Result<i64, String> {
    let rows = crate::db_json::query_rows(db, sql, &[])?;
    let c = rows.as_array().and_then(|a| a.first()).and_then(|r| r.get("c"));
    Ok(match c { None | Some(Value::Null) => 0, Some(v) => v.as_i64().unwrap_or(0) })
}

// `(p) => String(p).split('#')[0].split(/[\\/]/).filter(Boolean).slice(-2).join('/')`
fn short2(v: &Value) -> String {
    let s = js_string(v);
    let first = s.split('#').next().unwrap_or("");
    let segs: Vec<&str> = first.split(|c| c == '\\' || c == '/').filter(|s| !s.is_empty()).collect();
    let n = segs.len();
    let start = if n > 2 { n - 2 } else { 0 };
    segs[start..].join("/")
}

// The ATLAS block: `try { const files = ...; if (files > 0) { ...top-imports... } else { ... } } catch {}`
// files/syms/edges/top-imports share ONE try boundary (any of them throwing drops the whole block).
fn atlas_lines(db: &Connection) -> Result<Vec<String>, String> {
    let mut lines = Vec::new();
    let files = count_query(db, "SELECT COUNT(*) c FROM code.code_nodes WHERE origin='atlas' AND type='file'")?;
    if files > 0 {
        let syms = count_query(db, "SELECT COUNT(*) c FROM code.code_nodes WHERE origin='atlas' AND type<>'file'")?;
        let edges = count_query(db, "SELECT COUNT(*) c FROM code.code_edges WHERE origin='atlas'")?;
        lines.push(format!("CODEBASE (atlas): {} files, {} symbols, {} bindings indexed.", files, syms, edges));

        let top_v = crate::db_json::query_rows(
            db,
            "SELECT to_node, COUNT(*) n FROM code.code_edges WHERE origin='atlas' AND type='imports' GROUP BY to_node ORDER BY n DESC LIMIT 6",
            &[],
        )?;
        let top = top_v.as_array().cloned().unwrap_or_default();
        let mut seen: HashSet<String> = HashSet::new();
        let mut top_paths: Vec<String> = Vec::new();
        for r in &top {
            let p = short2(r.get("to_node").unwrap_or(&Value::Null));
            if !p.is_empty() && seen.insert(p.clone()) { top_paths.push(p); }
        }
        if !top_paths.is_empty() { lines.push(format!("MOST DEPENDED-ON FILES: {}.", top_paths.join(", "))); }

        // nested try: entry-points table absent doesn't drop the lines already computed above.
        if let Ok(eps_v) = crate::db_json::query_rows(
            db,
            "SELECT kind, COUNT(*) n FROM code.code_entry_points WHERE origin='atlas' GROUP BY kind ORDER BY n DESC",
            &[],
        ) {
            let eps = eps_v.as_array().cloned().unwrap_or_default();
            if !eps.is_empty() {
                let parts: Vec<String> = eps.iter().map(|e| format!("{} {}", tmpl(e.get("n")), tmpl(e.get("kind")))).collect();
                lines.push(format!("EXTERNAL SURFACE (entry points): {}.", parts.join(", ")));
            }
        }
    } else {
        lines.push("CODEBASE: not indexed yet — the Atlas hasn't been built for this workspace. For code-aware answers, suggest the user run the Atlas rebuild (Code screen).".to_string());
    }
    Ok(lines)
}

pub fn interviewerWorkspaceBrief(_input: &Value, db: &Connection) -> Result<Value, String> {
    let mut lines: Vec<String> = Vec::new();

    if let Ok(rows) = crate::db_json::query_rows(db, "SELECT name, root_path, vcs FROM workspaces WHERE is_active = 1 LIMIT 1", &[]) {
        if let Some(ws) = rows.as_array().and_then(|a| a.first()) {
            let vcs_suffix = if truthy(ws.get("vcs")) { format!(" · {}", tmpl(ws.get("vcs"))) } else { String::new() };
            lines.push(format!(
                "WORKSPACE: \"{}\" — root {}{}. This is the project the user means by \"this project\".",
                tmpl(ws.get("name")), tmpl(ws.get("root_path")), vcs_suffix
            ));
        }
    }

    if let Ok(atlas) = atlas_lines(db) { lines.extend(atlas); }

    let git = crate::codebase_code_git_info::codeGitInfo(&json!({ "limit": 5 }), db);
    if truthy(git.get("is_git")) {
        if let Some(commits) = git.get("commits").and_then(Value::as_array) {
            if !commits.is_empty() {
                let body = commits.iter()
                    .map(|c| format!("  - {} {} ({}, {})", tmpl(c.get("sha")), tmpl(c.get("subject")), tmpl(c.get("author")), tmpl(c.get("date"))))
                    .collect::<Vec<_>>().join("\n");
                lines.push(format!("RECENT COMMITS:\n{}", body));
            }
        }
    }

    let mut out = Map::new();
    out.insert("brief_md".into(), json!(lines.join("\n")));
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db_with_schema() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE workspaces (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL UNIQUE, \
             root_path TEXT NOT NULL, vcs TEXT NOT NULL DEFAULT 'git', is_active INTEGER NOT NULL DEFAULT 0, \
             created_at TEXT NOT NULL DEFAULT '2026-01-01 00:00:00');
             ATTACH DATABASE ':memory:' AS code;
             CREATE TABLE code.code_nodes (id INTEGER PRIMARY KEY, origin TEXT, type TEXT);
             CREATE TABLE code.code_edges (id INTEGER PRIMARY KEY, origin TEXT, type TEXT, to_node TEXT);
             CREATE TABLE code.code_entry_points (id INTEGER PRIMARY KEY, origin TEXT, kind TEXT);",
        ).unwrap();
        c
    }

    #[test]
    fn brief_with_workspace_and_indexed_atlas() {
        let c = db_with_schema();
        c.execute(
            "INSERT INTO workspaces (name, root_path, vcs, is_active) VALUES ('antiloki', '/nonexistent/antiloki-fixture-workspace-brief-9k2x', 'git', 1)",
            [],
        ).unwrap();
        c.execute_batch(
            "INSERT INTO code.code_nodes (origin, type) VALUES ('atlas','file'),('atlas','file'),('atlas','function'),('atlas','function'),('atlas','function');
             INSERT INTO code.code_edges (origin, type, to_node) VALUES
               ('atlas','imports','src/a/foo.mjs#bar'),('atlas','imports','src/a/foo.mjs#baz'),('atlas','imports','src/b/qux.mjs#x');
             INSERT INTO code.code_entry_points (origin, kind) VALUES ('atlas','action'),('atlas','action'),('atlas','skill');",
        ).unwrap();

        let r = interviewerWorkspaceBrief(&json!({}), &c).unwrap();
        let md = r["brief_md"].as_str().unwrap();
        assert!(md.contains("WORKSPACE: \"antiloki\" — root /nonexistent/antiloki-fixture-workspace-brief-9k2x · git."));
        assert!(md.contains("CODEBASE (atlas): 2 files, 3 symbols, 3 bindings indexed."));
        assert!(md.contains("MOST DEPENDED-ON FILES: ") && md.contains("a/foo.mjs") && md.contains("b/qux.mjs"));
        assert!(md.contains("EXTERNAL SURFACE (entry points): 2 action, 1 skill."));
        assert!(!md.contains("RECENT COMMITS"));
    }

    // NB: an active workspace (pointing at a nonexistent path) is seeded here too — without it,
    // getWorkspaceRoot() falls back to SUBSTRATE_ROOT (this repo checkout, a real git repo),
    // which would make codeGitInfo() append a non-deterministic RECENT COMMITS section.
    #[test]
    fn brief_degrades_when_nothing_is_indexed() {
        let c = db_with_schema();
        c.execute(
            "INSERT INTO workspaces (name, root_path, vcs, is_active) VALUES ('empty-atlas', '/nonexistent/antiloki-fixture-workspace-brief-empty', 'git', 1)",
            [],
        ).unwrap();
        let r = interviewerWorkspaceBrief(&json!({}), &c).unwrap();
        assert_eq!(
            r["brief_md"],
            json!("WORKSPACE: \"empty-atlas\" — root /nonexistent/antiloki-fixture-workspace-brief-empty · git. This is the project the user means by \"this project\".\nCODEBASE: not indexed yet — the Atlas hasn't been built for this workspace. For code-aware answers, suggest the user run the Atlas rebuild (Code screen).")
        );
    }
}
