//! code-regenerate-stage.rs — twin of skills/codebase/code-regenerate-stage.mjs. NAMES ARE
//! CONTRACT: `codeRegenerateStage` (default export, only export).
//!
//! BESPOKE-NATIVE, not conform-db fixture-tested: this skill drives a REAL sandbox worktree
//! (`crate::agents_in_terminal_sandbox::terminalSandbox`, itself bespoke-native — real `git`
//! subprocesses against a real worktree dir), does a REAL fs read/write of the spliced file
//! inside that sandbox, and shells a REAL `git apply --check` subprocess against the real
//! workspace root to report mergeability. None of that is conform-db fixture-seedable (same
//! family as terminal-sandbox.rs itself). Proven below via `#[cfg(test)]` against a real temp
//! git repo + real `ANTILOKI_HOME` tempdir, reusing terminal-sandbox.rs's `with_home()` /
//! `make_temp_git_repo()` pattern.
//!
//! Cross-module calls (already-converted twins, reused not reimplemented):
//! `crate::agents_in_terminal_sandbox::terminalSandbox` (ensure/diff/discard modes) and
//! `crate::engine_workspace_root::getWorkspaceRoot`.
//!
//! `dirname(path).split(/[\\/]/).join('/')`: Node's `path.dirname` (platform `win32` on
//! Windows, `posix` elsewhere) followed by separator normalization to `/`. This twin implements
//! a single separator-agnostic `dirname_normalized` that treats BOTH `/` and `\` as separators
//! on any host — a superset of either platform's real `dirname`, matching what the JS call site
//! actually observes once its own `.split(/[\\/]/).join('/')` normalization runs afterward.
//!
//! Message-slicing (`.slice(0, 200)` / `.slice(0, 140)`) uses UTF-16 code-unit semantics in JS;
//! this twin approximates with `chars().take(N)` (ASCII/BMP-safe, same disclosed gap as every
//! sibling git-subprocess twin — terminal-sandbox.rs, orphan-worktree-dirs.rs).
// QUALITY: 80/100 — real git-subprocess dry-run gate + real sandbox fs splice, cross-module reuse of terminal-sandbox twin, UTF-16 slice approximation (disclosed, shared convention)

use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn slice_n(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

// `String(x ?? '')`.
fn str_qq(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(),
        Some(o) => o.to_string(),
    }
}

fn js_number(v: Option<&Value>) -> f64 {
    match v {
        None => f64::NAN,
        Some(Value::Null) => 0.0,
        Some(Value::Bool(b)) => {
            if *b {
                1.0
            } else {
                0.0
            }
        }
        Some(Value::Number(n)) => n.as_f64().unwrap_or(f64::NAN),
        Some(Value::String(s)) => {
            let t = s.trim();
            if t.is_empty() {
                0.0
            } else {
                t.parse().unwrap_or(f64::NAN)
            }
        }
        Some(_) => f64::NAN,
    }
}

// `dirname(path)` (win32 or posix, both collapse to the same normalized shape once split/join
// runs) — see module doc.
fn dirname_normalized(path: &str) -> String {
    let trimmed = path.trim_end_matches(['/', '\\']);
    let d = match trimmed.rfind(['/', '\\']) {
        Some(pos) => {
            let head = &trimmed[..pos];
            if head.is_empty() {
                "/"
            } else {
                head
            }
        }
        None => ".",
    };
    d.split(['/', '\\'])
        .collect::<Vec<_>>()
        .join("/")
}

struct GitOut {
    status: i32,
    stderr: String,
}

fn run_git_check(args: &[&str], cwd: &Path, input: Option<&str>) -> GitOut {
    use std::io::Write;
    let mut cmd = crate::proc::cmd("git");
    cmd.args(args)
        .current_dir(cwd)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    cmd.stdin(if input.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    });
    let child = match cmd.spawn() {
        Ok(c) => c,
        Err(_) => {
            return GitOut {
                status: -1,
                stderr: String::new(),
            }
        }
    };
    let mut child = child;
    if let Some(data) = input {
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(data.as_bytes());
        }
    }
    match child.wait_with_output() {
        Ok(out) => GitOut {
            status: out.status.code().unwrap_or(-1),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        },
        Err(_) => GitOut {
            status: -1,
            stderr: String::new(),
        },
    }
}

fn err_obj(msg: String) -> Value {
    json!({ "ok": false, "error": msg })
}

/// `export default function codeRegenerateStage(input, context)`.
pub fn codeRegenerateStage(input: &Value, db: &Connection) -> Value {
    let o = input.as_object();
    let path_v = o.and_then(|m| m.get("path")).cloned().unwrap_or(Value::Null);
    let start_line_v = o.and_then(|m| m.get("start_line")).cloned();
    let end_line_v = o.and_then(|m| m.get("end_line")).cloned();
    let proposed_v = o.and_then(|m| m.get("proposed")).cloned();
    let session_id_v = o.and_then(|m| m.get("session_id")).cloned();

    let path_str = str_qq(Some(&path_v));
    let path_truthy = match &path_v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().map_or(false, |f| f != 0.0),
        Value::String(s) => !s.is_empty(),
        _ => true,
    };
    let session_id_truthy = match session_id_v.as_ref() {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    };
    let start_line_truthy = match start_line_v.as_ref() {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    };
    let proposed_is_null = matches!(proposed_v, None | Some(Value::Null));

    if !path_truthy || !session_id_truthy || proposed_is_null || !start_line_truthy {
        return err_obj("need { path, start_line, end_line, proposed, session_id }".to_string());
    }

    let dir = dirname_normalized(&path_str);
    let allow_folders = if !dir.is_empty() && dir != "." {
        json!([dir])
    } else {
        json!([])
    };

    let ensure_input = json!({
        "mode": "ensure",
        "session_id": session_id_v.clone().unwrap_or(Value::Null),
        "allow_folders": allow_folders,
    });
    let sb = match crate::agents_in_terminal_sandbox::terminalSandbox(&ensure_input, db) {
        Ok(v) => v,
        Err(e) => {
            return err_obj(format!(
                "sandbox ensure failed (is the workspace a git repo? run git-init): {}",
                slice_n(&e, 200)
            ))
        }
    };
    let sb_path = sb.get("path").and_then(Value::as_str).unwrap_or_default().to_string();

    let sandbox_file = PathBuf::from(&sb_path).join(&path_str);
    let content = match std::fs::read_to_string(&sandbox_file) {
        Ok(c) => c,
        Err(e) => {
            let discard_input = json!({ "mode": "discard", "session_id": session_id_v.clone().unwrap_or(Value::Null) });
            let _ = crate::agents_in_terminal_sandbox::terminalSandbox(&discard_input, db);
            return err_obj(format!(
                "file is not in the sandbox — is it committed? ({})",
                slice_n(&e.to_string(), 140)
            ));
        }
    };

    let lines: Vec<&str> = content.split('\n').collect();
    let lo_raw = js_number(start_line_v.as_ref());
    let hi_input = if let Some(e) = end_line_v.as_ref() {
        if matches!(e, Value::Null) {
            js_number(start_line_v.as_ref())
        } else {
            let et = match e {
                Value::Bool(b) => *b,
                Value::Number(n) => n.as_f64().map_or(false, |f| f != 0.0),
                Value::String(s) => !s.is_empty(),
                _ => true,
            };
            if et {
                js_number(Some(e))
            } else {
                js_number(start_line_v.as_ref())
            }
        }
    } else {
        js_number(start_line_v.as_ref())
    };
    let lo = (1.0f64).max(lo_raw) as i64;
    let hi = (lines.len() as i64).min(lo.max(hi_input as i64));
    let lo = lo.max(0) as usize;
    let hi = hi.max(0) as usize;
    let lo_idx = lo.saturating_sub(1).min(lines.len());
    let hi_idx = hi.min(lines.len());

    let proposed_str = str_qq(proposed_v.as_ref());
    let mut next_lines: Vec<&str> = Vec::new();
    next_lines.extend_from_slice(&lines[..lo_idx]);
    let proposed_split: Vec<&str> = proposed_str.split('\n').collect();
    next_lines.extend_from_slice(&proposed_split);
    next_lines.extend_from_slice(&lines[hi_idx..]);
    let next = next_lines.join("\n");

    if let Err(e) = std::fs::write(&sandbox_file, next) {
        return err_obj(format!("sandbox write failed: {}", slice_n(&e.to_string(), 160)));
    }

    let diff_input = json!({ "mode": "diff", "session_id": session_id_v.clone().unwrap_or(Value::Null) });
    let d = match crate::agents_in_terminal_sandbox::terminalSandbox(&diff_input, db) {
        Ok(v) => v,
        Err(e) => return err_obj(e),
    };
    let patch = d.get("patch").and_then(Value::as_str).unwrap_or_default().to_string();

    let mut applies_clean = false;
    let mut reason: Option<String> = None;
    if !patch.trim().is_empty() {
        let root_v = crate::engine_workspace_root::getWorkspaceRoot(db);
        let root = root_v.as_str().unwrap_or_default().to_string();
        let check = run_git_check(&["apply", "--check", "-"], Path::new(&root), Some(&patch));
        applies_clean = check.status == 0;
        if !applies_clean {
            reason = Some(slice_n(&check.stderr, 240));
        }
    } else {
        reason = Some("no changes (the proposal matches the committed source)".to_string());
    }

    let mut out = Map::new();
    out.insert("ok".into(), json!(true));
    out.insert("session_id".into(), session_id_v.unwrap_or(Value::Null));
    out.insert("path".into(), path_v);
    out.insert("sandbox_path".into(), json!(sb_path));
    out.insert("patch".into(), json!(patch));
    out.insert("files".into(), d.get("files").cloned().unwrap_or(json!([])));
    out.insert("changed".into(), d.get("changed").cloned().unwrap_or(json!(0)));
    out.insert("applies_clean".into(), json!(applies_clean));
    match reason {
        Some(r) => {
            out.insert("reason".into(), json!(r));
        }
        None => {
            out.insert("reason".into(), Value::Null);
        }
    }
    Value::Object(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn with_home<T>(antiloki_home: &str, f: impl FnOnce() -> T) -> T {
        let _guard = crate::test_env_lock::ENV_LOCK.lock().unwrap();
        unsafe {
            std::env::set_var("ANTILOKI_HOME", antiloki_home);
        }
        let result = f();
        unsafe {
            std::env::remove_var("ANTILOKI_HOME");
        }
        result
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!(
            "antiloki-code-regen-stage-rs-{tag}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn run_git_ok(args: &[&str], cwd: &Path) {
        let out = crate::proc::cmd("git")
            .args(args)
            .current_dir(cwd)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn make_temp_git_repo(tag: &str) -> PathBuf {
        let dir = temp_dir(tag);
        run_git_ok(&["init", "-q"], &dir);
        run_git_ok(&["config", "user.email", "test@test"], &dir);
        run_git_ok(&["config", "user.name", "test"], &dir);
        fs::write(dir.join("hello.txt"), "line1\nline2\nline3\n").unwrap();
        run_git_ok(&["add", "hello.txt"], &dir);
        run_git_ok(&["commit", "-q", "-m", "init"], &dir);
        dir
    }

    fn db_with_workspace(root: &Path) -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE workspaces (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL UNIQUE, \
             root_path TEXT NOT NULL, vcs TEXT NOT NULL DEFAULT 'git', is_active INTEGER NOT NULL DEFAULT 0, \
             created_at TEXT NOT NULL DEFAULT '2026-01-01 00:00:00')",
        )
        .unwrap();
        c.execute(
            "INSERT INTO workspaces (name, root_path, is_active) VALUES ('x', ?1, 1)",
            rusqlite::params![root.to_string_lossy().into_owned()],
        )
        .unwrap();
        c
    }

    #[test]
    fn missing_required_input_returns_error() {
        let c = Connection::open_in_memory().unwrap();
        let r = codeRegenerateStage(&json!({}), &c);
        assert_eq!(r["ok"], json!(false));
        assert_eq!(
            r["error"],
            json!("need { path, start_line, end_line, proposed, session_id }")
        );
    }

    #[test]
    fn dirname_normalized_handles_both_separators() {
        assert_eq!(dirname_normalized("a/b/c.mjs"), "a/b");
        assert_eq!(dirname_normalized("a\\b\\c.mjs"), "a/b");
        assert_eq!(dirname_normalized("c.mjs"), ".");
    }

    #[test]
    fn stages_a_splice_and_reports_clean_apply() {
        let root = make_temp_git_repo("splice-root");
        let home = temp_dir("splice-home");
        let c = db_with_workspace(&root);

        let r = with_home(home.to_str().unwrap(), || {
            codeRegenerateStage(
                &json!({
                    "path": "hello.txt",
                    "start_line": 2,
                    "end_line": 2,
                    "proposed": "LINE-TWO-REPLACED",
                    "session_id": "stage1",
                }),
                &c,
            )
        });

        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["session_id"], json!("stage1"));
        assert_eq!(r["path"], json!("hello.txt"));
        assert!(r["patch"].as_str().unwrap().contains("LINE-TWO-REPLACED"));
        assert_eq!(r["changed"], json!(1));
        assert_eq!(r["applies_clean"], json!(true));
        assert_eq!(r["reason"], Value::Null);

        // real tree untouched — only the sandbox worktree got the splice.
        let real_content = fs::read_to_string(root.join("hello.txt")).unwrap();
        assert_eq!(real_content, "line1\nline2\nline3\n");

        let _ = fs::remove_dir_all(&root);
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn no_op_proposal_reports_no_changes_reason() {
        let root = make_temp_git_repo("noop-root");
        let home = temp_dir("noop-home");
        let c = db_with_workspace(&root);

        let r = with_home(home.to_str().unwrap(), || {
            codeRegenerateStage(
                &json!({
                    "path": "hello.txt",
                    "start_line": 2,
                    "end_line": 2,
                    "proposed": "line2",
                    "session_id": "stage2",
                }),
                &c,
            )
        });

        assert_eq!(r["ok"], json!(true), "stage failed: {r}");
        assert_eq!(r["patch"], json!(""));
        assert_eq!(
            r["reason"],
            json!("no changes (the proposal matches the committed source)")
        );
        assert_eq!(r["applies_clean"], json!(false));

        let _ = fs::remove_dir_all(&root);
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn uncommitted_file_returns_not_in_sandbox_error() {
        let root = make_temp_git_repo("missing-file-root");
        let home = temp_dir("missing-file-home");
        let c = db_with_workspace(&root);

        let r = with_home(home.to_str().unwrap(), || {
            codeRegenerateStage(
                &json!({
                    "path": "never-committed.txt",
                    "start_line": 1,
                    "end_line": 1,
                    "proposed": "x",
                    "session_id": "stage3",
                }),
                &c,
            )
        });

        assert_eq!(r["ok"], json!(false));
        assert!(
            r["error"]
                .as_str()
                .unwrap()
                .starts_with("file is not in the sandbox"),
            "{r}"
        );

        let _ = fs::remove_dir_all(&root);
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn non_git_workspace_root_returns_ensure_failed_error() {
        let root = temp_dir("not-a-repo");
        let home = temp_dir("not-a-repo-home");
        let c = db_with_workspace(&root);

        let r = with_home(home.to_str().unwrap(), || {
            codeRegenerateStage(
                &json!({
                    "path": "hello.txt",
                    "start_line": 1,
                    "end_line": 1,
                    "proposed": "x",
                    "session_id": "stage4",
                }),
                &c,
            )
        });

        assert_eq!(r["ok"], json!(false));
        assert!(
            r["error"]
                .as_str()
                .unwrap()
                .starts_with("sandbox ensure failed"),
            "{r}"
        );

        let _ = fs::remove_dir_all(&root);
        let _ = fs::remove_dir_all(&home);
    }
}
