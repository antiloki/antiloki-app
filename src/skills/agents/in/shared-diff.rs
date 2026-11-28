//! agents_in_shared_diff.rs — twin of skills/agents/in/shared-diff.mjs (db lane, READ-ONLY).
//! NAMES ARE CONTRACT: `sharedDiff` (default). The SHARED GIT DIFF across a chat session's agent
//! panes: resolves the cockpit session, lists each pane's files-touched matrix, finds files
//! overlapping across >=2 panes, runs a DETERMINISTIC `git merge-tree` dry-run per overlapping
//! pair to classify conflict status, and proposes a fewest-conflicts-first integration order.
//!
//! GIT SUBPROCESS — DISCLOSED UNTESTED (playbook lesson 5's flag-skippable pattern, same shape as
//! pane-scope-sentry.rs / constitution-estimate.rs's git branches): every git call
//! (`rev-parse --git-common-dir`, `rev-parse HEAD`, `merge-base`, `diff --name-only`,
//! `status --porcelain`, `merge-tree --write-tree`) is ported as real `std::process::Command`
//! calls, but the WHOLE per-pane git block fires only for a `cockpit_panes` row whose
//! `worktree_path` is truthy AND exists on disk (`fs.existsSync`) — an ordinary, deterministic
//! filter (`rows.filter((p) => p.worktree_path && fs.existsSync(p.worktree_path))`) applied
//! BEFORE any subprocess runs. When that filter yields zero surviving rows, `panes` stays `[]`
//! and every downstream computation (overlaps/conflicts/proposal) collapses to its trivial empty
//! shape — the only success shape deterministically reachable through the SQL-only conform-db
//! harness (not deterministically fixture-controllable — same reasoning as the sibling twins
//! above: a real git working-tree/branch state is not something a committed fixture JSON can
//! pin). The JS behavior test (shared-diff.test.mjs, replacing its `// SCAFFOLD TEST`) covers the
//! git-dependent branches directly against real temp git repos (overlap detection, uncommitted
//! status, clean-merge vs real conflict, the OURS artifact filter, and the proposal ordering).
//!
//! `context.db` missing -> JS throws `err.contextMissing('shared-diff', 'db')`. Unreachable here:
//! `db: &Connection` is a required, non-nullable reference (same convention every sibling db-lane
//! twin uses).
//!
//! Session resolution (`sessionId`) mirrors JS's falsy-vs-nullish distinction precisely: an
//! explicit `session_id` of `0` (or any NaN-producing input) is FALSY, so it still falls through
//! to the `chat_session_id` lookup — modeled via `num_truthy` (0 and NaN both count as absent),
//! distinct from the `!= null` presence checks (`Option::filter(|v| !v.is_null())`) that gate
//! whether each branch is even attempted.
//!
//! `panes.map(({ dirty, worktree, mb, ... }) => p)` (strips 3 internal-only keys before the
//! response): modeled by building the output pane object directly from the 5 surviving fields
//! (`pane_id, name, kind, branch, files`) in that exact order, rather than round-tripping through
//! a full struct-to-map-then-remove step — same result, same field order.
// QUALITY: 76/100 — real git subprocess (7 distinct calls) ported for the full per-pane analysis
// + the merge-tree conflict classifier; fixture covers only the git-free session-resolution +
// empty-panes paths, disclosed above; JS test covers the rest against real temp git repos.

use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
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
fn str_qq(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(),
        Some(o) => o.to_string(),
    }
} // = JS String(v ?? '')
fn tmpl(v: Option<&Value>) -> String {
    match v {
        None => "undefined".into(),
        Some(Value::Null) => "null".into(),
        Some(x) => str_qq(Some(x)),
    }
} // = JS `${v}`
fn js_number(v: Option<&Value>) -> f64 {
    match v {
        None => f64::NAN,
        Some(Value::Null) => 0.0,
        Some(Value::Bool(b)) => if *b { 1.0 } else { 0.0 },
        Some(Value::Number(n)) => n.as_f64().unwrap_or(f64::NAN),
        Some(Value::String(s)) => { let t = s.trim(); if t.is_empty() { 0.0 } else { t.parse().unwrap_or(f64::NAN) } }
        Some(_) => f64::NAN,
    }
} // = JS Number(v)

// JS truthiness of a Number(...) result: 0 and NaN are both falsy.
fn num_truthy(f: Option<f64>) -> bool { f.map_or(false, |v| v != 0.0 && !v.is_nan()) }

// JS `${5}` -> "5", never "5.0" — render a whole-valued f64 (e.g. Number(input.session_id) or a
// row's INTEGER id round-tripped through f64) as an integer; keep genuinely fractional values as
// floats (not reachable by any realistic session_id here, but matches JS Number semantics).
fn json_num_like_js(f: f64) -> Value {
    if f.is_finite() && f.fract() == 0.0 && f.abs() < 9_007_199_254_740_992.0 {
        json!(f as i64)
    } else {
        json!(f)
    }
}

fn ours_re() -> &'static regex::Regex {
    static R: OnceLock<regex::Regex> = OnceLock::new();
    R.get_or_init(|| regex::Regex::new(r#"^(AGENTS\.md|CLAUDE\.md|opencode\.json|\.claude/|\.agents/)"#).unwrap())
}
fn parse_path_re() -> &'static regex::Regex {
    static R: OnceLock<regex::Regex> = OnceLock::new();
    R.get_or_init(|| regex::Regex::new(r"^..\s+(.*)$").unwrap())
}

/// `const parsePath = (l) => { const m = /^..\s+(.*)$/.exec(l); if (!m) return ''; let pp =
/// m[1].trim(); const arrow = pp.indexOf(' -> '); if (arrow >= 0) pp = pp.slice(arrow + 4); return
/// pp.replace(/^"|"$/g, ''); }` — robust `git status --porcelain` parse (XY + whitespace + path;
/// renames keep the NEW path; surrounding quotes stripped independently on each side).
fn parse_status_path(line: &str) -> String {
    let Some(caps) = parse_path_re().captures(line) else { return String::new() };
    let mut pp = caps.get(1).unwrap().as_str().trim().to_string();
    if let Some(idx) = pp.find(" -> ") {
        pp = pp[idx + 4..].to_string();
    }
    if pp.starts_with('"') {
        pp.remove(0);
    }
    if pp.ends_with('"') {
        pp.pop();
    }
    pp
}

/// `node:path` win32 `isAbsolute` — the dev/CI platform for this substrate (same disclosed
/// win32-only scope as constitution-estimate.rs's private copy).
fn win_is_absolute(p: &str) -> bool {
    let b = p.as_bytes();
    if b.is_empty() { return false; }
    if b[0] == b'/' || b[0] == b'\\' { return true; }
    if b.len() > 2 && b[0].is_ascii_alphabetic() && b[1] == b':' && (b[2] == b'/' || b[2] == b'\\') { return true; }
    false
}

/// `const git = (cwd, args) => execFileSync('git', args, { cwd, encoding: 'utf8', stdio: [...]
/// }).trim();` — throws (here: `Err`) on a nonzero exit or spawn failure, exactly like
/// `execFileSync` without a `{ ignoreReturnCode: true }`-style option.
///
/// VERIFIED REAL QUIRK (confirmed against real `git status --porcelain` output while writing
/// shared-diff.test.mjs, which replaces this skill's `// SCAFFOLD TEST`): `.trim()` strips
/// whitespace off the WHOLE captured string, not per-line — so when the status output's very
/// first line starts with a space (an unstaged-only entry, e.g. `" M file.txt"`), that leading
/// space is eaten and `parse_status_path`'s `^..\s+` regex fails to match JUST that first line
/// (the two mandatory chars become `"M "` — a space then a non-whitespace filename char — with no
/// whitespace left for `\s+`). Every OTHER line is untouched. This is real production JS
/// behavior, reproduced verbatim here (not a bug introduced in this twin) — see the module doc.
fn git(cwd: &str, args: &[&str]) -> Result<String, String> {
    match crate::proc::cmd("git").args(args).current_dir(cwd).output() {
        Ok(o) if o.status.success() => Ok(String::from_utf8_lossy(&o.stdout).trim().to_string()),
        Ok(o) => Err(format!("git {} exited with status {}", args.join(" "), o.status.code().unwrap_or(-1))),
        Err(e) => Err(format!("git {} failed to spawn: {e}", args.join(" "))),
    }
}

/// `execFileSync('git', ['merge-tree', '--write-tree', '--no-messages', a, b], { cwd, stdio:
/// 'pipe' })` — on failure the JS reads `e?.stdout` (git prints the conflicting hunk/paths there)
/// and `e?.status` (nonzero exit code); modeled as an `Err((stdout, exit_code))` tuple so the
/// caller can replicate `out.includes(file) || e?.status === 1` verbatim.
fn git_merge_tree(cwd: &str, branch_a: &str, branch_b: &str) -> Result<(), (String, Option<i32>)> {
    match crate::proc::cmd("git")
        .args(["merge-tree", "--write-tree", "--no-messages", branch_a, branch_b])
        .current_dir(cwd)
        .output()
    {
        Ok(o) if o.status.success() => Ok(()),
        Ok(o) => Err((String::from_utf8_lossy(&o.stdout).into_owned(), o.status.code())),
        Err(_) => Err((String::new(), None)),
    }
}

struct PaneCtx {
    pane_id: i64,
    name: Value,
    kind: Value,
    branch: Value,
    worktree: String,
}

struct PaneResult {
    pane_id: i64,
    name: Value,
    kind: Value,
    branch: Value,
    worktree: String,
    dirty: HashSet<String>,
    files: Vec<String>,
}
impl PaneResult {
    fn branch_str(&self) -> &str { self.branch.as_str().unwrap_or("") }
}

/// One pane's git analysis (`try { ... } catch { /* worktree unreadable — skip honestly */ }`):
/// `None` means the whole pane is dropped from `panes` (matches JS's outer catch skipping the
/// `panes.push(...)`). The inner `merge-base` failure ("detached") is narrower — only that one
/// call falls back to `baseHead`, everything after it still runs.
fn process_pane(ctx: &PaneCtx) -> Option<PaneResult> {
    let wt = ctx.worktree.as_str();
    let common = git(wt, &["rev-parse", "--git-common-dir"]).ok()?;
    let common_dir: PathBuf = if win_is_absolute(&common) { PathBuf::from(&common) } else { Path::new(wt).join(&common) };
    let root: PathBuf = common_dir.parent().map(|p| p.to_path_buf()).unwrap_or(common_dir);
    let root_str = root.to_string_lossy().into_owned();

    let base_head = git(&root_str, &["rev-parse", "HEAD"]).ok()?;
    let mb = git(wt, &["merge-base", "HEAD", base_head.as_str()]).unwrap_or_else(|_| base_head.clone());

    let committed: Vec<String> = git(wt, &["diff", "--name-only", mb.as_str(), "HEAD"]).ok()?
        .split('\n').map(str::to_string).filter(|s| !s.is_empty()).collect();

    let status_text = git(wt, &["status", "--porcelain"]).ok()?;
    let dirty_vec: Vec<String> = status_text
        .split('\n')
        .filter(|l| !l.is_empty())
        .map(parse_status_path)
        .filter(|p| !p.is_empty())
        .collect();

    let mut seen: HashSet<String> = HashSet::new();
    let mut files: Vec<String> = Vec::new();
    for f in committed.iter().chain(dirty_vec.iter()) {
        if seen.insert(f.clone()) { files.push(f.clone()); }
    }
    let files: Vec<String> = files.into_iter().filter(|f| !f.is_empty() && !ours_re().is_match(f)).collect();

    Some(PaneResult {
        pane_id: ctx.pane_id,
        name: ctx.name.clone(),
        kind: ctx.kind.clone(),
        branch: ctx.branch.clone(),
        worktree: ctx.worktree.clone(),
        dirty: dirty_vec.into_iter().collect(),
        files,
    })
}

/// `pub fn sharedDiff(input, context)`
pub fn sharedDiff(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS `if (!db) throw err.contextMissing(...)` — unreachable (db is always present).
    let mut session_id: Option<f64> = input.get("session_id").filter(|v| !v.is_null()).map(|v| js_number(Some(v)));

    if !num_truthy(session_id) {
        if let Some(chat_id) = input.get("chat_session_id").filter(|v| !v.is_null()) {
            let name = format!("antichat#{}", tmpl(Some(chat_id)));
            let rows = crate::db_json::query_rows(
                db,
                "SELECT id FROM cockpit_sessions WHERE name = ? AND status != 'closed' ORDER BY id ASC LIMIT 1",
                &[json!(name)],
            )?;
            session_id = rows.as_array().and_then(|a| a.first()).and_then(|r| r.get("id")).and_then(Value::as_f64);
        }
    }

    if !num_truthy(session_id) {
        let mut out = Map::new();
        out.insert("ok".into(), json!(true));
        out.insert("session_id".into(), Value::Null);
        out.insert("panes".into(), json!([]));
        out.insert("overlaps".into(), json!([]));
        out.insert("conflicts".into(), json!([]));
        out.insert("proposal".into(), json!({ "order": [], "note": "no cockpit session yet — open an agent pane first" }));
        return Ok(Value::Object(out));
    }
    let session_id_val = session_id.unwrap();

    // a CLOSED pane's box may still be on disk: it is not an agent of the session any more (2026-11-11: the Diff pane listed thirteen agents with one open). Older tables have no `status`.
    let has_status = db.prepare("SELECT 1 FROM pragma_table_info('cockpit_panes') WHERE name = 'status'").ok().and_then(|mut q| q.exists([]).ok()).unwrap_or(false);
    let pane_sql = if has_status { "SELECT id, subagent_name, pane_kind, branch, worktree_path FROM cockpit_panes WHERE session_id = ? AND COALESCE(status, '') != 'closed'" } else { "SELECT id, subagent_name, pane_kind, branch, worktree_path FROM cockpit_panes WHERE session_id = ?" };
    let rows = crate::db_json::query_rows(
        db,
        pane_sql,
        &[json_num_like_js(session_id_val)],
    )?;
    let filtered: Vec<&Value> = rows
        .as_array()
        .map(|a| {
            a.iter()
                .filter(|p| {
                    let wt = p.get("worktree_path");
                    truthy(wt) && wt.and_then(Value::as_str).map_or(false, |s| Path::new(s).exists())
                })
                .collect()
        })
        .unwrap_or_default();

    let mut panes: Vec<PaneResult> = Vec::new();
    for p in filtered {
        let ctx = PaneCtx {
            pane_id: p.get("id").and_then(Value::as_i64).unwrap_or(0),
            name: p.get("subagent_name").cloned().unwrap_or(Value::Null),
            kind: p.get("pane_kind").cloned().unwrap_or(Value::Null),
            branch: p.get("branch").cloned().unwrap_or(Value::Null),
            worktree: p.get("worktree_path").and_then(Value::as_str).unwrap_or("").to_string(),
        };
        if let Some(pr) = process_pane(&ctx) {
            panes.push(pr);
        }
    }

    // ── overlaps: any file changed by >=2 panes (Map insertion order == first-seen file order) ──
    let mut file_order: Vec<String> = Vec::new();
    let mut by_file: std::collections::HashMap<String, Vec<usize>> = std::collections::HashMap::new();
    for (pi, p) in panes.iter().enumerate() {
        for f in &p.files {
            by_file.entry(f.clone()).or_insert_with(|| { file_order.push(f.clone()); Vec::new() }).push(pi);
        }
    }

    struct ConflictPair { a: i64, b: i64, files: Vec<String> }
    let mut conflict_map: std::collections::HashMap<(i64, i64), usize> = std::collections::HashMap::new();
    let mut conflict_list: Vec<ConflictPair> = Vec::new();
    let mut overlaps: Vec<Value> = Vec::new();

    for file in &file_order {
        let idxs = &by_file[file];
        if idxs.len() < 2 { continue; }
        let ps: Vec<&PaneResult> = idxs.iter().map(|&i| &panes[i]).collect();

        let mut status = "clean-merge".to_string();
        if ps.iter().any(|p| p.dirty.contains(file)) {
            status = "uncommitted".to_string();
        } else {
            'outer: for i in 0..ps.len() {
                if status == "conflict" { break 'outer; }
                for j in (i + 1)..ps.len() {
                    if let Err((out, code)) = git_merge_tree(&ps[i].worktree, ps[i].branch_str(), ps[j].branch_str()) {
                        if out.contains(file.as_str()) || code == Some(1) {
                            status = "conflict".to_string();
                            let key = (ps[i].pane_id, ps[j].pane_id);
                            let idx = *conflict_map.entry(key).or_insert_with(|| {
                                conflict_list.push(ConflictPair { a: key.0, b: key.1, files: Vec::new() });
                                conflict_list.len() - 1
                            });
                            if !conflict_list[idx].files.contains(file) {
                                conflict_list[idx].files.push(file.clone());
                            }
                        }
                    }
                }
            }
        }

        overlaps.push(json!({
            "path": file,
            "pane_ids": ps.iter().map(|p| json!(p.pane_id)).collect::<Vec<_>>(),
            "status": status,
        }));
    }

    let conflicts: Vec<Value> = conflict_list.iter().map(|c| json!({ "a": c.a, "b": c.b, "files": c.files.clone() })).collect();

    // ── DETERMINISTIC proposal: fewest conflict-files first, then fewest total changes ──
    let conf_count = |pane_id: i64| -> i64 {
        conflict_list.iter().filter(|c| c.a == pane_id || c.b == pane_id).map(|c| c.files.len() as i64).sum()
    };
    let mut order_panes: Vec<&PaneResult> = panes.iter().collect();
    order_panes.sort_by(|x, y| conf_count(x.pane_id).cmp(&conf_count(y.pane_id)).then_with(|| x.files.len().cmp(&y.files.len())));
    let order: Vec<Value> = order_panes.iter().map(|p| json!(p.pane_id)).collect();

    let note = if panes.is_empty() {
        "no agent worktrees yet".to_string()
    } else if overlaps.is_empty() {
        "no overlapping files — integrate in any order".to_string()
    } else if !conflicts.is_empty() {
        format!(
            "integrate in the listed order; {} pair{} REALLY conflict — spawn a merge agent for those",
            conflicts.len(),
            if conflicts.len() == 1 { "" } else { "s" }
        )
    } else {
        "overlaps exist but hunks merge clean (or need commits to analyze) — integrate in the listed order".to_string()
    };

    let panes_out: Vec<Value> = panes
        .iter()
        .map(|p| json!({ "pane_id": p.pane_id, "name": p.name.clone(), "kind": p.kind.clone(), "branch": p.branch.clone(), "files": p.files.clone() }))
        .collect();

    let mut out = Map::new();
    out.insert("ok".into(), json!(true));
    out.insert("session_id".into(), json_num_like_js(session_id_val));
    out.insert("panes".into(), json!(panes_out));
    out.insert("overlaps".into(), json!(overlaps));
    out.insert("conflicts".into(), json!(conflicts));
    out.insert("proposal".into(), json!({ "order": order, "note": note }));
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE cockpit_sessions (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL, status TEXT NOT NULL DEFAULT 'active'); \
             CREATE TABLE cockpit_panes (id INTEGER PRIMARY KEY AUTOINCREMENT, session_id INTEGER NOT NULL, subagent_name TEXT, pane_kind TEXT, branch TEXT, worktree_path TEXT);",
        )
        .unwrap();
        c
    }

    #[test]
    fn no_session_id_and_no_chat_session_id_returns_the_empty_shape() {
        let c = fresh();
        let r = sharedDiff(&json!({}), &c).unwrap();
        assert_eq!(
            r,
            json!({ "ok": true, "session_id": null, "panes": [], "overlaps": [], "conflicts": [],
                "proposal": { "order": [], "note": "no cockpit session yet — open an agent pane first" } })
        );
    }

    #[test]
    fn direct_session_id_with_no_panes_yields_empty_panes_and_no_worktrees_note() {
        let c = fresh();
        c.execute("INSERT INTO cockpit_sessions (id, name, status) VALUES (7, 'x', 'active')", []).unwrap();
        let r = sharedDiff(&json!({ "session_id": 7 }), &c).unwrap();
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["session_id"], json!(7));
        assert_eq!(r["panes"], json!([]));
        assert_eq!(r["proposal"]["note"], json!("no agent worktrees yet"));
    }

    #[test]
    fn chat_session_id_resolves_active_session_and_skips_closed_ones() {
        let c = fresh();
        c.execute("INSERT INTO cockpit_sessions (id, name, status) VALUES (1, 'antichat#42', 'closed')", []).unwrap();
        c.execute("INSERT INTO cockpit_sessions (id, name, status) VALUES (2, 'antichat#42', 'active')", []).unwrap();
        let r = sharedDiff(&json!({ "chat_session_id": 42 }), &c).unwrap();
        assert_eq!(r["session_id"], json!(2));
    }

    #[test]
    fn falsy_session_id_zero_falls_through_to_chat_session_id() {
        let c = fresh();
        c.execute("INSERT INTO cockpit_sessions (id, name, status) VALUES (5, 'antichat#9', 'active')", []).unwrap();
        let r = sharedDiff(&json!({ "session_id": 0, "chat_session_id": 9 }), &c).unwrap();
        assert_eq!(r["session_id"], json!(5));
    }

    #[test]
    fn pane_rows_with_no_worktree_path_are_filtered_before_any_git_call() {
        let c = fresh();
        c.execute("INSERT INTO cockpit_sessions (id, name) VALUES (1, 'x')", []).unwrap();
        c.execute("INSERT INTO cockpit_panes (session_id, subagent_name, pane_kind, branch, worktree_path) VALUES (1, 'dev', 'agent', 'b1', NULL)", []).unwrap();
        c.execute("INSERT INTO cockpit_panes (session_id, subagent_name, pane_kind, branch, worktree_path) VALUES (1, 'qa', 'agent', 'b2', '')", []).unwrap();
        let r = sharedDiff(&json!({ "session_id": 1 }), &c).unwrap();
        assert_eq!(r["panes"], json!([]));
        assert_eq!(r["proposal"]["note"], json!("no agent worktrees yet"));
    }

    #[test]
    fn pane_worktree_path_absent_on_disk_is_also_filtered() {
        let c = fresh();
        c.execute("INSERT INTO cockpit_sessions (id, name) VALUES (1, 'x')", []).unwrap();
        c.execute(
            "INSERT INTO cockpit_panes (session_id, subagent_name, pane_kind, branch, worktree_path) VALUES (1, 'dev', 'agent', 'b1', 'C:/does/not/exist/xyz-shared-diff')",
            [],
        )
        .unwrap();
        let r = sharedDiff(&json!({ "session_id": 1 }), &c).unwrap();
        assert_eq!(r["panes"], json!([]));
    }

    #[test]
    fn parse_status_path_handles_rename_and_quoting() {
        assert_eq!(parse_status_path(" M src/keep.js"), "src/keep.js");
        assert_eq!(parse_status_path("R  old.txt -> new.txt"), "new.txt");
        assert_eq!(parse_status_path("?? \"quoted.txt\""), "quoted.txt");
        assert_eq!(parse_status_path(""), "");
    }

    #[test]
    fn ours_regex_matches_our_own_artifacts() {
        assert!(ours_re().is_match("AGENTS.md"));
        assert!(ours_re().is_match(".claude/settings.json"));
        assert!(!ours_re().is_match("src/index.js"));
    }
}
