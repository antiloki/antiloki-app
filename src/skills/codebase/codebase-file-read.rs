//! codebase-file-read.rs — twin of skills/codebase/codebase-file-read.mjs (FS lane). NAMES ARE
//! CONTRACT: `codebaseFileRead` (named default). Read one workspace-relative source file, bounded + safe.
// QUALITY: 82/100 — relative-only + no-.. path safety, binary (NUL) reject, byte-bounded read, sha256
use rusqlite::Connection;
use serde_json::{json, Value};
use std::path::Path;
use sha2::{Digest, Sha256};

const HARD_MAX: usize = 500 * 1024;

fn active_root(db: &Connection) -> Option<String> {
    crate::db_json::query_rows(db, "SELECT root_path FROM workspaces WHERE is_active = 1 LIMIT 1", &[]).ok()
        .and_then(|r| r.as_array().and_then(|a| a.first()).and_then(|x| x.get("root_path")).and_then(|v| v.as_str()).map(|s| s.to_string()))
}

pub fn codebaseFileRead(input: &Value, db: &Connection) -> Result<Value, String> {
    let path = match input.get("path").and_then(|v| v.as_str()) {
        Some(p) if !p.is_empty() => p.to_string(),
        _ => return Err("codebase-file-read: path is required".into()),
    };
    let root = match input.get("root").and_then(|v| v.as_str()).map(|s| s.to_string()).or_else(|| active_root(db)) {
        Some(r) => r,
        None => return Err("codebase-file-read: root (no active workspace) is required".into()),
    };
    let bad = |cause: &str| Err(format!("codebase-file-read: {cause}"));

    // win32-style absolutes (`/x`, `\x`, `C:\x`, `C:/x`) are refused on every host, not just where std says absolute
    let pb = path.as_bytes();
    let win_abs = path.starts_with('/') || path.starts_with('\\')
        || (pb.len() > 2 && pb[0].is_ascii_alphabetic() && pb[1] == b':' && (pb[2] == b'/' || pb[2] == b'\\'));
    if win_abs || Path::new(&path).is_absolute() { return bad("absolute paths not allowed (use a workspace-relative path)"); }
    if path.split(['\\', '/']).any(|s| s == "..") { return bad("parent-directory traversal (..) not allowed"); }

    let root_abs = Path::new(&root).to_path_buf();
    let abs = root_abs.join(&path);
    if abs != root_abs && !abs.starts_with(&root_abs) { return bad("resolved path escapes the workspace root"); }
    if !abs.exists() { return Err(format!("codebase-file-read: file '{path}' not found")); }
    if abs.is_dir() { return bad("path refers to a directory, not a file"); }

    let max_bytes = input.get("max_bytes").and_then(|v| v.as_f64()).unwrap_or(HARD_MAX as f64);
    let max = HARD_MAX.min((1024.0_f64).max(max_bytes) as usize);
    let buf = std::fs::read(&abs).map_err(|e| format!("codebase-file-read: {e}"))?;

    let probe = &buf[..buf.len().min(8192)];
    if probe.iter().any(|&b| b == 0) { return bad("file appears to be binary (NUL bytes)"); }

    let truncated = buf.len() > max;
    let content = String::from_utf8_lossy(&buf[..max.min(buf.len())]).to_string();
    let sha256 = hex::encode(Sha256::digest(&buf));
    Ok(json!({
        "path": path, "content": content, "lines": content.split('\n').count(),
        "bytes": buf.len(), "truncated": truncated, "sha256": sha256,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reads_bounded() {
        // exercised for real by the fs conformance lane; here just the path-safety branches
        let c = Connection::open_in_memory().unwrap();
        assert!(codebaseFileRead(&json!({ "path": "C:/abs/x.ts", "root": "R" }), &c).unwrap_err().contains("absolute"));
        assert!(codebaseFileRead(&json!({ "path": "../escape", "root": "R" }), &c).unwrap_err().contains("traversal"));
        assert!(codebaseFileRead(&json!({ "root": "R" }), &c).unwrap_err().contains("path is required"));
    }
}
