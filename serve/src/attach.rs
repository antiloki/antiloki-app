//! A FILE THE PERSON GIVES AN AGENT (operator 2026-11-11: "when I want to show an image it is really hard, because I cannot just paste the image"): pasted, imported or dropped on an agent's terminal, it is saved in the agent's box
//! under `.antiloki/attachments/` and the screen types its `@path` into the agent's input, which is how the engines' CLIs take a file (an image is read from its path).
//!
//!   POST /api/v1/cockpit/pane/attach {pane_id, name, data_base64}  ->  { ok, path (relative to the box), abs, bytes, kind }
//!
//! Only images, PDFs and plain text; at most 20 MB; the name is made safe and unique; nothing leaves the box. Pure parts (name, kind) are tested.
use crate::dispatch::AppState;
use axum::{body::Bytes, http::StatusCode, response::{IntoResponse, Response}, Json};
use serde_json::{json, Value};
use std::path::Path;
use std::sync::Arc;

pub const MAX_BYTES: usize = 20 * 1024 * 1024;
fn reply(v: Value) -> Response { (StatusCode::OK, Json(v)).into_response() }

/// What a file is, from its bytes (never from the name the browser claims): `image`, `pdf`, `text`, or None (refused). Pure - tested.
pub fn kind_of(b: &[u8]) -> Option<(&'static str, &'static str)> {
    if b.starts_with(&[0x89, b'P', b'N', b'G']) { return Some(("image", "png")); }
    if b.starts_with(&[0xFF, 0xD8, 0xFF]) { return Some(("image", "jpg")); }
    if b.starts_with(b"GIF8") { return Some(("image", "gif")); }
    if b.len() > 12 && &b[0..4] == b"RIFF" && &b[8..12] == b"WEBP" { return Some(("image", "webp")); }
    if b.starts_with(b"%PDF") { return Some(("pdf", "pdf")); }
    if !b.is_empty() && !b.contains(&0) && std::str::from_utf8(b).is_ok() { return Some(("text", "txt")); }
    None
}
/// A name that is safe to write: letters, digits, dot, dash, underscore; the extension the bytes call for; a time prefix so a second paste never overwrites the first. Pure - tested.
pub fn safe_name(given: &str, ext: &str, stamp: i64) -> String {
    let stem = Path::new(given).file_stem().and_then(|s| s.to_str()).unwrap_or("file");
    let clean: String = stem.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '-' }).collect::<String>().trim_matches('-').chars().take(40).collect();
    format!("{stamp}-{}.{ext}", if clean.is_empty() { "file".to_string() } else { clean })
}
fn decode(s: &str) -> Option<Vec<u8>> {
    let t: String = s.split(',').last().unwrap_or("").chars().filter(|c| !c.is_whitespace()).collect();
    let mut out = Vec::with_capacity(t.len() * 3 / 4); let (mut buf, mut bits) = (0u32, 0u32);
    for c in t.bytes() { let v = match c { b'A'..=b'Z' => c - b'A', b'a'..=b'z' => c - b'a' + 26, b'0'..=b'9' => c - b'0' + 52, b'+' | b'-' => 62, b'/' | b'_' => 63, b'=' => continue, _ => return None }; buf = (buf << 6) | v as u32; bits += 6; if bits >= 8 { bits -= 8; out.push((buf >> bits) as u8); buf &= (1 << bits) - 1; } }
    Some(out)
}

pub async fn post(state: Arc<AppState>, body: Bytes) -> Response {
    let b: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let id = b["pane_id"].as_i64().unwrap_or(0); if id <= 0 { return reply(json!({ "ok": false, "error": "say which pane" })); }
    let Some(bytes) = b["data_base64"].as_str().and_then(decode) else { return reply(json!({ "ok": false, "error": "the file did not arrive intact" })) };
    if bytes.is_empty() { return reply(json!({ "ok": false, "error": "the file is empty" })); }
    if bytes.len() > MAX_BYTES { return reply(json!({ "ok": false, "error": format!("the file is over {} MB", MAX_BYTES / 1024 / 1024) })); }
    let Some((kind, ext)) = kind_of(&bytes) else { return reply(json!({ "ok": false, "error": "only images, PDFs and plain text can be given to an agent" })) };
    let dir = match crate::chat_pane::pane_row(&state, id).await { Some(p) => crate::shared_folder::working_dir(&state, &p).await, None => return reply(json!({ "ok": false, "error": "no such pane" })) };
    if dir.is_empty() || !Path::new(&dir).exists() { return reply(json!({ "ok": false, "error": "this pane has no box on disk" })); }
    let name = safe_name(b["name"].as_str().unwrap_or(""), ext, crate::cli::now_ms());
    let rel = format!(".antiloki/attachments/{name}"); let abs = Path::new(&dir).join(&rel);
    let n = bytes.len();
    let r = tokio::task::spawn_blocking(move || { if let Some(d) = abs.parent() { std::fs::create_dir_all(d).map_err(|e| e.to_string())?; } std::fs::write(&abs, &bytes).map_err(|e| e.to_string())?; Ok::<String, String>(abs.to_string_lossy().to_string()) }).await;
    match r { Ok(Ok(abs)) => reply(json!({ "ok": true, "path": rel, "abs": abs, "bytes": n, "kind": kind })), Ok(Err(e)) => reply(json!({ "ok": false, "error": e })), Err(e) => reply(json!({ "ok": false, "error": e.to_string() })) }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn a_file_is_known_by_its_bytes_and_its_name_is_made_safe_and_unique() {
        assert_eq!(kind_of(&[0x89, b'P', b'N', b'G', 1, 2]), Some(("image", "png"))); assert_eq!(kind_of(&[0xFF, 0xD8, 0xFF, 0xE0]), Some(("image", "jpg"))); assert_eq!(kind_of(b"GIF89a"), Some(("image", "gif")));
        assert_eq!(kind_of(b"RIFF\0\0\0\0WEBPVP8 "), Some(("image", "webp"))); assert_eq!(kind_of(b"%PDF-1.7"), Some(("pdf", "pdf"))); assert_eq!(kind_of(b"hello\nworld"), Some(("text", "txt")));
        assert_eq!(kind_of(&[0x7f, b'E', b'L', b'F', 0, 1]), None, "a program is not given to an agent"); assert_eq!(kind_of(b""), None);
        assert_eq!(safe_name("My Screen Shot (1).PNG", "png", 99), "99-My-Screen-Shot--1.png"); assert_eq!(safe_name("../../etc/passwd", "txt", 5), "5-passwd.txt"); assert_eq!(safe_name("", "png", 5), "5-file.png"); assert_eq!(safe_name("image.png", "png", 5), "5-image.png");
    }
    #[test]
    fn base64_arrives_with_or_without_the_data_url_prefix() {
        assert_eq!(decode("aGVsbG8="), Some(b"hello".to_vec())); assert_eq!(decode("data:image/png;base64,aGVsbG8"), Some(b"hello".to_vec())); assert_eq!(decode("a G V s b G 8 ="), Some(b"hello".to_vec())); assert_eq!(decode("@@@"), None);
    }
}
