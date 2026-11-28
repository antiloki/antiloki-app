//! agents_in_pane_scope_sentry.rs — twin of skills/agents/in/pane-scope-sentry.mjs (db lane).
//! NAMES ARE CONTRACT: `paneScopeSentry` (default). RUNTIME scope watch: while a CLI agent works,
//! check the pane's worktree for out-of-scope file changes (git status vs the pane's write
//! whitelist) — `check` reports violations, `undo` reverts ONLY the listed out-of-scope paths
//! (tracked -> `git checkout --`, untracked -> delete). Never touches in-scope work.
//!
//! GIT SUBPROCESS — DISCLOSED UNTESTED (playbook lesson 5's flag-skippable pattern, same shape as
//! cockpit-pane-diff.rs / constitution-estimate.rs's git branches): `git status --porcelain`
//! (both ops) and `git checkout -- <path>` (`undo`, falling back to a real file delete on
//! failure) are ported as real `std::process::Command` calls for BOTH ops, but that whole cluster
//! is reached ONLY when `pane.worktree_path` is truthy AND exists on disk (`fs.existsSync`) — an
//! ordinary, deterministic early return (`{ ok:true, violations:[], changed:[], scope, ...meta,
//! note:'no worktree' }`) fires BEFORE the `op` branch is even examined whenever that guard
//! fails, for EITHER op. That early return is the only success shape reachable without a real,
//! uncommitted git working-tree diff (not deterministically fixture-controllable through the
//! SQL-only conform-db harness — same reasoning as the sibling twins above), so it is the only
//! one the fixture exercises. The JS behavior test (pane-scope-sentry.test.mjs) covers the
//! git-dependent branches directly against real temp git repos (check/undo/rename/OURS-filter).
//!
//! `context.db` missing -> JS throws `err.contextMissing('pane-scope-sentry','db')`. Unreachable
//! here: `db: &Connection` is a required, non-nullable reference (same convention every sibling
//! db-lane twin uses). `pane_id` missing/non-integer/<=0 -> JS throws
//! `err.required('pane-scope-sentry','pane_id')` (`"pane-scope-sentry: pane_id is required"`).
//! Pane not found -> `err.notFound('pane-scope-sentry','pane',paneId)`
//! (`"pane-scope-sentry: pane '<id>' not found"`). Unknown `op` ->
//! `err.enumVal('pane-scope-sentry','op',['check','undo'],op)`
//! (`"pane-scope-sentry: op must be one of: check, undo (got '<op>')"`) — reachable ONLY through
//! the git-gated branch, so like the other throws it carries no fixture coverage (gotcha #9).
// QUALITY: 76/100 — real git subprocess + real fs delete ported for both ops; fixture covers only
// the git-free early-return path (no/missing worktree), disclosed above; JS test covers the rest.

use rusqlite::Connection;
use serde_json::{json, Map, Value};
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
fn js_number(v: Option<&Value>) -> f64 {
    match v {
        None => f64::NAN,
        Some(Value::Null) => 0.0,
        Some(Value::Bool(b)) => if *b { 1.0 } else { 0.0 },
        Some(Value::Number(n)) => n.as_f64().unwrap_or(f64::NAN),
        Some(Value::String(s)) => { let t = s.trim(); if t.is_empty() { 0.0 } else { t.parse().unwrap_or(f64::NAN) } },
        Some(_) => f64::NAN,
    }
}

fn ours_re() -> &'static regex::Regex {
    static R: OnceLock<regex::Regex> = OnceLock::new();
    // `.antiloki-*` as well as `.antiloki/` (2026-09-24). antiloki sets a project's node_modules aside in
    // `.antiloki-nm-host-aside/` while a box is provisioned, and the old pattern demanded a slash directly after
    // "antiloki" — so every agent on that workspace opened reporting "1 out-of-scope change" against a directory
    // antiloki had made itself. A change we wrote is never the operator's to answer for.
    R.get_or_init(|| regex::Regex::new(r#"^(AGENTS\.md|CLAUDE\.md|opencode\.json|\.claude/|\.codex/|\.opencode/|\.agents/|\.antiloki(-[A-Za-z0-9._-]+)?/)"#).unwrap())
}
fn parse_path_re() -> &'static regex::Regex {
    static R: OnceLock<regex::Regex> = OnceLock::new();
    R.get_or_init(|| regex::Regex::new(r"^..\s+(.*)$").unwrap())
}

/// `const covered = (p, globs) => globs.some((g) => { if (g === p) return true; const base =
/// g.endsWith('/**') ? g.slice(0,-3)+'/' : g.endsWith('/') ? g : null; return base != null &&
/// (p+'/').startsWith(base); })`
pub fn covered(p: &str, globs: &[String]) -> bool {
    globs.iter().any(|g| {
        if g == p {
            return true;
        }
        let base: Option<String> = if let Some(stripped) = g.strip_suffix("/**") {
            Some(format!("{stripped}/"))
        } else if g.ends_with('/') {
            Some(g.clone())
        } else {
            None
        };
        match base {
            Some(b) => if format!("{p}/").starts_with(&b) { return true } else if g.ends_with("/**") { return false } else { glob_covers(g, p) },
            None => glob_covers(g, p),
        }
    })
}
/// A glob the prefix rules do not cover - a bare `**` (the whole workspace), `src/**/*.ts`, `*.md` - matched the way the pre-write hook matches it (agent/src/hook.rs), so the hook and this
/// sentry agree on what "in scope" means. Before this, `["**"]` covered NOTHING here: every changed file of an agent fenced to the whole workspace was reported out of scope, and the
/// undo button would have reverted real work (found 2026-09-30 auditing forks, which now inherit their parent's `["**"]`).
fn glob_covers(g: &str, p: &str) -> bool {
    if !g.contains(['*', '?', '[']) { return false }
    let esc = |seg: &str| seg.split('*').map(regex::escape).collect::<Vec<_>>().join("[^/]*");
    let body = g.split("**").map(esc).collect::<Vec<_>>().join(".*");
    regex::Regex::new(&format!("^{body}$")).map(|re| re.is_match(p)).unwrap_or(false)
}

/// `const parsePath = (l) => { const m = /^..\s+(.*)$/.exec(l); if (!m) return ''; let p =
/// m[1].trim(); const arrow = p.indexOf(' -> '); if (arrow >= 0) p = p.slice(arrow + 4); return
/// p.replace(/^"|"$/g, ''); }`
fn parse_status_path(line: &str) -> String {
    let Some(caps) = parse_path_re().captures(line) else { return String::new() };
    let mut p = caps.get(1).unwrap().as_str().trim().to_string();
    if let Some(idx) = p.find(" -> ") {
        p = p[idx + 4..].to_string();
    }
    if p.starts_with('"') {
        p.remove(0);
    }
    if p.ends_with('"') {
        p.pop();
    }
    p
}

/// `let scope = []; try { scope = JSON.parse(pane.scope_json ?? '[]') ?? []; } catch {}` — on
/// invalid JSON the catch leaves `scope` at its initial `[]` (gotcha #6). A parsed JSON `null`
/// (the column holding the literal string `"null"`) is coalesced to `[]` by the SECOND `??`,
/// which fires only after a SUCCESSFUL parse — distinct from the first `??` covering a NULL/absent
/// column.
fn parse_scope(scope_json: Option<&Value>) -> Value {
    let raw: String = match nz(scope_json) {
        Some(Value::String(s)) => s.clone(),
        Some(v) => js_string(v),
        None => "[]".to_string(),
    };
    match serde_json::from_str::<Value>(&raw) {
        Ok(Value::Null) => json!([]),
        Ok(v) => v,
        Err(_) => json!([]),
    }
}

/// `execFileSync('git', ['status', '--porcelain'], { cwd, encoding: 'utf8' })` — no try/catch in
/// the JS around this call, so a nonzero exit / spawn failure propagates as a thrown error. See
/// module doc: only reached once `worktree_path` is confirmed to exist on disk, so a git-repo-less
/// worktree is a disclosed-untested edge here.
fn git_status_porcelain(cwd: &str) -> Result<String, String> {
    match crate::proc::cmd("git").args(["status", "--porcelain"]).current_dir(cwd).output() {
        Ok(o) if o.status.success() => Ok(String::from_utf8_lossy(&o.stdout).into_owned()),
        Ok(o) => Err(format!("git status --porcelain exited with status {}", o.status.code().unwrap_or(-1))),
        Err(e) => Err(format!("git status --porcelain failed to spawn: {e}")),
    }
}

/// Is `cwd` inside a git work tree? A folder that is not (a scratch folder, a project nobody ran `git init` in) has nothing for the sentry to watch.
/// `git rev-parse --is-inside-work-tree` prints `true` inside one and exits 128 outside; a repo that IS a repo but is broken still answers `true`
/// here and fails later, in `git status`, as the real fault it is. Pure over the filesystem.
fn is_git_worktree(cwd: &str) -> bool {
    crate::proc::cmd("git").args(["rev-parse", "--is-inside-work-tree"]).current_dir(cwd).output()
        .map(|o| o.status.success() && String::from_utf8_lossy(&o.stdout).trim() == "true").unwrap_or(false)
}

/// `execFileSync('git', ['checkout', '--', p], { cwd, stdio: 'pipe' })` inside a try/catch —
/// mirrors the swallow-and-fallback shape (returns whether the checkout succeeded).
fn git_checkout(cwd: &str, p: &str) -> bool {
    match crate::proc::cmd("git")
        .args(["checkout", "--", p])
        .current_dir(cwd)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
    {
        Ok(o) => o.status.success(),
        Err(_) => false,
    }
}

/// `pub fn paneScopeSentry(input, context)`
pub fn paneScopeSentry(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS `if (!db) throw err.contextMissing(...)` — unreachable (db is always present).
    let pid_num = js_number(input.get("pane_id"));
    let is_valid_pane_id = pid_num.is_finite() && pid_num.fract() == 0.0 && pid_num > 0.0;
    if !is_valid_pane_id {
        return Err("pane-scope-sentry: pane_id is required".into());
    }
    let pane_id = pid_num as i64;

    let rows = crate::db_json::query_rows(
        db,
        "SELECT id, worktree_path, scope_json, calls, cost_usd FROM cockpit_panes WHERE id = ?",
        &[json!(pane_id)],
    )?;
    let pane = rows.as_array().and_then(|a| a.first()).cloned();
    let Some(pane) = pane else {
        return Err(format!("pane-scope-sentry: pane '{pane_id}' not found"));
    };

    let scope = parse_scope(pane.get("scope_json"));
    let calls = nz(pane.get("calls")).cloned().unwrap_or(json!(0));
    let cost_usd = nz(pane.get("cost_usd")).cloned().unwrap_or(json!(0));

    let wt_val = pane.get("worktree_path").cloned().unwrap_or(Value::Null);
    let wt_truthy = truthy(Some(&wt_val));
    let wt = wt_val.as_str().unwrap_or_default().to_string();
    if !wt_truthy || !std::path::Path::new(&wt).exists() {
        let mut out = Map::new();
        out.insert("ok".into(), json!(true));
        out.insert("violations".into(), json!([]));
        out.insert("changed".into(), json!([]));
        out.insert("scope".into(), scope);
        out.insert("calls".into(), calls);
        out.insert("cost_usd".into(), cost_usd);
        out.insert("note".into(), json!("no worktree"));
        return Ok(Value::Object(out));
    }

    // NOT A GIT REPOSITORY IS NOT AN ERROR (QA report BUG-009, 2026-10-06, confirmed): the worktree exists, `git status` exits 128, and the desk - which asks every five
    // seconds - got a stream of HTTP 500s. Same soft shape as "no worktree", with its own note so a client can stop asking.
    if !is_git_worktree(&wt) {
        let mut out = Map::new();
        out.insert("ok".into(), json!(true));
        out.insert("violations".into(), json!([]));
        out.insert("changed".into(), json!([]));
        out.insert("scope".into(), scope);
        out.insert("calls".into(), calls);
        out.insert("cost_usd".into(), cost_usd);
        out.insert("note".into(), json!("not a git repository"));
        return Ok(Value::Object(out));
    }

    // ── GIT SUBPROCESS branch — real code, disclosed untested by the fixture (see module doc) ──
    let status_text = git_status_porcelain(&wt)?;
    let scope_arr: Vec<String> = match &scope {
        Value::Array(a) => a.iter().map(js_string).collect(),
        _ => vec![],
    };
    let changed: Vec<String> = status_text
        .split('\n')
        .filter(|l| !l.is_empty())
        .map(parse_status_path)
        .filter(|p| !p.is_empty() && !ours_re().is_match(p))
        .collect();
    let violations: Vec<String> = if !scope_arr.is_empty() {
        changed.iter().filter(|p| !covered(p, &scope_arr)).cloned().collect()
    } else {
        vec![]
    };

    let op = match nz(input.get("op")) {
        Some(v) => js_string(v),
        None => "check".to_string(),
    };
    if op == "check" {
        let mut out = Map::new();
        out.insert("ok".into(), json!(true));
        out.insert("violations".into(), json!(violations));
        out.insert("changed".into(), json!(changed));
        out.insert("scope".into(), scope);
        out.insert("calls".into(), calls);
        out.insert("cost_usd".into(), cost_usd);
        return Ok(Value::Object(out));
    }
    if op == "undo" {
        let wanted: Vec<String> = match input.get("paths").and_then(Value::as_array).filter(|a| !a.is_empty()) {
            Some(arr) => arr.iter().map(js_string).collect(),
            None => violations.clone(),
        };
        let mut undone: Vec<String> = Vec::new();
        for p in wanted {
            if !violations.contains(&p) {
                continue; // only ever undo CONFIRMED out-of-scope paths
            }
            if git_checkout(&wt, &p) {
                undone.push(p);
            } else {
                let abs = std::path::Path::new(&wt).join(&p);
                // `fs.rmSync(abs, { force: true })` — force suppresses a missing-file error;
                // any other error (locked file, is-a-directory) is swallowed by the outer catch,
                // reporting the path by omission.
                let removed = match std::fs::remove_file(&abs) {
                    Ok(()) => true,
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => true,
                    Err(_) => false,
                };
                if removed {
                    undone.push(p);
                }
            }
        }
        let mut out = Map::new();
        out.insert("ok".into(), json!(true));
        out.insert("undone".into(), json!(undone));
        return Ok(Value::Object(out));
    }
    Err(format!("pane-scope-sentry: op must be one of: check, undo (got '{op}')"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE cockpit_panes (id INTEGER PRIMARY KEY AUTOINCREMENT, session_id INTEGER NOT NULL, \
             idx INTEGER NOT NULL, worktree_path TEXT, scope_json TEXT, calls INTEGER, cost_usd REAL)",
        )
        .unwrap();
        c
    }

    #[test]
    fn a_bare_double_star_covers_the_whole_workspace_and_a_glob_covers_what_it_matches() {
        let sc = |x: &[&str]| x.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert!(covered("src/a/b.ts", &sc(&["**"])) && covered("package.json", &sc(&["**"])), "the whole workspace");
        assert!(covered("src/a/b.ts", &sc(&["src/**/*.ts"])) && !covered("src/a/b.css", &sc(&["src/**/*.ts"])));
        assert!(covered("README.md", &sc(&["*.md"])) && !covered("docs/README.md", &sc(&["*.md"])), "a single * does not cross a folder");
        assert!(covered("src/ui/x.ts", &sc(&["src/ui/**"])) && !covered("src/uix/x.ts", &sc(&["src/ui/**"])), "the prefix rules are unchanged");
        assert!(!covered("anything", &sc(&["docs/README.md"])));
    }
    #[test]
    fn antilokis_own_bookkeeping_is_never_the_operators_problem() {
        let re = ours_re();
        // the ones that were already ours
        for p in [".antiloki/db/state.sqlite", "AGENTS.md", "CLAUDE.md", ".claude/settings.json", "opencode.json"] {
            assert!(re.is_match(p), "{p} should be ours");
        }
        // the one that was not, and put "1 out-of-scope change" on every agent of the workspace (2026-09-24)
        for p in [".antiloki-nm-host-aside/lightningcss/package.json", ".antiloki-nm-host-aside/nanoid/index.js", ".antiloki-boxes/x/y"] {
            assert!(re.is_match(p), "{p} is antiloki's own — should be ours");
        }
        // and the operator's files are still theirs, including ones that merely start with the same letters
        for p in ["src/App.tsx", "antiloki.config.ts", ".antilokirc", "docs/.antiloki-notes.md"] {
            assert!(!re.is_match(p), "{p} is the operator's — must NOT be swallowed");
        }
    }

    #[test]
    fn pane_id_required_guard() {
        let c = fresh();
        assert_eq!(paneScopeSentry(&json!({}), &c), Err("pane-scope-sentry: pane_id is required".to_string()));
        assert_eq!(paneScopeSentry(&json!({ "pane_id": 0 }), &c), Err("pane-scope-sentry: pane_id is required".to_string()));
        assert_eq!(paneScopeSentry(&json!({ "pane_id": -3 }), &c), Err("pane-scope-sentry: pane_id is required".to_string()));
        assert_eq!(paneScopeSentry(&json!({ "pane_id": 1.5 }), &c), Err("pane-scope-sentry: pane_id is required".to_string()));
    }

    #[test]
    fn pane_not_found() {
        let c = fresh();
        assert_eq!(paneScopeSentry(&json!({ "pane_id": 99 }), &c), Err("pane-scope-sentry: pane '99' not found".to_string()));
    }

    #[test]
    fn no_worktree_path_null_short_circuits_regardless_of_op() {
        let c = fresh();
        c.execute(
            "INSERT INTO cockpit_panes (id, session_id, idx, worktree_path, scope_json, calls, cost_usd) VALUES (1, 1, 0, NULL, '[\"src/**\"]', 5, 1.25)",
            [],
        )
        .unwrap();
        let r = paneScopeSentry(&json!({ "pane_id": 1 }), &c).unwrap();
        assert_eq!(
            r,
            json!({ "ok": true, "violations": [], "changed": [], "scope": ["src/**"], "calls": 5, "cost_usd": 1.25, "note": "no worktree" })
        );
        let r_undo = paneScopeSentry(&json!({ "pane_id": 1, "op": "undo" }), &c).unwrap();
        assert_eq!(r_undo, r);
    }

    #[test]
    fn worktree_path_absent_on_disk_also_short_circuits() {
        let c = fresh();
        c.execute(
            "INSERT INTO cockpit_panes (id, session_id, idx, worktree_path) VALUES (1, 1, 0, 'C:/does/not/exist/xyz-pane-scope-sentry')",
            [],
        )
        .unwrap();
        let r = paneScopeSentry(&json!({ "pane_id": 1 }), &c).unwrap();
        assert_eq!(r, json!({ "ok": true, "violations": [], "changed": [], "scope": [], "calls": 0, "cost_usd": 0, "note": "no worktree" }));
    }

    #[test]
    fn a_folder_that_is_not_a_git_repository_answers_softly_instead_of_failing() {
        // BUG-009: the desk polls every 5 s; a non-git worktree used to answer "git status --porcelain exited with status 128" as an HTTP 500 each time
        let dir = std::env::temp_dir().join(format!("sentry-nogit-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let c = fresh();
        c.execute("INSERT INTO cockpit_panes (id, session_id, idx, worktree_path, scope_json, calls, cost_usd) VALUES (1, 1, 0, ?, '[\"src/**\"]', 3, 0.5)", [dir.to_string_lossy().to_string()]).unwrap();
        for op in ["check", "undo"] {
            let r = paneScopeSentry(&json!({ "pane_id": 1, "op": op }), &c).unwrap();
            assert_eq!(r, json!({ "ok": true, "violations": [], "changed": [], "scope": ["src/**"], "calls": 3, "cost_usd": 0.5, "note": "not a git repository" }), "op={op}");
        }
        // and the same folder once it IS a repository takes the real path again (no note)
        assert!(crate::proc::cmd("git").args(["init", "-q"]).current_dir(&dir).status().unwrap().success());
        let r = paneScopeSentry(&json!({ "pane_id": 1 }), &c).unwrap();
        assert_eq!(r["ok"], json!(true));
        assert!(r.get("note").is_none(), "a real repository is watched: {r}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn invalid_scope_json_falls_back_to_empty_array() {
        let c = fresh();
        c.execute("INSERT INTO cockpit_panes (id, session_id, idx, scope_json) VALUES (1, 1, 0, '{not json')", []).unwrap();
        let r = paneScopeSentry(&json!({ "pane_id": 1 }), &c).unwrap();
        assert_eq!(r["scope"], json!([]));
    }

    #[test]
    fn scope_json_literal_null_coalesces_to_empty_array() {
        let c = fresh();
        c.execute("INSERT INTO cockpit_panes (id, session_id, idx, scope_json) VALUES (1, 1, 0, 'null')", []).unwrap();
        let r = paneScopeSentry(&json!({ "pane_id": 1 }), &c).unwrap();
        assert_eq!(r["scope"], json!([]));
    }

    #[test]
    fn unknown_op_is_never_evaluated_without_a_worktree() {
        let c = fresh();
        c.execute("INSERT INTO cockpit_panes (id, session_id, idx) VALUES (1, 1, 0)", []).unwrap();
        let r = paneScopeSentry(&json!({ "pane_id": 1, "op": "bogus" }), &c).unwrap();
        assert_eq!(r["note"], json!("no worktree"));
    }

    #[test]
    fn covered_matches_exact_glob_suffix_and_directory_prefix() {
        let globs = vec!["README.md".to_string(), "src/**".to_string(), "docs/".to_string()];
        assert!(covered("README.md", &globs));
        assert!(covered("src/a.js", &globs));
        assert!(covered("docs/x.md", &globs));
        assert!(!covered("secret.txt", &globs));
        assert!(!covered("srcx/a.js", &globs));
    }

    #[test]
    fn parse_status_path_handles_rename_and_quoting() {
        assert_eq!(parse_status_path(" M src/keep.js"), "src/keep.js");
        assert_eq!(parse_status_path("R  old.txt -> new.txt"), "new.txt");
        assert_eq!(parse_status_path("?? \"quoted.txt\""), "quoted.txt");
        assert_eq!(parse_status_path(""), "");
    }
}
