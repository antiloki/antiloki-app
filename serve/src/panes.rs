//! FREE PANES, THE FOLDER AND ITS RULES (RFC 0016, M0): a pane with no shape is a folder `.antiloki/panes/<id>/` (a manifest `pane.json`, a page `index.html` and a few files of its own). This module lists the folders, checks each one against the
//! limits and the manifest rules, gives it a hash, and keeps the person's approval of that hash (`.antiloki/panes/approved.json`). A pane edited by anyone has a new hash and is a draft again. Only the person approves: an agent may draft, never approve.
//! Pure over the files (tested with temporary folders); `http` is the door.
use axum::{body::Bytes, http::StatusCode, response::{IntoResponse, Response}, Json};
use base64::Engine as _;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub const SCHEMA: &str = "antiloki-pane/1";
pub const MAX_TOTAL: u64 = 2 * 1024 * 1024;
pub const MAX_FILE: u64 = 1024 * 1024;
pub const MAX_FILES: usize = 40;
/// The kinds of file a pane may hold. Anything else is refused (an executable, an archive, a font that is not woff2).
pub const EXTS: [&str; 11] = ["html", "css", "js", "json", "svg", "png", "jpg", "webp", "woff2", "txt", "md"];
const BINARY: [&str; 4] = ["png", "jpg", "webp", "woff2"];
/// Not part of what a pane IS: its own state is written at run time and never changes its hash.
const OUTSIDE_HASH: [&str; 1] = ["state.json"];

fn s(v: &Value) -> String { v.as_str().unwrap_or("").trim().to_string() }
/// A pane's name: lower case letters, digits and dashes (the same rule as a team's), so a request can never build a path that leaves the folder. Pure.
pub fn id_ok(id: &str) -> bool { !id.is_empty() && id.len() <= 60 && id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-') && !id.starts_with('-') && !id.ends_with('-') }
fn name_ok(n: &str) -> bool { !n.is_empty() && n.len() <= 80 && n.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.') }
pub fn panes_dir(root: &str) -> PathBuf { Path::new(root).join(".antiloki").join("panes") }
pub fn pane_dir(root: &str, id: &str) -> Result<PathBuf, String> { if !id_ok(id) { return Err("a pane's name is lower case letters, digits and dashes".into()); } Ok(panes_dir(root).join(id)) }
fn ext_of(path: &str) -> String { path.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default() }

/// Every file of a pane folder as (path, bytes), at most two levels deep, in a fixed order; refused when a limit or a rule is broken (too many files, too big, a kind not allowed, a link). Reads the disk.
pub fn read_folder(dir: &Path) -> Result<Vec<(String, Vec<u8>)>, String> {
    let mut out: Vec<(String, Vec<u8>)> = vec![]; let mut total = 0u64;
    fn walk(base: &Path, dir: &Path, depth: usize, out: &mut Vec<(String, Vec<u8>)>, total: &mut u64) -> Result<(), String> {
        let mut entries: Vec<_> = std::fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?.flatten().collect(); entries.sort_by_key(|e| e.file_name());
        for e in entries {
            let p = e.path(); let ft = e.file_type().map_err(|e| e.to_string())?; let rel = p.strip_prefix(base).map_err(|e| e.to_string())?.to_string_lossy().replace('\\', "/");
            if ft.is_symlink() { return Err(format!("`{rel}` is a link: a pane holds only its own files")); }
            if ft.is_dir() { if depth >= 1 { return Err(format!("`{rel}` is nested too deep (at most one folder inside the pane)")); } walk(base, &p, depth + 1, out, total)?; continue; }
            let name = e.file_name().to_string_lossy().to_string(); if name == "approved.json" { continue; }
            if !EXTS.contains(&ext_of(&name).as_str()) { return Err(format!("`{rel}` is not a kind of file a pane may hold ({})", EXTS.join(", "))); }
            let len = e.metadata().map_err(|e| e.to_string())?.len(); if len > MAX_FILE { return Err(format!("`{rel}` is {} KB: at most {} KB a file", len / 1024, MAX_FILE / 1024)); }
            // the pane's own memory (state.json, at most 256 KB) is not part of what the pane IS: it must never push a full pane over its limit
            if !OUTSIDE_HASH.contains(&rel.as_str()) { *total += len; } if *total > MAX_TOTAL { return Err(format!("the pane is over {} MB in all", MAX_TOTAL / 1024 / 1024)); }
            if out.len() >= MAX_FILES { return Err(format!("the pane has more than {MAX_FILES} files")); }
            let bytes = std::fs::read(&p).map_err(|e| format!("{rel}: {e}"))?; out.push((rel, bytes));
        }
        Ok(())
    }
    walk(dir, dir, 0, &mut out, &mut total)?; out.sort_by(|a, b| a.0.cmp(&b.0)); Ok(out)
}
fn canon(v: &Value) -> Value { match v { Value::Object(m) => { let mut k: Vec<_> = m.iter().collect(); k.sort_by(|a, b| a.0.cmp(b.0)); Value::Object(k.into_iter().map(|(a, b)| (a.clone(), canon(b))).collect()) } Value::Array(a) => Value::Array(a.iter().map(canon).collect()), o => o.clone() } }
/// The hash of a pane: SHA-256 over its files in path order (the manifest in canonical form without its own `hash` field; `state.json` left out), each as path, length and bytes, so no two different folders hash alike. Pure.
pub fn hash_of(files: &[(String, Vec<u8>)]) -> String {
    let mut h = Sha256::new();
    for (path, bytes) in files { if OUTSIDE_HASH.contains(&path.as_str()) { continue; }
        let body: Vec<u8> = if path == "pane.json" { match serde_json::from_slice::<Value>(bytes) { Ok(mut v) => { if let Some(m) = v.as_object_mut() { m.remove("hash"); } serde_json::to_string(&canon(&v)).unwrap_or_default().into_bytes() } Err(_) => bytes.clone() } } else { bytes.clone() };
        h.update(path.as_bytes()); h.update([0]); h.update((body.len() as u64).to_le_bytes()); h.update(&body); }
    format!("sha256:{}", h.finalize().iter().map(|b| format!("{b:02x}")).collect::<String>())
}

/// What is wrong with a pane's manifest and folder, in plain words (empty = fine). Pure over the parsed manifest and the files.
pub fn check_manifest(m: &Value, id: &str, files: &[(String, Vec<u8>)]) -> Vec<String> {
    let mut e = vec![];
    if s(&m["schema"]) != SCHEMA { e.push(format!("`schema` must be `{SCHEMA}`")); }
    if s(&m["id"]) != id { e.push(format!("`id` must be the folder's name `{id}`")); }
    let title = s(&m["title"]); if title.is_empty() || title.chars().count() > 80 { e.push("`title` is 1 to 80 characters".into()); }
    let ver = s(&m["version"]); if ver.split('.').count() != 3 || !ver.split('.').all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit())) { e.push("`version` is three numbers like 1.0.0".into()); }
    let entry = s(&m["entry"]); if entry.is_empty() { e.push("`entry` names the page (index.html)".into()); } else if ext_of(&entry) != "html" || !files.iter().any(|(p, _)| p == &entry) { e.push(format!("`entry` ({entry}) must be an .html file in the folder")); }
    if s(&m["notes"]).chars().count() > 600 { e.push("`notes` is at most 600 characters".into()); }
    if s(&m["made_by"]).chars().count() > 80 { e.push("`made_by` is at most 80 characters".into()); }
    match m["apps"].as_array() {
        None => e.push("`apps` is a list (it may be empty: a pane that talks to no app)".into()),
        Some(apps) => {
            if apps.len() > 8 { e.push("at most 8 apps".into()); }
            for (i, a) in apps.iter().enumerate() {
                let app = s(&a["app"]); if !name_ok(&app) { e.push(format!("apps[{i}].app is the installed app's id")); }
                for k in ["read", "write"] { match a[k].as_array() { None if a.get(k).is_none() => {} None => e.push(format!("apps[{i}].{k} is a list of tool names")), Some(l) => { if l.len() > 60 { e.push(format!("apps[{i}].{k} has more than 60 tools")); } for t in l { if !name_ok(&s(t)) { e.push(format!("apps[{i}].{k}: `{}` is not a tool name", s(t))); } } } } }
                let (r, w) = (a["read"].as_array().cloned().unwrap_or_default(), a["write"].as_array().cloned().unwrap_or_default());
                if let Some(t) = r.iter().find(|t| w.contains(t)) { e.push(format!("apps[{i}]: `{}` is listed as both read and write", s(t))); }
            }
        }
    }
    e
}

fn approved_file(root: &str) -> PathBuf { panes_dir(root).join("approved.json") }
pub fn load_approved(root: &str) -> Map<String, Value> { std::fs::read_to_string(approved_file(root)).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()).and_then(|v| v.as_object().cloned()).unwrap_or_default() }
/// Is this exact content what the person approved? Pure over the file's record.
pub fn is_approved(root: &str, id: &str, hash: &str) -> bool { load_approved(root).get(id).and_then(|v| v["hash"].as_str()) == Some(hash) }
fn save_approved(root: &str, m: &Map<String, Value>) -> Result<(), String> { let p = approved_file(root); if let Some(d) = p.parent() { std::fs::create_dir_all(d).map_err(|e| e.to_string())?; } std::fs::write(&p, serde_json::to_string_pretty(&Value::Object(m.clone())).unwrap_or_default()).map_err(|e| e.to_string()) }
/// The person's yes to what a pane is NOW: refused when the hash given is not the folder's current hash (it was edited after the person looked), or when the folder has problems.
pub fn approve(root: &str, id: &str, hash: &str, now_ms: i64) -> Result<Value, String> {
    let r = read(root, id)?; if r["problems"].as_array().map_or(false, |p| !p.is_empty()) { return Err("the pane has problems: fix them before approving".into()); }
    if r["hash"].as_str() != Some(hash) { return Err("the pane changed since it was shown: look at it again".into()); }
    let mut m = load_approved(root); m.insert(id.to_string(), json!({ "hash": hash, "approved_ms": now_ms })); save_approved(root, &m)?; Ok(json!({ "ok": true, "id": id, "hash": hash }))
}
pub fn revoke(root: &str, id: &str) -> Result<bool, String> { pane_dir(root, id)?; let mut m = load_approved(root); let had = m.remove(id).is_some(); save_approved(root, &m)?; Ok(had) }

/// One pane as the screen needs it: its manifest, its files (text as text, pictures and fonts as data addresses), its hash, whether the person approved exactly this, and what is wrong with it. Reads the disk.
pub fn read(root: &str, id: &str) -> Result<Value, String> {
    let dir = pane_dir(root, id)?; if !dir.is_dir() { return Err(format!("no pane called `{id}`")); }
    let files = match read_folder(&dir) { Ok(f) => f, Err(e) => return Ok(json!({ "ok": true, "id": id, "manifest": Value::Null, "files": {}, "assets": {}, "hash": "", "approved": false, "problems": [e] })) };
    let manifest_bytes = files.iter().find(|(p, _)| p == "pane.json").map(|(_, b)| b.clone());
    let (manifest, mut problems) = match manifest_bytes { None => (Value::Null, vec!["`pane.json` (the manifest) is missing".to_string()]), Some(b) => match serde_json::from_slice::<Value>(&b) { Ok(m) => { let p = check_manifest(&m, id, &files); (m, p) } Err(e) => (Value::Null, vec![format!("`pane.json` is not valid JSON: {e}")]) } };
    let hash = hash_of(&files); let approved = problems.is_empty() && is_approved(root, id, &hash);
    let mut text = Map::new(); let mut assets = Map::new();
    for (p, b) in &files { if p == "pane.json" || OUTSIDE_HASH.contains(&p.as_str()) { continue; } if BINARY.contains(&ext_of(p).as_str()) { let mime = match ext_of(p).as_str() { "png" => "image/png", "jpg" => "image/jpeg", "webp" => "image/webp", _ => "font/woff2" }; assets.insert(p.clone(), json!(format!("data:{mime};base64,{}", base64::engine::general_purpose::STANDARD.encode(b)))); } else { match String::from_utf8(b.clone()) { Ok(t) => { text.insert(p.clone(), json!(t)); } Err(_) => problems.push(format!("`{p}` is not valid text")) } } }
    Ok(json!({ "ok": true, "id": id, "manifest": manifest, "files": text, "assets": assets, "hash": hash, "approved": approved, "problems": problems }))
}
/// Every pane folder of the project, briefly (no file contents): enough for a menu and for a card.
/// A CHEAP "HAS THIS PANE CHANGED?" (operator 2026-11-25: an agent rewrote a pane that was open and it did not refresh): the number of files, the newest change time and the total size of the pane's folder,
/// without reading or hashing a file. `state.json` (the pane's own memory, written by the pane itself) is left out, so a pane saving its state does not look like a rewrite. A different stamp means: read the pane again.
pub fn stamp(root: &str, id: &str) -> Result<Value, String> {
    if !id_ok(id) { return Err("a pane id is lower case letters, digits and dashes".into()); }
    let dir = panes_dir(root).join(id); if !dir.is_dir() { return Err(format!("no pane `{id}`")); }
    let (mut n, mut newest, mut size) = (0u64, 0u128, 0u64); let mut stack = vec![dir]; let mut seen = 0;
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            seen += 1; if seen > 2000 { break; }
            let p = e.path(); let Ok(m) = e.metadata() else { continue };
            if m.is_dir() { stack.push(p); continue; }
            if p.file_name().map_or(false, |f| f == "state.json") { continue; }
            n += 1; size += m.len(); if let Ok(t) = m.modified().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))) { newest = newest.max(t.as_millis()); }
        }
    }
    Ok(json!({ "ok": true, "stamp": format!("{n}:{newest}:{size}") }))
}
pub fn list(root: &str) -> Value {
    let mut out = vec![]; let Ok(rd) = std::fs::read_dir(panes_dir(root)) else { return json!([]) };
    let mut names: Vec<String> = rd.flatten().filter(|e| e.path().is_dir()).filter_map(|e| e.file_name().to_str().map(String::from)).filter(|n| id_ok(n)).collect(); names.sort();
    for id in names { if let Ok(r) = read(root, &id) { let m = &r["manifest"]; out.push(json!({ "id": id, "title": if m["title"].is_string() { m["title"].clone() } else { json!(id) }, "version": m["version"], "notes": m["notes"], "made_by": m["made_by"], "icon": m["icon"], "apps": m["apps"], "hash": r["hash"], "approved": r["approved"], "problems": r["problems"] })); } }
    Value::Array(out)
}

/// May this pane read this tool of this app? The person approved exactly this folder (its hash), the manifest lists the app, and the tool is in that app's `read` list and not in `write`. The serve checks the risk class again at the door (`read_only`). Pure.
pub fn call_allowed(r: &Value, app: &str, tool: &str) -> Result<(), String> { listed(r, app, tool, false) }
/// May this pane WRITE this tool? The same checks against the `write` list (RFC 0016 M2); the click is checked at the door. Pure.
pub fn write_allowed(r: &Value, app: &str, tool: &str) -> Result<(), String> { listed(r, app, tool, true) }
fn listed(r: &Value, app: &str, tool: &str, writing: bool) -> Result<(), String> {
    if r["problems"].as_array().map_or(false, |p| !p.is_empty()) { return Err("the pane has problems: it cannot reach an app".into()); }
    if r["approved"] != json!(true) { return Err("the pane is a draft: the person has not approved it, so it cannot reach an app".into()); }
    let apps = r["manifest"]["apps"].as_array().cloned().unwrap_or_default();
    let Some(a) = apps.iter().find(|a| a["app"].as_str() == Some(app)) else { return Err(format!("the pane did not ask for `{app}` in its manifest")) };
    if writing { return if a["write"].as_array().map_or(false, |w| w.iter().any(|t| t.as_str() == Some(tool))) { Ok(()) } else { Err(format!("the pane did not ask to write `{tool}` of `{app}`")) }; }
    if a["write"].as_array().map_or(false, |w| w.iter().any(|t| t.as_str() == Some(tool))) { return Err(format!("`{tool}` is a write tool in the manifest: reading cannot use it")); }
    if !a["read"].as_array().map_or(false, |l| l.iter().any(|t| t.as_str() == Some(tool))) { return Err(format!("the pane did not ask to read `{tool}` of `{app}`")); }
    Ok(())
}
/// The answer is at most 2 MB (RFC 0016 §4.3): a bigger one is refused in words, never cut mid-way. Pure.
pub fn within_reply_limit(v: &Value) -> Result<(), String> { let n = serde_json::to_string(v).map(|t| t.len()).unwrap_or(0); if n > 2 * 1024 * 1024 { Err(format!("the app's answer is {} KB: a pane may take at most 2 MB", n / 1024)) } else { Ok(()) } }

/// A pane that has problems or is a draft cannot reach an app at all, read or write. Pure.
fn call_allowed_any(r: &Value) -> Result<(), String> {
    if r["problems"].as_array().map_or(false, |p| !p.is_empty()) { return Err("the pane has problems: it cannot reach an app".into()); }
    if r["approved"] != json!(true) { return Err("the pane is a draft: the person has not approved it, so it cannot reach an app".into()); } Ok(())
}
/// Every confirmed write of a pane is a line of `.antiloki/panes/trail.jsonl` (outside every pane's folder, so it never changes a hash): when, which pane, app, tool, whether it worked. The arguments are not kept.
fn saga_line(root: &str, id: &str, app: &str, tool: &str, ok: bool, why: &str) {
    use std::io::Write; let p = panes_dir(root).join("trail.jsonl"); if let Some(d) = p.parent() { let _ = std::fs::create_dir_all(d); }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(p) { let _ = writeln!(f, "{}", json!({ "t_ms": crate::cli::now_ms(), "pane": id, "app": app, "tool": tool, "kind": "write", "confirmed": true, "ok": ok, "error": why })); }
}

// ── M3: STATE, DRAFTS BY AN AGENT, EXPORT AND IMPORT (RFC 0016 §4.1, §4.6, §4.8) ─────────────────────────────────────────────────────────────────────────────────────────────
pub const MAX_STATE: usize = 256 * 1024;
/// The pane's own memory: `state.json`, at most 256 KB, JSON only. It is outside the hash, so keeping state never makes a pane a draft again. Reads the disk.
pub fn state_get(root: &str, id: &str) -> Result<Value, String> {
    let d = pane_dir(root, id)?; if !d.is_dir() { return Err(format!("no pane called `{id}`")); }
    match std::fs::read_to_string(d.join("state.json")) { Ok(t) => serde_json::from_str(&t).map_err(|e| format!("state.json is not JSON: {e}")), Err(_) => Ok(Value::Null) }
}
pub fn state_set(root: &str, id: &str, v: &Value) -> Result<(), String> {
    let d = pane_dir(root, id)?; if !d.is_dir() { return Err(format!("no pane called `{id}`")); }
    let t = serde_json::to_string(v).map_err(|e| e.to_string())?; if t.len() > MAX_STATE { return Err(format!("the state is {} KB: at most {} KB", t.len() / 1024, MAX_STATE / 1024)); }
    std::fs::write(d.join("state.json"), t).map_err(|e| e.to_string())
}
/// A file path inside a pane: at most one folder deep, each part a plain name, a kind that is allowed, and not one of the files the host owns. Pure.
fn path_ok(p: &str) -> Result<(), String> {
    let parts: Vec<&str> = p.split('/').collect(); if parts.is_empty() || parts.len() > 2 || !parts.iter().all(|x| name_ok(x) && !x.starts_with('.')) { return Err(format!("`{p}` is not a plain file name (at most one folder inside the pane)")); }
    if ["pane.json", "approved.json", "state.json"].contains(&p) { return Err(format!("`{p}` is the host's own file")); }
    if !EXTS.contains(&ext_of(p).as_str()) { return Err(format!("`{p}` is not a kind of file a pane may hold ({})", EXTS.join(", "))); } Ok(())
}
/// Write a pane's folder from `files` (path, bytes) and a manifest: checked FIRST (limits, names, the manifest rules), nothing touched when anything is wrong; an older version of the same folder is kept under `.archive/` (never deleted), its `state.json` stays, and the new manifest says which hash it came from. The result is a DRAFT: nothing here approves. Pure over the disk.
pub fn write_pane(root: &str, id: &str, manifest: &Value, files: Vec<(String, Vec<u8>)>, made_by: &str) -> Result<Value, String> {
    let dir = pane_dir(root, id)?;
    if files.len() + 1 > MAX_FILES { return Err(format!("more than {MAX_FILES} files")); }
    let mut total = 0u64; for (p, b) in &files { path_ok(p)?; if b.len() as u64 > MAX_FILE { return Err(format!("`{p}` is {} KB: at most {} KB a file", b.len() / 1024, MAX_FILE / 1024)); } total += b.len() as u64; }
    if !manifest.is_object() { return Err("the manifest is an object".into()); }
    let mut m = manifest.clone(); { let o = m.as_object_mut().unwrap(); o.remove("hash"); o.insert("schema".into(), json!(SCHEMA)); o.insert("id".into(), json!(id)); if s(&o.get("made_by").cloned().unwrap_or(Value::Null)).is_empty() { o.insert("made_by".into(), json!(made_by)); } if !o.contains_key("version") { o.insert("version".into(), json!("1.0.0")); } if !o.contains_key("entry") { o.insert("entry".into(), json!("index.html")); } if !o.contains_key("apps") { o.insert("apps".into(), json!([])); } }
    let was = if dir.is_dir() { read(root, id).ok().and_then(|r| r["hash"].as_str().map(String::from)) } else { None };
    if let Some(h) = &was { if !h.is_empty() { m["from"] = json!(h); } }
    let mut all = files.clone(); let mbytes = serde_json::to_vec_pretty(&m).map_err(|e| e.to_string())?; total += mbytes.len() as u64; all.push(("pane.json".into(), mbytes.clone()));
    if total > MAX_TOTAL { return Err(format!("the pane is over {} MB in all", MAX_TOTAL / 1024 / 1024)); }
    all.sort_by(|a, b| a.0.cmp(&b.0)); let problems = check_manifest(&m, id, &all); if !problems.is_empty() { return Err(format!("the pane was not written: {}", problems.join("; "))); }
    let mut kept = None; if dir.is_dir() {
        let arch = panes_dir(root).join(".archive").join(format!("{id}-{}", crate::cli::now_ms())); std::fs::create_dir_all(arch.parent().unwrap()).map_err(|e| e.to_string())?; std::fs::rename(&dir, &arch).map_err(|e| format!("the old version could not be kept: {e}"))?; kept = Some(arch);
    }
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    for (p, b) in &all { let f = dir.join(p); if let Some(d) = f.parent() { std::fs::create_dir_all(d).map_err(|e| e.to_string())?; } std::fs::write(&f, b).map_err(|e| format!("{p}: {e}"))?; }
    if let Some(a) = &kept { if let Ok(st) = std::fs::read(a.join("state.json")) { let _ = std::fs::write(dir.join("state.json"), st); } }
    let r = read(root, id)?; Ok(json!({ "ok": true, "id": id, "hash": r["hash"], "approved": r["approved"], "problems": r["problems"], "kept_old": kept.map(|k| k.to_string_lossy().to_string()) }))
}
/// The first outside address a page would load or open (`src=`, `href=`, CSS `url(` pointing at http, https or `//`), if any. Pure.
pub fn outside_address(text: &str) -> Option<String> {
    let low: String = text.to_ascii_lowercase();
    for key in ["src=", "src =", "href=", "href =", "url(", "@import "] { let mut at = 0; while let Some(i) = low[at..].find(key) { let from = at + i + key.len(); let rest = low[from..].trim_start_matches([' ', '\t', '\n', '\r', '"', '\'']);
        if ["http://", "https://", "//"].iter().any(|p| rest.starts_with(p)) { let start = low.len() - rest.len(); return Some(text[start..].chars().take_while(|c| !c.is_whitespace() && !"\"')>".contains(*c)).take(120).collect()); } at = from; } }
    None
}
/// A draft from an agent or a person's request: text files only (a page, styles, scripts, data).
pub fn save_draft(root: &str, id: &str, manifest: &Value, files: &Value, made_by: &str) -> Result<Value, String> {
    let o = files.as_object().ok_or("`files` is an object: {\"index.html\": \"<html>…\"}")?; let mut v = vec![];
    for (p, t) in o { let Some(t) = t.as_str() else { return Err(format!("`{p}` must be text")) }; if BINARY.contains(&ext_of(p).as_str()) { return Err(format!("`{p}` is a picture or font: a draft by tool holds text files only")); } v.push((p.clone(), t.as_bytes().to_vec())); }
    for (p, b) in &v { if ["html", "htm", "css", "svg"].contains(&ext_of(p).as_str()) { if let Some(u) = outside_address(&String::from_utf8_lossy(b)) { return Err(format!("the pane was not written: `{p}` points outside ({u}). A free pane has no network: put the file in the pane and name it by its own path")); } } }
    write_pane(root, id, manifest, v, made_by)
}
/// A pane as ONE file (RFC 0016 §4.8): its manifest, its files (pictures and fonts as base64), its hash. Never the approval, never the state.
pub fn export(root: &str, id: &str) -> Result<Value, String> {
    let dir = pane_dir(root, id)?; if !dir.is_dir() { return Err(format!("no pane called `{id}`")); }
    let files = read_folder(&dir)?; let hash = hash_of(&files); let mut text = Map::new(); let mut bin = Map::new();
    for (p, b) in &files { if p == "pane.json" || OUTSIDE_HASH.contains(&p.as_str()) { continue; } if BINARY.contains(&ext_of(p).as_str()) { bin.insert(p.clone(), json!(base64::engine::general_purpose::STANDARD.encode(b))); } else { text.insert(p.clone(), json!(String::from_utf8(b.clone()).map_err(|_| format!("`{p}` is not valid text"))?)); } }
    let m: Value = files.iter().find(|(p, _)| p == "pane.json").and_then(|(_, b)| serde_json::from_slice(b).ok()).ok_or("`pane.json` is missing or not JSON")?;
    Ok(json!({ "schema": SCHEMA, "kind": "export", "id": id, "manifest": m, "files": text, "binary": bin, "hash": hash }))
}
/// Bring an export in as a DRAFT: the hash must match what the file holds (it was not edited on the way), every limit and rule applies again, an existing pane is never overwritten (name it differently with `as_id`), and no approval comes with it. Pure over the disk.
pub fn import(root: &str, bundle: &Value, as_id: &str, made_by: &str) -> Result<Value, String> {
    if s(&bundle["schema"]) != SCHEMA || s(&bundle["kind"]) != "export" { return Err(format!("this is not a pane export (`schema` is `{SCHEMA}`, `kind` is `export`)")); }
    let id = if as_id.is_empty() { s(&bundle["id"]) } else { as_id.to_string() }; let dir = pane_dir(root, &id)?; if dir.is_dir() { return Err(format!("a pane called `{id}` is already here: import it under another name")); }
    let mut files: Vec<(String, Vec<u8>)> = vec![];
    for (p, t) in bundle["files"].as_object().ok_or("`files` is missing")? { files.push((p.clone(), t.as_str().ok_or_else(|| format!("`{p}` must be text"))?.as_bytes().to_vec())); }
    if let Some(bi) = bundle["binary"].as_object() { for (p, t) in bi { let b = base64::engine::general_purpose::STANDARD.decode(t.as_str().unwrap_or("")).map_err(|_| format!("`{p}` is not valid base64"))?; files.push((p.clone(), b)); } }
    // the file's own hash, over what it holds, with ITS id: it must match what it says it is
    let mut check = files.clone(); let mut om = bundle["manifest"].clone(); check.push(("pane.json".into(), serde_json::to_vec(&om).unwrap_or_default())); check.sort_by(|a, b| a.0.cmp(&b.0));
    if hash_of(&check) != s(&bundle["hash"]) { return Err("the file's fingerprint does not match what it holds: it was changed after it was exported".into()); }
    if let Some(o) = om.as_object_mut() { o.remove("from"); } let mut o = om; if o.is_object() { o["imported_from"] = bundle["hash"].clone(); }
    write_pane(root, &id, &o, files, made_by)
}

fn reply(v: Value) -> Response { (StatusCode::OK, Json(v)).into_response() }
/// `POST /api/v1/panes` `{ op: "list" | "read" | "approve" | "revoke", id, hash }`. Approving and revoking are the person's: a request that names an agent's pane (`pane_id` above 0) is refused.
pub async fn http(state: Arc<crate::AppState>, body: Bytes) -> Response {
    let b: Value = serde_json::from_slice(&body).unwrap_or(Value::Null); let root = crate::agents_http::active_root(&state).await;
    if root.is_empty() { return reply(json!({ "ok": false, "error": "no project is open" })); }
    let by_agent = b["pane_id"].as_i64().unwrap_or(0) > 0; let id = s(&b["id"]);
    // A FREE PANE READS AN APP (M1): the manifest and the approved hash are read again from the disk here, whatever the screen says, then the call goes through the same door as the console with `read_only`
    // WHAT KIND OF WRITE (M2): the host asks before it draws its confirmation card, so a destructive tool gets the second tick. Nothing is called.
    if s(&b["op"]) == "risk" {
        let (app, tool) = (s(&b["app"]), s(&b["tool"])); let (r2, id2) = (root.clone(), id.clone());
        let folder = match tokio::task::spawn_blocking(move || read(&r2, &id2)).await.unwrap_or_else(|_| Err("cut off".into())) { Ok(f) => f, Err(e) => return reply(json!({ "ok": false, "error": e })) };
        if let Err(e) = call_allowed_any(&folder).and_then(|_| write_allowed(&folder, &app, &tool)) { return reply(json!({ "ok": false, "error": e })); }
        let v = crate::webapps::call_for_pane(&json!({ "op": "tools", "app": app })).await; if v["ok"] != json!(true) { return reply(v); }
        return match v["tools"].as_array().and_then(|t| t.iter().find(|t| t["name"].as_str() == Some(tool.as_str()))) { Some(t) => reply(json!({ "ok": true, "risk": t["risk"], "title": t["title"], "description": t["description"] })), None => reply(json!({ "ok": false, "error": format!("the app has no tool `{tool}`") })) };
    }
    if s(&b["op"]) == "write" {
        if by_agent { return reply(json!({ "ok": false, "error": "a write goes only after the person's click: an agent cannot make a pane write" })); }
        let (app, tool) = (s(&b["app"]), s(&b["tool"])); let (r2, id2) = (root.clone(), id.clone());
        let folder = tokio::task::spawn_blocking(move || read(&r2, &id2)).await.unwrap_or_else(|_| Err("cut off".into()));
        let folder = match folder { Ok(f) => f, Err(e) => return reply(json!({ "ok": false, "error": e })) };
        if let Err(e) = call_allowed_any(&folder).and_then(|_| write_allowed(&folder, &app, &tool)) { return reply(json!({ "ok": false, "error": e })); }
        let v = crate::webapps::call_for_pane(&json!({ "op": "call", "app": app, "tool": tool, "args": if b["args"].is_object() { b["args"].clone() } else { json!({}) }, "pane": id, "pane_write": true, "confirm": b["confirm"].as_bool() == Some(true), "confirm2": b["confirm2"].as_bool() == Some(true) })).await;
        let (ok, why) = (v["ok"] == json!(true) && v["result"]["is_error"] != json!(true), v["error"].as_str().unwrap_or("").to_string());
        saga_line(&root, &id, &app, &tool, ok, &why);
        if let Err(e) = within_reply_limit(&v) { return reply(json!({ "ok": false, "error": e })); }
        return reply(v);
    }
    if s(&b["op"]) == "call" {
        let (app, tool) = (s(&b["app"]), s(&b["tool"])); let (r2, id2) = (root.clone(), id.clone());
        let folder = tokio::task::spawn_blocking(move || read(&r2, &id2)).await.unwrap_or_else(|_| Err("cut off".into()));
        let folder = match folder { Ok(f) => f, Err(e) => return reply(json!({ "ok": false, "error": e })) };
        if let Err(e) = call_allowed(&folder, &app, &tool) { return reply(json!({ "ok": false, "error": e })); }
        let v = crate::webapps::call_for_pane(&json!({ "op": "call", "app": app, "tool": tool, "args": if b["args"].is_object() { b["args"].clone() } else { json!({}) }, "read_only": true, "pane": id })).await;
        if let Err(e) = within_reply_limit(&v) { return reply(json!({ "ok": false, "error": e })); }
        return reply(v);
    }
    let r = tokio::task::spawn_blocking(move || -> Value {
        match s(&b["op"]).as_str() {
            "list" => json!({ "ok": true, "panes": list(&root) }),
            "stamp" => stamp(&root, &id).unwrap_or_else(|e| json!({ "ok": false, "error": e })),
            "state_get" => { match read(&root, &id) { Ok(f) if f["approved"] == json!(true) => state_get(&root, &id).map(|v| json!({ "ok": true, "state": v })).unwrap_or_else(|e| json!({ "ok": false, "error": e })), Ok(_) => json!({ "ok": false, "error": "the pane is a draft: it has no memory until the person approves it" }), Err(e) => json!({ "ok": false, "error": e }) } }
            "state_set" => { match read(&root, &id) { Ok(f) if f["approved"] == json!(true) => match state_set(&root, &id, &b["state"]) { Ok(()) => json!({ "ok": true }), Err(e) => json!({ "ok": false, "error": e }) }, Ok(_) => json!({ "ok": false, "error": "the pane is a draft: it has no memory until the person approves it" }), Err(e) => json!({ "ok": false, "error": e }) } }
            "write_draft" => save_draft(&root, &id, &b["manifest"], &b["files"], if by_agent { "agent" } else { "person" }).unwrap_or_else(|e| json!({ "ok": false, "error": e })),
            // "Install this pane" (M4): a team's bundle {id, manifest, files} lands as a DRAFT, made by a team, never approved
            "install" => { let bd = &b["bundle"]; let bid = if s(&bd["id"]).is_empty() { s(&bd["manifest"]["id"]) } else { s(&bd["id"]) }; save_draft(&root, &bid, &bd["manifest"], &bd["files"], if by_agent { "agent" } else { "team" }).unwrap_or_else(|e| json!({ "ok": false, "error": e })) }
            "export" => export(&root, &id).map(|v| json!({ "ok": true, "export": v })).unwrap_or_else(|e| json!({ "ok": false, "error": e })),
            "import" => import(&root, &b["export"], &s(&b["as_id"]), if by_agent { "agent" } else { "person" }).unwrap_or_else(|e| json!({ "ok": false, "error": e })),
            "read" => read(&root, &id).unwrap_or_else(|e| json!({ "ok": false, "error": e })),
            "approve" => { if by_agent { return json!({ "ok": false, "error": "only the person approves a pane" }); } approve(&root, &id, &s(&b["hash"]), crate::cli::now_ms() as i64).unwrap_or_else(|e| json!({ "ok": false, "error": e })) }
            "revoke" => { if by_agent { return json!({ "ok": false, "error": "only the person revokes a pane" }); } match revoke(&root, &id) { Ok(h) => json!({ "ok": true, "had": h }), Err(e) => json!({ "ok": false, "error": e }) } }
            other => json!({ "ok": false, "error": format!("op is one of: list, read, call, write, risk, state_get, state_set, write_draft, install, export, import, approve, revoke (not `{other}`)") }),
        }
    }).await.unwrap_or_else(|_| json!({ "ok": false, "error": "cut off" }));
    reply(r)
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_panes_stamp_changes_when_an_agent_rewrites_it_and_not_when_the_pane_saves_its_own_state() {
        let root = std::env::temp_dir().join(format!("antiloki-pane-stamp-{}", std::process::id())); let _ = std::fs::remove_dir_all(&root); let rs = root.to_string_lossy().to_string();
        let d = panes_dir(&rs).join("demo"); std::fs::create_dir_all(&d).unwrap(); std::fs::write(d.join("pane.json"), "{}").unwrap(); std::fs::write(d.join("index.html"), "<p>one</p>").unwrap();
        let a = stamp(&rs, "demo").unwrap(); assert_eq!(a["ok"], json!(true)); assert_eq!(stamp(&rs, "demo").unwrap(), a, "the same folder, the same stamp");
        std::fs::write(d.join("state.json"), "{\"x\":1}").unwrap(); assert_eq!(stamp(&rs, "demo").unwrap(), a, "the pane's own state is not a rewrite");
        std::fs::write(d.join("index.html"), "<p>two, longer</p>").unwrap(); assert_ne!(stamp(&rs, "demo").unwrap(), a, "a rewritten file changes it");
        std::fs::write(d.join("extra.css"), "p{}").unwrap(); let b = stamp(&rs, "demo").unwrap(); std::fs::remove_file(d.join("extra.css")).unwrap(); assert_ne!(stamp(&rs, "demo").unwrap(), b, "so does a file added or removed");
        assert!(stamp(&rs, "nope").is_err() && stamp(&rs, "../x").is_err());
        let _ = std::fs::remove_dir_all(&root);
    }

    use super::*;
    #[test] fn a_draft_that_points_outside_is_named() {
        for (t, u) in [("<script src='https://evil.example/x.js'></script>", "https://evil.example/x.js"), ("<a HREF = \"HTTP://a.b/\">x</a>", "HTTP://a.b/"), ("body{background:url( '//cdn.x/y.png')}", "//cdn.x/y.png"), ("@import \"https://f.example/a.css\";", "https://f.example/a.css")] { assert_eq!(outside_address(t).as_deref(), Some(u), "{t}"); }
        for t in ["<script src=\"app.js\"></script>", "<a href=\"#top\">see http://not.a.link</a>", "<svg xmlns=\"http://www.w3.org/2000/svg\"></svg>", "div{background:url(pic.png)}", ""] { assert_eq!(outside_address(t), None, "{t}"); }
    }
    fn tmp(tag: &str) -> String { let d = std::env::temp_dir().join(format!("antiloki-panes-{tag}-{}", std::process::id())); let _ = std::fs::remove_dir_all(&d); std::fs::create_dir_all(&d).unwrap(); d.to_string_lossy().to_string() }
    fn write(root: &str, id: &str, file: &str, text: &str) { let p = panes_dir(root).join(id).join(file); std::fs::create_dir_all(p.parent().unwrap()).unwrap(); std::fs::write(p, text).unwrap(); }
    const MANIFEST: &str = r#"{ "schema": "antiloki-pane/1", "id": "demo", "title": "Demo", "version": "1.0.0", "entry": "index.html", "apps": [ { "app": "linear", "read": ["list_issues"], "write": ["save_comment"] } ], "state": true, "notes": "shows issues", "made_by": "person" }"#;
    fn good(root: &str) { write(root, "demo", "pane.json", MANIFEST); write(root, "demo", "index.html", "<html><body>hi</body></html>"); }
    #[test]
    fn a_write_is_allowed_only_for_a_listed_write_tool_of_an_approved_pane() {
        let r = tmp("write"); good(&r); let h = read(&r, "demo").unwrap()["hash"].as_str().unwrap().to_string();
        assert!(call_allowed_any(&read(&r, "demo").unwrap()).unwrap_err().contains("draft"));
        approve(&r, "demo", &h, 1).unwrap(); let f = read(&r, "demo").unwrap();
        assert!(call_allowed_any(&f).is_ok() && write_allowed(&f, "linear", "save_comment").is_ok());
        assert!(write_allowed(&f, "linear", "list_issues").unwrap_err().contains("did not ask to write"), "a read tool is not writable");
        assert!(write_allowed(&f, "notion", "save_comment").unwrap_err().contains("did not ask for"));
        assert!(call_allowed(&f, "linear", "save_comment").unwrap_err().contains("write tool"), "a write tool cannot be read");
    }
    #[test]
    fn a_draft_written_by_a_tool_is_checked_first_never_approved_and_keeps_the_old_one() {
        let r = tmp("draft"); let m = serde_json::from_str::<Value>(MANIFEST).unwrap();
        for bad in ["../x.html", "a/b/c.html", "pane.json", "x.exe", ".hidden.html"] { let e = save_draft(&r, "demo", &m, &json!({ "index.html": "<p>", bad: "x" }), "agent"); assert!(e.is_err(), "{bad}"); }
        assert!(!panes_dir(&r).join("demo").exists(), "nothing is written when anything is wrong");
        let w = save_draft(&r, "demo", &m, &json!({ "index.html": "<p>one</p>" }), "agent").unwrap(); assert_eq!(w["approved"], json!(false)); assert_eq!(w["problems"], json!([]));
        let h1 = w["hash"].as_str().unwrap().to_string(); approve(&r, "demo", &h1, 1).unwrap(); state_set(&r, "demo", &json!({ "n": 1 })).unwrap();
        let w2 = save_draft(&r, "demo", &m, &json!({ "index.html": "<p>two</p>" }), "agent").unwrap();
        assert_eq!(w2["approved"], json!(false), "an edited pane is a draft again"); assert!(w2["kept_old"].as_str().unwrap().contains(".archive"));
        assert_eq!(read(&r, "demo").unwrap()["manifest"]["from"], json!(h1), "the lineage names the hash it came from");
        assert_eq!(state_get(&r, "demo").unwrap(), json!({ "n": 1 }), "its memory stays");
        assert!(list(&r).as_array().unwrap().len() == 1, "the archive is not a pane in the list");
    }
    #[test]
    fn the_panes_memory_does_not_count_against_the_panes_size() {
        let r = tmp("statesize"); good(&r); write(&r, "demo", "big.txt", &"x".repeat(1000 * 1024)); write(&r, "demo", "big2.txt", &"x".repeat(1000 * 1024));   // just under the 2 MB total
        assert_eq!(read(&r, "demo").unwrap()["problems"], json!([]));
        state_set(&r, "demo", &json!("y".repeat(200 * 1024))).unwrap(); assert_eq!(read(&r, "demo").unwrap()["problems"], json!([]), "a full pane with its memory still opens");
    }
    #[test]
    fn state_is_json_and_at_most_256_kb() {
        let r = tmp("state"); good(&r); assert_eq!(state_get(&r, "demo").unwrap(), Value::Null);
        state_set(&r, "demo", &json!({ "a": [1, 2] })).unwrap(); assert_eq!(state_get(&r, "demo").unwrap(), json!({ "a": [1, 2] }));
        assert!(state_set(&r, "demo", &json!("x".repeat(MAX_STATE))).unwrap_err().contains("at most 256 KB"));
        let h = read(&r, "demo").unwrap()["hash"].clone(); state_set(&r, "demo", &json!(7)).unwrap(); assert_eq!(read(&r, "demo").unwrap()["hash"], h, "state is outside the hash");
    }
    #[test]
    fn an_export_imports_as_a_draft_and_a_changed_file_is_refused() {
        let a = tmp("exp-a"); let b = tmp("exp-b"); good(&a); write(&a, "demo", "style.css", "p{}"); std::fs::write(panes_dir(&a).join("demo").join("dot.png"), [137u8, 80, 78, 71]).unwrap();
        let h = read(&a, "demo").unwrap()["hash"].as_str().unwrap().to_string(); approve(&a, "demo", &h, 1).unwrap();
        let x = export(&a, "demo").unwrap(); assert_eq!(x["hash"], json!(h)); assert!(x.get("approved").is_none() && x["binary"]["dot.png"].is_string());
        let i = import(&b, &x, "", "person").unwrap(); assert_eq!(i["approved"], json!(false), "approvals are never imported"); assert_eq!(i["problems"], json!([]));
        assert!(load_approved(&b).is_empty(), "approved.json is empty on the clean project");
        assert!(import(&b, &x, "", "person").unwrap_err().contains("already here"), "never overwritten");
        let mut bad = x.clone(); bad["files"]["index.html"] = json!("<script>steal()</script>"); assert!(import(&b, &bad, "other", "person").unwrap_err().contains("fingerprint"));
        let c = import(&b, &x, "copy", "person").unwrap(); assert_eq!(c["id"], json!("copy"));
        assert!(import(&b, &json!({ "schema": "x" }), "z", "p").is_err());
    }
    #[test]
    fn a_name_cannot_leave_the_folder() {
        for bad in ["", "../x", "a/b", "A", "a b", "-a", "a-", &"x".repeat(61)] { assert!(!id_ok(bad), "{bad}"); assert!(pane_dir("/p", bad).is_err()); }
        assert!(id_ok("my-issues-2")); assert!(pane_dir("/p", "demo").unwrap().ends_with(".antiloki/panes/demo"));
    }
    #[test]
    fn a_good_folder_reads_clean_and_the_hash_follows_the_content_and_not_the_state() {
        let r = tmp("hash"); good(&r); let a = read(&r, "demo").unwrap(); assert_eq!(a["problems"], json!([]), "{a}"); assert_eq!(a["approved"], json!(false)); assert!(a["hash"].as_str().unwrap().starts_with("sha256:"));
        write(&r, "demo", "state.json", "{\"n\":1}"); assert_eq!(read(&r, "demo").unwrap()["hash"], a["hash"], "state is not part of the pane");
        write(&r, "demo", "index.html", "<html><body>hi!</body></html>"); assert_ne!(read(&r, "demo").unwrap()["hash"], a["hash"], "one byte changes it");
        write(&r, "demo", "pane.json", &MANIFEST.replace("\"made_by\": \"person\"", "\"made_by\": \"person\", \"hash\": \"sha256:whatever\"")); write(&r, "demo", "index.html", "<html><body>hi</body></html>"); assert_eq!(read(&r, "demo").unwrap()["hash"], a["hash"], "the manifest's own hash field is left out");
        // the folder as the screen reads it: text as text
        assert!(a["files"]["index.html"].as_str().unwrap().contains("hi")); assert!(a["files"].get("pane.json").is_none());
    }
    #[test]
    fn the_limits_and_the_rules_are_kept() {
        let r = tmp("limits"); good(&r);
        write(&r, "demo", "tool.exe", "x"); assert!(read(&r, "demo").unwrap()["problems"][0].as_str().unwrap().contains("not a kind of file")); std::fs::remove_file(panes_dir(&r).join("demo/tool.exe")).unwrap();
        write(&r, "demo", "big.txt", &"x".repeat(MAX_FILE as usize + 1)); assert!(read(&r, "demo").unwrap()["problems"][0].as_str().unwrap().contains("at most")); std::fs::remove_file(panes_dir(&r).join("demo/big.txt")).unwrap();
        write(&r, "demo", "a/b/deep.txt", "x"); assert!(read(&r, "demo").unwrap()["problems"][0].as_str().unwrap().contains("nested too deep")); std::fs::remove_dir_all(panes_dir(&r).join("demo/a")).unwrap();
        for i in 0..MAX_FILES { write(&r, "demo", &format!("f{i}.txt"), "x"); } assert!(read(&r, "demo").unwrap()["problems"][0].as_str().unwrap().contains("more than")); 
        #[cfg(unix)] { for i in 0..MAX_FILES { let _ = std::fs::remove_file(panes_dir(&r).join(format!("demo/f{i}.txt"))); } std::os::unix::fs::symlink("/etc/passwd", panes_dir(&r).join("demo/link.txt")).unwrap(); assert!(read(&r, "demo").unwrap()["problems"][0].as_str().unwrap().contains("is a link")); }
    }
    #[test]
    fn the_manifest_must_say_what_it_is_and_which_tools_it_may_use() {
        let files = vec![("pane.json".to_string(), vec![]), ("index.html".to_string(), vec![])]; let m: Value = serde_json::from_str(MANIFEST).unwrap();
        assert!(check_manifest(&m, "demo", &files).is_empty()); assert!(check_manifest(&m, "other", &files).iter().any(|e| e.contains("folder's name")));
        for (k, v, want) in [("schema", json!("x"), "schema"), ("title", json!(""), "title"), ("version", json!("1.0"), "version"), ("entry", json!("missing.html"), "entry"), ("apps", json!("linear"), "apps")] { let mut b = m.clone(); b[k] = v; assert!(check_manifest(&b, "demo", &files).iter().any(|e| e.contains(want)), "{k}"); }
        let mut b = m.clone(); b["apps"][0]["write"] = json!(["list_issues"]); assert!(check_manifest(&b, "demo", &files).iter().any(|e| e.contains("both read and write")));
        let mut b = m.clone(); b["apps"][0]["read"] = json!(["bad name!"]); assert!(check_manifest(&b, "demo", &files).iter().any(|e| e.contains("not a tool name")));
    }
    #[test]
    fn approval_is_of_exactly_what_was_shown_and_an_edit_takes_it_back() {
        let r = tmp("approve"); good(&r); let h = read(&r, "demo").unwrap()["hash"].as_str().unwrap().to_string();
        assert!(approve(&r, "demo", "sha256:wrong", 1).unwrap_err().contains("changed since")); assert!(!read(&r, "demo").unwrap()["approved"].as_bool().unwrap());
        approve(&r, "demo", &h, 5).unwrap(); assert!(read(&r, "demo").unwrap()["approved"].as_bool().unwrap()); assert_eq!(load_approved(&r)["demo"]["approved_ms"], json!(5));
        write(&r, "demo", "index.html", "<html><body>changed</body></html>"); assert!(!read(&r, "demo").unwrap()["approved"].as_bool().unwrap(), "an edited pane is a draft again");
        write(&r, "demo", "pane.json", "not json"); assert!(approve(&r, "demo", &h, 6).unwrap_err().contains("problems"));
        assert!(revoke(&r, "demo").unwrap()); assert!(!revoke(&r, "demo").unwrap()); assert!(revoke(&r, "../x").is_err());
    }
    #[test]
    fn the_list_has_every_folder_with_its_state_and_its_problems() {
        let r = tmp("list"); good(&r); write(&r, "broken", "pane.json", "{"); write(&r, "Bad Name", "pane.json", "{}");
        let l = list(&r); let l = l.as_array().unwrap(); assert_eq!(l.len(), 2, "a folder with an unfit name is not a pane"); assert_eq!(l[0]["id"], json!("broken")); assert!(!l[0]["problems"].as_array().unwrap().is_empty()); assert_eq!(l[1]["id"], json!("demo")); assert_eq!(l[1]["title"], json!("Demo")); assert_eq!(l[1]["approved"], json!(false));
        assert_eq!(list(&tmp("none")), json!([]));
    }
    #[test]
    fn a_pane_reads_only_what_the_person_approved_and_only_what_it_asked_for() {
        let r = tmp("call"); good(&r); let folder = read(&r, "demo").unwrap();
        assert!(call_allowed(&folder, "linear", "list_issues").unwrap_err().contains("draft"), "an unapproved pane reaches nothing");
        approve(&r, "demo", folder["hash"].as_str().unwrap(), 1).unwrap(); let ok = read(&r, "demo").unwrap();
        assert!(call_allowed(&ok, "linear", "list_issues").is_ok());
        assert!(call_allowed(&ok, "notion", "list_issues").unwrap_err().contains("did not ask for `notion`"));
        assert!(call_allowed(&ok, "linear", "get_issue").unwrap_err().contains("did not ask to read"));
        // an edit after the approval is a new hash: a draft again
        write(&r, "demo", "index.html", "<html><body>changed</body></html>"); let again = read(&r, "demo").unwrap(); assert!(call_allowed(&again, "linear", "list_issues").unwrap_err().contains("draft"));
    }
    #[test]
    fn a_write_tool_in_the_manifest_is_never_a_read_and_a_huge_answer_is_refused() {
        let m = json!({ "problems": [], "approved": true, "manifest": { "apps": [ { "app": "linear", "read": ["list_issues"], "write": ["save_issue"] } ] } });
        assert!(call_allowed(&m, "linear", "save_issue").unwrap_err().contains("write tool")); assert!(call_allowed(&json!({ "problems": ["x"], "approved": true, "manifest": {} }), "a", "b").unwrap_err().contains("problems"));
        assert!(within_reply_limit(&json!({ "t": "x".repeat(100) })).is_ok()); assert!(within_reply_limit(&json!({ "t": "x".repeat(2 * 1024 * 1024 + 10) })).unwrap_err().contains("at most 2 MB"));
    }
}
