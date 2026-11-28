//! system/code-file.rs — twin of skills/system/code-file.mjs. BESPOKE-NATIVE, not conform-db
//! fixtured (same class as `system_code_tree`'s `codeTree`/`resolveBrowseRoot`, which this file
//! reuses as its cross-module dependency): the default export UNCONDITIONALLY shells out to a
//! REAL `git ls-files --error-unmatch` subprocess against the real `root` directory AND reads a
//! REAL file off disk (`statSync`/`readFileSync`) — neither is seedable through the conform-db
//! fixture format (it only seeds `context.db` rows). Proven below via `#[cfg(test)]` native tests
//! against real temp git repos, mirroring `system_code_tree.rs`'s precedent.
//!
//! NAMES ARE CONTRACT: `codeFile` (default export only — no other named export in the JS).
//!
//! Cross-module calls (already-converted twins, reused not reimplemented):
//! `crate::system_code_tree::resolveBrowseRoot` (root selection: registered session worktree or
//! the active workspace root) and `crate::engine_workspace_root::resolveWorkspacePath` (the
//! no-explicit-root containment/traversal guard).
//!
//! Two containment guards, exactly as split in the JS:
//! 1. When `input.root` is explicitly provided (truthy after `String(...).trim()`): this file
//!    resolves `path` against that root ITSELF and rejects escapes inline — `resolveWorkspacePath`
//!    is NOT used here because it always resolves against the *active workspace* root, not an
//!    arbitrary registered worktree. The win32 path-resolve/containment helpers below are copied
//!    verbatim (not exported, so not a `names_ok` contract) from `engine_workspace_root.rs`'s
//!    already-verified win32 port — see that twin's module doc for the win32-only-scope and
//!    prefix-check-vs-`path.relative()` rationale, which applies identically here.
//! 2. Otherwise: `crate::engine_workspace_root::resolveWorkspacePath(db, path)` — throws on
//!    traversal/escape against the active workspace root (already covered by that twin's own
//!    tests; this file's tests only need to prove the call is wired, not re-litigate the algorithm).
//!
//! `MAX_BYTES = 2 * 1024 * 1024`: files at or under this size are read in full (`content`,
//! `truncated: false`); larger files short-circuit to `{content: "", truncated: true, note}`
//! WITHOUT ever calling `readFileSync` — ported literally (the twin never reads bytes it doesn't
//! need to serialize).
//!
//! `rel = path.replace(/\\/g, '/')`: the returned `path` field and the git pathspec both use the
//! forward-slash form regardless of what separator the caller passed in `path`.
// QUALITY: bespoke-native (real git subprocess + real fs read, no conform-db fixture — see module doc)

use rusqlite::Connection;
use serde_json::{json, Value};

const MAX_BYTES: u64 = 2 * 1024 * 1024;

fn str_qq(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(),
        Some(o) => o.to_string(),
    }
}

// ── win32 path port, copied verbatim (private helper, not a names_ok contract) from
// engine/workspace-root.rs — see that twin's module doc for the full derivation rationale. ──
fn is_win_sep(c: u8) -> bool { c == b'/' || c == b'\\' }

fn normalize_string_win(path: &[u8], allow_above_root: bool) -> String {
    let mut res: Vec<u8> = Vec::new();
    let mut last_segment_length: isize = 0;
    let mut last_slash: isize = -1;
    let mut dots: isize = 0;
    let mut ch: u8 = 0;
    let n = path.len() as isize;
    let mut i: isize = 0;
    while i <= n {
        if i < n {
            ch = path[i as usize];
        } else if is_win_sep(ch) {
            break;
        } else {
            ch = b'\\';
        }
        if is_win_sep(ch) {
            if last_slash == i - 1 || dots == 1 {
                // NOOP: `\\` collapses, `\.\` drops
            } else if dots == 2 {
                if res.len() < 2 || last_segment_length != 2
                    || res[res.len() - 1] != b'.' || res[res.len() - 2] != b'.'
                {
                    if res.len() > 2 {
                        match res.iter().rposition(|&c| is_win_sep(c)) {
                            Some(lsi) => {
                                res.truncate(lsi);
                                let new_last = res.iter().rposition(|&c| is_win_sep(c)).map(|x| x as isize).unwrap_or(-1);
                                last_segment_length = res.len() as isize - 1 - new_last;
                            }
                            None => { res.clear(); last_segment_length = 0; }
                        }
                        last_slash = i;
                        dots = 0;
                        i += 1;
                        continue;
                    } else if !res.is_empty() {
                        res.clear();
                        last_segment_length = 0;
                        last_slash = i;
                        dots = 0;
                        i += 1;
                        continue;
                    }
                }
                if allow_above_root {
                    if !res.is_empty() { res.push(b'\\'); res.push(b'.'); res.push(b'.'); }
                    else { res.push(b'.'); res.push(b'.'); }
                    last_segment_length = 2;
                }
            } else {
                let seg = &path[(last_slash + 1) as usize..i as usize];
                if !res.is_empty() { res.push(b'\\'); res.extend_from_slice(seg); }
                else { res.extend_from_slice(seg); }
                last_segment_length = i - last_slash - 1;
            }
            last_slash = i;
            dots = 0;
        } else if ch == b'.' && dots != -1 {
            dots += 1;
        } else {
            dots = -1;
        }
        i += 1;
    }
    String::from_utf8(res).unwrap_or_default()
}

fn win_normalize(path: &str) -> String {
    let bytes = path.as_bytes();
    let len = bytes.len();
    if len == 0 { return ".".into(); }
    if len == 1 {
        return if is_win_sep(bytes[0]) { "\\".into() } else { path.to_string() };
    }
    let mut root_end: usize = 0;
    let mut device: Option<String> = None;
    let mut is_absolute = false;
    if is_win_sep(bytes[0]) {
        is_absolute = true;
        root_end = 1;
    } else if bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        device = Some(String::from_utf8(bytes[0..2].to_vec()).unwrap_or_default());
        root_end = 2;
        if len > 2 && is_win_sep(bytes[2]) {
            is_absolute = true;
            root_end = 3;
        }
    }
    let mut tail = if root_end < len { normalize_string_win(&bytes[root_end..], !is_absolute) } else { String::new() };
    if tail.is_empty() && !is_absolute { tail = ".".into(); }
    if !tail.is_empty() && is_win_sep(bytes[len - 1]) { tail.push('\\'); }
    match device {
        None => if is_absolute { if !tail.is_empty() { format!("\\{tail}") } else { "\\".into() } } else { tail },
        Some(d) => if is_absolute { if !tail.is_empty() { format!("{d}\\{tail}") } else { format!("{d}\\") } } else { format!("{d}{tail}") },
    }
}

fn win_is_absolute(p: &str) -> bool {
    let b = p.as_bytes();
    if b.is_empty() { return false; }
    if is_win_sep(b[0]) { return true; }
    if b.len() > 2 && b[0].is_ascii_alphabetic() && b[1] == b':' && is_win_sep(b[2]) { return true; }
    false
}

fn win_resolve1(p: &str) -> String { win_normalize(p) }

fn win_resolve2(root: &str, rel: &str) -> String {
    let root_trimmed = root.trim_end_matches(|c| c == '/' || c == '\\');
    let joined = if root_trimmed.is_empty() { rel.to_string() } else { format!("{root_trimmed}\\{rel}") };
    win_normalize(&joined)
}
// ── end copied win32 helpers ──────────────────────────────────────────────────────────────

/// `path.posix.normalize` for an ABSOLUTE input: collapse `.`, `..` and repeated separators lexically
/// (`..` never climbs above `/`). Same shape as workspace-root.rs's private copy.
fn posix_normalize(p: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for seg in p.split('/') {
        match seg {
            "" | "." => {}
            ".." => { parts.pop(); }
            s => parts.push(s),
        }
    }
    format!("/{}", parts.join("/"))
}

/// `spawnSync('git', ['-C', root, 'ls-files', '--error-unmatch', '--', rel], {...})` — real
/// subprocess. `false` mirrors JS's `tracked.status !== 0` (untracked, missing git, non-repo).
fn git_tracked(root: &str, rel: &str) -> bool {
    match crate::proc::cmd("git").args(["-C", root, "ls-files", "--error-unmatch", "--", rel]).output() {
        Ok(out) => out.status.success(),
        Err(_) => false,
    }
}

/// `export default function codeFile(input, context)` — see module doc for the two containment
/// guards and the size short-circuit.
pub fn codeFile(input: &Value, db: &Connection) -> Result<Value, String> {
    let empty = json!({});
    let input_obj = if input.is_object() { input } else { &empty };
    let path = match input_obj.get("path").and_then(Value::as_str) {
        Some(s) if !s.is_empty() => s.to_string(),
        _ => return Err("code-file: path is required".to_string()),
    };

    let input_root = input_obj.get("root").cloned().unwrap_or(Value::Null);
    let root_v = crate::system_code_tree::resolveBrowseRoot(db, &input_root, &json!("code-file"))?;
    let root = root_v.as_str().unwrap_or("").to_string();

    let explicit_root = !str_qq(Some(&input_root)).trim().is_empty();

    // Backslashes are separators for the UI's callers (a Windows client sends `src\\app.ts`); on POSIX the
    // filesystem would read that as ONE odd filename, so resolve the slash-normalized form there.
    let path_fwd = path.replace('\\', "/");

    let abs: String;
    if explicit_root && cfg!(windows) {
        abs = if win_is_absolute(&path) { win_resolve1(&path) } else { win_resolve2(&root, &path) };
        let root_resolved = win_resolve1(&root);
        let abs_lc = abs.to_ascii_lowercase();
        let root_lc = root_resolved.to_ascii_lowercase();
        if abs_lc != root_lc {
            let root_prefix = if root_lc.ends_with('\\') { root_lc.clone() } else { format!("{root_lc}\\") };
            if !abs_lc.starts_with(&root_prefix) {
                return Err("code-file: path must be a path inside the worktree (got string)".to_string());
            }
        }
    } else if explicit_root {
        // POSIX twin of the branch above: `path.posix.resolve(root, path)` (lexical, case-sensitive) + the
        // same containment guard. The win32 helpers would yield `\tmp\...`, which no `fs::metadata` can stat.
        abs = if path_fwd.starts_with('/') { posix_normalize(&path_fwd) } else { posix_normalize(&format!("{}/{}", root.trim_end_matches('/'), path_fwd)) };
        let root_resolved = posix_normalize(&root);
        if abs != root_resolved {
            let root_prefix = if root_resolved.ends_with('/') { root_resolved.clone() } else { format!("{root_resolved}/") };
            if !abs.starts_with(&root_prefix) {
                return Err("code-file: path must be a path inside the worktree (got string)".to_string());
            }
        }
    } else {
        let resolved = crate::engine_workspace_root::resolveWorkspacePath(db, &json!(if cfg!(windows) { &path } else { &path_fwd }))?;
        abs = resolved.as_str().unwrap_or_default().to_string();
    }

    let rel = path.replace('\\', "/");

    if !git_tracked(&root, &rel) {
        return Err(format!("code-file: tracked file '{rel}' not found"));
    }

    let meta = std::fs::metadata(&abs).map_err(|e| e.to_string())?;
    let size = meta.len();
    if size > MAX_BYTES {
        return Ok(json!({
            "path": rel,
            "size": size,
            "truncated": true,
            "content": "",
            "lines": 0,
            "note": format!("file too large to display ({size} bytes > {MAX_BYTES})"),
        }));
    }

    let content = std::fs::read_to_string(&abs).map_err(|e| e.to_string())?;
    let lines = content.split('\n').count() as i64;
    Ok(json!({ "path": rel, "size": size, "truncated": false, "content": content, "lines": lines }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use std::process::Command;

    fn temp_dir(tag: &str) -> PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!("antiloki-code-file-rs-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn run_git_ok(args: &[&str], cwd: &std::path::Path) {
        let out = Command::new("git").args(args).current_dir(cwd).output().unwrap();
        assert!(out.status.success(), "git {:?} failed: {}", args, String::from_utf8_lossy(&out.stderr));
    }

    fn make_temp_git_repo(tag: &str) -> PathBuf {
        let dir = temp_dir(tag);
        run_git_ok(&["init", "-q"], &dir);
        run_git_ok(&["config", "user.email", "test@test"], &dir);
        run_git_ok(&["config", "user.name", "test"], &dir);
        dir
    }

    fn db_with_workspace(root: &std::path::Path) -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE workspaces (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL UNIQUE, \
             root_path TEXT NOT NULL, vcs TEXT NOT NULL DEFAULT 'git', is_active INTEGER NOT NULL DEFAULT 0, \
             created_at TEXT NOT NULL DEFAULT '2026-01-01 00:00:00'); \
             CREATE TABLE cockpit_panes (id INTEGER PRIMARY KEY AUTOINCREMENT, worktree_path TEXT)",
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
    fn code_file_missing_path_errs() {
        let root = make_temp_git_repo("missing-path");
        let c = db_with_workspace(&root);
        let e = codeFile(&json!({}), &c).unwrap_err();
        assert_eq!(e, "code-file: path is required");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn code_file_reads_a_tracked_file_via_active_workspace_root() {
        let root = make_temp_git_repo("tracked");
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/app.ts"), "console.log('hi');\n").unwrap();
        run_git_ok(&["add", "src/app.ts"], &root);
        let c = db_with_workspace(&root);

        let r = codeFile(&json!({ "path": "src/app.ts" }), &c).unwrap();
        assert_eq!(r["path"], json!("src/app.ts"));
        assert_eq!(r["truncated"], json!(false));
        assert_eq!(r["content"], json!("console.log('hi');\n"));
        assert_eq!(r["lines"], json!(2)); // trailing \n produces a final empty split segment
        assert_eq!(r["size"], json!(19u64)); // "console.log('hi');\n".len() == 19 bytes (18 chars + trailing \n)
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn code_file_normalizes_backslash_path_separators_in_output() {
        let root = make_temp_git_repo("backslash");
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/app.ts"), "x").unwrap();
        run_git_ok(&["add", "src/app.ts"], &root);
        let c = db_with_workspace(&root);

        let r = codeFile(&json!({ "path": "src\\app.ts" }), &c).unwrap();
        assert_eq!(r["path"], json!("src/app.ts"));
        assert_eq!(r["content"], json!("x"));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn code_file_untracked_file_is_not_found_even_if_present_on_disk() {
        let root = make_temp_git_repo("untracked");
        fs::write(root.join("ignored.txt"), "not tracked").unwrap();
        let c = db_with_workspace(&root);

        let e = codeFile(&json!({ "path": "ignored.txt" }), &c).unwrap_err();
        assert_eq!(e, "code-file: tracked file 'ignored.txt' not found");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn code_file_default_root_traversal_escape_is_rejected() {
        let root = make_temp_git_repo("traversal");
        let c = db_with_workspace(&root);
        let e = codeFile(&json!({ "path": "../escape.txt" }), &c).unwrap_err();
        assert!(e.contains("escapes workspace root"), "{e}");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn code_file_truncates_when_over_max_bytes() {
        let root = make_temp_git_repo("big");
        let big = "a".repeat((MAX_BYTES + 10) as usize);
        fs::write(root.join("big.txt"), &big).unwrap();
        run_git_ok(&["add", "big.txt"], &root);
        let c = db_with_workspace(&root);

        let r = codeFile(&json!({ "path": "big.txt" }), &c).unwrap();
        assert_eq!(r["truncated"], json!(true));
        assert_eq!(r["content"], json!(""));
        assert_eq!(r["lines"], json!(0));
        assert_eq!(r["size"], json!(big.len() as u64));
        assert!(r["note"].as_str().unwrap().contains("file too large to display"));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn code_file_explicit_registered_worktree_root_reads_and_guards_escape() {
        // A worktree registered in cockpit_panes, DIFFERENT from the active workspace root —
        // exercises the explicit-root branch (inline containment check, not resolveWorkspacePath).
        let active_root = make_temp_git_repo("explicit-active");
        let worktree = make_temp_git_repo("explicit-worktree");
        fs::create_dir_all(worktree.join("lib")).unwrap();
        fs::write(worktree.join("lib/util.js"), "module.exports = {};\n").unwrap();
        run_git_ok(&["add", "lib/util.js"], &worktree);
        let c = db_with_workspace(&active_root);
        let wt_str = worktree.to_string_lossy().into_owned();
        c.execute("INSERT INTO cockpit_panes (worktree_path) VALUES (?1)", rusqlite::params![wt_str.clone()]).unwrap();

        let r = codeFile(&json!({ "path": "lib/util.js", "root": wt_str }), &c).unwrap();
        assert_eq!(r["path"], json!("lib/util.js"));
        assert_eq!(r["content"], json!("module.exports = {};\n"));

        let e = codeFile(&json!({ "path": "../escape.js", "root": wt_str }), &c).unwrap_err();
        assert_eq!(e, "code-file: path must be a path inside the worktree (got string)");

        let _ = fs::remove_dir_all(&active_root);
        let _ = fs::remove_dir_all(&worktree);
    }

    #[test]
    fn code_file_unregistered_root_propagates_not_found_from_resolve_browse_root() {
        let root = make_temp_git_repo("unreg-root");
        let c = db_with_workspace(&root);
        let e = codeFile(&json!({ "path": "x.txt", "root": "C:/not/registered" }), &c).unwrap_err();
        assert_eq!(e, "code-file: registered session worktree 'C:/not/registered' not found");
        let _ = fs::remove_dir_all(&root);
    }
}
