//! CUSTOM THEMES (the operator 2026-11-22: "themes, backgrounds and animations changeable via MCP, so I can ask for a new theme or a new animation on the topbars, loadings; stored in sqlite, rendered into YAML; the themes we have are base themes that cannot be deleted or changed; new ones appear flagged custom").
//! A custom theme is a ROW of the user's own database (`~/.local/share/antiloki/user/.antiloki/antiloki.db`, kind `theme`, through `docstore`); the YAML anyone reads is `team_yaml::to_yaml` of the row. The base themes (Omarchy's, the downloaded ones, the four looks) are never in that table, so they cannot be changed or deleted: a custom id that names one of them is refused.
//! A theme is DATA, never code: colours (hex), a look (pixel | shadow | liquid | minimal), a wallpaper (a gradient or a solid), and for each place that moves (topbar, loading, reasoning, avatar) one effect from a short list with colours, a speed and an intensity. Whatever is outside the list is refused with the list, so an agent can learn the vocabulary from the error.
use serde_json::{json, Map, Value};

pub const KIND: &str = "theme";
pub const LOOKS: [&str; 4] = ["pixel", "shadow", "liquid", "minimal"];
pub const SLOTS: [&str; 4] = ["topbar", "loading", "reasoning", "avatar"];
/// What an effect may be. `glow` turns slowly, `pulse` breathes in place, `shimmer` is a quick narrow sweep, `orbit` is a bright spot circling, `pixels` blocks, `liquid` drops, `dots` three dots, `bar` a sweeping line, `none` stills it.
pub const EFFECTS: [&str; 9] = ["glow", "pulse", "shimmer", "orbit", "pixels", "liquid", "dots", "bar", "none"];
const COLOR_KEYS: [&str; 6] = ["background", "foreground", "accent", "cursor", "selection_background", "selection_foreground"];

pub fn user_root() -> String { std::env::var("ANTILOKI_USER_DB_DIR").ok().filter(|p| !p.is_empty()).unwrap_or_else(|| format!("{}/.local/share/antiloki/user", std::env::var("HOME").unwrap_or_default())) }
fn is_hex(v: &str) -> bool { v.strip_prefix('#').map_or(false, |h| matches!(h.len(), 3 | 6 | 8) && h.chars().all(|c| c.is_ascii_hexdigit())) }
fn slug(id: &str) -> String { id.trim().to_lowercase().chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect::<String>().split('-').filter(|p| !p.is_empty()).collect::<Vec<_>>().join("-") }
/// The ids that are base themes: what is on this machine plus the names of the four looks. Blocking.
pub fn base_ids() -> Vec<String> { let mut v: Vec<String> = crate::themes::list_themes(&crate::themes::all_dirs()).iter().filter_map(|t| t["id"].as_str().map(str::to_string)).collect(); for l in ["pixelated", "pixel", "shadow", "liquid", "minimal", "default"] { v.push(l.into()); } v }

/// A theme as the agent gave it, checked and tidied. Every refusal says what is allowed. Pure.
pub fn lint(input: &Value) -> Result<Value, String> {
    let id = slug(input["id"].as_str().or(input["name"].as_str()).unwrap_or("")); if id.is_empty() || id.len() > 48 { return Err("a theme needs an `id` (letters, digits, dashes; up to 48)".into()); }
    let name = input["name"].as_str().map(str::trim).filter(|n| !n.is_empty()).map(str::to_string).unwrap_or_else(|| crate::themes::display_name(&id));
    let mode = match input["mode"].as_str().unwrap_or("dark") { m @ ("dark" | "light") => m, o => return Err(format!("`mode` is dark or light, not {o:?}")) };
    let mut colors = Map::new();
    if let Some(c) = input["colors"].as_object() { for (k, v) in c {
        let key_ok = COLOR_KEYS.contains(&k.as_str()) || (k.starts_with("color") && k[5..].parse::<u8>().map_or(false, |n| n < 16));
        if !key_ok { return Err(format!("unknown colour `{k}`: use {} or color0..color15", COLOR_KEYS.join(", "))); }
        let t = v.as_str().unwrap_or(""); if !is_hex(t) { return Err(format!("colour `{k}` must be a hex value like #1e1e2e, not {t:?}")); } colors.insert(k.clone(), json!(t)); } }
    for need in ["background", "foreground", "accent"] { if !colors.contains_key(need) { return Err(format!("a theme needs the colour `{need}` (hex)")); } }
    colors.insert("mode".into(), json!(mode));
    let look = match input["look"].as_str() { None | Some("") => "pixel", Some("pixelated") => "pixel", Some(l) if LOOKS.contains(&l) => l, Some(o) => return Err(format!("`look` is one of {}, not {o:?}", LOOKS.join(", "))) };
    let wallpaper = match &input["wallpaper"] { Value::Null => Value::Null, w => {
        match w["kind"].as_str() { Some("solid") => { let c = w["color"].as_str().unwrap_or(""); if !is_hex(c) { return Err("a solid wallpaper needs `color` (hex)".into()); } json!({ "kind": "solid", "color": c }) }
            Some("gradient") => { let stops: Vec<String> = w["stops"].as_array().into_iter().flatten().filter_map(|x| x.as_str().map(str::to_string)).collect(); if !(2..=4).contains(&stops.len()) || !stops.iter().all(|s| is_hex(s)) { return Err("a gradient wallpaper needs `stops`: 2 to 4 hex colours".into()); }
                let angle = w["angle"].as_i64().unwrap_or(135).clamp(0, 360); json!({ "kind": "gradient", "angle": angle, "stops": stops }) }
            // A PICTURE (operator 2026-11-26: "it reused the default wallpaper instead of creating or downloading a new one"): drawn by the agent (`svg`), downloaded (`url`) or a file of this machine (`path`)
            Some("image") => { let given: Vec<&str> = ["svg", "url", "path", "file"].into_iter().filter(|k| w[*k].as_str().is_some_and(|v| !v.trim().is_empty())).collect();
                if given.len() != 1 && !(w["file"].is_string() && given.contains(&"file")) { return Err("an image wallpaper needs exactly one of `svg` (art you draw), `url` (https) or `path` (an image file)".into()); }
                let mut o = json!({ "kind": "image" }); for k in ["svg", "url", "path", "file", "source"] { if w[k].is_string() { o[k] = w[k].clone(); } } o }
            _ => return Err("`wallpaper.kind` is `image` (with `svg`, `url` or `path`), `solid` (with `color`) or `gradient` (with `angle` and `stops`)".into()) } } };
    let mut anim = Map::new();
    if let Some(a) = input["animations"].as_object() { for (slot, spec) in a {
        if !SLOTS.contains(&slot.as_str()) { return Err(format!("unknown animation place `{slot}`: use {}", SLOTS.join(", "))); }
        let effect = spec["effect"].as_str().unwrap_or(""); if !EFFECTS.contains(&effect) { return Err(format!("`{slot}.effect` is one of {}, not {effect:?}", EFFECTS.join(", "))); }
        let cs: Vec<String> = spec["colors"].as_array().into_iter().flatten().filter_map(|x| x.as_str().map(str::to_string)).collect(); if cs.len() > 5 || !cs.iter().all(|c| is_hex(c)) { return Err(format!("`{slot}.colors` is up to 5 hex colours")); }
        let speed = spec["speed_s"].as_f64().unwrap_or(3.2).clamp(0.3, 30.0); let intensity = spec["intensity"].as_f64().unwrap_or(0.6).clamp(0.0, 1.0);
        anim.insert(slot.clone(), json!({ "effect": effect, "colors": cs, "speed_s": speed, "intensity": intensity })); } }
    Ok(json!({ "schema": "antiloki-theme/1", "id": id, "name": name, "mode": mode, "colors": colors, "look": look, "wallpaper": wallpaper, "animations": anim }))
}
/// A stored theme as the screen lists it: shaped like an Omarchy theme, flagged `custom`. Pure.
pub fn listing(t: &Value) -> Value {
    let wp = &t["wallpaper"]; let css = match wp["kind"].as_str() { Some("solid") => wp["color"].as_str().map(str::to_string), Some("gradient") => Some(format!("linear-gradient({}deg, {})", wp["angle"].as_i64().unwrap_or(135), wp["stops"].as_array().into_iter().flatten().filter_map(Value::as_str).collect::<Vec<_>>().join(", "))), _ => None };
    let pictures: Vec<&str> = wp["file"].as_str().filter(|_| wp["kind"] == json!("image")).into_iter().collect();
    json!({ "id": t["id"], "name": t["name"], "source": "custom", "custom": true, "mode": t["mode"], "colors": t["colors"], "backgrounds": pictures, "preview": false, "look": t["look"], "animations": t["animations"], "wallpaper_css": css })
}
/// Save (create or replace) a CUSTOM theme. A base id is refused. Blocking.
pub fn save(input: &Value) -> Result<Value, String> {
    let t = lint(input)?; let id = t["id"].as_str().unwrap_or("").to_string();
    if base_ids().iter().any(|b| *b == id) { return Err(format!("`{id}` is a base theme: base themes cannot be changed or replaced. Pick another id.")); }
    let mut t = t;
    if t["wallpaper"]["kind"] == json!("image") { t["wallpaper"] = keep_picture(&id, &t["wallpaper"])?; }
    crate::docstore::put(&user_root(), KIND, &id, &t)?; Ok(t)
}
const MAX_PICTURE: usize = 12 * 1024 * 1024;
fn pictures_dir(id: &str) -> std::path::PathBuf { std::path::PathBuf::from(user_root()).join("theme-wallpapers").join(id) }
/// What kind of picture these bytes are, by what they start with. Pure.
pub fn picture_ext(b: &[u8]) -> Option<&'static str> {
    if b.starts_with(&[0x89, b'P', b'N', b'G']) { return Some("png"); } if b.starts_with(&[0xff, 0xd8, 0xff]) { return Some("jpg"); } if b.len() > 12 && &b[0..4] == b"RIFF" && &b[8..12] == b"WEBP" { return Some("webp"); }
    let head = String::from_utf8_lossy(&b[..b.len().min(600)]).to_ascii_lowercase(); (head.trim_start().starts_with("<svg") || (head.trim_start().starts_with("<?xml") && head.contains("<svg"))).then_some("svg")
}
/// A drawn picture is only a picture: no script, no handler, nothing fetched from elsewhere. Pure.
pub fn safe_svg(svg: &str) -> Result<(), String> {
    let l = svg.to_ascii_lowercase();
    let handler = regex::Regex::new(r"\son[a-z]+\s*=").map_err(|e| e.to_string())?.is_match(&l);
    let outside = regex::Regex::new(r#"(href|src)\s*=\s*["']\s*(https?:|//|file:|data:text)|url\(\s*["']?\s*(https?:|//|file:)|@import"#).map_err(|e| e.to_string())?.is_match(&l);
    if l.contains("<script") || l.contains("<foreignobject") || l.contains("javascript:") || handler || outside { return Err("the svg may hold shapes, gradients, filters and text only: no script, no event handler, no outside address".into()); }
    Ok(())
}
/// Bring the picture into antiloki's own folder and say what is kept. Blocking (a download takes the network).
fn keep_picture(id: &str, w: &Value) -> Result<Value, String> {
    let (bytes, source): (Vec<u8>, String) = if let Some(svg) = w["svg"].as_str() { (svg.as_bytes().to_vec(), "drawn".into()) }
        else if let Some(url) = w["url"].as_str() {
            if !url.starts_with("https://") { return Err("a wallpaper is downloaded over https".into()); }
            let r = ureq::get(url).set("User-Agent", "antiloki").timeout(std::time::Duration::from_secs(40)).call().map_err(|e| format!("could not download the picture: {e}"))?;
            use std::io::Read; let mut b = vec![]; r.into_reader().take(MAX_PICTURE as u64 + 1).read_to_end(&mut b).map_err(|e| e.to_string())?; (b, url.to_string())
        } else if let Some(path) = w["path"].as_str() { let p = std::path::Path::new(path); if std::fs::metadata(p).map_err(|e| format!("{path}: {e}"))?.len() as usize > MAX_PICTURE { return Err("a wallpaper is at most 12 MB".into()); } (std::fs::read(p).map_err(|e| e.to_string())?, path.to_string()) }
        else { let f = w["file"].as_str().unwrap_or(""); if !f.contains('/') && pictures_dir(id).join(f).is_file() { return Ok(w.clone()); } return Err("that wallpaper file is not here: give `svg`, `url` or `path`".into()); };
    if bytes.len() > MAX_PICTURE { return Err("a wallpaper is at most 12 MB".into()); }
    let ext = picture_ext(&bytes).ok_or("that is not a png, jpg, webp or svg picture (a web page's address gives a page, not its picture)")?;
    if ext == "svg" { safe_svg(&String::from_utf8_lossy(&bytes))?; }
    let dir = pictures_dir(id); let _ = std::fs::remove_dir_all(&dir); std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    // a new name each time, so a screen does not keep showing the old picture
    let file = format!("wallpaper-{}.{ext}", crate::cli::now_ms()); std::fs::write(dir.join(&file), bytes).map_err(|e| e.to_string())?;
    Ok(json!({ "kind": "image", "file": file, "source": source }))
}
/// The bytes of a custom theme's picture, for the screen. Blocking.
pub fn picture(id: &str, name: &str) -> Option<(Vec<u8>, String)> {
    let t = get(id)?; if t["wallpaper"]["file"].as_str() != Some(name) || name.contains('/') { return None; }
    let ext = name.rsplit('.').next()?.to_string(); Some((std::fs::read(pictures_dir(id).join(name)).ok()?, ext))
}
pub fn get(id: &str) -> Option<Value> { crate::docstore::get(&user_root(), KIND, id) }
pub fn list() -> Vec<Value> { crate::docstore::ids(&user_root(), KIND).into_iter().filter_map(|i| get(&i)).collect() }
/// Delete a CUSTOM theme; a base theme (or one that is not there) is refused. Blocking.
pub fn remove(id: &str) -> Result<(), String> {
    if base_ids().iter().any(|b| b == id) { return Err(format!("`{id}` is a base theme: base themes cannot be deleted")); }
    if crate::docstore::delete(&user_root(), KIND, id) { Ok(()) } else { Err(format!("no custom theme `{id}`")) }
}
/// The tool's answer for one call (op: list | get | create | save | delete | vocabulary). Blocking.
pub fn tool(args: &Value) -> Result<String, String> {
    match args["op"].as_str().unwrap_or("list") {
        "vocabulary" => Ok(format!("a custom theme: id, name, mode (dark|light), colors {{background, foreground, accent (hex, required), cursor, selection_background, selection_foreground, color0..color15}}, look ({}), wallpaper {{kind: image, svg (art you draw yourself, a whole 1920x1080 scene) | url (https, a picture you may use) | path}} or {{kind: solid, color}} or {{kind: gradient, angle, stops[2..4]}}, animations {{ {} : {{effect ({}), colors[<=5 hex], speed_s 0.3..30, intensity 0..1}} }}. Base themes cannot be changed.", LOOKS.join("|"), SLOTS.join("|"), EFFECTS.join("|"))),
        "get" => { let id = args["id"].as_str().unwrap_or(""); get(id).map(|t| crate::docstore::render(&t)).ok_or_else(|| format!("no custom theme `{id}`")) }
        "create" | "save" => { let t = save(&args["theme"].as_object().map(|_| args["theme"].clone()).unwrap_or_else(|| args.clone()))?; Ok(format!("saved the custom theme `{}` (it shows on the themes menu, flagged custom):\n{}", t["id"].as_str().unwrap_or(""), crate::docstore::render(&t))) }
        "delete" => { let id = args["id"].as_str().unwrap_or(""); remove(id)?; Ok(format!("deleted the custom theme `{id}`")) }
        "list" => { let l = list(); Ok(if l.is_empty() { "no custom themes yet. Create one with op `create` (op `vocabulary` lists what a theme may hold).".into() } else { l.iter().map(|t| format!("{} ({}, look {}) custom", t["id"].as_str().unwrap_or(""), t["mode"].as_str().unwrap_or(""), t["look"].as_str().unwrap_or(""))).collect::<Vec<_>>().join("\n") }) }
        o => Err(format!("unknown op `{o}`: list, get, create, save, delete, vocabulary")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn good() -> Value { json!({ "id": "Midnight Rose", "mode": "dark", "colors": { "background": "#101018", "foreground": "#e8e6f0", "accent": "#ea3d8c" }, "look": "shadow", "wallpaper": { "kind": "gradient", "angle": 120, "stops": ["#101018", "#2a1030"] }, "animations": { "topbar": { "effect": "glow", "colors": ["#ea3d8c", "#7c5cf0"], "speed_s": 5 }, "loading": { "effect": "orbit" } } }) }
    #[test] fn a_picture_wallpaper_is_checked_as_a_picture() {
        assert_eq!(picture_ext(b"\x89PNG\r\n"), Some("png")); assert_eq!(picture_ext(b"<svg xmlns='x'></svg>"), Some("svg")); assert_eq!(picture_ext(b"<!doctype html><svg>"), None);
        assert!(safe_svg("<svg><defs><linearGradient id='g'/></defs><rect fill='url(#g)'/></svg>").is_ok());
        for bad in ["<svg><script>x</script></svg>", "<svg onload='x()'></svg>", "<svg><image href='https://x/y.png'/></svg>", "<svg><style>@import 'x';</style></svg>", "<svg><a href='javascript:1'/></svg>"] { assert!(safe_svg(bad).is_err(), "{bad}"); }
        assert!(lint(&json!({ "id": "p", "colors": { "background": "#000", "foreground": "#fff", "accent": "#f00" }, "wallpaper": { "kind": "image", "svg": "<svg/>", "url": "https://x" } })).is_err());
        assert_eq!(listing(&json!({ "id": "p", "wallpaper": { "kind": "image", "file": "wallpaper-1.svg" } }))["backgrounds"], json!(["wallpaper-1.svg"]));
    }
    #[test] fn a_theme_is_tidied_and_the_vocabulary_is_enforced_with_its_list() {
        let t = lint(&good()).unwrap(); assert_eq!(t["id"], json!("midnight-rose")); assert_eq!(t["name"], json!("Midnight Rose")); assert_eq!(t["look"], json!("shadow")); assert_eq!(t["animations"]["loading"]["speed_s"], json!(3.2)); assert_eq!(t["colors"]["mode"], json!("dark"));
        let mut bad = good(); bad["animations"]["topbar"]["effect"] = json!("explode"); let e = lint(&bad).unwrap_err(); assert!(e.contains("glow") && e.contains("liquid"), "{e}");
        let mut bad = good(); bad["colors"]["accent"] = json!("pink"); assert!(lint(&bad).unwrap_err().contains("hex"));
        let mut bad = good(); bad["animations"]["footer"] = json!({ "effect": "glow" }); assert!(lint(&bad).unwrap_err().contains("topbar"));
        let mut bad = good(); bad["colors"].as_object_mut().unwrap().remove("accent"); assert!(lint(&bad).unwrap_err().contains("accent"));
    }
    #[test] fn a_custom_theme_lists_like_a_base_one_flagged_custom_with_its_wallpaper_as_css() {
        let l = listing(&lint(&good()).unwrap()); assert_eq!(l["source"], json!("custom")); assert_eq!(l["custom"], json!(true)); assert_eq!(l["wallpaper_css"], json!("linear-gradient(120deg, #101018, #2a1030)")); assert_eq!(l["colors"]["accent"], json!("#ea3d8c"));
    }
    #[test] fn base_themes_cannot_be_taken_or_deleted_and_custom_ones_round_trip_through_sqlite_and_yaml() {
        let d = std::env::temp_dir().join(format!("ct-{}", crate::cli::now_ms())); std::env::set_var("ANTILOKI_USER_DB_DIR", &d); std::env::set_var("OMARCHY_PATH", d.join("none")); std::env::set_var("ANTILOKI_THEMES_DIR", d.join("none2")); std::env::set_var("ANTILOKI_OMARCHY_USER_THEMES", d.join("none3"));
        let mut b = good(); b["id"] = json!("shadow"); assert!(save(&b).unwrap_err().contains("base theme")); assert!(remove("liquid").unwrap_err().contains("base theme"));
        save(&good()).unwrap(); assert_eq!(list().len(), 1); let y = tool(&json!({ "op": "get", "id": "midnight-rose" })).unwrap(); assert!(y.contains("midnight-rose") && y.contains("effect: glow"), "{y}");
        assert!(tool(&json!({ "op": "list" })).unwrap().contains("custom")); assert!(tool(&json!({ "op": "vocabulary" })).unwrap().contains("orbit"));
        remove("midnight-rose").unwrap(); assert!(list().is_empty()); assert!(remove("midnight-rose").is_err()); let _ = std::fs::remove_dir_all(&d);
    }
}
