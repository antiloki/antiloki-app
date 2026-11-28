//! THE FILES PANE'S DOOR (the operator 2026-11-22: "a files pane, a files explorer of your computer"): list a folder, read a file, anywhere on this machine, READ ONLY. It answers only this machine's own screen (`remote::local_only`: a paired phone does not browse the computer's files), never writes, never follows a request to anything that is not a plain path, and keeps the folders that hold keys and secrets (ssh, gpg, cloud credentials, browser profiles' logins) out of the file preview (their names can be listed, their contents are not read).
use axum::{extract::{Query, Request}, http::StatusCode, response::{IntoResponse, Response}, Json};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

const MAX_ENTRIES: usize = 3000;
const MAX_TEXT: u64 = 256 * 1024;
const MAX_IMAGE: u64 = 4 * 1024 * 1024;
const IMAGE_EXT: [(&str, &str); 6] = [("png", "image/png"), ("jpg", "image/jpeg"), ("jpeg", "image/jpeg"), ("gif", "image/gif"), ("webp", "image/webp"), ("bmp", "image/bmp")];

pub fn home() -> PathBuf { std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/")) }
/// A path as the screen wrote it: `~` and an empty path are the home folder. Pure given the home.
pub fn expand(raw: &str, home: &Path) -> PathBuf { let t = raw.trim(); if t.is_empty() || t == "~" { return home.to_path_buf(); } if let Some(r) = t.strip_prefix("~/") { return home.join(r); } PathBuf::from(t) }
/// Is this path somewhere secrets live, so its CONTENTS are not read? Pure.
pub fn is_private(p: &Path) -> bool {
    let s = p.to_string_lossy().replace('\\', "/").to_lowercase();
    ["/.ssh", "/.gnupg", "/.aws", "/.config/gcloud", "/.azure", "/.kube", "/.docker/config", "/.netrc", "/.password-store", "/.config/gh/hosts", "/.mozilla/firefox", "/.config/google-chrome", "/.config/chromium", "/.config/bravesoftware", "/.config/microsoft-edge", "/.local/share/keyrings", "/appdata/roaming/mozilla", "/appdata/local/google/chrome"].iter().any(|k| s.contains(k))
        || p.file_name().map_or(false, |n| { let n = n.to_string_lossy().to_lowercase(); n == ".env" || n.starts_with(".env.") || n.ends_with(".pem") || n.ends_with(".key") || n.starts_with("id_rsa") || n.starts_with("id_ed25519") || n == "credentials" || n == "credentials.json" })
}
/// What is in a folder: folders first, then files, each A to Z; the places a person goes first. Blocking.
pub fn list(raw: &str) -> Value {
    let h = home(); let dir = expand(raw, &h);
    let rd = match std::fs::read_dir(&dir) { Ok(r) => r, Err(e) => return json!({ "ok": false, "error": format!("cannot open {}: {e}", dir.display()), "path": dir.to_string_lossy() }) };
    let mut entries: Vec<Value> = vec![]; let mut more = false;
    for e in rd.flatten() { if entries.len() >= MAX_ENTRIES { more = true; break; }
        let name = e.file_name().to_string_lossy().to_string(); let md = e.metadata().ok(); let ft = e.file_type().ok();
        let link = ft.as_ref().map_or(false, |t| t.is_symlink()); let is_dir = if link { std::fs::metadata(e.path()).map_or(false, |m| m.is_dir()) } else { ft.as_ref().map_or(false, |t| t.is_dir()) };
        entries.push(json!({ "name": name, "kind": if is_dir { "dir" } else { "file" }, "link": link, "size": md.as_ref().filter(|m| m.is_file()).map(|m| m.len()), "mtime": md.as_ref().and_then(|m| m.modified().ok()).and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_millis() as u64), "hidden": name.starts_with('.') })); }
    entries.sort_by(|a, b| { let (da, db) = (a["kind"] == "dir", b["kind"] == "dir"); db.cmp(&da).then_with(|| a["name"].as_str().unwrap_or("").to_lowercase().cmp(&b["name"].as_str().unwrap_or("").to_lowercase())) });
    let places: Vec<Value> = [("Home", h.clone()), ("Desktop", h.join("Desktop")), ("Documents", h.join("Documents")), ("Downloads", h.join("Downloads")), ("Pictures", h.join("Pictures")), ("Projects", h.join("dev")), ("Computer", PathBuf::from(if cfg!(windows) { "C:\\" } else { "/" }))].into_iter().filter(|(_, p)| p.is_dir()).map(|(n, p)| json!({ "name": n, "path": p.to_string_lossy() })).collect();
    json!({ "ok": true, "path": dir.to_string_lossy(), "parent": dir.parent().map(|p| p.to_string_lossy().to_string()), "home": h.to_string_lossy(), "entries": entries, "more": more, "places": places, "private": is_private(&dir) })
}
/// The look of one file: its text (up to 256 KB), an image (up to 4 MB) as a data URL, or just its size when it is something else. Blocking.
pub fn read(raw: &str) -> Value {
    let p = expand(raw, &home());
    if is_private(&p) { return json!({ "ok": true, "kind": "private", "path": p.to_string_lossy(), "note": "this one holds keys or secrets: its name is listed, its contents are not read" }); }
    let md = match std::fs::metadata(&p) { Ok(m) => m, Err(e) => return json!({ "ok": false, "error": format!("cannot read {}: {e}", p.display()) }) };
    if !md.is_file() { return json!({ "ok": false, "error": "that is not a file" }); }
    let ext = p.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
    if let Some((_, mime)) = IMAGE_EXT.iter().find(|(e, _)| *e == ext) { if md.len() > MAX_IMAGE { return json!({ "ok": true, "kind": "big", "size": md.len(), "path": p.to_string_lossy() }); }
        return match std::fs::read(&p) { Ok(b) => json!({ "ok": true, "kind": "image", "size": md.len(), "path": p.to_string_lossy(), "data_url": format!("data:{mime};base64,{}", b64(&b)) }), Err(e) => json!({ "ok": false, "error": e.to_string() }) }; }
    let mut buf = vec![0u8; md.len().min(MAX_TEXT) as usize];
    use std::io::Read; let n = std::fs::File::open(&p).and_then(|mut f| f.read(&mut buf)).unwrap_or(0); buf.truncate(n);
    if buf.contains(&0) { return json!({ "ok": true, "kind": "binary", "size": md.len(), "path": p.to_string_lossy() }); }
    match String::from_utf8(buf) { Ok(t) => json!({ "ok": true, "kind": "text", "size": md.len(), "path": p.to_string_lossy(), "text": t, "truncated": md.len() > MAX_TEXT }), Err(_) => json!({ "ok": true, "kind": "binary", "size": md.len(), "path": p.to_string_lossy() }) }
}
/// What the pane plays or shows straight from the file. Pictures, sound and video only: this door hands out bytes by an address, so nothing a person could be hurt by reading is ever sent through it. (No SVG: it can carry script.)
const MEDIA_EXT: [(&str, &str); 19] = [("png", "image/png"), ("jpg", "image/jpeg"), ("jpeg", "image/jpeg"), ("gif", "image/gif"), ("webp", "image/webp"), ("bmp", "image/bmp"), ("avif", "image/avif"),
    ("mp3", "audio/mpeg"), ("wav", "audio/wav"), ("ogg", "audio/ogg"), ("oga", "audio/ogg"), ("opus", "audio/ogg"), ("flac", "audio/flac"), ("m4a", "audio/mp4"), ("aac", "audio/aac"),
    ("mp4", "video/mp4"), ("m4v", "video/mp4"), ("webm", "video/webm"), ("mov", "video/quicktime")];
const MAX_PIECE: u64 = 4 * 1024 * 1024;
/// The media type of a path by its ending, when the pane may be handed its bytes. Pure.
pub fn media_type(p: &Path) -> Option<&'static str> { let e = p.extension()?.to_string_lossy().to_lowercase(); MEDIA_EXT.iter().find(|(x, _)| *x == e).map(|(_, m)| *m) }
/// `Range: bytes=a-b` over a file of `len` bytes, as (first, last) inclusive, never more than one piece. `None` when it cannot be satisfied. Pure.
pub fn piece(range: &str, len: u64) -> Option<(u64, u64)> {
    let r = range.strip_prefix("bytes=")?.split(',').next()?.trim(); let (a, b) = r.split_once('-')?;
    let (first, last) = if a.is_empty() { let n: u64 = b.parse().ok()?; (len.saturating_sub(n), len.checked_sub(1)?) } else { (a.parse().ok()?, if b.is_empty() { len.checked_sub(1)? } else { b.parse::<u64>().ok()?.min(len.checked_sub(1)?) }) };
    if first >= len || last < first { return None; } Some((first, last.min(first + MAX_PIECE - 1)))
}
/// The bytes of a media file, whole or one piece. Blocking.
pub fn media(raw: &str, range: Option<&str>) -> Result<(u16, &'static str, Option<String>, Vec<u8>), (u16, &'static str)> {
    use std::io::{Read, Seek, SeekFrom};
    let p = expand(raw, &home()); let real = std::fs::canonicalize(&p).map_err(|_| (404, "no such file"))?;
    if is_private(&p) || is_private(&real) { return Err((403, "this one holds keys or secrets")); }
    let mime = media_type(&p).filter(|_| media_type(&real).is_some()).ok_or((415, "only pictures, sound and video are sent this way"))?;
    let mut f = std::fs::File::open(&real).map_err(|_| (404, "cannot open it"))?; let md = f.metadata().map_err(|_| (404, "cannot read it"))?; if !md.is_file() { return Err((404, "that is not a file")); } let len = md.len();
    let (status, first, last) = match range { Some(r) => { let (a, b) = piece(r, len).ok_or((416, "that part is not in the file"))?; (206, a, b) } None => { if len > 64 * 1024 * 1024 { return Err((413, "too big to send whole: ask for a part")); } (200, 0, len.saturating_sub(1)) } };
    let mut buf = vec![0u8; if len == 0 { 0 } else { (last - first + 1) as usize }]; f.seek(SeekFrom::Start(first)).and_then(|_| f.read_exact(&mut buf)).map_err(|_| (404, "cannot read it"))?;
    Ok((status, mime, (status == 206).then(|| format!("bytes {first}-{last}/{len}")), buf))
}
/// `GET /api/v1/files/raw?path=`: a picture, a song or a film, for this computer's own screen. Another site's page is refused.
pub async fn raw_get(Query(q): Query<HashMap<String, String>>, req: Request) -> Response {
    if crate::remote::local_only(&req).is_some() { return deny(); }
    if req.headers().get("sec-fetch-site").and_then(|v| v.to_str().ok()) == Some("cross-site") { return deny(); }
    let p = q.get("path").cloned().unwrap_or_default(); let range = req.headers().get("range").and_then(|v| v.to_str().ok()).map(String::from);
    match tokio::task::spawn_blocking(move || media(&p, range.as_deref())).await.unwrap_or(Err((500, "could not read it"))) {
        Ok((status, mime, part, bytes)) => { let mut b = Response::builder().status(status).header("content-type", mime).header("accept-ranges", "bytes").header("x-content-type-options", "nosniff").header("cache-control", "no-store"); if let Some(r) = part { b = b.header("content-range", r); } b.body(axum::body::Body::from(bytes)).unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response()) }
        Err((code, why)) => (StatusCode::from_u16(code).unwrap_or(StatusCode::BAD_REQUEST), Json(json!({ "ok": false, "error": why }))).into_response(),
    }
}
fn b64(b: &[u8]) -> String { const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/"; let mut o = String::with_capacity(b.len() * 4 / 3 + 4); for c in b.chunks(3) { let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32; o.push(T[(n >> 18 & 63) as usize] as char); o.push(T[(n >> 12 & 63) as usize] as char); o.push(if c.len() > 1 { T[(n >> 6 & 63) as usize] as char } else { '=' }); o.push(if c.len() > 2 { T[(n & 63) as usize] as char } else { '=' }); } o }
fn deny() -> Response { (StatusCode::FORBIDDEN, Json(json!({ "ok": false, "error": "the files of this computer are shown only on this computer's own screen" }))).into_response() }
/// `GET /api/v1/files/list?path=`
pub async fn list_get(Query(q): Query<HashMap<String, String>>, req: Request) -> Response {
    if crate::remote::local_only(&req).is_some() { return deny(); } let p = q.get("path").cloned().unwrap_or_default();
    Json(tokio::task::spawn_blocking(move || list(&p)).await.unwrap_or_else(|_| json!({ "ok": false }))).into_response()
}
/// `GET /api/v1/files/read?path=`
pub async fn read_get(Query(q): Query<HashMap<String, String>>, req: Request) -> Response {
    if crate::remote::local_only(&req).is_some() { return deny(); } let p = q.get("path").cloned().unwrap_or_default();
    Json(tokio::task::spawn_blocking(move || read(&p)).await.unwrap_or_else(|_| json!({ "ok": false }))).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    fn tmp(tag: &str) -> PathBuf { let d = std::env::temp_dir().join(format!("antiloki-files-{tag}-{}", std::process::id())); let _ = std::fs::remove_dir_all(&d); std::fs::create_dir_all(&d).unwrap(); d }
    #[test] fn a_folder_lists_folders_first_then_files_a_to_z_with_sizes_and_a_way_up() {
        let d = tmp("list"); std::fs::create_dir(d.join("zeta")).unwrap(); std::fs::create_dir(d.join("Alpha")).unwrap(); std::fs::write(d.join("b.txt"), "hi").unwrap(); std::fs::write(d.join(".hidden"), "x").unwrap(); std::fs::write(d.join("a.txt"), "hello").unwrap();
        let v = list(&d.to_string_lossy()); assert_eq!(v["ok"], json!(true)); let names: Vec<&str> = v["entries"].as_array().unwrap().iter().map(|e| e["name"].as_str().unwrap()).collect();
        assert_eq!(names, vec!["Alpha", "zeta", ".hidden", "a.txt", "b.txt"]); assert_eq!(v["entries"][3]["size"], json!(5)); assert_eq!(v["entries"][2]["hidden"], json!(true)); assert!(v["parent"].is_string());
        assert_eq!(list("/definitely/not/here")["ok"], json!(false)); let _ = std::fs::remove_dir_all(&d);
    }
    #[test] fn a_file_is_read_as_text_an_image_a_binary_or_kept_private() {
        let d = tmp("read"); std::fs::write(d.join("n.md"), "# hi\n").unwrap(); std::fs::write(d.join("p.png"), [137u8, 80, 78, 71]).unwrap(); std::fs::write(d.join("b.bin"), [0u8, 1, 2]).unwrap(); std::fs::write(d.join(".env"), "KEY=secret").unwrap();
        assert_eq!(read(&d.join("n.md").to_string_lossy())["text"], json!("# hi\n")); assert!(read(&d.join("p.png").to_string_lossy())["data_url"].as_str().unwrap().starts_with("data:image/png;base64,iVBORw==")); assert_eq!(read(&d.join("b.bin").to_string_lossy())["kind"], json!("binary"));
        let secret = read(&d.join(".env").to_string_lossy()); assert_eq!(secret["kind"], json!("private")); assert!(!secret.to_string().contains("KEY=secret"), "the contents are never read"); assert_eq!(read(&d.to_string_lossy())["ok"], json!(false)); let _ = std::fs::remove_dir_all(&d);
    }
    #[test] fn media_is_sent_whole_or_in_pieces_and_nothing_else_goes_through_that_door() {
        let d = tmp("media"); std::fs::write(d.join("a.mp3"), b"0123456789").unwrap(); std::fs::write(d.join("n.txt"), "x").unwrap(); let s = d.join("a.mp3").to_string_lossy().to_string();
        let (code, mime, part, bytes) = media(&s, None).unwrap(); assert_eq!((code, mime, part, bytes.len()), (200, "audio/mpeg", None, 10));
        let (code, _, part, bytes) = media(&s, Some("bytes=2-4")).unwrap(); assert_eq!((code, part.as_deref(), bytes.as_slice()), (206, Some("bytes 2-4/10"), &b"234"[..]));
        assert_eq!(piece("bytes=-3", 10), Some((7, 9))); assert_eq!(piece("bytes=0-", 100 * 1024 * 1024), Some((0, MAX_PIECE - 1))); assert_eq!(piece("bytes=10-", 10), None); assert_eq!(piece("bytes=0-", 0), None);
        assert_eq!(media(&d.join("n.txt").to_string_lossy(), None).unwrap_err().0, 415); assert_eq!(media(&d.join("nope.mp3").to_string_lossy(), None).unwrap_err().0, 404);
        #[cfg(unix)] { std::os::unix::fs::symlink(d.join("n.txt"), d.join("sly.png")).unwrap(); assert_eq!(media(&d.join("sly.png").to_string_lossy(), None).unwrap_err().0, 415, "a link does not smuggle another file"); }
        let _ = std::fs::remove_dir_all(&d);
    }
    #[test] fn keys_and_secrets_are_private_and_home_is_what_an_empty_path_means() {
        for p in ["/home/u/.ssh/id_rsa", "/home/u/.aws/credentials", "/home/u/.config/google-chrome/Default/Login Data", "/x/.env.local", "/x/server.pem"] { assert!(is_private(Path::new(p)), "{p}"); }
        for p in ["/home/u/Documents/a.txt", "/home/u/dev/app/src/main.rs"] { assert!(!is_private(Path::new(p)), "{p}"); }
        let h = Path::new("/home/u"); assert_eq!(expand("", h), h); assert_eq!(expand("~", h), h); assert_eq!(expand("~/dev", h), h.join("dev")); assert_eq!(expand("/etc", h), PathBuf::from("/etc"));
    }
}
