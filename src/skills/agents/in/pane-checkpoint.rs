//! pane-checkpoint.rs — twin of skills/agents/in/pane-checkpoint.mjs. NAMES ARE CONTRACT:
//! `paneCheckpoint` (default).
//!
//! Named git checkpoints inside a pane's worktree: save (add -A + commit --no-verify), list
//! (last 20 log entries), rewind (branch-local `git reset --hard` to a given sha — the pane's
//! own branch only, never the main branch).
//!
//! BESPOKE-NATIVE (playbook's "not conditionally avoidable" rule): unlike
//! constitution-estimate.rs's git-diff branch (which fires only when `input.base` is truthy,
//! with a git-free default path carrying the fixture), EVERY success path of every op here
//! (`save`/`list`/`rewind`) unconditionally shells out to `git` — there is no git-free branch
//! to fixture. Per the playbook: real correct code for the whole function, proven by
//! `#[cfg(test)]` native tests against a real temp-dir git repo (code-git-init.rs /
//! orphan-worktree-dirs.rs precedent), NO conform-db fixture and NO frontier entry.
//!
//! `context.db` missing → JS throws `err.contextMissing('pane-checkpoint', 'db')`. Not modeled:
//! `db: &Connection` is a required, non-nullable reference here — same "guard-throws are not
//! exercised" convention used by every sibling twin.
//!
//! `git = (cwd, args) => execFileSync('git', args, { cwd, encoding:'utf8', ... }).trim()`:
//! execFileSync THROWS on a non-zero exit (unlike spawnSync). Modeled as `git()` returning
//! `Result<String, GitOutput>` so the `save` op's specific `catch { if !stdout.includes('nothing
//! to commit') throw }` swallow can inspect the failed output; every other call site propagates
//! the failure with `?`/`map_err`.
//!
//! `op = String(input?.op ?? 'list')` and `label = String(input?.label ?? 'checkpoint').slice(0,
//! 80)`: `nz` (JS `??`, null/undefined only) then `js_string` (JS `String(x)` on the resolved
//! value), label additionally `.chars().take(80)` (JS UTF-16 slice — ASCII/BMP approximation per
//! playbook gotcha 5).
//!
//! `sha = String(input?.sha ?? '').replace(/[^0-9a-f]/gi, '').slice(0, 40)`: `str_qq` (`??''`)
//! then `/[^0-9a-f]/gi` strips every character that is NOT a case-insensitive hex digit —
//! equivalent to keeping only `is_ascii_hexdigit()` chars — then truncated to 40.
//!
//! `list`'s `out.split('\n').map(l => { const [sha,subject,when] = l.split('|'); ... })`: JS
//! `split('|')` splits on EVERY `|` (unlike a `splitn`); the 3-element destructure only keeps
//! the first three parts (a `|` inside a commit subject would shift/lose `when` — a real JS
//! quirk, reproduced verbatim below via `parts.first()/.get(1)/.get(2)`, never "fixed").

use rusqlite::{Connection, OptionalExtension};
use serde_json::{json, Map, Value};

fn str_qq(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(),
        Some(o) => o.to_string(),
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
} // = JS String(v)
fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) } // JS `??` right operand
fn js_number(v: Option<&Value>) -> f64 {
    match v {
        None => f64::NAN,
        Some(Value::Null) => 0.0,
        Some(Value::Bool(b)) => if *b { 1.0 } else { 0.0 },
        Some(Value::Number(n)) => n.as_f64().unwrap_or(f64::NAN),
        Some(Value::String(s)) => { let t = s.trim(); if t.is_empty() { 0.0 } else { t.parse().unwrap_or(f64::NAN) } }
        Some(_) => f64::NAN,
    }
}

struct GitOutput { stdout: String, stderr: String }

// `execFileSync('git', args, { cwd, encoding:'utf8', stdio:['pipe','pipe','pipe'] }).trim()`.
// Ok(trimmed stdout) on exit 0; Err(GitOutput) on non-zero exit or an unspawnable binary.
fn git(cwd: &str, args: &[&str]) -> Result<String, GitOutput> {
    match crate::proc::cmd("git").args(args).current_dir(cwd).output() {
        Ok(o) if o.status.success() => Ok(String::from_utf8_lossy(&o.stdout).trim().to_string()),
        Ok(o) => Err(GitOutput {
            stdout: String::from_utf8_lossy(&o.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&o.stderr).into_owned(),
        }),
        Err(e) => Err(GitOutput { stdout: String::new(), stderr: e.to_string() }),
    }
}

fn git_err(args: &[&str], o: &GitOutput) -> String {
    let src = if !o.stderr.is_empty() { &o.stderr } else { &o.stdout };
    format!("pane-checkpoint: git {} failed: {}", args.join(" "), src.trim())
}

pub fn paneCheckpoint(input: &Value, db: &Connection) -> Result<Value, String> {
    let pane_id_f = js_number(input.get("pane_id"));
    if !pane_id_f.is_finite() || pane_id_f.fract() != 0.0 || pane_id_f <= 0.0 {
        return Err("pane-checkpoint: pane_id is required".to_string());
    }
    let pane_id = pane_id_f as i64;

    let worktree_path: Option<Option<String>> = db
        .query_row(
            "SELECT worktree_path FROM cockpit_panes WHERE id = ?1",
            rusqlite::params![pane_id],
            |r| r.get::<usize, Option<String>>(0),
        )
        .optional()
        .map_err(|e| e.to_string())?;

    let wt = match worktree_path.flatten() {
        Some(p) if !p.is_empty() && std::path::Path::new(&p).exists() => p,
        _ => return Err(format!("pane-checkpoint: worktree for pane '{pane_id}' not found")),
    };

    let op = js_string(nz(input.get("op")).unwrap_or(&json!("list")));

    if op == "save" {
        let label_full = js_string(nz(input.get("label")).unwrap_or(&json!("checkpoint")));
        let label: String = label_full.chars().take(80).collect();

        git(&wt, &["add", "-A"]).map_err(|o| git_err(&["add", "-A"], &o))?;

        let commit_msg = format!("\u{2691} checkpoint: {label}");
        match git(&wt, &["commit", "-m", &commit_msg, "--no-verify"]) {
            Ok(_) => {}
            Err(o) => {
                let check = if !o.stdout.is_empty() { &o.stdout } else { &o.stderr };
                if !check.contains("nothing to commit") {
                    return Err(git_err(&["commit", "-m", &commit_msg, "--no-verify"], &o));
                }
            }
        }
        let sha = git(&wt, &["rev-parse", "--short", "HEAD"]).map_err(|o| git_err(&["rev-parse", "--short", "HEAD"], &o))?;
        return Ok(json!({ "ok": true, "sha": sha }));
    }

    if op == "list" {
        let out = git(&wt, &["log", "--format=%h|%s|%cr", "-n", "20"]).map_err(|o| git_err(&["log"], &o))?;
        let checkpoints: Vec<Value> = if out.is_empty() {
            Vec::new()
        } else {
            out.split('\n')
                .map(|l| {
                    // JS `l.split('|')` splits on EVERY '|'; the 3-way destructure keeps only
                    // the first three parts (see module doc).
                    let parts: Vec<&str> = l.split('|').collect();
                    let mut m = Map::new();
                    if let Some(s) = parts.first().copied() { m.insert("sha".into(), json!(s)); }
                    if let Some(s) = parts.get(1).copied() { m.insert("subject".into(), json!(s)); }
                    if let Some(s) = parts.get(2).copied() { m.insert("when".into(), json!(s)); }
                    Value::Object(m)
                })
                .collect()
        };
        return Ok(json!({ "ok": true, "checkpoints": checkpoints }));
    }

    if op == "rewind" {
        let raw = str_qq(input.get("sha"));
        let cleaned: String = raw.chars().filter(|c| c.is_ascii_hexdigit()).collect();
        let sha: String = cleaned.chars().take(40).collect();
        if sha.is_empty() {
            return Err("pane-checkpoint: sha is required".to_string());
        }
        git(&wt, &["reset", "--hard", &sha]).map_err(|o| git_err(&["reset", "--hard", &sha], &o))?;
        return Ok(json!({ "ok": true, "sha": sha }));
    }

    Err(format!("pane-checkpoint: op must be one of: save, list, rewind (got '{op}')"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

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
            "INSERT INTO cockpit_panes (session_id, idx, worktree_path) VALUES (1, 0, ?1)",
            rusqlite::params![worktree_path],
        ).unwrap();
        c.last_insert_rowid()
    }
    fn temp_dir(tag: &str) -> PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!("antiloki-pane-checkpoint-rs-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }
    fn fresh_repo(tag: &str) -> PathBuf {
        let dir = temp_dir(tag);
        let d = dir.to_str().unwrap();
        crate::proc::cmd("git").args(["init"]).current_dir(d).output().unwrap();
        crate::proc::cmd("git").args(["config", "user.email", "test@antiloki.local"]).current_dir(d).output().unwrap();
        crate::proc::cmd("git").args(["config", "user.name", "antiloki-test"]).current_dir(d).output().unwrap();
        fs::write(dir.join("a.txt"), "a").unwrap();
        crate::proc::cmd("git").args(["add", "-A"]).current_dir(d).output().unwrap();
        crate::proc::cmd("git").args(["commit", "-m", "initial"]).current_dir(d).output().unwrap();
        dir
    }
    fn head_sha(dir: &str) -> String {
        let o = crate::proc::cmd("git").args(["rev-parse", "--short", "HEAD"]).current_dir(dir).output().unwrap();
        String::from_utf8_lossy(&o.stdout).trim().to_string()
    }
    fn head_sha_full(dir: &str) -> String {
        let o = crate::proc::cmd("git").args(["rev-parse", "HEAD"]).current_dir(dir).output().unwrap();
        String::from_utf8_lossy(&o.stdout).trim().to_string()
    }

    #[test]
    fn rejects_non_integer_or_non_positive_pane_id() {
        let c = db();
        assert!(paneCheckpoint(&json!({}), &c).unwrap_err().contains("pane_id"));
        assert!(paneCheckpoint(&json!({ "pane_id": 0 }), &c).unwrap_err().contains("pane_id"));
        assert!(paneCheckpoint(&json!({ "pane_id": 1.5 }), &c).unwrap_err().contains("pane_id"));
    }

    #[test]
    fn rejects_pane_with_no_provisioned_worktree() {
        let c = db();
        let no_wt = make_pane(&c, None);
        assert!(paneCheckpoint(&json!({ "pane_id": no_wt, "op": "list" }), &c).unwrap_err().contains("not found"));

        let missing = std::env::temp_dir().join(format!("antiloki-pane-checkpoint-absent-{}", std::process::id()));
        let missing_pane = make_pane(&c, Some(missing.to_str().unwrap()));
        assert!(paneCheckpoint(&json!({ "pane_id": missing_pane, "op": "list" }), &c).unwrap_err().contains("not found"));
    }

    #[test]
    fn rejects_unknown_op() {
        let c = db();
        let dir = fresh_repo("bad-op");
        let pane_id = make_pane(&c, Some(dir.to_str().unwrap()));
        let e = paneCheckpoint(&json!({ "pane_id": pane_id, "op": "nope" }), &c).unwrap_err();
        assert!(e.contains("op must be one of: save, list, rewind (got 'nope')"), "{e}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn save_commits_pending_changes_and_returns_new_short_sha() {
        let c = db();
        let dir = fresh_repo("save");
        let d = dir.to_str().unwrap();
        let pane_id = make_pane(&c, Some(d));
        fs::write(dir.join("b.txt"), "b").unwrap();

        let r = paneCheckpoint(&json!({ "pane_id": pane_id, "op": "save", "label": "my checkpoint" }), &c).unwrap();
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["sha"], json!(head_sha(d)));

        let o = crate::proc::cmd("git").args(["log", "-1", "--format=%s"]).current_dir(d).output().unwrap();
        let subject = String::from_utf8_lossy(&o.stdout).trim().to_string();
        assert!(subject.ends_with("checkpoint: my checkpoint"), "{subject}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn save_default_label_is_checkpoint() {
        let c = db();
        let dir = fresh_repo("save-default-label");
        let d = dir.to_str().unwrap();
        let pane_id = make_pane(&c, Some(d));
        fs::write(dir.join("c.txt"), "c").unwrap();

        paneCheckpoint(&json!({ "pane_id": pane_id, "op": "save" }), &c).unwrap();
        let o = crate::proc::cmd("git").args(["log", "-1", "--format=%s"]).current_dir(d).output().unwrap();
        let subject = String::from_utf8_lossy(&o.stdout).trim().to_string();
        assert!(subject.ends_with("checkpoint: checkpoint"), "{subject}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn save_nothing_to_commit_is_swallowed_returns_unchanged_head_sha() {
        let c = db();
        let dir = fresh_repo("save-noop");
        let d = dir.to_str().unwrap();
        let pane_id = make_pane(&c, Some(d));
        let before = head_sha(d);

        let r = paneCheckpoint(&json!({ "pane_id": pane_id, "op": "save" }), &c).unwrap();
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["sha"], json!(before));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn list_returns_checkpoints_newest_first_with_sha_subject_when() {
        let c = db();
        let dir = fresh_repo("list");
        let d = dir.to_str().unwrap();
        let pane_id = make_pane(&c, Some(d));
        fs::write(dir.join("b.txt"), "b").unwrap();
        paneCheckpoint(&json!({ "pane_id": pane_id, "op": "save", "label": "second" }), &c).unwrap();

        let r = paneCheckpoint(&json!({ "pane_id": pane_id, "op": "list" }), &c).unwrap();
        assert_eq!(r["ok"], json!(true));
        let cps = r["checkpoints"].as_array().unwrap();
        assert_eq!(cps.len(), 2);
        assert!(cps[0]["subject"].as_str().unwrap().ends_with("checkpoint: second"));
        assert_eq!(cps[1]["subject"], json!("initial"));
        for cp in cps {
            assert!(cp["sha"].is_string());
            assert!(cp["subject"].is_string());
            assert!(cp["when"].is_string());
        }
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn list_is_the_default_op_when_omitted() {
        let c = db();
        let dir = fresh_repo("list-default");
        let d = dir.to_str().unwrap();
        let pane_id = make_pane(&c, Some(d));
        let r = paneCheckpoint(&json!({ "pane_id": pane_id }), &c).unwrap();
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["checkpoints"].as_array().unwrap().len(), 1);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn rewind_hard_resets_worktree_to_given_sha() {
        let c = db();
        let dir = fresh_repo("rewind");
        let d = dir.to_str().unwrap();
        let pane_id = make_pane(&c, Some(d));
        let first_sha = head_sha(d);

        fs::write(dir.join("b.txt"), "b").unwrap();
        paneCheckpoint(&json!({ "pane_id": pane_id, "op": "save", "label": "second" }), &c).unwrap();
        assert!(dir.join("b.txt").exists());

        let r = paneCheckpoint(&json!({ "pane_id": pane_id, "op": "rewind", "sha": first_sha }), &c).unwrap();
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["sha"], json!(first_sha));
        assert_eq!(head_sha(d), first_sha);
        assert!(!dir.join("b.txt").exists(), "rewind removes files added after the checkpoint");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn rewind_strips_non_hex_chars_and_truncates_to_40() {
        let c = db();
        let dir = fresh_repo("rewind-sanitize");
        let d = dir.to_str().unwrap();
        let pane_id = make_pane(&c, Some(d));
        let full = head_sha_full(d);

        let r = paneCheckpoint(&json!({ "pane_id": pane_id, "op": "rewind", "sha": format!("  {full}!! ") }), &c).unwrap();
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["sha"], json!(full.to_lowercase()));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn rewind_throws_when_sha_sanitizes_to_empty() {
        let c = db();
        let dir = fresh_repo("rewind-empty-sha");
        let pane_id = make_pane(&c, Some(dir.to_str().unwrap()));
        assert!(paneCheckpoint(&json!({ "pane_id": pane_id, "op": "rewind", "sha": "!!!" }), &c).unwrap_err().contains("sha"));
        assert!(paneCheckpoint(&json!({ "pane_id": pane_id, "op": "rewind" }), &c).unwrap_err().contains("sha"));
        let _ = fs::remove_dir_all(&dir);
    }
}
