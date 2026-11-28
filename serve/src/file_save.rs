//! SAVING A FILE FROM THE X-RAY CODE EDITOR (operator 2026-10-11: "turn the read-only into a real code editor like VSCode, so we can edit and save code directly").
//!
//! The Code tab used to only READ (`system.codebase.file.read`). This is its twin for writing, and it is deliberately stricter than the read, because a write is the one thing that cannot be undone from here:
//!
//!   POST /api/v1/codebase/file/save {path, content, expected_sha256}
//!     -> 200 {ok, path, sha256, bytes, lines}
//!     -> 409 {ok:false, conflict:true, current_sha256, error}   the file changed on disk since the editor loaded it (an agent, a git checkout, another editor): nothing is written, the editor offers to reload
//!     -> 400 {ok:false, error}                                   refused: see the rules
//!
//! The rules, each of which is a test below:
//!   · the folder is the ACTIVE workspace's, never one the request names - a page cannot aim this at another folder
//!   · a workspace-relative path only: no absolute path (any OS's spelling), no `..`, nothing under `.git` or `.antiloki`, and the real path (symlinks resolved) must still be inside the workspace
//!   · an EXISTING, regular, text, UTF-8 file of at most 500 KB - the read truncates past that, so saving a truncated view would cut the file; creating files is not this route's job
//!   · `expected_sha256` is required and must match the bytes on disk NOW (optimistic concurrency): the editor never overwrites a change it has not seen
//!   · the file's own line endings and permissions survive: a CRLF file stays CRLF (the editor works in LF), the executable bit stays
//!   · atomic: the new bytes go to a sibling temp file, then replace the file in one rename - a crash leaves the old file or the new one, never half
use axum::{http::StatusCode, response::{IntoResponse, Response}, Json};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::dispatch::AppState;

/// the same ceiling `codebase-file-read` applies; past it the read is truncated, so the editor refuses rather than save a cut file
pub const MAX_BYTES: usize = 500 * 1024;

#[derive(Debug, PartialEq)]
pub enum SaveError { Refused(String), Conflict { current_sha256: String } }

#[derive(Debug, PartialEq)]
pub struct Saved { pub sha256: String, pub bytes: usize, pub lines: usize }

fn sha_hex(b: &[u8]) -> String { hex::encode(Sha256::digest(b)) }

/// Is this a relative path that stays inside the workspace and avoids the folders nobody should write through the editor? Pure - tested.
pub fn path_refusal(rel: &str) -> Option<String> {
    let p = rel.trim();
    if p.is_empty() { return Some("a path is required".into()); }
    let b = p.as_bytes();
    // win32-style absolutes are refused on every host, not only where std says absolute
    if p.starts_with('/') || p.starts_with('\\') || (b.len() > 2 && b[0].is_ascii_alphabetic() && b[1] == b':' && (b[2] == b'/' || b[2] == b'\\')) || Path::new(p).is_absolute() {
        return Some("absolute paths are not allowed (use a workspace-relative path)".into());
    }
    let segs: Vec<&str> = p.split(['/', '\\']).collect();
    if segs.iter().any(|s| *s == "..") { return Some("parent-directory traversal (..) is not allowed".into()); }
    if segs.iter().any(|s| s.eq_ignore_ascii_case(".git") || s.eq_ignore_ascii_case(".antiloki")) { return Some("files under .git and .antiloki are not edited from here".into()); }
    None
}

/// The text to write: the editor's LF text, in the file's own line endings (CRLF when the file on disk has any CRLF, else LF). Pure - tested.
pub fn with_file_eol(on_disk: &str, edited: &str) -> String {
    let lf = edited.replace("\r\n", "\n").replace('\r', "\n");
    if on_disk.contains("\r\n") { lf.replace('\n', "\r\n") } else { lf }
}

/// Save `content` over the existing file `rel` inside `root`. Pure over the filesystem it is given - tested against a temp folder.
pub fn save(root: &Path, rel: &str, content: &str, expected_sha256: &str) -> Result<Saved, SaveError> {
    let bad = |m: &str| SaveError::Refused(m.to_string());
    if let Some(m) = path_refusal(rel) { return Err(SaveError::Refused(m)); }
    if expected_sha256.trim().is_empty() { return Err(bad("expected_sha256 is required: the editor must say which version of the file it loaded")); }
    let root_real = root.canonicalize().map_err(|_| bad("the workspace folder is not available"))?;
    let target = root_real.join(rel.trim());
    if !target.exists() { return Err(bad("the file does not exist: the editor saves existing files only")); }
    let real = target.canonicalize().map_err(|e| SaveError::Refused(format!("cannot resolve the file: {e}")))?;
    if !real.starts_with(&root_real) { return Err(bad("the file resolves to a place outside the workspace")); }
    // a path that REACHES .git/.antiloki through a symlink is refused too
    if real.strip_prefix(&root_real).ok().map(|r| r.components().any(|c| { let s = c.as_os_str().to_string_lossy(); s.eq_ignore_ascii_case(".git") || s.eq_ignore_ascii_case(".antiloki") })).unwrap_or(false) { return Err(bad("files under .git and .antiloki are not edited from here")); }
    let meta = std::fs::metadata(&real).map_err(|e| SaveError::Refused(e.to_string()))?;
    if !meta.is_file() { return Err(bad("that is not a regular file")); }
    let disk = std::fs::read(&real).map_err(|e| SaveError::Refused(format!("cannot read the file: {e}")))?;
    let current = sha_hex(&disk);
    if !current.eq_ignore_ascii_case(expected_sha256.trim()) { return Err(SaveError::Conflict { current_sha256: current }); }
    if disk.len() > MAX_BYTES { return Err(bad("the file is larger than 500 KB: the editor only opened a cut copy of it, so it will not save over the whole file")); }
    if disk[..disk.len().min(8192)].contains(&0) { return Err(bad("the file looks binary")); }
    let on_disk = std::str::from_utf8(&disk).map_err(|_| bad("the file is not valid UTF-8, so a text editor would corrupt it"))?;
    if content.contains('\0') { return Err(bad("the text contains a NUL character")); }
    let out = with_file_eol(on_disk, content);
    if out.len() > MAX_BYTES { return Err(bad("the edited file would be larger than 500 KB")); }

    // atomic: a sibling temp file, the old permissions, then one rename over the file
    let dir = real.parent().ok_or_else(|| bad("the file has no folder"))?;
    let name = real.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    let tmp: PathBuf = dir.join(format!(".{name}.antiloki-save-{}", std::process::id()));
    let result = (|| -> std::io::Result<()> {
        std::fs::write(&tmp, out.as_bytes())?;
        std::fs::set_permissions(&tmp, meta.permissions())?;
        std::fs::rename(&tmp, &real)
    })();
    if let Err(e) = result { let _ = std::fs::remove_file(&tmp); return Err(SaveError::Refused(format!("could not write the file: {e}"))); }
    Ok(Saved { sha256: sha_hex(out.as_bytes()), bytes: out.len(), lines: out.split('\n').count() })
}

/// `POST /api/v1/codebase/file/save`
pub async fn handle(state: Arc<AppState>, body: axum::body::Bytes) -> Response {
    let b: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let (path, content, expected) = (
        b.get("path").and_then(Value::as_str).unwrap_or("").to_string(),
        b.get("content").and_then(Value::as_str),
        b.get("expected_sha256").and_then(Value::as_str).unwrap_or("").to_string(),
    );
    let Some(content) = content.map(String::from) else { return (StatusCode::BAD_REQUEST, Json(json!({ "ok": false, "error": "content is required" }))).into_response() };
    let root = crate::agents_http::active_root(&state).await;
    if root.is_empty() { return (StatusCode::BAD_REQUEST, Json(json!({ "ok": false, "error": "no workspace is open" }))).into_response() }
    let rel = path.clone();
    let res = tokio::task::spawn_blocking(move || save(Path::new(&root), &rel, &content, &expected)).await
        .unwrap_or_else(|e| Err(SaveError::Refused(e.to_string())));
    match res {
        Ok(s) => Json(json!({ "ok": true, "path": path, "sha256": s.sha256, "bytes": s.bytes, "lines": s.lines })).into_response(),
        Err(SaveError::Conflict { current_sha256 }) => (StatusCode::CONFLICT, Json(json!({ "ok": false, "conflict": true, "current_sha256": current_sha256,
            "error": "the file changed on disk since it was opened (an agent, git or another editor): reload it before saving" }))).into_response(),
        Err(SaveError::Refused(m)) => (StatusCode::BAD_REQUEST, Json(json!({ "ok": false, "error": m }))).into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn dir(tag: &str) -> PathBuf {
        static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let d = std::env::temp_dir().join(format!("antiloki-save-{tag}-{}-{}", std::process::id(), N.fetch_add(1, std::sync::atomic::Ordering::SeqCst)));
        let _ = std::fs::remove_dir_all(&d); std::fs::create_dir_all(&d).unwrap(); d
    }
    fn put(d: &Path, rel: &str, body: &[u8]) { let p = d.join(rel); std::fs::create_dir_all(p.parent().unwrap()).unwrap(); std::fs::write(p, body).unwrap(); }

    #[test]
    fn paths_that_leave_the_workspace_or_reach_git_are_refused_before_anything_is_read() {
        for p in ["", "  ", "/etc/passwd", "\\windows\\x", "C:\\x\\y.ts", "C:/x/y.ts", "../x.ts", "a/../../x.ts", "a\\..\\x.ts", ".git/config", "src/.GIT/hooks/pre-commit", ".antiloki/db/memory.sqlite"] {
            assert!(path_refusal(p).is_some(), "{p:?} must be refused");
        }
        for p in ["src/a.ts", "README.md", "a/b/c.rs", ".github/workflows/ci.yml", ".env", "src/.gitkeep"] { assert_eq!(path_refusal(p), None, "{p:?}"); }
    }
    #[test]
    fn a_save_replaces_the_text_returns_the_new_hash_and_leaves_no_temp_file() {
        let d = dir("ok"); put(&d, "src/a.ts", b"let a = 1;\n");
        let before = sha_hex(b"let a = 1;\n");
        let s = save(&d, "src/a.ts", "let a = 2;\nlet b = 3;\n", &before).unwrap();
        assert_eq!(std::fs::read_to_string(d.join("src/a.ts")).unwrap(), "let a = 2;\nlet b = 3;\n");
        assert_eq!(s.sha256, sha_hex(b"let a = 2;\nlet b = 3;\n")); assert_eq!(s.bytes, 22); assert_eq!(s.lines, 3);
        assert_eq!(std::fs::read_dir(d.join("src")).unwrap().count(), 1, "no temp file left behind");
        // the NEW hash is what the next save must present
        assert!(save(&d, "src/a.ts", "x\n", &s.sha256).is_ok());
    }
    #[test]
    fn a_file_changed_on_disk_since_it_was_opened_is_a_conflict_and_nothing_is_written() {
        let d = dir("conflict"); put(&d, "a.txt", b"mine\n");
        let loaded = sha_hex(b"mine\n");
        std::fs::write(d.join("a.txt"), b"an agent edited this\n").unwrap();
        match save(&d, "a.txt", "my edit\n", &loaded) { Err(SaveError::Conflict { current_sha256 }) => assert_eq!(current_sha256, sha_hex(b"an agent edited this\n")), other => panic!("{other:?}") }
        assert_eq!(std::fs::read_to_string(d.join("a.txt")).unwrap(), "an agent edited this\n");
        assert!(matches!(save(&d, "a.txt", "x", ""), Err(SaveError::Refused(m)) if m.contains("expected_sha256")), "a save with no version is refused");
    }
    #[test]
    fn crlf_files_stay_crlf_and_lf_files_stay_lf() {
        assert_eq!(with_file_eol("a\r\nb\r\n", "x\ny\n"), "x\r\ny\r\n");
        assert_eq!(with_file_eol("a\nb\n", "x\ny\n"), "x\ny\n");
        assert_eq!(with_file_eol("a\r\nb\r\n", "x\r\ny\n"), "x\r\ny\r\n", "no double conversion");
        let d = dir("crlf"); put(&d, "w.txt", b"one\r\ntwo\r\n");
        save(&d, "w.txt", "one\ntwo\nthree\n", &sha_hex(b"one\r\ntwo\r\n")).unwrap();
        assert_eq!(std::fs::read(d.join("w.txt")).unwrap(), b"one\r\ntwo\r\nthree\r\n");
    }
    #[test]
    fn missing_binary_non_utf8_big_and_directory_targets_are_refused() {
        let d = dir("kinds"); put(&d, "bin.dat", &[1, 2, 0, 3]); put(&d, "latin.txt", &[0x63, 0x61, 0x66, 0xE9]); put(&d, "d/x.txt", b"x");
        let refused = |rel: &str, body: &[u8]| matches!(save(&d, rel, "t", &sha_hex(body)), Err(SaveError::Refused(_)));
        assert!(refused("nope.txt", b""), "a missing file is not created");
        assert!(refused("bin.dat", &[1, 2, 0, 3]), "binary"); assert!(refused("latin.txt", &[0x63, 0x61, 0x66, 0xE9]), "not UTF-8");
        assert!(matches!(save(&d, "d", "t", "x"), Err(SaveError::Conflict { .. }) | Err(SaveError::Refused(_))), "a directory");
        let big = vec![b'a'; MAX_BYTES + 1]; put(&d, "big.txt", &big);
        assert!(matches!(save(&d, "big.txt", "t", &sha_hex(&big)), Err(SaveError::Refused(m)) if m.contains("500 KB")), "a file the read would have cut");
        assert!(matches!(save(&d, "a.txt", "x\0y", "z"), Err(SaveError::Refused(_))));
    }
    #[cfg(unix)]
    #[test]
    fn a_symlink_out_of_the_workspace_and_the_executable_bit() {
        use std::os::unix::fs::{symlink, PermissionsExt};
        let d = dir("link"); let outside = dir("outside"); put(&outside, "secret.txt", b"s\n");
        symlink(outside.join("secret.txt"), d.join("link.txt")).unwrap();
        assert!(matches!(save(&d, "link.txt", "pwned\n", &sha_hex(b"s\n")), Err(SaveError::Refused(m)) if m.contains("outside")), "a link out is not followed");
        assert_eq!(std::fs::read_to_string(outside.join("secret.txt")).unwrap(), "s\n");
        put(&d, "run.sh", b"#!/bin/sh\n"); std::fs::set_permissions(d.join("run.sh"), std::fs::Permissions::from_mode(0o755)).unwrap();
        save(&d, "run.sh", "#!/bin/sh\necho hi\n", &sha_hex(b"#!/bin/sh\n")).unwrap();
        assert_eq!(std::fs::metadata(d.join("run.sh")).unwrap().permissions().mode() & 0o777, 0o755, "still executable");
    }
}
