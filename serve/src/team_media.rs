//! THE MEDIA OF A RUN (the operator 2026-11-15, on a video team: "the video still cannot be seen on the preview"): a team that makes a video leaves it in its working folder (`video/out/test.mp4`), which no page of the app could open. A run's media are the
//! videos, pictures and sounds in the trees it worked in; this lists them and streams one, with byte ranges so a player can seek. Read-only; a file is served only from inside one of THIS run's trees, only with a media extension, and never through `..`.
use crate::AppState;
use axum::{body::Body, http::{header, HeaderMap, StatusCode}, response::{IntoResponse, Response}};
use serde_json::{json, Value};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

/// (extension, kind, content type)
const MEDIA: [(&str, &str, &str); 12] = [("mp4", "video", "video/mp4"), ("m4v", "video", "video/mp4"), ("webm", "video", "video/webm"), ("mov", "video", "video/quicktime"), ("png", "image", "image/png"), ("jpg", "image", "image/jpeg"), ("jpeg", "image", "image/jpeg"), ("gif", "image", "image/gif"), ("webp", "image", "image/webp"), ("mp3", "audio", "audio/mpeg"), ("wav", "audio", "audio/wav"), ("ogg", "audio", "audio/ogg")];
const SKIP: [&str; 5] = [".git", "node_modules", "target", ".antiloki", ".next"];
const MAX_FILES: usize = 60;
const MAX_BYTES: u64 = 400 * 1024 * 1024;

/// A page under a `video/` folder is the team's composition (an HTML video: scenes and timings drawn in a page): it shows as `page`. Served only inside a sandbox (no scripts reaching the app, no cookies, no storage).
pub fn kind_of(name: &str) -> Option<(&'static str, &'static str)> { let ext = name.rsplit_once('.')?.1.to_ascii_lowercase(); if ext == "html" { return (name.starts_with("video/") || name.contains("/video/")).then_some(("page", "text/html; charset=utf-8")); } MEDIA.iter().find(|m| m.0 == ext).map(|m| (m.1, m.2)) }
/// A path that stays inside its folder: only plain names, no `..`, no root. Pure.
pub fn safe_rel(rel: &str) -> Option<PathBuf> { let p = Path::new(rel); if rel.is_empty() || p.components().any(|c| !matches!(c, Component::Normal(_))) { return None; } Some(p.to_path_buf()) }
/// `bytes=a-b`, `bytes=a-` or `bytes=-n` against a file of `len` bytes: the first and last byte to send. Pure.
pub fn parse_range(h: &str, len: u64) -> Option<(u64, u64)> {
    let spec = h.trim().strip_prefix("bytes=")?; let (a, b) = spec.split_once('-')?; if len == 0 { return None; }
    let (start, end) = if a.is_empty() { let n: u64 = b.parse().ok()?; (len.saturating_sub(n), len - 1) } else { let s: u64 = a.parse().ok()?; (s, if b.is_empty() { len - 1 } else { b.parse::<u64>().ok()?.min(len - 1) }) };
    if start > end || start >= len { None } else { Some((start, end)) }
}
/// Where a run keeps the media its working folders held, so they outlive the folders (a folder is taken away once its work is in the team's branch, and a video is rarely committed). Pure.
pub fn kept_dir(root: &str, run: &str) -> PathBuf { crate::team_graph_run::run_dir(root, run).join("media") }
/// The file a media artifact names: `<name>.<format>` anywhere in the run's working folders (or already kept); failing that, the newest file of that format. Its place on disk, or None. Blocking.
pub fn find_named(root: &str, run: &str, name: &str, format: &str) -> Option<PathBuf> {
    let want = format!("{name}.{format}").to_ascii_lowercase(); let mut best: Option<(std::time::SystemTime, PathBuf)> = None;
    for (_, dir) in trees_of(root, run) { let mut v = vec![]; walk(&dir, &dir, 0, &mut v);
        for (rel, _) in v { let p = dir.join(&rel); let n = rel.file_name().map(|x| x.to_string_lossy().to_ascii_lowercase()).unwrap_or_default(); if n == want { return Some(p); } if n.ends_with(&format!(".{format}")) { if let Ok(t) = p.metadata().and_then(|m| m.modified()) { if best.as_ref().map_or(true, |b| t > b.0) { best = Some((t, p)); } } } } }
    best.map(|b| b.1)
}
/// Copy the media of a working folder into the run's own folder, before the folder goes. A file already kept is left; none above 200 MB, and no more than 400 MB in all. Blocking.
pub fn keep(root: &str, run: &str, tree: &Path) -> usize {
    let dest = kept_dir(root, run); let mut found = vec![]; walk(tree, tree, 0, &mut found); let mut have: u64 = 0; let mut n = 0;
    for (rel, bytes) in found { if bytes > 200 * 1024 * 1024 || have + bytes > MAX_BYTES { continue; } let to = dest.join(&rel); if to.exists() { continue; } if let Some(d) = to.parent() { if std::fs::create_dir_all(d).is_err() { continue; } } if std::fs::copy(tree.join(&rel), &to).is_ok() { have += bytes; n += 1; } }
    n
}
/// The trees one run worked in: `<thread tree>--<run number>-<unit>`, with their names. Blocking.
pub fn trees_of(root: &str, run: &str) -> Vec<(String, PathBuf)> {
    let mut out: Vec<(String, PathBuf)> = vec![]; let kept = kept_dir(root, run); if kept.is_dir() { out.push(("kept".to_string(), kept)); }
    let Some(st) = crate::team_graph_run::run_on_disk(root, run) else { return out }; let thread = st["thread"].as_str().unwrap_or("").to_string(); if thread.is_empty() { return out; }
    let num = run.rsplit('-').next().unwrap_or(run); let prefix = format!("{}--{}-", crate::thread_tree::slug(root, &thread).to_lowercase(), num.to_lowercase());
    out.extend(std::fs::read_dir(crate::thread_tree::trees_dir()).into_iter().flatten().flatten().filter_map(|e| { let n = e.file_name().to_string_lossy().to_string(); (n.to_lowercase().starts_with(&prefix) && e.path().is_dir()).then(|| (n, e.path())) })); out
}
pub(crate) fn walk(dir: &Path, base: &Path, depth: usize, out: &mut Vec<(PathBuf, u64)>) {
    if depth > 6 || out.len() >= MAX_FILES { return; }
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() { let p = e.path(); let n = e.file_name().to_string_lossy().to_string();
        if p.is_dir() { if !SKIP.contains(&n.as_str()) && !n.starts_with('.') { walk(&p, base, depth + 1, out); } } else if kind_of(&p.strip_prefix(base).unwrap_or(&p).to_string_lossy().replace('\\', "/")).is_some() { if let Ok(m) = e.metadata() { if m.len() > 0 { out.push((p.strip_prefix(base).unwrap_or(&p).to_path_buf(), m.len())); } } } }
}
/// The media of a run: where each is (`file`: the tree's name, a slash, the path in it), its name, what it is and its size. Newest names last. Blocking.
pub fn media_of(root: &str, run: &str) -> Vec<Value> {
    let mut out = vec![]; for (name, dir) in trees_of(root, run) { let mut v = vec![]; walk(&dir, &dir, 0, &mut v); v.sort();
        for (rel, bytes) in v { let rel_s = rel.to_string_lossy().replace('\\', "/"); let n = rel.file_name().map(|x| x.to_string_lossy().to_string()).unwrap_or_default(); let kind = kind_of(&rel_s).map(|k| k.0).unwrap_or("file"); out.push(json!({ "file": format!("{name}/{rel_s}"), "name": n, "path": rel_s, "kind": kind, "bytes": bytes })); } }
    // what the archive holds that is no longer on disk (a folder that was removed, a file that was put back)
    for a in crate::team_archive::media_list(root, run) { let dup = out.iter().any(|o: &Value| o["name"] == a["name"] && o["bytes"] == a["bytes"]); if !dup { out.push(a); } }
    out
}
/// The media of a run for its result page. The page does not carry the bytes (a video inside a page made it megabytes that were fetched again and again): it points at the serve's media route, through `__ANTILOKI_BASE__`, which the app puts the serve's own address in when it shows the page. Blocking.
pub fn media_for_page(root: &str, run: &str) -> Vec<crate::team_compose::MediaSrc> {
    let enc = |t: &str| -> String { t.bytes().map(|c| if c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_' | b'.' | b'~') { (c as char).to_string() } else { format!("%{c:02X}") }).collect() };
    let mut out = vec![]; let mut seen = std::collections::HashSet::new();
    for m in media_of(root, run) { let name = m["name"].as_str().unwrap_or("").to_string(); let kind = m["kind"].as_str().unwrap_or("file").to_string(); let bytes = m["bytes"].as_u64().unwrap_or(0); if !seen.insert((name.clone(), bytes)) { continue; }
        let uri = m["file"].as_str().map(|f| format!("__ANTILOKI_BASE__/api/v1/cockpit/team-graph/media?run={}&file={}", enc(run), enc(f))); out.push(crate::team_compose::MediaSrc { name, kind, bytes, uri }); }
    out
}
fn reply(code: StatusCode, msg: &str) -> Response { (code, msg.to_string()).into_response() }
/// Bytes in hand, whole or a range of them, as a response. Pure over its arguments.
fn from_bytes(data: &[u8], ctype: &str, range: Option<&str>) -> Response {
    let len = data.len() as u64; let (start, end, code) = match range.and_then(|r| parse_range(r, len)) { Some((a, b)) => (a, b, StatusCode::PARTIAL_CONTENT), None => (0, len.saturating_sub(1), StatusCode::OK) };
    let body: Vec<u8> = if len == 0 { vec![] } else { data[start as usize..=end as usize].to_vec() };
    let mut h = HeaderMap::new(); h.insert(header::CONTENT_TYPE, ctype.parse().unwrap()); h.insert(header::ACCEPT_RANGES, "bytes".parse().unwrap()); h.insert(header::CACHE_CONTROL, "no-store".parse().unwrap()); h.insert(header::CONTENT_LENGTH, body.len().to_string().parse().unwrap());
    if code == StatusCode::PARTIAL_CONTENT { h.insert(header::CONTENT_RANGE, format!("bytes {start}-{end}/{len}").parse().unwrap()); } if ctype.starts_with("text/html") { h.insert(header::CONTENT_SECURITY_POLICY, "sandbox allow-scripts".parse().unwrap()); } (code, h, Body::from(body)).into_response()
}
/// One media file of a run, whole or a range of it. Blocking.
pub fn serve(root: &str, run: &str, file: &str, range: Option<&str>) -> Response {
    // a file the archive holds (its folder is gone): `archive/<id>/<name>`
    if let Some(rest) = file.strip_prefix("archive/") { let Some((id, _)) = rest.split_once('/') else { return reply(StatusCode::BAD_REQUEST, "name the file") }; let Ok(id) = id.parse::<i64>() else { return reply(StatusCode::BAD_REQUEST, "no such file") };
        let Some((path, data)) = crate::team_archive::get(root, run, id) else { return reply(StatusCode::NOT_FOUND, "the archive has no such file") }; let Some((_, ctype)) = kind_of(&path) else { return reply(StatusCode::FORBIDDEN, "only videos, pictures and sounds are served") }; return from_bytes(&data, ctype, range); }
    let Some((tree, rel)) = file.split_once('/') else { return reply(StatusCode::BAD_REQUEST, "name the tree and the file") };
    let Some(rel) = safe_rel(rel) else { return reply(StatusCode::BAD_REQUEST, "that path is not allowed") };
    let Some((_, dir)) = trees_of(root, run).into_iter().find(|(n, _)| n == tree) else { return reply(StatusCode::NOT_FOUND, "that run has no such folder") };
    let name = rel.to_string_lossy().replace('\\', "/"); let Some((_, ctype)) = kind_of(&name) else { return reply(StatusCode::FORBIDDEN, "only videos, pictures and sounds are served") };
    let (Ok(canon_dir), Ok(canon)) = (dir.canonicalize(), dir.join(&rel).canonicalize()) else { return reply(StatusCode::NOT_FOUND, "no such file") }; if !canon.starts_with(&canon_dir) { return reply(StatusCode::FORBIDDEN, "that path is not allowed"); }
    let Ok(mut f) = std::fs::File::open(&canon) else { return reply(StatusCode::NOT_FOUND, "no such file") }; let len = f.metadata().map(|m| m.len()).unwrap_or(0); if len > MAX_BYTES { return reply(StatusCode::PAYLOAD_TOO_LARGE, "that file is too large to preview"); }
    let (start, end, code) = match range.and_then(|r| parse_range(r, len)) { Some((a, b)) => (a, b, StatusCode::PARTIAL_CONTENT), None => (0, len.saturating_sub(1), StatusCode::OK) };
    let want = if len == 0 { 0 } else { end - start + 1 }; let mut buf = Vec::with_capacity(want as usize); if f.seek(SeekFrom::Start(start)).is_err() || f.take(want).read_to_end(&mut buf).is_err() { return reply(StatusCode::INTERNAL_SERVER_ERROR, "could not read it"); }
    let mut h = HeaderMap::new(); h.insert(header::CONTENT_TYPE, ctype.parse().unwrap()); h.insert(header::ACCEPT_RANGES, "bytes".parse().unwrap()); h.insert(header::CACHE_CONTROL, "no-store".parse().unwrap()); h.insert(header::CONTENT_LENGTH, buf.len().to_string().parse().unwrap());
    if code == StatusCode::PARTIAL_CONTENT { h.insert(header::CONTENT_RANGE, format!("bytes {start}-{end}/{len}").parse().unwrap()); }
    if ctype.starts_with("text/html") { h.insert(header::CONTENT_SECURITY_POLICY, "sandbox allow-scripts".parse().unwrap()); }
    (code, h, Body::from(buf)).into_response()
}
pub async fn http(state: Arc<AppState>, q: std::collections::HashMap<String, String>, headers: HeaderMap) -> Response {
    let root = crate::chat_pane::workspace_root_of(&state); if root.is_empty() { return reply(StatusCode::BAD_REQUEST, "no project is open"); }
    let (run, file) = (q.get("run").cloned().unwrap_or_default(), q.get("file").cloned().unwrap_or_default());
    if run.is_empty() || !run.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') { return reply(StatusCode::BAD_REQUEST, "no such run"); }
    let range = headers.get(header::RANGE).and_then(|v| v.to_str().ok()).map(|s| s.to_string());
    tokio::task::spawn_blocking(move || serve(&root, &run, &file, range.as_deref())).await.unwrap_or_else(|_| reply(StatusCode::INTERNAL_SERVER_ERROR, "the read was cut off"))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_media_is_media_and_paths_stay_inside_the_folder() {
        assert_eq!(kind_of("out/Test.MP4"), Some(("video", "video/mp4"))); assert_eq!(kind_of("a.png").unwrap().0, "image"); assert!(kind_of("a.rs").is_none() && kind_of("noext").is_none() && kind_of("secrets.env").is_none());
        assert_eq!(kind_of("video/index.html").unwrap().0, "page"); assert_eq!(kind_of("kept/video/scene.html").unwrap().0, "page"); assert!(kind_of("index.html").is_none() && kind_of("docs/page.html").is_none(), "only a page under video/ is a composition");
        assert!(safe_rel("video/out/test.mp4").is_some()); assert!(safe_rel("../x.mp4").is_none() && safe_rel("/etc/passwd").is_none() && safe_rel("a/../../b.mp4").is_none() && safe_rel("").is_none());
    }
    #[test]
    fn the_media_of_a_folder_are_kept_with_the_run_before_the_folder_goes() {
        let tmp = std::env::temp_dir().join(format!("antiloki-media-{}", std::process::id())); let _ = std::fs::remove_dir_all(&tmp); let tree = tmp.join("tree"); std::fs::create_dir_all(tree.join("video/out")).unwrap(); std::fs::create_dir_all(tree.join("node_modules/x")).unwrap();
        std::fs::write(tree.join("video/out/test.mp4"), b"0123456789").unwrap(); std::fs::write(tree.join("notes.md"), b"x").unwrap(); std::fs::write(tree.join("node_modules/x/a.png"), b"x").unwrap(); let root = tmp.join("ws"); let root_s = root.to_string_lossy().to_string();
        assert_eq!(keep(&root_s, "r-1", &tree), 1, "the video only: not the notes, not what is inside node_modules"); assert_eq!(std::fs::read(kept_dir(&root_s, "r-1").join("video/out/test.mp4")).unwrap(), b"0123456789"); assert_eq!(keep(&root_s, "r-1", &tree), 0, "a file already kept is left"); std::fs::remove_dir_all(&tree).unwrap();
        let t = trees_of(&root_s, "r-1"); assert_eq!(t.len(), 1); assert_eq!(t[0].0, "kept"); let m = media_of(&root_s, "r-1"); assert_eq!(m[0]["file"], serde_json::json!("kept/video/out/test.mp4")); assert_eq!(m[0]["kind"], serde_json::json!("video"));
        let r = serve(&root_s, "r-1", "kept/video/out/test.mp4", Some("bytes=2-4")); assert_eq!(r.status(), StatusCode::PARTIAL_CONTENT); assert_eq!(serve(&root_s, "r-1", "kept/../../x.mp4", None).status(), StatusCode::BAD_REQUEST); let _ = std::fs::remove_dir_all(&tmp);
    }
    #[test]
    fn a_range_asks_for_the_bytes_a_player_wants() {
        assert_eq!(parse_range("bytes=0-99", 1000), Some((0, 99))); assert_eq!(parse_range("bytes=900-", 1000), Some((900, 999))); assert_eq!(parse_range("bytes=-100", 1000), Some((900, 999))); assert_eq!(parse_range("bytes=0-5000", 1000), Some((0, 999)));
        assert_eq!(parse_range("bytes=2000-", 1000), None); assert_eq!(parse_range("bytes=5-2", 1000), None); assert_eq!(parse_range("items=0-1", 1000), None); assert_eq!(parse_range("bytes=0-1", 0), None);
    }
}
