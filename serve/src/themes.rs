//! OMARCHY THEMES FOR ANTILOKI'S BACKGROUND (RFC 0007 §17.8). An Omarchy theme is data: a `colors.toml` palette, a `backgrounds/` folder and a `preview.png`, under
//! `$OMARCHY_PATH/themes` (the system's) and `~/.config/omarchy/themes` (the ones you installed with Omarchy). This lists them, serves a wallpaper or preview, and downloads one
//! from a git repository into ANTILOKI'S OWN store (`~/.local/share/antiloki/themes`), never into Omarchy's folder: a theme from a stranger's repository can carry scripts that
//! Omarchy would run when it applies the theme, so what antiloki downloads stays where only antiloki reads it, and only its palette and images are ever read. It never runs
//! `omarchy-theme-set`, so antiloki wearing a theme never changes the desktop. Applying a palette is the front end's (CSS variables); nothing here changes a setting.
//!
//!   GET  /api/v1/themes/omarchy                       { current, themes:[{id, name, source, mode, colors, backgrounds, preview}] }
//!   GET  /api/v1/themes/omarchy/file?id=&path=        a wallpaper or the preview, only from inside a listed theme, only an image
//!   POST /api/v1/themes/omarchy/install {url, replace?}  `git clone --depth 1` into antiloki's store, kept only if it has a colors.toml
use axum::{
    body::Bytes,
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};

const IMAGE_EXT: [&str; 4] = ["png", "jpg", "jpeg", "webp"];
/// Omarchy's logo wallpapers carry the product's name in the picture (`omarchy.png`, `omarchy.webp`, `2-omarchy.webp`, `3-omarchy-plants.webp`...), so no file whose name has it is offered: a FLAT one (a colour and the name,
/// a few kilobytes) is replaced by `antiloki.svg` drawn in the theme's own colours; one with art around the name is replaced by a repainted copy shipped with antiloki, where there is one (see `ART_FILES`).
const WORDMARK_FILE: &str = "antiloki.svg";
const FLAT_LOGO_MAX_BYTES: u64 = 16_384;
/// The repainted pictures, for pictures with the name drawn into them whatever the file is called: (theme, the original file, the name antiloki offers instead, its bytes). A theme that does not have the original gets nothing.
const ART_FILES: &[(&str, &str, &str, &[u8])] = &[
    ("rose-pine", "3-omarchy-plants.webp", "3-antiloki-plants.webp", include_bytes!("../assets/3-antiloki-plants.webp")),
    ("lumon", "02-opinions-equally.webp", "02-antiloki-opinions-equally.webp", include_bytes!("../assets/lumon-opinions-antiloki.webp")),
];
/// The repainted pictures a theme folder has the original of: (original, offered name, bytes).
fn art_of(theme: &Path) -> Vec<(&'static str, &'static str, &'static [u8])> {
    let id = theme.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    ART_FILES.iter().filter(|(t, orig, _, _)| *t == id && theme.join("backgrounds").join(orig).is_file()).map(|(_, o, n, b)| (*o, *n, *b)).collect()
}
fn is_logo_name(n: &str) -> bool { n.to_ascii_lowercase().contains("omarchy") }
fn is_image(n: &str) -> bool { Path::new(n).extension().map(|e| IMAGE_EXT.contains(&e.to_string_lossy().to_ascii_lowercase().as_str())).unwrap_or(false) }
/// The logo wallpapers of a theme's `backgrounds/` folder: (file name, size).
fn logo_files(dir: &Path) -> Vec<(String, u64)> {
    std::fs::read_dir(dir).into_iter().flatten().flatten().filter_map(|e| { let n = e.file_name().to_string_lossy().to_string(); (is_image(&n) && is_logo_name(&n) && e.path().is_file()).then(|| (n, e.metadata().map(|m| m.len()).unwrap_or(0))) }).collect()
}
fn has_flat_logo(dir: &Path) -> bool { logo_files(dir).iter().any(|(_, sz)| *sz <= FLAT_LOGO_MAX_BYTES) }

/// The person's home folder: `HOME` (Linux), else `USERPROFILE` (Windows, where there is no `HOME`: found 2026-11-25 on a friend's machine, where every theme path came out relative and nothing was found).
fn home() -> String { std::env::var("HOME").ok().filter(|h| !h.is_empty()).or_else(|| std::env::var("USERPROFILE").ok()).unwrap_or_default() }
pub fn user_dir() -> PathBuf { std::env::var("ANTILOKI_OMARCHY_USER_THEMES").ok().filter(|p| !p.is_empty()).map(PathBuf::from).unwrap_or_else(|| PathBuf::from(home()).join(".config/omarchy/themes")) }
/// Where antiloki keeps the themes it downloaded: its own, away from Omarchy's.
pub fn store_dir() -> PathBuf { std::env::var("ANTILOKI_THEMES_DIR").ok().filter(|p| !p.is_empty()).map(PathBuf::from).unwrap_or_else(|| PathBuf::from(home()).join(".local/share/antiloki/themes")) }
pub fn system_dir() -> PathBuf { PathBuf::from(std::env::var("OMARCHY_PATH").ok().filter(|p| !p.is_empty()).unwrap_or_else(|| "/usr/share/omarchy".into())).join("themes") }
fn state_dir() -> PathBuf { std::env::var("ANTILOKI_OMARCHY_STATE").ok().filter(|p| !p.is_empty()).map(PathBuf::from).unwrap_or_else(|| PathBuf::from(home()).join(".local/state/omarchy/current")) }

/// `tokyo-night` -> `Tokyo Night`, the way Omarchy's own list prints it.
pub fn display_name(slug: &str) -> String {
    slug.split('-').filter(|w| !w.is_empty()).map(|w| { let mut c = w.chars(); c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default() }).collect::<Vec<_>>().join(" ")
}

/// The palette of a `colors.toml`: only `key = "value"` lines with a plain key and a hex colour, plus `mode`. Anything else in the file is ignored. Pure.
pub fn parse_colors(text: &str) -> Map<String, Value> {
    let mut out = Map::new();
    for raw in text.lines() {
        let t = raw.trim();
        if t.starts_with('#') || t.starts_with('[') { continue; }
        let Some((k, v)) = t.split_once('=') else { continue };
        let k = k.trim(); let v = v.trim().trim_matches('"').trim_matches('\'');
        if k.is_empty() || !k.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_') { continue; }
        let hex = v.strip_prefix('#').map_or(false, |h| matches!(h.len(), 3 | 6 | 8) && h.chars().all(|c| c.is_ascii_hexdigit()));
        if hex || (k == "mode" && (v == "dark" || v == "light")) { out.insert(k.to_string(), json!(v)); }
    }
    out
}

fn images_in(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir).into_iter().flatten().flatten().filter_map(|e| {
        let n = e.file_name().to_string_lossy().to_string();
        let ext = Path::new(&n).extension()?.to_string_lossy().to_ascii_lowercase();
        // Omarchy ships its own logo as a wallpaper: the name is in the picture, so it is not offered as a background (see `logo_files`)
        (IMAGE_EXT.contains(&ext.as_str()) && !is_logo_name(&n) && e.path().is_file()).then_some(n)
    }).collect();
    v.sort(); v
}

/// The wallpapers a theme offers: its own pictures, and, where it ships Omarchy's logo wallpaper, `antiloki.svg` in its place (see `wordmark`).
fn backgrounds_of(theme: &Path) -> Vec<String> {
    let dir = theme.join("backgrounds");
    let art = art_of(theme);
    let mut v = images_in(&dir); v.retain(|n| !art.iter().any(|(o, _, _)| o == n));   // a picture with the name drawn in is never offered as it is
    for (_, shown, _) in &art { v.push(shown.to_string()); }
    v.sort();
    if has_flat_logo(&dir) { v.insert(0, WORDMARK_FILE.to_string()); }
    v
}

/// Every theme under `dirs` (earlier folders first: one you downloaded or installed hides a system one of the same name). A directory with no `colors.toml` is not a theme. Pure over the disk.
pub fn list_themes(dirs: &[(PathBuf, &'static str)]) -> Vec<Value> {
    let mut seen: HashMap<String, ()> = HashMap::new(); let mut out = vec![];
    for (dir, source) in dirs {
        let mut names: Vec<String> = std::fs::read_dir(dir).into_iter().flatten().flatten().filter(|e| e.path().is_dir()).map(|e| e.file_name().to_string_lossy().to_string()).collect();
        names.sort();
        for id in names {
            if id.starts_with('.') || seen.contains_key(&id) { continue; }
            let p = dir.join(&id);
            let Ok(text) = std::fs::read_to_string(p.join("colors.toml")) else { continue };
            let colors = parse_colors(&text);
            if colors.is_empty() { continue; }
            seen.insert(id.clone(), ());
            out.push(json!({ "id": id, "name": display_name(&id), "source": source, "mode": colors.get("mode").cloned().unwrap_or(json!("dark")), "colors": colors,
                "backgrounds": backgrounds_of(&p), "preview": p.join("preview.png").is_file() }));
        }
    }
    out.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
    out
}

/// A file of a theme the person may be shown: relative, no `..`, an image, and (after following links) still inside the theme. `None` otherwise. Pure over the disk.
pub fn safe_theme_file(theme_dir: &Path, rel: &str) -> Option<PathBuf> {
    let rel = Path::new(rel);
    if rel.is_absolute() || !rel.components().all(|c| matches!(c, Component::Normal(_))) { return None; }
    let ext = rel.extension()?.to_string_lossy().to_ascii_lowercase();
    if !IMAGE_EXT.contains(&ext.as_str()) { return None; }
    let root = std::fs::canonicalize(theme_dir).ok()?;
    let file = std::fs::canonicalize(theme_dir.join(rel)).ok()?;
    (file.starts_with(&root) && file.is_file()).then_some(file)
}

/// The directory name Omarchy would give a theme cloned from `url` (`omarchy-nord-theme.git` -> `nord`), or `None` if the URL is not one we clone. Only `https://host/path`
/// and `git@host:org/repo` are cloned: no `ext::`, `file:`, `ssh://`, no credentials in the URL, nothing that starts with a dash or holds a space. Pure.
pub fn slug_of_url(url: &str) -> Option<String> {
    let u = url.trim();
    if u.is_empty() || u.len() > 300 || u.starts_with('-') || u.chars().any(|c| c.is_whitespace() || c.is_control()) { return None; }
    // a real domain name: dots, and a last label with a letter in it (an IP address such as 127.0.0.1 or 192.168.1.5 is not cloned from: not the local network, on a web page's say-so)
    let host_ok = |h: &str| !h.is_empty() && h.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-') && h.contains('.') && !h.starts_with('.') && !h.ends_with('.') && h.rsplit('.').next().map_or(false, |t| t.chars().any(|c| c.is_ascii_alphabetic()));
    let path_ok = |p: &str| !p.is_empty() && !p.contains("..") && p.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '/' | '~'));
    let path = if let Some(rest) = u.strip_prefix("https://") {
        let (host, path) = rest.split_once('/')?; if !host_ok(host) || !path_ok(path) { return None; } path
    } else if let Some(rest) = u.strip_prefix("git@") {
        let (host, path) = rest.split_once(':')?; if !host_ok(host) || !path_ok(path) { return None; } path
    } else { return None };
    let base = path.trim_end_matches('/').rsplit('/').next()?.trim_end_matches(".git");
    let base = base.to_ascii_lowercase(); let base = base.strip_prefix("omarchy-").unwrap_or(&base); let base = base.strip_suffix("-theme").unwrap_or(base);
    let ok = !base.is_empty() && base.len() <= 60 && base.chars().next().map_or(false, |c| c.is_ascii_alphanumeric()) && base.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
    ok.then(|| base.to_string())
}

/// Install a theme into `themes_dir`: cloned somewhere private first, kept only when it has a palette, and put in place only then. `clone` does the fetching (injected so this
/// is testable without a network). An existing theme of that name is kept unless `replace`. Returns the theme's id.
pub fn install_with(url: &str, themes_dir: &Path, replace: bool, clone: impl Fn(&str, &Path) -> Result<(), String>) -> Result<String, (u16, String)> {
    let slug = slug_of_url(url).ok_or((400, "that is not a git address I can clone (use https://host/org/repo or git@host:org/repo)".to_string()))?;
    let target = themes_dir.join(&slug);
    if target.exists() && !replace { return Err((409, format!("a theme called {slug} is already installed"))); }
    std::fs::create_dir_all(themes_dir).map_err(|e| (500, e.to_string()))?;
    let tmp = themes_dir.join(format!(".antiloki-install-{}-{}", std::process::id(), crate::cli::now_ms()));
    let cleanup = |p: &Path| { let _ = std::fs::remove_dir_all(p); };
    if let Err(e) = clone(url, &tmp) { cleanup(&tmp); return Err((502, format!("could not fetch it: {e}"))); }
    let has_palette = std::fs::read_to_string(tmp.join("colors.toml")).map(|t| !parse_colors(&t).is_empty()).unwrap_or(false);
    if !has_palette { cleanup(&tmp); return Err((422, "that repository is not a theme (it has no colors.toml)".into())); }
    let _ = std::fs::remove_dir_all(tmp.join(".git"));
    if target.exists() { cleanup(&target); }
    std::fs::rename(&tmp, &target).map_err(|e| { cleanup(&tmp); (500, e.to_string()) })?;
    Ok(slug)
}

/// ANTILOKI'S OWN THEME, shipped inside the app (operator 2026-11-25: a friend's Windows machine had no wallpapers, because every other theme comes from an Omarchy desktop or a download).
/// Its colours and seven halftone etchings are written into antiloki's own store the first time the themes are listed, so every machine has them and a machine with no desktop theme wears this one.
pub const BUILTIN_ID: &str = "antiloki";
const BUILTIN: &[(&str, &[u8])] = &[
    ("colors.toml", include_bytes!("../assets/themes/antiloki/colors.toml")),
    ("preview.png", include_bytes!("../assets/themes/antiloki/preview.png")),
    ("backgrounds/1-antiloki.webp", include_bytes!("../assets/themes/antiloki/backgrounds/1-antiloki.webp")),
    ("backgrounds/2-antihunt.webp", include_bytes!("../assets/themes/antiloki/backgrounds/2-antihunt.webp")),
    ("backgrounds/3-antipost.webp", include_bytes!("../assets/themes/antiloki/backgrounds/3-antipost.webp")),
    ("backgrounds/4-antibot.webp", include_bytes!("../assets/themes/antiloki/backgrounds/4-antibot.webp")),
    ("backgrounds/5-antiqa.webp", include_bytes!("../assets/themes/antiloki/backgrounds/5-antiqa.webp")),
    ("backgrounds/6-antidrive.webp", include_bytes!("../assets/themes/antiloki/backgrounds/6-antidrive.webp")),
    ("backgrounds/7-antidub.webp", include_bytes!("../assets/themes/antiloki/backgrounds/7-antidub.webp")),
];
pub fn seed_builtin(store: &Path) {
    let dir = store.join(BUILTIN_ID); if dir.join("colors.toml").is_file() { return; }
    for (rel, bytes) in BUILTIN { let p = dir.join(rel); if let Some(d) = p.parent() { let _ = std::fs::create_dir_all(d); } let _ = std::fs::write(&p, bytes); }
}
pub fn all_dirs() -> Vec<(PathBuf, &'static str)> { seed_builtin(&store_dir()); vec![(store_dir(), "downloaded"), (user_dir(), "omarchy"), (system_dir(), "system")] }
fn reply(s: StatusCode, v: Value) -> Response { (s, Json(v)).into_response() }

/// The theme Omarchy itself is wearing right now (its own state file), for a "follow Omarchy" choice. `None` when it is not there.
pub fn current() -> Option<String> { current_in(&state_dir()).or_else(|| Some(BUILTIN_ID.to_string())) }   // no desktop theme (Windows, a plain Linux): antiloki's own
fn current_in(state: &Path) -> Option<String> { std::fs::read_to_string(state.join("theme.name")).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()) }

/// The themes on this machine as a short overview (id, name, mode, wallpapers), and the one the desktop is wearing. For the tools that read it; blocking.
pub fn overview() -> Value {
    let themes: Vec<Value> = list_themes(&all_dirs()).into_iter().map(|t| json!({ "id": t["id"], "name": t["name"], "mode": t["mode"], "source": t["source"], "wallpapers": t["backgrounds"] })).collect();
    json!({ "current": current(), "themes": themes })
}

/// `GET /api/v1/themes/omarchy`
pub async fn themes_get() -> Response {
    let v = tokio::task::spawn_blocking(|| { let mut themes = list_themes(&all_dirs()); themes.extend(crate::custom_themes::list().iter().map(crate::custom_themes::listing)); json!({ "ok": true, "current": current(), "themes": themes, "dir": store_dir() }) }).await.unwrap_or_else(|_| json!({ "ok": false }));
    reply(StatusCode::OK, v)
}

/// The antiloki wallpaper of a theme that ships Omarchy's logo wallpaper, in that theme's own colours. `None` for a theme that does not.
fn wordmark_of(theme: &Path) -> Option<String> {
    if !has_flat_logo(&theme.join("backgrounds")) { return None; }
    let colors = parse_colors(&std::fs::read_to_string(theme.join("colors.toml")).ok()?);
    let get = |k: &str| colors.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    Some(crate::wordmark::svg(&get("background"), &get("accent")))
}

/// `GET /api/v1/themes/omarchy/file?id=&path=`
pub async fn theme_file(q: HashMap<String, String>) -> Response {
    let (id, rel) = (q.get("id").cloned().unwrap_or_default(), q.get("path").cloned().unwrap_or_default());
    let found = tokio::task::spawn_blocking(move || {
        if let Some(found) = rel.strip_prefix("backgrounds/").and_then(|n| crate::custom_themes::picture(&id, n)) { return Some(found); }
        let dir = all_dirs().into_iter().map(|(d, _)| d.join(&id)).find(|d| !id.is_empty() && !id.contains('/') && !id.starts_with('.') && d.join("colors.toml").is_file())?;
        if rel == format!("backgrounds/{WORDMARK_FILE}") { return wordmark_of(&dir).map(|s| (s.into_bytes(), "svg".to_string())); }
        if let Some((_, _, bytes)) = art_of(&dir).into_iter().find(|(_, shown, _)| rel == format!("backgrounds/{shown}")) { return Some((bytes.to_vec(), "webp".to_string())); }
        let f = safe_theme_file(&dir, &rel)?; let ext = f.extension()?.to_string_lossy().to_ascii_lowercase();
        let bytes = std::fs::read(&f).ok()?; Some((bytes, ext))
    }).await.ok().flatten();
    match found {
        Some((bytes, ext)) => {
            let mime = match ext.as_str() { "png" => "image/png", "webp" => "image/webp", "svg" => "image/svg+xml", _ => "image/jpeg" };
            ([(header::CONTENT_TYPE, mime), (header::CACHE_CONTROL, "private, max-age=3600")], bytes).into_response()
        }
        None => reply(StatusCode::NOT_FOUND, json!({ "ok": false, "error": "no such theme file" })),
    }
}

/// `POST /api/v1/themes/omarchy/install {url, replace?}`
pub async fn theme_install(body: Bytes) -> Response {
    let b: Value = serde_json::from_slice(&body).unwrap_or(json!({}));
    let url = b["url"].as_str().unwrap_or("").to_string(); let replace = b["replace"] == true;
    if slug_of_url(&url).is_none() { return reply(StatusCode::BAD_REQUEST, json!({ "ok": false, "error": "that is not a git address I can clone (use https://host/org/repo or git@host:org/repo)" })); }
    let dir = store_dir();
    let r = tokio::task::spawn_blocking(move || install_with(&url, &dir, replace, git_clone)).await.unwrap_or(Err((500, "the install did not finish".into())));
    match r {
        Ok(id) => { let t = list_themes(&all_dirs()).into_iter().find(|t| t["id"] == id.as_str()); reply(StatusCode::OK, json!({ "ok": true, "id": id, "theme": t })) }
        Err((code, error)) => reply(StatusCode::from_u16(code).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR), json!({ "ok": false, "error": error })),
    }
}

/// `git clone --depth 1` with no prompts, no helpers, no exotic transports, and a time limit.
fn git_clone(url: &str, dest: &Path) -> Result<(), String> {
    let mut child = crate::proc::cmd("git")
        .args(["-c", "protocol.ext.allow=never", "-c", "protocol.file.allow=never", "-c", "credential.helper=", "clone", "--depth", "1", "--quiet", "--", url])
        .arg(dest).env("GIT_TERMINAL_PROMPT", "0").env("GIT_ASKPASS", "true").stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::piped()).spawn().map_err(|e| e.to_string())?;
    let t0 = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(st)) => { if st.success() { return Ok(()); } let mut e = String::new(); if let Some(mut s) = child.stderr.take() { use std::io::Read; let _ = s.read_to_string(&mut e); } return Err(e.trim().lines().last().unwrap_or("git failed").to_string()); }
            Ok(None) if t0.elapsed() > std::time::Duration::from_secs(120) => { let _ = child.kill(); return Err("it took longer than two minutes".into()); }
            Ok(None) => std::thread::sleep(std::time::Duration::from_millis(100)),
            Err(e) => return Err(e.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn antilokis_own_theme_is_written_once_listed_with_its_wallpapers_and_worn_when_there_is_no_desktop_theme() {
        let dir = std::env::temp_dir().join(format!("antiloki-builtin-theme-{}", std::process::id())); let _ = std::fs::remove_dir_all(&dir);
        seed_builtin(&dir); seed_builtin(&dir);   // twice: the second is a no-op
        let listed = list_themes(&[(dir.clone(), "downloaded")]);
        let t = listed.iter().find(|t| t["id"] == "antiloki").expect("the built-in theme is listed");
        assert_eq!(t["backgrounds"].as_array().map(|a| a.len()), Some(7), "{t}"); assert_eq!(t["mode"], "dark");
        assert!(t["colors"]["accent"].is_string(), "{t}"); assert!(dir.join("antiloki").join("preview.png").is_file());
        assert!(!BUILTIN.iter().any(|(n, _)| is_logo_name(n)), "no wallpaper carries Omarchy's name");
        // a machine with no desktop theme state wears antiloki's own
        assert_eq!(current_in(&dir.join("nothing-here")), None);
        std::fs::create_dir_all(dir.join("state")).unwrap(); std::fs::write(dir.join("state").join("theme.name"), "tokyo-night\n").unwrap(); assert_eq!(current_in(&dir.join("state")).as_deref(), Some("tokyo-night"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    use super::*;

    fn scratch(name: &str) -> PathBuf { let d = std::env::temp_dir().join(format!("antiloki-themes-{name}-{}", std::process::id())); let _ = std::fs::remove_dir_all(&d); std::fs::create_dir_all(&d).unwrap(); d }
    fn theme(dir: &Path, id: &str, colors: &str) { let p = dir.join(id); std::fs::create_dir_all(p.join("backgrounds")).unwrap(); std::fs::write(p.join("colors.toml"), colors).unwrap(); }

    #[test]
    fn a_palette_keeps_hex_colours_and_the_mode_and_nothing_else() {
        let c = parse_colors("mode = \"dark\"\n# a comment\naccent = \"#7aa2f7\"\nbackground='#1a1b26'\nname = \"not a colour\"\nbad = \"#12345\"\n[section]\nred = \"#f7768e\"\nBAD_KEY = \"#ffffff\"\nrun = \"rm -rf /\"\n");
        assert_eq!(c.get("accent"), Some(&json!("#7aa2f7"))); assert_eq!(c.get("background"), Some(&json!("#1a1b26"))); assert_eq!(c.get("mode"), Some(&json!("dark"))); assert_eq!(c.get("red"), Some(&json!("#f7768e")));
        assert!(c.get("name").is_none() && c.get("bad").is_none() && c.get("BAD_KEY").is_none() && c.get("run").is_none(), "{c:?}");
    }

    #[test]
    fn names_read_like_omarchys_own_list() { assert_eq!(display_name("tokyo-night"), "Tokyo Night"); assert_eq!(display_name("rose-pine"), "Rose Pine"); assert_eq!(display_name("nord"), "Nord"); assert_eq!(display_name("retro-82"), "Retro 82"); }

    #[test]
    fn themes_come_from_both_folders_a_user_theme_hides_a_system_one_and_a_folder_without_a_palette_is_not_a_theme() {
        let (sys, usr) = (scratch("sys"), scratch("usr"));
        theme(&sys, "nord", "mode = \"dark\"\naccent = \"#88c0d0\"\nbackground = \"#2e3440\"\n"); theme(&sys, "gruvbox", "accent = \"#d79921\"\n");
        theme(&usr, "nord", "accent = \"#ffffff\"\n"); theme(&usr, "mine", "mode = \"light\"\naccent = \"#112233\"\n");
        std::fs::create_dir_all(sys.join("empty")).unwrap(); std::fs::write(sys.join("nord/backgrounds/0-a.jpg"), b"x").unwrap(); std::fs::write(sys.join("nord/backgrounds/omarchy.png"), b"x").unwrap(); std::fs::write(sys.join("nord/backgrounds/notes.txt"), b"x").unwrap(); std::fs::write(sys.join("nord/preview.png"), b"x").unwrap();
        let l = list_themes(&[(usr.clone(), "user"), (sys.clone(), "system")]);
        assert_eq!(l.iter().map(|t| t["id"].as_str().unwrap()).collect::<Vec<_>>(), vec!["gruvbox", "mine", "nord"]);
        let nord = l.iter().find(|t| t["id"] == "nord").unwrap(); assert_eq!((nord["source"].clone(), nord["colors"]["accent"].clone()), (json!("user"), json!("#ffffff")), "the one you installed wins");
        let sysnord = list_themes(&[(sys.clone(), "system")]); assert_eq!(sysnord.iter().find(|t| t["id"] == "nord").unwrap()["backgrounds"], json!(["antiloki.svg", "0-a.jpg"]), "Omarchy\'s logo wallpaper is not offered; antiloki\'s own, drawn in the theme\'s colours, is");
        let mine = l.iter().find(|t| t["id"] == "mine").unwrap(); assert_eq!(mine["mode"], "light");
        assert_eq!(l.iter().find(|t| t["id"] == "gruvbox").unwrap()["mode"], "dark", "dark unless it says light");
    }

    #[test]
    fn a_theme_that_ships_omarchys_logo_gets_antilokis_wallpaper_in_its_own_colours_and_one_that_does_not_gets_none() {
        let d = scratch("wordmark");
        theme(&d, "with", "background = \"#101010\"\naccent = \"#abcdef\"\n"); std::fs::write(d.join("with/backgrounds/omarchy.png"), b"logo").unwrap(); std::fs::write(d.join("with/backgrounds/1-sea.jpg"), b"jpg").unwrap();
        theme(&d, "without", "background = \"#101010\"\naccent = \"#abcdef\"\n"); std::fs::write(d.join("without/backgrounds/1-sea.jpg"), b"jpg").unwrap();
        let l = list_themes(&[(d.clone(), "downloaded")]);
        let bg = |id: &str| l.iter().find(|t| t["id"] == id).unwrap()["backgrounds"].clone();
        assert_eq!(bg("with"), json!(["antiloki.svg", "1-sea.jpg"])); assert_eq!(bg("without"), json!(["1-sea.jpg"]));
        let svg = wordmark_of(&d.join("with")).unwrap(); assert!(svg.contains("#101010") && svg.contains("#abcdef") && !svg.to_lowercase().contains("omarchy"), "the theme\'s palette, and the other name nowhere");
        assert!(wordmark_of(&d.join("without")).is_none());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn every_logo_wallpaper_is_replaced_whatever_its_extension_or_number_and_none_is_offered_with_the_name_in_it() {
        let d = scratch("logos"); let c = "background = \"#202020\"\naccent = \"#88aacc\"\n";
        theme(&d, "webp", c); std::fs::write(d.join("webp/backgrounds/omarchy.webp"), b"flat").unwrap();
        theme(&d, "numbered", c); std::fs::write(d.join("numbered/backgrounds/2-omarchy.webp"), b"flat").unwrap(); std::fs::write(d.join("numbered/backgrounds/1-sea.jpg"), b"jpg").unwrap();
        theme(&d, "rose-pine", c); std::fs::write(d.join("rose-pine/backgrounds/3-omarchy-plants.webp"), vec![0u8; 40_000]).unwrap();
        theme(&d, "lumon", c); std::fs::write(d.join("lumon/backgrounds/02-opinions-equally.webp"), vec![0u8; 40_000]).unwrap(); std::fs::write(d.join("lumon/backgrounds/01-united.webp"), vec![0u8; 40_000]).unwrap();
        theme(&d, "unknownart", c); std::fs::write(d.join("unknownart/backgrounds/9-omarchy-sky.webp"), vec![0u8; 40_000]).unwrap();
        let l = list_themes(&[(d.clone(), "downloaded")]);
        let bg = |id: &str| l.iter().find(|t| t["id"] == id).unwrap()["backgrounds"].clone();
        assert_eq!(bg("webp"), json!(["antiloki.svg"]), "omarchy.webp (what Omarchy really ships) used to show nothing at all");
        assert_eq!(bg("numbered"), json!(["antiloki.svg", "1-sea.jpg"]));
        assert_eq!(bg("rose-pine"), json!(["3-antiloki-plants.webp"]), "the picture with art around the name is offered as the repainted copy");
        assert_eq!(bg("lumon"), json!(["01-united.webp", "02-antiloki-opinions-equally.webp"]), "a picture with the name drawn in under another file name is replaced too, the rest of the theme is untouched");
        assert_eq!(bg("unknownart"), json!([]), "an art picture we have no repainted copy of is hidden, not shown with the name in it");
        for id in ["webp", "numbered", "rose-pine", "lumon", "unknownart"] { assert!(!bg(id).to_string().to_lowercase().contains("omarchy"), "{id}"); }
        assert!(ART_FILES.iter().all(|(_, _, n, b)| !n.contains("omarchy") && b.len() > 1000));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_theme_file_is_an_image_inside_the_theme_and_nothing_else() {
        let d = scratch("file"); theme(&d, "t", "accent = \"#111111\"\n"); let t = d.join("t");
        std::fs::write(t.join("backgrounds/a.jpg"), b"jpg").unwrap(); std::fs::write(t.join("preview.png"), b"png").unwrap(); std::fs::write(t.join("colors.toml"), b"accent = \"#111111\"").unwrap(); std::fs::write(d.join("secret.png"), b"s").unwrap();
        assert!(safe_theme_file(&t, "backgrounds/a.jpg").is_some() && safe_theme_file(&t, "preview.png").is_some());
        for bad in ["../secret.png", "backgrounds/../../secret.png", "/etc/passwd", "colors.toml", "backgrounds/missing.jpg", "backgrounds", ""] { assert!(safe_theme_file(&t, bad).is_none(), "{bad}"); }
        #[cfg(unix)] { std::os::unix::fs::symlink(d.join("secret.png"), t.join("backgrounds/link.png")).unwrap(); assert!(safe_theme_file(&t, "backgrounds/link.png").is_none(), "a link out of the theme is not followed"); }
    }

    #[test]
    fn only_clean_https_and_git_at_addresses_are_cloned_and_named_like_omarchy_names_them() {
        assert_eq!(slug_of_url("https://github.com/example/omarchy-nord-theme.git").as_deref(), Some("nord"));
        assert_eq!(slug_of_url("https://github.com/example/Omarchy-Tokyo-Night-theme").as_deref(), Some("tokyo-night"));
        assert_eq!(slug_of_url("git@github.com:example/omarchy-example-theme.git").as_deref(), Some("example"));
        assert_eq!(slug_of_url("https://gitlab.com/a/b/c/solo/").as_deref(), Some("solo"));
        for bad in ["", "   ", "http://github.com/a/b", "file:///etc", "ext::sh -c id", "ssh://git@github.com/a/b", "-oProxyCommand=x", "https://user:pw@github.com/a/b", "https://github.com/a/b c", "https://github.com/../etc/x", "https://localhost/a/b", "https://127.0.0.1/a/b", "https://192.168.1.5/a/b", "https://[::1]/a/b", "git@github.com:../x", "https://github.com/", "https://github.com/a/.git", "javascript:alert(1)", "https://evil.com/a/b;rm"] { assert_eq!(slug_of_url(bad), None, "{bad:?}"); }
    }

    #[test]
    fn an_install_is_kept_only_with_a_palette_and_replaces_only_when_asked() {
        let d = scratch("install"); let ok = |_: &str, p: &Path| -> Result<(), String> { std::fs::create_dir_all(p.join(".git")).unwrap(); std::fs::write(p.join("colors.toml"), "accent = \"#abcdef\"\n").unwrap(); Ok(()) };
        let id = install_with("https://github.com/x/omarchy-zen-theme.git", &d, false, ok).unwrap(); assert_eq!(id, "zen");
        assert!(d.join("zen/colors.toml").is_file() && !d.join("zen/.git").exists(), "the history is not kept");
        assert_eq!(install_with("https://github.com/x/omarchy-zen-theme.git", &d, false, ok).unwrap_err().0, 409);
        assert!(install_with("https://github.com/x/omarchy-zen-theme.git", &d, true, ok).is_ok());
        let no_palette = |_: &str, p: &Path| -> Result<(), String> { std::fs::create_dir_all(p).unwrap(); std::fs::write(p.join("README.md"), "x").unwrap(); Ok(()) };
        assert_eq!(install_with("https://github.com/x/not-a-theme", &d, false, no_palette).unwrap_err().0, 422); assert!(!d.join("not-a-theme").exists());
        let boom = |_: &str, _: &Path| -> Result<(), String> { Err("repository not found".into()) };
        let e = install_with("https://github.com/x/missing", &d, false, boom).unwrap_err(); assert_eq!(e.0, 502); assert!(e.1.contains("repository not found"));
        assert_eq!(install_with("http://insecure.example/x/y", &d, false, ok).unwrap_err().0, 400);
        let leftovers: Vec<_> = std::fs::read_dir(&d).unwrap().flatten().filter(|e| e.file_name().to_string_lossy().starts_with(".antiloki-install")).collect(); assert!(leftovers.is_empty(), "no half-installed folder is left behind");
    }
}
