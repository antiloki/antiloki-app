//! pane-feed.rs — twin of skills/agents/in/pane-feed.mjs (db lane). NAMES ARE CONTRACT:
//! `paneFeed` (default).
//!
//! Hand-delivers a widget's intel to an agent's worktree: writes
//! `<worktree>/.agents/context/<name>.md`, then best-effort rebuilds a marker-delimited
//! `<!-- ANTILOKI-OPERATOR-CONTEXT -->...<!-- /ANTILOKI-OPERATOR-CONTEXT -->` index block inside
//! `<worktree>/AGENTS.md` listing every standing context file.
//!
//! BESPOKE-NATIVE, not conform-db fixture-tested, despite `worktree_path` being DB-derived
//! (`cockpit_panes.worktree_path`, read straight off the seeded row — no override param needed,
//! unlike orphan-worktree-dirs.rs's `detect(db, root)` seam). The reason is NOT the root
//! resolution (that part alone would fixture fine, same technique as stack-facts.rs's
//! `workspaces.root_path` testdata dirs) — it's that this skill, unlike stack-facts, WRITES:
//! - The return value `{ ok, path, removed? }` carries no content signal at all — a fixture's
//!   value+db_after diff would stay green even if BOTH lanes wrote the wrong bytes, or spliced
//!   the AGENTS.md markers incorrectly, since nothing about the write is reflected in `value` or
//!   in `db_after` (the skill never touches the db). A real proof requires reading the written
//!   file back, which only a native `#[cfg(test)]` (`fs::read_to_string` assertions) can do.
//! - A committed `fixtures/testdata/` dir would get MUTATED by every conform run (new
//!   `.agents/context/*.md` files, a rewritten `AGENTS.md`) — unlike every existing DB-derived-root
//!   fixture in this codebase (stack-facts, constitution-estimate), which are read-only and so
//!   never dirty the working tree. A `remove`-op case's precondition (a pre-existing content file)
//!   would also be CONSUMED by the first real run, degrading to a no-op on every rerun after.
//! This mirrors the reasoning already established by the closest siblings in this codebase —
//! orphan-worktree-dirs.rs, orphan-sandbox-dirs.rs, god-driver-halt.rs, god-driver-resume.rs — all
//! real fs-mutating skills, all proven via native tests against a real tempdir + real db, never via
//! conform-db fixtures.
//!
//! `context.db` missing → JS throws `err.contextMissing('pane-feed', 'db')`
//! (`"pane-feed: context.db is required"`). Not modeled: Rust's `db: &Connection` is a required,
//! non-nullable reference, so this guard is unreachable here — same convention as every sibling
//! twin (stack-facts.rs, db-reset.rs, naming-derive.rs, ...).
//!
//! Sanitization mirrors JS exactly: `String(input?.name ?? 'context').toLowerCase()`, then every
//! MAXIMAL run of a non `[A-Za-z0-9_.-]` character collapses to a single `-` (JS `\w` has no `u`
//! flag here, so it's ASCII-only — matched below by hand, not the `regex` crate, since it's a
//! simple char-class run-collapse with no lookaround), then leading/trailing `-` trimmed, then
//! truncated to 80 chars (JS `.slice` is UTF-16-code-unit; `.chars().take(80)` is the ASCII/BMP
//! approximation per playbook gotcha 5), falling back to `"context"` if that leaves nothing.
//!
//! `op === 'remove' || undefined`: JS drops `removed` from the JSON entirely when `op !==
//! 'remove'` (`JSON.stringify` omits `undefined`-valued keys) — modeled as conditional `Map`
//! insertion, only when `op == "remove"`.
//!
//! SUBTLE QUIRK (verified against the real JS via a native behavior test —
//! `skills/agents/in/pane-feed.test.mjs`, replacing its `// SCAFFOLD TEST`): when the LAST
//! context file is removed, the recomputed `index` is `''`, so the marker-splice produces
//! `agents = ''` — and `if (agents) fs.writeFileSync(...)` in JS is an `if (truthy)` guard, so an
//! EMPTY resulting string skips the write entirely. `AGENTS.md` is left holding a now-stale
//! index (still naming the just-removed file) rather than being cleared. This looks like a minor
//! JS bug, but per NAMES ARE CONTRACT / "never fix the JS in the twin", it is reproduced verbatim
//! below (`if !agents.is_empty() { write }`).
//!
//! The whole "IMPLICIT DELIVERY" index-rebuild step is JS `try { ... } catch { /* best-effort */
//! }` — ANY failure (dir listing, AGENTS.md read/write) is swallowed and `paneFeed` still returns
//! its normal `{ ok: true, ... }`. Modeled as a closure returning `Option<()>` whose `Err`/`None`
//! outcomes are discarded with `let _ =`.

use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};

fn str_qq(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(),
        Some(o) => o.to_string(),
    }
} // = JS String(v ?? '')

fn js_string(v: &Value) -> String {
    match v {
        Value::Null => "null".into(),
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Array(a) => a.iter().map(js_string).collect::<Vec<_>>().join(","),
        Value::Object(_) => "[object Object]".into(),
    }
} // = JS String(v)

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

// `String(input?.name ?? 'context').toLowerCase().replace(/[^\w.-]+/g, '-').replace(/^-+|-+$/g, '').slice(0, 80) || 'context'`
fn sanitize_name(input: &Value) -> String {
    let raw = match input.get("name").filter(|v| !v.is_null()) {
        Some(v) => js_string(v),
        None => "context".to_string(),
    };
    let lower = raw.to_lowercase();
    let mut collapsed = String::new();
    let mut in_run = false;
    for ch in lower.chars() {
        let keep = ch.is_ascii_alphanumeric() || ch == '_' || ch == '.' || ch == '-';
        if keep {
            collapsed.push(ch);
            in_run = false;
        } else if !in_run {
            collapsed.push('-');
            in_run = true;
        }
    }
    let trimmed = collapsed.trim_matches('-');
    let truncated: String = trimmed.chars().take(80).collect();
    if truncated.is_empty() { "context".to_string() } else { truncated }
}

fn implicit_delivery(worktree_path: &str, dir: &Path) {
    let _: Option<()> = (|| {
        const MARK_A: &str = "<!-- ANTILOKI-OPERATOR-CONTEXT -->";
        const MARK_B: &str = "<!-- /ANTILOKI-OPERATOR-CONTEXT -->";
        let mut files: Vec<String> = if dir.exists() {
            std::fs::read_dir(dir).ok()?
                .filter_map(|e| e.ok())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| n.ends_with(".md"))
                .collect()
        } else {
            Vec::new()
        };
        files.sort();

        let index = if files.is_empty() {
            String::new()
        } else {
            let lines: Vec<String> = files.iter().map(|f| format!("- `.agents/context/{f}`")).collect();
            format!(
                "{MARK_A}\n## Operator context (standing — read what you have not read yet; applies until removed)\n{}\n{MARK_B}\n",
                lines.join("\n")
            )
        };

        let agents_path = PathBuf::from(worktree_path).join("AGENTS.md");
        let mut agents = if agents_path.exists() { std::fs::read_to_string(&agents_path).ok()? } else { String::new() };

        let a = agents.find(MARK_A);
        let b = agents.find(MARK_B);
        match (a, b) {
            (Some(ai), Some(bi)) if bi > ai => {
                let tail_start = bi + MARK_B.len();
                let mut tail = agents[tail_start..].to_string();
                if let Some(stripped) = tail.strip_prefix('\n') { tail = stripped.to_string(); }
                agents = format!("{}{}{}", &agents[..ai], index, tail);
            }
            _ => {
                if !index.is_empty() {
                    agents = if agents.is_empty() { index } else { format!("{}\n\n{}", agents.trim_end(), index) };
                }
            }
        }

        // JS `if (agents) fs.writeFileSync(...)`: an empty string is falsy, so the write is
        // skipped — see module doc "SUBTLE QUIRK".
        if !agents.is_empty() {
            std::fs::write(&agents_path, &agents).ok()?;
        }
        Some(())
    })();
}

pub fn paneFeed(input: &Value, db: &Connection) -> Result<Value, String> {
    let pane_id_f = js_number(input.get("pane_id"));
    if !pane_id_f.is_finite() || pane_id_f.fract() != 0.0 || pane_id_f <= 0.0 {
        return Err("pane-feed: pane_id is required".to_string());
    }
    let pane_id = pane_id_f as i64;

    let rows = crate::db_json::query_rows(db, "SELECT id, worktree_path FROM cockpit_panes WHERE id = ?", &[json!(pane_id)])?;
    let pane = rows.as_array().and_then(|a| a.first());
    let pane = match pane {
        Some(p) => p,
        None => return Err(format!("pane-feed: pane '{pane_id}' not found")),
    };

    let worktree_path = pane.get("worktree_path").and_then(|v| v.as_str()).unwrap_or("");
    if worktree_path.is_empty() || !Path::new(worktree_path).exists() {
        return Err("pane-feed: a provisioned worktree is required".to_string());
    }

    let name = sanitize_name(input);
    let dir = PathBuf::from(worktree_path).join(".agents").join("context");
    let op = match input.get("op").filter(|v| !v.is_null()) {
        Some(v) => js_string(v),
        None => "write".to_string(),
    };
    let rel = format!(".agents/context/{name}.md");

    if op == "remove" {
        let _ = std::fs::remove_file(dir.join(format!("{name}.md")));
    } else {
        let content = str_qq(input.get("content"));
        if content.trim().is_empty() {
            return Err("pane-feed: content is required".to_string());
        }
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let truncated: String = content.chars().take(400_000).collect();
        std::fs::write(dir.join(format!("{name}.md")), truncated).map_err(|e| e.to_string())?;
    }

    implicit_delivery(worktree_path, &dir);

    let mut out = Map::new();
    out.insert("ok".into(), json!(true));
    out.insert("path".into(), json!(rel));
    if op == "remove" {
        out.insert("removed".into(), json!(true));
    }
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE cockpit_panes (id INTEGER PRIMARY KEY AUTOINCREMENT, session_id INTEGER NOT NULL, \
             idx INTEGER NOT NULL, worktree_path TEXT)",
        ).unwrap();
        c
    }
    fn make_pane(c: &Connection, worktree_path: Option<&str>) -> i64 {
        c.execute(
            "INSERT INTO cockpit_panes (session_id, idx, worktree_path) VALUES (1, 1, ?1)",
            rusqlite::params![worktree_path],
        ).unwrap();
        c.last_insert_rowid()
    }
    fn temp_dir(tag: &str) -> PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!("antiloki-pane-feed-rs-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn write_creates_context_file_and_agents_md_index() {
        let c = db();
        let wt = temp_dir("write");
        let pane_id = make_pane(&c, Some(wt.to_str().unwrap()));
        let r = paneFeed(&json!({ "pane_id": pane_id, "name": "my-notes", "content": "hello world" }), &c).unwrap();
        assert_eq!(r, json!({ "ok": true, "path": ".agents/context/my-notes.md" }));
        assert_eq!(r.as_object().unwrap().keys().collect::<Vec<_>>(), vec!["ok", "path"]); // removed omitted

        let file_path = wt.join(".agents").join("context").join("my-notes.md");
        assert_eq!(fs::read_to_string(&file_path).unwrap(), "hello world");

        let agents = fs::read_to_string(wt.join("AGENTS.md")).unwrap();
        assert!(agents.contains("<!-- ANTILOKI-OPERATOR-CONTEXT -->"));
        assert!(agents.contains("<!-- /ANTILOKI-OPERATOR-CONTEXT -->"));
        assert!(agents.contains("- `.agents/context/my-notes.md`"));
        let _ = fs::remove_dir_all(&wt);
    }

    #[test]
    fn default_name_is_context() {
        let c = db();
        let wt = temp_dir("default-name");
        let pane_id = make_pane(&c, Some(wt.to_str().unwrap()));
        let r = paneFeed(&json!({ "pane_id": pane_id, "content": "x" }), &c).unwrap();
        assert_eq!(r["path"], json!(".agents/context/context.md"));
        let _ = fs::remove_dir_all(&wt);
    }

    #[test]
    fn name_sanitization_collapses_runs_and_trims_edges() {
        let c = db();
        let wt = temp_dir("sanitize");
        let pane_id = make_pane(&c, Some(wt.to_str().unwrap()));
        let r = paneFeed(&json!({ "pane_id": pane_id, "name": "Foo Bar/Baz!!", "content": "x" }), &c).unwrap();
        assert_eq!(r["path"], json!(".agents/context/foo-bar-baz.md"));
        let _ = fs::remove_dir_all(&wt);
    }

    #[test]
    fn all_symbol_name_falls_back_to_context() {
        let c = db();
        let wt = temp_dir("all-symbol");
        let pane_id = make_pane(&c, Some(wt.to_str().unwrap()));
        let r = paneFeed(&json!({ "pane_id": pane_id, "name": "!!!", "content": "x" }), &c).unwrap();
        assert_eq!(r["path"], json!(".agents/context/context.md"));
        let _ = fs::remove_dir_all(&wt);
    }

    #[test]
    fn name_truncated_to_80_chars() {
        let c = db();
        let wt = temp_dir("truncate-name");
        let pane_id = make_pane(&c, Some(wt.to_str().unwrap()));
        let long_name = "a".repeat(100);
        let r = paneFeed(&json!({ "pane_id": pane_id, "name": long_name, "content": "x" }), &c).unwrap();
        assert_eq!(r["path"], json!(format!(".agents/context/{}.md", "a".repeat(80))));
        let _ = fs::remove_dir_all(&wt);
    }

    #[test]
    fn content_truncated_to_400000_chars() {
        let c = db();
        let wt = temp_dir("truncate-content");
        let pane_id = make_pane(&c, Some(wt.to_str().unwrap()));
        let content = "x".repeat(400_010);
        paneFeed(&json!({ "pane_id": pane_id, "name": "big", "content": content }), &c).unwrap();
        let written = fs::read_to_string(wt.join(".agents").join("context").join("big.md")).unwrap();
        assert_eq!(written.len(), 400_000);
        let _ = fs::remove_dir_all(&wt);
    }

    #[test]
    fn empty_or_whitespace_content_is_rejected() {
        let c = db();
        let wt = temp_dir("empty-content");
        let pane_id = make_pane(&c, Some(wt.to_str().unwrap()));
        assert!(paneFeed(&json!({ "pane_id": pane_id, "content": "   " }), &c).unwrap_err().contains("content is required"));
        assert!(paneFeed(&json!({ "pane_id": pane_id }), &c).unwrap_err().contains("content is required"));
        let _ = fs::remove_dir_all(&wt);
    }

    #[test]
    fn remove_deletes_file_and_updates_index() {
        let c = db();
        let wt = temp_dir("remove");
        let pane_id = make_pane(&c, Some(wt.to_str().unwrap()));
        paneFeed(&json!({ "pane_id": pane_id, "name": "a", "content": "A" }), &c).unwrap();
        paneFeed(&json!({ "pane_id": pane_id, "name": "b", "content": "B" }), &c).unwrap();

        let r = paneFeed(&json!({ "pane_id": pane_id, "name": "a", "op": "remove" }), &c).unwrap();
        assert_eq!(r, json!({ "ok": true, "path": ".agents/context/a.md", "removed": true }));
        assert!(!wt.join(".agents").join("context").join("a.md").exists());

        let agents = fs::read_to_string(wt.join("AGENTS.md")).unwrap();
        assert!(!agents.contains("a.md"));
        assert!(agents.contains("- `.agents/context/b.md`"));
        let _ = fs::remove_dir_all(&wt);
    }

    #[test]
    fn removing_last_file_leaves_agents_md_stale_because_empty_string_is_falsy() {
        let c = db();
        let wt = temp_dir("remove-last");
        let pane_id = make_pane(&c, Some(wt.to_str().unwrap()));
        paneFeed(&json!({ "pane_id": pane_id, "name": "only", "content": "only" }), &c).unwrap();
        let before = fs::read_to_string(wt.join("AGENTS.md")).unwrap();
        let r = paneFeed(&json!({ "pane_id": pane_id, "name": "only", "op": "remove" }), &c).unwrap();
        assert_eq!(r["removed"], json!(true));
        let after = fs::read_to_string(wt.join("AGENTS.md")).unwrap();
        assert_eq!(after, before, "agents=='' is falsy in JS, so the write is skipped — stale index survives");
        assert!(after.contains("- `.agents/context/only.md`"));
        let _ = fs::remove_dir_all(&wt);
    }

    #[test]
    fn remove_of_absent_file_is_a_silent_noop() {
        let c = db();
        let wt = temp_dir("remove-absent");
        let pane_id = make_pane(&c, Some(wt.to_str().unwrap()));
        let r = paneFeed(&json!({ "pane_id": pane_id, "name": "ghost", "op": "remove" }), &c).unwrap();
        assert_eq!(r, json!({ "ok": true, "path": ".agents/context/ghost.md", "removed": true }));
        let _ = fs::remove_dir_all(&wt);
    }

    #[test]
    fn agents_md_content_outside_markers_preserved_markers_replaced_in_place() {
        let c = db();
        let wt = temp_dir("markers");
        let pane_id = make_pane(&c, Some(wt.to_str().unwrap()));
        fs::create_dir_all(&wt).unwrap();
        fs::write(
            wt.join("AGENTS.md"),
            "BEFORE\n<!-- ANTILOKI-OPERATOR-CONTEXT -->\nSTALE\n<!-- /ANTILOKI-OPERATOR-CONTEXT -->\nAFTER\n",
        ).unwrap();
        paneFeed(&json!({ "pane_id": pane_id, "name": "fresh", "content": "fresh content" }), &c).unwrap();
        let agents = fs::read_to_string(wt.join("AGENTS.md")).unwrap();
        assert!(agents.starts_with("BEFORE\n"));
        assert!(agents.ends_with("AFTER\n"));
        assert!(!agents.contains("STALE"));
        assert!(agents.contains("- `.agents/context/fresh.md`"));
        let _ = fs::remove_dir_all(&wt);
    }

    #[test]
    fn agents_md_without_markers_gets_index_appended() {
        let c = db();
        let wt = temp_dir("no-markers");
        let pane_id = make_pane(&c, Some(wt.to_str().unwrap()));
        fs::create_dir_all(&wt).unwrap();
        fs::write(wt.join("AGENTS.md"), "# Project notes\nsome text").unwrap();
        paneFeed(&json!({ "pane_id": pane_id, "name": "n", "content": "c" }), &c).unwrap();
        let agents = fs::read_to_string(wt.join("AGENTS.md")).unwrap();
        assert!(agents.starts_with("# Project notes\nsome text\n\n<!-- ANTILOKI-OPERATOR-CONTEXT -->"));
        let _ = fs::remove_dir_all(&wt);
    }

    #[test]
    fn invalid_pane_id_rejected() {
        let c = db();
        assert!(paneFeed(&json!({}), &c).unwrap_err().contains("pane_id is required"));
        assert!(paneFeed(&json!({ "pane_id": 0 }), &c).unwrap_err().contains("pane_id is required"));
        assert!(paneFeed(&json!({ "pane_id": -3 }), &c).unwrap_err().contains("pane_id is required"));
        assert!(paneFeed(&json!({ "pane_id": 1.5 }), &c).unwrap_err().contains("pane_id is required"));
    }

    #[test]
    fn unknown_pane_not_found() {
        let c = db();
        assert!(paneFeed(&json!({ "pane_id": 999 }), &c).unwrap_err().contains("pane '999' not found"));
    }

    #[test]
    fn pane_without_a_real_worktree_on_disk_is_rejected() {
        let c = db();
        let no_wt = make_pane(&c, None);
        assert!(paneFeed(&json!({ "pane_id": no_wt, "content": "x" }), &c).unwrap_err().contains("a provisioned worktree is required"));

        let missing = std::env::temp_dir().join(format!("antiloki-pane-feed-absent-{}", std::process::id()));
        let missing_pane = make_pane(&c, Some(missing.to_str().unwrap()));
        assert!(paneFeed(&json!({ "pane_id": missing_pane, "content": "x" }), &c).unwrap_err().contains("a provisioned worktree is required"));
    }
}
